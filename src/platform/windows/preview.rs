use std::{marker::PhantomData, mem::size_of, panic::AssertUnwindSafe, rc::Rc};

use crate::{
    hover::PhysicalScreenPoint,
    preview::{PreviewPlacement, ScreenRect, place_diagnostic_preview},
};

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{
            COLOR_HIGHLIGHT, GetMonitorInfoW, GetSysColorBrush, MONITOR_DEFAULTTONEAREST,
            MONITORINFO, MonitorFromPoint,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOPMOST, MA_NOACTIVATEANDEAT,
                RegisterClassW, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
                SWP_NOZORDER, SWP_SHOWWINDOW, SendMessageW, SetWindowPos, UnregisterClassW,
                WINDOW_EX_STYLE, WM_MOUSEACTIVATE, WNDCLASSW, WS_BORDER, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{Error, PCWSTR, Result, w},
};

const CLASS_NAME: PCWSTR = w!("CursorPeek.PreviewWindow");

pub(crate) struct PreviewWindow {
    hwnd: HWND,
    _class: RegisteredPreviewClass,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl PreviewWindow {
    pub(crate) fn create() -> Result<Self> {
        let class = RegisteredPreviewClass::register()?;
        let ex_style: WINDOW_EX_STYLE = WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;

        // SAFETY: The registered class remains owned by `class`, all strings are static, the
        // module instance belongs to this process, and the top-level popup starts hidden.
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                CLASS_NAME,
                w!("CursorPeek preview diagnostic"),
                WS_POPUP | WS_BORDER,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(class.instance),
                None,
            )?
        };

        Ok(Self {
            hwnd,
            _class: class,
            _thread_affinity: PhantomData,
        })
    }

    pub(crate) fn show_at(&self, anchor: PhysicalScreenPoint) -> Result<PreviewPlacement> {
        // SAFETY: `self.hwnd` is a live hidden top-level window owned by this UI thread. Moving it
        // to the anchor monitor before querying DPI associates the correct monitor without making
        // the one-pixel setup window visible or active.
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                anchor.x,
                anchor.y,
                1,
                1,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )?;
        }

        // SAFETY: `self.hwnd` remains live after the successful positioning call.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 {
            return Err(Error::from_thread());
        }

        let monitor = unsafe {
            MonitorFromPoint(
                POINT {
                    x: anchor.x,
                    y: anchor.y,
                },
                MONITOR_DEFAULTTONEAREST,
            )
        };
        if monitor.0.is_null() {
            return Err(Error::from_thread());
        }

        let mut monitor_info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `monitor` is the borrowed nearest-monitor handle and `monitor_info` is valid
        // writable storage with its required size initialized.
        if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
            return Err(Error::from_thread());
        }

        let placement = place_diagnostic_preview(
            anchor,
            ScreenRect {
                left: monitor_info.rcWork.left,
                top: monitor_info.rcWork.top,
                right: monitor_info.rcWork.right,
                bottom: monitor_info.rcWork.bottom,
            },
            dpi,
        )
        .ok_or_else(Error::from_thread)?;

        // SAFETY: The same live HWND is positioned wholly inside the selected work area. TOPMOST
        // plus SHOWWINDOW displays it, while NOACTIVATE and WS_EX_NOACTIVATE preserve focus.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                placement.x,
                placement.y,
                placement.width,
                placement.height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
        }

        Ok(placement)
    }

    pub(crate) fn hide(&self) -> Result<()> {
        // SAFETY: The live HWND belongs to this UI thread. The flags hide it without changing
        // position, size, Z order, or activation.
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_HIDEWINDOW | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            )
        }
    }

    #[cfg(test)]
    pub(crate) fn is_visible(&self) -> bool {
        // SAFETY: The handle is owned and live for `self`.
        unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(self.hwnd).as_bool() }
    }

    pub(crate) fn eats_mouse_activation(&self) -> bool {
        // SAFETY: This synchronous policy probe sends the popup's documented activation message
        // with zeroed informational parameters and retains no borrowed data.
        let result = unsafe { SendMessageW(self.hwnd, WM_MOUSEACTIVATE, None, None) };
        result.0 == isize::try_from(MA_NOACTIVATEANDEAT).expect("the constant fits isize")
    }

    pub(crate) fn handle(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        let _ = self.hide();

        // SAFETY: The owner is !Send and drops on the creating UI thread. The HWND is destroyed
        // before the registered class field is released.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct RegisteredPreviewClass {
    instance: HINSTANCE,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl RegisteredPreviewClass {
    fn register() -> Result<Self> {
        let instance = HINSTANCE::from(unsafe { GetModuleHandleW(None)? });
        // SAFETY: This returns a borrowed system-color brush. Windows owns it for the process
        // lifetime; CursorPeek neither deletes nor transfers it.
        let background = unsafe { GetSysColorBrush(COLOR_HIGHLIGHT) };
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(preview_window_proc),
            hInstance: instance,
            hbrBackground: background,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };

        // SAFETY: The class definition is initialized and every referenced callback/string remains
        // valid until the class is unregistered on this same thread.
        let atom = unsafe { RegisterClassW(&window_class) };
        if atom == 0 {
            return Err(Error::from_thread());
        }

        Ok(Self {
            instance,
            _thread_affinity: PhantomData,
        })
    }
}

impl Drop for RegisteredPreviewClass {
    fn drop(&mut self) {
        // SAFETY: PreviewWindow destroys its only HWND before this field drops. The background is
        // a borrowed system brush and must not be deleted by CursorPeek.
        unsafe {
            let _ = UnregisterClassW(CLASS_NAME, Some(self.instance));
        }
    }
}

unsafe extern "system" fn preview_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        if message == WM_MOUSEACTIVATE {
            return LRESULT(isize::try_from(MA_NOACTIVATEANDEAT).expect("the constant fits isize"));
        }

        // SAFETY: These are the untouched parameters supplied by Windows to this WNDPROC.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }))
    .unwrap_or(LRESULT(0))
}

#[cfg(test)]
mod tests {
    use super::PreviewWindow;
    use std::thread;
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    #[test]
    fn preview_window_lifecycle_and_mouse_activation_policy_are_sound() {
        thread::spawn(|| {
            for _ in 0..100 {
                let preview =
                    PreviewWindow::create().expect("the preview window should be created");
                let handle = preview.handle();

                // SAFETY: `handle` belongs to the live preview on this test thread.
                assert!(unsafe { IsWindow(Some(handle)).as_bool() });
                assert!(!preview.is_visible());
                assert!(preview.eats_mouse_activation());
                drop(preview);

                // SAFETY: IsWindow is the documented stale-HWND validity probe.
                assert!(!unsafe { IsWindow(Some(handle)).as_bool() });
            }
        })
        .join()
        .expect("the preview-window test thread should not panic");
    }
}
