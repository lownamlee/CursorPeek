use std::{
    marker::PhantomData,
    mem::{MaybeUninit, size_of},
    rc::Rc,
};

use crate::hover::{HoverRectangle, PhysicalScreenPoint};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, POINT},
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_UNAWARE, GetDpiForWindow,
                SetThreadDpiAwarenessContext,
            },
            Input::{
                GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE,
                RAWINPUTDEVICE_FLAGS, RAWINPUTHEADER, RAWMOUSE, RID_INPUT, RIDEV_INPUTSINK,
                RIDEV_REMOVE, RIM_TYPEMOUSE, RegisterRawInputDevices,
            },
            WindowsAndMessaging::{
                GetPhysicalCursorPos, SPI_GETMOUSEHOVERHEIGHT, SPI_GETMOUSEHOVERWIDTH,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
                WindowFromPhysicalPoint,
            },
        },
    },
    core::{Error, Result},
};

#[cfg(test)]
use windows::Win32::UI::Input::GetRegisteredRawInputDevices;

const GENERIC_DESKTOP_USAGE_PAGE: u16 = 0x01;
const MOUSE_USAGE: u16 = 0x02;
const RAW_INPUT_DEVICE_SIZE: u32 = size_of::<RAWINPUTDEVICE>() as u32;
const RAW_INPUT_SIZE: u32 = size_of::<RAWINPUT>() as u32;
const RAW_INPUT_HEADER_SIZE: u32 = size_of::<RAWINPUTHEADER>() as u32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct RawMouseActivity {
    moved: bool,
    button_or_wheel: bool,
}

impl RawMouseActivity {
    pub(super) fn is_relevant(self) -> bool {
        self.moved || self.button_or_wheel
    }

    pub(super) fn moved(self) -> bool {
        self.moved
    }

    pub(super) fn interrupted(self) -> bool {
        self.button_or_wheel
    }

    fn from_mouse(mouse: RAWMOUSE) -> Self {
        // SAFETY: RAWMOUSE defines `Anonymous.Anonymous` as the active view of the buttons
        // union. Reading it does not depend on a separate tag, and the complete RAWMOUSE value
        // was initialized before this classifier is called.
        let button_flags = unsafe { mouse.Anonymous.Anonymous.usButtonFlags };

        Self {
            moved: mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 != 0
                || mouse.lLastX != 0
                || mouse.lLastY != 0,
            button_or_wheel: button_flags != 0,
        }
    }
}

pub(super) fn physical_cursor_position() -> Result<POINT> {
    let mut point = POINT::default();

    // SAFETY: `point` is valid writable storage for the duration of the call. Windows writes one
    // complete POINT in physical screen coordinates or returns an error without transferring
    // ownership.
    unsafe {
        GetPhysicalCursorPos(&mut point)?;
    }

    Ok(point)
}

pub(super) fn system_hover_rectangle(anchor: PhysicalScreenPoint) -> Result<HoverRectangle> {
    let point = POINT {
        x: anchor.x,
        y: anchor.y,
    };

    // SAFETY: `point` contains physical screen coordinates from GetPhysicalCursorPos. The
    // returned HWND is borrowed only long enough to query its DPI and is never released here.
    let target = unsafe { WindowFromPhysicalPoint(point) };
    if target.0.is_null() {
        return Err(Error::from_thread());
    }

    // SAFETY: `target` is the live borrowed HWND returned immediately above. A zero result is
    // documented as failure and is rejected before it reaches safe scaling arithmetic.
    let target_dpi = unsafe { GetDpiForWindow(target) };
    if target_dpi == 0 {
        return Err(Error::from_thread());
    }

    let (width, height) = hover_dimensions_at_96_dpi()?;
    HoverRectangle::from_96_dpi(width, height, target_dpi).ok_or_else(Error::from_thread)
}

