use std::{
    marker::PhantomData,
    panic::{catch_unwind, AssertUnwindSafe},
    rc::Rc,
};

use windows::{
    core::{w, Error, Result, PCWSTR},
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, UnregisterClassW,
            HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
        },
    },
};

#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, SendMessageW, WM_APP};

const CLASS_NAME: PCWSTR = w!("CursorPeek.MessageWindow");

#[cfg(test)]
const TEST_PANIC_MESSAGE: u32 = WM_APP + 1;

pub(crate) struct MessageWindow {
    hwnd: HWND,
    _class: RegisteredWindowClass,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl MessageWindow {
    pub(crate) fn create() -> Result<Self> {
        let class = RegisteredWindowClass::register()?;

        // SAFETY: The class remains registered in `class`, all string pointers are static, the
        // module instance belongs to this process, and no creation parameter is passed. Using
        // HWND_MESSAGE creates a non-visible message-only window on the current thread.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                CLASS_NAME,
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                class.instance,
                None,
            )?
        };

        Ok(Self {
            hwnd,
            _class: class,
            _thread_affinity: PhantomData,
        })
    }

    #[cfg(test)]
    fn handle(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        // SAFETY: The owner is !Send and therefore drops on the creating thread. The HWND was
        // returned by CreateWindowExW and is destroyed before `_class` is dropped/unregistered.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct RegisteredWindowClass {
    instance: HINSTANCE,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl RegisteredWindowClass {
    fn register() -> Result<Self> {
        // SAFETY: A null module name asks for the current executable module. The returned handle
        // is borrowed and is deliberately stored as a plain HINSTANCE, never as Owned.
        let instance = HINSTANCE::from(unsafe { GetModuleHandleW(None)? });
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(message_window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };

        // SAFETY: `window_class` is fully initialized, its string and callback pointers remain
        // valid for the registration lifetime, and registration occurs on the owning thread.
        let atom = unsafe { RegisterClassW(&window_class) };
        if atom == 0 {
            return Err(Error::from_win32());
        }

        Ok(Self {
            instance,
            _thread_affinity: PhantomData,
        })
    }
}

impl Drop for RegisteredWindowClass {
    fn drop(&mut self) {
        // SAFETY: MessageWindow destroys its only HWND before this field is dropped. The class
        // name and borrowed process instance are the same values used during registration.
        unsafe {
            let _ = UnregisterClassW(CLASS_NAME, self.instance);
        }
    }
}

unsafe extern "system" fn message_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        dispatch_message(hwnd, message, wparam, lparam)
    }))
    .unwrap_or(LRESULT(0))
}

fn dispatch_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    #[cfg(test)]
    if message == TEST_PANIC_MESSAGE {
        panic!("intentional callback panic for containment testing");
    }

    // SAFETY: These are the untouched parameters supplied by Windows to this window procedure.
    // Unhandled messages are delegated to the system default procedure as required.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::{IsWindow, MessageWindow, SendMessageW, LRESULT, TEST_PANIC_MESSAGE};
    use std::thread;

    #[test]
    fn message_window_lifecycle_and_callback_boundary_are_sound() {
        thread::spawn(|| {
            let first = MessageWindow::create().expect("the message-only window should be created");
            let first_handle = first.handle();

            // SAFETY: `first_handle` belongs to the live window on this test thread.
            assert!(unsafe { IsWindow(first_handle).as_bool() });

            // SAFETY: The synchronous test message is sent to a live window on this thread. The
            // callback deliberately panics inside its catch_unwind boundary and must return zero.
            let panic_result =
                unsafe { SendMessageW(first_handle, TEST_PANIC_MESSAGE, None, None) };
            assert_eq!(panic_result, LRESULT(0));

            drop(first);

            // SAFETY: Checking a stale HWND with IsWindow is the documented validity probe.
            assert!(!unsafe { IsWindow(first_handle).as_bool() });

            for _ in 0..100 {
                let window = MessageWindow::create()
                    .expect("class cleanup should allow repeated message-window creation");
                let handle = window.handle();

                // SAFETY: `handle` belongs to the live window on this test thread.
                assert!(unsafe { IsWindow(handle).as_bool() });
                drop(window);

                // SAFETY: Checking a stale HWND with IsWindow is the documented validity probe.
                assert!(!unsafe { IsWindow(handle).as_bool() });
            }
        })
        .join()
        .expect("the message-window test thread should not panic");
    }
}
