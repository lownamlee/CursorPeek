use std::{
    env,
    error::Error,
    fmt,
    io::{self, ErrorKind, Read},
    sync::mpsc::{self, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::STATUS_SUCCESS,
        Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom},
    },
    core::{Error as WindowsError, HRESULT},
};

use crate::{
    hover::{Generation, PhysicalScreenPoint},
    platform::{ContainedWorker, ProcessError, WorkerPipes},
};

use super::protocol::{self, ProtocolStreamError, ResolverStatus, SessionNonce, WorkerMessage};

const WORKER_DEADLINE: Duration = Duration::from_secs(2);
const TIMEOUT_DIAGNOSTIC_DEADLINE: Duration = Duration::from_millis(100);
const DIAGNOSTIC_GENERATION: Generation = Generation::from_raw(1);

pub(crate) fn run_launch_diagnostic() -> Result<WorkerDiagnosticReport, WorkerManagerError> {
    let started = Instant::now();
    let status = run_session(SessionKind::ControlExchange, WORKER_DEADLINE)?;
    if status != ResolverStatus::Unavailable {
        return Err(WorkerManagerError::UnexpectedDiagnosticStatus);
    }

    Ok(WorkerDiagnosticReport {
        elapsed: started.elapsed(),
    })
}

pub(crate) fn run_timeout_diagnostic() -> Result<(), WorkerManagerError> {
    match run_session(SessionKind::WaitWithoutHello, TIMEOUT_DIAGNOSTIC_DEADLINE) {
        Err(WorkerManagerError::DeadlineExceeded) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(WorkerManagerError::ExpectedTimeout),
    }
}

fn run_session(
    session_kind: SessionKind,
    timeout: Duration,
) -> Result<ResolverStatus, WorkerManagerError> {
    let nonce = generate_nonce()?;
    let executable = env::current_exe().map_err(WorkerManagerError::CurrentExecutable)?;
    let mut worker = ContainedWorker::spawn(&executable)?;
    let deadline = Instant::now() + timeout;
    let WorkerPipes {
        stdin,
        stdout,
        stderr,
    } = worker.take_pipes()?;

    let stderr_thread = thread::Builder::new()
        .name("cursorpeek-worker-stderr".into())
        .spawn(move || drain_stderr(stderr))
        .map_err(WorkerManagerError::ThreadStart)?;

    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let protocol_thread = match thread::Builder::new()
        .name("cursorpeek-worker-protocol".into())
        .spawn(move || {
            let result = exchange(stdin, stdout, nonce, session_kind);
            let _ = result_sender.send(result);
        }) {
        Ok(thread) => thread,
        Err(error) => {
            worker.terminate_and_wait()?;
            join_stderr(stderr_thread)?;
            return Err(WorkerManagerError::ThreadStart(error));
        }
    };

    let exchange_result = match result_receiver.recv_timeout(remaining(deadline)) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            worker.terminate_and_wait()?;
            join_protocol(protocol_thread)?;
            join_stderr(stderr_thread)?;
            return Err(WorkerManagerError::DeadlineExceeded);
        }
        Err(RecvTimeoutError::Disconnected) => {
            worker.terminate_and_wait()?;
            join_protocol(protocol_thread)?;
            join_stderr(stderr_thread)?;
            return Err(WorkerManagerError::ProtocolChannelDisconnected);
        }
    };

    let status = match exchange_result {
        Ok(status) => status,
        Err(error) => {
            worker.terminate_and_wait()?;
            join_protocol(protocol_thread)?;
            join_stderr(stderr_thread)?;
            return Err(error);
        }
    };

    if !worker.wait_for_exit(remaining(deadline))? {
        worker.terminate_and_wait()?;
        join_protocol(protocol_thread)?;
        join_stderr(stderr_thread)?;
        return Err(WorkerManagerError::DeadlineExceeded);
    }

    let exit_code = worker.exit_code()?;
    join_protocol(protocol_thread)?;
    let stderr_bytes = join_stderr(stderr_thread)?;
    if exit_code != 0 {
        return Err(WorkerManagerError::UnexpectedExit {
            exit_code,
            stderr_bytes,
        });
    }

    Ok(status)
}

