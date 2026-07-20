mod file;
mod manager;
mod payload;
mod protocol;
mod text;

use std::{
    error::Error,
    fmt,
    io::{Read, Write},
};

use crate::resolver::{PointResolver, ResolveOutcome};
use crate::settings::LegacyEncoding;
use file::PreviewFile;
use payload::ResolverStatus;
use protocol::{ProtocolStreamError, WorkerMessage};
use text::TextDecodeResult;

pub(crate) use manager::{
    CompletionNotifier, PendingWorkerPoll, PendingWorkerResolution, WorkerManager,
    WorkerManagerError, run_launch_diagnostic, run_timeout_diagnostic,
};
pub(crate) use payload::{PreviewResult, TextPreview};

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

    loop {
        let (generation, point) = match protocol::read_message(reader)? {
            Some(WorkerMessage::ResolvePoint { generation, point }) => (generation, point),
            Some(_) => return Err(WorkerSessionError::ExpectedResolvePoint),
            None => return Ok(()),
        };
        let result = resolver_result(resolver.resolve(point), &legacy_encoding);
        protocol::write_message(writer, WorkerMessage::PreviewResult { generation, result })?;
    }
}

fn resolver_result(outcome: ResolveOutcome, legacy_encoding: &LegacyEncoding) -> PreviewResult {
    match outcome {
        ResolveOutcome::Resolved(target) => match PreviewFile::open(target.path()) {
            Ok(file) => match text::decode(&file, legacy_encoding) {
                Ok(TextDecodeResult::Preview(preview)) => PreviewResult::Text(preview),
                Ok(TextDecodeResult::Unsupported) => {
                    PreviewResult::Status(ResolverStatus::Unsupported)
                }
                Err(_) => PreviewResult::Status(ResolverStatus::Unavailable),
            },
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
    use super::{WorkerSessionError, protocol, resolver_result, run_session};
    use crate::hover::{Generation, PhysicalScreenPoint};
    use crate::resolver::{PointResolver, ResolveOutcome, ResolvedTarget};
    use crate::settings::LegacyEncoding;
    use crate::worker::payload::{PreviewResult, ResolverStatus};
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
        let PreviewResult::Text(preview) = resolver_result(
            ResolveOutcome::Resolved(ResolvedTarget::new(path.clone())),
            &LegacyEncoding::Auto,
        ) else {
            panic!("the resolved UTF-8 fixture should produce a text preview");
        };
        assert_eq!(preview.text, "preview");
        assert_eq!(preview.encoding, "UTF-8");
        assert_eq!(preview.file_size, 7);
        assert!(!preview.linked_content);
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
