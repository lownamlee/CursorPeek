use std::{
    marker::PhantomData,
    panic::{catch_unwind, AssertUnwindSafe},
    rc::Rc,
};

#[cfg(test)]
use super::input::registered_raw_mouse;
use super::input::{read_raw_mouse_activity, RawMouseInputRegistration};

use windows::{
    core::{w, Error, Result, PCWSTR},
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            PostMessageW, RegisterClassW, TranslateMessage, UnregisterClassW, HWND_MESSAGE, MSG,
            WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_INPUT, WNDCLASSW,
        },
    },
};

#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

const CLASS_NAME: PCWSTR = w!("CursorPeek.MessageWindow");
const SHUTDOWN_MESSAGE: u32 = WM_APP + 1;

#[cfg(test)]
const TEST_PANIC_MESSAGE: u32 = WM_APP + 2;

pub(crate) struct MessageWindow {
    hwnd: HWND,
    raw_mouse_input: Option<RawMouseInputRegistration>,
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

        let mut window = Self {
            hwnd,
            raw_mouse_input: None,
            _class: class,
            _thread_affinity: PhantomData,
        };
        window.raw_mouse_input = Some(RawMouseInputRegistration::register(hwnd)?);

        Ok(window)
    }

    pub(crate) fn request_shutdown(&self) -> Result<()> {
        // SAFETY: `self.hwnd` is owned by this live MessageWindow. The private message carries no
        // pointers or borrowed data, so its parameters remain valid until the queue processes it.
        unsafe { PostMessageW(self.hwnd, SHUTDOWN_MESSAGE, WPARAM(0), LPARAM(0)) }
    }

    pub(crate) fn run_message_loop(self) -> Result<()> {
        let mut message = MSG::default();

        loop {
            // SAFETY: `message` is valid writable storage for the duration of the call. No HWND or
            // range filter is used, so this thread's complete queue is serviced.
            let status = unsafe { GetMessageW(&mut message, None, 0, 0) };
            if status.0 < 0 {
                return Err(Error::from_win32());
            }
            if status.0 == 0 {
                return Ok(());
            }

            if message.hwnd == self.hwnd && message.message == SHUTDOWN_MESSAGE {
                return Ok(());
            }

            // SAFETY: `message` was populated by a successful GetMessageW call and remains valid
            // through translation and synchronous dispatch on this owning thread.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    #[cfg(test)]
    fn handle(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        drop(self.raw_mouse_input.take());

        // SAFETY: The owner is !Send and therefore drops on the creating thread. Raw Input has
        // already been unregistered, and the HWND returned by CreateWindowExW is destroyed before
        // `_class` is dropped/unregistered.
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

    if message == WM_INPUT {
        let _ = read_raw_mouse_activity(lparam);
    }

    // SAFETY: These are the untouched parameters supplied by Windows to this window procedure.
    // Every WM_INPUT is also delegated because foreground raw input requires system cleanup.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::{
        registered_raw_mouse, IsWindow, MessageWindow, PostMessageW, LPARAM, TEST_PANIC_MESSAGE,
        WPARAM,
    };
    use std::thread;
    use windows::Win32::UI::Input::RIDEV_INPUTSINK;

    #[test]
    fn message_window_lifecycle_and_callback_boundary_are_sound() {
        thread::spawn(|| {
            let first = MessageWindow::create().expect("the message-only window should be created");
            let first_handle = first.handle();

            // SAFETY: `first_handle` belongs to the live window on this test thread.
            assert!(unsafe { IsWindow(first_handle).as_bool() });
            let first_registration = registered_raw_mouse()
                .expect("the process registration should be queryable")
                .expect("the raw mouse should be registered");
            assert_eq!(first_registration.hwndTarget, first_handle);
            assert_eq!(first_registration.dwFlags, RIDEV_INPUTSINK);

            // SAFETY: The live window owns the receiving queue and this private message carries
            // only zero-valued parameters. Dispatch deliberately panics inside the WNDPROC's
            // catch_unwind boundary.
            unsafe { PostMessageW(first_handle, TEST_PANIC_MESSAGE, WPARAM(0), LPARAM(0)) }
                .expect("the callback test message should be queued");
            first
                .request_shutdown()
                .expect("the shutdown message should be queued");
            first
                .run_message_loop()
                .expect("the queued messages should be pumped");

            // SAFETY: Checking a stale HWND with IsWindow is the documented validity probe.
            assert!(!unsafe { IsWindow(first_handle).as_bool() });
            assert!(
                registered_raw_mouse()
                    .expect("the process registration should be queryable")
                    .is_none(),
                "raw mouse input should be unregistered before window teardown"
            );

            for _ in 0..100 {
                let window = MessageWindow::create()
                    .expect("class cleanup should allow repeated message-window creation");
                let handle = window.handle();

                // SAFETY: `handle` belongs to the live window on this test thread.
                assert!(unsafe { IsWindow(handle).as_bool() });
                let registration = registered_raw_mouse()
                    .expect("the process registration should be queryable")
                    .expect("the raw mouse should be registered");
                assert_eq!(registration.hwndTarget, handle);
                assert_eq!(registration.dwFlags, RIDEV_INPUTSINK);
                drop(window);

                // SAFETY: Checking a stale HWND with IsWindow is the documented validity probe.
                assert!(!unsafe { IsWindow(handle).as_bool() });
                assert!(
                    registered_raw_mouse()
                        .expect("the process registration should be queryable")
                        .is_none(),
                    "raw mouse input should be removed on every lifecycle"
                );
            }
        })
        .join()
        .expect("the message-window test thread should not panic");
    }
}