fn exchange(
    mut stdin: impl io::Write,
    mut stdout: impl Read,
    nonce: SessionNonce,
    session_kind: SessionKind,
) -> Result<ResolverStatus, WorkerManagerError> {
    if session_kind == SessionKind::WaitWithoutHello {
        return match protocol::read_message(&mut stdout)? {
            None => Err(WorkerManagerError::WorkerExitedBeforeReady),
            Some(_) => Err(WorkerManagerError::UnexpectedReady),
        };
    }

    protocol::write_message(&mut stdin, WorkerMessage::Hello { nonce })?;
    match protocol::read_message(&mut stdout)? {
        Some(WorkerMessage::Ready {
            nonce: returned_nonce,
        }) if returned_nonce == nonce => {}
        Some(WorkerMessage::Ready { .. }) => return Err(WorkerManagerError::NonceMismatch),
        Some(_) => return Err(WorkerManagerError::UnexpectedReady),
        None => return Err(WorkerManagerError::WorkerExitedBeforeReady),
    }

    protocol::write_message(
        &mut stdin,
        WorkerMessage::ResolvePoint {
            generation: DIAGNOSTIC_GENERATION,
            point: PhysicalScreenPoint::new(0, 0),
        },
    )?;
    match protocol::read_message(&mut stdout)? {
        Some(WorkerMessage::ResolverResult { generation, status })
            if generation == DIAGNOSTIC_GENERATION =>
        {
            Ok(status)
        }
        Some(WorkerMessage::ResolverResult { .. }) => Err(WorkerManagerError::GenerationMismatch),
        Some(_) => Err(WorkerManagerError::UnexpectedResult),
        None => Err(WorkerManagerError::WorkerExitedBeforeResult),
    }
}

fn generate_nonce() -> Result<SessionNonce, WorkerManagerError> {
    let mut bytes = [0_u8; 16];
    // SAFETY: no algorithm handle is required with BCRYPT_USE_SYSTEM_PREFERRED_RNG. bytes is a
    // live writable 16-byte slice and this user-mode call runs at ordinary PASSIVE_LEVEL.
    let status = unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status != STATUS_SUCCESS {
        return Err(WorkerManagerError::Random(WindowsError::from_hresult(
            HRESULT::from_nt(status.0),
        )));
    }
    Ok(SessionNonce::from_bytes(bytes))
}

fn drain_stderr(mut stderr: impl Read) -> io::Result<u64> {
    let mut total = 0_u64;
    let mut buffer = [0_u8; 512];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(count) => total = total.saturating_add(count as u64),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if error.kind() == ErrorKind::BrokenPipe => return Ok(total),
            Err(error) => return Err(error),
        }
    }
}

fn join_protocol(thread: JoinHandle<()>) -> Result<(), WorkerManagerError> {
    thread
        .join()
        .map_err(|_| WorkerManagerError::ProtocolThreadPanicked)
}

fn join_stderr(thread: JoinHandle<io::Result<u64>>) -> Result<u64, WorkerManagerError> {
    thread
        .join()
        .map_err(|_| WorkerManagerError::StderrThreadPanicked)?
        .map_err(WorkerManagerError::Stderr)
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionKind {
    ControlExchange,
    WaitWithoutHello,
}

pub(crate) struct WorkerDiagnosticReport {
    elapsed: Duration,
}

impl fmt::Display for WorkerDiagnosticReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Contained worker diagnostic completed: generation=1, status=Unavailable, elapsed={} ms",
            self.elapsed.as_millis()
        )
    }
}

#[derive(Debug)]
pub(crate) enum WorkerManagerError {
    CurrentExecutable(io::Error),
    Process(ProcessError),
    Random(WindowsError),
    ThreadStart(io::Error),
    Protocol(ProtocolStreamError),
    Stderr(io::Error),
    WorkerExitedBeforeReady,
    UnexpectedReady,
    NonceMismatch,
    WorkerExitedBeforeResult,
    UnexpectedResult,
    GenerationMismatch,
    UnexpectedDiagnosticStatus,
    DeadlineExceeded,
    ExpectedTimeout,
    ProtocolChannelDisconnected,
    UnexpectedExit { exit_code: u32, stderr_bytes: u64 },
    ProtocolThreadPanicked,
    StderrThreadPanicked,
}

