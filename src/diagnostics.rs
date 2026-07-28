#[cfg(feature = "diagnostic-log")]
mod enabled {
    use std::{
        env,
        error::Error,
        ffi::OsStr,
        fmt,
        fs::{self, File, OpenOptions},
        io::{self, BufWriter, Write},
        sync::{
            OnceLock,
            atomic::{AtomicU64, Ordering},
            mpsc::{self, SyncSender, TrySendError},
        },
        thread::{self, JoinHandle},
        time::{SystemTime, UNIX_EPOCH},
    };

    use windows::Win32::System::{
        Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        Threading::GetCurrentThreadId,
    };

    use crate::settings::{SettingsError, diagnostics_directory};

    const RUN_ID_ENVIRONMENT: &str = "CURSORPEEK_DIAGNOSTIC_RUN_ID";
    const CHANNEL_CAPACITY: usize = 4_096;
    const MAX_DETAIL_BYTES: usize = 4_096;
    const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;
    const SCHEMA_VERSION: u8 = 1;

    static LOGGER: OnceLock<Logger> = OnceLock::new();
    static DROPPED_RECORDS: AtomicU64 = AtomicU64::new(0);

    struct Logger {
        sender: SyncSender<Command>,
        frequency: i64,
        role: &'static str,
    }

    enum Command {
        Record(Record),
        Shutdown,
    }

    struct Record {
        unix_ms: u128,
        qpc: i64,
        pid: u32,
        tid: u32,
        role: &'static str,
        event: &'static str,
        detail: String,
    }

    pub(crate) struct DiagnosticGuard {
        sender: Option<SyncSender<Command>>,
        thread: Option<JoinHandle<io::Result<()>>>,
    }

    impl Drop for DiagnosticGuard {
        fn drop(&mut self) {
            if let Some(sender) = self.sender.take() {
                let _ = sender.send(Command::Shutdown);
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[derive(Debug)]
    pub(crate) enum DiagnosticError {
        Storage(SettingsError),
        Io {
            operation: &'static str,
            source: io::Error,
        },
        Clock(windows::core::Error),
        Thread(io::Error),
        AlreadyInitialized,
    }

    impl fmt::Display for DiagnosticError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Storage(error) => write!(formatter, "diagnostic storage: {error}"),
                Self::Io { operation, source } => {
                    write!(formatter, "{operation} diagnostic log: {source}")
                }
                Self::Clock(error) => write!(formatter, "initialize diagnostic clock: {error}"),
                Self::Thread(error) => write!(formatter, "start diagnostic writer: {error}"),
                Self::AlreadyInitialized => {
                    write!(formatter, "diagnostic logger is already active")
                }
            }
        }
    }

