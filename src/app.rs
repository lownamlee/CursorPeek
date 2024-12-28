use crate::{
    mode::ProcessMode,
    platform::{ApartmentKind, ComApartment},
};

pub(crate) fn run(process_mode: ProcessMode) -> windows::core::Result<()> {
    let apartment_kind = match process_mode {
        ProcessMode::Main => ApartmentKind::SingleThreaded,
        ProcessMode::PreviewWorker => ApartmentKind::MultiThreaded,
    };
    let _apartment = ComApartment::initialize(apartment_kind)?;

    match process_mode {
        ProcessMode::Main => {
            println!("CursorPeek foundation initialized in main/STA mode.");
        }
        ProcessMode::PreviewWorker => {
            println!(
                "CursorPeek preview-worker/MTA foundation initialized; protocol is not active."
            );
        }
    }

    Ok(())
}
