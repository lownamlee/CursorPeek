use std::{
    cell::RefCell,
    ffi::c_void,
    marker::PhantomData,
    mem::size_of,
    panic::AssertUnwindSafe,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    hover::PhysicalScreenPoint,
    preview::{PreviewPlacement, PreviewSize, ScreenRect, place_preview, place_preview_pixels},
    settings::Theme,
    worker::{ImagePreview, TextPreview},
};
use cursorpeek_core::{ExplorerWindowId, layout::fit_dimensions, payload::VideoPreview};

use super::explorer::is_explorer_infotip_window;

use windows::{
    Foundation::TypedEventHandler,
    UI::ViewManagement::{UIColorType, UISettings},
    Win32::{
        Foundation::{
            D2DERR_RECREATE_TARGET, E_INVALIDARG, FILETIME, HINSTANCE, HWND, LPARAM, LRESULT, RECT,
            SYSTEMTIME, WPARAM,
        },
        Globalization::{DATE_SHORTDATE, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
                    D2D1_PIXEL_FORMAT,
                },
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
                D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
                D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE,
                D2D1_RENDER_TARGET_PROPERTIES, D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory,
                ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
                DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
                DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory,
                IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{
                BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, EndPaint,
                GetMonitorInfoW, GetSysColor, MONITOR_DEFAULTTONEAREST, MONITORINFO,
                MonitorFromPoint, PAINTSTRUCT, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW,
                RedrawWindow,
            },
        },
        Media::MediaFoundation::{
            IMFPMediaPlayer, IMFPMediaPlayerCallback, IMFPMediaPlayerCallback_Impl,
            MF_MT_FRAME_SIZE, MF_VERSION, MFP_EVENT_HEADER, MFP_EVENT_TYPE_PLAYBACK_ENDED,
            MFP_OPTION_NONE, MFPCreateMediaPlayer, MFSTARTUP_FULL, MFShutdown, MFStartup,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTimeEx},
        },
        UI::{
            Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi},
            WindowsAndMessaging::{
                CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, GW_HWNDPREV,
                GWLP_USERDATA, GetClientRect, GetWindow, GetWindowLongPtrW, GetWindowRect,
                HWND_TOPMOST, IsWindowVisible, KillTimer, MA_NOACTIVATEANDEAT, NONCLIENTMETRICSW,
                PostMessageW, RegisterClassW, SPI_GETHIGHCONTRAST, SPI_GETNONCLIENTMETRICS,
                SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
                SWP_NOZORDER, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageW,
                SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, SystemParametersInfoW,
                UnregisterClassW, WINDOW_EX_STYLE, WM_APP, WM_DISPLAYCHANGE, WM_DPICHANGED,
                WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
                WM_SETTINGCHANGE, WM_SIZE, WM_SYSCOLORCHANGE, WM_THEMECHANGED, WM_TIMER, WNDCLASSW,
                WS_BORDER, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
                WS_VISIBLE,
            },
        },
    },
    core::{Error, HRESULT, PCWSTR, Result, implement, w},
};
use windows_numerics::Vector2;

const CLASS_NAME_PREFIX: &str = "CursorPeek.PreviewWindow";
static NEXT_CLASS_ID: AtomicU64 = AtomicU64::new(1);
const BASE_DPI: f32 = 96.0;
const TEXT_MARGIN: f32 = 4.0;
const TEXT_BODY_METADATA_GAP: f32 = 4.0;
const TEXT_METADATA_HEIGHT: f32 = 56.0;
const TEXT_MINIMUM_WIDTH: f32 = 158.0;
const TEXT_MEASUREMENT_GUARD: f32 = 1.0;
const IMAGE_MARGIN: f32 = 4.0;
const IMAGE_METADATA_HEIGHT: f32 = 56.0;
const IMAGE_MINIMUM_WIDTH: f32 = 158.0;
const SYSTEM_APPEARANCE_CHANGED_MESSAGE: u32 = WM_APP + 20;
const VIDEO_PREROLL_TIMER_ID: usize = 1;
const VIDEO_PREROLL_MS: u32 = 400;
const VIDEO_AUDIO_FADE_TIMER_ID: usize = 2;
const VIDEO_AUDIO_FADE_INTERVAL_MS: u32 = 20;
const VIDEO_AUDIO_FADE_STEPS: u8 = 8;
const MAX_Z_ORDER_SCAN: usize = 64;

pub(crate) struct PreviewWindow {
    hwnd: HWND,
    state: Box<RefCell<PreviewWindowState>>,
    theme_observer: Option<ThemeObserver>,
    video_player: RefCell<Option<VideoPlayer>>,
    _class: RegisteredPreviewClass,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl PreviewWindow {
    pub(crate) fn create() -> Result<Self> {
        Self::create_with_theme(Theme::System)
    }

    pub(crate) fn create_with_theme(theme: Theme) -> Result<Self> {
        let class = RegisteredPreviewClass::register()?;
        let class_name = class.name();
        let state = Box::new(RefCell::new(PreviewWindowState::new(theme)?));
        let state_pointer = std::ptr::from_ref(state.as_ref()).cast::<c_void>();
        let ex_style: WINDOW_EX_STYLE = WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;

        // SAFETY: The registered class and boxed state outlive the HWND. WM_NCCREATE copies the
        // stable RefCell pointer into GWLP_USERDATA, and Drop clears that pointer before destroying
        // the window.
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                class_name,
                w!("CursorPeek preview"),
                WS_POPUP | WS_BORDER,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(class.instance),
                Some(state_pointer),
            )?
        };
        let theme_observer = ThemeObserver::new(hwnd);

        Ok(Self {
            hwnd,
            state,
            theme_observer,
            video_player: RefCell::new(None),
            _class: class,
            _thread_affinity: PhantomData,
        })
    }

    pub(crate) fn set_theme(&self, theme: Theme) -> Result<()> {
        let dpi = self.window_dpi()?;
        self.state.borrow_mut().set_theme(theme, dpi)?;
        self.redraw()
    }

    pub(crate) fn show_at(&self, anchor: PhysicalScreenPoint) -> Result<PreviewPlacement> {
        self.show(anchor, PreviewSize::diagnostic(), None)
    }

    pub(crate) fn show_text_at(
        &self,
        anchor: PhysicalScreenPoint,
        size: PreviewSize,
        preview: &TextPreview,
    ) -> Result<PreviewPlacement> {
        self.stop_video();
        self.show(
            anchor,
            size,
            Some(RetainedContent::Text(TextContent::from_preview(preview))),
        )
    }

    pub(crate) fn show_image_at(
        &self,
        anchor: PhysicalScreenPoint,
        size: PreviewSize,
        preview: ImagePreview,
    ) -> Result<PreviewPlacement> {
        self.stop_video();
        self.show(
            anchor,
            size,
            Some(RetainedContent::Image(ImageContent::from_preview(preview))),
        )
    }

    pub(crate) fn show_video_at(
        &self,
        anchor: PhysicalScreenPoint,
        size: PreviewSize,
        preview: VideoPreview,
        play_audio: bool,
        smooth_start: bool,
    ) -> Result<PreviewPlacement> {
        self.stop_video();
        let file_lock = crate::worker::video::lock_for_playback(&preview.path)
            .map_err(|_| Error::from_hresult(E_INVALIDARG))?;
        match VideoPlayer::create(self.hwnd, &preview.path, file_lock) {
            Ok(player) => {
                let (native_width, native_height) = player.native_size();
                let (width, height) =
                    fit_dimensions(native_width, native_height, size.width(), size.height())
                        .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
                let placement = self.show_with_visibility(
                    anchor,
                    PreviewSize::new(width, height),
                    None,
                    !smooth_start,
                )?;
                player.fill_parent()?;
                if smooth_start {
                    self.state
                        .borrow_mut()
                        .begin_video_preroll(player.player.clone(), play_audio);
                } else {
                    if play_audio {
                        self.state
                            .borrow_mut()
                            .begin_audio_fade(player.player.clone())?;
                        self.start_audio_fade_timer()?;
                    }
                }
                self.video_player.replace(Some(player));
                if smooth_start {
                    // SAFETY: This live preview HWND owns the fixed private timer identifier.
                    // Expiry is delivered as WM_TIMER on the same UI thread without a callback.
                    if unsafe {
                        SetTimer(
                            Some(self.hwnd),
                            VIDEO_PREROLL_TIMER_ID,
                            VIDEO_PREROLL_MS,
                            None,
                        )
                    } == 0
                    {
                        self.stop_video();
                        return Err(Error::from_thread());
                    }
                }
                Ok(placement)
            }
            Err(error) => {
                let _ = self.hide();
                Err(error)
            }
        }
    }

    fn stop_video(&self) {
        // SAFETY: Killing an absent timer is benign; this HWND remains live for the UI-thread call.
        let _ = unsafe { KillTimer(Some(self.hwnd), VIDEO_PREROLL_TIMER_ID) };
        // SAFETY: Killing an absent timer is benign; this HWND remains live for the UI-thread call.
        let _ = unsafe { KillTimer(Some(self.hwnd), VIDEO_AUDIO_FADE_TIMER_ID) };
        self.state.borrow_mut().cancel_video_preroll();
        self.state.borrow_mut().cancel_audio_fade();
        self.video_player.borrow_mut().take();
    }

    fn start_audio_fade_timer(&self) -> Result<()> {
        // SAFETY: This live preview HWND owns the fixed private timer identifier.
        if unsafe {
            SetTimer(
                Some(self.hwnd),
                VIDEO_AUDIO_FADE_TIMER_ID,
                VIDEO_AUDIO_FADE_INTERVAL_MS,
                None,
            )
        } == 0
        {
            return Err(Error::from_thread());
        }
        Ok(())
    }

    fn show(
        &self,
        anchor: PhysicalScreenPoint,
        size: PreviewSize,
        content: Option<RetainedContent>,
    ) -> Result<PreviewPlacement> {
        self.show_with_visibility(anchor, size, content, true)
    }

    fn show_with_visibility(
        &self,
        anchor: PhysicalScreenPoint,
        size: PreviewSize,
        content: Option<RetainedContent>,
        visible: bool,
    ) -> Result<PreviewPlacement> {
        let visibility = if visible {
            SWP_SHOWWINDOW
        } else {
            Default::default()
        };
        // SAFETY: The hidden top-level HWND is owned by this UI thread. Moving the one-pixel setup
        // window first associates it with the anchor monitor without activating or showing it.
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

        let dpi = self.window_dpi()?;

        // SAFETY: MonitorFromPoint returns a borrowed monitor handle and transfers no ownership.
        let monitor = unsafe {
            MonitorFromPoint(
                windows::Win32::Foundation::POINT {
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
        // SAFETY: The borrowed monitor is valid and monitor_info is writable storage with the
        // required size initialized.
        if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
            return Err(Error::from_thread());
        }

        let work_area = ScreenRect {
            left: monitor_info.rcWork.left,
            top: monitor_info.rcWork.top,
            right: monitor_info.rcWork.right,
            bottom: monitor_info.rcWork.bottom,
        };
        let title = content
            .as_ref()
            .map_or(w!("CursorPeek preview"), RetainedContent::accessible_title);
        let has_content = content.is_some();
        self.state.borrow_mut().configure(content, dpi)?;
        let placement = if has_content {
            let client_size = self.state.borrow().content_client_pixel_size(size)?;
            let window_size = adjusted_window_pixel_size(client_size, dpi)?;
            place_preview_pixels(
                anchor,
                work_area,
                dpi,
                window_size.width,
                window_size.height,
            )
        } else {
            place_preview(anchor, work_area, dpi, size)
        }
        .ok_or_else(Error::from_thread)?;

        // SAFETY: The live preview HWND and selected static terminated title remain valid.
        unsafe { SetWindowTextW(self.hwnd, title)? };

        // SAFETY: The HWND is positioned wholly inside the selected work area. TOPMOST and
        // SHOWWINDOW display it; NOACTIVATE plus WS_EX_NOACTIVATE preserve the foreground window.
        // Any synchronous WM_SIZE callback observes the already-updated RefCell state.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                placement.x,
                placement.y,
                placement.width,
                placement.height,
                SWP_NOACTIVATE | visibility,
            )?;
        }

        self.state.borrow_mut().prepare(self.hwnd)?;
        self.redraw()?;
        Ok(placement)
    }

    fn redraw(&self) -> Result<()> {
        // SAFETY: Invalidating and synchronously updating this live UI-thread HWND causes its
        // WM_PAINT path to render retained state. No borrowed pointer is carried in a message.
        if !unsafe {
            RedrawWindow(
                Some(self.hwnd),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_UPDATENOW,
            )
        }
        .as_bool()
        {
            return Err(Error::from_thread());
        }

        match self.state.borrow_mut().last_paint_error.take() {
            Some(code) => Err(Error::from_hresult(code)),
            None => Ok(()),
        }
    }

    fn window_dpi(&self) -> Result<u32> {
        // SAFETY: The preview HWND remains live for the owning UI-thread call.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        (dpi != 0).then_some(dpi).ok_or_else(Error::from_thread)
    }

    pub(crate) fn hide(&self) -> Result<()> {
        self.stop_video();
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

    pub(crate) fn repair_explorer_infotip_overlap(
        &self,
        explorer_window: ExplorerWindowId,
    ) -> Result<bool> {
        // SAFETY: The preview HWND is owned by this UI thread. A hidden preview has no visual
        // overlap to repair.
        if !unsafe { IsWindowVisible(self.hwnd).as_bool() } {
            return Ok(false);
        }

        let mut preview_bounds = RECT::default();
        // SAFETY: `preview_bounds` is live writable storage and the preview HWND remains valid.
        unsafe { GetWindowRect(self.hwnd, &mut preview_bounds) }?;
        // SAFETY: The preview HWND remains live. GetWindow returns a borrowed adjacent top-level
        // HWND or no relation; the bounded scan never stores a handle beyond this synchronous call.
        let mut candidate = unsafe { GetWindow(self.hwnd, GW_HWNDPREV) }.ok();

        for _ in 0..MAX_Z_ORDER_SCAN {
            let Some(window) = candidate else {
                return Ok(false);
            };
            // Query the next relation before inspecting the borrowed candidate so a window that
            // disappears during inspection simply ends this bounded fallback scan.
            // SAFETY: `window` is the borrowed live-or-stale result of the preceding synchronous
            // Z-order query. A stale handle produces no relation and terminates the scan.
            candidate = unsafe { GetWindow(window, GW_HWNDPREV) }.ok();

            if !is_explorer_infotip_window(window, explorer_window) {
                continue;
            }

            let mut infotip_bounds = RECT::default();
            // SAFETY: `infotip_bounds` is writable storage. The borrowed cross-process HWND is
            // queried synchronously and may disappear, in which case this candidate is ignored.
            if unsafe { GetWindowRect(window, &mut infotip_bounds) }.is_err()
                || !rectangles_overlap(preview_bounds, infotip_bounds)
            {
                continue;
            }

            self.raise_topmost_without_activation()?;
            return Ok(true);
        }

        Ok(false)
    }

    fn raise_topmost_without_activation(&self) -> Result<()> {
        // SAFETY: Reordering this live top-level HWND with no move, size, owner, or activation
        // change affects only CursorPeek. HWND_TOPMOST restores it above a later topmost infotip.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER,
            )
        }
    }

    #[cfg(test)]
    pub(crate) fn is_visible(&self) -> bool {
        // SAFETY: The handle is owned and live for self.
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub(crate) fn eats_mouse_activation(&self) -> bool {
        // SAFETY: This synchronous policy probe sends the popup's documented activation message
        // with zeroed informational parameters and retains no borrowed data.
        let result = unsafe { SendMessageW(self.hwnd, WM_MOUSEACTIVATE, None, None) };
        result.0 == isize::try_from(MA_NOACTIVATEANDEAT).expect("the constant fits isize")
    }

    pub(crate) const fn handle(&self) -> HWND {
        self.hwnd
    }

    #[cfg(test)]
    fn has_content_layouts(&self) -> bool {
        self.state.borrow().layouts.is_some()
    }

    #[cfg(test)]
    fn has_image_bitmap(&self) -> bool {
        self.state.borrow().image_bitmap.is_some()
    }

    #[cfg(test)]
    fn has_device_resources(&self) -> bool {
        self.state.borrow().device.is_some()
    }

    #[cfg(test)]
    fn force_device_loss_and_redraw(&self) -> Result<()> {
        self.state.borrow_mut().force_recreate_target = true;
        self.redraw()
    }

    #[cfg(test)]
    fn accessible_title(&self) -> String {
        // SAFETY: Both calls synchronously inspect the live preview HWND. The allocation includes
        // room for the terminator required by GetWindowTextW.
        let length =
            unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowTextLengthW(self.hwnd) };
        let mut units = vec![0_u16; usize::try_from(length).unwrap_or(0) + 1];
        // SAFETY: `units` is writable for the advertised length plus its terminator, and the live
        // HWND is used synchronously on its owning thread.
        let copied = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(self.hwnd, &mut units)
        };
        String::from_utf16_lossy(&units[..usize::try_from(copied).unwrap_or(0)])
    }
}

