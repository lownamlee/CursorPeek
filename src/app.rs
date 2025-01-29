use std::{error::Error, fmt, io};

use crate::{
    hover::INPUT_DIAGNOSTIC_DURATION,
    mode::ProcessMode,
    platform::{ApartmentKind, ComApartment, MessageWindow},
    worker::{self, WorkerSessionError},
};

pub(crate) fn run(process_mode: ProcessMode) -> Result<(), AppError> {
    let apartment_kind = match process_mode {
        ProcessMode::Main | ProcessMode::InputDiagnostics => ApartmentKind::SingleThreaded,
        ProcessMode::PreviewWorker => ApartmentKind::MultiThreaded,
    };
    let _apartment = ComApartment::initialize(apartment_kind)?;

    match process_mode {
        ProcessMode::Main => {
            let message_window = MessageWindow::create()?;
            message_window.request_shutdown()?;
            message_window.run_message_loop()?;
            println!("CursorPeek main/STA foundation processed its message loop.");
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
        ProcessMode::PreviewWorker => {
            let stdin = io::stdin();
            let stdout = io::stdout();
            worker::run_diagnostic_session(&mut stdin.lock(), &mut stdout.lock())?;
        }
    }

    Ok(())
}

#[derive(Debug)]
pub(crate) enum AppError {
    Windows(windows::core::Error),
    Worker(WorkerSessionError),
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows(error) => write!(formatter, "{error}"),
            Self::Worker(error) => write!(formatter, "worker protocol: {error}"),
        }
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Windows(error) => Some(error),
            Self::Worker(error) => Some(error),
        }
    }
}

impl From<windows::core::Error> for AppError {
    fn from(error: windows::core::Error) -> Self {
        Self::Windows(error)
    }
}

impl From<WorkerSessionError> for AppError {
    fn from(error: WorkerSessionError) -> Self {
        Self::Worker(error)
    }
}