fn hover_dimensions_at_96_dpi() -> Result<(u32, u32)> {
    let mut context = ThreadDpiContext::enter_unaware()?;
    let mut width = 0_u32;
    let mut height = 0_u32;

    let query_result: Result<()> = (|| {
        // SAFETY: Each output pointer addresses one live, aligned u32 for the complete call.
        // These GET actions require uiParam/fWinIni zero and write only their documented UINT.
        unsafe {
            SystemParametersInfoW(
                SPI_GETMOUSEHOVERWIDTH,
                0,
                Some((&mut width as *mut u32).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
            )?;
            SystemParametersInfoW(
                SPI_GETMOUSEHOVERHEIGHT,
                0,
                Some((&mut height as *mut u32).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
            )?;
        }
        Ok(())
    })();
    let restore_result = context.restore();

    query_result?;
    restore_result?;
    Ok((width, height))
}

pub(super) fn read_raw_mouse_activity(lparam: LPARAM) -> Result<Option<RawMouseActivity>> {
    let handle = HRAWINPUT(lparam.0 as _);
    let mut raw_input = MaybeUninit::<RAWINPUT>::uninit();
    let mut buffer_size = RAW_INPUT_SIZE;

    // SAFETY: WM_INPUT supplies `lparam` as a borrowed HRAWINPUT valid while this callback runs.
    // `raw_input` is correctly aligned, has RAW_INPUT_SIZE writable bytes, and remains alive for
    // the call. `buffer_size` is initialized to that capacity, and RAW_INPUT_HEADER_SIZE is the
    // exact generated header size. The returned byte count is checked before initialization.
    let copied = unsafe {
        GetRawInputData(
            handle,
            RID_INPUT,
            Some(raw_input.as_mut_ptr().cast()),
            &mut buffer_size,
            RAW_INPUT_HEADER_SIZE,
        )
    };

    if copied == u32::MAX {
        return Err(Error::from_thread());
    }
    if copied != RAW_INPUT_SIZE || buffer_size != RAW_INPUT_SIZE {
        return Ok(None);
    }

    // SAFETY: GetRawInputData reported that it initialized every byte of the fixed RAWINPUT
    // destination. Short writes were rejected above.
    let raw_input = unsafe { raw_input.assume_init() };
    if raw_input.header.dwSize != RAW_INPUT_SIZE || raw_input.header.dwType != RIM_TYPEMOUSE.0 {
        return Ok(None);
    }

    // SAFETY: The validated RAWINPUT header identifies the active union member as RAWMOUSE.
    let mouse = unsafe { raw_input.data.mouse };
    Ok(Some(RawMouseActivity::from_mouse(mouse)))
}

struct ThreadDpiContext {
    previous: Option<DPI_AWARENESS_CONTEXT>,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl ThreadDpiContext {
    fn enter_unaware() -> Result<Self> {
        // SAFETY: This changes only the calling thread. The returned previous context is retained
        // by this !Send guard and restored before the platform query returns.
        let previous = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE) };
        if previous.0.is_null() {
            return Err(Error::from_thread());
        }

        Ok(Self {
            previous: Some(previous),
            _thread_affinity: PhantomData,
        })
    }

    fn restore(&mut self) -> Result<()> {
        let Some(previous) = self.previous else {
            return Ok(());
        };

        // SAFETY: `previous` came from the successful context switch on this same thread. It is
        // consumed only after Windows reports that the prior context was restored.
        let replaced = unsafe { SetThreadDpiAwarenessContext(previous) };
        if replaced.0.is_null() {
            return Err(Error::from_thread());
        }

        self.previous = None;
        Ok(())
    }
}

impl Drop for ThreadDpiContext {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            // SAFETY: The !Send guard drops on the thread where the successful switch occurred.
            // This is the best-effort retry used only if explicit restoration reported failure.
            unsafe {
                let _ = SetThreadDpiAwarenessContext(previous);
            }
        }
    }
}

pub(super) struct RawMouseInputRegistration {
    _thread_affinity: PhantomData<Rc<()>>,
}

impl RawMouseInputRegistration {
    pub(super) fn register(target: HWND) -> Result<Self> {
        let device = raw_mouse_device(RIDEV_INPUTSINK, target);

        // SAFETY: The one-element slice is correctly aligned and uses the exact generated
        // structure size. `target` is a live window owned by the calling UI thread, and
        // RIDEV_INPUTSINK is valid only because that non-null target is supplied.
        unsafe {
            RegisterRawInputDevices(&[device], RAW_INPUT_DEVICE_SIZE)?;
        }

        Ok(Self {
            _thread_affinity: PhantomData,
        })
    }
}

impl Drop for RawMouseInputRegistration {
    fn drop(&mut self) {
        let removal = raw_mouse_device(RIDEV_REMOVE, HWND::default());

        // SAFETY: The slice is aligned and sized as required. Microsoft requires a null target
        // when RIDEV_REMOVE is used. This !Send token is explicitly dropped by MessageWindow on
        // the owning UI thread before that registration's target HWND is destroyed.
        unsafe {
            let _ = RegisterRawInputDevices(&[removal], RAW_INPUT_DEVICE_SIZE);
        }
    }
}

fn raw_mouse_device(flags: RAWINPUTDEVICE_FLAGS, target: HWND) -> RAWINPUTDEVICE {
    RAWINPUTDEVICE {
        usUsagePage: GENERIC_DESKTOP_USAGE_PAGE,
        usUsage: MOUSE_USAGE,
        dwFlags: flags,
        hwndTarget: target,
    }
}

