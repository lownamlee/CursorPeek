mod cache;
mod file;
mod image;
mod manager;
mod text;

mod payload {
    pub(crate) use cursorpeek_core::payload::*;
}

mod protocol {
    pub(crate) use cursorpeek_core::protocol::*;
}

use std::{
    error::Error,
    fmt,
    io::{Read, Write},
};

use crate::resolver::{PointResolver, ResolveOutcome};
use crate::settings::LegacyEncoding;
use cache::{PreviewCache, PreviewCacheKey, PreviewProvider};
use file::PreviewFile;
use image::ImageDecodeResult;
use payload::ResolverStatus;
use protocol::{ProtocolStreamError, WorkerMessage};
use text::TextDecodeResult;

#[cfg(test)]
pub(crate) use image::corpus::renderable_previews as image_corpus_previews;
pub(crate) use manager::{
    CompletionNotifier, PendingWorkerPoll, PendingWorkerResolution, WorkerManager,
    WorkerManagerError, run_launch_diagnostic, run_timeout_diagnostic,
};
#[cfg(test)]
pub(crate) use payload::ImageFormat;
pub(crate) use payload::{ImagePreview, PreviewResult, TextPreview};

pub(crate) fn run_session<R, W>(
    reader: &mut R,
    writer: &mut W,
    resolver: &mut impl PointResolver,
) -> Result<(), WorkerSessionError>
where
    R: Read,
    W: Write,
{
    let (nonce, legacy_encoding) = match protocol::read_message(reader)? {
        Some(WorkerMessage::Hello {
            nonce,
            legacy_encoding,
        }) => (nonce, legacy_encoding),
        Some(_) => return Err(WorkerSessionError::ExpectedHello),
        None => return Err(WorkerSessionError::MissingHello),
    };
    protocol::write_message(writer, WorkerMessage::Ready { nonce })?;
    let mut cache = PreviewCache::default();

    loop {
        let (generation, point) = match protocol::read_message(reader)? {
            Some(WorkerMessage::ResolvePoint { generation, point }) => (generation, point),
            Some(_) => return Err(WorkerSessionError::ExpectedResolvePoint),
            None => return Ok(()),
        };
        let result =
            resolver_result_with_cache(resolver.resolve(point), &legacy_encoding, &mut cache);
        protocol::write_message(writer, WorkerMessage::PreviewResult { generation, result })?;
    }
}

fn resolver_result_with_cache(
    outcome: ResolveOutcome,
    legacy_encoding: &LegacyEncoding,
    cache: &mut PreviewCache,
) -> PreviewResult {
    match outcome {
        ResolveOutcome::Resolved(target) => match PreviewFile::open(target.path()) {
            Ok(file) => preview_file_result(&file, legacy_encoding, cache),
            Err(error) if error.is_unsupported() => {
                PreviewResult::Status(ResolverStatus::Unsupported)
            }
            Err(_) => PreviewResult::Status(ResolverStatus::Unavailable),
        },
        ResolveOutcome::Unsupported => PreviewResult::Status(ResolverStatus::Unsupported),
        ResolveOutcome::Ambiguous => PreviewResult::Status(ResolverStatus::Ambiguous),
        ResolveOutcome::Unavailable => PreviewResult::Status(ResolverStatus::Unavailable),
    }
}

