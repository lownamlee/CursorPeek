use std::{
    error::Error,
    fmt,
    io::{self, ErrorKind, Read, Write},
};

use crate::hover::{Generation, PhysicalScreenPoint};

const MAGIC: [u8; 4] = *b"CPWK";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 24;
const NONCE_LEN: usize = 16;
const MAX_CONTROL_PAYLOAD_LEN: usize = NONCE_LEN;
const MAX_CONTROL_FRAME_LEN: usize = HEADER_LEN + MAX_CONTROL_PAYLOAD_LEN;
const MAX_PREVIEW_PAYLOAD_LEN: u32 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SessionNonce([u8; NONCE_LEN]);

impl SessionNonce {
    pub(super) const fn from_bytes(bytes: [u8; NONCE_LEN]) -> Self {
        Self(bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResolverStatus {
    Resolved = 0,
    Unsupported = 1,
    Ambiguous = 2,
    Unavailable = 3,
    TimedOut = 4,
}

impl ResolverStatus {
    fn from_raw(value: u32) -> Result<Self, ProtocolError> {
        match value {
            0 => Ok(Self::Resolved),
            1 => Ok(Self::Unsupported),
            2 => Ok(Self::Ambiguous),
            3 => Ok(Self::Unavailable),
            4 => Ok(Self::TimedOut),
            _ => Err(ProtocolError::UnknownResolverStatus(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkerMessage {
    Hello {
        nonce: SessionNonce,
    },
    Ready {
        nonce: SessionNonce,
    },
    ResolvePoint {
        generation: Generation,
        point: PhysicalScreenPoint,
    },
    ResolverResult {
        generation: Generation,
        status: ResolverStatus,
    },
}

impl WorkerMessage {
    fn kind(self) -> MessageKind {
        match self {
            Self::Hello { .. } => MessageKind::Hello,
            Self::Ready { .. } => MessageKind::Ready,
            Self::ResolvePoint { .. } => MessageKind::ResolvePoint,
            Self::ResolverResult { .. } => MessageKind::ResolverResult,
        }
    }

    fn generation(self) -> Generation {
        match self {
            Self::Hello { .. } | Self::Ready { .. } => Generation::from_raw(0),
            Self::ResolvePoint { generation, .. } | Self::ResolverResult { generation, .. } => {
                generation
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageKind {
    Hello = 1,
    Ready = 2,
    ResolvePoint = 3,
    ResolverResult = 4,
}

impl MessageKind {
    fn from_raw(value: u16) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Ready),
            3 => Ok(Self::ResolvePoint),
            4 => Ok(Self::ResolverResult),
            _ => Err(ProtocolError::UnknownMessageKind(value)),
        }
    }

    fn payload_len(self) -> usize {
        match self {
            Self::Hello | Self::Ready => NONCE_LEN,
            Self::ResolvePoint => 8,
            Self::ResolverResult => 4,
        }
    }

    fn is_handshake(self) -> bool {
        matches!(self, Self::Hello | Self::Ready)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameHeader {
    kind: MessageKind,
    payload_len: usize,
    generation: Generation,
}

impl FrameHeader {
    fn decode(bytes: &[u8; HEADER_LEN]) -> Result<Self, ProtocolError> {
        if bytes[..4] != MAGIC {
            return Err(ProtocolError::InvalidMagic);
        }

        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }

        let kind = MessageKind::from_raw(u16::from_le_bytes([bytes[6], bytes[7]]))?;
        let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if payload_len > MAX_PREVIEW_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge(payload_len));
        }

        let reserved = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        if reserved != 0 {
            return Err(ProtocolError::ReservedFieldSet(reserved));
        }

        let generation = Generation::from_raw(u64::from_le_bytes([
            bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22], bytes[23],
        ]));
        if kind.is_handshake() && generation.get() != 0 {
            return Err(ProtocolError::HandshakeGeneration(generation.get()));
        }

        let payload_len = payload_len as usize;
        let expected = kind.payload_len();
        if payload_len != expected {
            return Err(ProtocolError::InvalidPayloadLength {
                expected,
                actual: payload_len,
            });
        }

        Ok(Self {
            kind,
            payload_len,
            generation,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ProtocolError {
    TruncatedHeader { actual: usize },
    InvalidMagic,
    UnsupportedVersion(u16),
    UnknownMessageKind(u16),
    PayloadTooLarge(u32),
    ReservedFieldSet(u32),
    HandshakeGeneration(u64),
    InvalidPayloadLength { expected: usize, actual: usize },
    FrameLengthMismatch { expected: usize, actual: usize },
    UnknownResolverStatus(u32),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader { actual } => {
                write!(
                    formatter,
                    "truncated header: expected {HEADER_LEN} bytes, received {actual}"
                )
            }
            Self::InvalidMagic => write!(formatter, "invalid worker frame magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported worker protocol version {version}")
            }
            Self::UnknownMessageKind(kind) => {
                write!(formatter, "unknown worker message kind {kind}")
            }
            Self::PayloadTooLarge(length) => {
                write!(
                    formatter,
                    "worker payload length {length} exceeds the protocol cap"
                )
            }
            Self::ReservedFieldSet(value) => {
                write!(
                    formatter,
                    "reserved worker header field is nonzero ({value})"
                )
            }
            Self::HandshakeGeneration(generation) => {
                write!(
                    formatter,
                    "handshake frame has generation {generation}, expected zero"
                )
            }
            Self::InvalidPayloadLength { expected, actual } => write!(
                formatter,
                "invalid control payload length: expected {expected} bytes, received {actual}"
            ),
            Self::FrameLengthMismatch { expected, actual } => write!(
                formatter,
                "worker frame length mismatch: expected {expected} bytes, received {actual}"
            ),
            Self::UnknownResolverStatus(status) => {
                write!(formatter, "unknown resolver status {status}")
            }
        }
    }
}

impl Error for ProtocolError {}

#[derive(Debug)]
pub(crate) enum ProtocolStreamError {
    Io(io::Error),
    TruncatedFrame,
    InvalidReadCount(usize),
    Protocol(ProtocolError),
}

impl fmt::Display for ProtocolStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "worker stream I/O failed: {error}"),
            Self::TruncatedFrame => write!(formatter, "worker stream ended inside a frame"),
            Self::InvalidReadCount(count) => {
                write!(
                    formatter,
                    "worker stream returned invalid read count {count}"
                )
            }
            Self::Protocol(error) => write!(formatter, "invalid worker frame: {error}"),
        }
    }
}

impl Error for ProtocolStreamError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::TruncatedFrame | Self::InvalidReadCount(_) => None,
        }
    }
}

impl From<ProtocolError> for ProtocolStreamError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

pub(super) fn read_message<R: Read>(
    reader: &mut R,
) -> Result<Option<WorkerMessage>, ProtocolStreamError> {
    let mut frame = [0_u8; MAX_CONTROL_FRAME_LEN];
    if !read_first_byte(reader, &mut frame[0])? {
        return Ok(None);
    }
    read_exact_frame(reader, &mut frame[1..HEADER_LEN])?;

    let header_bytes: &[u8; HEADER_LEN] = (&frame[..HEADER_LEN])
        .try_into()
        .expect("the header slice has the fixed header length");
    let header = FrameHeader::decode(header_bytes)?;
    let frame_len = HEADER_LEN + header.payload_len;
    read_exact_frame(reader, &mut frame[HEADER_LEN..frame_len])?;

    decode_frame(&frame[..frame_len])
        .map(Some)
        .map_err(Into::into)
}

pub(super) fn write_message<W: Write>(
    writer: &mut W,
    message: WorkerMessage,
) -> Result<(), ProtocolStreamError> {
    let encoded = encode_message(message);
    writer
        .write_all(encoded.as_bytes())
        .map_err(ProtocolStreamError::Io)?;
    writer.flush().map_err(ProtocolStreamError::Io)
}

fn read_first_byte<R: Read>(reader: &mut R, byte: &mut u8) -> Result<bool, ProtocolStreamError> {
    loop {
        match reader.read(std::slice::from_mut(byte)) {
            Ok(0) => return Ok(false),
            Ok(1) => return Ok(true),
            Ok(count) => return Err(ProtocolStreamError::InvalidReadCount(count)),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(ProtocolStreamError::Io(error)),
        }
    }
}

fn read_exact_frame<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<(), ProtocolStreamError> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
            Err(ProtocolStreamError::TruncatedFrame)
        }
        Err(error) => Err(ProtocolStreamError::Io(error)),
    }
}

