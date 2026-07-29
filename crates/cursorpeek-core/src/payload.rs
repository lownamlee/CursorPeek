use std::{error::Error, fmt};

pub use crate::layout::{
    BGRA_BYTES_PER_PIXEL, IMAGE_FIXED_LEN, MAX_PREVIEW_IMAGE_HEIGHT, MAX_PREVIEW_IMAGE_WIDTH,
    MAX_PREVIEW_PAYLOAD_LEN, MAX_SOURCE_IMAGE_AXIS, MAX_SOURCE_IMAGE_PIXELS,
    fitted_preview_dimensions,
};
use crate::layout::{LayoutError, checked_bgra_layout};

pub const MIN_PREVIEW_RESULT_LEN: usize = 8;
pub const MAX_TEXT_UTF8_LEN: usize = 128 * 1024;
pub const MAX_TEXT_SCALARS: usize = 32_000;
pub const MAX_TEXT_LINES: usize = 200;
pub const MAX_ENCODING_LABEL_LEN: usize = 40;
pub const MAX_DISPLAY_NAME_UTF8_LEN: usize = 1_024;
pub const MAX_VIDEO_PATH_UNITS: usize = 32_768;

const RESULT_STATUS: u32 = 0;
const RESULT_TEXT: u32 = 1;
const RESULT_IMAGE: u32 = 2;
const RESULT_VIDEO: u32 = 3;
const STATUS_PAYLOAD_LEN: usize = 8;
const TEXT_FIXED_LEN: usize = 36;
const FLAG_LINKED_CONTENT: u32 = 1 << 0;
const FLAG_TEXT_ENCODING_GUESSED: u32 = 1 << 1;
const FLAG_TEXT_TRUNCATED: u32 = 1 << 2;
const TEXT_FLAGS: u32 = FLAG_LINKED_CONTENT | FLAG_TEXT_ENCODING_GUESSED | FLAG_TEXT_TRUNCATED;
const FLAG_IMAGE_FIRST_FRAME_ONLY: u32 = 1 << 1;
const IMAGE_FLAGS: u32 = FLAG_LINKED_CONTENT | FLAG_IMAGE_FIRST_FRAME_ONLY;
const VIDEO_FIXED_LEN: usize = 32;
const VIDEO_FLAGS: u32 = FLAG_LINKED_CONTENT;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolverStatus {
    Resolved = 0,
    Unsupported = 1,
    Ambiguous = 2,
    Unavailable = 3,
    TimedOut = 4,
    PointerMoved = 5,
}

