#[cfg(not(windows))]
compile_error!("CursorPeek currently supports only Windows.");

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(crate) use self::windows::{ApartmentKind, ComApartment};
