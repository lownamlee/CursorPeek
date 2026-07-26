use super::{ImageDecodeResult, ImageValidationError, decode};
use crate::worker::{
    file::PreviewFile,
    payload::{ImageFormat, ImagePreview},
};
use image::{DynamicImage, GrayImage, ImageFormat as DecoderFormat, Luma, Rgba, RgbaImage};
use std::{
    env, fs, io,
    io::Cursor,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const MANIFEST: &str = include_str!("../../../corpus/images/cases.tsv");
const CHILD_CASE_ENV: &str = "CURSORPEEK_IMAGE_CORPUS_CASE";
const CHILD_TEST_NAME: &str = "worker::image::corpus::image_corpus_child";
const CORPUS_WATCHDOG: Duration = Duration::from_secs(6);
static NEXT_CORPUS_DIRECTORY: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
enum Generator {
    Sample(DecoderFormat),
    WidePng,
    StressPng,
    Truncated(DecoderFormat),
    Bytes(&'static [u8]),
    PngHeader {
        width: u32,
        height: u32,
        bit_depth: u8,
        color_type: u8,
    },
    CorruptPngHeaderCrc,
}

impl Generator {
    fn generate(self) -> Vec<u8> {
        match self {
            Self::Sample(format) => encode_dynamic(sample_image(), format),
            Self::WidePng => encode_dynamic(
                DynamicImage::ImageRgba8(RgbaImage::from_fn(1_000, 2, |x, _| {
                    if x < 500 {
                        Rgba([200, 100, 50, 128])
                    } else {
                        Rgba([255, 255, 255, 0])
                    }
                })),
                DecoderFormat::Png,
            ),
            Self::StressPng => encode_dynamic(
                DynamicImage::ImageLuma8(GrayImage::from_pixel(1_536, 1_536, Luma([7]))),
                DecoderFormat::Png,
            ),
            Self::Truncated(format) => {
                let mut bytes = encode_dynamic(sample_image(), format);
                bytes.truncate(format_prefix_len(format).min(bytes.len()));
                bytes
            }
            Self::Bytes(bytes) => bytes.to_vec(),
            Self::PngHeader {
                width,
                height,
                bit_depth,
                color_type,
            } => png_with_empty_pixels(width, height, bit_depth, color_type),
            Self::CorruptPngHeaderCrc => {
                let mut bytes = encode_dynamic(sample_image(), DecoderFormat::Png);
                bytes[29] ^= 1;
                bytes
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Expected {
    Preview {
        format: ImageFormat,
        source: (u32, u32),
        preview: (u32, u32),
        first_frame_only: bool,
        first_bgra: Option<[u8; 4]>,
    },
    Unsupported,
    FailClosed,
    ResourceRejected,
}

struct CorpusCase {
    id: &'static str,
    extension: &'static str,
    generator: Generator,
    expected: Expected,
    render: bool,
}

pub(crate) struct RenderCorpusCase {
    pub(crate) id: &'static str,
    pub(crate) preview: ImagePreview,
}

#[test]
fn generated_corpus_manifest_matches_every_executable_case() {
    let manifest_ids = MANIFEST
        .lines()
        .skip(1)
        .map(|line| {
            line.split_once('\t')
                .expect("each image corpus row must contain a tab")
                .0
        })
        .collect::<Vec<_>>();
    let executable_ids = corpus_cases()
        .into_iter()
        .map(|case| case.id)
        .collect::<Vec<_>>();

    assert_eq!(manifest_ids, executable_ids);
}

#[test]
fn generated_image_corpus_obeys_decode_and_resource_contracts() {
    let root = TestDirectory::new("contract");
    for case in corpus_cases() {
        let started = Instant::now();
        let input_len = case.generator.generate().len();
        let preview = run_case(&root, &case);
        if case.id == "png-decompression-stress" {
            assert!(input_len < 64 * 1024, "the stress PNG should stay compact");
            assert!(
                started.elapsed() < CORPUS_WATCHDOG,
                "the stress PNG exceeded the corpus watchdog"
            );
        }
        assert_eq!(
            preview.is_some(),
            matches!(case.expected, Expected::Preview { .. })
        );
    }
}

#[test]
fn image_corpus_child() {
    let Ok(id) = env::var(CHILD_CASE_ENV) else {
        return;
    };
    run_named_case(&id);
    println!("IMAGE_CORPUS_CHILD_OK={id}");
}

#[test]
fn stress_and_rejection_cases_finish_in_restartable_subprocesses() {
    for id in ["png-decompression-stress", "png-over-axis", "truncated-png"] {
        let mut child = Command::new(env::current_exe().expect("the test executable should exist"))
            .args([
                "--exact",
                CHILD_TEST_NAME,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_CASE_ENV, id)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the isolated image-corpus process should start");
        let deadline = Instant::now() + CORPUS_WATCHDOG;
        loop {
            match child
                .try_wait()
                .expect("the isolated image-corpus process should remain queryable")
            {
                Some(_) => break,
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("image corpus case `{id}` exceeded the subprocess watchdog");
                }
            }
        }
        let output = child
            .wait_with_output()
            .expect("the isolated image-corpus output should be collected");
        assert!(
            output.status.success(),
            "image corpus case `{id}` failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains(&format!("IMAGE_CORPUS_CHILD_OK={id}")),
            "image corpus case `{id}` did not execute in the child"
        );
    }
}

pub(crate) fn renderable_previews() -> Vec<RenderCorpusCase> {
    let root = TestDirectory::new("render");
    corpus_cases()
        .into_iter()
        .filter(|case| case.render)
        .map(|case| RenderCorpusCase {
            id: case.id,
            preview: run_case(&root, &case)
                .expect("render corpus cases must produce a preview payload"),
        })
        .collect()
}

fn run_named_case(id: &str) {
    let case = corpus_cases()
        .into_iter()
        .find(|case| case.id == id)
        .unwrap_or_else(|| panic!("unknown image corpus case `{id}`"));
    let root = TestDirectory::new(id);
    let _ = run_case(&root, &case);
}

fn run_case(root: &TestDirectory, case: &CorpusCase) -> Option<ImagePreview> {
    let bytes = case.generator.generate();
    let path = root.path().join(format!("{}.{}", case.id, case.extension));
    fs::write(&path, &bytes).expect("the generated image corpus case should be written");
    let file = PreviewFile::open(&path).expect("the generated image corpus case should open");
    let expected_linked_content = file.is_linked_content();
    let decoded = decode(&file);

    match case.expected {
        Expected::Preview {
            format,
            source,
            preview,
            first_frame_only,
            first_bgra,
        } => {
            let ImageDecodeResult::Decoded(decoded) =
                decoded.expect("a renderable corpus case should decode")
            else {
                panic!("corpus case `{}` should produce decoded pixels", case.id);
            };
            let actual = decoded
                .into_preview(&file)
                .expect("a renderable corpus case should produce bounded BGRA");
            assert_eq!(actual.format, format, "corpus case `{}` format", case.id);
            assert_eq!(
                (actual.source_width, actual.source_height),
                source,
                "corpus case `{}` source dimensions",
                case.id
            );
            assert_eq!(
                (actual.width, actual.height),
                preview,
                "corpus case `{}` preview dimensions",
                case.id
            );
            assert_eq!(
                actual.first_frame_only, first_frame_only,
                "corpus case `{}` still-frame policy",
                case.id
            );
            assert_eq!(actual.file_size, bytes.len() as u64);
            assert_eq!(
                actual.linked_content, expected_linked_content,
                "corpus case `{}` linked-content propagation",
                case.id
            );
            assert_eq!(
                actual.premultiplied_bgra.len(),
                usize::try_from(actual.width).unwrap()
                    * usize::try_from(actual.height).unwrap()
                    * 4
            );
            for pixel in actual.premultiplied_bgra.chunks_exact(4) {
                assert!(
                    pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3],
                    "corpus case `{}` produced straight-alpha color",
                    case.id
                );
            }
            if let Some(expected) = first_bgra {
                assert_eq!(&actual.premultiplied_bgra[..4], &expected);
            }
            Some(actual)
        }
        Expected::Unsupported => {
            assert!(
                matches!(decoded, Ok(ImageDecodeResult::Unsupported)),
                "corpus case `{}` must be unsupported",
                case.id
            );
            None
        }
        Expected::FailClosed => {
            assert!(
                !matches!(decoded, Ok(ImageDecodeResult::Decoded(_))),
                "corpus case `{}` must not produce pixels",
                case.id
            );
            None
        }
        Expected::ResourceRejected => {
            assert!(
                matches!(decoded, Err(ref error) if resource_rejection(error)),
                "corpus case `{}` must hit a declared resource limit, got {:?}",
                case.id,
                decoded.as_ref().err()
            );
            None
        }
    }
}

fn resource_rejection(error: &ImageValidationError) -> bool {
    matches!(
        error,
        ImageValidationError::InvalidDimensions { .. }
            | ImageValidationError::TooManyPixels { .. }
            | ImageValidationError::DecodedImageTooLarge { .. }
            | ImageValidationError::Decoder(image::ImageError::Limits(_))
    )
}

fn corpus_cases() -> Vec<CorpusCase> {
    let preview = |format, first_frame_only| Expected::Preview {
        format,
        source: (3, 2),
        preview: (3, 2),
        first_frame_only,
        first_bgra: None,
    };
    vec![
        case(
            "valid-jpeg",
            "jpg",
            Generator::Sample(DecoderFormat::Jpeg),
            preview(ImageFormat::Jpeg, false),
            false,
        ),
        case(
            "valid-png-alpha",
            "png",
            Generator::Sample(DecoderFormat::Png),
            Expected::Preview {
                format: ImageFormat::Png,
                source: (3, 2),
                preview: (3, 2),
                first_frame_only: true,
                first_bgra: Some([5, 3, 2, 40]),
            },
            true,
        ),
        case(
            "valid-gif",
            "gif",
            Generator::Sample(DecoderFormat::Gif),
            preview(ImageFormat::Gif, true),
            false,
        ),
        case(
            "valid-webp",
            "webp",
            Generator::Sample(DecoderFormat::WebP),
            preview(ImageFormat::WebP, true),
            false,
        ),
        case(
            "valid-bmp",
            "bmp",
            Generator::Sample(DecoderFormat::Bmp),
            preview(ImageFormat::Bmp, false),
            false,
        ),
        case(
            "valid-ico",
            "ico",
            Generator::Sample(DecoderFormat::Ico),
            preview(ImageFormat::Ico, false),
            false,
        ),
        case(
            "valid-tiff",
            "tiff",
            Generator::Sample(DecoderFormat::Tiff),
            preview(ImageFormat::Tiff, false),
            false,
        ),
        case(
            "png-downscale",
            "png",
            Generator::WidePng,
            Expected::Preview {
                format: ImageFormat::Png,
                source: (1_000, 2),
                preview: (960, 2),
                first_frame_only: true,
                first_bgra: None,
            },
            true,
        ),
        case(
            "png-decompression-stress",
            "png",
            Generator::StressPng,
            Expected::Preview {
                format: ImageFormat::Png,
                source: (1_536, 1_536),
                preview: (720, 720),
                first_frame_only: true,
                first_bgra: None,
            },
            true,
        ),
        case(
            "png-magic-jpeg-extension",
            "jpg",
            Generator::Sample(DecoderFormat::Png),
            preview(ImageFormat::Png, true),
            true,
        ),
        case(
            "png-unsupported-extension",
            "txt",
            Generator::Sample(DecoderFormat::Png),
            Expected::Unsupported,
            false,
        ),
        case(
            "unsupported-magic-png-extension",
            "png",
            Generator::Bytes(b"qoif\0\0\0\x01\0\0\0\x01\x04\0"),
            Expected::Unsupported,
            false,
        ),
        truncated("truncated-jpeg", "jpg", DecoderFormat::Jpeg),
        truncated("truncated-png", "png", DecoderFormat::Png),
        truncated("truncated-gif", "gif", DecoderFormat::Gif),
        truncated("truncated-webp", "webp", DecoderFormat::WebP),
        truncated("truncated-bmp", "bmp", DecoderFormat::Bmp),
        truncated("truncated-ico", "ico", DecoderFormat::Ico),
        truncated("truncated-tiff", "tiff", DecoderFormat::Tiff),
        case(
            "png-over-axis",
            "png",
            Generator::PngHeader {
                width: 20_001,
                height: 1,
                bit_depth: 8,
                color_type: 6,
            },
            Expected::ResourceRejected,
            false,
        ),
        case(
            "png-too-many-pixels",
            "png",
            Generator::PngHeader {
                width: 10_000,
                height: 4_001,
                bit_depth: 8,
                color_type: 0,
            },
            Expected::ResourceRejected,
            false,
        ),
        case(
            "png-decoded-too-large",
            "png",
            Generator::PngHeader {
                width: 6_000,
                height: 6_000,
                bit_depth: 16,
                color_type: 6,
            },
            Expected::ResourceRejected,
            false,
        ),
        case(
            "png-missing-pixels",
            "png",
            Generator::PngHeader {
                width: 2,
                height: 2,
                bit_depth: 8,
                color_type: 6,
            },
            Expected::FailClosed,
            false,
        ),
        case(
            "png-corrupt-header-crc",
            "png",
            Generator::CorruptPngHeaderCrc,
            Expected::FailClosed,
            false,
        ),
    ]
}

fn case(
    id: &'static str,
    extension: &'static str,
    generator: Generator,
    expected: Expected,
    render: bool,
) -> CorpusCase {
    CorpusCase {
        id,
        extension,
        generator,
        expected,
        render,
    }
}

fn truncated(id: &'static str, extension: &'static str, format: DecoderFormat) -> CorpusCase {
    case(
        id,
        extension,
        Generator::Truncated(format),
        Expected::FailClosed,
        false,
    )
}

fn sample_image() -> DynamicImage {
    DynamicImage::ImageRgba8(RgbaImage::from_fn(3, 2, |x, y| {
        Rgba([
            10 + u8::try_from(x).unwrap(),
            20 + u8::try_from(y).unwrap(),
            30,
            40 + u8::try_from(x + y).unwrap(),
        ])
    }))
}

fn encode_dynamic(image: DynamicImage, format: DecoderFormat) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, format)
        .expect("the generated corpus image should encode");
    output.into_inner()
}

const fn format_prefix_len(format: DecoderFormat) -> usize {
    match format {
        DecoderFormat::Jpeg => 3,
        DecoderFormat::Png => 8,
        DecoderFormat::Gif => 6,
        DecoderFormat::WebP => 12,
        DecoderFormat::Bmp => 2,
        DecoderFormat::Ico => 6,
        DecoderFormat::Tiff => 8,
        _ => unreachable!(),
    }
}

fn png_with_empty_pixels(width: u32, height: u32, bit_depth: u8, color_type: u8) -> Vec<u8> {
    let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
    push_png_chunk(&mut output, b"IHDR", &ihdr);
    push_png_chunk(
        &mut output,
        b"IDAT",
        &[0x78, 0x01, 0x01, 0, 0, 0xff, 0xff, 0, 0, 0, 1],
    );
    push_png_chunk(&mut output, b"IEND", &[]);
    output
}

fn push_png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(
        &u32::try_from(data.len())
            .expect("generated PNG chunks fit u32")
            .to_be_bytes(),
    );
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(kind.len() + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    output.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        for _ in 0..32 {
            let sequence = NEXT_CORPUS_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "cursorpeek-image-corpus-{label}-{}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("test directory `{}` failed: {error}", path.display()),
            }
        }
        panic!("could not reserve a unique image-corpus directory");
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0)
            .unwrap_or_else(|error| panic!("test cleanup `{}` failed: {error}", self.0.display()));
    }
}
