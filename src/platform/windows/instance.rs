use std::{
    os::windows::io::{FromRawHandle, OwnedHandle},
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{E_FAIL, ERROR_ALREADY_EXISTS, GetLastError, HWND},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, FindWindowExW, GetWindowThreadProcessId, PostMessageW,
        },
    },
    core::{PCWSTR, Result, w},
};

use super::window::{ACTIVATE_MESSAGE, CLASS_NAME};

const ACTIVATION_WAIT: Duration = Duration::from_secs(5);
const ACTIVATION_RETRY: Duration = Duration::from_millis(25);

pub(crate) struct SingleInstance {
    _handle: OwnedHandle,
}

impl SingleInstance {
    pub(crate) fn acquire() -> Result<Option<Self>> {
        Self::acquire_named(w!("Local\\CursorPeek.SingleInstance"))
    }

    fn acquire_named(name: PCWSTR) -> Result<Option<Self>> {
        // SAFETY: Default security is appropriate for this per-session user application, the
        // handle is non-inheritable, initial ownership is not requested, and `name` is terminated.
        let handle = unsafe { CreateMutexW(None, false, name)? };
        // GetLastError must be sampled immediately after successful CreateMutexW: it distinguishes
        // an atomic first creation from opening the already-live named object.
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        // SAFETY: CreateMutexW returned a live owned HANDLE. OwnedHandle closes it exactly once;
        // ReleaseMutex is unnecessary because initial ownership was never requested.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };

        if already_exists {
            drop(handle);
            Ok(None)
        } else {
            Ok(Some(Self { _handle: handle }))
        }
    }
}

pub(crate) fn activate_existing_instance() -> Result<()> {
    let deadline = Instant::now() + ACTIVATION_WAIT;
    let hwnd = loop {
        // SAFETY: HWND_MESSAGE requests only message-only children. Both class and title arguments
        // are terminated and no returned handle ownership is transferred.
        if let Ok(hwnd) =
            unsafe { FindWindowExW(Some(HWND_MESSAGE), None, CLASS_NAME, PCWSTR::null()) }
        {
            break hwnd;
        }
        if Instant::now() >= deadline {
            return Err(windows::core::Error::from_hresult(E_FAIL));
        }
        thread::sleep(ACTIVATION_RETRY);
    };

    let mut process_id = 0;
    // SAFETY: `hwnd` is the live message-only window found above and the output pointer is valid.
    let thread_id = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)) };
    if thread_id == 0 || process_id == 0 {
        return Err(windows::core::Error::from_hresult(E_FAIL));
    }

    // If this second launch owns foreground permission, transfer it to the existing process so
    // its native menu can satisfy SetForegroundWindow. The activation message remains useful even
    // when policy rejects this optional transfer.
    let _ = unsafe { AllowSetForegroundWindow(process_id) };

    // SAFETY: The private message carries no pointers and targets the exact live message window.
    unsafe {
        PostMessageW(
            Some(hwnd),
            ACTIVATE_MESSAGE,
            Default::default(),
            Default::default(),
        )
    }
}

const HWND_MESSAGE: HWND = HWND(-3_isize as *mut core::ffi::c_void);

#[cfg(test)]
mod tests {
    use super::SingleInstance;
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows::core::PCWSTR;

    static NEXT_NAME: AtomicU32 = AtomicU32::new(1);

    #[test]
    fn named_mutex_allows_one_owner_and_recovers_after_every_handle_closes() {
        let sequence = NEXT_NAME.fetch_add(1, Ordering::Relaxed);
        let name = format!("Local\\CursorPeek.Test.{}.{}", std::process::id(), sequence);
        let mut wide: Vec<u16> = name.encode_utf16().collect();
        wide.push(0);
        let name = PCWSTR(wide.as_ptr());

        let first = SingleInstance::acquire_named(name)
            .expect("the unique test mutex should be created")
            .expect("the first caller should own the instance guard");
        assert!(
            SingleInstance::acquire_named(name)
                .expect("the existing mutex should be opened")
                .is_none(),
            "the second caller must be redirected"
        );

        drop(first);
        assert!(
            SingleInstance::acquire_named(name)
                .expect("the released name should be creatable again")
                .is_some(),
            "closing the final handle must release the instance name"
        );
    }
}
