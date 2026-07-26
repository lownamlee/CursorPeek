#[cfg(not(windows))]
compile_error!("CursorPeek currently supports only Windows.");

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(crate) use self::windows::{
    ApartmentKind, ApplicationRunError, ComApartment, ContainedWorker, DPI_DIAGNOSTIC_SUCCESS,
    DpiAwarenessError, MessageWindow, PREVIEW_WINDOW_DIAGNOSTIC_DURATION,
    PREVIEW_WINDOW_PRACTICE_DURATION, ProcessError, SingleInstance, WorkerPipes,
    activate_existing_instance, show_error, show_information, verify_per_monitor_v2,
};