#[implement(IMFPMediaPlayerCallback)]
struct VideoPlayerCallback;

impl IMFPMediaPlayerCallback_Impl for VideoPlayerCallback_Impl {
    fn OnMediaPlayerEvent(&self, event: *const MFP_EVENT_HEADER) {
        if event.is_null() {
            return;
        }
        // SAFETY: MFPlay invokes the callback with one synchronous event header that remains live
        // for this method. The embedded player is borrowed and cloned before the callback returns.
        let header = unsafe { &*event };
        if header.hrEvent.is_err() || header.eEventType != MFP_EVENT_TYPE_PLAYBACK_ENDED {
            return;
        }
        if let Some(player) = header.pMediaPlayer.as_ref() {
            // Stop resets the current media item to its beginning. Replaying here creates the
            // short hover loop without queuing work onto the no-activate UI window.
            // SAFETY: The callback header holds a live MFPlay interface for this callback.
            let _ = unsafe { player.Stop() };
            // SAFETY: The same live interface remains valid after the synchronous stop call.
            let _ = unsafe { player.Play() };
        }
    }
}

struct VideoPlayer {
    player: IMFPMediaPlayer,
    video_window: VideoChildWindow,
    native_width: u32,
    native_height: u32,
    _file_lock: crate::worker::video::PlaybackFileLock,
    _media_foundation: MediaFoundationSession,
}

impl VideoPlayer {
    fn create(
        hwnd: HWND,
        path: &[u16],
        file_lock: crate::worker::video::PlaybackFileLock,
    ) -> Result<Self> {
        let terminated = media_foundation_path(path)?;
        let media_foundation = MediaFoundationSession::start()?;
        let video_window = VideoChildWindow::create(hwnd)?;
        let callback: IMFPMediaPlayerCallback = VideoPlayerCallback.into();
        let mut player = None;
        // SAFETY: The callback, dedicated live child HWND, and output storage remain valid for the
        // synchronous call. MFPlay retains its own callback reference.
        unsafe {
            MFPCreateMediaPlayer(
                PCWSTR::null(),
                false,
                MFP_OPTION_NONE,
                &callback,
                Some(video_window.hwnd),
                Some(&mut player),
            )?;
        }
        let player = player.ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        let mut owner = Self {
            player,
            video_window,
            native_width: 0,
            native_height: 0,
            _file_lock: file_lock,
            _media_foundation: media_foundation,
        };
        let mut item = None;
        // SAFETY: The ordinary absolute path is terminated and stays live for synchronous item
        // creation. The output receives one owned MFPlay media-item interface.
        unsafe {
            owner.player.CreateMediaItemFromURL(
                PCWSTR(terminated.as_ptr()),
                true,
                0,
                Some(&mut item),
            )?;
        }
        let item = item.ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        // SAFETY: Synchronous media-item creation completed and the returned interface is live.
        let stream_count = unsafe { item.GetNumberOfStreams()? };
        let mut frame_size = None;
        for stream in 0..stream_count {
            // SAFETY: The synchronous media item owns `stream_count` streams and returns an owned
            // PROPVARIANT. Non-video streams simply do not expose MF_MT_FRAME_SIZE.
            let Ok(value) = (unsafe { item.GetStreamAttribute(stream, &MF_MT_FRAME_SIZE) }) else {
                continue;
            };
            if let Ok(packed) = u64::try_from(&value) {
                frame_size = Some(packed);
                break;
            }
        }
        let packed = frame_size.ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        owner.native_width =
            u32::try_from(packed >> 32).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
        owner.native_height =
            u32::try_from(packed & u64::from(u32::MAX)).expect("the low half fits u32");
        if owner.native_width == 0 || owner.native_height == 0 {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        // SAFETY: Both interfaces are live and owned through this synchronous setup sequence.
        unsafe { owner.player.SetMediaItem(&item)? };
        // Keep the audio path unmuted from the outset, but silent at zero volume. Toggling MFPlay
        // mute after playback starts can open the audio endpoint abruptly and produce a click.
        // SAFETY: MFPlay returned this live player interface on the current COM-initialized thread.
        unsafe {
            owner.player.SetVolume(0.0)?;
            owner.player.SetMute(false)?;
        }
        // SAFETY: The media item, video child, and player remain live in `owner`.
        unsafe { owner.player.Play()? };
        Ok(owner)
    }

    const fn native_size(&self) -> (u32, u32) {
        (self.native_width, self.native_height)
    }

    fn fill_parent(&self) -> Result<()> {
        let mut bounds = RECT::default();
        // SAFETY: The parent preview HWND and writable rectangle are live.
        unsafe { GetClientRect(self.video_window.parent, &mut bounds)? };
        let width = bounds
            .right
            .checked_sub(bounds.left)
            .filter(|value| *value > 0)
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        let height = bounds
            .bottom
            .checked_sub(bounds.top)
            .filter(|value| *value > 0)
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        // SAFETY: The child belongs to this parent and fills its checked client rectangle without
        // activation or Z-order changes.
        unsafe {
            SetWindowPos(
                self.video_window.hwnd,
                None,
                0,
                0,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        }
    }
}

struct VideoChildWindow {
    hwnd: HWND,
    parent: HWND,
}

impl VideoChildWindow {
    fn create(parent: HWND) -> Result<Self> {
        // SAFETY: STATIC is a system class. The dedicated child has no borrowed creation data and
        // remains owned by this RAII wrapper inside the live preview parent.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("CursorPeek.VideoSurface"),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                1,
                1,
                Some(parent),
                None,
                None,
                None,
            )?
        };
        Ok(Self { hwnd, parent })
    }
}

