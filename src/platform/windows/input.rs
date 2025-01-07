use std::{marker::PhantomData, mem::size_of, rc::Rc};

use windows::{
    core::Result,
    Win32::{
        Foundation::HWND,
        UI::Input::{
            RegisterRawInputDevices, RAWINPUTDEVICE, RAWINPUTDEVICE_FLAGS, RIDEV_INPUTSINK,
            RIDEV_REMOVE,
        },
    },
};

#[cfg(test)]
use windows::{core::Error, Win32::UI::Input::GetRegisteredRawInputDevices};

const GENERIC_DESKTOP_USAGE_PAGE: u16 = 0x01;
const MOUSE_USAGE: u16 = 0x02;
const RAW_INPUT_DEVICE_SIZE: u32 = size_of::<RAWINPUTDEVICE>() as u32;

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

        let error = Error::from_win32();
        if count <= capacity || count > 1_024 {
            return Err(error);
        }
        capacity = count;
    }
}
