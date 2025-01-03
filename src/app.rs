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
            let _message_window = MessageWindow::create()?;
            println!("CursorPeek main/STA foundation initialized with a message-only window.");
        }
        ProcessMode::PreviewWorker => {
            println!(
                "CursorPeek preview-worker/MTA foundation initialized; protocol is not active."
            );
        }
    }

    Ok(())
}
