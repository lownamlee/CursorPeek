mod manager;
mod payload;
mod protocol;

use std::{
    error::Error,
    fmt,
    io::{Read, Write},
};

use crate::resolver::{PointResolver, ResolveOutcome};
use payload::{PreviewResult, ResolverStatus};
use protocol::{ProtocolStreamError, WorkerMessage};

pub(crate) use manager::{
    CompletionNotifier, PendingWorkerPoll, PendingWorkerResolution, WorkerManager,
    WorkerManagerError, run_launch_diagnostic, run_timeout_diagnostic,
};

pub(crate) fn run_session<R, W>(
    reader: &mut R,
    writer: &mut W,
    resolver: &mut impl PointResolver,
) -> Result<(), WorkerSessionError>
where
    R: Read,
    W: Write,
{
    let nonce = match protocol::read_message(reader)? {
        Some(WorkerMessage::Hello { nonce }) => nonce,
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
        let result = resolver_result(resolver.resolve(point));
        protocol::write_message(writer, WorkerMessage::PreviewResult { generation, result })?;
    }
}

fn resolver_result(outcome: ResolveOutcome) -> PreviewResult {
    PreviewResult::Status(match outcome {
        ResolveOutcome::Resolved(target) => {
            debug_assert!(target.path().is_absolute());
            ResolverStatus::Resolved
        }
        ResolveOutcome::Unsupported => ResolverStatus::Unsupported,
        ResolveOutcome::Ambiguous => ResolverStatus::Ambiguous,
        ResolveOutcome::Unavailable => ResolverStatus::Unavailable,
    })
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
    use crate::worker::payload::{PreviewResult, ResolverStatus};
    use protocol::{SessionNonce, WorkerMessage};
    use std::{io::Cursor, path::PathBuf};

    const NONCE: SessionNonce = SessionNonce::from_bytes([
        0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23,
        0x01,
    ]);

    struct UnavailableResolver;

    impl PointResolver for UnavailableResolver {
        fn resolve(&mut self, _point: PhysicalScreenPoint) -> ResolveOutcome {
            ResolveOutcome::Unavailable
        }
    }

    #[test]
    fn resolver_outcomes_map_to_typed_preview_statuses() {
        assert_eq!(
            resolver_result(ResolveOutcome::Resolved(ResolvedTarget::new(
                PathBuf::from(r"C:\preview.txt")
            ))),
            PreviewResult::Status(ResolverStatus::Resolved)
        );
        assert_eq!(
            resolver_result(ResolveOutcome::Unsupported),
            PreviewResult::Status(ResolverStatus::Unsupported)
        );
        assert_eq!(
            resolver_result(ResolveOutcome::Ambiguous),
            PreviewResult::Status(ResolverStatus::Ambiguous)
        );
        assert_eq!(
            resolver_result(ResolveOutcome::Unavailable),
            PreviewResult::Status(ResolverStatus::Unavailable)
        );
    }

    #[test]
    fn session_echoes_nonce_and_handles_requests_until_clean_eof() {
        let generations = [Generation::from_raw(1), Generation::from_raw(u64::MAX)];
        let mut input = Vec::new();
        protocol::write_message(&mut input, WorkerMessage::Hello { nonce: NONCE }).unwrap();
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
        protocol::write_message(&mut wrong_second, WorkerMessage::Hello { nonce: NONCE }).unwrap();
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
        protocol::write_message(&mut handshake_only, WorkerMessage::Hello { nonce: NONCE })
            .unwrap();
        run_session(
            &mut handshake_only.as_slice(),
            &mut output,
            &mut UnavailableResolver,
        )
        .unwrap();
    }
}