impl ResolverStatus {
    fn from_raw(value: u32) -> Result<Self, PayloadError> {
        match value {
            0 => Ok(Self::Resolved),
            1 => Ok(Self::Unsupported),
            2 => Ok(Self::Ambiguous),
            3 => Ok(Self::Unavailable),
            4 => Ok(Self::TimedOut),
            5 => Ok(Self::PointerMoved),
            _ => Err(PayloadError::UnknownResolverStatus(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageFormat {
    Jpeg = 0,
    Png = 1,
    Gif = 2,
    WebP = 3,
    Bmp = 4,
    Ico = 5,
    Tiff = 6,
}

impl ImageFormat {
    fn from_raw(value: u32) -> Result<Self, PayloadError> {
        match value {
            0 => Ok(Self::Jpeg),
            1 => Ok(Self::Png),
            2 => Ok(Self::Gif),
            3 => Ok(Self::WebP),
            4 => Ok(Self::Bmp),
            5 => Ok(Self::Ico),
            6 => Ok(Self::Tiff),
            _ => Err(PayloadError::UnknownImageFormat(value)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextPreview {
    pub file_size: u64,
    pub last_write_time: i64,
    pub linked_content: bool,
    pub encoding_was_guessed: bool,
    pub truncated: bool,
    pub display_name: String,
    pub encoding: String,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImagePreview {
    pub file_size: u64,
    pub last_write_time: i64,
    pub linked_content: bool,
    pub first_frame_only: bool,
    pub display_name: String,
    pub format: ImageFormat,
    pub source_width: u32,
    pub source_height: u32,
    pub width: u32,
    pub height: u32,
    pub premultiplied_bgra: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoPreview {
    pub file_size: u64,
    pub last_write_time: i64,
    pub linked_content: bool,
    pub display_name: String,
    /// Absolute, local, non-NUL-terminated UTF-16 path validated by the worker.
    pub path: Vec<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreviewResult {
    Status(ResolverStatus),
    Text(TextPreview),
    Image(ImagePreview),
    Video(VideoPreview),
}

impl PreviewResult {
    pub const fn status(&self) -> Option<ResolverStatus> {
        match self {
            Self::Status(status) => Some(*status),
            Self::Text(_) | Self::Image(_) | Self::Video(_) => None,
        }
    }
}

pub fn encode_result(result: &PreviewResult) -> Result<Vec<u8>, PayloadError> {
    let mut output = Vec::new();
    match result {
        PreviewResult::Status(status) => {
            output.reserve(STATUS_PAYLOAD_LEN);
            push_u32(&mut output, RESULT_STATUS);
            push_u32(&mut output, *status as u32);
        }
        PreviewResult::Text(preview) => {
            validate_text(preview)?;
            let payload_len = TEXT_FIXED_LEN
                .checked_add(preview.display_name.len())
                .and_then(|length| length.checked_add(preview.encoding.len()))
                .and_then(|length| length.checked_add(preview.text.len()))
                .ok_or(PayloadError::LengthOverflow)?;
            ensure_payload_cap(payload_len)?;
            output.reserve(payload_len);
            push_u32(&mut output, RESULT_TEXT);
            push_u32(
                &mut output,
                flags(&[
                    (preview.linked_content, FLAG_LINKED_CONTENT),
                    (preview.encoding_was_guessed, FLAG_TEXT_ENCODING_GUESSED),
                    (preview.truncated, FLAG_TEXT_TRUNCATED),
                ]),
            );
            push_u64(&mut output, preview.file_size);
            push_i64(&mut output, preview.last_write_time);
            push_u32(
                &mut output,
                u32::try_from(preview.display_name.len())
                    .expect("the display-name cap fits the wire length"),
            );
            push_u32(
                &mut output,
                u32::try_from(preview.encoding.len())
                    .expect("the encoding label cap fits the wire length"),
            );
            push_u32(
                &mut output,
                u32::try_from(preview.text.len()).expect("the text cap fits the wire length"),
            );
            output.extend_from_slice(preview.display_name.as_bytes());
            output.extend_from_slice(preview.encoding.as_bytes());
            output.extend_from_slice(preview.text.as_bytes());
        }
        PreviewResult::Image(preview) => {
            validate_image(preview)?;
            let payload_len = IMAGE_FIXED_LEN
                .checked_add(preview.display_name.len())
                .and_then(|length| length.checked_add(preview.premultiplied_bgra.len()))
                .ok_or(PayloadError::LengthOverflow)?;
            ensure_payload_cap(payload_len)?;
            output.reserve(payload_len);
            push_u32(&mut output, RESULT_IMAGE);
            push_u32(
                &mut output,
                flags(&[
                    (preview.linked_content, FLAG_LINKED_CONTENT),
                    (preview.first_frame_only, FLAG_IMAGE_FIRST_FRAME_ONLY),
                ]),
            );
            push_u64(&mut output, preview.file_size);
            push_i64(&mut output, preview.last_write_time);
            push_u32(&mut output, preview.format as u32);
            push_u32(&mut output, preview.source_width);
            push_u32(&mut output, preview.source_height);
            push_u32(&mut output, preview.width);
            push_u32(&mut output, preview.height);
            push_u32(
                &mut output,
                u32::try_from(preview.display_name.len())
                    .expect("the display-name cap fits the wire length"),
            );
            push_u32(
                &mut output,
                u32::try_from(preview.premultiplied_bgra.len())
                    .expect("the image payload cap fits the wire length"),
            );
            output.extend_from_slice(preview.display_name.as_bytes());
            output.extend_from_slice(&preview.premultiplied_bgra);
        }
        PreviewResult::Video(preview) => {
            validate_video(preview)?;
            let path_bytes = preview
                .path
                .len()
                .checked_mul(2)
                .ok_or(PayloadError::LengthOverflow)?;
            let payload_len = VIDEO_FIXED_LEN
                .checked_add(preview.display_name.len())
                .and_then(|length| length.checked_add(path_bytes))
                .ok_or(PayloadError::LengthOverflow)?;
            ensure_payload_cap(payload_len)?;
            output.reserve(payload_len);
            push_u32(&mut output, RESULT_VIDEO);
            push_u32(
                &mut output,
                flags(&[(preview.linked_content, FLAG_LINKED_CONTENT)]),
            );
            push_u64(&mut output, preview.file_size);
            push_i64(&mut output, preview.last_write_time);
            push_u32(
                &mut output,
                u32::try_from(preview.display_name.len())
                    .expect("the display-name cap fits the wire length"),
            );
            push_u32(
                &mut output,
                u32::try_from(preview.path.len()).expect("the path cap fits the wire length"),
            );
            output.extend_from_slice(preview.display_name.as_bytes());
            for unit in &preview.path {
                output.extend_from_slice(&unit.to_le_bytes());
            }
        }
    }
    Ok(output)
}

pub fn decode_result(bytes: &[u8]) -> Result<PreviewResult, PayloadError> {
    if !(MIN_PREVIEW_RESULT_LEN..=MAX_PREVIEW_PAYLOAD_LEN).contains(&bytes.len()) {
        return Err(PayloadError::PayloadLengthOutOfRange {
            actual: bytes.len(),
        });
    }

    match read_u32(bytes, 0) {
        RESULT_STATUS => decode_status(bytes),
        RESULT_TEXT => decode_text(bytes),
        RESULT_IMAGE => decode_image(bytes),
        RESULT_VIDEO => decode_video(bytes),
        kind => Err(PayloadError::UnknownResultKind(kind)),
    }
}

fn decode_video(bytes: &[u8]) -> Result<PreviewResult, PayloadError> {
    if bytes.len() < VIDEO_FIXED_LEN {
        return Err(PayloadError::PayloadLengthMismatch {
            kind: "video",
            expected: VIDEO_FIXED_LEN,
            actual: bytes.len(),
        });
    }
    let raw_flags = read_u32(bytes, 4);
    reject_unknown_flags("video", raw_flags, VIDEO_FLAGS)?;
    let display_name_len = read_u32(bytes, 24) as usize;
    let path_units = read_u32(bytes, 28) as usize;
    let path_bytes = path_units
        .checked_mul(2)
        .ok_or(PayloadError::LengthOverflow)?;
    let expected = VIDEO_FIXED_LEN
        .checked_add(display_name_len)
        .and_then(|length| length.checked_add(path_bytes))
        .ok_or(PayloadError::LengthOverflow)?;
    require_exact_length("video", bytes.len(), expected)?;
    let display_name_end = VIDEO_FIXED_LEN + display_name_len;
    let display_name = std::str::from_utf8(&bytes[VIDEO_FIXED_LEN..display_name_end])
        .map_err(|_| PayloadError::InvalidDisplayName)?;
    let path = bytes[display_name_end..]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let preview = VideoPreview {
        file_size: read_u64(bytes, 8),
        last_write_time: read_i64(bytes, 16),
        linked_content: raw_flags & FLAG_LINKED_CONTENT != 0,
        display_name: display_name.to_owned(),
        path,
    };
    validate_video(&preview)?;
    Ok(PreviewResult::Video(preview))
}

fn decode_status(bytes: &[u8]) -> Result<PreviewResult, PayloadError> {
    require_exact_length("status", bytes.len(), STATUS_PAYLOAD_LEN)?;
    Ok(PreviewResult::Status(ResolverStatus::from_raw(read_u32(
        bytes, 4,
    ))?))
}

fn decode_text(bytes: &[u8]) -> Result<PreviewResult, PayloadError> {
    if bytes.len() < TEXT_FIXED_LEN {
        return Err(PayloadError::PayloadLengthMismatch {
            kind: "text",
            expected: TEXT_FIXED_LEN,
            actual: bytes.len(),
        });
    }

    let raw_flags = read_u32(bytes, 4);
    reject_unknown_flags("text", raw_flags, TEXT_FLAGS)?;
    let display_name_len = read_u32(bytes, 24) as usize;
    let encoding_len = read_u32(bytes, 28) as usize;
    let text_len = read_u32(bytes, 32) as usize;
    let expected = TEXT_FIXED_LEN
        .checked_add(display_name_len)
        .and_then(|length| length.checked_add(encoding_len))
        .and_then(|length| length.checked_add(text_len))
        .ok_or(PayloadError::LengthOverflow)?;
    require_exact_length("text", bytes.len(), expected)?;
    if display_name_len > MAX_DISPLAY_NAME_UTF8_LEN {
        return Err(PayloadError::DisplayNameTooLong {
            actual: display_name_len,
        });
    }
    if encoding_len > MAX_ENCODING_LABEL_LEN {
        return Err(PayloadError::EncodingLabelTooLong {
            actual: encoding_len,
        });
    }
    if text_len > MAX_TEXT_UTF8_LEN {
        return Err(PayloadError::TextTooLarge { actual: text_len });
    }

    let display_name_end = TEXT_FIXED_LEN + display_name_len;
    let encoding_end = display_name_end + encoding_len;
    let display_name = std::str::from_utf8(&bytes[TEXT_FIXED_LEN..display_name_end])
        .map_err(|_| PayloadError::InvalidDisplayName)?;
    let encoding = std::str::from_utf8(&bytes[display_name_end..encoding_end])
        .map_err(|_| PayloadError::InvalidEncodingLabel)?;
    if !is_encoding_label(encoding) {
        return Err(PayloadError::InvalidEncodingLabel);
    }
    let text =
        std::str::from_utf8(&bytes[encoding_end..]).map_err(|_| PayloadError::InvalidTextUtf8)?;
    let preview = TextPreview {
        file_size: read_u64(bytes, 8),
        last_write_time: read_i64(bytes, 16),
        linked_content: raw_flags & FLAG_LINKED_CONTENT != 0,
        encoding_was_guessed: raw_flags & FLAG_TEXT_ENCODING_GUESSED != 0,
        truncated: raw_flags & FLAG_TEXT_TRUNCATED != 0,
        display_name: display_name.to_owned(),
        encoding: encoding.to_owned(),
        text: text.to_owned(),
    };
    validate_text(&preview)?;
    Ok(PreviewResult::Text(preview))
}

fn decode_image(bytes: &[u8]) -> Result<PreviewResult, PayloadError> {
    if bytes.len() < IMAGE_FIXED_LEN {
        return Err(PayloadError::PayloadLengthMismatch {
            kind: "image",
            expected: IMAGE_FIXED_LEN,
            actual: bytes.len(),
        });
    }

    let raw_flags = read_u32(bytes, 4);
    reject_unknown_flags("image", raw_flags, IMAGE_FLAGS)?;
    let display_name_len = read_u32(bytes, 44) as usize;
    let pixel_len = read_u32(bytes, 48) as usize;
    let expected = IMAGE_FIXED_LEN
        .checked_add(display_name_len)
        .and_then(|length| length.checked_add(pixel_len))
        .ok_or(PayloadError::LengthOverflow)?;
    require_exact_length("image", bytes.len(), expected)?;
    if display_name_len > MAX_DISPLAY_NAME_UTF8_LEN {
        return Err(PayloadError::DisplayNameTooLong {
            actual: display_name_len,
        });
    }

    let display_name_end = IMAGE_FIXED_LEN + display_name_len;
    let display_name = std::str::from_utf8(&bytes[IMAGE_FIXED_LEN..display_name_end])
        .map_err(|_| PayloadError::InvalidDisplayName)?;
    let preview = ImagePreview {
        file_size: read_u64(bytes, 8),
        last_write_time: read_i64(bytes, 16),
        linked_content: raw_flags & FLAG_LINKED_CONTENT != 0,
        first_frame_only: raw_flags & FLAG_IMAGE_FIRST_FRAME_ONLY != 0,
        display_name: display_name.to_owned(),
        format: ImageFormat::from_raw(read_u32(bytes, 24))?,
        source_width: read_u32(bytes, 28),
        source_height: read_u32(bytes, 32),
        width: read_u32(bytes, 36),
        height: read_u32(bytes, 40),
        premultiplied_bgra: bytes[display_name_end..].to_vec(),
    };
    validate_image(&preview)?;
    Ok(PreviewResult::Image(preview))
}

fn validate_text(preview: &TextPreview) -> Result<(), PayloadError> {
    validate_display_name(&preview.display_name)?;
    if preview.encoding.len() > MAX_ENCODING_LABEL_LEN {
        return Err(PayloadError::EncodingLabelTooLong {
            actual: preview.encoding.len(),
        });
    }
    if !is_encoding_label(&preview.encoding) {
        return Err(PayloadError::InvalidEncodingLabel);
    }
    if preview.text.len() > MAX_TEXT_UTF8_LEN {
        return Err(PayloadError::TextTooLarge {
            actual: preview.text.len(),
        });
    }

    let mut scalar_count = 0;
    let mut line_count = usize::from(!preview.text.is_empty());
    for scalar in preview.text.chars() {
        scalar_count += 1;
        if scalar_count > MAX_TEXT_SCALARS {
            return Err(PayloadError::TooManyTextScalars {
                actual: scalar_count,
            });
        }
        if scalar == '\n' {
            line_count += 1;
            if line_count > MAX_TEXT_LINES {
                return Err(PayloadError::TooManyTextLines { actual: line_count });
            }
        } else if is_noncanonical_text_line_break(scalar) || is_unsafe_text_control(scalar) {
            return Err(PayloadError::UnsafeTextScalar(u32::from(scalar)));
        }
    }
    Ok(())
}

pub const fn is_noncanonical_text_line_break(scalar: char) -> bool {
    matches!(scalar, '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}')
}

pub const fn is_unsafe_text_control(scalar: char) -> bool {
    matches!(
        scalar,
        '\u{0000}'..='\u{0008}'
            | '\u{000b}'..='\u{001f}'
            | '\u{007f}'..='\u{009f}'
            | '\u{061c}'
            | '\u{200e}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
    )
}

fn validate_image(preview: &ImagePreview) -> Result<(), PayloadError> {
    validate_display_name(&preview.display_name)?;
    if preview.source_width == 0
        || preview.source_height == 0
        || preview.source_width > MAX_SOURCE_IMAGE_AXIS
        || preview.source_height > MAX_SOURCE_IMAGE_AXIS
    {
        return Err(PayloadError::InvalidSourceDimensions {
            width: preview.source_width,
            height: preview.source_height,
        });
    }
    let source_pixels = u64::from(preview.source_width)
        .checked_mul(u64::from(preview.source_height))
        .ok_or(PayloadError::LengthOverflow)?;
    if source_pixels > MAX_SOURCE_IMAGE_PIXELS {
        return Err(PayloadError::TooManySourceImagePixels {
            actual: source_pixels,
        });
    }
    if preview.width == 0
        || preview.height == 0
        || preview.width > MAX_PREVIEW_IMAGE_WIDTH
        || preview.height > MAX_PREVIEW_IMAGE_HEIGHT
    {
        return Err(PayloadError::InvalidPreviewDimensions {
            width: preview.width,
            height: preview.height,
        });
    }
    if preview.width > preview.source_width || preview.height > preview.source_height {
        return Err(PayloadError::PreviewUpscalesSource);
    }
    let expected_dimensions =
        fitted_preview_dimensions(preview.source_width, preview.source_height)
            .ok_or(PayloadError::LengthOverflow)?;
    if (preview.width, preview.height) != expected_dimensions {
        return Err(PayloadError::NonFittingPreviewDimensions {
            expected_width: expected_dimensions.0,
            expected_height: expected_dimensions.1,
            actual_width: preview.width,
            actual_height: preview.height,
        });
    }

    let (_, expected) =
        checked_bgra_layout(preview.width, preview.height).map_err(|error| match error {
            LayoutError::InvalidDimensions { width, height } => {
                PayloadError::InvalidPreviewDimensions { width, height }
            }
            LayoutError::ArithmeticOverflow => PayloadError::LengthOverflow,
            LayoutError::PayloadTooLarge { actual } => PayloadError::PayloadTooLarge { actual },
        })?;
    if preview.premultiplied_bgra.len() != expected {
        return Err(PayloadError::InvalidPixelLength {
            expected,
            actual: preview.premultiplied_bgra.len(),
        });
    }
    if let Some((index, _)) = preview
        .premultiplied_bgra
        .chunks_exact(BGRA_BYTES_PER_PIXEL)
        .enumerate()
        .find(|(_, pixel)| pixel[0] > pixel[3] || pixel[1] > pixel[3] || pixel[2] > pixel[3])
    {
        return Err(PayloadError::NonPremultipliedPixel { index });
    }
    ensure_payload_cap(
        IMAGE_FIXED_LEN
            .checked_add(preview.display_name.len())
            .and_then(|length| length.checked_add(expected))
            .ok_or(PayloadError::LengthOverflow)?,
    )
}

fn validate_video(preview: &VideoPreview) -> Result<(), PayloadError> {
    validate_display_name(&preview.display_name)?;
    if preview.path.is_empty()
        || preview.path.len() > MAX_VIDEO_PATH_UNITS
        || preview.path.contains(&0)
        || String::from_utf16(&preview.path).is_err()
    {
        return Err(PayloadError::InvalidVideoPath);
    }
    let path_bytes = preview
        .path
        .len()
        .checked_mul(2)
        .ok_or(PayloadError::LengthOverflow)?;
    ensure_payload_cap(
        VIDEO_FIXED_LEN
            .checked_add(preview.display_name.len())
            .and_then(|length| length.checked_add(path_bytes))
            .ok_or(PayloadError::LengthOverflow)?,
    )
}

fn validate_display_name(value: &str) -> Result<(), PayloadError> {
    if value.len() > MAX_DISPLAY_NAME_UTF8_LEN {
        return Err(PayloadError::DisplayNameTooLong {
            actual: value.len(),
        });
    }
    if value.is_empty() || value.chars().any(is_unsafe_display_name_scalar) {
        return Err(PayloadError::InvalidDisplayName);
    }
    Ok(())
}

pub const fn is_unsafe_display_name_scalar(scalar: char) -> bool {
    matches!(scalar, '/' | '\\')
        || is_noncanonical_text_line_break(scalar)
        || is_unsafe_text_control(scalar)
}

fn ensure_payload_cap(length: usize) -> Result<(), PayloadError> {
    if length <= MAX_PREVIEW_PAYLOAD_LEN {
        Ok(())
    } else {
        Err(PayloadError::PayloadTooLarge { actual: length })
    }
}

fn is_encoding_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ENCODING_LABEL_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn flags(values: &[(bool, u32)]) -> u32 {
    values.iter().fold(0, |flags, (enabled, bit)| {
        flags | (u32::from(*enabled) * bit)
    })
}

fn reject_unknown_flags(kind: &'static str, actual: u32, allowed: u32) -> Result<(), PayloadError> {
    if actual & !allowed == 0 {
        Ok(())
    } else {
        Err(PayloadError::UnknownFlags { kind, actual })
    }
}

fn require_exact_length(
    kind: &'static str,
    actual: usize,
    expected: usize,
) -> Result<(), PayloadError> {
    if actual == expected {
        Ok(())
    } else {
        Err(PayloadError::PayloadLengthMismatch {
            kind,
            expected,
            actual,
        })
    }
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_i64(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("the variant length is checked before fixed metadata is read"),
    )
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("the variant length is checked before fixed metadata is read"),
    )
}

fn read_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("the variant length is checked before fixed metadata is read"),
    )
}

#[derive(Debug, Eq, PartialEq)]
pub enum PayloadError {
    PayloadLengthOutOfRange {
        actual: usize,
    },
    UnknownResultKind(u32),
    UnknownResolverStatus(u32),
    UnknownImageFormat(u32),
    UnknownFlags {
        kind: &'static str,
        actual: u32,
    },
    PayloadLengthMismatch {
        kind: &'static str,
        expected: usize,
        actual: usize,
    },
    LengthOverflow,
    EncodingLabelTooLong {
        actual: usize,
    },
    InvalidEncodingLabel,
    TextTooLarge {
        actual: usize,
    },
    TooManyTextScalars {
        actual: usize,
    },
    TooManyTextLines {
        actual: usize,
    },
    UnsafeTextScalar(u32),
    InvalidTextUtf8,
    DisplayNameTooLong {
        actual: usize,
    },
    InvalidDisplayName,
    InvalidVideoPath,
    InvalidSourceDimensions {
        width: u32,
        height: u32,
    },
    TooManySourceImagePixels {
        actual: u64,
    },
    InvalidPreviewDimensions {
        width: u32,
        height: u32,
    },
    PreviewUpscalesSource,
    NonFittingPreviewDimensions {
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    InvalidPixelLength {
        expected: usize,
        actual: usize,
    },
    NonPremultipliedPixel {
        index: usize,
    },
    PayloadTooLarge {
        actual: usize,
    },
}

impl fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadLengthOutOfRange { actual } => write!(
                formatter,
                "preview-result payload length {actual} is outside \
                 {MIN_PREVIEW_RESULT_LEN}-{MAX_PREVIEW_PAYLOAD_LEN}"
            ),
            Self::UnknownResultKind(kind) => {
                write!(formatter, "unknown preview result kind {kind}")
            }
            Self::UnknownResolverStatus(status) => {
                write!(formatter, "unknown resolver status {status}")
            }
            Self::UnknownImageFormat(format) => {
                write!(formatter, "unknown image format {format}")
            }
            Self::UnknownFlags { kind, actual } => {
                write!(formatter, "unknown {kind} preview flags 0x{actual:08x}")
            }
            Self::PayloadLengthMismatch {
                kind,
                expected,
                actual,
            } => write!(
                formatter,
                "{kind} preview payload length mismatch: expected {expected}, received {actual}"
            ),
            Self::LengthOverflow => write!(formatter, "preview payload length overflow"),
            Self::EncodingLabelTooLong { actual } => write!(
                formatter,
                "encoding label length {actual} exceeds {MAX_ENCODING_LABEL_LEN} bytes"
            ),
            Self::InvalidEncodingLabel => write!(formatter, "invalid preview encoding label"),
            Self::TextTooLarge { actual } => write!(
                formatter,
                "text preview length {actual} exceeds {MAX_TEXT_UTF8_LEN} UTF-8 bytes"
            ),
            Self::TooManyTextScalars { actual } => write!(
                formatter,
                "text preview contains at least {actual} Unicode scalars; the limit is \
                 {MAX_TEXT_SCALARS}"
            ),
            Self::TooManyTextLines { actual } => write!(
                formatter,
                "text preview contains at least {actual} lines; the limit is {MAX_TEXT_LINES}"
            ),
            Self::UnsafeTextScalar(scalar) => {
                write!(
                    formatter,
                    "text preview contains unsafe scalar U+{scalar:04X}"
                )
            }
            Self::InvalidTextUtf8 => write!(formatter, "text preview is not valid UTF-8"),
            Self::DisplayNameTooLong { actual } => write!(
                formatter,
                "display name length {actual} exceeds {MAX_DISPLAY_NAME_UTF8_LEN} UTF-8 bytes"
            ),
            Self::InvalidDisplayName => write!(formatter, "invalid preview display name"),
            Self::InvalidVideoPath => write!(formatter, "invalid preview video path"),
            Self::InvalidSourceDimensions { width, height } => write!(
                formatter,
                "invalid source image dimensions {width}x{height}"
            ),
            Self::TooManySourceImagePixels { actual } => write!(
                formatter,
                "source image has {actual} pixels; the limit is {MAX_SOURCE_IMAGE_PIXELS}"
            ),
            Self::InvalidPreviewDimensions { width, height } => {
                write!(
                    formatter,
                    "invalid preview image dimensions {width}x{height}"
                )
            }
            Self::PreviewUpscalesSource => {
                write!(formatter, "preview image dimensions upscale the source")
            }
            Self::NonFittingPreviewDimensions {
                expected_width,
                expected_height,
                actual_width,
                actual_height,
            } => write!(
                formatter,
                "preview image dimensions must be the bounded aspect fit \
                 {expected_width}x{expected_height}, received {actual_width}x{actual_height}"
            ),
            Self::InvalidPixelLength { expected, actual } => write!(
                formatter,
                "invalid premultiplied BGRA length: expected {expected}, received {actual}"
            ),
            Self::NonPremultipliedPixel { index } => write!(
                formatter,
                "BGRA pixel {index} has a color channel greater than alpha"
            ),
            Self::PayloadTooLarge { actual } => write!(
                formatter,
                "preview payload length {actual} exceeds {MAX_PREVIEW_PAYLOAD_LEN} bytes"
            ),
        }
    }
}

impl Error for PayloadError {}

#[cfg(test)]
mod tests {
    use super::{
        FLAG_IMAGE_FIRST_FRAME_ONLY, FLAG_TEXT_TRUNCATED, IMAGE_FIXED_LEN, ImageFormat,
        ImagePreview, MAX_DISPLAY_NAME_UTF8_LEN, MAX_ENCODING_LABEL_LEN, MAX_PREVIEW_IMAGE_WIDTH,
        MAX_SOURCE_IMAGE_AXIS, MAX_SOURCE_IMAGE_PIXELS, MAX_TEXT_LINES, MAX_TEXT_SCALARS,
        MAX_TEXT_UTF8_LEN, PayloadError, PreviewResult, ResolverStatus, TEXT_FIXED_LEN,
        TextPreview, VideoPreview, decode_result, encode_result,
    };

    fn text_preview() -> TextPreview {
        TextPreview {
            file_size: 91_234,
            last_write_time: 133_000_000_000_000_000,
            linked_content: true,
            encoding_was_guessed: true,
            truncated: false,
            display_name: "sample.txt".to_owned(),
            encoding: "windows-1252".to_owned(),
            text: "hello, 世界\n".to_owned(),
        }
    }

    fn image_preview() -> ImagePreview {
        ImagePreview {
            file_size: 45_678,
            last_write_time: 133_000_000_000_000_000,
            linked_content: false,
            first_frame_only: true,
            display_name: "sample.png".to_owned(),
            format: ImageFormat::Png,
            source_width: 3,
            source_height: 2,
            width: 3,
            height: 2,
            premultiplied_bgra: vec![0x7f; 24],
        }
    }

    fn video_preview() -> VideoPreview {
        VideoPreview {
            file_size: 1_234_567,
            last_write_time: 133_000_000_000_000_000,
            linked_content: false,
            display_name: "sample.mp4".to_owned(),
            path: r"C:\Videos\sample.mp4".encode_utf16().collect(),
        }
    }

    #[test]
    fn every_result_kind_round_trips_with_typed_metadata() {
        for status in [
            ResolverStatus::Resolved,
            ResolverStatus::Unsupported,
            ResolverStatus::Ambiguous,
            ResolverStatus::Unavailable,
            ResolverStatus::TimedOut,
            ResolverStatus::PointerMoved,
        ] {
            let result = PreviewResult::Status(status);
            assert_eq!(decode_result(&encode_result(&result).unwrap()), Ok(result));
        }

        let text = PreviewResult::Text(text_preview());
        assert_eq!(decode_result(&encode_result(&text).unwrap()), Ok(text));

        for format in [
            ImageFormat::Jpeg,
            ImageFormat::Png,
            ImageFormat::Gif,
            ImageFormat::WebP,
            ImageFormat::Bmp,
            ImageFormat::Ico,
            ImageFormat::Tiff,
        ] {
            let mut image = image_preview();
            image.format = format;
            let result = PreviewResult::Image(image);
            assert_eq!(decode_result(&encode_result(&result).unwrap()), Ok(result));
        }

        let video = PreviewResult::Video(video_preview());
        assert_eq!(decode_result(&encode_result(&video).unwrap()), Ok(video));
    }

    #[test]
    fn text_limits_and_labels_fail_closed() {
        let mut preview = text_preview();
        preview.display_name = "x".repeat(MAX_DISPLAY_NAME_UTF8_LEN + 1);
        assert_eq!(
            encode_result(&PreviewResult::Text(preview)),
            Err(PayloadError::DisplayNameTooLong {
                actual: MAX_DISPLAY_NAME_UTF8_LEN + 1
            })
        );

        for display_name in ["", "folder/name.txt", "before\u{202e}after.txt"] {
            let mut preview = text_preview();
            preview.display_name = display_name.to_owned();
            assert_eq!(
                encode_result(&PreviewResult::Text(preview)),
                Err(PayloadError::InvalidDisplayName)
            );
        }

        let mut preview = text_preview();
        preview.text = "x".repeat(MAX_TEXT_UTF8_LEN + 1);
        assert_eq!(
            encode_result(&PreviewResult::Text(preview)),
            Err(PayloadError::TextTooLarge {
                actual: MAX_TEXT_UTF8_LEN + 1
            })
        );

        let mut preview = text_preview();
        preview.encoding = "x".repeat(MAX_ENCODING_LABEL_LEN + 1);
        assert_eq!(
            encode_result(&PreviewResult::Text(preview)),
            Err(PayloadError::EncodingLabelTooLong {
                actual: MAX_ENCODING_LABEL_LEN + 1
            })
        );

        let mut preview = text_preview();
        preview.encoding = "utf 8".to_owned();
        assert_eq!(
            encode_result(&PreviewResult::Text(preview)),
            Err(PayloadError::InvalidEncodingLabel)
        );

        let mut preview = text_preview();
        preview.text = "世".repeat(MAX_TEXT_SCALARS + 1);
        assert_eq!(
            encode_result(&PreviewResult::Text(preview)),
            Err(PayloadError::TooManyTextScalars {
                actual: MAX_TEXT_SCALARS + 1
            })
        );

        let mut preview = text_preview();
        preview.text = "line\n".repeat(MAX_TEXT_LINES);
        assert_eq!(
            encode_result(&PreviewResult::Text(preview)),
            Err(PayloadError::TooManyTextLines {
                actual: MAX_TEXT_LINES + 1
            })
        );

        for scalar in ['\r', '\u{001b}', '\u{0085}', '\u{202e}', '\u{2066}'] {
            let mut preview = text_preview();
            preview.text = format!("before{scalar}after");
            assert_eq!(
                encode_result(&PreviewResult::Text(preview)),
                Err(PayloadError::UnsafeTextScalar(u32::from(scalar)))
            );
        }
    }

    #[test]
    fn image_dimensions_and_exact_bgra_length_fail_closed() {
        let mut preview = image_preview();
        preview.display_name = "x".repeat(MAX_DISPLAY_NAME_UTF8_LEN + 1);
        assert_eq!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::DisplayNameTooLong {
                actual: MAX_DISPLAY_NAME_UTF8_LEN + 1
            })
        );

        for display_name in ["", "folder/name.png", "before\u{202e}after.png"] {
            let mut preview = image_preview();
            preview.display_name = display_name.to_owned();
            assert_eq!(
                encode_result(&PreviewResult::Image(preview)),
                Err(PayloadError::InvalidDisplayName)
            );
        }

        let mut preview = image_preview();
        preview.source_width = MAX_SOURCE_IMAGE_AXIS + 1;
        assert!(matches!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::InvalidSourceDimensions { .. })
        ));

        let mut preview = image_preview();
        preview.source_width = MAX_SOURCE_IMAGE_AXIS;
        preview.source_height =
            u32::try_from(MAX_SOURCE_IMAGE_PIXELS / u64::from(MAX_SOURCE_IMAGE_AXIS) + 1).unwrap();
        assert!(matches!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::TooManySourceImagePixels { .. })
        ));

        let mut preview = image_preview();
        preview.width = MAX_PREVIEW_IMAGE_WIDTH + 1;
        assert!(matches!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::InvalidPreviewDimensions { .. })
        ));

