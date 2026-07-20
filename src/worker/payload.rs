use std::{error::Error, fmt};

pub(super) const MAX_PREVIEW_PAYLOAD_LEN: usize = 4 * 1024 * 1024;
pub(super) const MIN_PREVIEW_RESULT_LEN: usize = 8;
pub(super) const MAX_TEXT_UTF8_LEN: usize = 128 * 1024;
pub(super) const MAX_TEXT_SCALARS: usize = 32_000;
pub(super) const MAX_TEXT_LINES: usize = 200;
pub(super) const MAX_ENCODING_LABEL_LEN: usize = 40;
pub(super) const MAX_SOURCE_IMAGE_AXIS: u32 = 20_000;
pub(super) const MAX_PREVIEW_IMAGE_WIDTH: u32 = 960;
pub(super) const MAX_PREVIEW_IMAGE_HEIGHT: u32 = 720;

const RESULT_STATUS: u32 = 0;
const RESULT_TEXT: u32 = 1;
const RESULT_IMAGE: u32 = 2;
const STATUS_PAYLOAD_LEN: usize = 8;
const TEXT_FIXED_LEN: usize = 24;
const IMAGE_FIXED_LEN: usize = 40;
const BGRA_BYTES_PER_PIXEL: usize = 4;

const FLAG_LINKED_CONTENT: u32 = 1 << 0;
const FLAG_TEXT_ENCODING_GUESSED: u32 = 1 << 1;
const FLAG_TEXT_TRUNCATED: u32 = 1 << 2;
const TEXT_FLAGS: u32 = FLAG_LINKED_CONTENT | FLAG_TEXT_ENCODING_GUESSED | FLAG_TEXT_TRUNCATED;
const FLAG_IMAGE_FIRST_FRAME_ONLY: u32 = 1 << 1;
const IMAGE_FLAGS: u32 = FLAG_LINKED_CONTENT | FLAG_IMAGE_FIRST_FRAME_ONLY;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResolverStatus {
    Resolved = 0,
    Unsupported = 1,
    Ambiguous = 2,
    Unavailable = 3,
    TimedOut = 4,
}