impl Drop for VideoChildWindow {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns the dedicated child and destroys it once while its parent lives.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

fn media_foundation_path(path: &[u16]) -> Result<Vec<u16>> {
    if path.is_empty() || path.contains(&0) {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    // GetFinalPathNameByHandleW deliberately gives the worker a canonical `\\?\C:\...` path.
    // MFPlay accepts ordinary absolute DOS paths but rejects that extended-length spelling for
    // some media sources, so remove only the verified drive-path prefix at this API boundary.
    let extended_prefix = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    let path = path.strip_prefix(&extended_prefix).unwrap_or(path);
    if path.len() < 3
        || !u8::try_from(path[0]).is_ok_and(|unit| unit.is_ascii_alphabetic())
        || path[1] != b':' as u16
        || path[2] != b'\\' as u16
    {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    let mut terminated = Vec::with_capacity(path.len() + 1);
    terminated.extend_from_slice(path);
    terminated.push(0);
    Ok(terminated)
}

struct MediaFoundationSession;

impl MediaFoundationSession {
    fn start() -> Result<Self> {
        // SAFETY: Startup and shutdown are balanced by this thread-affine playback owner.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL)? };
        Ok(Self)
    }
}

impl Drop for MediaFoundationSession {
    fn drop(&mut self) {
        // SAFETY: This balances this owner's successful MFStartup after the player was shut down.
        let _ = unsafe { MFShutdown() };
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        // Shutdown synchronously stops rendering and audio before the hover window is hidden.
        // SAFETY: This owner shuts down its live player once before Media Foundation shutdown.
        let _ = unsafe { self.player.Shutdown() };
    }
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        let _ = self.hide();
        drop(self.theme_observer.take());

        // SAFETY: Clearing GWLP_USERDATA prevents teardown callbacks from reaching the boxed state
        // through an alias to this exclusive Drop borrow. The !Send owner destroys its HWND on the
        // creating UI thread before the state and registered class are released.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct ThemeObserver {
    settings: UISettings,
    token: i64,
}

impl ThemeObserver {
    fn new(hwnd: HWND) -> Option<Self> {
        let settings = UISettings::new().ok()?;
        let raw_hwnd = hwnd.0 as usize;
        let handler =
            TypedEventHandler::<UISettings, windows::core::IInspectable>::new(move |_, _| {
                let hwnd = HWND(raw_hwnd as *mut c_void);
                // SAFETY: The observer is removed before the owning preview HWND is destroyed.
                // The event carries no borrowed state and a late post is allowed to fail.
                let _ = unsafe {
                    PostMessageW(
                        Some(hwnd),
                        SYSTEM_APPEARANCE_CHANGED_MESSAGE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
                Ok(())
            });
        let token = settings.ColorValuesChanged(&handler).ok()?;
        Some(Self { settings, token })
    }
}

impl Drop for ThemeObserver {
    fn drop(&mut self) {
        let _ = self.settings.RemoveColorValuesChanged(self.token);
    }
}

struct PreviewWindowState {
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    formats: TextFormats,
    theme: Theme,
    colors: PreviewColors,
    content: Option<RetainedContent>,
    layouts: Option<ContentLayouts>,
    device: Option<DeviceResources>,
    image_bitmap: Option<ID2D1Bitmap>,
    pixel_size: D2D_SIZE_U,
    dpi: u32,
    last_paint_error: Option<HRESULT>,
    preroll_player: Option<IMFPMediaPlayer>,
    preroll_enable_audio: bool,
    audio_fade_player: Option<IMFPMediaPlayer>,
    audio_fade_step: u8,
    #[cfg(test)]
    force_recreate_target: bool,
}

impl PreviewWindowState {
    fn new(theme: Theme) -> Result<Self> {
        // SAFETY: Both factory calls write a fresh COM interface. The Direct2D factory is used only
        // on this UI thread; the recommended shared DirectWrite factory is retained with COM
        // reference counting.
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }?;
        // SAFETY: The current UI thread has a live COM apartment and the call returns an owned,
        // reference-counted factory interface without borrowing caller storage.
        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }?;

        let formats = TextFormats::create(&dwrite_factory, 96)?;

        Ok(Self {
            d2d_factory,
            dwrite_factory,
            formats,
            theme,
            colors: PreviewColors::resolve(theme),
            content: None,
            layouts: None,
            device: None,
            image_bitmap: None,
            pixel_size: D2D_SIZE_U::default(),
            dpi: 96,
            last_paint_error: None,
            preroll_player: None,
            preroll_enable_audio: false,
            audio_fade_player: None,
            audio_fade_step: 0,
            #[cfg(test)]
            force_recreate_target: false,
        })
    }

    fn configure(&mut self, content: Option<RetainedContent>, dpi: u32) -> Result<()> {
        if self.dpi != dpi {
            self.dpi = dpi;
            self.refresh_environment()?;
        }
        self.content = content;
        self.layouts = None;
        self.image_bitmap = None;
        self.last_paint_error = None;
        Ok(())
    }

    fn begin_video_preroll(&mut self, player: IMFPMediaPlayer, enable_audio: bool) {
        self.preroll_player = Some(player);
        self.preroll_enable_audio = enable_audio;
    }

    fn finish_video_preroll(&mut self) -> Result<bool> {
        let should_fade_audio = self.preroll_enable_audio;
        if self.preroll_enable_audio
            && let Some(player) = self.preroll_player.as_ref()
        {
            self.begin_audio_fade(player.clone())?;
        }
        self.cancel_video_preroll();
        Ok(should_fade_audio)
    }

    fn cancel_video_preroll(&mut self) {
        self.preroll_player = None;
        self.preroll_enable_audio = false;
    }

    fn begin_audio_fade(&mut self, player: IMFPMediaPlayer) -> Result<()> {
        // Playback already owns an open, unmuted audio path at zero volume. Only changing volume
        // here avoids the endpoint transition that can produce a click on some audio drivers.
        // SAFETY: The retained player belongs to the live video preview on this UI thread.
        unsafe { player.SetVolume(0.0)? };
        self.audio_fade_player = Some(player);
        self.audio_fade_step = 0;
        Ok(())
    }

    fn advance_audio_fade(&mut self) -> bool {
        let Some(player) = self.audio_fade_player.as_ref() else {
            return true;
        };
        self.audio_fade_step = self.audio_fade_step.saturating_add(1);
        let volume = f32::from(self.audio_fade_step) / f32::from(VIDEO_AUDIO_FADE_STEPS);
        // SAFETY: The retained player belongs to the live video preview on this UI thread.
        if unsafe { player.SetVolume(volume.min(1.0)) }.is_err()
            || self.audio_fade_step >= VIDEO_AUDIO_FADE_STEPS
        {
            self.cancel_audio_fade();
            return true;
        }
        false
    }

    fn cancel_audio_fade(&mut self) {
        self.audio_fade_player = None;
        self.audio_fade_step = 0;
    }

    fn content_client_pixel_size(&self, maximum: PreviewSize) -> Result<D2D_SIZE_U> {
        match self
            .content
            .as_ref()
            .expect("content sizing runs only for retained preview content")
        {
            RetainedContent::Text(content) => text_client_pixel_size(
                &self.dwrite_factory,
                &self.formats,
                content,
                maximum,
                self.dpi,
            ),
            RetainedContent::Image(content) => {
                image_client_pixel_size(&content.preview, maximum, self.dpi)
                    .ok_or_else(Error::from_thread)
            }
        }
    }

    fn set_theme(&mut self, theme: Theme, dpi: u32) -> Result<()> {
        self.theme = theme;
        self.dpi = dpi;
        self.refresh_environment()
    }

    fn refresh_environment(&mut self) -> Result<()> {
        let colors = PreviewColors::resolve(self.theme);
        let formats = TextFormats::create(&self.dwrite_factory, self.dpi)?;
        self.colors = colors;
        self.formats = formats;
        self.layouts = None;
        self.discard_device_resources();
        Ok(())
    }

    fn discard_device_resources(&mut self) {
        self.image_bitmap = None;
        self.device = None;
    }

    fn prepare(&mut self, hwnd: HWND) -> Result<()> {
        let pixel_size = client_pixel_size(hwnd)?;
        if self.pixel_size != pixel_size {
            self.pixel_size = pixel_size;
            if let Some(device) = self.device.as_ref() {
                // SAFETY: The render target belongs to this HWND and UI thread. pixel_size was
                // measured from the same live client area.
                if let Err(error) = unsafe { device.target.Resize(&pixel_size) } {
                    if error.code() == D2DERR_RECREATE_TARGET {
                        self.discard_device_resources();
                    } else {
                        return Err(error);
                    }
                }
            }
            self.layouts = None;
        }

        if self.content.is_some()
            && self.layouts.is_none()
            && pixel_size.width != 0
            && pixel_size.height != 0
        {
            self.layouts = Some(self.create_layouts()?);
        }
        Ok(())
    }

    fn create_layouts(&self) -> Result<ContentLayouts> {
        let content = self
            .content
            .as_ref()
            .expect("layouts are created only for retained content");
        let width = pixels_to_dips(self.pixel_size.width, self.dpi);
        let height = pixels_to_dips(self.pixel_size.height, self.dpi);
        match content {
            RetainedContent::Text(content) => {
                let layout_width = (width - TEXT_MARGIN * 2.0).max(1.0);
                let metadata_origin_y =
                    (height - TEXT_MARGIN - TEXT_METADATA_HEIGHT).max(TEXT_MARGIN);
                let body_height =
                    (metadata_origin_y - TEXT_BODY_METADATA_GAP - TEXT_MARGIN).max(1.0);
                let body = if content.body.is_empty() {
                    None
                } else {
                    // SAFETY: The bounded UTF-16 body and retained format remain valid for this
                    // synchronous call; DirectWrite returns an independently owned layout.
                    Some(unsafe {
                        self.dwrite_factory.CreateTextLayout(
                            &content.body,
                            &self.formats.body,
                            layout_width,
                            body_height,
                        )?
                    })
                };
                // SAFETY: The bounded UTF-16 metadata remains alive in content for this
                // synchronous call. The returned layout owns its DirectWrite state.
                let metadata = unsafe {
                    self.dwrite_factory.CreateTextLayout(
                        &content.metadata,
                        &self.formats.metadata,
                        layout_width,
                        TEXT_METADATA_HEIGHT,
                    )?
                };
                Ok(ContentLayouts::Text {
                    body,
                    metadata,
                    metadata_origin_y,
                })
            }
            RetainedContent::Image(content) => {
                let layout_width = (width - IMAGE_MARGIN * 2.0).max(1.0);
                let metadata_origin_y =
                    (height - IMAGE_MARGIN - IMAGE_METADATA_HEIGHT).max(IMAGE_MARGIN);
                // SAFETY: The bounded UTF-16 metadata remains alive in content for this
                // synchronous call. The returned layout owns its DirectWrite state.
                let metadata = unsafe {
                    self.dwrite_factory.CreateTextLayout(
                        &content.metadata,
                        &self.formats.metadata,
                        layout_width,
                        IMAGE_METADATA_HEIGHT,
                    )?
                };
                Ok(ContentLayouts::Image {
                    metadata,
                    metadata_origin_y,
                })
            }
        }
    }

    fn render_with_recovery(&mut self, hwnd: HWND) -> Result<()> {
        match self.render_once(hwnd) {
            Err(error) if error.code() == D2DERR_RECREATE_TARGET => {
                self.discard_device_resources();
                self.render_once(hwnd)
            }
            result => result,
        }
    }

    fn render_once(&mut self, hwnd: HWND) -> Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.force_recreate_target) {
            return Err(Error::from_hresult(D2DERR_RECREATE_TARGET));
        }

        self.prepare(hwnd)?;
        if self.pixel_size.width == 0 || self.pixel_size.height == 0 {
            return Ok(());
        }
        if self.device.is_none() {
            self.device = Some(self.create_device_resources(hwnd)?);
        }
        if matches!(self.content, Some(RetainedContent::Image(_))) && self.image_bitmap.is_none() {
            self.image_bitmap = Some(self.create_image_bitmap()?);
        }

        let device = self
            .device
            .as_ref()
            .expect("device resources are created before drawing");
        // SAFETY: BeginDraw and EndDraw are paired without an early return. Every retained layout,
        // brush, and target is a live COM resource created for this UI-thread HWND.
        unsafe {
            device.target.BeginDraw();
            device.target.Clear(Some(&self.colors.background));
            if let (Some(bitmap), Some(RetainedContent::Image(content))) =
                (self.image_bitmap.as_ref(), self.content.as_ref())
                && let Some(destination) =
                    image_destination_rect(&content.preview, self.pixel_size, self.dpi)
            {
                device.target.DrawBitmap(
                    bitmap,
                    Some(&destination),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
            if let Some(layouts) = self.layouts.as_ref() {
                match layouts {
                    ContentLayouts::Text {
                        body,
                        metadata,
                        metadata_origin_y,
                    } => {
                        if let Some(body) = body.as_ref() {
                            device.target.DrawTextLayout(
                                Vector2 {
                                    X: TEXT_MARGIN,
                                    Y: TEXT_MARGIN,
                                },
                                body,
                                &device.body_brush,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            );
                        }
                        device.target.DrawTextLayout(
                            Vector2 {
                                X: TEXT_MARGIN,
                                Y: *metadata_origin_y,
                            },
                            metadata,
                            &device.metadata_brush,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        );
                    }
                    ContentLayouts::Image {
                        metadata,
                        metadata_origin_y,
                    } => {
                        device.target.DrawTextLayout(
                            Vector2 {
                                X: IMAGE_MARGIN,
                                Y: *metadata_origin_y,
                            },
                            metadata,
                            &device.body_brush,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        );
                    }
                }
            }
            device.target.EndDraw(None, None)
        }
    }

    fn create_image_bitmap(&self) -> Result<ID2D1Bitmap> {
        let device = self
            .device
            .as_ref()
            .expect("an image bitmap is created only after the render target");
        let RetainedContent::Image(content) = self
            .content
            .as_ref()
            .expect("an image bitmap is created only for retained content")
        else {
            return Err(Error::from_hresult(E_INVALIDARG));
        };
        let (pitch, _) = checked_image_layout(&content.preview)
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        let properties = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: self.dpi as f32,
            dpiY: self.dpi as f32,
        };
        // SAFETY: checked_image_layout proves exact pitch/length and premultiplied BGRA pixels.
        // Direct2D copies the borrowed buffer into a new target-owned bitmap during this call.
        unsafe {
            device.target.CreateBitmap(
                D2D_SIZE_U {
                    width: content.preview.width,
                    height: content.preview.height,
                },
                Some(content.preview.premultiplied_bgra.as_ptr().cast()),
                pitch,
                &properties,
            )
        }
    }

    fn create_device_resources(&self, hwnd: HWND) -> Result<DeviceResources> {
        let target_properties = D2D1_RENDER_TARGET_PROPERTIES {
            dpiX: self.dpi as f32,
            dpiY: self.dpi as f32,
            ..Default::default()
        };
        let hwnd_properties = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: self.pixel_size,
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };

        // SAFETY: Both property structures are initialized for this live HWND and client size.
        // The returned target and brushes are owned COM references retained together.
        let target = unsafe {
            self.d2d_factory
                .CreateHwndRenderTarget(&target_properties, &hwnd_properties)?
        };
        // SAFETY: `target` is a live render target and the color value is fully initialized; the
        // call returns an owned COM brush without retaining the Rust reference.
        let body_brush = unsafe { target.CreateSolidColorBrush(&self.colors.body, None) }?;
        // SAFETY: The same live target synchronously copies the initialized metadata color and
        // returns a separate owned COM brush.
        let metadata_brush = unsafe { target.CreateSolidColorBrush(&self.colors.metadata, None) }?;
        Ok(DeviceResources {
            target,
            body_brush,
            metadata_brush,
        })
    }
}

struct TextContent {
    body: Vec<u16>,
    metadata: Vec<u16>,
}

impl TextContent {
    fn from_preview(preview: &TextPreview) -> Self {
        Self {
            body: preview.text.encode_utf16().collect(),
            metadata: text_preview_metadata(preview).encode_utf16().collect(),
        }
    }
}

enum RetainedContent {
    Text(TextContent),
    Image(ImageContent),
}

impl RetainedContent {
    const fn accessible_title(&self) -> PCWSTR {
        match self {
            Self::Text(_) => w!("CursorPeek text preview"),
            Self::Image(_) => w!("CursorPeek image preview"),
        }
    }
}

struct ImageContent {
    metadata: Vec<u16>,
    preview: ImagePreview,
}

impl ImageContent {
    fn from_preview(preview: ImagePreview) -> Self {
        Self {
            metadata: image_preview_metadata(&preview).encode_utf16().collect(),
            preview,
        }
    }
}

enum ContentLayouts {
    Text {
        body: Option<IDWriteTextLayout>,
        metadata: IDWriteTextLayout,
        metadata_origin_y: f32,
    },
    Image {
        metadata: IDWriteTextLayout,
        metadata_origin_y: f32,
    },
}

struct DeviceResources {
    target: ID2D1HwndRenderTarget,
    body_brush: ID2D1SolidColorBrush,
    metadata_brush: ID2D1SolidColorBrush,
}

struct TextFormats {
    body: IDWriteTextFormat,
    metadata: IDWriteTextFormat,
}

impl TextFormats {
    fn create(factory: &IDWriteFactory, dpi: u32) -> Result<Self> {
        let font = system_message_font(dpi);
        let family = PCWSTR(font.family.as_ptr());

        // SAFETY: The terminated family buffer remains live for this synchronous call, the
        // factory is valid, and DirectWrite retains its own format state.
        let body = unsafe {
            factory.CreateTextFormat(
                family,
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                (font.size + 1.0).min(32.0),
                w!("en-US"),
            )?
        };
        // SAFETY: The same terminated family buffer remains live for this synchronous call.
        let metadata = unsafe {
            factory.CreateTextFormat(
                family,
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                font.size,
                w!("en-US"),
            )?
        };
        // SAFETY: These calls mutate only newly created device-independent formats owned here.
        unsafe {
            body.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            body.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            body.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            metadata.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            metadata.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            metadata.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        }
        Ok(Self { body, metadata })
    }
}

struct SystemFont {
    family: Vec<u16>,
    size: f32,
}

fn system_message_font(dpi: u32) -> SystemFont {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: metrics is writable storage with the required size and dpi is obtained from the
    // target window. A failed query falls back to the documented Windows UI family.
    let queried = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            size_of::<NONCLIENTMETRICSW>() as u32,
            Some(std::ptr::from_mut(&mut metrics).cast()),
            0,
            dpi,
        )
    }
    .is_ok();

    if queried {
        let face = &metrics.lfMessageFont.lfFaceName;
        let length = face
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(face.len());
        if length != 0 {
            let mut family = face[..length].to_vec();
            family.push(0);
            let pixel_height = metrics.lfMessageFont.lfHeight.unsigned_abs().max(1);
            let size = (pixel_height as f32 * BASE_DPI / dpi.max(1) as f32).clamp(9.0, 32.0);
            return SystemFont { family, size };
        }
    }

    SystemFont {
        family: "Segoe UI"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect(),
        size: 12.0,
    }
}

