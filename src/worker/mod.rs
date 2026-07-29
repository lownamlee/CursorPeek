mod cache;
mod file;
mod image;
mod manager;
mod text;
mod vector;

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

use crate::{
    diagnostics,
    resolver::{PointResolver, ResolveOutcome},
    settings::LegacyEncoding,
};
use cache::{PreviewCache, PreviewCacheKey, PreviewProvider};
#[cfg(test)]
use cursorpeek_core::PhysicalScreenPoint;
use cursorpeek_core::{PhysicalScreenRect, PhysicalScreenSpan};
use file::PreviewFile;
use image::ImageDecodeResult;
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
pub(crate) use payload::{
    ImagePreview, PreviewResult, ResolverStatus, TextPreview, VectorPreview,
};
use vector::VectorDecodeResult;
#[cfg(test)]
pub(crate) use vector::corpus::renderable_previews as vector_corpus_previews;

pub(crate) fn run_session<R, W>(
    reader: &mut R,
    writer: &mut W,
    resolver: &mut impl PointResolver,
) -> Result<(), WorkerSessionError>
where
    R: Read,
    W: Write,
{
    diagnostics::record(
        "worker.process.session",
        format_args!("state=waiting_hello"),
    );
    let (nonce, legacy_encoding, cache_entries) = match protocol::read_message(reader)? {
        Some(WorkerMessage::Hello {
            nonce,
            cache_entries,
            legacy_encoding,
        }) => (nonce, legacy_encoding, cache_entries),
        Some(_) => return Err(WorkerSessionError::ExpectedHello),
        None => return Err(WorkerSessionError::MissingHello),
    };
    protocol::write_message(writer, WorkerMessage::Ready { nonce })?;
    diagnostics::record(
        "worker.process.session",
        format_args!(
            "state=ready cache_entries={} legacy_encoding={legacy_encoding:?}",
            cache_entries
        ),
    );
    let mut cache = PreviewCache::with_entry_limit(cache_entries);

    loop {
        let (generation, point, explorer_window, pointer_span) =
            match protocol::read_message(reader)? {
                Some(WorkerMessage::ResolvePoint {
                    generation,
                    point,
                    explorer_window,
                    pointer_span,
                }) => (generation, point, explorer_window, pointer_span),
                Some(_) => return Err(WorkerSessionError::ExpectedResolvePoint),
                None => return Ok(()),
            };
        let request_started = diagnostics::counter();
        diagnostics::record(
            "worker.process.request",
            format_args!(
                "generation={} x={} y={} explorer={}",
                generation.get(),
                point.x,
                point.y,
                explorer_window.map_or(0, cursorpeek_core::ExplorerWindowId::get)
            ),
        );
        let resolver_started = diagnostics::counter();
        let outcome = resolver.resolve(point, explorer_window);
        diagnostics::record(
            "worker.resolver.completed",
            format_args!(
                "generation={} outcome={} elapsed_us={}",
                generation.get(),
                resolve_outcome_kind(&outcome),
                diagnostics::elapsed_us(resolver_started).unwrap_or(0)
            ),
        );
        let (target_bounds, result) =
            resolver_result_with_cache(outcome, pointer_span, &legacy_encoding, &mut cache);
        record_preview_result(
            "worker.preview.completed",
            generation,
            target_bounds,
            &result,
            request_started,
        );
        let write_started = diagnostics::counter();
        protocol::write_message(
            writer,
            WorkerMessage::PreviewResult {
                generation,
                target_bounds,
                result,
            },
        )?;
        diagnostics::record(
            "worker.process.response",
            format_args!(
                "generation={} write_us={} total_us={}",
                generation.get(),
                diagnostics::elapsed_us(write_started).unwrap_or(0),
                diagnostics::elapsed_us(request_started).unwrap_or(0)
            ),
        );
    }
}

