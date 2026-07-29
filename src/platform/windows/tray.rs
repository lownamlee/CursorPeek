use std::mem::size_of;

use super::load_small_application_icon;
use crate::settings::Theme;

use windows::{
    Win32::{
        Foundation::{E_FAIL, HWND, LPARAM, POINT, WPARAM},
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
                NIM_SETFOCUS, NIM_SETVERSION, NIN_SELECT, NOTIFY_ICON_MESSAGE,
                NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONDATAW_0, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DestroyMenu, DestroyWindow,
                GetCursorPos, HMENU, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MENU_ITEM_FLAGS,
                MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, MessageBoxW, PostMessageW,
                SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
                WM_CONTEXTMENU, WM_NULL, WM_RBUTTONUP, WS_EX_TOOLWINDOW, WS_POPUP,
            },
        },
    },
    core::{Error, PCWSTR, Result, w},
};

const ICON_ID: u32 = 1;
const TOGGLE_PAUSE_COMMAND: usize = 1;
const ABOUT_COMMAND: usize = 2;
const EXIT_COMMAND: usize = 3;
const DWELL_FAST_COMMAND: usize = 10;
const DWELL_STANDARD_COMMAND: usize = 11;
const DWELL_RELAXED_COMMAND: usize = 12;
const PREVIEW_COMPACT_COMMAND: usize = 20;
const PREVIEW_STANDARD_COMMAND: usize = 21;
const PREVIEW_LARGE_COMMAND: usize = 22;
const TOGGLE_STARTUP_COMMAND: usize = 30;
const THEME_SYSTEM_COMMAND: usize = 40;
const THEME_LIGHT_COMMAND: usize = 41;
const THEME_DARK_COMMAND: usize = 42;
const TOGGLE_VIDEO_PREVIEWS_COMMAND: usize = 50;
const TOGGLE_VIDEO_AUDIO_COMMAND: usize = 51;
const NIN_KEYSELECT: u32 = NIN_SELECT + 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrayStatus {
    Active,
    Paused,
    WorkerRecovering,
}