#[cfg(test)]
pub(super) fn registered_raw_mouse() -> Result<Option<RAWINPUTDEVICE>> {
    let mut capacity = 4_u32;

    loop {
        let mut devices = vec![RAWINPUTDEVICE::default(); capacity as usize];
        let mut count = capacity;

        // SAFETY: `devices` provides `capacity` initialized, correctly aligned structures and
        // remains allocated while Windows writes to it. `count` and the exact structure size are
        // valid for the duration of the call.
        let written = unsafe {
            GetRegisteredRawInputDevices(
                Some(devices.as_mut_ptr()),
                &mut count,
                RAW_INPUT_DEVICE_SIZE,
            )
        };

        if written != u32::MAX {
            devices.truncate(written as usize);
            return Ok(devices.into_iter().find(|device| {
                device.usUsagePage == GENERIC_DESKTOP_USAGE_PAGE && device.usUsage == MOUSE_USAGE
            }));
        }

        let error = Error::from_thread();
        if count <= capacity || count > 1_024 {
            return Err(error);
        }
        capacity = count;
    }
}

#[cfg(test)]
mod tests {
    use super::{RawMouseActivity, physical_cursor_position, system_hover_rectangle};
    use crate::hover::PhysicalScreenPoint;
    use windows::Win32::UI::{
        HiDpi::{AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext},
        Input::{MOUSE_MOVE_ABSOLUTE, MOUSE_MOVE_RELATIVE, RAWMOUSE, RAWMOUSE_0, RAWMOUSE_0_0},
        WindowsAndMessaging::{RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_WHEEL},
    };

    #[test]
    fn empty_relative_packet_has_no_activity() {
        assert_eq!(
            RawMouseActivity::from_mouse(raw_mouse(MOUSE_MOVE_RELATIVE, 0, 0, 0)),
            RawMouseActivity::default()
        );
    }

    #[test]
    fn relative_and_absolute_packets_report_movement() {
        assert_eq!(
            RawMouseActivity::from_mouse(raw_mouse(MOUSE_MOVE_RELATIVE, -4, 7, 0)),
            RawMouseActivity {
                moved: true,
                button_or_wheel: false,
            }
        );
        assert_eq!(
            RawMouseActivity::from_mouse(raw_mouse(MOUSE_MOVE_ABSOLUTE, 0, 0, 0)),
            RawMouseActivity {
                moved: true,
                button_or_wheel: false,
            },
            "zero is a valid absolute desktop coordinate"
        );
    }

    #[test]
    fn button_and_wheel_packets_report_interruption() {
        for button_flags in [RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_WHEEL] {
            assert_eq!(
                RawMouseActivity::from_mouse(raw_mouse(
                    MOUSE_MOVE_RELATIVE,
                    0,
                    0,
                    button_flags as u16,
                )),
                RawMouseActivity {
                    moved: false,
                    button_or_wheel: true,
                }
            );
        }
    }

    #[test]
    fn combined_packet_preserves_both_classifications() {
        assert_eq!(
            RawMouseActivity::from_mouse(raw_mouse(
                MOUSE_MOVE_RELATIVE,
                1,
                0,
                RI_MOUSE_LEFT_BUTTON_DOWN as u16,
            )),
            RawMouseActivity {
                moved: true,
                button_or_wheel: true,
            }
        );
    }

    #[test]
    fn system_hover_query_restores_the_calling_thread_context() {
        // SAFETY: Reading the current thread context has no pointer or ownership requirements.
        let before = unsafe { GetThreadDpiAwarenessContext() };
        let cursor = physical_cursor_position().expect("the physical cursor should be available");
        let anchor = PhysicalScreenPoint::new(cursor.x, cursor.y);

        let rectangle =
            system_hover_rectangle(anchor).expect("the system hover rectangle should be queryable");
        assert!(
            rectangle.contains(anchor, anchor),
            "the normalized rectangle must contain its anchor"
        );

        // SAFETY: Both values are predefined/returned DPI context handles used only for equality.
        let restored = unsafe {
            AreDpiAwarenessContextsEqual(before, GetThreadDpiAwarenessContext()).as_bool()
        };
        assert!(
            restored,
            "the platform query must restore the thread context"
        );
    }

    fn raw_mouse(
        state: windows::Win32::UI::Input::MOUSE_STATE,
        x: i32,
        y: i32,
        button_flags: u16,
    ) -> RAWMOUSE {
        RAWMOUSE {
            usFlags: state,
            Anonymous: RAWMOUSE_0 {
                Anonymous: RAWMOUSE_0_0 {
                    usButtonFlags: button_flags,
                    usButtonData: 0,
                },
            },
            lLastX: x,
            lLastY: y,
            ..Default::default()
        }
    }
}