fn preview_file_result(
    file: &PreviewFile,
    legacy_encoding: &LegacyEncoding,
    cache: &mut PreviewCache,
) -> PreviewResult {
    let Some(provider) = PreviewProvider::for_path(file.final_path()) else {
        return PreviewResult::Status(ResolverStatus::Unsupported);
    };
    let key = PreviewCacheKey::new(file, provider, legacy_encoding);
    match file.is_unchanged() {
        Ok(true) => {}
        Ok(false) | Err(_) => return PreviewResult::Status(ResolverStatus::Unavailable),
    }
    if let Some(mut result) = cache.get(&key) {
        if let PreviewResult::Image(preview) = &mut result {
            preview.display_name = file.display_name();
            preview.last_write_time = file.last_write_time();
        }
        return match file.is_unchanged() {
            Ok(true) => result,
            Ok(false) | Err(_) => PreviewResult::Status(ResolverStatus::Unavailable),
        };
    }

    let result = match provider {
        PreviewProvider::Text => match text::decode(file, legacy_encoding) {
            Ok(TextDecodeResult::Preview(preview)) => PreviewResult::Text(preview),
            Ok(TextDecodeResult::Unsupported) => PreviewResult::Status(ResolverStatus::Unsupported),
            Err(_) => PreviewResult::Status(ResolverStatus::Unavailable),
        },
        PreviewProvider::Image => match image::decode(file) {
            Ok(ImageDecodeResult::Decoded(decoded)) => match decoded.into_preview(file) {
                Ok(preview) => PreviewResult::Image(preview),
                Err(error) if error.is_unsupported() => {
                    PreviewResult::Status(ResolverStatus::Unsupported)
                }
                Err(_) => PreviewResult::Status(ResolverStatus::Unavailable),
            },
            Ok(ImageDecodeResult::Unsupported) => {
                PreviewResult::Status(ResolverStatus::Unsupported)
            }
            Err(error) if error.is_unsupported() => {
                PreviewResult::Status(ResolverStatus::Unsupported)
            }
            Err(_) => PreviewResult::Status(ResolverStatus::Unavailable),
        },
    };
    cache.insert(key, result.clone());
    result
}

#[cfg(test)]
fn resolver_result(outcome: ResolveOutcome, legacy_encoding: &LegacyEncoding) -> PreviewResult {
    resolver_result_with_cache(outcome, legacy_encoding, &mut PreviewCache::default())
}

#[derive(Debug)]
pub(crate) enum WorkerSessionError {
    Stream(ProtocolStreamError),
    MissingHello,
    ExpectedHello,
    ExpectedResolvePoint,
}

impl fmt::Display for WorkerSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stream(error) => write!(formatter, "{error}"),
            Self::MissingHello => write!(formatter, "input closed before the hello frame"),
            Self::ExpectedHello => write!(formatter, "the first frame was not hello"),
            Self::ExpectedResolvePoint => {
                write!(formatter, "a post-handshake frame was not resolve-point")
            }
        }
    }
}

impl Error for WorkerSessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Stream(error) => Some(error),
            Self::MissingHello | Self::ExpectedHello | Self::ExpectedResolvePoint => None,
        }
    }
}

