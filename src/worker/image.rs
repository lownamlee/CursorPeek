use std::{error::Error, fmt, io::BufReader, path::Path};

#[cfg(test)]
pub(super) mod corpus;

use ::image::{
    DynamicImage, ImageDecoder, ImageError, ImageFormat as DecoderFormat, Limits,
    codecs::{
        bmp::BmpDecoder, gif::GifDecoder, ico::IcoDecoder, jpeg::JpegDecoder, png::PngDecoder,
        tiff::TiffDecoder, webp::WebPDecoder,
    },
    imageops::{self, FilterType},
    metadata::Orientation,
};

use super::{
    file::{PreviewFile, PreviewFileError},
    payload::{
        BGRA_BYTES_PER_PIXEL, IMAGE_FIXED_LEN, ImageFormat, ImagePreview, MAX_PREVIEW_IMAGE_HEIGHT,
        MAX_PREVIEW_IMAGE_WIDTH, MAX_PREVIEW_PAYLOAD_LEN, MAX_SOURCE_IMAGE_AXIS,
        MAX_SOURCE_IMAGE_PIXELS, fitted_preview_dimensions,
    },
};

const MAGIC_PREFIX_LEN: usize = 16;
const MAX_IMAGE_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DECODED_IMAGE_BYTES: u64 = 160 * 1024 * 1024;
const MAX_DECODER_ALLOC_BYTES: u64 = 256 * 1024 * 1024;

const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "webp", "bmp", "dib", "ico", "tif", "tiff",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ValidatedImage {
    pub(super) format: ImageFormat,
    pub(super) source_width: u32,
    pub(super) source_height: u32,
    pub(super) decoded_bytes: u64,
    pub(super) orientation: Orientation,
}

impl ValidatedImage {
    fn display_dimensions(self) -> (u32, u32) {
        match self.orientation {
            Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH => (self.source_height, self.source_width),
            Orientation::NoTransforms
            | Orientation::Rotate180
            | Orientation::FlipHorizontal
            | Orientation::FlipVertical => (self.source_width, self.source_height),
        }
    }
}

#[derive(Debug)]
pub(super) struct DecodedImage {
    pub(super) metadata: ValidatedImage,
    pub(super) pixels: DynamicImage,
}

impl DecodedImage {
    pub(super) fn matches_metadata(&self) -> bool {
        let (expected_width, expected_height) = self.metadata.display_dimensions();
        self.pixels.width() == expected_width
            && self.pixels.height() == expected_height
            && u64::try_from(self.pixels.as_bytes().len()) == Ok(self.metadata.decoded_bytes)
    }

    pub(super) fn into_preview(
        self,
        file: &PreviewFile,
    ) -> Result<ImagePreview, ImageValidationError> {
        if !self.matches_metadata() {
            let (expected_width, expected_height) = self.metadata.display_dimensions();
            return Err(ImageValidationError::DecodedLayoutMismatch {
                expected_width,
                expected_height,
                expected_bytes: self.metadata.decoded_bytes,
                actual_width: self.pixels.width(),
                actual_height: self.pixels.height(),
                actual_bytes: self.pixels.as_bytes().len(),
            });
        }

        let metadata = self.metadata;
        let (source_width, source_height) = metadata.display_dimensions();
        let (width, height) = fitted_preview_dimensions(source_width, source_height)
            .ok_or(ImageValidationError::ArithmeticOverflow)?;
        let (_, expected_bytes) = checked_bgra_layout(width, height)?;

        let mut rgba = self.pixels.into_rgba8();
        premultiply_rgba(&mut rgba);
        let pixels = if (width, height) == (source_width, source_height) {
            rgba
        } else {
            imageops::resize(&rgba, width, height, FilterType::Triangle)
        };
        let mut premultiplied_bgra = pixels.into_raw();
        for pixel in premultiplied_bgra.chunks_exact_mut(BGRA_BYTES_PER_PIXEL) {
            pixel.swap(0, 2);
        }
        if premultiplied_bgra.len() != expected_bytes {
            return Err(ImageValidationError::PreviewLayoutMismatch {
                expected_bytes,
                actual_bytes: premultiplied_bgra.len(),
            });
        }
        if !file.is_unchanged()? {
            return Err(ImageValidationError::File(
                PreviewFileError::ChangedDuringRead,
            ));
        }

        Ok(ImagePreview {
            file_size: file.file_size(),
            linked_content: file.is_linked_content(),
            first_frame_only: matches!(
                metadata.format,
                ImageFormat::Png | ImageFormat::Gif | ImageFormat::WebP
            ),
            format: metadata.format,
            source_width,
            source_height,
            width,
            height,
            premultiplied_bgra,
        })
    }
}

fn premultiply_rgba(pixels: &mut ::image::RgbaImage) {
    for pixel in pixels.pixels_mut() {
        let [red, green, blue, alpha] = pixel.0;
        let premultiply = |channel: u8| {
            let product = u16::from(channel) * u16::from(alpha);
            u8::try_from((product + 127) / 255).expect("premultiplication stays within one byte")
        };
        pixel.0 = [
            premultiply(red),
            premultiply(green),
            premultiply(blue),
            alpha,
        ];
    }
}