#[derive(Clone, Copy)]
struct PreviewColors {
    background: D2D1_COLOR_F,
    body: D2D1_COLOR_F,
    metadata: D2D1_COLOR_F,
}

impl PreviewColors {
    fn system() -> Self {
        // SAFETY: GetSysColor returns process-independent COLORREF values and transfers no handle.
        unsafe {
            Self {
                background: color_from_colorref(GetSysColor(COLOR_WINDOW)),
                body: color_from_colorref(GetSysColor(COLOR_WINDOWTEXT)),
                metadata: color_from_colorref(GetSysColor(COLOR_GRAYTEXT)),
            }
        }
    }

    fn resolve(theme: Theme) -> Self {
        Self::resolve_for_environment(theme, high_contrast_active(), system_dark_mode())
    }

    fn resolve_for_environment(
        theme: Theme,
        high_contrast: bool,
        system_dark: Option<bool>,
    ) -> Self {
        if high_contrast {
            return Self::system();
        }

        match (theme, system_dark) {
            (Theme::System, Some(true)) => Self::dark(),
            (Theme::System, Some(false)) => Self::light(),
            (Theme::System, None) => Self::system(),
            (Theme::Light, _) => Self::light(),
            (Theme::Dark, _) => Self::dark(),
        }
    }

    const fn light() -> Self {
        Self {
            background: color_from_rgb(250, 250, 250),
            body: color_from_rgb(27, 27, 27),
            metadata: color_from_rgb(79, 79, 79),
        }
    }

    const fn dark() -> Self {
        Self {
            background: color_from_rgb(32, 32, 32),
            body: color_from_rgb(245, 245, 245),
            metadata: color_from_rgb(200, 200, 200),
        }
    }
}

fn system_dark_mode() -> Option<bool> {
    let foreground = UISettings::new()
        .and_then(|settings| settings.GetColorValue(UIColorType::Foreground))
        .ok()?;
    let weighted_brightness =
        5 * u16::from(foreground.G) + 2 * u16::from(foreground.R) + u16::from(foreground.B);
    Some(weighted_brightness > 8 * 128)
}

fn high_contrast_active() -> bool {
    let mut high_contrast = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: high_contrast is writable storage with the required size. This is a read-only
    // accessibility query; failure conservatively falls back to the selected palette.
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            size_of::<HIGHCONTRASTW>() as u32,
            Some(std::ptr::from_mut(&mut high_contrast).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
        )
    }
    .is_ok()
        && high_contrast.dwFlags.contains(HCF_HIGHCONTRASTON)
}

const fn color_from_rgb(red: u8, green: u8, blue: u8) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: red as f32 / 255.0,
        g: green as f32 / 255.0,
        b: blue as f32 / 255.0,
        a: 1.0,
    }
}

fn text_preview_metadata(preview: &TextPreview) -> String {
    let mut details = format!(
        "{}    ({})",
        format_file_size(preview.file_size),
        preview.encoding
    );
    if preview.encoding_was_guessed {
        details.push_str("  \u{b7}  guessed");
    }
    if preview.truncated {
        details.push_str("  \u{b7}  truncated");
    }
    if preview.linked_content {
        details.push_str("  \u{b7}  linked");
    }
    let modified = format_last_write_time(preview.last_write_time)
        .unwrap_or_else(|| "Modified time unavailable".to_owned());
    format!("{}\n{details}\n{modified}", preview.display_name)
}

fn image_preview_metadata(preview: &ImagePreview) -> String {
    let mut details = format!(
        "{}    ({} \u{d7} {})",
        format_file_size(preview.file_size),
        preview.source_width,
        preview.source_height,
    );
    if preview.first_frame_only {
        details.push_str("  \u{b7}  first frame");
    }
    if preview.linked_content {
        details.push_str("  \u{b7}  linked");
    }
    let modified = format_last_write_time(preview.last_write_time)
        .unwrap_or_else(|| "Modified time unavailable".to_owned());
    format!("{}\n{details}\n{modified}", preview.display_name)
}

