use std::{
    env,
    error::Error,
    fmt,
    io::{self, ErrorKind, Read},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
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
const DEFAULT_WORKER_IDLE_LIFETIME: Duration = Duration::from_secs(15);
const DIAGNOSTIC_IDLE_LIFETIME: Duration = Duration::from_millis(250);
const TIMEOUT_DIAGNOSTIC_DEADLINE: Duration = Duration::from_millis(100);

pub(crate) fn run_launch_diagnostic() -> Result<WorkerDiagnosticReport, WorkerManagerError> {
    let started = Instant::now();
    let manager = WorkerManager::start(WorkerManagerConfig {
        idle_lifetime: DIAGNOSTIC_IDLE_LIFETIME,
    })?;

    let diagnostic_result = (|| {
        let first = manager.resolve(Generation::from_raw(1), PhysicalScreenPoint::new(0, 0))?;
        ensure_unavailable(first.status)?;

        let second = manager.resolve(Generation::from_raw(2), PhysicalScreenPoint::new(0, 0))?;
        ensure_unavailable(second.status)?;
        if second.session_id != first.session_id {
            return Err(WorkerManagerError::SessionNotReused);
        }

        let expired_session = manager.wait_for_idle_expiry(WORKER_DEADLINE)?;
        if expired_session != first.session_id {
            return Err(WorkerManagerError::UnexpectedIdleSession);
        }

        let third = manager.resolve(Generation::from_raw(3), PhysicalScreenPoint::new(0, 0))?;
        ensure_unavailable(third.status)?;
        if third.session_id == first.session_id {
            return Err(WorkerManagerError::SessionNotRestarted);
        }

        Ok(WorkerDiagnosticReport {
            elapsed: started.elapsed(),
            requests: 3,
            sessions: 2,
        })
    })();

    let shutdown_result = manager.shutdown();
    match (diagnostic_result, shutdown_result) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(shutdown)) => Err(WorkerManagerError::ShutdownAfterFailure {
            operation: Box::new(operation),
            shutdown: Box::new(shutdown),
        }),
    }
}

pub(crate) fn run_timeout_diagnostic() -> Result<(), WorkerManagerError> {
    let executable = env::current_exe().map_err(WorkerManagerError::CurrentExecutable)?;
    let mut worker = ContainedWorker::spawn(&executable)?;
    let WorkerPipes {
        stdin: _stdin,
        mut stdout,
        stderr,
    } = worker.take_pipes()?;

    let stderr_thread = match start_stderr_thread(stderr) {
        Ok(thread) => thread,
        Err(error) => {
            worker.terminate_and_wait()?;
            return Err(error);
        }
    };
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let protocol_thread = match thread::Builder::new()
        .name("cursorpeek-timeout-protocol".into())
        .spawn(move || {
            let _ = result_sender.send(protocol::read_message(&mut stdout));
        }) {
        Ok(thread) => thread,
        Err(error) => {
            worker.terminate_and_wait()?;
            join_stderr(stderr_thread)?;
            return Err(WorkerManagerError::ThreadStart(error));
        }
    };

    match result_receiver.recv_timeout(TIMEOUT_DIAGNOSTIC_DEADLINE) {
        Err(RecvTimeoutError::Timeout) => {
            worker.terminate_and_wait()?;
            join_protocol(protocol_thread)?;
            join_stderr(stderr_thread)?;
            Ok(())
        }
        Ok(result) => {
            worker.terminate_and_wait()?;
            join_protocol(protocol_thread)?;
            join_stderr(stderr_thread)?;
            match result? {
                None => Err(WorkerManagerError::WorkerExitedBeforeReady),
                Some(_) => Err(WorkerManagerError::UnexpectedReady),
            }
        }
        Err(RecvTimeoutError::Disconnected) => {
            worker.terminate_and_wait()?;
            join_protocol(protocol_thread)?;
            join_stderr(stderr_thread)?;
            Err(WorkerManagerError::ProtocolChannelDisconnected)
        }
    }
}

