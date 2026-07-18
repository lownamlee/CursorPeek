#[cfg(not(windows))]
compile_error!("CursorPeek currently supports only Windows.");

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(crate) use self::windows::{
    ApartmentKind, ComApartment, ContainedWorker, DPI_DIAGNOSTIC_SUCCESS, DpiAwarenessError,
    MessageWindow, PREVIEW_WINDOW_DIAGNOSTIC_DURATION, PREVIEW_WINDOW_PRACTICE_DURATION,
    ProcessError, WorkerPipes, verify_per_monitor_v2,
};
