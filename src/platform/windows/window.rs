use std::{
    marker::PhantomData,
    panic::{catch_unwind, AssertUnwindSafe},
    rc::Rc,
    time::{Duration, Instant},
};

use super::explorer::is_explorer_window_at;
#[cfg(test)]
use super::input::registered_raw_mouse;
use super::input::{
    physical_cursor_position, read_raw_mouse_activity, system_hover_rectangle, RawMouseActivity,
    RawMouseInputRegistration,
};

use crate::hover::{DwellTimerEvent, HoverState, PhysicalScreenPoint, DEFAULT_DWELL_DELAY};

use windows::{
    core::{w, Error, Result, PCWSTR},
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            KillTimer, PostMessageW, RegisterClassW, SetTimer, TranslateMessage, UnregisterClassW,
            HWND_MESSAGE, MSG, USER_TIMER_MAXIMUM, USER_TIMER_MINIMUM, WINDOW_EX_STYLE,
            WINDOW_STYLE, WM_APP, WM_INPUT, WM_TIMER, WNDCLASSW,
        },
    },
};

#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

const CLASS_NAME: PCWSTR = w!("CursorPeek.MessageWindow");
const SHUTDOWN_MESSAGE: u32 = WM_APP + 1;
const DWELL_TIMER_ID: usize = 1;

#[cfg(test)]
const TEST_PANIC_MESSAGE: u32 = WM_APP + 2;

pub(crate) struct MessageWindow {
    hwnd: HWND,
    dwell_timer: Option<WindowDwellTimer>,
    hover_state: HoverState,
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
            dwell_timer: Some(WindowDwellTimer::new(hwnd)),
            hover_state: HoverState::new(DEFAULT_DWELL_DELAY),
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

    pub(crate) fn run_message_loop(mut self) -> Result<()> {
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

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == DWELL_TIMER_ID
            {
                self.handle_dwell_timer(Instant::now());
                continue;
            }

            let raw_mouse = if message.hwnd == self.hwnd && message.message == WM_INPUT {
                Some(read_raw_mouse_activity(message.lParam))
            } else {
                None
            };

            // SAFETY: `message` was populated by a successful GetMessageW call and remains valid
            // through translation and synchronous dispatch on this owning thread.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }

            if let Some(raw_mouse) = raw_mouse {
                self.handle_raw_mouse(raw_mouse);
            }
        }
    }

    fn handle_raw_mouse(&mut self, raw_mouse: Result<Option<RawMouseActivity>>) {
        match raw_mouse {
            Ok(Some(activity)) if activity.is_relevant() => match physical_cursor_position() {
                Ok(point) => {
                    self.restart_dwell(PhysicalScreenPoint::new(point.x, point.y), Instant::now())
                }
                Err(_) => self.cancel_dwell(),
            },
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => self.cancel_dwell(),
        }
    }

    fn restart_dwell(&mut self, point: PhysicalScreenPoint, now: Instant) {
        let interval = self.hover_state.restart(point, now);
        let armed = self
            .dwell_timer
            .as_mut()
            .is_some_and(|timer| timer.arm(interval).is_ok());

        if !armed {
            self.cancel_dwell();
        }
    }

    fn cancel_dwell(&mut self) {
        self.hover_state.cancel();
        if let Some(timer) = self.dwell_timer.as_mut() {
            let _ = timer.stop();
        }
    }

    fn handle_dwell_timer(&mut self, now: Instant) {
        if let Some(timer) = self.dwell_timer.as_mut() {
            let _ = timer.stop();
        }

        match self.hover_state.on_timer(now) {
            DwellTimerEvent::Inactive => {}
            DwellTimerEvent::Rearm(remaining) => {
                let armed = self
                    .dwell_timer
                    .as_mut()
                    .is_some_and(|timer| timer.arm(remaining).is_ok());
                if !armed {
                    self.cancel_dwell();
                }
            }
            DwellTimerEvent::Candidate(candidate) => {
                let ready = physical_cursor_position()
                    .map(|point| PhysicalScreenPoint::new(point.x, point.y))
                    .and_then(|current| {
                        let rectangle = system_hover_rectangle(candidate.anchor())?;
                        Ok(candidate.validate(current, rectangle))
                    })
                    .ok()
                    .flatten();

                let Some(ready) = ready else {
                    self.cancel_dwell();
                    return;
                };

                // The resolver handoff will consume this validated generation/current-point pair
                // in the next Milestone 1 slice.
                let (generation, point) = ready.into_parts();
                if !is_explorer_window_at(point) {
                    self.cancel_dwell();
                    return;
                }

                let PhysicalScreenPoint { x, y } = point;
                let _ = (generation, x, y);
            }
        }
    }

    #[cfg(test)]
    fn handle(&self) -> HWND {
        self.hwnd
    }

    #[cfg(test)]
    fn dwell_timer_is_armed(&self) -> bool {
        self.dwell_timer
            .as_ref()
            .is_some_and(WindowDwellTimer::is_armed)
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        drop(self.dwell_timer.take());
        drop(self.raw_mouse_input.take());

        // SAFETY: The owner is !Send and therefore drops on the creating thread. The dwell timer
        // has been stopped and Raw Input unregistered. The HWND returned by CreateWindowExW is
        // destroyed before `_class` is dropped/unregistered.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct WindowDwellTimer {
    hwnd: HWND,
    armed: bool,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl WindowDwellTimer {
    fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            armed: false,
            _thread_affinity: PhantomData,
        }
    }

    fn arm(&mut self, interval: Duration) -> Result<()> {
        let interval_ms = timer_interval_ms(interval);

        // SAFETY: `hwnd` is a live window owned by this calling UI thread. The fixed nonzero ID
        // belongs only to that window, the interval is clamped to Windows' documented range, and
        // no callback pointer is supplied, so expiry is delivered as WM_TIMER.
        let timer = unsafe { SetTimer(self.hwnd, DWELL_TIMER_ID, interval_ms, None) };
        if timer == 0 {
            let error = Error::from_win32();
            let _ = self.stop();
            return Err(error);
        }

        self.armed = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if !self.armed {
            return Ok(());
        }

        // SAFETY: This uses the same live owning HWND and fixed ID passed to SetTimer. The token
        // never leaves the UI thread and retries in Drop if Windows reports a failure.
        let result = unsafe { KillTimer(self.hwnd, DWELL_TIMER_ID) };
        if result.is_ok() {
            self.armed = false;
        }
        result
    }

    #[cfg(test)]
    fn is_armed(&self) -> bool {
        self.armed
    }
}

