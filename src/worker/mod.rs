mod manager;
mod protocol;

use std::{
    error::Error,
    fmt,
    io::{Read, Write},
};

use protocol::{ProtocolStreamError, ResolverStatus, WorkerMessage};

pub(crate) use manager::{run_launch_diagnostic, run_timeout_diagnostic, WorkerManagerError};

pub(crate) fn run_diagnostic_session<R, W>(
    reader: &mut R,
    writer: &mut W,
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

    let generation = match protocol::read_message(reader)? {
        Some(WorkerMessage::ResolvePoint {
            generation,
            point: _,
        }) => generation,
        Some(_) => return Err(WorkerSessionError::ExpectedResolvePoint),
        None => return Err(WorkerSessionError::MissingResolvePoint),
    };
    protocol::write_message(
        writer,
        WorkerMessage::ResolverResult {
            generation,
            status: ResolverStatus::Unavailable,
        },
    )?;

    Ok(())
}

#[derive(Debug)]
pub(crate) enum WorkerSessionError {
    Stream(ProtocolStreamError),
    MissingHello,
    ExpectedHello,
    MissingResolvePoint,
    ExpectedResolvePoint,
}

impl fmt::Display for WorkerSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stream(error) => write!(formatter, "{error}"),
            Self::MissingHello => write!(formatter, "input closed before the hello frame"),
            Self::ExpectedHello => write!(formatter, "the first frame was not hello"),
            Self::MissingResolvePoint => {
                write!(formatter, "input closed before the resolve-point frame")
            }
            Self::ExpectedResolvePoint => {
                write!(formatter, "the second frame was not resolve-point")
            }
        }
    }
}

impl Error for WorkerSessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Stream(error) => Some(error),
            Self::MissingHello
            | Self::ExpectedHello
            | Self::MissingResolvePoint
            | Self::ExpectedResolvePoint => None,
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
    use super::{protocol, run_diagnostic_session, WorkerSessionError};
    use crate::hover::{Generation, PhysicalScreenPoint};
    use protocol::{ResolverStatus, SessionNonce, WorkerMessage};
    use std::io::Cursor;

    const NONCE: SessionNonce = SessionNonce::from_bytes([
        0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23,
        0x01,
    ]);

    #[test]
    fn diagnostic_session_echoes_nonce_and_preserves_generation() {
        let generation = Generation::from_raw(u64::MAX);
        let mut input = Vec::new();
        protocol::write_message(&mut input, WorkerMessage::Hello { nonce: NONCE }).unwrap();
        protocol::write_message(
            &mut input,
            WorkerMessage::ResolvePoint {
                generation,
                point: PhysicalScreenPoint::new(-1_920, 1_080),
            },
        )
        .unwrap();

        let mut output = Vec::new();
        run_diagnostic_session(&mut Cursor::new(input), &mut output).unwrap();

        let mut output = output.as_slice();
        assert_eq!(
            protocol::read_message(&mut output).unwrap(),
            Some(WorkerMessage::Ready { nonce: NONCE })
        );
        assert_eq!(
            protocol::read_message(&mut output).unwrap(),
            Some(WorkerMessage::ResolverResult {
                generation,
                status: ResolverStatus::Unavailable,
            })
        );
        assert_eq!(protocol::read_message(&mut output).unwrap(), None);
    }

    #[test]
    fn diagnostic_session_requires_hello_then_resolve_point() {
        let mut output = Vec::new();
        assert!(matches!(
            run_diagnostic_session(&mut &[][..], &mut output),
            Err(WorkerSessionError::MissingHello)
        ));

        let mut wrong_first = Vec::new();
        protocol::write_message(
            &mut wrong_first,
            WorkerMessage::ResolverResult {
                generation: Generation::from_raw(1),
                status: ResolverStatus::Unavailable,
            },
        )
        .unwrap();
        assert!(matches!(
            run_diagnostic_session(&mut wrong_first.as_slice(), &mut output),
            Err(WorkerSessionError::ExpectedHello)
        ));

        let mut missing_second = Vec::new();
        protocol::write_message(&mut missing_second, WorkerMessage::Hello { nonce: NONCE })
            .unwrap();
        assert!(matches!(
            run_diagnostic_session(&mut missing_second.as_slice(), &mut output),
            Err(WorkerSessionError::MissingResolvePoint)
        ));

        let mut wrong_second = Vec::new();
        protocol::write_message(&mut wrong_second, WorkerMessage::Hello { nonce: NONCE }).unwrap();
        protocol::write_message(&mut wrong_second, WorkerMessage::Ready { nonce: NONCE }).unwrap();
        assert!(matches!(
            run_diagnostic_session(&mut wrong_second.as_slice(), &mut output),
            Err(WorkerSessionError::ExpectedResolvePoint)
        ));
    }
}
