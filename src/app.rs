use std::{error::Error, fmt, io};

#[cfg(feature = "resolver-corpus")]
use crate::corpus;
use crate::{
    hover::INPUT_DIAGNOSTIC_DURATION,
    mode::ProcessMode,
    platform::{
        ApartmentKind, ApplicationRunError, ComApartment, DPI_DIAGNOSTIC_SUCCESS,
        DpiAwarenessError, MessageWindow, PREVIEW_WINDOW_DIAGNOSTIC_DURATION,
        PREVIEW_WINDOW_PRACTICE_DURATION, SingleInstance, activate_existing_instance,
        verify_per_monitor_v2,
    },
    preview::PreviewSize,
    resolver::{ExplorerResolver, ResolverError},
    settings::{SettingsError, SettingsFile},
    worker::{self, WorkerManagerError, WorkerSessionError},
};

pub(crate) fn run(process_mode: ProcessMode) -> Result<(), AppError> {
    verify_per_monitor_v2()?;

    let _single_instance_guard = if process_mode == ProcessMode::Main {
        match SingleInstance::acquire()? {
            Some(instance) => Some(instance),
            None => {
                activate_existing_instance()?;
                return Ok(());
            }
        }
    } else {
        None
    };

    let _apartment = match process_mode {
        ProcessMode::Main
        | ProcessMode::InputDiagnostics
        | ProcessMode::PreviewWindowDiagnostics
        | ProcessMode::PreviewWindowPracticeDiagnostics
        | ProcessMode::WorkerDiagnostics
        | ProcessMode::WorkerTimeoutDiagnostics => {
            Some(ComApartment::initialize(ApartmentKind::SingleThreaded)?)
        }
        ProcessMode::DpiDiagnostics | ProcessMode::PreviewWorker => None,
        #[cfg(feature = "resolver-corpus")]
        ProcessMode::ResolverCorpusProbe => None,
    };

    match process_mode {
        ProcessMode::Main => {
            let settings_file = SettingsFile::discover()?;
            let settings = settings_file.load_or_create()?;
            let preview_size = PreviewSize::new(
                u32::from(settings.settings().preview_width()),
                u32::from(settings.settings().preview_height()),
            );
            let message_window = MessageWindow::create_for_application(
                settings.settings().dwell_delay(),
                preview_size,
            )?;
            let worker_manager =
                worker::WorkerManager::start(settings.settings().legacy_encoding().clone())?;
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