#[derive(Debug)]
pub(super) enum ImageDecodeResult {
    Decoded(DecodedImage),
    Unsupported,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ImageValidationResult {
    Validated(ValidatedImage),
    Unsupported,
}

#[cfg(test)]
pub(super) fn validate(file: &PreviewFile) -> Result<ImageValidationResult, ImageValidationError> {
    let Some((decoder_format, format)) = selected_format(file)? else {
        return Ok(ImageValidationResult::Unsupported);
    };

    let (source_width, source_height, decoded_bytes, orientation) = inspect_header(
        BufReader::new(file.duplicate_reader()?),
        decoder_format,
        decoder_limits(),
    )
    .map_err(ImageValidationError::Decoder)?;

    let metadata = validated_metadata(
        format,
        source_width,
        source_height,
        decoded_bytes,
        orientation,
    )?;
    if !file.is_unchanged()? {
        return Err(ImageValidationError::File(
            PreviewFileError::ChangedDuringRead,
        ));
    }

    Ok(ImageValidationResult::Validated(metadata))
}

pub(super) fn decode(file: &PreviewFile) -> Result<ImageDecodeResult, ImageValidationError> {
    let Some((decoder_format, format)) = selected_format(file)? else {
        return Ok(ImageDecodeResult::Unsupported);
    };
    let decoded = decode_selected(
        BufReader::new(file.duplicate_reader()?),
        decoder_format,
        format,
        decoder_limits(),
    )?;
    if !file.is_unchanged()? {
        return Err(ImageValidationError::File(
            PreviewFileError::ChangedDuringRead,
        ));
    }
    Ok(ImageDecodeResult::Decoded(decoded))
}

fn selected_format(
    file: &PreviewFile,
) -> Result<Option<(DecoderFormat, ImageFormat)>, ImageValidationError> {
    if !is_eligible_path(file.final_path()) {
        return Ok(None);
    }
    if file.file_size() > MAX_IMAGE_FILE_BYTES {
        return Err(ImageValidationError::FileTooLarge {
            actual: file.file_size(),
        });
    }

    let prefix = file.read_prefix(MAGIC_PREFIX_LEN)?;
    let Ok(decoder_format) = ::image::guess_format(&prefix) else {
        return Ok(None);
    };
    let Some(format) = supported_format(decoder_format) else {
        return Ok(None);
    };
    if !decoder_format.reading_enabled() {
        return Ok(None);
    }
    Ok(Some((decoder_format, format)))
}

#[cfg(test)]
fn inspect_header<R>(
    reader: R,
    format: DecoderFormat,
    limits: Limits,
) -> Result<(u32, u32, u64, Orientation), ImageError>
where
    R: std::io::BufRead + std::io::Seek,
{
    match format {
        DecoderFormat::Jpeg => inspect_decoder(JpegDecoder::new(reader)?, limits),
        DecoderFormat::Png => {
            inspect_decoder(PngDecoder::with_limits(reader, limits.clone())?, limits)
        }
        DecoderFormat::Gif => inspect_decoder(GifDecoder::new(reader)?, limits),
        DecoderFormat::WebP => inspect_decoder(WebPDecoder::new(reader)?, limits),
        DecoderFormat::Bmp => inspect_decoder(BmpDecoder::new(reader)?, limits),
        DecoderFormat::Ico => inspect_decoder(IcoDecoder::new(reader)?, limits),
        DecoderFormat::Tiff => inspect_decoder(TiffDecoder::new(reader)?, limits),
        _ => unreachable!("unsupported formats are rejected before decoder construction"),
    }
}

#[cfg(test)]
fn inspect_decoder(
    mut decoder: impl ImageDecoder,
    limits: Limits,
) -> Result<(u32, u32, u64, Orientation), ImageError> {
    decoder.set_limits(limits)?;
    let (width, height) = decoder.dimensions();
    let decoded_bytes = decoder.total_bytes();
    let orientation = decoder.orientation()?;
    Ok((width, height, decoded_bytes, orientation))
}

fn decode_selected<R>(
    reader: R,
    decoder_format: DecoderFormat,
    format: ImageFormat,
    limits: Limits,
) -> Result<DecodedImage, ImageValidationError>
where
    R: std::io::BufRead + std::io::Seek,
{
    match decoder_format {
        DecoderFormat::Jpeg => decode_with_decoder(JpegDecoder::new(reader)?, format, limits),
        // ImageDecoder reads the PNG default image rather than iterating APNG frames.
        DecoderFormat::Png => decode_with_decoder(
            PngDecoder::with_limits(reader, limits.clone())?,
            format,
            limits,
        ),
        // The still-image decoder produces the first GIF composited on its logical canvas.
        DecoderFormat::Gif => decode_with_decoder(GifDecoder::new(reader)?, format, limits),
        // The image-webp still-image contract returns the first animation frame.
        DecoderFormat::WebP => decode_with_decoder(WebPDecoder::new(reader)?, format, limits),
        DecoderFormat::Bmp => decode_with_decoder(BmpDecoder::new(reader)?, format, limits),
        // IcoDecoder selects the entry with the highest (color depth, pixel area).
        DecoderFormat::Ico => decode_with_decoder(IcoDecoder::new(reader)?, format, limits),
        // TiffDecoder starts at the first image file directory and never advances pages here.
        DecoderFormat::Tiff => decode_with_decoder(TiffDecoder::new(reader)?, format, limits),
        _ => unreachable!("unsupported formats are rejected before decoder construction"),
    }
}

fn decode_with_decoder(
    mut decoder: impl ImageDecoder,
    format: ImageFormat,
    limits: Limits,
) -> Result<DecodedImage, ImageValidationError> {
    decoder.set_limits(limits)?;
    let (source_width, source_height) = decoder.dimensions();
    let decoded_bytes = decoder.total_bytes();
    let orientation = decoder.orientation()?;
    let metadata = validated_metadata(
        format,
        source_width,
        source_height,
        decoded_bytes,
        orientation,
    )?;
    let mut pixels = DynamicImage::from_decoder(decoder)?;
    let actual_width = pixels.width();
    let actual_height = pixels.height();
    let actual_bytes = pixels.as_bytes().len();
    if actual_width != source_width
        || actual_height != source_height
        || u64::try_from(actual_bytes) != Ok(decoded_bytes)
    {
        return Err(ImageValidationError::DecodedLayoutMismatch {
            expected_width: source_width,
            expected_height: source_height,
            expected_bytes: decoded_bytes,
            actual_width,
            actual_height,
            actual_bytes,
        });
    }
    pixels.apply_orientation(orientation);
    let decoded = DecodedImage { metadata, pixels };
    if !decoded.matches_metadata() {
        let (expected_width, expected_height) = metadata.display_dimensions();
        return Err(ImageValidationError::DecodedLayoutMismatch {
            expected_width,
            expected_height,
            expected_bytes: decoded_bytes,
            actual_width: decoded.pixels.width(),
            actual_height: decoded.pixels.height(),
            actual_bytes: decoded.pixels.as_bytes().len(),
        });
    }
    Ok(decoded)
}

fn validated_metadata(
    format: ImageFormat,
    source_width: u32,
    source_height: u32,
    decoded_bytes: u64,
    orientation: Orientation,
) -> Result<ValidatedImage, ImageValidationError> {
    validate_source_layout(source_width, source_height, decoded_bytes)?;
    checked_bgra_layout(
        source_width.min(MAX_PREVIEW_IMAGE_WIDTH),
        source_height.min(MAX_PREVIEW_IMAGE_HEIGHT),
    )?;
    Ok(ValidatedImage {
        format,
        source_width,
        source_height,
        decoded_bytes,
        orientation,
    })
}

fn decoder_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_IMAGE_AXIS);
    limits.max_image_height = Some(MAX_SOURCE_IMAGE_AXIS);
    limits.max_alloc = Some(MAX_DECODER_ALLOC_BYTES);
    limits
}

