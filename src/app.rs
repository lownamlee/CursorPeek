use std::{error::Error, fmt, io, time::Instant};

#[cfg(feature = "resolver-corpus")]
use crate::corpus;
use crate::{
    diagnostics,
    hover::INPUT_DIAGNOSTIC_DURATION,
    mode::ProcessMode,
    platform::{
        ApartmentKind, ApplicationRunError, ComApartment, DPI_DIAGNOSTIC_SUCCESS,
        DpiAwarenessError, MessageWindow, PREVIEW_WINDOW_DIAGNOSTIC_DURATION,
        PREVIEW_WINDOW_PRACTICE_DURATION, SingleInstance, StartupRegistration,
        activate_existing_instance, shutdown_existing_instance, verify_per_monitor_v2,
    },
    preview::PreviewSize,
    resolver::{ExplorerResolver, ResolverError},
    settings::{SettingsDocument, SettingsError, SettingsFile, SettingsMode},
    worker::{self, WorkerManagerError, WorkerSessionError},
};

pub(crate) fn run(process_mode: ProcessMode) -> Result<(), AppError> {
    #[cfg(feature = "diagnostic-log")]
    let _diagnostic_guard = diagnostics::initialize(process_mode.as_str()).ok();
    #[cfg(not(feature = "diagnostic-log"))]
    let _diagnostic_guard =
        diagnostics::initialize(process_mode.as_str()).unwrap_or_else(|never| match never {});

    diagnostics::record(
        "process.start",
        format_args!(
            "version={} mode={} diagnostic_build={}",
            env!("CARGO_PKG_VERSION"),
            process_mode.as_str(),
            cfg!(feature = "diagnostic-log")
        ),
    );
    let result = run_inner(process_mode);
    match &result {
        Ok(()) => diagnostics::record("process.stop", format_args!("outcome=success")),
        Err(error) => diagnostics::record(
            "process.stop",
            format_args!("outcome=error category={}", error.category()),
        ),
    }
    result
}