fn resolver_result_with_cache(
    outcome: ResolveOutcome,
    pointer_span: PhysicalScreenSpan,
    legacy_encoding: &LegacyEncoding,
    cache: &mut PreviewCache,
) -> (Option<PhysicalScreenRect>, PreviewResult) {
    match outcome {
        ResolveOutcome::Resolved(target) => {
            let target_bounds = target.target_bounds();
            if !pointer_span.fits_within(target_bounds) {
                diagnostics::record(
                    "worker.pointer_span",
                    format_args!("outcome=rejected reason=outside_target"),
                );
                return (None, PreviewResult::Status(ResolverStatus::PointerMoved));
            }
            diagnostics::record("worker.pointer_span", format_args!("outcome=accepted"));
            let open_started = diagnostics::counter();
            let result = match PreviewFile::open(target.path()) {
                Ok(file) => {
                    diagnostics::record(
                        "worker.file.open",
                        format_args!(
                            "outcome=success file_size={} elapsed_us={}",
                            file.file_size(),
                            diagnostics::elapsed_us(open_started).unwrap_or(0)
                        ),
                    );
                    preview_file_result(&file, legacy_encoding, cache)
                }
                Err(error) if error.is_unsupported() => {
                    diagnostics::record(
                        "worker.file.open",
                        format_args!(
                            "outcome=unsupported elapsed_us={}",
                            diagnostics::elapsed_us(open_started).unwrap_or(0)
                        ),
                    );
                    PreviewResult::Status(ResolverStatus::Unsupported)
                }
                Err(_) => {
                    diagnostics::record(
                        "worker.file.open",
                        format_args!(
                            "outcome=unavailable elapsed_us={}",
                            diagnostics::elapsed_us(open_started).unwrap_or(0)
                        ),
                    );
                    PreviewResult::Status(ResolverStatus::Unavailable)
                }
            };
            (
                matches!(
                    result,
                    PreviewResult::Text(_) | PreviewResult::Image(_) | PreviewResult::Vector(_)
                )
                .then_some(target_bounds),
                result,
            )
        }
        ResolveOutcome::Unsupported => (None, PreviewResult::Status(ResolverStatus::Unsupported)),
        ResolveOutcome::Ambiguous => (None, PreviewResult::Status(ResolverStatus::Ambiguous)),
        ResolveOutcome::Unavailable => (None, PreviewResult::Status(ResolverStatus::Unavailable)),
    }
}

