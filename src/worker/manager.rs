use std::{
    env,
    error::Error,
    fmt,
    io::{self, ErrorKind, Read},
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    },
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
    settings::LegacyEncoding,
};

use super::{
    payload::{PreviewResult, ResolverStatus},
    protocol::{self, ProtocolStreamError, SessionNonce, WorkerMessage},
};

const WORKER_DEADLINE: Duration = Duration::from_secs(2);
const DEFAULT_WORKER_IDLE_LIFETIME: Duration = Duration::from_secs(15);
const DIAGNOSTIC_IDLE_LIFETIME: Duration = Duration::from_millis(250);
const TIMEOUT_DIAGNOSTIC_DEADLINE: Duration = Duration::from_millis(100);

pub(crate) fn run_launch_diagnostic() -> Result<WorkerDiagnosticReport, WorkerManagerError> {
    let started = Instant::now();
    let manager = WorkerManager::start_with_config(WorkerManagerConfig {
        idle_lifetime: DIAGNOSTIC_IDLE_LIFETIME,
        ..WorkerManagerConfig::default()
    })?;

    let diagnostic_result = (|| {
        let first = manager.resolve(Generation::from_raw(1), PhysicalScreenPoint::new(0, 0))?;
        ensure_unavailable(&first.result)?;

        let second = manager.resolve(Generation::from_raw(2), PhysicalScreenPoint::new(0, 0))?;
        ensure_unavailable(&second.result)?;
        if second.session_id != first.session_id {
            return Err(WorkerManagerError::SessionNotReused);
        }

        let expired_session = manager.wait_for_idle_expiry(WORKER_DEADLINE)?;
        if expired_session != first.session_id {
            return Err(WorkerManagerError::UnexpectedIdleSession);
        }

        let third = manager.resolve(Generation::from_raw(3), PhysicalScreenPoint::new(0, 0))?;
        ensure_unavailable(&third.result)?;
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

fn ensure_unavailable(result: &PreviewResult) -> Result<(), WorkerManagerError> {
    if result.status() == Some(ResolverStatus::Unavailable) {
        Ok(())
    } else {
        Err(WorkerManagerError::UnexpectedDiagnosticStatus)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkerManagerConfig {
    idle_lifetime: Duration,
    legacy_encoding: LegacyEncoding,
}

impl Default for WorkerManagerConfig {
    fn default() -> Self {
        Self {
            idle_lifetime: DEFAULT_WORKER_IDLE_LIFETIME,
            legacy_encoding: LegacyEncoding::Auto,
        }
    }
}

pub(crate) struct WorkerManager {
    requests: Arc<LatestRequestMailbox>,
    idle_receiver: Receiver<u64>,
    thread: Option<JoinHandle<Result<(), WorkerManagerError>>>,
}

impl WorkerManager {
    pub(crate) fn start(legacy_encoding: LegacyEncoding) -> Result<Self, WorkerManagerError> {
        Self::start_with_config(WorkerManagerConfig {
            legacy_encoding,
            ..WorkerManagerConfig::default()
        })
    }

    fn start_with_config(config: WorkerManagerConfig) -> Result<Self, WorkerManagerError> {
        let requests = Arc::new(LatestRequestMailbox::new());
        let manager_requests = Arc::clone(&requests);
        let (idle_sender, idle_receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("cursorpeek-worker-manager".into())
            .spawn(move || {
                let result = manager_loop(config, &manager_requests, idle_sender);
                manager_requests.close();
                result
            })
            .map_err(WorkerManagerError::ThreadStart)?;

        Ok(Self {
            requests,
            idle_receiver,
            thread: Some(thread),
        })
    }

    fn resolve(
        &self,
        generation: Generation,
        point: PhysicalScreenPoint,
    ) -> Result<WorkerResolution, WorkerManagerError> {
        self.submit(generation, point)?.wait()
    }

    pub(crate) fn submit(
        &self,
        generation: Generation,
        point: PhysicalScreenPoint,
    ) -> Result<PendingWorkerResolution, WorkerManagerError> {
        Ok(PendingWorkerResolution {
            receiver: self.requests.submit(generation, point)?,
        })
    }

    pub(crate) fn submit_with_notifier(
        &self,
        generation: Generation,
        point: PhysicalScreenPoint,
        notifier: CompletionNotifier,
    ) -> Result<PendingWorkerResolution, WorkerManagerError> {
        Ok(PendingWorkerResolution {
            receiver: self
                .requests
                .submit_with_notifier(generation, point, notifier)?,
        })
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

    pub(crate) fn shutdown(mut self) -> Result<(), WorkerManagerError> {
        self.finish()
    }

    fn finish(&mut self) -> Result<(), WorkerManagerError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };

        self.requests.close();
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkerResolution {
    generation: Generation,
    session_id: u64,
    result: PreviewResult,
}

impl WorkerResolution {
    pub(crate) const fn generation(&self) -> Generation {
        self.generation
    }

    pub(crate) fn into_result(self) -> PreviewResult {
        self.result
    }
}

pub(crate) struct PendingWorkerResolution {
    receiver: Receiver<Result<WorkerResolution, WorkerManagerError>>,
}

impl PendingWorkerResolution {
    pub(crate) fn poll(&mut self) -> PendingWorkerPoll {
        match self.receiver.try_recv() {
            Ok(result) => PendingWorkerPoll::Ready(result),
            Err(TryRecvError::Empty) => PendingWorkerPoll::Pending,
            Err(TryRecvError::Disconnected) => {
                PendingWorkerPoll::Ready(Err(WorkerManagerError::ManagerChannelDisconnected))
            }
        }
    }

    fn wait(self) -> Result<WorkerResolution, WorkerManagerError> {
        self.receiver
            .recv()
            .map_err(|_| WorkerManagerError::ManagerChannelDisconnected)?
    }
}

pub(crate) enum PendingWorkerPoll {
    Pending,
    Ready(Result<WorkerResolution, WorkerManagerError>),
}

struct PendingRequest {
    generation: Generation,
    point: PhysicalScreenPoint,
    response_sender: SyncSender<Result<WorkerResolution, WorkerManagerError>>,
    completion_notifier: Option<CompletionNotifier>,
}

#[derive(Clone, Copy)]
pub(crate) struct CompletionNotifier {
    context: usize,
    callback: fn(usize),
}

impl CompletionNotifier {
    pub(crate) const fn new(context: usize, callback: fn(usize)) -> Self {
        Self { context, callback }
    }

    fn notify(self) {
        (self.callback)(self.context);
    }
}

#[derive(Default)]
struct RequestMailboxState {
    pending: Option<PendingRequest>,
    closed: bool,
    #[cfg(test)]
    max_pending: usize,
}

struct LatestRequestMailbox {
    state: Mutex<RequestMailboxState>,
    changed: Condvar,
}

impl LatestRequestMailbox {
    fn new() -> Self {
        Self {
            state: Mutex::new(RequestMailboxState::default()),
            changed: Condvar::new(),
        }
    }

    fn submit(
        &self,
        generation: Generation,
        point: PhysicalScreenPoint,
    ) -> Result<Receiver<Result<WorkerResolution, WorkerManagerError>>, WorkerManagerError> {
        self.submit_request(generation, point, None)
    }

    fn submit_with_notifier(
        &self,
        generation: Generation,
        point: PhysicalScreenPoint,
        notifier: CompletionNotifier,
    ) -> Result<Receiver<Result<WorkerResolution, WorkerManagerError>>, WorkerManagerError> {
        self.submit_request(generation, point, Some(notifier))
    }

    fn submit_request(
        &self,
        generation: Generation,
        point: PhysicalScreenPoint,
        completion_notifier: Option<CompletionNotifier>,
    ) -> Result<Receiver<Result<WorkerResolution, WorkerManagerError>>, WorkerManagerError> {
        let (response_sender, response_receiver) = mpsc::sync_channel(1);
        let replaced = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| WorkerManagerError::RequestMailboxPoisoned)?;
            if state.closed {
                return Err(WorkerManagerError::ManagerChannelDisconnected);
            }

            let replaced = state.pending.replace(PendingRequest {
                generation,
                point,
                response_sender,
                completion_notifier,
            });
            #[cfg(test)]
            {
                state.max_pending = state.max_pending.max(usize::from(state.pending.is_some()));
            }
            replaced
        };

        if let Some(replaced) = replaced {
            complete_request(replaced, Err(WorkerManagerError::RequestSuperseded));
        }
        self.changed.notify_one();
        Ok(response_receiver)
    }

    fn take(&self, timeout: Option<Duration>) -> Result<MailboxTake, WorkerManagerError> {
        let deadline = timeout.map(|duration| Instant::now() + duration);
        let mut state = self
            .state
            .lock()
            .map_err(|_| WorkerManagerError::RequestMailboxPoisoned)?;

        loop {
            if let Some(request) = state.pending.take() {
                return Ok(MailboxTake::Request(request));
            }
            if state.closed {
                return Ok(MailboxTake::Closed);
            }

            match deadline {
                None => {
                    state = self
                        .changed
                        .wait(state)
                        .map_err(|_| WorkerManagerError::RequestMailboxPoisoned)?;
                }
                Some(deadline) => {
                    let wait = remaining(deadline);
                    if wait.is_zero() {
                        return Ok(MailboxTake::TimedOut);
                    }
                    let (next_state, timeout_result) = self
                        .changed
                        .wait_timeout(state, wait)
                        .map_err(|_| WorkerManagerError::RequestMailboxPoisoned)?;
                    state = next_state;
                    if timeout_result.timed_out() && state.pending.is_none() {
                        return Ok(MailboxTake::TimedOut);
                    }
                }
            }
        }
    }

    fn close(&self) {
        let cancelled = match self.state.lock() {
            Ok(mut state) => {
                state.closed = true;
                state.pending.take()
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.closed = true;
                state.pending.take()
            }
        };
        if let Some(cancelled) = cancelled {
            complete_request(cancelled, Err(WorkerManagerError::RequestCancelled));
        }
        self.changed.notify_all();
    }

    #[cfg(test)]
    fn max_pending(&self) -> usize {
        self.state
            .lock()
            .expect("mailbox should not be poisoned")
            .max_pending
    }
}

enum MailboxTake {
    Request(PendingRequest),
    TimedOut,
    Closed,
}

fn manager_loop(
    config: WorkerManagerConfig,
    requests: &LatestRequestMailbox,
    idle_sender: mpsc::Sender<u64>,
) -> Result<(), WorkerManagerError> {
    let mut session: Option<WorkerSession> = None;
    let mut next_session_id = 1_u64;
    let mut last_used = Instant::now();

    loop {
        let timeout = session
            .as_ref()
            .map(|_| remaining(last_used + config.idle_lifetime));
        match requests.take(timeout)? {
            MailboxTake::Request(PendingRequest {
                generation,
                point,
                response_sender,
                completion_notifier,
            }) => {
                if session.is_none() {
                    let session_id = next_session_id;
                    let Some(following_session_id) = next_session_id.checked_add(1) else {
                        send_completion(
                            response_sender,
                            completion_notifier,
                            Err(WorkerManagerError::SessionIdExhausted),
                        );
                        continue;
                    };
                    match WorkerSession::spawn(session_id, config.legacy_encoding.clone()) {
                        Ok(started) => {
                            next_session_id = following_session_id;
                            session = Some(started);
                        }
                        Err(error) => {
                            send_completion(response_sender, completion_notifier, Err(error));
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
                    Ok(result) => {
                        let session_id = session
                            .as_ref()
                            .expect("a successful request keeps its session")
                            .id;
                        send_completion(
                            response_sender,
                            completion_notifier,
                            Ok(WorkerResolution {
                                generation,
                                session_id,
                                result,
                            }),
                        );
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
                        send_completion(response_sender, completion_notifier, result);
                    }
                }
            }
            MailboxTake::TimedOut => {
                let expired = session
                    .take()
                    .expect("only a live session supplies an idle timeout");
                let session_id = expired.id;
                expired.shutdown()?;
                let _ = idle_sender.send(session_id);
            }
            MailboxTake::Closed => return shutdown_session(session),
        }
    }
}

fn complete_request(request: PendingRequest, result: Result<WorkerResolution, WorkerManagerError>) {
    send_completion(request.response_sender, request.completion_notifier, result);
}

fn send_completion(
    response_sender: SyncSender<Result<WorkerResolution, WorkerManagerError>>,
    completion_notifier: Option<CompletionNotifier>,
    result: Result<WorkerResolution, WorkerManagerError>,
) {
    let _ = response_sender.send(result);
    if let Some(notifier) = completion_notifier {
        notifier.notify();
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
    fn spawn(id: u64, legacy_encoding: LegacyEncoding) -> Result<Self, WorkerManagerError> {
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
                protocol_loop(
                    stdin,
                    stdout,
                    nonce,
                    legacy_encoding,
                    command_receiver,
                    ready_sender,
                );
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
    ) -> Result<PreviewResult, WorkerManagerError> {
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
    response_sender: SyncSender<Result<PreviewResult, WorkerManagerError>>,
}

fn protocol_loop(
    mut stdin: impl io::Write,
    mut stdout: impl Read,
    nonce: SessionNonce,
    legacy_encoding: LegacyEncoding,
    command_receiver: Receiver<ProtocolCommand>,
    ready_sender: SyncSender<Result<(), WorkerManagerError>>,
) {
    let handshake = (|| {
        protocol::write_message(
            &mut stdin,
            WorkerMessage::Hello {
                nonce,
                legacy_encoding,
            },
        )?;
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
) -> Result<PreviewResult, WorkerManagerError> {
    match message {
        Some(WorkerMessage::PreviewResult { generation, result })
            if generation == expected_generation =>
        {
            Ok(result)
        }
        Some(WorkerMessage::PreviewResult { .. }) => Err(WorkerManagerError::GenerationMismatch),
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
    RequestMailboxPoisoned,
    RequestSuperseded,
    RequestCancelled,
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
            Self::RequestMailboxPoisoned => write!(formatter, "preview request mailbox poisoned"),
            Self::RequestSuperseded => {
                write!(
                    formatter,
                    "preview request was superseded by a newer request"
                )
            }
            Self::RequestCancelled => write!(formatter, "preview request was cancelled"),
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
            | Self::RequestMailboxPoisoned
            | Self::RequestSuperseded
            | Self::RequestCancelled
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
        CompletionNotifier, DEFAULT_WORKER_IDLE_LIFETIME, LatestRequestMailbox, MailboxTake,
        PendingWorkerPoll, PendingWorkerResolution, WorkerManagerConfig, WorkerManagerError,
        WorkerResolution, validate_ready, validate_result,
    };
    use crate::{
        hover::{Generation, PhysicalScreenPoint},
        worker::{
            payload::{PreviewResult, ResolverStatus},
            protocol::{SessionNonce, WorkerMessage},
        },
    };
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    const NONCE: SessionNonce = SessionNonce::from_bytes([0x11; 16]);
    const OTHER_NONCE: SessionNonce = SessionNonce::from_bytes([0x22; 16]);
    static NOTIFICATION_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn record_notification(increment: usize) {
        NOTIFICATION_COUNT.fetch_add(increment, Ordering::SeqCst);
    }

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
                Some(WorkerMessage::PreviewResult {
                    generation: Generation::from_raw(2),
                    result: PreviewResult::Status(ResolverStatus::Unavailable),
                }),
                Generation::from_raw(1),
            ),
            Err(WorkerManagerError::GenerationMismatch)
        ));
    }

    #[test]
    fn newer_request_supersedes_the_single_pending_request() {
        let mailbox = LatestRequestMailbox::new();
        let older = mailbox
            .submit(Generation::from_raw(1), PhysicalScreenPoint::new(1, 1))
            .unwrap();
        let newer = mailbox
            .submit(Generation::from_raw(2), PhysicalScreenPoint::new(2, 2))
            .unwrap();

        assert!(matches!(
            older.recv().unwrap(),
            Err(WorkerManagerError::RequestSuperseded)
        ));
        let pending = match mailbox.take(Some(Duration::ZERO)).unwrap() {
            MailboxTake::Request(request) => request,
            MailboxTake::TimedOut | MailboxTake::Closed => panic!("latest request was not pending"),
        };
        assert_eq!(pending.generation, Generation::from_raw(2));
        assert_eq!(pending.point, PhysicalScreenPoint::new(2, 2));
        pending
            .response_sender
            .send(Ok(WorkerResolution {
                generation: Generation::from_raw(2),
                session_id: 7,
                result: PreviewResult::Status(ResolverStatus::Unavailable),
            }))
            .unwrap();
        assert_eq!(
            newer.recv().unwrap().unwrap(),
            WorkerResolution {
                generation: Generation::from_raw(2),
                session_id: 7,
                result: PreviewResult::Status(ResolverStatus::Unavailable),
            }
        );
    }

    #[test]
    fn superseded_notified_request_wakes_its_consumer_once() {
        NOTIFICATION_COUNT.store(0, Ordering::SeqCst);
        let mailbox = LatestRequestMailbox::new();
        let older = mailbox
            .submit_with_notifier(
                Generation::from_raw(1),
                PhysicalScreenPoint::new(1, 1),
                CompletionNotifier::new(1, record_notification),
            )
            .unwrap();

        let _newer = mailbox
            .submit(Generation::from_raw(2), PhysicalScreenPoint::new(2, 2))
            .unwrap();

        assert!(matches!(
            older.recv().unwrap(),
            Err(WorkerManagerError::RequestSuperseded)
        ));
        assert_eq!(NOTIFICATION_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pending_resolution_poll_never_blocks_the_ui_contract() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let mut pending = PendingWorkerResolution { receiver };

        assert!(matches!(pending.poll(), PendingWorkerPoll::Pending));

        sender
            .send(Ok(WorkerResolution {
                generation: Generation::from_raw(9),
                session_id: 4,
                result: PreviewResult::Status(ResolverStatus::Resolved),
            }))
            .unwrap();

        match pending.poll() {
            PendingWorkerPoll::Ready(Ok(resolution)) => {
                assert_eq!(resolution.generation(), Generation::from_raw(9));
            }
            PendingWorkerPoll::Pending | PendingWorkerPoll::Ready(Err(_)) => {
                panic!("the queued resolution should be ready")
            }
        }
    }

    #[test]
    fn shutdown_cancels_pending_work_and_closes_the_mailbox() {
        let mailbox = LatestRequestMailbox::new();
        let pending = mailbox
            .submit(Generation::from_raw(1), PhysicalScreenPoint::new(0, 0))
            .unwrap();

        mailbox.close();

        assert!(matches!(
            pending.recv().unwrap(),
            Err(WorkerManagerError::RequestCancelled)
        ));
        assert!(matches!(
            mailbox.take(Some(Duration::ZERO)).unwrap(),
            MailboxTake::Closed
        ));
        assert!(matches!(
            mailbox.submit(Generation::from_raw(2), PhysicalScreenPoint::new(0, 0)),
            Err(WorkerManagerError::ManagerChannelDisconnected)
        ));
    }

    #[test]
    fn ten_thousand_submissions_keep_only_the_latest_request() {
        let mailbox = LatestRequestMailbox::new();
        let active_receiver = mailbox
            .submit(Generation::from_raw(1), PhysicalScreenPoint::new(1, -1))
            .unwrap();
        let active = match mailbox.take(Some(Duration::ZERO)).unwrap() {
            MailboxTake::Request(request) => request,
            MailboxTake::TimedOut | MailboxTake::Closed => panic!("first request was not active"),
        };
        let mut previous = mailbox
            .submit(Generation::from_raw(2), PhysicalScreenPoint::new(2, -2))
            .unwrap();

        for raw_generation in 3..=10_000 {
            let latest = mailbox
                .submit(
                    Generation::from_raw(raw_generation),
                    PhysicalScreenPoint::new(raw_generation as i32, -(raw_generation as i32)),
                )
                .unwrap();
            assert!(matches!(
                previous.recv().unwrap(),
                Err(WorkerManagerError::RequestSuperseded)
            ));
            previous = latest;
        }

        assert_eq!(mailbox.max_pending(), 1);
        active
            .response_sender
            .send(Ok(WorkerResolution {
                generation: Generation::from_raw(1),
                session_id: 1,
                result: PreviewResult::Status(ResolverStatus::Unavailable),
            }))
            .unwrap();
        assert!(active_receiver.recv().unwrap().is_ok());
        let latest = match mailbox.take(Some(Duration::ZERO)).unwrap() {
            MailboxTake::Request(request) => request,
            MailboxTake::TimedOut | MailboxTake::Closed => panic!("latest request was not pending"),
        };
        assert_eq!(latest.generation, Generation::from_raw(10_000));
        assert_eq!(latest.point, PhysicalScreenPoint::new(10_000, -10_000));
        latest
            .response_sender
            .send(Ok(WorkerResolution {
                generation: Generation::from_raw(10_000),
                session_id: 1,
                result: PreviewResult::Status(ResolverStatus::Unavailable),
            }))
            .unwrap();
        assert!(previous.recv().unwrap().is_ok());
        assert!(matches!(
            mailbox.take(Some(Duration::ZERO)).unwrap(),
            MailboxTake::TimedOut
        ));
    }
}