fn format_last_write_time(value: i64) -> Option<String> {
    let value = u64::try_from(value).ok()?;
    let file_time = FILETIME {
        dwLowDateTime: value as u32,
        dwHighDateTime: (value >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: Both calls read fully initialized time structures and write to distinct valid
    // outputs. A null time-zone argument selects the current dynamic Windows time zone.
    unsafe {
        FileTimeToSystemTime(&file_time, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTimeEx(None, &utc, &mut local).ok()?;
    }

    let date = localized_system_time_part(&local, true)
        .unwrap_or_else(|| format!("{:04}-{:02}-{:02}", local.wYear, local.wMonth, local.wDay));
    let time = localized_system_time_part(&local, false)
        .unwrap_or_else(|| format!("{:02}:{:02}", local.wHour, local.wMinute));
    Some(format!("{date} {time}"))
}

fn localized_system_time_part(time: &SYSTEMTIME, date: bool) -> Option<String> {
    let mut buffer = [0_u16; 96];
    // SAFETY: `time` is initialized local calendar time. Null locale/format pointers request the
    // current user's defaults, and the fixed writable buffer supplies its exact element count.
    let length = unsafe {
        if date {
            GetDateFormatEx(
                PCWSTR::null(),
                DATE_SHORTDATE,
                Some(std::ptr::from_ref(time)),
                PCWSTR::null(),
                Some(&mut buffer),
                PCWSTR::null(),
            )
        } else {
            GetTimeFormatEx(
                PCWSTR::null(),
                TIME_NOSECONDS,
                Some(std::ptr::from_ref(time)),
                PCWSTR::null(),
                Some(&mut buffer),
            )
        }
    };
    let length = usize::try_from(length).ok()?;
    if !(2..=buffer.len()).contains(&length) || buffer[length - 1] != 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..length - 1]))
}

fn checked_image_layout(preview: &ImagePreview) -> Option<(u32, usize)> {
    if preview.width == 0
        || preview.height == 0
        || preview.source_width == 0
        || preview.source_height == 0
    {
        return None;
    }

    let pitch = preview.width.checked_mul(4)?;
    let length = usize::try_from(pitch)
        .ok()?
        .checked_mul(usize::try_from(preview.height).ok()?)?;
    if preview.premultiplied_bgra.len() != length
        || !preview
            .premultiplied_bgra
            .chunks_exact(4)
            .all(|pixel| pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3])
    {
        return None;
    }

    Some((pitch, length))
}

fn text_client_pixel_size(
    factory: &IDWriteFactory,
    formats: &TextFormats,
    content: &TextContent,
    maximum: PreviewSize,
    dpi: u32,
) -> Result<D2D_SIZE_U> {
    if dpi == 0 {
        return Err(Error::from_thread());
    }

    let maximum_width = maximum.width() as f32;
    let maximum_height = maximum.height() as f32;
    let horizontal_chrome = TEXT_MARGIN * 2.0;
    let body_gap = if content.body.is_empty() {
        0.0
    } else {
        TEXT_BODY_METADATA_GAP
    };
    let vertical_chrome = TEXT_MARGIN * 2.0 + TEXT_METADATA_HEIGHT + body_gap;
    let available_width = maximum_width - horizontal_chrome;
    let available_body_height = maximum_height - vertical_chrome;
    if available_width <= 0.0 || available_body_height <= 0.0 {
        return Err(Error::from_thread());
    }

    // SAFETY: Both bounded UTF-16 buffers and retained formats remain valid for the complete
    // synchronous measurement calls. DirectWrite owns the returned layout state.
    let metadata_layout = unsafe {
        factory.CreateTextLayout(
            &content.metadata,
            &formats.metadata,
            available_width,
            TEXT_METADATA_HEIGHT,
        )?
    };
    let metadata_metrics = text_layout_metrics(&metadata_layout)?;
    let body_metrics = if content.body.is_empty() {
        None
    } else {
        // SAFETY: The bounded body buffer and retained format remain valid for this synchronous
        // call. The layout is measured and dropped on the same UI thread.
        let body_layout = unsafe {
            factory.CreateTextLayout(
                &content.body,
                &formats.body,
                available_width,
                available_body_height,
            )?
        };
        Some(text_layout_metrics(&body_layout)?)
    };

    let measured_width = body_metrics
        .as_ref()
        .map_or(0.0, |metrics| metrics.widthIncludingTrailingWhitespace)
        .max(metadata_metrics.widthIncludingTrailingWhitespace)
        + TEXT_MEASUREMENT_GUARD;
    let minimum_width = TEXT_MINIMUM_WIDTH.min(available_width);
    let content_width = measured_width.max(minimum_width).min(available_width);
    let body_height = body_metrics.as_ref().map_or(0.0, |metrics| {
        (metrics.height + TEXT_MEASUREMENT_GUARD).min(available_body_height)
    });
    let client_width = (content_width + horizontal_chrome).min(maximum_width);
    let client_height = (body_height + vertical_chrome).min(maximum_height);

    Ok(D2D_SIZE_U {
        width: dips_to_pixels(client_width, dpi).ok_or_else(Error::from_thread)?,
        height: dips_to_pixels(client_height, dpi).ok_or_else(Error::from_thread)?,
    })
}

fn text_layout_metrics(layout: &IDWriteTextLayout) -> Result<DWRITE_TEXT_METRICS> {
    let mut metrics = DWRITE_TEXT_METRICS::default();
    // SAFETY: `metrics` is valid writable storage and the retained layout is live for this call.
    unsafe { layout.GetMetrics(&mut metrics)? };
    Ok(metrics)
}

fn image_client_pixel_size(
    preview: &ImagePreview,
    maximum: PreviewSize,
    dpi: u32,
) -> Option<D2D_SIZE_U> {
    if dpi == 0 || checked_image_layout(preview).is_none() {
        return None;
    }

    let max_width = dips_to_pixels(maximum.width() as f32, dpi)?;
    let max_height = dips_to_pixels(maximum.height() as f32, dpi)?;
    let margin = dips_to_pixels(IMAGE_MARGIN, dpi)?;
    let metadata_height = dips_to_pixels(IMAGE_METADATA_HEIGHT, dpi)?;
    let horizontal_chrome = margin.checked_mul(2)?;
    let vertical_chrome = horizontal_chrome.checked_add(metadata_height)?;
    let available_width = max_width.checked_sub(horizontal_chrome)?;
    let available_height = max_height.checked_sub(vertical_chrome)?;
    let (image_width, image_height) = fit_dimensions(
        preview.width,
        preview.height,
        available_width,
        available_height,
    )?;
    let minimum_width = dips_to_pixels(IMAGE_MINIMUM_WIDTH, dpi)?.min(available_width);
    let content_width = image_width.max(minimum_width);

    Some(D2D_SIZE_U {
        width: content_width.checked_add(horizontal_chrome)?,
        height: image_height.checked_add(vertical_chrome)?,
    })
}

fn adjusted_window_pixel_size(client: D2D_SIZE_U, dpi: u32) -> Result<D2D_SIZE_U> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: i32::try_from(client.width).map_err(|_| Error::from_thread())?,
        bottom: i32::try_from(client.height).map_err(|_| Error::from_thread())?,
    };
    // SAFETY: `rect` describes a desired client area and remains writable for the complete call.
    // The style values exactly match the preview HWND created by this module.
    unsafe {
        AdjustWindowRectExForDpi(
            &mut rect,
            WS_POPUP | WS_BORDER,
            false,
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            dpi,
        )?;
    }
    let (_, _, width, height) = checked_window_rect(&rect)?;
    Ok(D2D_SIZE_U {
        width: u32::try_from(width).map_err(|_| Error::from_thread())?,
        height: u32::try_from(height).map_err(|_| Error::from_thread())?,
    })
}

fn image_destination_rect(
    preview: &ImagePreview,
    pixel_size: D2D_SIZE_U,
    dpi: u32,
) -> Option<D2D_RECT_F> {
    if dpi == 0 || checked_image_layout(preview).is_none() {
        return None;
    }

    let client_width = pixels_to_dips(pixel_size.width, dpi);
    let client_height = pixels_to_dips(pixel_size.height, dpi);
    let available_width = client_width - IMAGE_MARGIN * 2.0;
    let available_height = client_height - IMAGE_MARGIN * 2.0 - IMAGE_METADATA_HEIGHT;
    if available_width <= 0.0 || available_height <= 0.0 {
        return None;
    }

    let image_width = pixels_to_dips(preview.width, dpi);
    let image_height = pixels_to_dips(preview.height, dpi);
    let scale = (available_width / image_width)
        .min(available_height / image_height)
        .min(1.0);
    let rendered_width = image_width * scale;
    let rendered_height = image_height * scale;
    let left = IMAGE_MARGIN + (available_width - rendered_width) / 2.0;
    let top = IMAGE_MARGIN;

    Some(D2D_RECT_F {
        left,
        top,
        right: left + rendered_width,
        bottom: top + rendered_height,
    })
}

fn format_file_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KiB", bytes as f64 / KIB),
        1_048_576..=1_073_741_823 => format!("{:.1} MiB", bytes as f64 / MIB),
        _ => format!("{:.1} GiB", bytes as f64 / GIB),
    }
}

fn color_from_colorref(color: u32) -> D2D1_COLOR_F {
    const CHANNEL_MAX: f32 = 255.0;
    D2D1_COLOR_F {
        r: (color & 0xff) as f32 / CHANNEL_MAX,
        g: ((color >> 8) & 0xff) as f32 / CHANNEL_MAX,
        b: ((color >> 16) & 0xff) as f32 / CHANNEL_MAX,
        a: 1.0,
    }
}

fn pixels_to_dips(pixels: u32, dpi: u32) -> f32 {
    pixels as f32 * BASE_DPI / dpi as f32
}

fn dips_to_pixels(dips: f32, dpi: u32) -> Option<u32> {
    if !dips.is_finite() || dips <= 0.0 || dpi == 0 {
        return None;
    }
    let pixels = (dips * dpi as f32 / BASE_DPI).ceil();
    if pixels > u32::MAX as f32 {
        None
    } else {
        Some(pixels as u32)
    }
}

fn client_pixel_size(hwnd: HWND) -> Result<D2D_SIZE_U> {
    let mut client = RECT::default();
    // SAFETY: client is valid writable storage and hwnd is the live preview window.
    unsafe { GetClientRect(hwnd, &mut client)? };
    let width = u32::try_from(client.right.saturating_sub(client.left))
        .map_err(|_| Error::from_thread())?;
    let height = u32::try_from(client.bottom.saturating_sub(client.top))
        .map_err(|_| Error::from_thread())?;
    Ok(D2D_SIZE_U { width, height })
}

