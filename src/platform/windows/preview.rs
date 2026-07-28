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
use cursorpeek_core::layout::fit_dimensions;

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
                DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_LEADING,
                DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory,
                IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{
                BeginPaint, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, EndPaint,
                GetMonitorInfoW, GetSysColor, MONITOR_DEFAULTTONEAREST, MONITORINFO,
                MonitorFromPoint, PAINTSTRUCT, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW,
                RedrawWindow,
            },
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTimeEx},
        },
        UI::{
            Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi},
            WindowsAndMessaging::{
                CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
                GetClientRect, GetWindowLongPtrW, HWND_TOPMOST, MA_NOACTIVATEANDEAT,
                NONCLIENTMETRICSW, PostMessageW, RegisterClassW, SPI_GETHIGHCONTRAST,
                SPI_GETNONCLIENTMETRICS, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
                SWP_NOZORDER, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageW,
                SetWindowLongPtrW, SetWindowPos, SetWindowTextW, SystemParametersInfoW,
                UnregisterClassW, WINDOW_EX_STYLE, WM_APP, WM_DISPLAYCHANGE, WM_DPICHANGED,
                WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
                WM_SETTINGCHANGE, WM_SIZE, WM_SYSCOLORCHANGE, WM_THEMECHANGED, WNDCLASSW,
                WS_BORDER, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{Error, HRESULT, PCWSTR, Result, w},
};
use windows_numerics::Vector2;

const CLASS_NAME_PREFIX: &str = "CursorPeek.PreviewWindow";
static NEXT_CLASS_ID: AtomicU64 = AtomicU64::new(1);
const BASE_DPI: f32 = 96.0;
const CONTENT_MARGIN: f32 = 12.0;
const HEADER_HEIGHT: f32 = 18.0;
const HEADER_BODY_GAP: f32 = 8.0;
const IMAGE_MARGIN: f32 = 4.0;
const IMAGE_METADATA_HEIGHT: f32 = 56.0;
const IMAGE_MINIMUM_WIDTH: f32 = 158.0;
const SYSTEM_APPEARANCE_CHANGED_MESSAGE: u32 = WM_APP + 20;