impl ResolverStatus {
    fn from_raw(value: u32) -> Result<Self, PayloadError> {
        match value {
            0 => Ok(Self::Resolved),
            1 => Ok(Self::Unsupported),
            2 => Ok(Self::Ambiguous),
            3 => Ok(Self::Unavailable),
            4 => Ok(Self::TimedOut),
            _ => Err(PayloadError::UnknownResolverStatus(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ImageFormat {
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
pub(super) struct TextPreview {
    pub(super) file_size: u64,
    pub(super) linked_content: bool,
    pub(super) encoding_was_guessed: bool,
    pub(super) truncated: bool,
    pub(super) encoding: String,
    pub(super) text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ImagePreview {
    pub(super) file_size: u64,
    pub(super) linked_content: bool,
    pub(super) first_frame_only: bool,
    pub(super) format: ImageFormat,
    pub(super) source_width: u32,
    pub(super) source_height: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) premultiplied_bgra: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PreviewResult {
    Status(ResolverStatus),
    Text(TextPreview),
    Image(ImagePreview),
}

impl PreviewResult {
    pub(super) const fn status(&self) -> Option<ResolverStatus> {
        match self {
            Self::Status(status) => Some(*status),
            Self::Text(_) | Self::Image(_) => None,
        }
    }
}

pub(super) fn encode_result(result: &PreviewResult) -> Result<Vec<u8>, PayloadError> {
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
                .checked_add(preview.encoding.len())
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
            push_u32(
                &mut output,
                u32::try_from(preview.encoding.len())
                    .expect("the encoding label cap fits the wire length"),
            );
            push_u32(
                &mut output,
                u32::try_from(preview.text.len()).expect("the text cap fits the wire length"),
            );
            output.extend_from_slice(preview.encoding.as_bytes());
            output.extend_from_slice(preview.text.as_bytes());
        }
        PreviewResult::Image(preview) => {
            validate_image(preview)?;
            let payload_len = IMAGE_FIXED_LEN
                .checked_add(preview.premultiplied_bgra.len())
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
            push_u32(&mut output, preview.format as u32);
            push_u32(&mut output, preview.source_width);
            push_u32(&mut output, preview.source_height);
            push_u32(&mut output, preview.width);
            push_u32(&mut output, preview.height);
            push_u32(
                &mut output,
                u32::try_from(preview.premultiplied_bgra.len())
                    .expect("the image payload cap fits the wire length"),
            );
            output.extend_from_slice(&preview.premultiplied_bgra);
        }
    }
    Ok(output)
}

pub(super) fn decode_result(bytes: &[u8]) -> Result<PreviewResult, PayloadError> {
    if !(MIN_PREVIEW_RESULT_LEN..=MAX_PREVIEW_PAYLOAD_LEN).contains(&bytes.len()) {
        return Err(PayloadError::PayloadLengthOutOfRange {
            actual: bytes.len(),
        });
    }

    match read_u32(bytes, 0) {
        RESULT_STATUS => decode_status(bytes),
        RESULT_TEXT => decode_text(bytes),
        RESULT_IMAGE => decode_image(bytes),
        kind => Err(PayloadError::UnknownResultKind(kind)),
    }
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
    let encoding_len = read_u32(bytes, 16) as usize;
    let text_len = read_u32(bytes, 20) as usize;
    let expected = TEXT_FIXED_LEN
        .checked_add(encoding_len)
        .and_then(|length| length.checked_add(text_len))
        .ok_or(PayloadError::LengthOverflow)?;
    require_exact_length("text", bytes.len(), expected)?;
    if encoding_len > MAX_ENCODING_LABEL_LEN {
        return Err(PayloadError::EncodingLabelTooLong {
            actual: encoding_len,
        });
    }
    if text_len > MAX_TEXT_UTF8_LEN {
        return Err(PayloadError::TextTooLarge { actual: text_len });
    }

    let encoding_end = TEXT_FIXED_LEN + encoding_len;
    let encoding = std::str::from_utf8(&bytes[TEXT_FIXED_LEN..encoding_end])
        .map_err(|_| PayloadError::InvalidEncodingLabel)?;
    if !is_encoding_label(encoding) {
        return Err(PayloadError::InvalidEncodingLabel);
    }
    let text =
        std::str::from_utf8(&bytes[encoding_end..]).map_err(|_| PayloadError::InvalidTextUtf8)?;
    let preview = TextPreview {
        file_size: read_u64(bytes, 8),
        linked_content: raw_flags & FLAG_LINKED_CONTENT != 0,
        encoding_was_guessed: raw_flags & FLAG_TEXT_ENCODING_GUESSED != 0,
        truncated: raw_flags & FLAG_TEXT_TRUNCATED != 0,
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
    let pixel_len = read_u32(bytes, 36) as usize;
    let expected = IMAGE_FIXED_LEN
        .checked_add(pixel_len)
        .ok_or(PayloadError::LengthOverflow)?;
    require_exact_length("image", bytes.len(), expected)?;

    let preview = ImagePreview {
        file_size: read_u64(bytes, 8),
        linked_content: raw_flags & FLAG_LINKED_CONTENT != 0,
        first_frame_only: raw_flags & FLAG_IMAGE_FIRST_FRAME_ONLY != 0,
        format: ImageFormat::from_raw(read_u32(bytes, 16))?,
        source_width: read_u32(bytes, 20),
        source_height: read_u32(bytes, 24),
        width: read_u32(bytes, 28),
        height: read_u32(bytes, 32),
        premultiplied_bgra: bytes[IMAGE_FIXED_LEN..].to_vec(),
    };
    validate_image(&preview)?;
    Ok(PreviewResult::Image(preview))
}

fn validate_text(preview: &TextPreview) -> Result<(), PayloadError> {
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

pub(super) const fn is_noncanonical_text_line_break(scalar: char) -> bool {
    matches!(scalar, '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}')
}

pub(super) const fn is_unsafe_text_control(scalar: char) -> bool {
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

    let expected = usize::try_from(preview.width)
        .ok()
        .and_then(|width| width.checked_mul(BGRA_BYTES_PER_PIXEL))
        .and_then(|stride| {
            usize::try_from(preview.height)
                .ok()
                .and_then(|height| stride.checked_mul(height))
        })
        .ok_or(PayloadError::LengthOverflow)?;
    if preview.premultiplied_bgra.len() != expected {
        return Err(PayloadError::InvalidPixelLength {
            expected,
            actual: preview.premultiplied_bgra.len(),
        });
    }
    ensure_payload_cap(
        IMAGE_FIXED_LEN
            .checked_add(expected)
            .ok_or(PayloadError::LengthOverflow)?,
    )
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

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PayloadError {
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
    InvalidSourceDimensions {
        width: u32,
        height: u32,
    },
    InvalidPreviewDimensions {
        width: u32,
        height: u32,
    },
    PreviewUpscalesSource,
    InvalidPixelLength {
        expected: usize,
        actual: usize,
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
            Self::InvalidSourceDimensions { width, height } => write!(
                formatter,
                "invalid source image dimensions {width}x{height}"
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
            Self::InvalidPixelLength { expected, actual } => write!(
                formatter,
                "invalid premultiplied BGRA length: expected {expected}, received {actual}"
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
        ImagePreview, MAX_ENCODING_LABEL_LEN, MAX_PREVIEW_IMAGE_WIDTH, MAX_SOURCE_IMAGE_AXIS,
        MAX_TEXT_LINES, MAX_TEXT_SCALARS, MAX_TEXT_UTF8_LEN, PayloadError, PreviewResult,
        ResolverStatus, TEXT_FIXED_LEN, TextPreview, decode_result, encode_result,
    };

    fn text_preview() -> TextPreview {
        TextPreview {
            file_size: 91_234,
            linked_content: true,
            encoding_was_guessed: true,
            truncated: false,
            encoding: "windows-1252".to_owned(),
            text: "hello, 世界\n".to_owned(),
        }
    }

    fn image_preview() -> ImagePreview {
        ImagePreview {
            file_size: 45_678,
            linked_content: false,
            first_frame_only: true,
            format: ImageFormat::Png,
            source_width: 3,
            source_height: 2,
            width: 2,
            height: 2,
            premultiplied_bgra: vec![0x7f; 16],
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
    }

    #[test]
    fn text_limits_and_labels_fail_closed() {
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
        preview.source_width = MAX_SOURCE_IMAGE_AXIS + 1;
        assert!(matches!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::InvalidSourceDimensions { .. })
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
        preview.premultiplied_bgra.pop();
        assert_eq!(
            encode_result(&PreviewResult::Image(preview)),
            Err(PayloadError::InvalidPixelLength {
                expected: 16,
                actual: 15
            })
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
        let encoding_len = u32::from_le_bytes(text[16..20].try_into().unwrap()) as usize;
        text[TEXT_FIXED_LEN + encoding_len] = 0xff;
        assert_eq!(decode_result(&text), Err(PayloadError::InvalidTextUtf8));

        let mut image = encode_result(&PreviewResult::Image(image_preview())).unwrap();
        image[4..8].copy_from_slice(&(FLAG_IMAGE_FIRST_FRAME_ONLY | 0x4000_0000).to_le_bytes());
        assert!(matches!(
            decode_result(&image),
            Err(PayloadError::UnknownFlags { kind: "image", .. })
        ));

        let mut image = encode_result(&PreviewResult::Image(image_preview())).unwrap();
        image[36..40].copy_from_slice(&15_u32.to_le_bytes());
        assert_eq!(
            decode_result(&image),
            Err(PayloadError::PayloadLengthMismatch {
                kind: "image",
                expected: IMAGE_FIXED_LEN + 15,
                actual: IMAGE_FIXED_LEN + 16
            })
        );
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