struct RegisteredPreviewClass {
    instance: HINSTANCE,
    name: Vec<u16>,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl RegisteredPreviewClass {
    fn register() -> Result<Self> {
        // SAFETY: The returned module handle is borrowed from the current process.
        let instance = HINSTANCE::from(unsafe { GetModuleHandleW(None)? });
        let class_id = NEXT_CLASS_ID.fetch_add(1, Ordering::Relaxed);
        let name: Vec<u16> = format!("{CLASS_NAME_PREFIX}.{}.{class_id}", std::process::id())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(preview_window_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };

        // SAFETY: The initialized callback and owned class string remain valid until the only
        // preview HWND is destroyed and this class is unregistered on the same thread. The
        // process-local suffix permits independent preview owners in concurrent tests.
        let atom = unsafe { RegisterClassW(&window_class) };
        if atom == 0 {
            return Err(Error::from_thread());
        }

        Ok(Self {
            instance,
            name,
            _thread_affinity: PhantomData,
        })
    }

    fn name(&self) -> PCWSTR {
        PCWSTR(self.name.as_ptr())
    }
}

impl Drop for RegisteredPreviewClass {
    fn drop(&mut self) {
        // SAFETY: PreviewWindow destroys its only HWND before this field drops.
        unsafe {
            let _ = UnregisterClassW(self.name(), Some(self.instance));
        }
    }
}

struct PaintSession {
    hwnd: HWND,
    paint: PAINTSTRUCT,
}

impl PaintSession {
    fn begin(hwnd: HWND) -> Self {
        let mut paint = PAINTSTRUCT::default();
        // SAFETY: paint is valid writable storage and EndPaint is guaranteed by Drop.
        unsafe {
            let _ = BeginPaint(hwnd, &mut paint);
        }
        Self { hwnd, paint }
    }
}

impl Drop for PaintSession {
    fn drop(&mut self) {
        // SAFETY: This exactly balances the successful BeginPaint call for the same HWND and
        // PAINTSTRUCT, including while unwinding inside the callback containment boundary.
        unsafe {
            let _ = EndPaint(self.hwnd, &self.paint);
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
        dispatch_preview_message(hwnd, message, wparam, lparam)
    }))
    .unwrap_or(LRESULT(0))
}

fn dispatch_preview_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: WM_NCCREATE supplies a valid CREATESTRUCTW whose lpCreateParams is the stable
        // RefCell pointer passed to CreateWindowExW.
        let state = unsafe { &*(lparam.0 as *const CREATESTRUCTW) }.lpCreateParams;
        // SAFETY: `hwnd` is being initialized and `state` is the stable non-owning pointer supplied
        // by this process. Storing the integer value does not dereference or transfer it.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        }
        return LRESULT(1);
    }

    if message == WM_NCDESTROY {
        // SAFETY: Clearing the non-owning pointer prevents later callbacks from observing state.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
    }

    if message == WM_MOUSEACTIVATE {
        return LRESULT(isize::try_from(MA_NOACTIVATEANDEAT).expect("the constant fits isize"));
    }

    if message == WM_ERASEBKGND {
        return LRESULT(1);
    }

    if message == WM_TIMER && wparam.0 == VIDEO_PREROLL_TIMER_ID {
        // SAFETY: This consumes the private one-shot timer owned by this live preview HWND.
        let _ = unsafe { KillTimer(Some(hwnd), VIDEO_PREROLL_TIMER_ID) };
        if let Some(state) = preview_state(hwnd)
            && state.borrow_mut().finish_video_preroll().unwrap_or(false)
        {
            // SAFETY: The live preview HWND owns this private fade timer.
            let _ = unsafe {
                SetTimer(
                    Some(hwnd),
                    VIDEO_AUDIO_FADE_TIMER_ID,
                    VIDEO_AUDIO_FADE_INTERVAL_MS,
                    None,
                )
            };
        }
        // SAFETY: Preroll positioned and sized the hidden top-level preview already. This reveals
        // it without activation, movement, resizing, ownership, or Z-order changes.
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_SHOWWINDOW
                    | SWP_NOACTIVATE
                    | SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOOWNERZORDER
                    | SWP_NOZORDER,
            )
        };
        return LRESULT(0);
    }

    if message == WM_TIMER && wparam.0 == VIDEO_AUDIO_FADE_TIMER_ID {
        let finished =
            preview_state(hwnd).is_none_or(|state| state.borrow_mut().advance_audio_fade());
        if finished {
            // SAFETY: Killing an absent timer is benign for this live preview HWND.
            let _ = unsafe { KillTimer(Some(hwnd), VIDEO_AUDIO_FADE_TIMER_ID) };
        }
        return LRESULT(0);
    }

    if message == WM_DPICHANGED {
        let result = (|| {
            let dpi = u32::from((wparam.0 & 0xffff) as u16);
            if dpi == 0 || lparam.0 == 0 {
                return Err(Error::from_hresult(E_INVALIDARG));
            }
            let state = preview_state(hwnd).ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
            {
                let mut state = state.borrow_mut();
                state.dpi = dpi;
                state.refresh_environment()?;
            }

            // SAFETY: WM_DPICHANGED supplies a synchronous pointer to one suggested RECT that
            // remains valid for the duration of this callback.
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            let (x, y, width, height) = checked_window_rect(suggested)?;
            // SAFETY: The callback HWND is live, the rectangle was copied and checked above, and
            // these flags preserve both activation and Z order.
            unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )?
            };
            Ok(())
        })();
        if let Some(state) = preview_state(hwnd) {
            state.borrow_mut().last_paint_error = result.err().map(|error| error.code());
        }
        // SAFETY: Schedule one repaint after applying the new DPI and suggested physical bounds.
        let _ = unsafe { RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE) };
        return LRESULT(0);
    }

    if matches!(
        message,
        WM_DISPLAYCHANGE
            | WM_SETTINGCHANGE
            | WM_SYSCOLORCHANGE
            | WM_THEMECHANGED
            | SYSTEM_APPEARANCE_CHANGED_MESSAGE
    ) {
        if let Some(state) = preview_state(hwnd) {
            // SAFETY: The callback HWND is live for this synchronous DPI query.
            let dpi = unsafe { GetDpiForWindow(hwnd) };
            let result = if dpi == 0 {
                Err(Error::from_thread())
            } else {
                let mut state = state.borrow_mut();
                state.dpi = dpi;
                state.refresh_environment()
            };
            state.borrow_mut().last_paint_error = result.err().map(|error| error.code());
        }
        // SAFETY: This schedules a repaint of the live preview without forcing synchronous work
        // inside a broadcast settings callback.
        let _ = unsafe { RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE) };
    } else if message == WM_SIZE {
        if let Some(state) = preview_state(hwnd) {
            let mut state = state.borrow_mut();
            let result = state.prepare(hwnd);
            state.last_paint_error = result.err().map(|error| error.code());
        }
    } else if message == WM_PAINT {
        let _paint = PaintSession::begin(hwnd);
        if let Some(state) = preview_state(hwnd) {
            let mut state = state.borrow_mut();
            let result = state.render_with_recovery(hwnd);
            state.last_paint_error = result.err().map(|error| error.code());
        }
        return LRESULT(0);
    }

    // SAFETY: These are the untouched parameters supplied by Windows to this WNDPROC.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn checked_window_rect(rect: &RECT) -> Result<(i32, i32, i32, i32)> {
    let width = rect
        .right
        .checked_sub(rect.left)
        .filter(|width| *width > 0)
        .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .filter(|height| *height > 0)
        .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
    Ok((rect.left, rect.top, width, height))
}

fn rectangles_overlap(left: RECT, right: RECT) -> bool {
    left.left < left.right
        && left.top < left.bottom
        && right.left < right.right
        && right.top < right.bottom
        && left.left < right.right
        && right.left < left.right
        && left.top < right.bottom
        && right.top < left.bottom
}