    impl Error for DiagnosticError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            match self {
                Self::Storage(error) => Some(error),
                Self::Io { source, .. } | Self::Thread(source) => Some(source),
                Self::Clock(error) => Some(error),
                Self::AlreadyInitialized => None,
            }
        }
    }

    pub(crate) fn initialize(role: &'static str) -> Result<DiagnosticGuard, DiagnosticError> {
        let inherited_run_id = inherited_run_id();
        if role != "main" && !(role == "preview-worker" && inherited_run_id.is_some()) {
            return Ok(DiagnosticGuard {
                sender: None,
                thread: None,
            });
        }
        if LOGGER.get().is_some() {
            return Err(DiagnosticError::AlreadyInitialized);
        }
        let mut frequency = 0_i64;
        // SAFETY: frequency points to live writable storage for the duration of the call.
        unsafe { QueryPerformanceFrequency(&mut frequency) }.map_err(DiagnosticError::Clock)?;

        let run_id = inherited_run_id.unwrap_or_else(new_run_id);
        let directory = diagnostics_directory()
            .map_err(DiagnosticError::Storage)?
            .join(&run_id);
        fs::create_dir_all(&directory).map_err(|source| DiagnosticError::Io {
            operation: "create",
            source,
        })?;

        let path = directory.join(format!("{role}-{}.jsonl", std::process::id()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|source| DiagnosticError::Io {
                operation: "open",
                source,
            })?;
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let writer_frequency = frequency;
        let writer_role = role;
        let thread = thread::Builder::new()
            .name("cursorpeek-diagnostic-writer".into())
            .spawn(move || writer_loop(file, receiver, writer_frequency, writer_role))
            .map_err(DiagnosticError::Thread)?;

        LOGGER
            .set(Logger {
                sender: sender.clone(),
                frequency,
                role,
            })
            .map_err(|_| DiagnosticError::AlreadyInitialized)?;

        let latest = diagnostics_directory()
            .map_err(DiagnosticError::Storage)?
            .join("latest-run.txt");
        let _ = fs::write(latest, format!("{run_id}\n"));

        Ok(DiagnosticGuard {
            sender: Some(sender),
            thread: Some(thread),
        })
    }

    pub(crate) fn record(event: &'static str, detail: fmt::Arguments<'_>) {
        let Some(logger) = LOGGER.get() else {
            return;
        };
        let mut qpc = 0_i64;
        // SAFETY: qpc points to live writable storage for the duration of the call.
        if unsafe { QueryPerformanceCounter(&mut qpc) }.is_err() {
            qpc = 0;
        }
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let detail = bounded_detail(format!("{detail}"));
        // SAFETY: GetCurrentThreadId takes no parameters and has no preconditions.
        let tid = unsafe { GetCurrentThreadId() };
        let record = Record {
            unix_ms,
            qpc,
            pid: std::process::id(),
            tid,
            role: current_role(),
            event,
            detail,
        };
        match logger.sender.try_send(Command::Record(record)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                DROPPED_RECORDS.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    pub(crate) fn elapsed_us(start_qpc: i64) -> Option<u64> {
        let logger = LOGGER.get()?;
        let now = counter();
        let ticks = now.checked_sub(start_qpc)?;
        let micros = i128::from(ticks)
            .checked_mul(1_000_000)?
            .checked_div(i128::from(logger.frequency))?;
        u64::try_from(micros).ok()
    }

    pub(crate) fn counter() -> i64 {
        let mut qpc = 0_i64;
        // SAFETY: qpc points to live writable storage for the duration of the call.
        let _ = unsafe { QueryPerformanceCounter(&mut qpc) };
        qpc
    }

    fn writer_loop(
        file: File,
        receiver: mpsc::Receiver<Command>,
        frequency: i64,
        role: &'static str,
    ) -> io::Result<()> {
        let mut writer = BufWriter::with_capacity(64 * 1024, file);
        let mut written = 0_u64;
        let mut records_since_flush = 0_u8;
        while let Ok(command) = receiver.recv() {
            match command {
                Command::Record(record) => {
                    let flush_after_record = matches!(
                        record.event,
                        "preview.visible"
                            | "preview.show.failed"
                            | "process.stop"
                            | "worker.manager.resolved"
                    );
                    let line = encode_record(&record, frequency);
                    if written.saturating_add(line.len() as u64) > MAX_LOG_BYTES {
                        write_limit_record(&mut writer, frequency, role, written)?;
                        break;
                    }
                    writer.write_all(line.as_bytes())?;
                    written = written.saturating_add(line.len() as u64);
                    records_since_flush = records_since_flush.saturating_add(1);
                    if flush_after_record || records_since_flush >= 16 {
                        writer.flush()?;
                        records_since_flush = 0;
                    }
                }
                Command::Shutdown => {
                    write_summary_record(
                        &mut writer,
                        frequency,
                        role,
                        written,
                        DROPPED_RECORDS.load(Ordering::Relaxed),
                    )?;
                    break;
                }
            }
        }
        writer.flush()
    }

    fn encode_record(record: &Record, frequency: i64) -> String {
        format!(
            "{{\"schema\":{SCHEMA_VERSION},\"unix_ms\":{},\"qpc\":{},\
             \"qpc_frequency\":{frequency},\"pid\":{},\"tid\":{},\"role\":\"{}\",\
             \"event\":\"{}\",\"detail\":\"{}\"}}\n",
            record.unix_ms,
            record.qpc,
            record.pid,
            record.tid,
            escape_json(record.role),
            escape_json(record.event),
            escape_json(&record.detail),
        )
    }

    fn write_summary_record(
        writer: &mut impl Write,
        frequency: i64,
        role: &str,
        written: u64,
        dropped: u64,
    ) -> io::Result<()> {
        writer.write_all(
            format!(
                "{{\"schema\":{SCHEMA_VERSION},\"qpc_frequency\":{frequency},\
                 \"role\":\"{}\",\"event\":\"logger.summary\",\
                 \"detail\":\"written_bytes={written} dropped_records={dropped}\"}}\n",
                escape_json(role)
            )
            .as_bytes(),
        )
    }

    fn write_limit_record(
        writer: &mut impl Write,
        frequency: i64,
        role: &str,
        written: u64,
    ) -> io::Result<()> {
        writer.write_all(
            format!(
                "{{\"schema\":{SCHEMA_VERSION},\"qpc_frequency\":{frequency},\
                 \"role\":\"{}\",\"event\":\"logger.size_limit\",\
                 \"detail\":\"written_bytes={written} max_bytes={MAX_LOG_BYTES}\"}}\n",
                escape_json(role)
            )
            .as_bytes(),
        )
    }

    fn inherited_run_id() -> Option<String> {
        env::var_os(RUN_ID_ENVIRONMENT)
            .as_deref()
            .and_then(valid_run_id)
    }

    fn new_run_id() -> String {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let run_id = format!("{unix_ms}-{}", std::process::id());
        // SAFETY: diagnostic initialization runs before CursorPeek creates any threads. Child
        // workers inherit this immutable correlation identifier.
        unsafe { env::set_var(RUN_ID_ENVIRONMENT, &run_id) };
        run_id
    }

    fn valid_run_id(value: &OsStr) -> Option<String> {
        let value = value.to_str()?;
        (value.len() <= 64
            && !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        .then(|| value.to_owned())
    }

    fn current_role() -> &'static str {
        LOGGER.get().map_or("unknown", |logger| logger.role)
    }

    fn bounded_detail(mut detail: String) -> String {
        if detail.len() <= MAX_DETAIL_BYTES {
            return detail;
        }
        let mut boundary = MAX_DETAIL_BYTES;
        while !detail.is_char_boundary(boundary) {
            boundary -= 1;
        }
        detail.truncate(boundary);
        detail.push('…');
        detail
    }

    fn escape_json(value: &str) -> String {
        let mut escaped = String::with_capacity(value.len());
        for character in value.chars() {
            match character {
                '"' => escaped.push_str("\\\""),
                '\\' => escaped.push_str("\\\\"),
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                character if character.is_control() => {
                    use fmt::Write as _;
                    let _ = write!(escaped, "\\u{:04x}", character as u32);
                }
                character => escaped.push(character),
            }
        }
        escaped
    }

    #[cfg(test)]
    mod tests {
        use super::{MAX_DETAIL_BYTES, bounded_detail, escape_json, valid_run_id};
        use std::ffi::OsStr;

        #[test]
        fn json_text_is_escaped_without_losing_unicode() {
            assert_eq!(escape_json("a\"b\\c\n雪"), "a\\\"b\\\\c\\n雪");
        }

        #[test]
        fn details_are_bounded_on_a_character_boundary() {
            let detail = "雪".repeat(MAX_DETAIL_BYTES);
            let bounded = bounded_detail(detail);
            assert!(bounded.len() <= MAX_DETAIL_BYTES + "…".len());
            assert!(bounded.ends_with('…'));
        }

        #[test]
        fn inherited_identifiers_are_filename_safe() {
            assert_eq!(
                valid_run_id(OsStr::new("1234-5678_worker")),
                Some("1234-5678_worker".to_owned())
            );
            assert_eq!(valid_run_id(OsStr::new("../escape")), None);
            assert_eq!(valid_run_id(OsStr::new("")), None);
        }
    }
}

#[cfg(feature = "diagnostic-log")]
pub(crate) use enabled::{counter, elapsed_us, initialize, record};

#[cfg(not(feature = "diagnostic-log"))]
mod disabled {
    use std::{convert::Infallible, fmt};

    pub(crate) type DiagnosticError = Infallible;

    pub(crate) struct DiagnosticGuard;

    pub(crate) fn initialize(_role: &'static str) -> Result<DiagnosticGuard, DiagnosticError> {
        Ok(DiagnosticGuard)
    }

    pub(crate) fn record(_event: &'static str, _detail: fmt::Arguments<'_>) {}

    pub(crate) const fn counter() -> i64 {
        0
    }

    pub(crate) const fn elapsed_us(_start_qpc: i64) -> Option<u64> {
        None
    }
}

#[cfg(not(feature = "diagnostic-log"))]
pub(crate) use disabled::{counter, elapsed_us, initialize, record};