impl fmt::Display for WorkerManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExecutable(error) => {
                write!(
                    formatter,
                    "could not locate the running executable: {error}"
                )
            }
            Self::Process(error) => write!(formatter, "contained process failed: {error}"),
            Self::Random(error) => write!(formatter, "session nonce generation failed: {error}"),
            Self::ThreadStart(error) => write!(formatter, "worker I/O thread failed: {error}"),
            Self::Protocol(error) => write!(formatter, "{error}"),
            Self::Stderr(error) => write!(formatter, "worker stderr drain failed: {error}"),
            Self::WorkerExitedBeforeReady => write!(formatter, "worker exited before ready"),
            Self::UnexpectedReady => write!(formatter, "worker returned an unexpected ready frame"),
            Self::NonceMismatch => write!(formatter, "worker returned the wrong session nonce"),
            Self::WorkerExitedBeforeResult => write!(formatter, "worker exited before its result"),
            Self::UnexpectedResult => {
                write!(formatter, "worker returned an unexpected result frame")
            }
            Self::GenerationMismatch => write!(formatter, "worker returned the wrong generation"),
            Self::UnexpectedDiagnosticStatus => {
                write!(formatter, "worker returned an unexpected diagnostic status")
            }
            Self::DeadlineExceeded => write!(formatter, "worker exceeded its response deadline"),
            Self::ExpectedTimeout => write!(
                formatter,
                "worker unexpectedly completed the timeout diagnostic"
            ),
            Self::ProtocolChannelDisconnected => {
                write!(
                    formatter,
                    "worker protocol thread disconnected without a result"
                )
            }
            Self::UnexpectedExit {
                exit_code,
                stderr_bytes,
            } => write!(
                formatter,
                "worker exited with code {exit_code} after writing {stderr_bytes} stderr bytes"
            ),
            Self::ProtocolThreadPanicked => write!(formatter, "worker protocol thread panicked"),
            Self::StderrThreadPanicked => write!(formatter, "worker stderr thread panicked"),
        }
    }
}

impl Error for WorkerManagerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CurrentExecutable(error) | Self::ThreadStart(error) | Self::Stderr(error) => {
                Some(error)
            }
            Self::Process(error) => Some(error),
            Self::Random(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::WorkerExitedBeforeReady
            | Self::UnexpectedReady
            | Self::NonceMismatch
            | Self::WorkerExitedBeforeResult
            | Self::UnexpectedResult
            | Self::GenerationMismatch
            | Self::UnexpectedDiagnosticStatus
            | Self::DeadlineExceeded
            | Self::ExpectedTimeout
            | Self::ProtocolChannelDisconnected
            | Self::UnexpectedExit { .. }
            | Self::ProtocolThreadPanicked
            | Self::StderrThreadPanicked => None,
        }
    }
}

impl From<ProcessError> for WorkerManagerError {
    fn from(error: ProcessError) -> Self {
        Self::Process(error)
    }
}

impl From<ProtocolStreamError> for WorkerManagerError {
    fn from(error: ProtocolStreamError) -> Self {
        Self::Protocol(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{DIAGNOSTIC_GENERATION, SessionKind, WorkerManagerError, exchange};
    use crate::{
        hover::Generation,
        worker::protocol::{self, ResolverStatus, SessionNonce, WorkerMessage},
    };

    const NONCE: SessionNonce = SessionNonce::from_bytes([0x11; 16]);
    const OTHER_NONCE: SessionNonce = SessionNonce::from_bytes([0x22; 16]);

    #[test]
    fn parent_rejects_a_ready_frame_with_the_wrong_nonce() {
        let mut child_output = Vec::new();
        protocol::write_message(
            &mut child_output,
            WorkerMessage::Ready { nonce: OTHER_NONCE },
        )
        .unwrap();

        assert!(matches!(
            exchange(
                Vec::new(),
                child_output.as_slice(),
                NONCE,
                SessionKind::ControlExchange,
            ),
            Err(WorkerManagerError::NonceMismatch)
        ));
    }

    #[test]
    fn parent_rejects_a_result_with_the_wrong_generation() {
        let mut child_output = Vec::new();
        protocol::write_message(&mut child_output, WorkerMessage::Ready { nonce: NONCE }).unwrap();
        protocol::write_message(
            &mut child_output,
            WorkerMessage::ResolverResult {
                generation: Generation::from_raw(DIAGNOSTIC_GENERATION.get() + 1),
                status: ResolverStatus::Unavailable,
            },
        )
        .unwrap();

        assert!(matches!(
            exchange(
                Vec::new(),
                child_output.as_slice(),
                NONCE,
                SessionKind::ControlExchange,
            ),
            Err(WorkerManagerError::GenerationMismatch)
        ));
    }
}