        let mut preview = image_preview();
        preview.width = 4;
        assert_eq!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::PreviewUpscalesSource)
        );

        let mut preview = image_preview();
        preview.width = 2;
        preview.premultiplied_bgra = vec![0x7f; 16];
        assert_eq!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::NonFittingPreviewDimensions {
                expected_width: 3,
                expected_height: 2,
                actual_width: 2,
                actual_height: 2,
            })
        );

        let mut preview = image_preview();
        preview.premultiplied_bgra.pop();
        assert_eq!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::InvalidPixelLength {
                expected: 24,
                actual: 23
            })
        );

        let mut preview = image_preview();
        preview.premultiplied_bgra[..4].copy_from_slice(&[128, 127, 126, 127]);
        assert_eq!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::NonPremultipliedPixel { index: 0 })
        );
    }

    #[test]
    fn malformed_tags_flags_lengths_and_utf8_are_rejected() {
        let mut unknown_kind = vec![0; 8];
        unknown_kind[..4].copy_from_slice(&99_u32.to_le_bytes());
        assert_eq!(
            decode_result(&unknown_kind),
            Err(PayloadError::UnknownResultKind(99))
        );

        let mut bad_status =
            encode_result(&PreviewResult::Status(ResolverStatus::Resolved)).unwrap();
        bad_status[4..8].copy_from_slice(&99_u32.to_le_bytes());
        assert_eq!(
            decode_result(&bad_status),
            Err(PayloadError::UnknownResolverStatus(99))
        );

        let mut text = encode_result(&PreviewResult::Text(text_preview())).unwrap();
        text[4..8].copy_from_slice(&(FLAG_TEXT_TRUNCATED | 0x8000_0000).to_le_bytes());
        assert!(matches!(
            decode_result(&text),
            Err(PayloadError::UnknownFlags { kind: "text", .. })
        ));

        let mut text = encode_result(&PreviewResult::Text(text_preview())).unwrap();
        let display_name_len = u32::from_le_bytes(text[24..28].try_into().unwrap()) as usize;
        let encoding_len = u32::from_le_bytes(text[28..32].try_into().unwrap()) as usize;
        text[TEXT_FIXED_LEN + display_name_len + encoding_len] = 0xff;
        assert_eq!(decode_result(&text), Err(PayloadError::InvalidTextUtf8));

        let mut text = encode_result(&PreviewResult::Text(text_preview())).unwrap();
        text[TEXT_FIXED_LEN] = 0xff;
        assert_eq!(decode_result(&text), Err(PayloadError::InvalidDisplayName));

        let mut image = encode_result(&PreviewResult::Image(image_preview())).unwrap();
        image[4..8].copy_from_slice(&(FLAG_IMAGE_FIRST_FRAME_ONLY | 0x4000_0000).to_le_bytes());
        assert!(matches!(
            decode_result(&image),
            Err(PayloadError::UnknownFlags { kind: "image", .. })
        ));

        let mut image = encode_result(&PreviewResult::Image(image_preview())).unwrap();
        image[48..52].copy_from_slice(&23_u32.to_le_bytes());
        let display_name_len = image_preview().display_name.len();
        assert_eq!(
            decode_result(&image),
            Err(PayloadError::PayloadLengthMismatch {
                kind: "image",
                expected: IMAGE_FIXED_LEN + display_name_len + 23,
                actual: IMAGE_FIXED_LEN + display_name_len + 24
            })
        );

        let mut image = encode_result(&PreviewResult::Image(image_preview())).unwrap();
        image[IMAGE_FIXED_LEN] = 0xff;
        assert_eq!(decode_result(&image), Err(PayloadError::InvalidDisplayName));
    }

    #[test]
    fn video_paths_are_bounded_well_formed_utf16_without_embedded_nuls() {
        for path in [Vec::new(), vec![0], vec![0xd800]] {
            let mut preview = video_preview();
            preview.path = path;
            assert_eq!(
                encode_result(&PreviewResult::Video(preview)),
                Err(PayloadError::InvalidVideoPath)
            );
        }
    }

    #[test]
    fn minimum_and_embedded_lengths_are_checked_before_slicing() {
        assert_eq!(
            decode_result(&[0; 7]),
            Err(PayloadError::PayloadLengthOutOfRange { actual: 7 })
        );

        let mut short_text = vec![0; TEXT_FIXED_LEN - 1];
        short_text[..4].copy_from_slice(&1_u32.to_le_bytes());
        assert_eq!(
            decode_result(&short_text),
            Err(PayloadError::PayloadLengthMismatch {
                kind: "text",
                expected: TEXT_FIXED_LEN,
                actual: TEXT_FIXED_LEN - 1
            })
        );
    }
}
