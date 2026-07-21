use std::{error::Error, fmt, io::BufReader, path::Path};

use ::image::{
    ImageDecoder, ImageError, ImageFormat as DecoderFormat, Limits,
    codecs::{
        bmp::BmpDecoder, gif::GifDecoder, ico::IcoDecoder, jpeg::JpegDecoder, png::PngDecoder,
        tiff::TiffDecoder, webp::WebPDecoder,
    },
};

use super::{
    file::{PreviewFile, PreviewFileError},
    payload::{
        BGRA_BYTES_PER_PIXEL, IMAGE_FIXED_LEN, ImageFormat, MAX_PREVIEW_IMAGE_HEIGHT,
        MAX_PREVIEW_IMAGE_WIDTH, MAX_PREVIEW_PAYLOAD_LEN, MAX_SOURCE_IMAGE_AXIS,
        MAX_SOURCE_IMAGE_PIXELS,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ImageValidationResult {
    Validated(ValidatedImage),
    Unsupported,
}

pub(super) fn validate(file: &PreviewFile) -> Result<ImageValidationResult, ImageValidationError> {
    if !is_eligible_path(file.final_path()) {
        return Ok(ImageValidationResult::Unsupported);
    }
    if file.file_size() > MAX_IMAGE_FILE_BYTES {
        return Err(ImageValidationError::FileTooLarge {
            actual: file.file_size(),
        });
    }

    let prefix = file.read_prefix(MAGIC_PREFIX_LEN)?;
    let Ok(decoder_format) = ::image::guess_format(&prefix) else {
        return Ok(ImageValidationResult::Unsupported);
    };
    let Some(format) = supported_format(decoder_format) else {
        return Ok(ImageValidationResult::Unsupported);
    };
    if !decoder_format.reading_enabled() {
        return Ok(ImageValidationResult::Unsupported);
    }

    let (source_width, source_height, decoded_bytes) = inspect_header(
        BufReader::new(file.duplicate_reader()?),
        decoder_format,
        decoder_limits(),
    )
    .map_err(ImageValidationError::Decoder)?;

    validate_source_layout(source_width, source_height, decoded_bytes)?;
    checked_bgra_layout(
        source_width.min(MAX_PREVIEW_IMAGE_WIDTH),
        source_height.min(MAX_PREVIEW_IMAGE_HEIGHT),
    )?;
    if !file.is_unchanged()? {
        return Err(ImageValidationError::File(
            PreviewFileError::ChangedDuringRead,
        ));
    }

    Ok(ImageValidationResult::Validated(ValidatedImage {
        format,
        source_width,
        source_height,
        decoded_bytes,
    }))
}

fn inspect_header<R>(
    reader: R,
    format: DecoderFormat,
    limits: Limits,
) -> Result<(u32, u32, u64), ImageError>
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

fn inspect_decoder(
    mut decoder: impl ImageDecoder,
    limits: Limits,
) -> Result<(u32, u32, u64), ImageError> {
    decoder.set_limits(limits)?;
    let (width, height) = decoder.dimensions();
    Ok((width, height, decoder.total_bytes()))
}

fn decoder_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_IMAGE_AXIS);
    limits.max_image_height = Some(MAX_SOURCE_IMAGE_AXIS);
    limits.max_alloc = Some(MAX_DECODER_ALLOC_BYTES);
    limits
}

fn is_eligible_path(path: &Path) -> bool {
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
    FileTooLarge { actual: u64 },
    InvalidDimensions { width: u32, height: u32 },
    TooManyPixels { actual: u64 },
    DecodedImageTooLarge { actual: u64 },
    InvalidPreviewDimensions { width: u32, height: u32 },
    PreviewPayloadTooLarge { actual: usize },
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
            Self::Decoder(error) => write!(formatter, "image header validation failed: {error}"),
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

#[cfg(test)]
mod tests {
    use super::{
        IMAGE_EXTENSIONS, ImageValidationError, ImageValidationResult, MAX_DECODED_IMAGE_BYTES,
        MAX_DECODER_ALLOC_BYTES, MAX_IMAGE_FILE_BYTES, checked_bgra_layout, decoder_limits,
        is_eligible_path, validate, validate_source_layout,
    };
    use crate::worker::{
        file::PreviewFile,
        payload::{
            ImageFormat, MAX_PREVIEW_IMAGE_HEIGHT, MAX_PREVIEW_IMAGE_WIDTH, MAX_SOURCE_IMAGE_AXIS,
            MAX_SOURCE_IMAGE_PIXELS,
        },
    };
    use image::{DynamicImage, ImageFormat as DecoderFormat};
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

    fn encoded(format: DecoderFormat) -> Vec<u8> {
        let image = DynamicImage::new_rgba8(3, 2);
        let mut output = Cursor::new(Vec::new());
        image
            .write_to(&mut output, format)
            .expect("the generated image should encode");
        output.into_inner()
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
    fn every_selected_magic_format_produces_bounded_header_metadata() {
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
            assert!(validated.decoded_bytes > 0);
            assert!(validated.decoded_bytes <= MAX_DECODED_IMAGE_BYTES);
        }
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

        let unsupported_extension = root.path().join("sample.bin");
        fs::write(&unsupported_extension, encoded(DecoderFormat::Png)).unwrap();
        let file = PreviewFile::open(&unsupported_extension).unwrap();
        assert_eq!(validate(&file).unwrap(), ImageValidationResult::Unsupported);

        let unsupported_magic = root.path().join("sample.png");
        fs::write(&unsupported_magic, b"qoif\0\0\0\x01\0\0\0\x01\x04\0").unwrap();
        let file = PreviewFile::open(&unsupported_magic).unwrap();
        assert_eq!(validate(&file).unwrap(), ImageValidationResult::Unsupported);
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