#[derive(Clone, Copy)]
struct EncodedMessage {
    bytes: [u8; MAX_CONTROL_FRAME_LEN],
    len: usize,
}

impl EncodedMessage {
    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

fn encode_message(message: WorkerMessage) -> EncodedMessage {
    let kind = message.kind();
    let payload_len = kind.payload_len();
    let mut bytes = [0_u8; MAX_CONTROL_FRAME_LEN];

    bytes[..4].copy_from_slice(&MAGIC);
    bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
    bytes[6..8].copy_from_slice(&(kind as u16).to_le_bytes());
    bytes[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());
    bytes[16..24].copy_from_slice(&message.generation().get().to_le_bytes());

    match message {
        WorkerMessage::Hello { nonce } | WorkerMessage::Ready { nonce } => {
            bytes[HEADER_LEN..HEADER_LEN + NONCE_LEN].copy_from_slice(&nonce.0);
        }
        WorkerMessage::ResolvePoint { point, .. } => {
            bytes[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&point.x.to_le_bytes());
            bytes[HEADER_LEN + 4..HEADER_LEN + 8].copy_from_slice(&point.y.to_le_bytes());
        }
        WorkerMessage::ResolverResult { status, .. } => {
            bytes[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&(status as u32).to_le_bytes());
        }
    }

    EncodedMessage {
        bytes,
        len: HEADER_LEN + payload_len,
    }
}

fn decode_frame(bytes: &[u8]) -> Result<WorkerMessage, ProtocolError> {
    if bytes.len() < HEADER_LEN {
        return Err(ProtocolError::TruncatedHeader {
            actual: bytes.len(),
        });
    }

    let header_bytes: &[u8; HEADER_LEN] = bytes[..HEADER_LEN]
        .try_into()
        .expect("the length check guarantees a complete fixed header");
    let header = FrameHeader::decode(header_bytes)?;
    let expected_len = HEADER_LEN + header.payload_len;
    if bytes.len() != expected_len {
        return Err(ProtocolError::FrameLengthMismatch {
            expected: expected_len,
            actual: bytes.len(),
        });
    }
    let payload = &bytes[HEADER_LEN..];

    match header.kind {
        MessageKind::Hello | MessageKind::Ready => {
            let mut nonce = [0_u8; NONCE_LEN];
            nonce.copy_from_slice(payload);
            let nonce = SessionNonce(nonce);
            Ok(if header.kind == MessageKind::Hello {
                WorkerMessage::Hello { nonce }
            } else {
                WorkerMessage::Ready { nonce }
            })
        }
        MessageKind::ResolvePoint => Ok(WorkerMessage::ResolvePoint {
            generation: header.generation,
            point: PhysicalScreenPoint::new(
                i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]),
                i32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]),
            ),
        }),
        MessageKind::ResolverResult => Ok(WorkerMessage::ResolverResult {
            generation: header.generation,
            status: ResolverStatus::from_raw(u32::from_le_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ]))?,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HEADER_LEN, MAGIC, ProtocolError, ProtocolStreamError, ResolverStatus, SessionNonce,
        VERSION, WorkerMessage, decode_frame, encode_message, read_message, write_message,
    };
    use crate::hover::{Generation, PhysicalScreenPoint};
    use std::io::{self, ErrorKind, Read, Write};

    const NONCE: SessionNonce = SessionNonce::from_bytes([
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ]);

    fn messages() -> [WorkerMessage; 4] {
        [
            WorkerMessage::Hello { nonce: NONCE },
            WorkerMessage::Ready { nonce: NONCE },
            WorkerMessage::ResolvePoint {
                generation: Generation::from_raw(0x0102_0304_0506_0708),
                point: PhysicalScreenPoint::new(-2, 0x0102_0304),
            },
            WorkerMessage::ResolverResult {
                generation: Generation::from_raw(u64::MAX),
                status: ResolverStatus::TimedOut,
            },
        ]
    }

    #[test]
    fn every_control_message_round_trips() {
        for message in messages() {
            let encoded = encode_message(message);
            assert_eq!(decode_frame(encoded.as_bytes()), Ok(message));
        }
    }

    #[test]
    fn encoding_has_a_stable_little_endian_layout() {
        let encoded = encode_message(messages()[2]);
        let bytes = encoded.as_bytes();

        assert_eq!(&bytes[..4], &MAGIC);
        assert_eq!(&bytes[4..6], &VERSION.to_le_bytes());
        assert_eq!(&bytes[6..8], &3_u16.to_le_bytes());
        assert_eq!(&bytes[8..12], &8_u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &[0; 4]);
        assert_eq!(&bytes[16..24], &0x0102_0304_0506_0708_u64.to_le_bytes());
        assert_eq!(&bytes[24..28], &(-2_i32).to_le_bytes());
        assert_eq!(&bytes[28..32], &0x0102_0304_i32.to_le_bytes());
    }

    #[test]
    fn malformed_headers_are_rejected_before_payload_use() {
        let encoded = encode_message(WorkerMessage::Hello { nonce: NONCE });

        let mut bad_magic = encoded.bytes;
        bad_magic[0] ^= 0xff;
        assert_eq!(
            decode_frame(&bad_magic[..encoded.len]),
            Err(ProtocolError::InvalidMagic)
        );

        let mut bad_version = encoded.bytes;
        bad_version[4..6].copy_from_slice(&2_u16.to_le_bytes());
        assert_eq!(
            decode_frame(&bad_version[..encoded.len]),
            Err(ProtocolError::UnsupportedVersion(2))
        );

        let mut bad_kind = encoded.bytes;
        bad_kind[6..8].copy_from_slice(&99_u16.to_le_bytes());
        assert_eq!(
            decode_frame(&bad_kind[..encoded.len]),
            Err(ProtocolError::UnknownMessageKind(99))
        );

        let mut oversized = encoded.bytes;
        oversized[8..12].copy_from_slice(&(4 * 1024 * 1024_u32 + 1).to_le_bytes());
        assert_eq!(
            decode_frame(&oversized[..encoded.len]),
            Err(ProtocolError::PayloadTooLarge(4 * 1024 * 1024 + 1))
        );

        let mut reserved = encoded.bytes;
        reserved[12..16].copy_from_slice(&1_u32.to_le_bytes());
        assert_eq!(
            decode_frame(&reserved[..encoded.len]),
            Err(ProtocolError::ReservedFieldSet(1))
        );

        let mut generated_handshake = encoded.bytes;
        generated_handshake[16..24].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(
            decode_frame(&generated_handshake[..encoded.len]),
            Err(ProtocolError::HandshakeGeneration(1))
        );
    }

    #[test]
    fn malformed_lengths_status_and_trailing_bytes_are_rejected() {
        assert_eq!(
            decode_frame(&[0; HEADER_LEN - 1]),
            Err(ProtocolError::TruncatedHeader {
                actual: HEADER_LEN - 1
            })
        );

        let hello = encode_message(WorkerMessage::Hello { nonce: NONCE });
        let mut bad_payload_len = hello.bytes;
        bad_payload_len[8..12].copy_from_slice(&15_u32.to_le_bytes());
        assert_eq!(
            decode_frame(&bad_payload_len[..hello.len]),
            Err(ProtocolError::InvalidPayloadLength {
                expected: 16,
                actual: 15,
            })
        );

        let mut trailing = hello.as_bytes().to_vec();
        trailing.push(0);
        assert_eq!(
            decode_frame(&trailing),
            Err(ProtocolError::FrameLengthMismatch {
                expected: hello.len,
                actual: hello.len + 1,
            })
        );

        let result = encode_message(WorkerMessage::ResolverResult {
            generation: Generation::from_raw(7),
            status: ResolverStatus::Resolved,
        });
        let mut bad_status = result.bytes;
        bad_status[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&99_u32.to_le_bytes());
        assert_eq!(
            decode_frame(&bad_status[..result.len]),
            Err(ProtocolError::UnknownResolverStatus(99))
        );
    }

    #[test]
    fn exact_stream_helpers_tolerate_fragmentation_and_interruption() {
        let message = WorkerMessage::ResolvePoint {
            generation: Generation::from_raw(42),
            point: PhysicalScreenPoint::new(i32::MIN, i32::MAX),
        };
        let mut writer = FragmentedWriter::default();
        write_message(&mut writer, message).unwrap();
        assert!(writer.flushed);

        let mut reader = FragmentedReader::new(&writer.bytes);
        assert_eq!(read_message(&mut reader).unwrap(), Some(message));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn stream_distinguishes_clean_eof_from_truncation() {
        assert_eq!(read_message(&mut &[][..]).unwrap(), None);

        let encoded = encode_message(WorkerMessage::Hello { nonce: NONCE });
        for truncated_len in [1, HEADER_LEN - 1, HEADER_LEN, encoded.len - 1] {
            assert!(matches!(
                read_message(&mut &encoded.as_bytes()[..truncated_len]),
                Err(ProtocolStreamError::TruncatedFrame)
            ));
        }
    }

    struct FragmentedReader<'a> {
        bytes: &'a [u8],
        offset: usize,
        interrupted: bool,
    }

    impl<'a> FragmentedReader<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self {
                bytes,
                offset: 0,
                interrupted: false,
            }
        }
    }

    impl Read for FragmentedReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::from(ErrorKind::Interrupted));
            }
            if self.offset == self.bytes.len() {
                return Ok(0);
            }

            buffer[0] = self.bytes[self.offset];
            self.offset += 1;
            Ok(1)
        }
    }

    #[derive(Default)]
    struct FragmentedWriter {
        bytes: Vec<u8>,
        interrupted: bool,
        flushed: bool,
    }

    impl Write for FragmentedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::from(ErrorKind::Interrupted));
            }

            self.bytes.push(buffer[0]);
            Ok(1)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed = true;
            Ok(())
        }
    }
}
