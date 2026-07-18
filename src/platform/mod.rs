#[cfg(not(windows))]
compile_error!("CursorPeek currently supports only Windows.");

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(crate) use self::windows::{
    ApartmentKind, ComApartment, ContainedWorker, DPI_DIAGNOSTIC_SUCCESS, DpiAwarenessError,
    MessageWindow, ProcessError, WorkerPipes, verify_per_monitor_v2,
};
