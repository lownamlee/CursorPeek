use crate::{
    mode::ProcessMode,
    platform::{ApartmentKind, ComApartment, MessageWindow},
};

pub(crate) fn run(process_mode: ProcessMode) -> windows::core::Result<()> {
    let apartment_kind = match process_mode {
        ProcessMode::Main => ApartmentKind::SingleThreaded,
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
        ProcessMode::PreviewWorker => {
            println!(
                "CursorPeek preview-worker/MTA foundation initialized; protocol is not active."
            );
        }
    }

    Ok(())
}