fn preview_file_result(
    file: &PreviewFile,
    legacy_encoding: &LegacyEncoding,
    cache: &mut PreviewCache,
) -> PreviewResult {
    let Some(provider) = PreviewProvider::for_path(file.final_path()) else {
        diagnostics::record(
            "worker.provider",
            format_args!("outcome=unsupported file_size={}", file.file_size()),
        );
        return PreviewResult::Status(ResolverStatus::Unsupported);
    };
    diagnostics::record(
        "worker.provider",
        format_args!(
            "outcome=selected provider={provider:?} file_size={}",
            file.file_size()
        ),
    );
    let key = PreviewCacheKey::new(file, provider, legacy_encoding);
    match file.is_unchanged() {
        Ok(true) => {}
        Ok(false) | Err(_) => return PreviewResult::Status(ResolverStatus::Unavailable),
    }
    if let Some(mut result) = cache.get(&key) {
        diagnostics::record(
            "worker.cache",
            format_args!(
                "outcome=hit provider={provider:?} entries={}",
                cache.entry_count()
            ),
        );
        match &mut result {
            PreviewResult::Text(preview) => {
                preview.display_name = file.display_name();
                preview.last_write_time = file.last_write_time();
            }
            PreviewResult::Image(preview) => {
                preview.display_name = file.display_name();
                preview.last_write_time = file.last_write_time();
            }
            PreviewResult::Vector(preview) => {
                preview.display_name = file.display_name();
                preview.last_write_time = file.last_write_time();
            }
            PreviewResult::Status(_) => {}
        }
        return match file.is_unchanged() {
            Ok(true) => result,
            Ok(false) | Err(_) => PreviewResult::Status(ResolverStatus::Unavailable),
        };
    }

    diagnostics::record(
        "worker.cache",
        format_args!(
            "outcome=miss provider={provider:?} entries={}",
            cache.entry_count()
        ),
    );
    let decode_started = diagnostics::counter();
    let result = match provider {
        PreviewProvider::Text => text_result(file, legacy_encoding),
        PreviewProvider::Vector => match vector::decode(file) {
            Ok(VectorDecodeResult::Preview(preview)) => PreviewResult::Vector(preview),
            Ok(VectorDecodeResult::Fallback(reason)) => {
                diagnostics::record(
                    "worker.vector.refused",
                    format_args!("reason={reason} fallback=text"),
                );
                text_result(file, legacy_encoding)
            }
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
    diagnostics::record(
        "worker.decode",
        format_args!(
            "provider={provider:?} kind={} elapsed_us={}",
            preview_result_kind(&result),
            diagnostics::elapsed_us(decode_started).unwrap_or(0)
        ),
    );
    let retained = cache.insert(key, result.clone());
    diagnostics::record(
        "worker.cache",
        format_args!(
            "outcome=insert retained={retained} entries={}",
            cache.entry_count()
        ),
    );
    result
}

fn text_result(file: &PreviewFile, legacy_encoding: &LegacyEncoding) -> PreviewResult {
    match text::decode(file, legacy_encoding) {
        Ok(TextDecodeResult::Preview(preview)) => PreviewResult::Text(preview),
        Ok(TextDecodeResult::Unsupported) => PreviewResult::Status(ResolverStatus::Unsupported),
        Err(_) => PreviewResult::Status(ResolverStatus::Unavailable),
    }
}

fn resolve_outcome_kind(outcome: &ResolveOutcome) -> &'static str {
    match outcome {
        ResolveOutcome::Resolved(_) => "resolved",
        ResolveOutcome::Unsupported => "unsupported",
        ResolveOutcome::Ambiguous => "ambiguous",
        ResolveOutcome::Unavailable => "unavailable",
    }
}

fn preview_result_kind(result: &PreviewResult) -> &'static str {
    match result {
        PreviewResult::Status(status) => match status {
            ResolverStatus::Resolved => "status-resolved",
            ResolverStatus::Unsupported => "status-unsupported",
            ResolverStatus::Ambiguous => "status-ambiguous",
            ResolverStatus::Unavailable => "status-unavailable",
            ResolverStatus::TimedOut => "status-timed-out",
            ResolverStatus::PointerMoved => "status-pointer-moved",
        },
        PreviewResult::Text(_) => "text",
        PreviewResult::Image(_) => "image",
        PreviewResult::Vector(_) => "vector",
    }
}

fn record_preview_result(
    event: &'static str,
    generation: cursorpeek_core::Generation,
    target_bounds: Option<PhysicalScreenRect>,
    result: &PreviewResult,
    started: i64,
) {
    let elapsed = diagnostics::elapsed_us(started).unwrap_or(0);
    match result {
        PreviewResult::Status(status) => diagnostics::record(
            event,
            format_args!(
                "generation={} kind={} status={status:?} target_bounds={} elapsed_us={elapsed}",
                generation.get(),
                preview_result_kind(result),
                target_bounds.is_some()
            ),
        ),
        PreviewResult::Text(preview) => diagnostics::record(
            event,
            format_args!(
                "generation={} kind=text file_size={} text_bytes={} encoding={} truncated={} \
                 guessed={} linked={} target_bounds={} elapsed_us={elapsed}",
                generation.get(),
                preview.file_size,
                preview.text.len(),
                preview.encoding,
                preview.truncated,
                preview.encoding_was_guessed,
                preview.linked_content,
                target_bounds.is_some()
            ),
        ),
        PreviewResult::Image(preview) => diagnostics::record(
            event,
            format_args!(
                "generation={} kind=image file_size={} format={:?} source_width={} \
                 source_height={} decoded_width={} decoded_height={} decoded_bytes={} \
                 first_frame_only={} linked={} target_bounds={} elapsed_us={elapsed}",
                generation.get(),
                preview.file_size,
                preview.format,
                preview.source_width,
                preview.source_height,
                preview.width,
                preview.height,
                preview.premultiplied_bgra.len(),
                preview.first_frame_only,
                preview.linked_content,
                target_bounds.is_some()
            ),
        ),
        PreviewResult::Vector(preview) => diagnostics::record(
            event,
            format_args!(
                "generation={} kind=vector file_size={} source_width={} source_height={} \
                 width={} height={} frames={} frame_delay_ms={} animated={} linked={} \
                 target_bounds={} elapsed_us={elapsed}",
                generation.get(),
                preview.file_size,
                preview.source_width,
                preview.source_height,
                preview.width,
                preview.height,
                preview.frames.len(),
                preview.frame_delay_ms,
                preview.animated,
                preview.linked_content,
                target_bounds.is_some()
            ),
        ),
    }
}

