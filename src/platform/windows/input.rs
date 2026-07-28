use std::{
    marker::PhantomData,
    mem::{MaybeUninit, size_of},
    rc::Rc,
};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, POINT},
        UI::{
            Input::{
                GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE,
                RAWINPUTDEVICE_FLAGS, RAWINPUTHEADER, RAWMOUSE, RID_INPUT, RIDEV_INPUTSINK,
                RIDEV_REMOVE, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE, RegisterRawInputDevices,
            },
            WindowsAndMessaging::GetPhysicalCursorPos,
        },
    },
    core::{Error, Result},
};

#[cfg(test)]
use windows::Win32::UI::Input::GetRegisteredRawInputDevices;

const GENERIC_DESKTOP_USAGE_PAGE: u16 = 0x01;
const MOUSE_USAGE: u16 = 0x02;
const KEYBOARD_USAGE: u16 = 0x06;
const RAW_INPUT_DEVICE_SIZE: u32 = size_of::<RAWINPUTDEVICE>() as u32;
const RAW_INPUT_SIZE: u32 = size_of::<RAWINPUT>() as u32;
const RAW_INPUT_HEADER_SIZE: u32 = size_of::<RAWINPUTHEADER>() as u32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RawInputActivity {
    Mouse(RawMouseActivity),
    Keyboard,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct RawMouseActivity {
    moved: bool,
    button_or_wheel: bool,
}

impl RawMouseActivity {
    #[cfg(test)]
    pub(super) const fn for_test(moved: bool, button_or_wheel: bool) -> Self {
        Self {
            moved,
            button_or_wheel,
        }
    }

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

pub(super) fn read_raw_input_activity(lparam: LPARAM) -> Result<Option<RawInputActivity>> {
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
    if raw_input.header.dwSize != RAW_INPUT_SIZE {
        return Ok(None);
    }

    if raw_input.header.dwType == RIM_TYPEMOUSE.0 {
        // SAFETY: The validated RAWINPUT header identifies the active union member as RAWMOUSE.
        let mouse = unsafe { raw_input.data.mouse };
        Ok(Some(RawInputActivity::Mouse(RawMouseActivity::from_mouse(
            mouse,
        ))))
    } else if raw_input.header.dwType == RIM_TYPEKEYBOARD.0 {
        Ok(Some(RawInputActivity::Keyboard))
    } else {
        Ok(None)
    }
}

pub(super) struct RawInputRegistration {
    _thread_affinity: PhantomData<Rc<()>>,
}

impl RawInputRegistration {
    pub(super) fn register(target: HWND) -> Result<Self> {
        let devices = raw_input_devices(RIDEV_INPUTSINK, target);

        // SAFETY: The mouse/keyboard slice is correctly aligned and uses the exact generated
        // structure size. `target` is a live window owned by the calling UI thread, and
        // RIDEV_INPUTSINK is valid only because that non-null target is supplied. Legacy input
        // remains enabled; this registration observes rather than captures either device class.
        unsafe {
            RegisterRawInputDevices(&devices, RAW_INPUT_DEVICE_SIZE)?;
        }

        Ok(Self {
            _thread_affinity: PhantomData,
        })
    }
}

impl Drop for RawInputRegistration {
    fn drop(&mut self) {
        let removals = raw_input_devices(RIDEV_REMOVE, HWND::default());

        // SAFETY: The mouse/keyboard slice is aligned and sized as required. Microsoft requires a
        // null target when RIDEV_REMOVE is used. This !Send token is explicitly dropped by
        // MessageWindow on the owning UI thread before that registration's target HWND is
        // destroyed.
        unsafe {
            let _ = RegisterRawInputDevices(&removals, RAW_INPUT_DEVICE_SIZE);
        }
    }
}

fn raw_input_devices(flags: RAWINPUTDEVICE_FLAGS, target: HWND) -> [RAWINPUTDEVICE; 2] {
    [MOUSE_USAGE, KEYBOARD_USAGE].map(|usage| RAWINPUTDEVICE {
        usUsagePage: GENERIC_DESKTOP_USAGE_PAGE,
        usUsage: usage,
        dwFlags: flags,
        hwndTarget: target,
    })
}

#[cfg(test)]
pub(super) fn registered_raw_devices() -> Result<Vec<RAWINPUTDEVICE>> {
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
            devices.retain(|device| {
                device.usUsagePage == GENERIC_DESKTOP_USAGE_PAGE
                    && matches!(device.usUsage, MOUSE_USAGE | KEYBOARD_USAGE)
            });
            devices.sort_by_key(|device| device.usUsage);
            return Ok(devices);
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
    use super::RawMouseActivity;
    use windows::Win32::UI::{
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