fn run_inner(process_mode: ProcessMode) -> Result<(), AppError> {
    let startup_started = Instant::now();
    verify_per_monitor_v2()?;
    diagnostics::record("startup.dpi", format_args!("per_monitor_v2=true"));

    let _single_instance_guard = if process_mode == ProcessMode::Main {
        match SingleInstance::acquire()? {
            Some(instance) => Some(instance),
            None => {
                diagnostics::record(
                    "startup.single_instance",
                    format_args!("outcome=activate_existing"),
                );
                activate_existing_instance()?;
                return Ok(());
            }
        }
    } else {
        None
    };
    diagnostics::record(
        "startup.single_instance",
        format_args!("owned={}", _single_instance_guard.is_some()),
    );

    let _apartment = match process_mode {
        ProcessMode::Main
        | ProcessMode::InputDiagnostics
        | ProcessMode::PreviewWindowDiagnostics
        | ProcessMode::PreviewWindowPracticeDiagnostics
        | ProcessMode::WorkerDiagnostics
        | ProcessMode::WorkerTimeoutDiagnostics
        | ProcessMode::RecoverySoakDiagnostics
        | ProcessMode::PerformanceDiagnostics => {
            Some(ComApartment::initialize(ApartmentKind::SingleThreaded)?)
        }
        ProcessMode::DpiDiagnostics
        | ProcessMode::SettingsDiagnostics
        | ProcessMode::ShutdownExisting
        | ProcessMode::SetStartupEnabled
        | ProcessMode::SetStartupDisabled
        | ProcessMode::PreviewWorker => None,
        #[cfg(feature = "resolver-corpus")]
        ProcessMode::ResolverCorpusProbe => None,
    };
    diagnostics::record(
        "startup.com",
        format_args!("initialized={}", _apartment.is_some()),
    );

    match process_mode {
        ProcessMode::Main => {
            let settings_file = SettingsFile::discover()?;
            let settings = settings_file.load_or_create()?;
            diagnostics::record(
                "settings.loaded",
                format_args!(
                    "mode={} dwell_ms={} preview_width={} preview_height={} cache_entries={} \
                     theme={} legacy_encoding={:?} startup={}",
                    settings_file.mode().as_str(),
                    settings.settings().dwell_delay_ms(),
                    settings.settings().preview_width(),
                    settings.settings().preview_height(),
                    settings.settings().cache_entries(),
                    settings.settings().theme().as_str(),
                    settings.settings().legacy_encoding(),
                    settings.settings().start_with_windows()
                ),
            );
            let preview_size = PreviewSize::new(
                u32::from(settings.settings().preview_width()),
                u32::from(settings.settings().preview_height()),
            );
            let message_window = MessageWindow::create_for_application(
                settings.settings().dwell_delay(),
                preview_size,
            )?;
            let worker_manager = worker::WorkerManager::start(
                settings.settings().legacy_encoding().clone(),
                settings.settings().cache_entries(),
            )?;
            diagnostics::record(
                "worker.manager",
                format_args!("state=started prewarm=requested"),
            );
            message_window.run_application(worker_manager, settings_file, settings)?;
        }
        ProcessMode::InputDiagnostics => {
            println!(
                "Starting a 30-second Raw Input coverage sample. Switch to File Explorer and use \
                 one labeled input device."
            );
            let report =
                MessageWindow::create()?.run_input_diagnostics(INPUT_DIAGNOSTIC_DURATION)?;
            println!("{report}");
            println!(
                "Unmatched changes are candidate coverage gaps, not an automatic support result."
            );
        }
        ProcessMode::DpiDiagnostics => {
            println!("{DPI_DIAGNOSTIC_SUCCESS}");
        }
        ProcessMode::PreviewWindowDiagnostics => {
            let report = MessageWindow::create()?
                .run_preview_window_diagnostics(PREVIEW_WINDOW_DIAGNOSTIC_DURATION)?;
            println!("{report}");
        }
        ProcessMode::PreviewWindowPracticeDiagnostics => {
            let report = MessageWindow::create()?
                .run_preview_window_diagnostics(PREVIEW_WINDOW_PRACTICE_DURATION)?;
            println!("{report}");
        }
        ProcessMode::WorkerDiagnostics => {
            println!("{}", worker::run_launch_diagnostic()?);
        }
        ProcessMode::WorkerTimeoutDiagnostics => {
            worker::run_timeout_diagnostic()?;
            println!("Contained worker timeout cleanup completed.");
        }
        ProcessMode::RecoverySoakDiagnostics => {
            let report = MessageWindow::create()?.run_recovery_soak_diagnostics()?;
            println!("{report}");
        }
        ProcessMode::PerformanceDiagnostics => {
            let settings = SettingsDocument::default();
            let preview_size = PreviewSize::new(
                u32::from(settings.settings().preview_width()),
                u32::from(settings.settings().preview_height()),
            );
            let message_window = MessageWindow::create_for_application(
                settings.settings().dwell_delay(),
                preview_size,
            )?;
            let worker_manager = worker::WorkerManager::start(
                settings.settings().legacy_encoding().clone(),
                settings.settings().cache_entries(),
            )?;
            let report = message_window.run_performance_diagnostics(
                worker_manager,
                settings,
                startup_started,
            )?;
            println!("{report}");
        }
        ProcessMode::SettingsDiagnostics => {
            let settings_file = SettingsFile::discover()?;
            let mode = settings_file.mode();
            settings_file.load_or_create()?;
            println!(
                "Settings storage diagnostic completed: mode={}, configuration_created=yes",
                mode.as_str()
            );
        }
        ProcessMode::ShutdownExisting => {
            shutdown_existing_instance()?;
        }
        ProcessMode::SetStartupEnabled => {
            set_installed_startup(true)?;
        }
        ProcessMode::SetStartupDisabled => {
            set_installed_startup(false)?;
        }
        ProcessMode::PreviewWorker => {
            let stdin = io::stdin();
            let stdout = io::stdout();
            let mut resolver = ExplorerResolver::initialize()?;
            worker::run_session(&mut stdin.lock(), &mut stdout.lock(), &mut resolver)?;
        }
        #[cfg(feature = "resolver-corpus")]
        ProcessMode::ResolverCorpusProbe => {
            let stdin = io::stdin();
            let stdout = io::stdout();
            corpus::run_probe(&mut stdin.lock(), &mut stdout.lock())?;
        }
    }

    Ok(())
}