impl From<ProtocolStreamError> for WorkerSessionError {
    fn from(error: ProtocolStreamError) -> Self {
        Self::Stream(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WorkerSessionError, cache::PreviewCache, file::PreviewFile, protocol, resolver_result,
        resolver_result_with_cache, run_session,
    };
    use crate::hover::{Generation, PhysicalScreenPoint};
    use crate::resolver::{PointResolver, ResolveOutcome, ResolvedTarget};
    use crate::settings::LegacyEncoding;
    use crate::worker::payload::{PreviewResult, ResolverStatus};
    use ::image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    use protocol::{SessionNonce, WorkerMessage};
    use std::{
        env, fs,
        io::Cursor,
        sync::atomic::{AtomicU64, Ordering},
    };

    const NONCE: SessionNonce = SessionNonce::from_bytes([
        0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23,
        0x01,
    ]);
    static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(1);

    struct UnavailableResolver;

    impl PointResolver for UnavailableResolver {
        fn resolve(&mut self, _point: PhysicalScreenPoint) -> ResolveOutcome {
            ResolveOutcome::Unavailable
        }
    }

    #[test]
    fn resolver_outcomes_map_to_typed_preview_statuses() {
        let path = env::temp_dir().join(format!(
            "cursorpeek-resolved-target-{}-{}.txt",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, b"preview").expect("the resolved fixture should be written");
        let expected_linked_content = PreviewFile::open(&path)
            .expect("the resolved fixture should open")
            .is_linked_content();
        let PreviewResult::Text(preview) = resolver_result(
            ResolveOutcome::Resolved(ResolvedTarget::new(path.clone())),
            &LegacyEncoding::Auto,
        ) else {
            panic!("the resolved UTF-8 fixture should produce a text preview");
        };
        assert_eq!(preview.text, "preview");
        assert_eq!(preview.encoding, "UTF-8");
        assert_eq!(preview.file_size, 7);
        assert_eq!(preview.linked_content, expected_linked_content);
        assert!(!preview.encoding_was_guessed);
        assert!(!preview.truncated);
        fs::remove_file(&path).expect("the resolved fixture should be removed");
        assert_eq!(
            resolver_result(
                ResolveOutcome::Resolved(ResolvedTarget::new(path)),
                &LegacyEncoding::Auto,
            ),
            PreviewResult::Status(ResolverStatus::Unavailable)
        );
        let binary_path = env::temp_dir().join(format!(
            "cursorpeek-resolved-target-{}-{}.txt",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&binary_path, b"\x89PNG\r\n\x1a\npayload")
            .expect("the disguised binary fixture should be written");
        assert_eq!(
            resolver_result(
                ResolveOutcome::Resolved(ResolvedTarget::new(binary_path.clone())),
                &LegacyEncoding::Auto,
            ),
            PreviewResult::Status(ResolverStatus::Unsupported)
        );
        fs::remove_file(binary_path).expect("the disguised binary fixture should be removed");

        let malformed_image_path = env::temp_dir().join(format!(
            "cursorpeek-resolved-target-{}-{}.png",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&malformed_image_path, b"\x89PNG\r\n\x1a\n")
            .expect("the malformed image fixture should be written");
        assert_eq!(
            resolver_result(
                ResolveOutcome::Resolved(ResolvedTarget::new(malformed_image_path.clone())),
                &LegacyEncoding::Auto,
            ),
            PreviewResult::Status(ResolverStatus::Unavailable)
        );
        fs::remove_file(malformed_image_path)
            .expect("the malformed image fixture should be removed");

        let image_path = env::temp_dir().join(format!(
            "cursorpeek-resolved-target-{}-{}.png",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 1, Rgba([120, 80, 40, 128])));
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("the valid image fixture should encode");
        fs::write(&image_path, encoded.into_inner())
            .expect("the valid image fixture should be written");
        let PreviewResult::Image(preview) = resolver_result(
            ResolveOutcome::Resolved(ResolvedTarget::new(image_path.clone())),
            &LegacyEncoding::Auto,
        ) else {
            panic!("the valid PNG should produce an image preview");
        };
        assert_eq!((preview.source_width, preview.source_height), (2, 1));
        assert_eq!((preview.width, preview.height), (2, 1));
        assert_eq!(
            preview.display_name,
            image_path.file_name().unwrap().to_string_lossy()
        );
        assert!(preview.last_write_time > 0);
        assert_eq!(
            preview.premultiplied_bgra,
            [20, 40, 60, 128, 20, 40, 60, 128]
        );
        fs::remove_file(image_path).expect("the valid image fixture should be removed");

        let legacy_path = env::temp_dir().join(format!(
            "cursorpeek-resolved-target-{}-{}.txt",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&legacy_path, b"I\x92").expect("the legacy text fixture should be written");
        let PreviewResult::Text(preview) = resolver_result(
            ResolveOutcome::Resolved(ResolvedTarget::new(legacy_path.clone())),
            &LegacyEncoding::Auto,
        ) else {
            panic!("the detected legacy fixture should produce a text preview");
        };
        assert_eq!(preview.text, "I’");
        assert_eq!(preview.encoding, "windows-1252");
        assert!(preview.encoding_was_guessed);
        assert_eq!(
            resolver_result(
                ResolveOutcome::Resolved(ResolvedTarget::new(legacy_path.clone())),
                &LegacyEncoding::Off,
            ),
            PreviewResult::Status(ResolverStatus::Unsupported)
        );
        fs::remove_file(legacy_path).expect("the legacy text fixture should be removed");

        assert_eq!(
            resolver_result(ResolveOutcome::Unsupported, &LegacyEncoding::Auto),
            PreviewResult::Status(ResolverStatus::Unsupported)
        );
        assert_eq!(
            resolver_result(ResolveOutcome::Ambiguous, &LegacyEncoding::Auto),
            PreviewResult::Status(ResolverStatus::Ambiguous)
        );
        assert_eq!(
            resolver_result(ResolveOutcome::Unavailable, &LegacyEncoding::Auto),
            PreviewResult::Status(ResolverStatus::Unavailable)
        );
    }

    #[test]
    fn verified_preview_cache_hits_and_file_changes_miss() {
        let path = env::temp_dir().join(format!(
            "cursorpeek-cache-session-{}-{}.txt",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, b"first").expect("the first cache fixture should be written");
        let mut cache = PreviewCache::default();
        let resolve = |cache: &mut PreviewCache| {
            resolver_result_with_cache(
                ResolveOutcome::Resolved(ResolvedTarget::new(path.clone())),
                &LegacyEncoding::Auto,
                cache,
            )
        };

        let first = resolve(&mut cache);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.hit_count(), 0);
        assert_eq!(resolve(&mut cache), first);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.hit_count(), 1);

        fs::write(&path, b"second version").expect("the changed cache fixture should be written");
        let PreviewResult::Text(changed) = resolve(&mut cache) else {
            panic!("the changed file should still produce text");
        };
        assert_eq!(changed.text, "second version");
        assert_eq!(cache.hit_count(), 1);
        assert_eq!(cache.len(), 2);

        fs::remove_file(path).expect("the cache fixture should be removed");
    }

    #[test]
    fn session_echoes_nonce_and_handles_requests_until_clean_eof() {
        let generations = [Generation::from_raw(1), Generation::from_raw(u64::MAX)];
        let mut input = Vec::new();
        protocol::write_message(
            &mut input,
            WorkerMessage::Hello {
                nonce: NONCE,
                legacy_encoding: LegacyEncoding::Auto,
            },
        )
        .unwrap();
        for (index, generation) in generations.into_iter().enumerate() {
            protocol::write_message(
                &mut input,
                WorkerMessage::ResolvePoint {
                    generation,
                    point: PhysicalScreenPoint::new(-1_920 + index as i32, 1_080),
                },
            )
            .unwrap();
        }

        let mut output = Vec::new();
        run_session(
            &mut Cursor::new(input),
            &mut output,
            &mut UnavailableResolver,
        )
        .unwrap();

        let mut output = output.as_slice();
        assert_eq!(
            protocol::read_message(&mut output).unwrap(),
            Some(WorkerMessage::Ready { nonce: NONCE })
        );
        for generation in generations {
            assert_eq!(
                protocol::read_message(&mut output).unwrap(),
                Some(WorkerMessage::PreviewResult {
                    generation,
                    result: PreviewResult::Status(ResolverStatus::Unavailable),
                })
            );
        }
        assert_eq!(protocol::read_message(&mut output).unwrap(), None);
    }

    #[test]
    fn session_requires_hello_and_rejects_non_request_frames() {
        let mut output = Vec::new();
        assert!(matches!(
            run_session(&mut &[][..], &mut output, &mut UnavailableResolver),
            Err(WorkerSessionError::MissingHello)
        ));

        let mut wrong_first = Vec::new();
        protocol::write_message(
            &mut wrong_first,
            WorkerMessage::PreviewResult {
                generation: Generation::from_raw(1),
                result: PreviewResult::Status(ResolverStatus::Unavailable),
            },
        )
        .unwrap();
        assert!(matches!(
            run_session(
                &mut wrong_first.as_slice(),
                &mut output,
                &mut UnavailableResolver
            ),
            Err(WorkerSessionError::ExpectedHello)
        ));

        let mut wrong_second = Vec::new();
        protocol::write_message(
            &mut wrong_second,
            WorkerMessage::Hello {
                nonce: NONCE,
                legacy_encoding: LegacyEncoding::Auto,
            },
        )
        .unwrap();
        protocol::write_message(&mut wrong_second, WorkerMessage::Ready { nonce: NONCE }).unwrap();
        assert!(matches!(
            run_session(
                &mut wrong_second.as_slice(),
                &mut output,
                &mut UnavailableResolver
            ),
            Err(WorkerSessionError::ExpectedResolvePoint)
        ));

        let mut handshake_only = Vec::new();
        protocol::write_message(
            &mut handshake_only,
            WorkerMessage::Hello {
                nonce: NONCE,
                legacy_encoding: LegacyEncoding::Auto,
            },
        )
        .unwrap();
        run_session(
            &mut handshake_only.as_slice(),
            &mut output,
            &mut UnavailableResolver,
        )
        .unwrap();
    }
}
