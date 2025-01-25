use crate::{
    hover::INPUT_DIAGNOSTIC_DURATION,
    mode::ProcessMode,
    platform::{ApartmentKind, ComApartment, MessageWindow},
};

pub(crate) fn run(process_mode: ProcessMode) -> windows::core::Result<()> {
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
            println!(
                "CursorPeek preview-worker/MTA foundation initialized; protocol is not active."
            );
        }
    }

    Ok(())
}
