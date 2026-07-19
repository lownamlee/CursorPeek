use std::mem::size_of;

use windows::{
    Win32::{
        Foundation::{E_FAIL, HWND, LPARAM, POINT, WPARAM},
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
                NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
                NOTIFYICONDATAW_0, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DestroyMenu, DestroyWindow, HMENU,
                IDI_APPLICATION, LoadIconW, MB_ICONINFORMATION, MB_OK, MF_SEPARATOR, MF_STRING,
                MessageBoxW, PostMessageW, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD,
                TPM_RIGHTBUTTON, TrackPopupMenu, WM_CONTEXTMENU, WM_NULL, WS_EX_TOOLWINDOW,
                WS_POPUP,
            },
        },
    },
    core::{Error, PCWSTR, Result, w},
};

const ICON_ID: u32 = 1;
const TOGGLE_PAUSE_COMMAND: usize = 1;
const ABOUT_COMMAND: usize = 2;
const EXIT_COMMAND: usize = 3;
const NIN_KEYSELECT: u32 = NIN_SELECT + 1;

pub(crate) struct TrayIcon {
    data: NOTIFYICONDATAW,
    menu_owner: TrayMenuOwner,
    added: bool,
}

impl TrayIcon {
    pub(crate) fn create(callback_window: HWND, callback_message: u32) -> Result<Self> {
        let menu_owner = TrayMenuOwner::create()?;
        // SAFETY: IDI_APPLICATION selects a system-owned shared icon. The returned handle remains
        // valid without DestroyIcon and is used only while the notification icon is registered.
        let icon = unsafe { LoadIconW(None, IDI_APPLICATION)? };
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: callback_window,
            uID: ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: callback_message,
            hIcon: icon,
            ..Default::default()
        };
        write_wide_z(&mut data.szTip, "CursorPeek");

        let mut tray = Self {
            data,
            menu_owner,
            added: false,
        };

        // SAFETY: `data` has the current structure size, a live application-owned callback HWND,
        // a shared system icon, a terminated bounded tooltip, and a private callback message.
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &tray.data) }.as_bool() {
            return Err(Error::from_hresult(E_FAIL));
        }
        tray.added = true;

        tray.data.Anonymous = NOTIFYICONDATAW_0 {
            uVersion: NOTIFYICON_VERSION_4,
        };
        // SAFETY: NIM_SETVERSION identifies the icon by the same live HWND/uID pair and reads the
        // initialized uVersion union member.
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &tray.data) }.as_bool() {
            return Err(Error::from_hresult(E_FAIL));
        }

        Ok(tray)
    }

    pub(crate) fn set_paused(&mut self, paused: bool) -> Result<()> {
        self.data.uFlags = NIF_TIP | NIF_SHOWTIP;
        write_wide_z(
            &mut self.data.szTip,
            if paused {
                "CursorPeek (paused)"
            } else {
                "CursorPeek"
            },
        );

        // SAFETY: The icon remains registered by this owner and the bounded tooltip is terminated.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data) }.as_bool() {
            Ok(())
        } else {
            Err(Error::from_hresult(E_FAIL))
        }
    }

    pub(crate) fn command_for_callback(
        &self,
        wparam: WPARAM,
        lparam: LPARAM,
        paused: bool,
    ) -> Result<Option<TrayCommand>> {
        let Some(anchor) = callback_anchor(wparam, lparam) else {
            return Ok(None);
        };

        self.menu_owner.show_menu(anchor, paused)
    }

    pub(crate) fn show_about(&self) {
        let text: Vec<u16> = concat!(
            "CursorPeek ",
            env!("CARGO_PKG_VERSION"),
            "\r\nLightweight previews for Windows File Explorer."
        )
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
        // SAFETY: The hidden top-level owner lives for this call and both strings are static,
        // or locally owned terminated UTF-16. MessageBoxW consumes the text synchronously, and the
        // user explicitly requested the modal dialog from the tray menu.
        unsafe {
            MessageBoxW(
                Some(self.menu_owner.hwnd),
                PCWSTR(text.as_ptr()),
                w!("About CursorPeek"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        if self.added {
            // SAFETY: This removes the same HWND/uID registration before the callback and menu
            // owner windows are destroyed. Shell_NotifyIcon owns no borrowed Rust data.
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data) };
            self.added = false;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrayCommand {
    TogglePaused,
    About,
    Exit,
}

struct TrayMenuOwner {
    hwnd: HWND,
}

impl TrayMenuOwner {
    fn create() -> Result<Self> {
        // SAFETY: STATIC is a system window class. The zero-sized WS_POPUP is never shown and
        // exists only as the top-level foreground/menu owner required by TrackPopupMenu.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("CursorPeek.TrayMenuOwner"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )?
        };
        Ok(Self { hwnd })
    }

    fn show_menu(&self, anchor: POINT, paused: bool) -> Result<Option<TrayCommand>> {
        let menu = PopupMenu::create(paused)?;

        // The notification-area contract grants foreground activation in response to the user's
        // icon action. Fail closed instead of showing a menu that cannot dismiss on outside click.
        // SAFETY: `self.hwnd` is a live top-level window owned by this UI thread.
        if !unsafe { SetForegroundWindow(self.hwnd) }.as_bool() {
            return Ok(None);
        }

        // SAFETY: The menu and owner remain live for the synchronous call. TPM_RETURNCMD returns
        // the selected command directly and TPM_NONOTIFY prevents unrelated WM_COMMAND delivery.
        let selected = unsafe {
            TrackPopupMenu(
                menu.handle,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                anchor.x,
                anchor.y,
                None,
                self.hwnd,
                None,
            )
        };

        // Microsoft documents this benign post after a notification-area menu so a subsequent
        // invocation does not disappear immediately.
        // SAFETY: The private owner window is still live and WM_NULL carries no data.
        let _ = unsafe { PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0)) };

        Ok(command_from_id(selected.0 as usize))
    }
}

