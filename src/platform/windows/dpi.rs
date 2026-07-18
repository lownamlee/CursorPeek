use std::{error::Error, fmt};

use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    GetThreadDpiAwarenessContext,
};

pub(crate) const DIAGNOSTIC_SUCCESS: &str = "Per-Monitor V2 DPI awareness is active.";

pub(crate) fn verify_per_monitor_v2() -> Result<(), DpiAwarenessError> {
    // SAFETY: Both calls inspect DPI-awareness handles owned by the operating system. No handle is
    // retained or released by CursorPeek.
    let is_per_monitor_v2 = unsafe {
        let actual = GetThreadDpiAwarenessContext();
        AreDpiAwarenessContextsEqual(actual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).as_bool()
    };

    if is_per_monitor_v2 {
        Ok(())
    } else {
        Err(DpiAwarenessError)
    }
}

#[derive(Debug)]
pub(crate) struct DpiAwarenessError;

impl fmt::Display for DpiAwarenessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("embedded Per-Monitor V2 application manifest is not active")
    }
}

impl Error for DpiAwarenessError {}