fn preview_state(hwnd: HWND) -> Option<&'static RefCell<PreviewWindowState>> {
    // SAFETY: PreviewWindow stores a stable Box pointer at WM_NCCREATE and clears it before either
    // HWND or Box teardown. This helper is called only synchronously on the owning UI thread.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if pointer == 0 {
        None
    } else {
        // SAFETY: The nonzero value is exactly the stable Box pointer described above; access is
        // confined to the owning UI thread and ends before either the HWND or Box can be destroyed.
        Some(unsafe { &*(pointer as *const RefCell<PreviewWindowState>) })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContentLayouts, D2D_SIZE_U, MAX_Z_ORDER_SCAN, PreviewColors, PreviewWindow,
        PreviewWindowState, RetainedContent, TextContent, VIDEO_PREROLL_TIMER_ID,
        adjusted_window_pixel_size, checked_image_layout, checked_window_rect, client_pixel_size,
        color_from_colorref, format_file_size, image_client_pixel_size, image_destination_rect,
        image_preview_metadata, media_foundation_path, rectangles_overlap, system_dark_mode,
        system_message_font, text_preview_metadata,
    };
    use crate::{
        hover::PhysicalScreenPoint,
        platform::{ApartmentKind, ComApartment},
        preview::PreviewSize,
        settings::Theme,
        worker::{ImageFormat, ImagePreview, TextPreview, image_corpus_previews},
    };
    use cursorpeek_core::payload::VideoPreview;
    use std::{env, os::windows::ffi::OsStrExt, path::PathBuf, thread};
    use windows::Win32::{
        Foundation::{HWND, LPARAM, RECT, WPARAM},
        Graphics::DirectWrite::DWRITE_TEXT_METRICS,
        UI::WindowsAndMessaging::{
            GW_HWNDPREV, GetForegroundWindow, GetWindow, GetWindowRect, IsWindow, IsWindowVisible,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SendMessageW, SetWindowPos, WM_DPICHANGED,
            WM_TIMER,
        },
    };

    fn window_is_above(above: HWND, below: HWND) -> bool {
        // SAFETY: Both HWND values belong to live test windows while this helper is called.
        let mut candidate = unsafe { GetWindow(below, GW_HWNDPREV) }.ok();
        for _ in 0..MAX_Z_ORDER_SCAN {
            let Some(window) = candidate else {
                return false;
            };
            if window == above {
                return true;
            }
            // SAFETY: The candidate came from the preceding synchronous Z-order query and both
            // test windows remain live for the bounded traversal.
            candidate = unsafe { GetWindow(window, GW_HWNDPREV) }.ok();
        }
        false
    }

    fn text_preview() -> TextPreview {
        TextPreview {
            file_size: 12_800,
            last_write_time: 133_000_000_000_000_000,
            linked_content: true,
            encoding_was_guessed: true,
            truncated: true,
            display_name: "sample.txt".to_owned(),
            encoding: "windows-1252".to_owned(),
            text: "Hello, 世界\nPlain text only.".to_owned(),
        }
    }

    fn image_preview() -> ImagePreview {
        ImagePreview {
            file_size: 12_800,
            last_write_time: 133_000_000_000_000_000,
            linked_content: true,
            first_frame_only: true,
            display_name: "sample.png".to_owned(),
            format: ImageFormat::Png,
            source_width: 2,
            source_height: 1,
            width: 2,
            height: 1,
            premultiplied_bgra: vec![10, 20, 30, 40, 0, 0, 0, 0],
        }
    }

    #[test]
    fn preview_window_lifecycle_and_mouse_activation_policy_are_sound() {
        thread::spawn(|| {
            for _ in 0..100 {
                let preview =
                    PreviewWindow::create().expect("the preview window should be created");
                let handle = preview.handle();

                // SAFETY: handle belongs to the live preview on this test thread.
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

    #[test]
    fn topmost_repair_changes_relative_order_without_activation() {
        thread::spawn(|| {
            let preview = PreviewWindow::create().expect("the preview window should be created");
            let later_topmost =
                PreviewWindow::create().expect("the later topmost window should be created");
            preview
                .show_at(PhysicalScreenPoint::new(200, 200))
                .expect("the preview should be visible");
            later_topmost
                .show_at(PhysicalScreenPoint::new(220, 220))
                .expect("the competing topmost window should be visible");

            // SAFETY: Both topmost HWNDs are live on this test thread. This deliberately places
            // the preview immediately below the competing window without activating either one.
            unsafe {
                SetWindowPos(
                    preview.handle(),
                    Some(later_topmost.handle()),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                )
            }
            .expect("the test should establish the losing relative Z order");
            assert!(window_is_above(later_topmost.handle(), preview.handle()));

            // SAFETY: GetForegroundWindow performs a synchronous query with no ownership
            // transfer. This short test makes no request that could change the foreground window.
            let foreground_before = unsafe { GetForegroundWindow() };
            preview
                .raise_topmost_without_activation()
                .expect("the preview should return to the top of the topmost band");
            assert!(window_is_above(preview.handle(), later_topmost.handle()));
            // SAFETY: GetForegroundWindow performs a synchronous query with no ownership transfer.
            assert_eq!(unsafe { GetForegroundWindow() }, foreground_before);
        })
        .join()
        .expect("the topmost-repair test thread should not panic");
    }

    #[test]
    fn overlap_detection_uses_nonempty_half_open_rectangles() {
        let preview = RECT {
            left: 100,
            top: 100,
            right: 300,
            bottom: 250,
        };
        assert!(rectangles_overlap(
            preview,
            RECT {
                left: 250,
                top: 200,
                right: 350,
                bottom: 300,
            }
        ));
        assert!(!rectangles_overlap(
            preview,
            RECT {
                left: 300,
                top: 100,
                right: 400,
                bottom: 200,
            }
        ));
        assert!(!rectangles_overlap(
            preview,
            RECT {
                left: 150,
                top: 150,
                right: 150,
                bottom: 200,
            }
        ));
    }

    #[test]
    fn explicit_palettes_are_distinct_and_high_contrast_uses_system_colors() {
        let light = PreviewColors::resolve_for_environment(Theme::Light, false, Some(true));
        let dark = PreviewColors::resolve_for_environment(Theme::Dark, false, Some(false));
        assert!(light.background.r > dark.background.r);
        assert!(light.body.r < dark.body.r);

        let system = PreviewColors::system();
        let overridden = PreviewColors::resolve_for_environment(Theme::Dark, true, Some(true));
        assert_eq!(overridden.background, system.background);
        assert_eq!(overridden.body, system.body);
        assert_eq!(overridden.metadata, system.metadata);

        let system_dark = PreviewColors::resolve_for_environment(Theme::System, false, Some(true));
        let system_light =
            PreviewColors::resolve_for_environment(Theme::System, false, Some(false));
        assert_eq!(system_dark.background, dark.background);
        assert_eq!(system_light.background, light.background);
    }

    #[test]
    fn documented_system_theme_source_is_available_in_a_runtime_apartment() {
        thread::spawn(|| {
            let _apartment = ComApartment::initialize(ApartmentKind::SingleThreaded)
                .expect("the Windows Runtime apartment should initialize");
            assert!(
                system_dark_mode().is_some(),
                "UISettings should expose the current foreground mode"
            );
            let preview = PreviewWindow::create_with_theme(Theme::System)
                .expect("the system-themed preview should be created");
            assert!(
                preview.theme_observer.is_some(),
                "the system appearance observer should subscribe while the preview is alive"
            );
        })
        .join()
        .expect("the system-theme query thread should not panic");
    }

    #[test]
    fn system_message_font_is_terminated_and_dpi_bounded() {
        for dpi in [96, 120, 144, 168, 192] {
            let font = system_message_font(dpi);
            assert_eq!(font.family.last(), Some(&0));
            assert!(!font.family[..font.family.len() - 1].contains(&0));
            assert!((9.0..=32.0).contains(&font.size));
        }
    }

    #[test]
    fn suggested_dpi_rectangles_require_positive_checked_dimensions() {
        assert_eq!(
            checked_window_rect(&RECT {
                left: -320,
                top: 40,
                right: 320,
                bottom: 520,
            })
            .expect("the physical rectangle should be valid"),
            (-320, 40, 640, 480)
        );
        assert!(
            checked_window_rect(&RECT {
                left: 10,
                top: 10,
                right: 10,
                bottom: 20,
            })
            .is_err()
        );
        assert!(
            checked_window_rect(&RECT {
                left: i32::MIN,
                top: 0,
                right: i32::MAX,
                bottom: 1,
            })
            .is_err()
        );
    }

    #[test]
    fn dpi_change_applies_suggested_bounds_and_rebuilds_visible_resources() {
        thread::spawn(|| {
            let preview = PreviewWindow::create().expect("the preview window should be created");
            preview
                .show_text_at(
                    PhysicalScreenPoint::new(200, 200),
                    PreviewSize::new(640, 480),
                    &text_preview(),
                )
                .expect("the initial preview should render");

            let suggested = RECT {
                left: 50,
                top: 60,
                right: 370,
                bottom: 300,
            };
            let packed_dpi = usize::from(144_u16) | (usize::from(144_u16) << 16);
            // SAFETY: SendMessageW consumes the pointer synchronously, and suggested stays alive
            // for the complete WM_DPICHANGED callback.
            unsafe {
                SendMessageW(
                    preview.handle(),
                    WM_DPICHANGED,
                    Some(WPARAM(packed_dpi)),
                    Some(LPARAM(std::ptr::from_ref(&suggested) as isize)),
                );
            }
            preview
                .redraw()
                .expect("the DPI-refreshed preview should repaint");

            let mut actual = RECT::default();
            // SAFETY: The preview HWND remains live and actual is writable output storage.
            unsafe { GetWindowRect(preview.handle(), &mut actual) }
                .expect("the physical window rectangle should be queryable");
            assert_eq!(actual, suggested);
            assert!(preview.has_content_layouts());
            assert!(preview.has_device_resources());
        })
        .join()
        .expect("the DPI-change render test thread should not panic");
    }

    #[test]
    fn changing_theme_recreates_visible_device_resources() {
        thread::spawn(|| {
            let preview = PreviewWindow::create_with_theme(Theme::Dark)
                .expect("the dark preview window should be created");
            preview
                .show_text_at(
                    PhysicalScreenPoint::new(200, 200),
                    PreviewSize::new(640, 480),
                    &text_preview(),
                )
                .expect("the dark preview should render");
            assert!(preview.has_device_resources());

            preview
                .set_theme(Theme::Light)
                .expect("the visible preview should refresh in light mode");
            assert!(preview.has_content_layouts());
            assert!(preview.has_device_resources());
        })
        .join()
        .expect("the theme-refresh test thread should not panic");
    }

    #[test]
    fn text_layout_renders_and_recovers_device_resources() {
        thread::spawn(|| {
            let preview = PreviewWindow::create().expect("the preview window should be created");
            let maximum = PreviewSize::new(640, 480);
            let placement = preview
                .show_text_at(PhysicalScreenPoint::new(200, 200), maximum, &text_preview())
                .expect("bounded text should render");
            let dpi = preview
                .window_dpi()
                .expect("the shown window should expose its monitor DPI");
            let expected_client = preview
                .state
                .borrow()
                .content_client_pixel_size(maximum)
                .expect("retained text should produce a measured client size");
            assert_eq!(
                client_pixel_size(preview.handle())
                    .expect("the live window client rectangle should be queryable"),
                expected_client
            );
            let expected_window = adjusted_window_pixel_size(expected_client, dpi)
                .expect("the measured text client should produce a window rectangle");
            assert_eq!(
                placement.width,
                i32::try_from(expected_window.width).unwrap()
            );
            assert_eq!(
                placement.height,
                i32::try_from(expected_window.height).unwrap()
            );
            assert!(preview.is_visible());
            assert_eq!(preview.accessible_title(), "CursorPeek text preview");
            assert!(preview.has_content_layouts());
            assert!(preview.has_device_resources());

            preview
                .force_device_loss_and_redraw()
                .expect("a lost target should be discarded and recreated once");
            assert!(preview.has_device_resources());
        })
        .join()
        .expect("the render test thread should not panic");
    }

    #[test]
    fn image_bitmap_renders_and_recovers_device_resources() {
        thread::spawn(|| {
            let preview = PreviewWindow::create().expect("the preview window should be created");
            let content = image_preview();
            let placement = preview
                .show_image_at(
                    PhysicalScreenPoint::new(200, 200),
                    PreviewSize::new(640, 480),
                    content.clone(),
                )
                .expect("bounded image pixels should render");
            let dpi = preview
                .window_dpi()
                .expect("the shown window should expose its monitor DPI");
            let expected_client =
                image_client_pixel_size(&content, PreviewSize::new(640, 480), dpi)
                    .expect("the valid image should produce a bounded natural client size");
            assert_eq!(
                client_pixel_size(preview.handle())
                    .expect("the live window client rectangle should be queryable"),
                expected_client
            );
            let expected_window = adjusted_window_pixel_size(expected_client, dpi)
                .expect("the natural client should produce a window rectangle");
            assert_eq!(
                placement.width,
                i32::try_from(expected_window.width).unwrap()
            );
            assert_eq!(
                placement.height,
                i32::try_from(expected_window.height).unwrap()
            );
            assert!(preview.is_visible());
            assert_eq!(preview.accessible_title(), "CursorPeek image preview");
            assert!(preview.has_content_layouts());
            assert!(preview.has_image_bitmap());
            assert!(preview.has_device_resources());

            preview
                .force_device_loss_and_redraw()
                .expect("a lost target and bitmap should be recreated once");
            assert!(preview.has_image_bitmap());
            assert!(preview.has_device_resources());
        })
        .join()
        .expect("the image-render test thread should not panic");
    }

    #[test]
    fn generated_image_corpus_renders_and_recovers_device_resources() {
        thread::spawn(|| {
            let preview = PreviewWindow::create().expect("the preview window should be created");
            for case in image_corpus_previews() {
                preview
                    .show_image_at(
                        PhysicalScreenPoint::new(200, 200),
                        PreviewSize::new(640, 480),
                        case.preview,
                    )
                    .unwrap_or_else(|error| {
                        panic!("image corpus case `{}` should render: {error}", case.id)
                    });
                assert!(preview.has_content_layouts(), "corpus case `{}`", case.id);
                assert!(preview.has_image_bitmap(), "corpus case `{}`", case.id);
                preview
                    .force_device_loss_and_redraw()
                    .unwrap_or_else(|error| {
                        panic!(
                            "image corpus case `{}` should recover device loss: {error}",
                            case.id
                        )
                    });
                assert!(preview.has_image_bitmap(), "corpus case `{}`", case.id);
                assert!(preview.has_device_resources(), "corpus case `{}`", case.id);
            }
        })
        .join()
        .expect("the image-corpus render thread should not panic");
    }

    #[test]
    fn text_window_tracks_rendered_content_until_the_selected_maximum() {
        thread::spawn(|| {
            let maximum = PreviewSize::new(640, 480);
            let measure = |preview: &TextPreview, dpi: u32| {
                let mut state = PreviewWindowState::new(Theme::System)
                    .expect("DirectWrite factories should initialize");
                state
                    .configure(
                        Some(RetainedContent::Text(TextContent::from_preview(preview))),
                        dpi,
                    )
                    .expect("system fonts should refresh for the target DPI");
                state
                    .content_client_pixel_size(maximum)
                    .expect("bounded text should produce a measured client size")
            };

            let mut empty = text_preview();
            empty.text.clear();
            let empty_size = measure(&empty, 96);

            let mut short = text_preview();
            short.text = "A short line.".to_owned();
            let short_size = measure(&short, 96);

            let mut multiline = text_preview();
            multiline.text = "alpha\nβeta\n中文\nemoji 👩🏽‍💻".to_owned();
            let multiline_size = measure(&multiline, 96);

            let mut long = text_preview();
            long.text = (0..160)
                .map(|_| "W".repeat(180))
                .collect::<Vec<_>>()
                .join("\n");
            let long_size = measure(&long, 96);

            assert!(empty_size.width < maximum.width());
            assert!(empty_size.height < short_size.height);
            assert!(short_size.width < maximum.width());
            assert!(short_size.height < maximum.height());
            assert!(multiline_size.height > short_size.height);
            assert!(multiline_size.width <= maximum.width());
            assert_eq!(long_size.width, maximum.width());
            assert_eq!(long_size.height, maximum.height());

            let short_at_200 = measure(&short, 192);
            assert!(short_at_200.width.abs_diff(short_size.width * 2) <= 2);
            assert!(short_at_200.height.abs_diff(short_size.height * 2) <= 2);
        })
        .join()
        .expect("the adaptive text sizing test thread should not panic");
    }

    #[test]
    fn multilingual_layout_keeps_logical_bounds_from_100_to_200_percent_dpi() {
        thread::spawn(|| {
            let preview = TextPreview {
                file_size: 4_096,
                last_write_time: 133_000_000_000_000_000,
                linked_content: false,
                encoding_was_guessed: false,
                truncated: false,
                display_name: "multilingual.txt".to_owned(),
                encoding: "UTF-8".to_owned(),
                text: concat!(
                    "Latin café · Ελληνικά · Русский\n",
                    "العربية · עברית · हिन्दी · ไทย\n",
                    "中文 · 日本語 · 한국어 · 👩🏽‍💻 · e\u{301}"
                )
                .to_owned(),
            };

            for dpi in [96, 120, 144, 168, 192] {
                let mut state = PreviewWindowState::new(Theme::System)
                    .expect("DirectWrite factories should initialize");
                state
                    .configure(
                        Some(RetainedContent::Text(TextContent::from_preview(&preview))),
                        dpi,
                    )
                    .expect("system fonts should refresh for the target DPI");
                state.pixel_size = D2D_SIZE_U {
                    width: 640 * dpi / 96,
                    height: 480 * dpi / 96,
                };
                let layouts = state
                    .create_layouts()
                    .expect("the multilingual text should produce layouts");
                let ContentLayouts::Text { body, .. } = layouts else {
                    panic!("text content should create text layouts");
                };
                let body = body.expect("the corpus body is not empty");
                let mut metrics = DWRITE_TEXT_METRICS::default();
                // SAFETY: metrics is writable storage and body is a live retained layout.
                unsafe {
                    body.GetMetrics(&mut metrics)
                        .expect("DirectWrite should return layout metrics");
                }

                assert!((metrics.layoutWidth - 632.0).abs() < f32::EPSILON);
                assert!((metrics.layoutHeight - 412.0).abs() < f32::EPSILON);
                assert!(metrics.lineCount >= 3);
            }
        })
        .join()
        .expect("the high-DPI layout test thread should not panic");
    }

    #[test]
    fn text_metadata_and_size_labels_are_bounded_and_explicit() {
        let metadata = text_preview_metadata(&text_preview());
        let lines: Vec<_> = metadata.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "sample.txt");
        assert_eq!(
            lines[1],
            "12.5 KiB    (windows-1252)  ·  guessed  ·  truncated  ·  linked"
        );
        assert!(!lines[2].is_empty());
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(1023), "1023 B");
        assert_eq!(format_file_size(1024), "1.0 KiB");
        assert_eq!(format_file_size(1024 * 1024), "1.0 MiB");
        assert_eq!(format_file_size(1024 * 1024 * 1024), "1.0 GiB");
    }

    #[test]
    fn image_metadata_and_renderer_boundary_are_explicit() {
        let preview = image_preview();
        let metadata = image_preview_metadata(&preview);
        let lines: Vec<_> = metadata.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "sample.png");
        assert_eq!(lines[1], "12.5 KiB    (2 × 1)  ·  first frame  ·  linked");
        assert!(!lines[2].is_empty());
        assert_eq!(checked_image_layout(&preview), Some((8, 8)));

        let mut wrong_length = preview.clone();
        wrong_length.premultiplied_bgra.pop();
        assert_eq!(checked_image_layout(&wrong_length), None);

        let mut straight_alpha = preview;
        straight_alpha.premultiplied_bgra[2] = 41;
        assert_eq!(checked_image_layout(&straight_alpha), None);
    }

    #[test]
    fn image_window_uses_natural_pixels_until_the_selected_maximum() {
        let preview = |width: u32, height: u32| ImagePreview {
            file_size: 1,
            last_write_time: 133_000_000_000_000_000,
            linked_content: false,
            first_frame_only: false,
            display_name: "sample.png".to_owned(),
            format: ImageFormat::Png,
            source_width: width,
            source_height: height,
            width,
            height,
            premultiplied_bgra: vec![0; width as usize * height as usize * 4],
        };
        let maximum = PreviewSize::new(640, 480);

        assert_eq!(
            image_client_pixel_size(&preview(480, 300), maximum, 96),
            Some(D2D_SIZE_U {
                width: 488,
                height: 364,
            })
        );
        assert_eq!(
            image_client_pixel_size(&preview(960, 540), maximum, 96),
            Some(D2D_SIZE_U {
                width: 640,
                height: 420,
            })
        );
        assert_eq!(
            image_client_pixel_size(&preview(540, 960), maximum, 96),
            Some(D2D_SIZE_U {
                width: 242,
                height: 480,
            })
        );
        assert_eq!(
            image_client_pixel_size(&preview(480, 300), maximum, 192),
            Some(D2D_SIZE_U {
                width: 496,
                height: 428,
            })
        );
        assert_eq!(
            image_client_pixel_size(&preview(2, 1), maximum, 96),
            Some(D2D_SIZE_U {
                width: 166,
                height: 65,
            })
        );
    }

    #[test]
    fn image_destination_preserves_aspect_ratio_without_upscaling() {
        let preview = ImagePreview {
            file_size: 1,
            last_write_time: 133_000_000_000_000_000,
            linked_content: false,
            first_frame_only: false,
            display_name: "wallpaper.jpg".to_owned(),
            format: ImageFormat::Jpeg,
            source_width: 960,
            source_height: 540,
            width: 960,
            height: 540,
            premultiplied_bgra: vec![0; 960 * 540 * 4],
        };

        let at_100 = image_destination_rect(
            &preview,
            D2D_SIZE_U {
                width: 640,
                height: 480,
            },
            96,
        )
        .expect("the image should fit at 100 percent");
        assert!((at_100.left - 4.0).abs() < 0.01);
        assert!((at_100.top - 4.0).abs() < 0.01);
        assert!((at_100.right - 636.0).abs() < 0.01);
        assert!((at_100.bottom - 359.5).abs() < 0.01);

        let at_200 = image_destination_rect(
            &preview,
            D2D_SIZE_U {
                width: 1_280,
                height: 960,
            },
            192,
        )
        .expect("the image should fit at 200 percent");
        assert!((at_200.left - 80.0).abs() < 0.01);
        assert!((at_200.top - 4.0).abs() < 0.01);
        assert!((at_200.right - 560.0).abs() < 0.01);
        assert!((at_200.bottom - 274.0).abs() < 0.01);
    }

    #[test]
    fn colorref_conversion_preserves_windows_channel_order() {
        let color = color_from_colorref(0x00_33_22_11);
        assert!((color.r - 0x11 as f32 / 255.0).abs() < f32::EPSILON);
        assert!((color.g - 0x22 as f32 / 255.0).abs() < f32::EPSILON);
        assert!((color.b - 0x33 as f32 / 255.0).abs() < f32::EPSILON);
        assert_eq!(color.a, 1.0);
    }

    #[test]
    #[ignore = "requires CURSORPEEK_TEST_MP4 pointing to a local playable MP4"]
    fn native_mp4_player_starts_silent_and_stops_with_the_preview() {
        let path = PathBuf::from(
            env::var_os("CURSORPEEK_TEST_MP4")
                .expect("CURSORPEEK_TEST_MP4 must point to a local playable MP4"),
        );
        let metadata = path.metadata().expect("the MP4 fixture should be readable");
        let _apartment = ComApartment::initialize(ApartmentKind::SingleThreaded)
            .expect("the test thread should initialize COM");
        let window = PreviewWindow::create().expect("the preview window should be created");
        let mut worker_path: Vec<u16> = r"\\?\".encode_utf16().collect();
        worker_path.extend(path.as_os_str().encode_wide());
        let preview = VideoPreview {
            file_size: metadata.len(),
            last_write_time: 0,
            linked_content: false,
            display_name: "native-player-test.mp4".to_owned(),
            path: worker_path,
        };
        window
            .show_video_at(
                PhysicalScreenPoint::new(100, 100),
                PreviewSize::new(480, 360),
                preview,
                false,
                true,
            )
            .expect("MFPlay should accept and start the local MP4");
        // SAFETY: The preview HWND is live and should remain hidden during preroll.
        assert!(!unsafe { IsWindowVisible(window.handle()).as_bool() });
        // SAFETY: Deliver the private timer message synchronously to exercise preroll completion.
        unsafe {
            SendMessageW(
                window.handle(),
                WM_TIMER,
                Some(WPARAM(VIDEO_PREROLL_TIMER_ID)),
                Some(LPARAM(0)),
            )
        };
        // SAFETY: The same preview HWND remains live after timer dispatch.
        assert!(unsafe { IsWindowVisible(window.handle()).as_bool() });
        let video_surface = window
            .video_player
            .borrow()
            .as_ref()
            .expect("the player should be retained while visible")
            .video_window
            .hwnd;
        window.hide().expect("hiding should stop native playback");
        // SAFETY: The captured HWND belonged to the video child and should now be stale.
        assert!(!unsafe { IsWindow(Some(video_surface)).as_bool() });
        window
            .show_image_at(
                PhysicalScreenPoint::new(100, 100),
                PreviewSize::new(480, 360),
                image_preview(),
            )
            .expect("an image should render after the video surface is destroyed");
        window.hide().expect("the image preview should hide");
        let immediate_preview = VideoPreview {
            file_size: metadata.len(),
            last_write_time: 0,
            linked_content: false,
            display_name: "immediate-player-test.mp4".to_owned(),
            path: {
                let mut value: Vec<u16> = r"\\?\".encode_utf16().collect();
                value.extend(path.as_os_str().encode_wide());
                value
            },
        };
        window
            .show_video_at(
                PhysicalScreenPoint::new(100, 100),
                PreviewSize::new(480, 360),
                immediate_preview,
                false,
                false,
            )
            .expect("immediate mode should start the same MP4");
        // SAFETY: Immediate mode reveals the live preview before returning.
        assert!(unsafe { IsWindowVisible(window.handle()).as_bool() });
        window.hide().expect("the immediate preview should hide");
    }

    #[test]
    fn media_foundation_receives_an_ordinary_absolute_dos_path() {
        let converted = media_foundation_path(
            &r"\\?\C:\Video\sample.mp4"
                .encode_utf16()
                .collect::<Vec<_>>(),
        )
        .expect("the worker's canonical drive path should convert");
        assert_eq!(
            String::from_utf16(&converted).unwrap(),
            "C:\\Video\\sample.mp4\0"
        );
        assert!(
            media_foundation_path(&r"\\server\share\a.mp4".encode_utf16().collect::<Vec<_>>())
                .is_err()
        );
    }
}