impl Drop for WindowDwellTimer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn timer_interval_ms(interval: Duration) -> u32 {
    let rounded_up_ms = interval.as_nanos().div_ceil(1_000_000);
    rounded_up_ms.clamp(
        u128::from(USER_TIMER_MINIMUM),
        u128::from(USER_TIMER_MAXIMUM),
    ) as u32
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
    // Every WM_INPUT reaches this default procedure because the owning loop copies it before
    // dispatch and applies safe state only after required foreground cleanup.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::{
        registered_raw_mouse, timer_interval_ms, IsWindow, MessageWindow, PostMessageW,
        DWELL_TIMER_ID, LPARAM, TEST_PANIC_MESSAGE, WM_TIMER, WPARAM,
    };
    use crate::hover::PhysicalScreenPoint;
    use crate::platform::windows::explorer::is_explorer_window;
    use std::{
        thread,
        time::{Duration, Instant},
    };
    use windows::Win32::UI::Input::RIDEV_INPUTSINK;

    #[test]
    fn message_window_lifecycle_and_callback_boundary_are_sound() {
        thread::spawn(|| {
            let mut first =
                MessageWindow::create().expect("the message-only window should be created");
            let first_handle = first.handle();

            // SAFETY: `first_handle` belongs to the live window on this test thread.
            assert!(unsafe { IsWindow(first_handle).as_bool() });
            assert!(
                !is_explorer_window(first_handle),
                "the private message window must fail the Explorer candidate gate"
            );
            let first_registration = registered_raw_mouse()
                .expect("the process registration should be queryable")
                .expect("the raw mouse should be registered");
            assert_eq!(first_registration.hwndTarget, first_handle);
            assert_eq!(first_registration.dwFlags, RIDEV_INPUTSINK);

            first.restart_dwell(PhysicalScreenPoint::new(-10, 20), Instant::now());
            assert!(first.dwell_timer_is_armed());
            first.handle_dwell_timer(Instant::now());
            assert!(
                first.dwell_timer_is_armed(),
                "an early timer must re-arm the remaining dwell"
            );

            // SAFETY: This posts an early message using the live window's owned timer ID and no
            // callback pointer. The monotonic state must re-arm the remaining dwell instead of
            // treating message arrival alone as expiry.
            unsafe { PostMessageW(first_handle, WM_TIMER, WPARAM(DWELL_TIMER_ID), LPARAM(0)) }
                .expect("the early timer message should be queued");

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

    #[test]
    fn timer_intervals_round_up_and_respect_windows_limits() {
        assert_eq!(timer_interval_ms(Duration::from_nanos(1)), 10);
        assert_eq!(timer_interval_ms(Duration::from_micros(10_001)), 11);
        assert_eq!(timer_interval_ms(Duration::from_millis(400)), 400);
        assert_eq!(timer_interval_ms(Duration::MAX), 2_147_483_647);
    }
}