pub(super) fn is_eligible_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn supported_format(format: DecoderFormat) -> Option<ImageFormat> {
    match format {
        DecoderFormat::Jpeg => Some(ImageFormat::Jpeg),
        DecoderFormat::Png => Some(ImageFormat::Png),
        DecoderFormat::Gif => Some(ImageFormat::Gif),
        DecoderFormat::WebP => Some(ImageFormat::WebP),
        DecoderFormat::Bmp => Some(ImageFormat::Bmp),
        DecoderFormat::Ico => Some(ImageFormat::Ico),
        DecoderFormat::Tiff => Some(ImageFormat::Tiff),
        _ => None,
    }
}

fn validate_source_layout(
    width: u32,
    height: u32,
    decoded_bytes: u64,
) -> Result<(), ImageValidationError> {
    if width == 0 || height == 0 || width > MAX_SOURCE_IMAGE_AXIS || height > MAX_SOURCE_IMAGE_AXIS
    {
        return Err(ImageValidationError::InvalidDimensions { width, height });
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(ImageValidationError::ArithmeticOverflow)?;
    if pixels > MAX_SOURCE_IMAGE_PIXELS {
        return Err(ImageValidationError::TooManyPixels { actual: pixels });
    }
    if decoded_bytes > MAX_DECODED_IMAGE_BYTES {
        return Err(ImageValidationError::DecodedImageTooLarge {
            actual: decoded_bytes,
        });
    }
    Ok(())
}

pub(super) fn checked_bgra_layout(
    width: u32,
    height: u32,
) -> Result<(usize, usize), ImageValidationError> {
    if width == 0
        || height == 0
        || width > MAX_PREVIEW_IMAGE_WIDTH
        || height > MAX_PREVIEW_IMAGE_HEIGHT
    {
        return Err(ImageValidationError::InvalidPreviewDimensions { width, height });
    }
    let stride = usize::try_from(width)
        .ok()
        .and_then(|value| value.checked_mul(BGRA_BYTES_PER_PIXEL))
        .ok_or(ImageValidationError::ArithmeticOverflow)?;
    let length = usize::try_from(height)
        .ok()
        .and_then(|value| stride.checked_mul(value))
        .ok_or(ImageValidationError::ArithmeticOverflow)?;
    let wire_length = IMAGE_FIXED_LEN
        .checked_add(length)
        .ok_or(ImageValidationError::ArithmeticOverflow)?;
    if wire_length > MAX_PREVIEW_PAYLOAD_LEN {
        return Err(ImageValidationError::PreviewPayloadTooLarge {
            actual: wire_length,
        });
    }
    Ok((stride, length))
}

#[derive(Debug)]
pub(super) enum ImageValidationError {
    File(PreviewFileError),
    Decoder(ImageError),
    FileTooLarge {
        actual: u64,
    },
    InvalidDimensions {
        width: u32,
        height: u32,
    },
    TooManyPixels {
        actual: u64,
    },
    DecodedImageTooLarge {
        actual: u64,
    },
    DecodedLayoutMismatch {
        expected_width: u32,
        expected_height: u32,
        expected_bytes: u64,
        actual_width: u32,
        actual_height: u32,
        actual_bytes: usize,
    },
    PreviewLayoutMismatch {
        expected_bytes: usize,
        actual_bytes: usize,
    },
    InvalidPreviewDimensions {
        width: u32,
        height: u32,
    },
    PreviewPayloadTooLarge {
        actual: usize,
    },
    ArithmeticOverflow,
}

impl ImageValidationError {
    pub(super) fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::FileTooLarge { .. }
                | Self::InvalidDimensions { .. }
                | Self::TooManyPixels { .. }
                | Self::DecodedImageTooLarge { .. }
                | Self::InvalidPreviewDimensions { .. }
                | Self::PreviewPayloadTooLarge { .. }
                | Self::ArithmeticOverflow
                | Self::Decoder(ImageError::Limits(_) | ImageError::Unsupported(_))
        )
    }
}