fn set_installed_startup(enabled: bool) -> Result<(), AppError> {
    let settings_file = SettingsFile::discover()?;
    if settings_file.mode() != SettingsMode::Installed {
        return Err(SettingsError::UnsupportedMode {
            operation: "startup configuration",
            mode: settings_file.mode(),
        }
        .into());
    }

    let registration = StartupRegistration::for_current_executable()?;
    if !enabled {
        registration.set_enabled(false)?;
    }

    let mut document = settings_file.load_or_create()?;
    let previous = document.settings().start_with_windows();
    if enabled {
        registration.set_enabled(true)?;
    }
    document.set_start_with_windows(enabled);
    if let Err(error) = settings_file.save(&document) {
        if enabled {
            let _ = registration.set_enabled(previous);
        }
        return Err(error.into());
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum AppError {
    Windows(windows::core::Error),
    DpiAwareness(DpiAwarenessError),
    WorkerManager(WorkerManagerError),
    Worker(WorkerSessionError),
    Resolver(ResolverError),
    Settings(SettingsError),
    #[cfg(feature = "resolver-corpus")]
    Corpus(corpus::CorpusError),
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows(error) => write!(formatter, "{error}"),
            Self::DpiAwareness(error) => write!(formatter, "DPI awareness: {error}"),
            Self::WorkerManager(error) => write!(formatter, "worker manager: {error}"),
            Self::Worker(error) => write!(formatter, "worker protocol: {error}"),
            Self::Resolver(error) => write!(formatter, "Explorer resolver: {error}"),
            Self::Settings(error) => write!(formatter, "settings: {error}"),
            #[cfg(feature = "resolver-corpus")]
            Self::Corpus(error) => write!(formatter, "resolver corpus: {error}"),
        }
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Windows(error) => Some(error),
            Self::DpiAwareness(error) => Some(error),
            Self::WorkerManager(error) => Some(error),
            Self::Worker(error) => Some(error),
            Self::Resolver(error) => Some(error),
            Self::Settings(error) => Some(error),
            #[cfg(feature = "resolver-corpus")]
            Self::Corpus(error) => Some(error),
        }
    }
}

impl AppError {
    const fn category(&self) -> &'static str {
        match self {
            Self::Windows(_) => "windows",
            Self::DpiAwareness(_) => "dpi-awareness",
            Self::WorkerManager(_) => "worker-manager",
            Self::Worker(_) => "worker-protocol",
            Self::Resolver(_) => "explorer-resolver",
            Self::Settings(_) => "settings",
            #[cfg(feature = "resolver-corpus")]
            Self::Corpus(_) => "resolver-corpus",
        }
    }
}

impl From<windows::core::Error> for AppError {
    fn from(error: windows::core::Error) -> Self {
        Self::Windows(error)
    }
}

impl From<DpiAwarenessError> for AppError {
    fn from(error: DpiAwarenessError) -> Self {
        Self::DpiAwareness(error)
    }
}

impl From<WorkerSessionError> for AppError {
    fn from(error: WorkerSessionError) -> Self {
        Self::Worker(error)
    }
}

impl From<WorkerManagerError> for AppError {
    fn from(error: WorkerManagerError) -> Self {
        Self::WorkerManager(error)
    }
}

impl From<ApplicationRunError> for AppError {
    fn from(error: ApplicationRunError) -> Self {
        match error {
            ApplicationRunError::Windows(error) => Self::Windows(error),
            ApplicationRunError::WorkerManager(error) => Self::WorkerManager(error),
        }
    }
}

impl From<ResolverError> for AppError {
    fn from(error: ResolverError) -> Self {
        Self::Resolver(error)
    }
}

impl From<SettingsError> for AppError {
    fn from(error: SettingsError) -> Self {
        Self::Settings(error)
    }
}

#[cfg(feature = "resolver-corpus")]
impl From<corpus::CorpusError> for AppError {
    fn from(error: corpus::CorpusError) -> Self {
        Self::Corpus(error)
    }
}