#[cfg(test)]
fn resolver_result(outcome: ResolveOutcome, legacy_encoding: &LegacyEncoding) -> PreviewResult {
    let pointer_span = match &outcome {
        ResolveOutcome::Resolved(target) => PhysicalScreenSpan::from_point(
            PhysicalScreenPoint::new(target.target_bounds().left(), target.target_bounds().top()),
        ),
        ResolveOutcome::Unsupported | ResolveOutcome::Ambiguous | ResolveOutcome::Unavailable => {
            PhysicalScreenSpan::from_point(PhysicalScreenPoint::new(0, 0))
        }
    };
    resolver_result_with_cache(
        outcome,
        pointer_span,
        legacy_encoding,
        &mut PreviewCache::default(),
    )
    .1
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
    use cursorpeek_core::{ExplorerWindowId, PhysicalScreenRect, PhysicalScreenSpan};
    use protocol::{SessionNonce, WorkerMessage};
    use std::{
        env, fs,
        io::Cursor,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    const NONCE: SessionNonce = SessionNonce::from_bytes([
        0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23,
        0x01,
    ]);
    static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(1);

    fn resolved_target(path: PathBuf) -> ResolvedTarget {
        ResolvedTarget::new(
            path,
            PhysicalScreenRect::try_new(-100, -50, 200, 250).unwrap(),
        )
    }

    struct UnavailableResolver;

    impl PointResolver for UnavailableResolver {
        fn resolve(
            &mut self,
            _point: PhysicalScreenPoint,
            _explorer_window: Option<ExplorerWindowId>,
        ) -> ResolveOutcome {
            ResolveOutcome::Unavailable
        }
    }

    #[derive(Default)]
    struct RecordingResolver {
        explorer_windows: Vec<Option<ExplorerWindowId>>,
    }

    impl PointResolver for RecordingResolver {
        fn resolve(
            &mut self,
            _point: PhysicalScreenPoint,
            explorer_window: Option<ExplorerWindowId>,
        ) -> ResolveOutcome {
            self.explorer_windows.push(explorer_window);
            ResolveOutcome::Unavailable
        }
    }

    #[test]
    fn session_delivers_the_authenticated_explorer_identity_to_the_resolver() {
        let explorer_window = ExplorerWindowId::try_from_raw(0x1234_5678).unwrap();
        let point = PhysicalScreenPoint::new(400, 300);
        let mut input = Vec::new();
        protocol::write_message(
            &mut input,
            WorkerMessage::Hello {
                nonce: NONCE,
                cache_entries: protocol::DEFAULT_PREVIEW_CACHE_ENTRIES,
                legacy_encoding: LegacyEncoding::Auto,
            },
        )
        .unwrap();
        protocol::write_message(
            &mut input,
            WorkerMessage::ResolvePoint {
                generation: Generation::from_raw(1),
                point,
                explorer_window: Some(explorer_window),
                pointer_span: PhysicalScreenSpan::from_point(point),
            },
        )
        .unwrap();

        let mut output = Vec::new();
        let mut resolver = RecordingResolver::default();
        run_session(&mut input.as_slice(), &mut output, &mut resolver).unwrap();

        assert_eq!(resolver.explorer_windows, vec![Some(explorer_window)]);
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
            ResolveOutcome::Resolved(resolved_target(path.clone())),
            &LegacyEncoding::Auto,
        ) else {
            panic!("the resolved UTF-8 fixture should produce a text preview");
        };
        assert_eq!(preview.text, "preview");
        assert_eq!(preview.encoding, "UTF-8");
        assert_eq!(preview.file_size, 7);
        assert_eq!(
            preview.display_name,
            path.file_name().unwrap().to_string_lossy()
        );
        assert!(preview.last_write_time > 0);
        assert_eq!(preview.linked_content, expected_linked_content);
        assert!(!preview.encoding_was_guessed);
        assert!(!preview.truncated);
        fs::remove_file(&path).expect("the resolved fixture should be removed");
        assert_eq!(
            resolver_result(
                ResolveOutcome::Resolved(resolved_target(path)),
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
                ResolveOutcome::Resolved(resolved_target(binary_path.clone())),
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
                ResolveOutcome::Resolved(resolved_target(malformed_image_path.clone())),
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
            ResolveOutcome::Resolved(resolved_target(image_path.clone())),
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
            ResolveOutcome::Resolved(resolved_target(legacy_path.clone())),
            &LegacyEncoding::Auto,
        ) else {
            panic!("the detected legacy fixture should produce a text preview");
        };
        assert_eq!(preview.text, "I’");
        assert_eq!(preview.encoding, "windows-1252");
        assert!(preview.encoding_was_guessed);
        assert_eq!(
            resolver_result(
                ResolveOutcome::Resolved(resolved_target(legacy_path.clone())),
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
    fn svg_targets_render_as_vectors_and_fall_back_to_text_when_refused() {
        let rendered_path = env::temp_dir().join(format!(
            "cursorpeek-resolved-target-{}-{}.svg",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(
            &rendered_path,
            b"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'>\
              <rect width='16' height='16' fill='#2f81f7'/></svg>",
        )
        .expect("the vector fixture should be written");
        let PreviewResult::Vector(preview) = resolver_result(
            ResolveOutcome::Resolved(resolved_target(rendered_path.clone())),
            &LegacyEncoding::Auto,
        ) else {
            panic!("a well-formed SVG should produce a vector preview");
        };
        assert_eq!((preview.source_width, preview.source_height), (16, 16));
        assert_eq!(preview.frames.len(), 1);
        assert!(!preview.animated);
        assert_eq!(
            preview.display_name,
            rendered_path.file_name().unwrap().to_string_lossy()
        );
        fs::remove_file(&rendered_path).expect("the vector fixture should be removed");

        let refused_path = env::temp_dir().join(format!(
            "cursorpeek-resolved-target-{}-{}.svg",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let markup = "<svg><script>alert(1)</script></svg>";
        fs::write(&refused_path, markup.as_bytes())
            .expect("the refused vector fixture should be written");
        let PreviewResult::Text(preview) = resolver_result(
            ResolveOutcome::Resolved(resolved_target(refused_path.clone())),
            &LegacyEncoding::Auto,
        ) else {
            panic!("a refused SVG should fall back to an inert text preview");
        };
        assert_eq!(preview.text, markup);
        fs::remove_file(&refused_path).expect("the refused vector fixture should be removed");
    }

    #[test]
    fn pointer_travel_must_fit_the_resolved_item_before_file_access() {
        let missing = env::temp_dir().join(format!(
            "cursorpeek-pointer-span-{}-{}.txt",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let (_, result) = resolver_result_with_cache(
            ResolveOutcome::Resolved(resolved_target(missing)),
            PhysicalScreenSpan::try_new(-101, 0, 0, 1).unwrap(),
            &LegacyEncoding::Auto,
            &mut PreviewCache::default(),
        );

        assert_eq!(result, PreviewResult::Status(ResolverStatus::PointerMoved));
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
                ResolveOutcome::Resolved(resolved_target(path.clone())),
                PhysicalScreenSpan::from_point(PhysicalScreenPoint::new(0, 0)),
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
        let (Some(bounds), PreviewResult::Text(changed)) = resolve(&mut cache) else {
            panic!("the changed file should still produce text");
        };
        assert!(bounds.contains(PhysicalScreenPoint::new(0, 0)));
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
                cache_entries: protocol::DEFAULT_PREVIEW_CACHE_ENTRIES,
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
                    explorer_window: ExplorerWindowId::try_from_raw(0x1234),
                    pointer_span: PhysicalScreenSpan::from_point(PhysicalScreenPoint::new(
                        -1_920 + index as i32,
                        1_080,
                    )),
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
                    target_bounds: None,
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
                target_bounds: None,
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
                cache_entries: protocol::DEFAULT_PREVIEW_CACHE_ENTRIES,
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
                cache_entries: protocol::DEFAULT_PREVIEW_CACHE_ENTRIES,
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