impl Drop for TrayMenuOwner {
    fn drop(&mut self) {
        // SAFETY: The owner is held by the thread-affine MessageWindow and is destroyed once on
        // that same UI thread after any synchronous menu or About dialog has returned.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

struct PopupMenu {
    handle: HMENU,
}

impl PopupMenu {
    fn create(paused: bool) -> Result<Self> {
        // SAFETY: CreatePopupMenu has no preconditions and transfers ownership on success.
        let menu = Self {
            handle: unsafe { CreatePopupMenu()? },
        };

        // SAFETY: `menu.handle` remains live; every label is static terminated UTF-16 and command
        // identifiers are unique within this menu.
        unsafe {
            AppendMenuW(
                menu.handle,
                MF_STRING,
                TOGGLE_PAUSE_COMMAND,
                if paused { w!("Resume") } else { w!("Pause") },
            )?;
            AppendMenuW(menu.handle, MF_SEPARATOR, 0, PCWSTR::null())?;
            AppendMenuW(
                menu.handle,
                MF_STRING,
                ABOUT_COMMAND,
                w!("About CursorPeek"),
            )?;
            AppendMenuW(menu.handle, MF_STRING, EXIT_COMMAND, w!("Exit"))?;
        }

        Ok(menu)
    }
}

impl Drop for PopupMenu {
    fn drop(&mut self) {
        // SAFETY: This menu is never assigned to a window, so this owner must destroy it once.
        let _ = unsafe { DestroyMenu(self.handle) };
    }
}

fn callback_anchor(wparam: WPARAM, lparam: LPARAM) -> Option<POINT> {
    let packed_event = lparam.0 as u32;
    let event = packed_event & 0xffff;
    let icon_id = packed_event >> 16;
    if icon_id != ICON_ID || !matches!(event, WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT) {
        return None;
    }

    let packed_point = wparam.0 as u32;
    Some(POINT {
        x: i32::from(packed_point as u16 as i16),
        y: i32::from((packed_point >> 16) as u16 as i16),
    })
}

fn command_from_id(id: usize) -> Option<TrayCommand> {
    match id {
        TOGGLE_PAUSE_COMMAND => Some(TrayCommand::TogglePaused),
        ABOUT_COMMAND => Some(TrayCommand::About),
        EXIT_COMMAND => Some(TrayCommand::Exit),
        _ => None,
    }
}

fn write_wide_z<const N: usize>(target: &mut [u16; N], value: &str) {
    target.fill(0);
    if N == 0 {
        return;
    }

    let mut written = 0;
    for character in value.chars() {
        let mut encoded = [0; 2];
        let units = character.encode_utf16(&mut encoded);
        if written + units.len() >= N {
            break;
        }
        target[written..written + units.len()].copy_from_slice(units);
        written += units.len();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ABOUT_COMMAND, EXIT_COMMAND, ICON_ID, NIN_KEYSELECT, NIN_SELECT, TOGGLE_PAUSE_COMMAND,
        TrayCommand, callback_anchor, command_from_id, write_wide_z,
    };
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::WM_CONTEXTMENU,
    };

    #[test]
    fn version_four_callback_decoding_checks_icon_event_and_signed_coordinates() {
        let point = WPARAM(u32::from(0xfff0_u16) as usize | ((25_u32 as usize) << 16));

        for event in [WM_CONTEXTMENU, NIN_SELECT, NIN_KEYSELECT] {
            let callback = LPARAM(((ICON_ID << 16) | event) as isize);
            let anchor = callback_anchor(point, callback).expect("the callback should open a menu");
            assert_eq!((anchor.x, anchor.y), (-16, 25));
        }

        assert!(callback_anchor(point, LPARAM(WM_CONTEXTMENU as isize)).is_none());
        assert!(
            callback_anchor(
                point,
                LPARAM((((ICON_ID + 1) << 16) | WM_CONTEXTMENU) as isize)
            )
            .is_none()
        );
    }

    #[test]
    fn tray_commands_are_explicit_and_cancellation_is_not_a_command() {
        assert_eq!(
            command_from_id(TOGGLE_PAUSE_COMMAND),
            Some(TrayCommand::TogglePaused)
        );
        assert_eq!(command_from_id(ABOUT_COMMAND), Some(TrayCommand::About));
        assert_eq!(command_from_id(EXIT_COMMAND), Some(TrayCommand::Exit));
        assert_eq!(command_from_id(0), None);
        assert_eq!(command_from_id(usize::MAX), None);
    }

    #[test]
    fn bounded_utf16_text_is_terminated_without_splitting_a_scalar() {
        let mut exact = [0xffff; 6];
        write_wide_z(&mut exact, "Peek");
        assert_eq!(
            exact,
            ['P' as u16, 'e' as u16, 'e' as u16, 'k' as u16, 0, 0]
        );

        let mut bounded = [0xffff; 4];
        write_wide_z(&mut bounded, "A\u{1f600}B");
        assert_eq!(bounded, ['A' as u16, 0xd83d, 0xde00, 0]);
    }
}