fn ensure_unavailable(status: ResolverStatus) -> Result<(), WorkerManagerError> {
    if status == ResolverStatus::Unavailable {
        Ok(())
    } else {
        Err(WorkerManagerError::UnexpectedDiagnosticStatus)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkerManagerConfig {
    idle_lifetime: Duration,
}

impl Default for WorkerManagerConfig {
    fn default() -> Self {
        Self {
            idle_lifetime: DEFAULT_WORKER_IDLE_LIFETIME,
        }
    }
}

struct WorkerManager {
    command_sender: SyncSender<ManagerCommand>,
    idle_receiver: Receiver<u64>,
    thread: Option<JoinHandle<Result<(), WorkerManagerError>>>,
}

impl WorkerManager {
    fn start(config: WorkerManagerConfig) -> Result<Self, WorkerManagerError> {
        let (command_sender, command_receiver) = mpsc::sync_channel(0);
        let (idle_sender, idle_receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("cursorpeek-worker-manager".into())
            .spawn(move || manager_loop(config, command_receiver, idle_sender))
            .map_err(WorkerManagerError::ThreadStart)?;

        Ok(Self {
            command_sender,
            idle_receiver,
            thread: Some(thread),
        })
    }

    fn resolve(
        &self,
        generation: Generation,
        point: PhysicalScreenPoint,
    ) -> Result<WorkerResolution, WorkerManagerError> {
        let (response_sender, response_receiver) = mpsc::sync_channel(1);
        self.command_sender
            .send(ManagerCommand::Resolve {
                generation,
                point,
                response_sender,
            })
            .map_err(|_| WorkerManagerError::ManagerChannelDisconnected)?;
        response_receiver
            .recv()
            .map_err(|_| WorkerManagerError::ManagerChannelDisconnected)?
    }

    fn wait_for_idle_expiry(&self, timeout: Duration) -> Result<u64, WorkerManagerError> {
        match self.idle_receiver.recv_timeout(timeout) {
            Ok(session_id) => Ok(session_id),
            Err(RecvTimeoutError::Timeout) => Err(WorkerManagerError::IdleNotificationTimeout),
            Err(RecvTimeoutError::Disconnected) => {
                Err(WorkerManagerError::ManagerChannelDisconnected)
            }
        }
    }

    fn shutdown(mut self) -> Result<(), WorkerManagerError> {
        self.finish()
    }

    fn finish(&mut self) -> Result<(), WorkerManagerError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };

        let (acknowledged_sender, acknowledged_receiver) = mpsc::sync_channel(0);
        if self
            .command_sender
            .send(ManagerCommand::Shutdown {
                acknowledged_sender,
            })
            .is_ok()
        {
            let _ = acknowledged_receiver.recv();
        }

        thread
            .join()
            .map_err(|_| WorkerManagerError::ManagerThreadPanicked)?
    }
}

impl Drop for WorkerManager {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

enum ManagerCommand {
    Resolve {
        generation: Generation,
        point: PhysicalScreenPoint,
        response_sender: SyncSender<Result<WorkerResolution, WorkerManagerError>>,
    },
    Shutdown {
        acknowledged_sender: SyncSender<()>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkerResolution {
    session_id: u64,
    status: ResolverStatus,
}

fn manager_loop(
    config: WorkerManagerConfig,
    command_receiver: Receiver<ManagerCommand>,
    idle_sender: mpsc::Sender<u64>,
) -> Result<(), WorkerManagerError> {
    let mut session: Option<WorkerSession> = None;
    let mut next_session_id = 1_u64;
    let mut last_used = Instant::now();

    loop {
        let command = match session.as_ref() {
            Some(_) => {
                let idle_deadline = last_used + config.idle_lifetime;
                match command_receiver.recv_timeout(remaining(idle_deadline)) {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Timeout) => {
                        let expired = session
                            .take()
                            .expect("the idle deadline exists only for a live session");
                        let session_id = expired.id;
                        expired.shutdown()?;
                        let _ = idle_sender.send(session_id);
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return shutdown_session(session);
                    }
                }
            }
            None => match command_receiver.recv() {
                Ok(command) => command,
                Err(_) => return Ok(()),
            },
        };

        match command {
            ManagerCommand::Resolve {
                generation,
                point,
                response_sender,
            } => {
                if session.is_none() {
                    let session_id = next_session_id;
                    let Some(following_session_id) = next_session_id.checked_add(1) else {
                        let _ = response_sender.send(Err(WorkerManagerError::SessionIdExhausted));
                        continue;
                    };
                    match WorkerSession::spawn(session_id) {
                        Ok(started) => {
                            next_session_id = following_session_id;
                            session = Some(started);
                        }
                        Err(error) => {
                            let _ = response_sender.send(Err(error));
                            continue;
                        }
                    }
                }

                let result = session
                    .as_ref()
                    .expect("a session is created before request dispatch")
                    .resolve(generation, point);
                last_used = Instant::now();

                match result {
                    Ok(status) => {
                        let session_id = session
                            .as_ref()
                            .expect("a successful request keeps its session")
                            .id;
                        let _ = response_sender.send(Ok(WorkerResolution { session_id, status }));
                    }
                    Err(operation) => {
                        let failed = session
                            .take()
                            .expect("a failed request still owns its session");
                        let result = match failed.terminate_and_join() {
                            Ok(()) => Err(operation),
                            Err(cleanup) => Err(WorkerManagerError::RecoveryCleanupFailed {
                                operation: Box::new(operation),
                                cleanup: Box::new(cleanup),
                            }),
                        };
                        let _ = response_sender.send(result);
                    }
                }
            }
            ManagerCommand::Shutdown {
                acknowledged_sender,
            } => {
                let result = shutdown_session(session);
                let _ = acknowledged_sender.send(());
                return result;
            }
        }
    }
}

fn shutdown_session(session: Option<WorkerSession>) -> Result<(), WorkerManagerError> {
    match session {
        Some(session) => session.shutdown(),
        None => Ok(()),
    }
}

struct WorkerSession {
    id: u64,
    worker: ContainedWorker,
    command_sender: Option<SyncSender<ProtocolCommand>>,
    protocol_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<io::Result<u64>>>,
}

impl WorkerSession {
    fn spawn(id: u64) -> Result<Self, WorkerManagerError> {
        let nonce = generate_nonce()?;
        let executable = env::current_exe().map_err(WorkerManagerError::CurrentExecutable)?;
        let mut worker = ContainedWorker::spawn(&executable)?;
        let WorkerPipes {
            stdin,
            stdout,
            stderr,
        } = worker.take_pipes()?;

        let stderr_thread = match start_stderr_thread(stderr) {
            Ok(thread) => thread,
            Err(error) => {
                worker.terminate_and_wait()?;
                return Err(error);
            }
        };
        let (command_sender, command_receiver) = mpsc::sync_channel(0);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let protocol_thread = match thread::Builder::new()
            .name("cursorpeek-worker-protocol".into())
            .spawn(move || {
                protocol_loop(stdin, stdout, nonce, command_receiver, ready_sender);
            }) {
            Ok(thread) => thread,
            Err(error) => {
                worker.terminate_and_wait()?;
                join_stderr(stderr_thread)?;
                return Err(WorkerManagerError::ThreadStart(error));
            }
        };

        let startup_result = match ready_receiver.recv_timeout(WORKER_DEADLINE) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(WorkerManagerError::DeadlineExceeded),
            Err(RecvTimeoutError::Disconnected) => {
                Err(WorkerManagerError::ProtocolChannelDisconnected)
            }
        };

        if let Err(error) = startup_result {
            worker.terminate_and_wait()?;
            drop(command_sender);
            join_protocol(protocol_thread)?;
            join_stderr(stderr_thread)?;
            return Err(error);
        }

        Ok(Self {
            id,
            worker,
            command_sender: Some(command_sender),
            protocol_thread: Some(protocol_thread),
            stderr_thread: Some(stderr_thread),
        })
    }

    fn resolve(
        &self,
        generation: Generation,
        point: PhysicalScreenPoint,
    ) -> Result<ResolverStatus, WorkerManagerError> {
        let (response_sender, response_receiver) = mpsc::sync_channel(1);
        self.command_sender
            .as_ref()
            .expect("live sessions retain their protocol sender")
            .send(ProtocolCommand {
                generation,
                point,
                response_sender,
            })
            .map_err(|_| WorkerManagerError::ProtocolChannelDisconnected)?;

        match response_receiver.recv_timeout(WORKER_DEADLINE) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(WorkerManagerError::DeadlineExceeded),
            Err(RecvTimeoutError::Disconnected) => {
                Err(WorkerManagerError::ProtocolChannelDisconnected)
            }
        }
    }

    fn shutdown(mut self) -> Result<(), WorkerManagerError> {
        drop(self.command_sender.take());
        join_protocol(
            self.protocol_thread
                .take()
                .expect("live sessions retain their protocol thread"),
        )?;

        if !self.worker.wait_for_exit(WORKER_DEADLINE)? {
            self.worker.terminate_and_wait()?;
            join_stderr(
                self.stderr_thread
                    .take()
                    .expect("live sessions retain their stderr thread"),
            )?;
            return Err(WorkerManagerError::DeadlineExceeded);
        }

        let exit_code = self.worker.exit_code()?;
        let stderr_bytes = join_stderr(
            self.stderr_thread
                .take()
                .expect("live sessions retain their stderr thread"),
        )?;
        if exit_code != 0 {
            return Err(WorkerManagerError::UnexpectedExit {
                exit_code,
                stderr_bytes,
            });
        }
        Ok(())
    }

    fn terminate_and_join(mut self) -> Result<(), WorkerManagerError> {
        self.worker.terminate_and_wait()?;
        drop(self.command_sender.take());
        join_protocol(
            self.protocol_thread
                .take()
                .expect("live sessions retain their protocol thread"),
        )?;
        join_stderr(
            self.stderr_thread
                .take()
                .expect("live sessions retain their stderr thread"),
        )?;
        Ok(())
    }
}

struct ProtocolCommand {
    generation: Generation,
    point: PhysicalScreenPoint,
    response_sender: SyncSender<Result<ResolverStatus, WorkerManagerError>>,
}

fn protocol_loop(
    mut stdin: impl io::Write,
    mut stdout: impl Read,
    nonce: SessionNonce,
    command_receiver: Receiver<ProtocolCommand>,
    ready_sender: SyncSender<Result<(), WorkerManagerError>>,
) {
    let handshake = (|| {
        protocol::write_message(&mut stdin, WorkerMessage::Hello { nonce })?;
        validate_ready(protocol::read_message(&mut stdout)?, nonce)
    })();
    if let Err(error) = handshake {
        let _ = ready_sender.send(Err(error));
        return;
    }
    if ready_sender.send(Ok(())).is_err() {
        return;
    }

    while let Ok(command) = command_receiver.recv() {
        let result = (|| {
            protocol::write_message(
                &mut stdin,
                WorkerMessage::ResolvePoint {
                    generation: command.generation,
                    point: command.point,
                },
            )?;
            validate_result(protocol::read_message(&mut stdout)?, command.generation)
        })();
        let failed = result.is_err();
        let _ = command.response_sender.send(result);
        if failed {
            return;
        }
    }
}

fn validate_ready(
    message: Option<WorkerMessage>,
    nonce: SessionNonce,
) -> Result<(), WorkerManagerError> {
    match message {
        Some(WorkerMessage::Ready {
            nonce: returned_nonce,
        }) if returned_nonce == nonce => Ok(()),
        Some(WorkerMessage::Ready { .. }) => Err(WorkerManagerError::NonceMismatch),
        Some(_) => Err(WorkerManagerError::UnexpectedReady),
        None => Err(WorkerManagerError::WorkerExitedBeforeReady),
    }
}

fn validate_result(
    message: Option<WorkerMessage>,
    expected_generation: Generation,
) -> Result<ResolverStatus, WorkerManagerError> {
    match message {
        Some(WorkerMessage::ResolverResult { generation, status })
            if generation == expected_generation =>
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

fn start_stderr_thread(
    stderr: impl Read + Send + 'static,
) -> Result<JoinHandle<io::Result<u64>>, WorkerManagerError> {
    thread::Builder::new()
        .name("cursorpeek-worker-stderr".into())
        .spawn(move || drain_stderr(stderr))
        .map_err(WorkerManagerError::ThreadStart)
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

pub(crate) struct WorkerDiagnosticReport {
    elapsed: Duration,
    requests: u32,
    sessions: u32,
}

impl fmt::Display for WorkerDiagnosticReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Contained worker diagnostic completed: generation=1, status=Unavailable, \
             requests={}, sessions={}, reuse=yes, idle_restart=yes, elapsed={} ms",
            self.requests,
            self.sessions,
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
    ProtocolChannelDisconnected,
    ManagerChannelDisconnected,
    IdleNotificationTimeout,
    UnexpectedIdleSession,
    SessionNotReused,
    SessionNotRestarted,
    SessionIdExhausted,
    UnexpectedExit {
        exit_code: u32,
        stderr_bytes: u64,
    },
    ProtocolThreadPanicked,
    StderrThreadPanicked,
    ManagerThreadPanicked,
    RecoveryCleanupFailed {
        operation: Box<WorkerManagerError>,
        cleanup: Box<WorkerManagerError>,
    },
    ShutdownAfterFailure {
        operation: Box<WorkerManagerError>,
        shutdown: Box<WorkerManagerError>,
    },
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
            Self::ProtocolChannelDisconnected => {
                write!(
                    formatter,
                    "worker protocol thread disconnected without a result"
                )
            }
            Self::ManagerChannelDisconnected => {
                write!(formatter, "worker manager disconnected without a result")
            }
            Self::IdleNotificationTimeout => {
                write!(formatter, "worker did not expire within the idle deadline")
            }
            Self::UnexpectedIdleSession => {
                write!(formatter, "worker manager expired an unexpected session")
            }
            Self::SessionNotReused => {
                write!(formatter, "consecutive requests did not reuse the worker")
            }
            Self::SessionNotRestarted => {
                write!(formatter, "an idle worker was reused instead of restarted")
            }
            Self::SessionIdExhausted => write!(formatter, "worker session IDs were exhausted"),
            Self::UnexpectedExit {
                exit_code,
                stderr_bytes,
            } => write!(
                formatter,
                "worker exited with code {exit_code} after writing {stderr_bytes} stderr bytes"
            ),
            Self::ProtocolThreadPanicked => write!(formatter, "worker protocol thread panicked"),
            Self::StderrThreadPanicked => write!(formatter, "worker stderr thread panicked"),
            Self::ManagerThreadPanicked => write!(formatter, "worker manager thread panicked"),
            Self::RecoveryCleanupFailed { operation, cleanup } => write!(
                formatter,
                "worker request failed ({operation}) and cleanup also failed ({cleanup})"
            ),
            Self::ShutdownAfterFailure {
                operation,
                shutdown,
            } => write!(
                formatter,
                "worker diagnostic failed ({operation}) and shutdown also failed ({shutdown})"
            ),
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
            Self::RecoveryCleanupFailed { operation, .. }
            | Self::ShutdownAfterFailure { operation, .. } => Some(operation),
            Self::WorkerExitedBeforeReady
            | Self::UnexpectedReady
            | Self::NonceMismatch
            | Self::WorkerExitedBeforeResult
            | Self::UnexpectedResult
            | Self::GenerationMismatch
            | Self::UnexpectedDiagnosticStatus
            | Self::DeadlineExceeded
            | Self::ProtocolChannelDisconnected
            | Self::ManagerChannelDisconnected
            | Self::IdleNotificationTimeout
            | Self::UnexpectedIdleSession
            | Self::SessionNotReused
            | Self::SessionNotRestarted
            | Self::SessionIdExhausted
            | Self::UnexpectedExit { .. }
            | Self::ProtocolThreadPanicked
            | Self::StderrThreadPanicked
            | Self::ManagerThreadPanicked => None,
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
    use super::{
        DEFAULT_WORKER_IDLE_LIFETIME, WorkerManagerConfig, WorkerManagerError, validate_ready,
        validate_result,
    };
    use crate::{
        hover::Generation,
        worker::protocol::{ResolverStatus, SessionNonce, WorkerMessage},
    };
    use std::time::Duration;

    const NONCE: SessionNonce = SessionNonce::from_bytes([0x11; 16]);
    const OTHER_NONCE: SessionNonce = SessionNonce::from_bytes([0x22; 16]);

    #[test]
    fn default_worker_idle_lifetime_is_fifteen_seconds() {
        assert_eq!(
            WorkerManagerConfig::default().idle_lifetime,
            DEFAULT_WORKER_IDLE_LIFETIME
        );
        assert_eq!(DEFAULT_WORKER_IDLE_LIFETIME, Duration::from_secs(15));
    }

    #[test]
    fn parent_rejects_a_ready_frame_with_the_wrong_nonce() {
        assert!(matches!(
            validate_ready(Some(WorkerMessage::Ready { nonce: OTHER_NONCE }), NONCE),
            Err(WorkerManagerError::NonceMismatch)
        ));
    }

    #[test]
    fn parent_rejects_a_result_with_the_wrong_generation() {
        assert!(matches!(
            validate_result(
                Some(WorkerMessage::ResolverResult {
                    generation: Generation::from_raw(2),
                    status: ResolverStatus::Unavailable,
                }),
                Generation::from_raw(1),
            ),
            Err(WorkerManagerError::GenerationMismatch)
        ));
    }
}