impl TrayStatus {
    const fn tooltip(self) -> &'static str {
        match self {
            Self::Active => "CursorPeek",
            Self::Paused => "CursorPeek (paused)",
            Self::WorkerRecovering => "CursorPeek (worker retry on next hover)",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TrayMenuState {
    pub(crate) paused: bool,
    pub(crate) dwell_delay_ms: u64,
    pub(crate) preview_width: u16,
    pub(crate) preview_height: u16,
    pub(crate) theme: Theme,
    pub(crate) start_with_windows: bool,
    pub(crate) video_previews: bool,
    pub(crate) video_audio: bool,
}

pub(crate) struct TrayIcon {
    data: NOTIFYICONDATAW,
    menu_owner: TrayMenuOwner,
    added: bool,
}

impl TrayIcon {
    pub(crate) fn create(callback_window: HWND, callback_message: u32) -> Result<Self> {
        let menu_owner = TrayMenuOwner::create()?;
        let icon = load_small_application_icon()?;
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: callback_window,
            uID: ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: callback_message,
            hIcon: icon,
            ..Default::default()
        };
        write_wide_z(&mut data.szTip, TrayStatus::Active.tooltip());

        let mut tray = Self {
            data,
            menu_owner,
            added: false,
        };
        tray.add_to_notification_area(false)?;
        Ok(tray)
    }

    fn add_to_notification_area(&mut self, accept_existing: bool) -> Result<()> {
        establish_notification_area_registration(
            &mut self.data,
            &mut self.added,
            accept_existing,
            |message, data| {
                // SAFETY: The caller supplies a complete application-owned registration record,
                // and Shell_NotifyIcon reads it synchronously without retaining the pointer.
                unsafe { Shell_NotifyIconW(message, data) }.as_bool()
            },
        )
    }

    pub(crate) fn restore_after_taskbar_created(&mut self) -> Result<()> {
        // TaskbarCreated means Explorer discarded notification registrations. Do not issue a
        // speculative NIM_DELETE against the new taskbar; recreate the complete registration and
        // renegotiate version 4 exactly as at startup.
        self.added = false;
        self.add_to_notification_area(true)
    }

    pub(crate) fn set_status(&mut self, status: TrayStatus) -> Result<()> {
        self.data.uFlags = NIF_TIP | NIF_SHOWTIP;
        write_wide_z(&mut self.data.szTip, status.tooltip());

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
        state: TrayMenuState,
    ) -> Result<Option<TrayCommand>> {
        let Some(anchor) = callback_anchor(wparam, lparam) else {
            return Ok(None);
        };

        let command = self.menu_owner.show_menu(anchor.resolve()?, state);
        self.return_focus_to_notification_area();
        command
    }

    pub(crate) fn command_at_cursor(&self, state: TrayMenuState) -> Result<Option<TrayCommand>> {
        let command = self
            .menu_owner
            .show_menu(CallbackAnchor::Cursor.resolve()?, state);
        self.return_focus_to_notification_area();
        command
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

    pub(crate) fn show_error(&self, message: &str) {
        let text = terminated_utf16(message);
        // SAFETY: The tray owner lives for the synchronous modal call and both strings are
        // terminated UTF-16. A settings failure is reported without ending the application.
        unsafe {
            MessageBoxW(
                Some(self.menu_owner.hwnd),
                PCWSTR(text.as_ptr()),
                w!("CursorPeek settings"),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn return_focus_to_notification_area(&self) {
        // Microsoft recommends NIM_SETFOCUS after notification-area UI completes so keyboard
        // focus returns to the tray whether the user selected a command or cancelled the menu.
        // SAFETY: `self.data` still identifies the live icon owned by this TrayIcon.
        let _ = unsafe { Shell_NotifyIconW(NIM_SETFOCUS, &self.data) };
    }
}

fn establish_notification_area_registration(
    data: &mut NOTIFYICONDATAW,
    added_state: &mut bool,
    accept_existing: bool,
    mut notify: impl FnMut(NOTIFY_ICON_MESSAGE, &NOTIFYICONDATAW) -> bool,
) -> Result<()> {
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    let added = notify(NIM_ADD, data);
    // A TaskbarCreated broadcast can race with a registration performed against the freshly
    // created taskbar. If the exact HWND/uID entry already exists, NIM_MODIFY proves ownership and
    // refreshes every field without treating the benign race as an application failure.
    let refreshed = accept_existing && !added && notify(NIM_MODIFY, data);
    if !added && !refreshed {
        return Err(Error::from_hresult(E_FAIL));
    }
    *added_state = true;

    data.Anonymous = NOTIFYICONDATAW_0 {
        uVersion: NOTIFYICON_VERSION_4,
    };
    if !notify(NIM_SETVERSION, data) {
        let _ = notify(NIM_DELETE, data);
        *added_state = false;
        return Err(Error::from_hresult(E_FAIL));
    }

    Ok(())
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
    SetDwellDelay(u64),
    SetPreviewSize(u16, u16),
    SetTheme(Theme),
    ToggleStartWithWindows,
    ToggleVideoPreviews,
    ToggleVideoAudio,
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

    fn show_menu(&self, anchor: POINT, state: TrayMenuState) -> Result<Option<TrayCommand>> {
        let menu = PopupMenu::create(state)?;

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
    fn new() -> Result<Self> {
        // SAFETY: CreatePopupMenu has no preconditions and transfers ownership on success.
        let handle = unsafe { CreatePopupMenu()? };
        Ok(Self { handle })
    }

    fn create(state: TrayMenuState) -> Result<Self> {
        let menu = Self::new()?;

        menu.append_command(
            TOGGLE_PAUSE_COMMAND,
            if state.paused {
                w!("Resume")
            } else {
                w!("Pause")
            },
            false,
        )?;
        menu.append_separator()?;

        let settings = Self::new()?;
        let dwell = Self::new()?;
        dwell.append_command(
            DWELL_FAST_COMMAND,
            w!("Fast (50 ms)"),
            state.dwell_delay_ms == 50,
        )?;
        dwell.append_command(
            DWELL_STANDARD_COMMAND,
            w!("Standard (100 ms)"),
            state.dwell_delay_ms == 100,
        )?;
        dwell.append_command(
            DWELL_RELAXED_COMMAND,
            w!("Relaxed (250 ms)"),
            state.dwell_delay_ms == 250,
        )?;
        settings.append_submenu(dwell, w!("Dwell delay"))?;

        let preview = Self::new()?;
        preview.append_command(
            PREVIEW_COMPACT_COMMAND,
            w!("Compact (480 x 360)"),
            (state.preview_width, state.preview_height) == (480, 360),
        )?;
        preview.append_command(
            PREVIEW_STANDARD_COMMAND,
            w!("Standard (640 x 480)"),
            (state.preview_width, state.preview_height) == (640, 480),
        )?;
        preview.append_command(
            PREVIEW_LARGE_COMMAND,
            w!("Large (800 x 600)"),
            (state.preview_width, state.preview_height) == (800, 600),
        )?;
        settings.append_submenu(preview, w!("Maximum preview size"))?;

        let theme = Self::new()?;
        theme.append_command(
            THEME_SYSTEM_COMMAND,
            w!("Use system colors"),
            state.theme == Theme::System,
        )?;
        theme.append_command(
            THEME_LIGHT_COMMAND,
            w!("Light"),
            state.theme == Theme::Light,
        )?;
        theme.append_command(THEME_DARK_COMMAND, w!("Dark"), state.theme == Theme::Dark)?;
        settings.append_submenu(theme, w!("Theme"))?;
        let video = Self::new()?;
        video.append_command(
            TOGGLE_VIDEO_PREVIEWS_COMMAND,
            w!("Play video previews"),
            state.video_previews,
        )?;
        video.append_command(
            TOGGLE_VIDEO_AUDIO_COMMAND,
            w!("Play sound"),
            state.video_audio,
        )?;
        settings.append_submenu(video, w!("Video"))?;
        settings.append_separator()?;
        settings.append_command(
            TOGGLE_STARTUP_COMMAND,
            w!("Start with Windows"),
            state.start_with_windows,
        )?;
        menu.append_submenu(settings, w!("Settings"))?;

        menu.append_separator()?;
        menu.append_command(ABOUT_COMMAND, w!("About CursorPeek"), false)?;
        menu.append_command(EXIT_COMMAND, w!("Exit"), false)?;

        Ok(menu)
    }

    fn append_command(&self, id: usize, label: PCWSTR, checked: bool) -> Result<()> {
        let flags = MF_STRING
            | if checked {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS::default()
            };
        // SAFETY: The menu is live, the label is terminated UTF-16, and command identifiers are
        // unique within the complete root menu.
        unsafe { AppendMenuW(self.handle, flags, id, label) }
    }

    fn append_separator(&self) -> Result<()> {
        // SAFETY: The menu is live and separators do not consume the ignored string pointer.
        unsafe { AppendMenuW(self.handle, MF_SEPARATOR, 0, PCWSTR::null()) }
    }

    fn append_submenu(&self, submenu: Self, label: PCWSTR) -> Result<()> {
        // SAFETY: Both menus and the terminated label are live. A successful append transfers
        // recursive destruction of the child to the parent menu.
        unsafe {
            AppendMenuW(self.handle, MF_POPUP, submenu.handle.0 as usize, label)?;
        }
        std::mem::forget(submenu);
        Ok(())
    }
}

impl Drop for PopupMenu {
    fn drop(&mut self) {
        // SAFETY: This menu is never assigned to a window, so this owner must destroy it once.
        let _ = unsafe { DestroyMenu(self.handle) };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallbackAnchor {
    Cursor,
    Point(i32, i32),
}

impl CallbackAnchor {
    fn resolve(self) -> Result<POINT> {
        match self {
            Self::Cursor => {
                let mut point = POINT::default();
                // SAFETY: The output points to a valid initialized POINT and the process is
                // Per-Monitor V2 aware, so the physical screen coordinate is suitable for the
                // native popup-menu API.
                unsafe { GetCursorPos(&mut point)? };
                Ok(point)
            }
            Self::Point(x, y) => Ok(POINT { x, y }),
        }
    }
}

fn callback_anchor(wparam: WPARAM, lparam: LPARAM) -> Option<CallbackAnchor> {
    let packed_event = lparam.0 as u32;
    let event = packed_event & 0xffff;
    let icon_id = packed_event >> 16;
    if icon_id != ICON_ID
        || !matches!(
            event,
            WM_RBUTTONUP | WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT
        )
    {
        return None;
    }

    // NOTIFYICON_VERSION_4 defines wParam coordinates for NIN_SELECT and NIN_KEYSELECT, but not
    // for the keyboard-generated WM_CONTEXTMENU notification. Mouse right-click arrives as
    // WM_RBUTTONUP and follows the documented packed-point contract.
    if event == WM_CONTEXTMENU {
        return Some(CallbackAnchor::Cursor);
    }

    let packed_point = wparam.0 as u32;
    Some(CallbackAnchor::Point(
        i32::from(packed_point as u16 as i16),
        i32::from((packed_point >> 16) as u16 as i16),
    ))
}

fn command_from_id(id: usize) -> Option<TrayCommand> {
    match id {
        TOGGLE_PAUSE_COMMAND => Some(TrayCommand::TogglePaused),
        DWELL_FAST_COMMAND => Some(TrayCommand::SetDwellDelay(50)),
        DWELL_STANDARD_COMMAND => Some(TrayCommand::SetDwellDelay(100)),
        DWELL_RELAXED_COMMAND => Some(TrayCommand::SetDwellDelay(250)),
        PREVIEW_COMPACT_COMMAND => Some(TrayCommand::SetPreviewSize(480, 360)),
        PREVIEW_STANDARD_COMMAND => Some(TrayCommand::SetPreviewSize(640, 480)),
        PREVIEW_LARGE_COMMAND => Some(TrayCommand::SetPreviewSize(800, 600)),
        THEME_SYSTEM_COMMAND => Some(TrayCommand::SetTheme(Theme::System)),
        THEME_LIGHT_COMMAND => Some(TrayCommand::SetTheme(Theme::Light)),
        THEME_DARK_COMMAND => Some(TrayCommand::SetTheme(Theme::Dark)),
        TOGGLE_STARTUP_COMMAND => Some(TrayCommand::ToggleStartWithWindows),
        TOGGLE_VIDEO_PREVIEWS_COMMAND => Some(TrayCommand::ToggleVideoPreviews),
        TOGGLE_VIDEO_AUDIO_COMMAND => Some(TrayCommand::ToggleVideoAudio),
        ABOUT_COMMAND => Some(TrayCommand::About),
        EXIT_COMMAND => Some(TrayCommand::Exit),
        _ => None,
    }
}

fn terminated_utf16(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
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
        ABOUT_COMMAND, CallbackAnchor, DWELL_FAST_COMMAND, DWELL_RELAXED_COMMAND,
        DWELL_STANDARD_COMMAND, EXIT_COMMAND, ICON_ID, NIN_KEYSELECT, NIN_SELECT,
        PREVIEW_COMPACT_COMMAND, PREVIEW_LARGE_COMMAND, PREVIEW_STANDARD_COMMAND, PopupMenu,
        THEME_DARK_COMMAND, THEME_LIGHT_COMMAND, THEME_SYSTEM_COMMAND, TOGGLE_PAUSE_COMMAND,
        TOGGLE_STARTUP_COMMAND, TOGGLE_VIDEO_AUDIO_COMMAND, TOGGLE_VIDEO_PREVIEWS_COMMAND,
        TrayCommand, TrayMenuState, TrayStatus, callback_anchor, command_from_id,
        establish_notification_area_registration, write_wide_z,
    };
    use crate::settings::Theme;
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
                NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::{
                GetMenuState, GetSubMenu, HMENU, MF_BYCOMMAND, MF_CHECKED, WM_CONTEXTMENU,
                WM_RBUTTONUP,
            },
        },
    };

    #[test]
    fn version_four_callback_decoding_checks_icon_event_and_anchor_contract() {
        let point = WPARAM(u32::from(0xfff0_u16) as usize | ((25_u32 as usize) << 16));

        for event in [WM_RBUTTONUP, NIN_SELECT, NIN_KEYSELECT] {
            let callback = LPARAM(((ICON_ID << 16) | event) as isize);
            assert_eq!(
                callback_anchor(point, callback),
                Some(CallbackAnchor::Point(-16, 25))
            );
        }

        let context = LPARAM(((ICON_ID << 16) | WM_CONTEXTMENU) as isize);
        assert_eq!(
            callback_anchor(WPARAM(usize::MAX), context),
            Some(CallbackAnchor::Cursor),
            "WM_CONTEXTMENU must not consume its undefined wParam as a coordinate"
        );

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
        assert_eq!(
            command_from_id(DWELL_FAST_COMMAND),
            Some(TrayCommand::SetDwellDelay(50))
        );
        assert_eq!(
            command_from_id(DWELL_STANDARD_COMMAND),
            Some(TrayCommand::SetDwellDelay(100))
        );
        assert_eq!(
            command_from_id(DWELL_RELAXED_COMMAND),
            Some(TrayCommand::SetDwellDelay(250))
        );
        assert_eq!(
            command_from_id(PREVIEW_COMPACT_COMMAND),
            Some(TrayCommand::SetPreviewSize(480, 360))
        );
        assert_eq!(
            command_from_id(PREVIEW_STANDARD_COMMAND),
            Some(TrayCommand::SetPreviewSize(640, 480))
        );
        assert_eq!(
            command_from_id(PREVIEW_LARGE_COMMAND),
            Some(TrayCommand::SetPreviewSize(800, 600))
        );
        assert_eq!(
            command_from_id(THEME_SYSTEM_COMMAND),
            Some(TrayCommand::SetTheme(Theme::System))
        );
        assert_eq!(
            command_from_id(THEME_LIGHT_COMMAND),
            Some(TrayCommand::SetTheme(Theme::Light))
        );
        assert_eq!(
            command_from_id(THEME_DARK_COMMAND),
            Some(TrayCommand::SetTheme(Theme::Dark))
        );
        assert_eq!(
            command_from_id(TOGGLE_STARTUP_COMMAND),
            Some(TrayCommand::ToggleStartWithWindows)
        );
        assert_eq!(
            command_from_id(TOGGLE_VIDEO_PREVIEWS_COMMAND),
            Some(TrayCommand::ToggleVideoPreviews)
        );
        assert_eq!(
            command_from_id(TOGGLE_VIDEO_AUDIO_COMMAND),
            Some(TrayCommand::ToggleVideoAudio)
        );
        assert_eq!(command_from_id(0), None);
        assert_eq!(command_from_id(usize::MAX), None);
    }

    #[test]
    fn settings_submenus_reflect_the_saved_product_state() {
        let menu = PopupMenu::create(TrayMenuState {
            paused: false,
            dwell_delay_ms: 250,
            preview_width: 480,
            preview_height: 360,
            theme: Theme::Dark,
            start_with_windows: true,
            video_previews: true,
            video_audio: false,
        })
        .expect("the native popup hierarchy should be created");

        // SAFETY: `menu` owns the complete live hierarchy for these synchronous read-only queries.
        let settings = unsafe { GetSubMenu(menu.handle, 2) };
        assert!(!settings.0.is_null());
        // SAFETY: `settings` is the live submenu returned above and its first two items are menus.
        let dwell = unsafe { GetSubMenu(settings, 0) };
        // SAFETY: `settings` remains live and its second item is the preview-size submenu.
        let preview = unsafe { GetSubMenu(settings, 1) };
        assert!(!dwell.0.is_null());
        assert!(!preview.0.is_null());

        assert!(!is_checked(dwell, DWELL_FAST_COMMAND));
        assert!(!is_checked(dwell, DWELL_STANDARD_COMMAND));
        assert!(is_checked(dwell, DWELL_RELAXED_COMMAND));
        assert!(is_checked(preview, PREVIEW_COMPACT_COMMAND));
        assert!(!is_checked(preview, PREVIEW_STANDARD_COMMAND));
        assert!(!is_checked(preview, PREVIEW_LARGE_COMMAND));

        // SAFETY: Theme is the third live submenu in the settings hierarchy.
        let theme = unsafe { GetSubMenu(settings, 2) };
        assert!(!theme.0.is_null());
        assert!(!is_checked(theme, THEME_SYSTEM_COMMAND));
        assert!(!is_checked(theme, THEME_LIGHT_COMMAND));
        assert!(is_checked(theme, THEME_DARK_COMMAND));
        // SAFETY: Video is the fourth live submenu in the settings hierarchy.
        let video = unsafe { GetSubMenu(settings, 3) };
        assert!(!video.0.is_null());
        assert!(is_checked(video, TOGGLE_VIDEO_PREVIEWS_COMMAND));
        assert!(!is_checked(video, TOGGLE_VIDEO_AUDIO_COMMAND));
        assert!(is_checked(settings, TOGGLE_STARTUP_COMMAND));
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

    #[test]
    fn tray_status_tooltips_are_bounded_and_distinguish_recovery() {
        assert_eq!(TrayStatus::Active.tooltip(), "CursorPeek");
        assert_eq!(TrayStatus::Paused.tooltip(), "CursorPeek (paused)");
        assert_eq!(
            TrayStatus::WorkerRecovering.tooltip(),
            "CursorPeek (worker retry on next hover)"
        );
        assert!(
            TrayStatus::WorkerRecovering
                .tooltip()
                .encode_utf16()
                .count()
                < windows::Win32::UI::Shell::NOTIFYICONDATAW::default()
                    .szTip
                    .len()
        );
    }

    #[test]
    fn taskbar_registration_recovers_duplicates_and_cleans_version_failures() {
        let mut recovered = NOTIFYICONDATAW::default();
        let mut recovered_state = false;
        let mut recovery_calls = Vec::new();
        establish_notification_area_registration(
            &mut recovered,
            &mut recovered_state,
            true,
            |message, _| {
                recovery_calls.push(message.0);
                message == NIM_MODIFY || message == NIM_SETVERSION
            },
        )
        .expect("an existing exact tray entry should be refreshed");
        assert_eq!(
            recovery_calls,
            vec![NIM_ADD.0, NIM_MODIFY.0, NIM_SETVERSION.0]
        );
        assert!(recovered_state);
        assert_eq!(
            recovered.uFlags,
            NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP
        );
        // SAFETY: The successful helper initialized the uVersion union member before returning.
        let recovered_version = unsafe { recovered.Anonymous.uVersion };
        assert_eq!(recovered_version, NOTIFYICON_VERSION_4);

        let mut rejected = NOTIFYICONDATAW::default();
        let mut rejected_state = false;
        let mut rejection_calls = Vec::new();
        assert!(
            establish_notification_area_registration(
                &mut rejected,
                &mut rejected_state,
                false,
                |message, _| {
                    rejection_calls.push(message.0);
                    message == NIM_ADD || message == NIM_DELETE
                },
            )
            .is_err(),
            "a version failure must reject and remove the partial registration"
        );
        assert_eq!(
            rejection_calls,
            vec![NIM_ADD.0, NIM_SETVERSION.0, NIM_DELETE.0]
        );
        assert!(!rejected_state);
    }

    fn is_checked(menu: HMENU, command: usize) -> bool {
        // SAFETY: Callers pass a live menu and one product command identifier.
        (unsafe { GetMenuState(menu, command as u32, MF_BYCOMMAND) } & MF_CHECKED.0) != 0
    }
}