pub(crate) struct PreviewWindow {
    hwnd: HWND,
    state: Box<RefCell<PreviewWindowState>>,
    theme_observer: Option<ThemeObserver>,
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
        self.show(
            anchor,
            size,
            Some(RetainedContent::Image(ImageContent::from_preview(preview))),
        )
    }

    fn show(
        &self,
        anchor: PhysicalScreenPoint,
        size: PreviewSize,
        content: Option<RetainedContent>,
    ) -> Result<PreviewPlacement> {
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
        let placement = match content.as_ref() {
            Some(RetainedContent::Image(content)) => {
                let client_size = image_client_pixel_size(&content.preview, size, dpi)
                    .ok_or_else(Error::from_thread)?;
                let window_size = adjusted_window_pixel_size(client_size, dpi)?;
                place_preview_pixels(
                    anchor,
                    work_area,
                    dpi,
                    window_size.width,
                    window_size.height,
                )
            }
            Some(RetainedContent::Text(_)) | None => place_preview(anchor, work_area, dpi, size),
        }
        .ok_or_else(Error::from_thread)?;

        let title = content
            .as_ref()
            .map_or(w!("CursorPeek preview"), RetainedContent::accessible_title);
        // SAFETY: The live preview HWND and selected static terminated title remain valid.
        unsafe { SetWindowTextW(self.hwnd, title)? };
        self.state.borrow_mut().configure(content, dpi)?;

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
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
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
        // SAFETY: The handle is owned and live for self.
        unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(self.hwnd).as_bool() }
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
                let layout_width = (width - CONTENT_MARGIN * 2.0).max(1.0);
                let body_origin_y = CONTENT_MARGIN + HEADER_HEIGHT + HEADER_BODY_GAP;
                let body_height = (height - body_origin_y - CONTENT_MARGIN).max(1.0);

                // SAFETY: The bounded UTF-16 buffers remain alive in content for the complete
                // calls. Formats and the factory are retained COM resources on this thread.
                let header = unsafe {
                    self.dwrite_factory.CreateTextLayout(
                        &content.header,
                        &self.formats.header,
                        layout_width,
                        HEADER_HEIGHT,
                    )?
                };
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
                Ok(ContentLayouts::Text {
                    header,
                    body,
                    body_origin_y,
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
                        &self.formats.image_metadata,
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
                        header,
                        body,
                        body_origin_y,
                    } => {
                        device.target.DrawTextLayout(
                            Vector2 {
                                X: CONTENT_MARGIN,
                                Y: CONTENT_MARGIN,
                            },
                            header,
                            &device.metadata_brush,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        );
                        if let Some(body) = body.as_ref() {
                            device.target.DrawTextLayout(
                                Vector2 {
                                    X: CONTENT_MARGIN,
                                    Y: *body_origin_y,
                                },
                                body,
                                &device.body_brush,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            );
                        }
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
    header: Vec<u16>,
    body: Vec<u16>,
}

impl TextContent {
    fn from_preview(preview: &TextPreview) -> Self {
        Self {
            header: preview_header(preview).encode_utf16().collect(),
            body: preview.text.encode_utf16().collect(),
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
        header: IDWriteTextLayout,
        body: Option<IDWriteTextLayout>,
        body_origin_y: f32,
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
    header: IDWriteTextFormat,
    body: IDWriteTextFormat,
    image_metadata: IDWriteTextFormat,
}

impl TextFormats {
    fn create(factory: &IDWriteFactory, dpi: u32) -> Result<Self> {
        let font = system_message_font(dpi);
        let family = PCWSTR(font.family.as_ptr());

        // SAFETY: The terminated family buffer remains live for both synchronous calls. None
        // selects the system font collection, and DirectWrite retains its own format state.
        let header = unsafe {
            factory.CreateTextFormat(
                family,
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                font.size,
                w!("en-US"),
            )?
        };
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
        let image_metadata = unsafe {
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
            header.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            header.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            header.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            body.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            body.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            body.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            image_metadata.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            image_metadata.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            image_metadata.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        }
        Ok(Self {
            header,
            body,
            image_metadata,
        })
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

fn preview_header(preview: &TextPreview) -> String {
    let mut header = format!(
        "{}  \u{b7}  {}",
        preview.encoding,
        format_file_size(preview.file_size)
    );
    if preview.encoding_was_guessed {
        header.push_str("  \u{b7}  guessed");
    }
    if preview.truncated {
        header.push_str("  \u{b7}  truncated");
    }
    if preview.linked_content {
        header.push_str("  \u{b7}  linked");
    }
    header
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
        ContentLayouts, D2D_SIZE_U, PreviewColors, PreviewWindow, PreviewWindowState,
        RetainedContent, TextContent, adjusted_window_pixel_size, checked_image_layout,
        checked_window_rect, client_pixel_size, color_from_colorref, format_file_size,
        image_client_pixel_size, image_destination_rect, image_preview_metadata, preview_header,
        system_dark_mode, system_message_font,
    };
    use crate::{
        hover::PhysicalScreenPoint,
        platform::{ApartmentKind, ComApartment},
        preview::PreviewSize,
        settings::Theme,
        worker::{ImageFormat, ImagePreview, TextPreview, image_corpus_previews},
    };
    use std::thread;
    use windows::Win32::{
        Foundation::{LPARAM, RECT, WPARAM},
        Graphics::DirectWrite::DWRITE_TEXT_METRICS,
        UI::WindowsAndMessaging::{GetWindowRect, IsWindow, SendMessageW, WM_DPICHANGED},
    };

    fn text_preview() -> TextPreview {
        TextPreview {
            file_size: 12_800,
            linked_content: true,
            encoding_was_guessed: true,
            truncated: true,
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
            preview
                .show_text_at(
                    PhysicalScreenPoint::new(200, 200),
                    PreviewSize::new(640, 480),
                    &text_preview(),
                )
                .expect("bounded text should render");
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
    fn multilingual_layout_keeps_logical_bounds_from_100_to_200_percent_dpi() {
        thread::spawn(|| {
            let preview = TextPreview {
                file_size: 4_096,
                linked_content: false,
                encoding_was_guessed: false,
                truncated: false,
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

                assert!((metrics.layoutWidth - 616.0).abs() < f32::EPSILON);
                assert!((metrics.layoutHeight - 430.0).abs() < f32::EPSILON);
                assert!(metrics.lineCount >= 3);
            }
        })
        .join()
        .expect("the high-DPI layout test thread should not panic");
    }

    #[test]
    fn metadata_header_and_size_labels_are_bounded_and_explicit() {
        assert_eq!(
            preview_header(&text_preview()),
            "windows-1252  ·  12.5 KiB  ·  guessed  ·  truncated  ·  linked"
        );
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
}