impl fmt::Display for ImageValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(error) => write!(formatter, "{error}"),
            Self::Decoder(error) => write!(formatter, "image decoding failed: {error}"),
            Self::FileTooLarge { actual } => write!(
                formatter,
                "image file is {actual} bytes; the limit is {MAX_IMAGE_FILE_BYTES}"
            ),
            Self::InvalidDimensions { width, height } => write!(
                formatter,
                "image dimensions {width}x{height} are invalid or exceed the axis limit"
            ),
            Self::TooManyPixels { actual } => write!(
                formatter,
                "image has {actual} pixels; the limit is {MAX_SOURCE_IMAGE_PIXELS}"
            ),
            Self::DecodedImageTooLarge { actual } => write!(
                formatter,
                "decoded image requires {actual} bytes; the limit is {MAX_DECODED_IMAGE_BYTES}"
            ),
            Self::DecodedLayoutMismatch {
                expected_width,
                expected_height,
                expected_bytes,
                actual_width,
                actual_height,
                actual_bytes,
            } => write!(
                formatter,
                "decoded image layout mismatch: expected {expected_width}x{expected_height} \
                 and {expected_bytes} bytes, received {actual_width}x{actual_height} and \
                 {actual_bytes} bytes"
            ),
            Self::PreviewLayoutMismatch {
                expected_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "preview image layout mismatch: expected {expected_bytes} BGRA bytes, \
                 received {actual_bytes}"
            ),
            Self::InvalidPreviewDimensions { width, height } => {
                write!(formatter, "preview dimensions {width}x{height} are invalid")
            }
            Self::PreviewPayloadTooLarge { actual } => write!(
                formatter,
                "preview payload requires {actual} bytes; the limit is {MAX_PREVIEW_PAYLOAD_LEN}"
            ),
            Self::ArithmeticOverflow => write!(formatter, "image size arithmetic overflowed"),
        }
    }
}

impl Error for ImageValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::File(error) => Some(error),
            Self::Decoder(error) => Some(error),
            _ => None,
        }
    }
}

impl From<PreviewFileError> for ImageValidationError {
    fn from(error: PreviewFileError) -> Self {
        Self::File(error)
    }
}

