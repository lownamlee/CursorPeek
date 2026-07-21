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
    preview::{PreviewPlacement, PreviewSize, ScreenRect, place_preview},
    worker::{ImageFormat, ImagePreview, TextPreview},
};

use windows::{
    Win32::{
        Foundation::{
            D2DERR_RECREATE_TARGET, E_INVALIDARG, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
        },
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
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            WindowsAndMessaging::{
                CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
                GetClientRect, GetWindowLongPtrW, HWND_TOPMOST, MA_NOACTIVATEANDEAT,
                RegisterClassW, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
                SWP_NOZORDER, SWP_SHOWWINDOW, SendMessageW, SetWindowLongPtrW, SetWindowPos,
                UnregisterClassW, WINDOW_EX_STYLE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCCREATE,
                WM_NCDESTROY, WM_PAINT, WM_SIZE, WNDCLASSW, WS_BORDER, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
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

pub(crate) struct PreviewWindow {
    hwnd: HWND,
    state: Box<RefCell<PreviewWindowState>>,
    _class: RegisteredPreviewClass,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl PreviewWindow {
    pub(crate) fn create() -> Result<Self> {
        let class = RegisteredPreviewClass::register()?;
        let class_name = class.name();
        let state = Box::new(RefCell::new(PreviewWindowState::new()?));
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

        Ok(Self {
            hwnd,
            state,
            _class: class,
            _thread_affinity: PhantomData,
        })
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

        // SAFETY: The HWND remains live after the successful positioning call.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 {
            return Err(Error::from_thread());
        }

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

        let placement = place_preview(
            anchor,
            ScreenRect {
                left: monitor_info.rcWork.left,
                top: monitor_info.rcWork.top,
                right: monitor_info.rcWork.right,
                bottom: monitor_info.rcWork.bottom,
            },
            dpi,
            size,
        )
        .ok_or_else(Error::from_thread)?;

        {
            let mut state = self.state.borrow_mut();
            state.configure(content, dpi);
        }

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
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        let _ = self.hide();

        // SAFETY: Clearing GWLP_USERDATA prevents teardown callbacks from reaching the boxed state
        // through an alias to this exclusive Drop borrow. The !Send owner destroys its HWND on the
        // creating UI thread before the state and registered class are released.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct PreviewWindowState {
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    header_format: IDWriteTextFormat,
    body_format: IDWriteTextFormat,
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
    fn new() -> Result<Self> {
        // SAFETY: Both factory calls write a fresh COM interface. The Direct2D factory is used only
        // on this UI thread; the recommended shared DirectWrite factory is retained with COM
        // reference counting.
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }?;
        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }?;

        // SAFETY: Static family and locale strings remain valid for each call. None selects the
        // system font collection, and DirectWrite retains its own format state.
        let header_format = unsafe {
            dwrite_factory.CreateTextFormat(
                w!("Segoe UI"),
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.0,
                w!("en-US"),
            )?
        };
        let body_format = unsafe {
            dwrite_factory.CreateTextFormat(
                w!("Consolas"),
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                13.0,
                w!("en-US"),
            )?
        };
        // SAFETY: These calls mutate only newly created device-independent formats owned here.
        unsafe {
            header_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            header_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            header_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            body_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            body_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            body_format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        }

        Ok(Self {
            d2d_factory,
            dwrite_factory,
            header_format,
            body_format,
            colors: PreviewColors::system(),
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

    fn configure(&mut self, content: Option<RetainedContent>, dpi: u32) {
        if self.dpi != dpi {
            self.dpi = dpi;
            self.discard_device_resources();
        }
        self.content = content;
        self.layouts = None;
        self.image_bitmap = None;
        self.last_paint_error = None;
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
        let layout_width = (width - CONTENT_MARGIN * 2.0).max(1.0);
        let body_origin_y = CONTENT_MARGIN + HEADER_HEIGHT + HEADER_BODY_GAP;
        let body_height = (height - body_origin_y - CONTENT_MARGIN).max(1.0);

        // SAFETY: The bounded UTF-16 buffers remain alive in content for the complete calls.
        // Formats and the factory are retained COM resources on this thread.
        let header = unsafe {
            self.dwrite_factory.CreateTextLayout(
                content.header(),
                &self.header_format,
                layout_width,
                HEADER_HEIGHT,
            )?
        };
        let body = match content {
            RetainedContent::Text(content) if !content.body.is_empty() => Some(unsafe {
                self.dwrite_factory.CreateTextLayout(
                    &content.body,
                    &self.body_format,
                    layout_width,
                    body_height,
                )?
            }),
            RetainedContent::Text(_) | RetainedContent::Image(_) => None,
        };
        Ok(ContentLayouts {
            header,
            body,
            body_origin_y,
        })
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
            if let Some(layouts) = self.layouts.as_ref() {
                device.target.DrawTextLayout(
                    Vector2 {
                        X: CONTENT_MARGIN,
                        Y: CONTENT_MARGIN,
                    },
                    &layouts.header,
                    &device.metadata_brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                );
                if let Some(body) = layouts.body.as_ref() {
                    device.target.DrawTextLayout(
                        Vector2 {
                            X: CONTENT_MARGIN,
                            Y: layouts.body_origin_y,
                        },
                        body,
                        &device.body_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    );
                }
            }
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
        let body_brush = unsafe { target.CreateSolidColorBrush(&self.colors.body, None) }?;
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
    fn header(&self) -> &[u16] {
        match self {
            Self::Text(content) => &content.header,
            Self::Image(content) => &content.header,
        }
    }
}

struct ImageContent {
    header: Vec<u16>,
    preview: ImagePreview,
}

impl ImageContent {
    fn from_preview(preview: ImagePreview) -> Self {
        Self {
            header: image_preview_header(&preview).encode_utf16().collect(),
            preview,
        }
    }
}

struct ContentLayouts {
    header: IDWriteTextLayout,
    body: Option<IDWriteTextLayout>,
    body_origin_y: f32,
}

struct DeviceResources {
    target: ID2D1HwndRenderTarget,
    body_brush: ID2D1SolidColorBrush,
    metadata_brush: ID2D1SolidColorBrush,
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

fn image_preview_header(preview: &ImagePreview) -> String {
    let mut header = format!(
        "{}  \u{b7}  {} \u{d7} {}  \u{b7}  {}",
        image_format_label(preview.format),
        preview.source_width,
        preview.source_height,
        format_file_size(preview.file_size)
    );
    if preview.first_frame_only {
        header.push_str("  \u{b7}  first frame");
    }
    if preview.linked_content {
        header.push_str("  \u{b7}  linked");
    }
    header
}

const fn image_format_label(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Png => "PNG",
        ImageFormat::Gif => "GIF",
        ImageFormat::WebP => "WebP",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Ico => "ICO",
        ImageFormat::Tiff => "TIFF",
    }
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
    let body_origin_y = CONTENT_MARGIN + HEADER_HEIGHT + HEADER_BODY_GAP;
    let available_width = client_width - CONTENT_MARGIN * 2.0;
    let available_height = client_height - body_origin_y - CONTENT_MARGIN;
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
    let left = CONTENT_MARGIN + (available_width - rendered_width) / 2.0;
    let top = body_origin_y + (available_height - rendered_height) / 2.0;

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

    if message == WM_SIZE {
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

fn preview_state(hwnd: HWND) -> Option<&'static RefCell<PreviewWindowState>> {
    // SAFETY: PreviewWindow stores a stable Box pointer at WM_NCCREATE and clears it before either
    // HWND or Box teardown. This helper is called only synchronously on the owning UI thread.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    (pointer != 0).then(|| unsafe { &*(pointer as *const RefCell<PreviewWindowState>) })
}

#[cfg(test)]
mod tests {
    use super::{
        D2D_SIZE_U, PreviewWindow, PreviewWindowState, RetainedContent, TextContent,
        checked_image_layout, color_from_colorref, format_file_size, image_destination_rect,
        image_preview_header, preview_header,
    };
    use crate::{
        hover::PhysicalScreenPoint,
        preview::PreviewSize,
        worker::{ImageFormat, ImagePreview, TextPreview, image_corpus_previews},
    };
    use std::thread;
    use windows::Win32::{
        Graphics::DirectWrite::DWRITE_TEXT_METRICS, UI::WindowsAndMessaging::IsWindow,
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
            linked_content: true,
            first_frame_only: true,
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
            preview
                .show_image_at(
                    PhysicalScreenPoint::new(200, 200),
                    PreviewSize::new(640, 480),
                    image_preview(),
                )
                .expect("bounded image pixels should render");
            assert!(preview.is_visible());
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
                let mut state =
                    PreviewWindowState::new().expect("DirectWrite factories should initialize");
                state.configure(
                    Some(RetainedContent::Text(TextContent::from_preview(&preview))),
                    dpi,
                );
                state.pixel_size = D2D_SIZE_U {
                    width: 640 * dpi / 96,
                    height: 480 * dpi / 96,
                };
                let layouts = state
                    .create_layouts()
                    .expect("the multilingual text should produce layouts");
                let body = layouts.body.expect("the corpus body is not empty");
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
        assert_eq!(
            image_preview_header(&preview),
            "PNG  ·  2 × 1  ·  12.5 KiB  ·  first frame  ·  linked"
        );
        assert_eq!(checked_image_layout(&preview), Some((8, 8)));

        let mut wrong_length = preview.clone();
        wrong_length.premultiplied_bgra.pop();
        assert_eq!(checked_image_layout(&wrong_length), None);

        let mut straight_alpha = preview;
        straight_alpha.premultiplied_bgra[2] = 41;
        assert_eq!(checked_image_layout(&straight_alpha), None);
    }

    #[test]
    fn image_destination_preserves_aspect_ratio_without_upscaling() {
        let preview = ImagePreview {
            file_size: 1,
            linked_content: false,
            first_frame_only: false,
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
        assert!((at_100.left - 12.0).abs() < 0.01);
        assert!((at_100.top - 79.75).abs() < 0.01);
        assert!((at_100.right - 628.0).abs() < 0.01);
        assert!((at_100.bottom - 426.25).abs() < 0.01);

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
        assert!((at_200.top - 118.0).abs() < 0.01);
        assert!((at_200.right - 560.0).abs() < 0.01);
        assert!((at_200.bottom - 388.0).abs() < 0.01);
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