impl From<ImageError> for ImageValidationError {
    fn from(error: ImageError) -> Self {
        Self::Decoder(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IMAGE_EXTENSIONS, ImageDecodeResult, ImageValidationError, ImageValidationResult,
        MAX_DECODED_IMAGE_BYTES, MAX_DECODER_ALLOC_BYTES, MAX_IMAGE_FILE_BYTES,
        checked_bgra_layout, decode, decoder_limits, fitted_preview_dimensions, is_eligible_path,
        validate, validate_source_layout,
    };
    use crate::worker::{
        file::PreviewFile,
        payload::{
            ImageFormat, MAX_PREVIEW_IMAGE_HEIGHT, MAX_PREVIEW_IMAGE_WIDTH, MAX_SOURCE_IMAGE_AXIS,
            MAX_SOURCE_IMAGE_PIXELS,
        },
    };
    use image::{
        DynamicImage, ExtendedColorType, Frame, ImageEncoder, ImageFormat as DecoderFormat, Rgba,
        RgbaImage,
        codecs::{gif::GifEncoder, png::PngEncoder},
        metadata::Orientation,
    };
    use std::{
        fs,
        io::Cursor,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "cursorpeek-image-{label}-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("the image test directory should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).expect("the image test directory should be removed");
        }
    }

    fn encode_rgba(image: RgbaImage, format: DecoderFormat) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(image);
        let mut output = Cursor::new(Vec::new());
        image
            .write_to(&mut output, format)
            .expect("the generated image should encode");
        output.into_inner()
    }

    fn encoded(format: DecoderFormat) -> Vec<u8> {
        encode_rgba(
            RgbaImage::from_fn(3, 2, |x, y| {
                Rgba([
                    10 + u8::try_from(x).unwrap(),
                    20 + u8::try_from(y).unwrap(),
                    30,
                    40 + u8::try_from(x + y).unwrap(),
                ])
            }),
            format,
        )
    }

    fn exif_orientation(value: u8) -> Vec<u8> {
        vec![
            b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, value, 0, 0, 0, 0,
            0, 0, 0,
        ]
    }

    fn oriented_png(orientation: u8) -> Vec<u8> {
        let image = RgbaImage::from_fn(2, 3, |x, y| {
            let red = u8::try_from(y * 2 + x + 1).unwrap();
            Rgba([red, 0, 0, 255])
        });
        let mut output = Vec::new();
        let mut encoder = PngEncoder::new(&mut output);
        encoder
            .set_exif_metadata(exif_orientation(orientation))
            .expect("PNG should accept Exif orientation");
        encoder
            .write_image(image.as_raw(), 2, 3, ExtendedColorType::Rgba8)
            .expect("the oriented PNG should encode");
        output
    }

    fn two_frame_gif() -> Vec<u8> {
        let first = Frame::new(RgbaImage::from_pixel(2, 1, Rgba([210, 10, 20, 255])));
        let second = Frame::new(RgbaImage::from_pixel(2, 1, Rgba([20, 210, 10, 255])));
        let mut output = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut output);
            encoder
                .encode_frames([first, second])
                .expect("the animated GIF should encode");
        }
        output
    }

    fn push_u16(output: &mut Vec<u8>, value: u16) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_tiff_entry(output: &mut Vec<u8>, tag: u16, field_type: u16, value: u32) {
        push_u16(output, tag);
        push_u16(output, field_type);
        push_u32(output, 1);
        push_u32(output, value);
    }

    fn push_tiff_ifd(output: &mut Vec<u8>, strip_offset: u32, next_ifd: u32) {
        push_u16(output, 9);
        push_tiff_entry(output, 256, 4, 1); // ImageWidth
        push_tiff_entry(output, 257, 4, 1); // ImageLength
        push_tiff_entry(output, 258, 3, 8); // BitsPerSample
        push_tiff_entry(output, 259, 3, 1); // Compression: none
        push_tiff_entry(output, 262, 3, 1); // PhotometricInterpretation: BlackIsZero
        push_tiff_entry(output, 273, 4, strip_offset); // StripOffsets
        push_tiff_entry(output, 277, 3, 1); // SamplesPerPixel
        push_tiff_entry(output, 278, 4, 1); // RowsPerStrip
        push_tiff_entry(output, 279, 4, 1); // StripByteCounts
        push_u32(output, next_ifd);
    }

    fn two_page_tiff() -> Vec<u8> {
        const FIRST_IFD: u32 = 8;
        const IFD_BYTES: u32 = 2 + 9 * 12 + 4;
        const SECOND_IFD: u32 = FIRST_IFD + IFD_BYTES;
        const FIRST_PIXEL: u32 = SECOND_IFD + IFD_BYTES;
        const SECOND_PIXEL: u32 = FIRST_PIXEL + 1;

        let mut output = Vec::new();
        output.extend_from_slice(b"II");
        push_u16(&mut output, 42);
        push_u32(&mut output, FIRST_IFD);
        push_tiff_ifd(&mut output, FIRST_PIXEL, SECOND_IFD);
        push_tiff_ifd(&mut output, SECOND_PIXEL, 0);
        assert_eq!(output.len(), usize::try_from(FIRST_PIXEL).unwrap());
        output.extend_from_slice(&[17, 221]);
        output
    }

    fn png_entry(width: u32, height: u32, color: Rgba<u8>) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, color));
        let mut output = Cursor::new(Vec::new());
        image
            .write_to(&mut output, DecoderFormat::Png)
            .expect("the ICO PNG entry should encode");
        output.into_inner()
    }

    fn push_ico_entry(
        output: &mut Vec<u8>,
        width: u8,
        height: u8,
        bits_per_pixel: u16,
        length: usize,
        offset: usize,
    ) {
        output.extend_from_slice(&[width, height, 0, 0]);
        push_u16(output, 1);
        push_u16(output, bits_per_pixel);
        push_u32(output, u32::try_from(length).unwrap());
        push_u32(output, u32::try_from(offset).unwrap());
    }

    fn multi_entry_ico() -> Vec<u8> {
        let deeper = png_entry(1, 1, Rgba([190, 30, 20, 255]));
        let larger = png_entry(2, 2, Rgba([20, 190, 30, 255]));
        let directory_end = 6 + 2 * 16;
        let mut output = Vec::new();
        push_u16(&mut output, 0);
        push_u16(&mut output, 1);
        push_u16(&mut output, 2);
        push_ico_entry(&mut output, 1, 1, 32, deeper.len(), directory_end);
        push_ico_entry(
            &mut output,
            2,
            2,
            24,
            larger.len(),
            directory_end + deeper.len(),
        );
        output.extend_from_slice(&deeper);
        output.extend_from_slice(&larger);
        output
    }

    #[test]
    fn eligibility_is_case_insensitive_and_limited_to_the_product_formats() {
        for extension in IMAGE_EXTENSIONS {
            assert!(is_eligible_path(Path::new(&format!(
                "sample.{}",
                extension.to_ascii_uppercase()
            ))));
        }
        for path in [
            "sample",
            "sample.apng",
            "sample.avif",
            "sample.svg",
            "sample.txt",
        ] {
            assert!(!is_eligible_path(Path::new(path)));
        }
    }

    #[test]
    fn every_selected_format_validates_and_decodes_bounded_pixels() {
        let root = TestDirectory::new("formats");
        let cases = [
            (DecoderFormat::Jpeg, ImageFormat::Jpeg, "jpg"),
            (DecoderFormat::Png, ImageFormat::Png, "png"),
            (DecoderFormat::Gif, ImageFormat::Gif, "gif"),
            (DecoderFormat::WebP, ImageFormat::WebP, "webp"),
            (DecoderFormat::Bmp, ImageFormat::Bmp, "bmp"),
            (DecoderFormat::Ico, ImageFormat::Ico, "ico"),
            (DecoderFormat::Tiff, ImageFormat::Tiff, "tiff"),
        ];

        for (decoder_format, expected_format, extension) in cases {
            let path = root.path().join(format!("sample.{extension}"));
            fs::write(&path, encoded(decoder_format))
                .expect("the generated image should be written");
            let file = PreviewFile::open(&path).expect("the generated image should open");
            let ImageValidationResult::Validated(validated) =
                validate(&file).expect("the generated header should validate")
            else {
                panic!("the selected image format should be recognized");
            };
            assert_eq!(validated.format, expected_format);
            assert_eq!((validated.source_width, validated.source_height), (3, 2));
            assert_eq!(validated.orientation, Orientation::NoTransforms);
            assert!(validated.decoded_bytes > 0);
            assert!(validated.decoded_bytes <= MAX_DECODED_IMAGE_BYTES);

            let ImageDecodeResult::Decoded(decoded) =
                decode(&file).expect("the generated pixels should decode")
            else {
                panic!("the selected image format should decode");
            };
            assert_eq!(decoded.metadata, validated);
            assert!(decoded.matches_metadata());
            assert_eq!((decoded.pixels.width(), decoded.pixels.height()), (3, 2));
            if expected_format == ImageFormat::Png {
                assert_eq!(
                    decoded.pixels.to_rgba8().get_pixel(0, 0).0,
                    [10, 20, 30, 40]
                );
            }
        }
    }

    #[test]
    fn every_exif_orientation_is_applied_to_decoded_pixels() {
        let root = TestDirectory::new("orientation");
        let cases: [(u8, (u32, u32), &[u8]); 8] = [
            (1, (2, 3), &[1, 2, 3, 4, 5, 6]),
            (2, (2, 3), &[2, 1, 4, 3, 6, 5]),
            (3, (2, 3), &[6, 5, 4, 3, 2, 1]),
            (4, (2, 3), &[5, 6, 3, 4, 1, 2]),
            (5, (3, 2), &[1, 3, 5, 2, 4, 6]),
            (6, (3, 2), &[5, 3, 1, 6, 4, 2]),
            (7, (3, 2), &[6, 4, 2, 5, 3, 1]),
            (8, (3, 2), &[2, 4, 6, 1, 3, 5]),
        ];

        for (exif_value, expected_dimensions, expected_red) in cases {
            let path = root.path().join(format!("oriented-{exif_value}.png"));
            fs::write(&path, oriented_png(exif_value)).unwrap();
            let file = PreviewFile::open(&path).unwrap();

            let ImageValidationResult::Validated(validated) = validate(&file).unwrap() else {
                panic!("the oriented PNG should validate");
            };
            assert_eq!(
                validated.orientation,
                Orientation::from_exif(exif_value).unwrap()
            );
            assert_eq!((validated.source_width, validated.source_height), (2, 3));
            assert_eq!(validated.display_dimensions(), expected_dimensions);

            let ImageDecodeResult::Decoded(decoded) = decode(&file).unwrap() else {
                panic!("the oriented PNG should decode");
            };
            assert!(decoded.matches_metadata());
            assert_eq!(
                (decoded.pixels.width(), decoded.pixels.height()),
                expected_dimensions
            );
            let pixels = decoded.pixels.to_rgba8();
            assert_eq!(
                pixels.pixels().map(|pixel| pixel.0[0]).collect::<Vec<_>>(),
                expected_red
            );
        }
    }

    #[test]
    fn preview_fit_is_aspect_preserving_bounded_and_never_upscales() {
        assert_eq!(fitted_preview_dimensions(100, 50), Some((100, 50)));
        assert_eq!(fitted_preview_dimensions(1920, 1080), Some((960, 540)));
        assert_eq!(fitted_preview_dimensions(800, 1200), Some((480, 720)));
        assert_eq!(fitted_preview_dimensions(1000, 1), Some((960, 1)));
        assert_eq!(fitted_preview_dimensions(1, 1000), Some((1, 720)));
        assert_eq!(fitted_preview_dimensions(8000, 5000), Some((960, 600)));
        assert_eq!(fitted_preview_dimensions(0, 1), None);
        assert_eq!(fitted_preview_dimensions(1, 0), None);
    }

    #[test]
    fn decoded_pixels_become_checked_premultiplied_bgra_previews() {
        let root = TestDirectory::new("bgra");
        let exact_path = root.path().join("exact.png");
        fs::write(&exact_path, encoded(DecoderFormat::Png)).unwrap();
        let file = PreviewFile::open(&exact_path).unwrap();
        let ImageDecodeResult::Decoded(decoded) = decode(&file).unwrap() else {
            panic!("the exact-size PNG should decode");
        };
        let preview = decoded.into_preview(&file).unwrap();
        assert_eq!((preview.source_width, preview.source_height), (3, 2));
        assert_eq!((preview.width, preview.height), (3, 2));
        assert_eq!(preview.file_size, file.file_size());
        assert!(!preview.linked_content);
        assert!(preview.first_frame_only);
        assert_eq!(preview.format, ImageFormat::Png);
        assert_eq!(&preview.premultiplied_bgra[..4], &[5, 3, 2, 40]);
        assert_eq!(preview.premultiplied_bgra.len(), 3 * 2 * 4);

        let scaled_path = root.path().join("scaled.png");
        let wide = RgbaImage::from_fn(1000, 2, |x, _| {
            if x < 500 {
                Rgba([200, 100, 50, 128])
            } else {
                Rgba([255, 255, 255, 0])
            }
        });
        fs::write(&scaled_path, encode_rgba(wide, DecoderFormat::Png)).unwrap();
        let file = PreviewFile::open(&scaled_path).unwrap();
        let ImageDecodeResult::Decoded(decoded) = decode(&file).unwrap() else {
            panic!("the wide PNG should decode");
        };
        let preview = decoded.into_preview(&file).unwrap();
        assert_eq!((preview.source_width, preview.source_height), (1000, 2));
        assert_eq!((preview.width, preview.height), (960, 2));
        assert_eq!(preview.premultiplied_bgra.len(), 960 * 2 * 4);
        for pixel in preview.premultiplied_bgra.chunks_exact(4) {
            assert!(pixel[0] <= pixel[3]);
            assert!(pixel[1] <= pixel[3]);
            assert!(pixel[2] <= pixel[3]);
        }
    }

    #[test]
    fn animated_gif_uses_only_the_first_composited_frame() {
        let root = TestDirectory::new("gif-first-frame");
        let path = root.path().join("animated.gif");
        fs::write(&path, two_frame_gif()).unwrap();
        let file = PreviewFile::open(&path).unwrap();

        let ImageDecodeResult::Decoded(decoded) = decode(&file).unwrap() else {
            panic!("the animated GIF should decode");
        };
        assert_eq!((decoded.pixels.width(), decoded.pixels.height()), (2, 1));
        for pixel in decoded.pixels.to_rgba8().pixels() {
            assert!(pixel.0[0] > 180);
            assert!(pixel.0[1] < 40);
            assert!(pixel.0[2] < 50);
        }
    }

    #[test]
    fn tiff_and_ico_use_deterministic_first_image_policies() {
        let root = TestDirectory::new("multi-image-policy");

        let tiff_path = root.path().join("pages.tiff");
        fs::write(&tiff_path, two_page_tiff()).unwrap();
        let file = PreviewFile::open(&tiff_path).unwrap();
        let ImageDecodeResult::Decoded(decoded) = decode(&file).unwrap() else {
            panic!("the multipage TIFF should decode");
        };
        assert_eq!((decoded.pixels.width(), decoded.pixels.height()), (1, 1));
        assert_eq!(decoded.pixels.to_luma8().get_pixel(0, 0).0, [17]);

        let ico_path = root.path().join("entries.ico");
        fs::write(&ico_path, multi_entry_ico()).unwrap();
        let file = PreviewFile::open(&ico_path).unwrap();
        let ImageDecodeResult::Decoded(decoded) = decode(&file).unwrap() else {
            panic!("the multi-entry ICO should decode");
        };
        assert_eq!((decoded.pixels.width(), decoded.pixels.height()), (1, 1));
        assert_eq!(
            decoded.pixels.to_rgba8().get_pixel(0, 0).0,
            [190, 30, 20, 255]
        );
    }

    #[test]
    fn magic_is_authoritative_after_extension_eligibility() {
        let root = TestDirectory::new("magic");
        let misleading = root.path().join("sample.JpEg");
        fs::write(&misleading, encoded(DecoderFormat::Png)).unwrap();
        let file = PreviewFile::open(&misleading).unwrap();
        let ImageValidationResult::Validated(validated) = validate(&file).unwrap() else {
            panic!("a selected magic format should validate");
        };
        assert_eq!(validated.format, ImageFormat::Png);
        let ImageDecodeResult::Decoded(decoded) = decode(&file).unwrap() else {
            panic!("the selected magic format should decode");
        };
        assert_eq!(decoded.metadata.format, ImageFormat::Png);

        let unsupported_extension = root.path().join("sample.bin");
        fs::write(&unsupported_extension, encoded(DecoderFormat::Png)).unwrap();
        let file = PreviewFile::open(&unsupported_extension).unwrap();
        assert_eq!(validate(&file).unwrap(), ImageValidationResult::Unsupported);
        assert!(matches!(
            decode(&file).unwrap(),
            ImageDecodeResult::Unsupported
        ));

        let unsupported_magic = root.path().join("sample.png");
        fs::write(&unsupported_magic, b"qoif\0\0\0\x01\0\0\0\x01\x04\0").unwrap();
        let file = PreviewFile::open(&unsupported_magic).unwrap();
        assert_eq!(validate(&file).unwrap(), ImageValidationResult::Unsupported);
        assert!(matches!(
            decode(&file).unwrap(),
            ImageDecodeResult::Unsupported
        ));
    }

    #[test]
    fn malformed_supported_headers_and_oversized_files_fail_closed() {
        let root = TestDirectory::new("invalid");
        let malformed = root.path().join("malformed.png");
        fs::write(&malformed, b"\x89PNG\r\n\x1a\n").unwrap();
        let file = PreviewFile::open(&malformed).unwrap();
        assert!(matches!(
            validate(&file),
            Err(ImageValidationError::Decoder(_))
        ));
        assert!(matches!(
            decode(&file),
            Err(ImageValidationError::Decoder(_))
        ));

        let oversized = root.path().join("oversized.png");
        let sparse = fs::File::create(&oversized).unwrap();
        sparse.set_len(MAX_IMAGE_FILE_BYTES + 1).unwrap();
        drop(sparse);
        let file = PreviewFile::open(&oversized).unwrap();
        assert!(matches!(
            validate(&file),
            Err(ImageValidationError::FileTooLarge { actual })
                if actual == MAX_IMAGE_FILE_BYTES + 1
        ));
    }

    #[test]
    fn source_and_bgra_arithmetic_enforce_every_resource_boundary() {
        assert!(validate_source_layout(10_000, 4_000, MAX_DECODED_IMAGE_BYTES).is_ok());
        assert!(matches!(
            validate_source_layout(0, 1, 0),
            Err(ImageValidationError::InvalidDimensions { .. })
        ));
        assert!(matches!(
            validate_source_layout(MAX_SOURCE_IMAGE_AXIS + 1, 1, 0),
            Err(ImageValidationError::InvalidDimensions { .. })
        ));
        assert!(matches!(
            validate_source_layout(10_000, 4_001, 0),
            Err(ImageValidationError::TooManyPixels { actual })
                if actual > MAX_SOURCE_IMAGE_PIXELS
        ));
        assert!(matches!(
            validate_source_layout(1, 1, MAX_DECODED_IMAGE_BYTES + 1),
            Err(ImageValidationError::DecodedImageTooLarge { .. })
        ));

        let (stride, length) =
            checked_bgra_layout(MAX_PREVIEW_IMAGE_WIDTH, MAX_PREVIEW_IMAGE_HEIGHT).unwrap();
        assert_eq!(
            stride,
            usize::try_from(MAX_PREVIEW_IMAGE_WIDTH).unwrap() * 4
        );
        assert_eq!(
            length,
            stride * usize::try_from(MAX_PREVIEW_IMAGE_HEIGHT).unwrap()
        );
        for (width, height) in [
            (0, 1),
            (1, 0),
            (MAX_PREVIEW_IMAGE_WIDTH + 1, 1),
            (1, MAX_PREVIEW_IMAGE_HEIGHT + 1),
        ] {
            assert!(matches!(
                checked_bgra_layout(width, height),
                Err(ImageValidationError::InvalidPreviewDimensions { .. })
            ));
        }
    }

    #[test]
    fn crate_limits_are_a_defense_in_depth_layer_below_product_caps() {
        let limits = decoder_limits();
        assert_eq!(limits.max_image_width, Some(MAX_SOURCE_IMAGE_AXIS));
        assert_eq!(limits.max_image_height, Some(MAX_SOURCE_IMAGE_AXIS));
        assert_eq!(limits.max_alloc, Some(MAX_DECODER_ALLOC_BYTES));
    }
}
