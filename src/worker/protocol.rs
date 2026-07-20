use std::{
    error::Error,
    fmt,
    io::{self, ErrorKind, Read, Write},
};

use crate::hover::{Generation, PhysicalScreenPoint};

use super::payload::{
    MAX_PREVIEW_PAYLOAD_LEN, MIN_PREVIEW_RESULT_LEN, PayloadError, PreviewResult, decode_result,
    encode_result,
};

const MAGIC: [u8; 4] = *b"CPWK";
const VERSION: u16 = 2;
const HEADER_LEN: usize = 24;
const NONCE_LEN: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SessionNonce([u8; NONCE_LEN]);

impl SessionNonce {
    pub(super) const fn from_bytes(bytes: [u8; NONCE_LEN]) -> Self {
        Self(bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
    PreviewResult {
        generation: Generation,
        result: PreviewResult,
    },
}

impl WorkerMessage {
    fn kind(&self) -> MessageKind {
        match self {
            Self::Hello { .. } => MessageKind::Hello,
            Self::Ready { .. } => MessageKind::Ready,
            Self::ResolvePoint { .. } => MessageKind::ResolvePoint,
            Self::PreviewResult { .. } => MessageKind::PreviewResult,
        }
    }

    fn generation(&self) -> Generation {
        match self {
            Self::Hello { .. } | Self::Ready { .. } => Generation::from_raw(0),
            Self::ResolvePoint { generation, .. } | Self::PreviewResult { generation, .. } => {
                *generation
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageKind {
    Hello = 1,
    Ready = 2,
    ResolvePoint = 3,
    PreviewResult = 4,
}

impl MessageKind {
    fn from_raw(value: u16) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Ready),
            3 => Ok(Self::ResolvePoint),
            4 => Ok(Self::PreviewResult),
            _ => Err(ProtocolError::UnknownMessageKind(value)),
        }
    }

    fn payload_limits(self) -> (usize, usize) {
        match self {
            Self::Hello | Self::Ready => (NONCE_LEN, NONCE_LEN),
            Self::ResolvePoint => (8, 8),
            Self::PreviewResult => (MIN_PREVIEW_RESULT_LEN, MAX_PREVIEW_PAYLOAD_LEN),
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
        if payload_len > MAX_PREVIEW_PAYLOAD_LEN as u32 {
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
        let (minimum, maximum) = kind.payload_limits();
        if !(minimum..=maximum).contains(&payload_len) {
            return Err(ProtocolError::InvalidPayloadLength {
                minimum,
                maximum,
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
    #[cfg(test)]
    TruncatedHeader {
        actual: usize,
    },
    InvalidMagic,
    UnsupportedVersion(u16),
    UnknownMessageKind(u16),
    PayloadTooLarge(u32),
    ReservedFieldSet(u32),
    HandshakeGeneration(u64),
    InvalidPayloadLength {
        minimum: usize,
        maximum: usize,
        actual: usize,
    },
    FrameLengthMismatch {
        expected: usize,
        actual: usize,
    },
    Payload(PayloadError),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(test)]
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
            Self::InvalidPayloadLength {
                minimum,
                maximum,
                actual,
            } if minimum == maximum => write!(
                formatter,
                "invalid worker payload length: expected {minimum} bytes, received {actual}"
            ),
            Self::InvalidPayloadLength {
                minimum,
                maximum,
                actual,
            } => write!(
                formatter,
                "invalid worker payload length: expected {minimum}-{maximum} bytes, \
                 received {actual}"
            ),
            Self::FrameLengthMismatch { expected, actual } => write!(
                formatter,
                "worker frame length mismatch: expected {expected} bytes, received {actual}"
            ),
            Self::Payload(error) => write!(formatter, "invalid preview payload: {error}"),
        }
    }
}

impl Error for ProtocolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Payload(error) => Some(error),
            _ => None,
        }
    }
}

impl From<PayloadError> for ProtocolError {
    fn from(error: PayloadError) -> Self {
        Self::Payload(error)
    }
}

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
    let mut header_bytes = [0_u8; HEADER_LEN];
    if !read_first_byte(reader, &mut header_bytes[0])? {
        return Ok(None);
    }
    read_exact_frame(reader, &mut header_bytes[1..])?;

    let header = FrameHeader::decode(&header_bytes)?;
    let mut payload = vec![0_u8; header.payload_len];
    read_exact_frame(reader, &mut payload)?;

    decode_payload(header, &payload)
        .map(Some)
        .map_err(Into::into)
}

pub(super) fn write_message<W: Write>(
    writer: &mut W,
    message: WorkerMessage,
) -> Result<(), ProtocolStreamError> {
    let encoded = encode_message(message)?;
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

struct EncodedMessage {
    bytes: Vec<u8>,
}

impl EncodedMessage {
    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn encode_message(message: WorkerMessage) -> Result<EncodedMessage, ProtocolError> {
    let kind = message.kind();
    let generation = message.generation();
    let payload = match message {
        WorkerMessage::Hello { nonce } | WorkerMessage::Ready { nonce } => nonce.0.to_vec(),
        WorkerMessage::ResolvePoint { point, .. } => {
            let mut payload = Vec::with_capacity(8);
            payload.extend_from_slice(&point.x.to_le_bytes());
            payload.extend_from_slice(&point.y.to_le_bytes());
            payload
        }
        WorkerMessage::PreviewResult { result, .. } => encode_result(&result)?,
    };
    let payload_len =
        u32::try_from(payload.len()).map_err(|_| ProtocolError::PayloadTooLarge(u32::MAX))?;
    let mut bytes = Vec::with_capacity(
        HEADER_LEN
            .checked_add(payload.len())
            .expect("the bounded payload length fits usize"),
    );
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&(kind as u16).to_le_bytes());
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&generation.get().to_le_bytes());
    bytes.extend_from_slice(&payload);

    let header_bytes: &[u8; HEADER_LEN] = bytes[..HEADER_LEN]
        .try_into()
        .expect("the encoder always emits a complete fixed header");
    FrameHeader::decode(header_bytes)?;
    Ok(EncodedMessage { bytes })
}

fn decode_payload(header: FrameHeader, payload: &[u8]) -> Result<WorkerMessage, ProtocolError> {
    if payload.len() != header.payload_len {
        return Err(ProtocolError::FrameLengthMismatch {
            expected: HEADER_LEN + header.payload_len,
            actual: HEADER_LEN + payload.len(),
        });
    }

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
        MessageKind::PreviewResult => Ok(WorkerMessage::PreviewResult {
            generation: header.generation,
            result: decode_result(payload)?,
        }),
    }
}

#[cfg(test)]
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
    decode_payload(header, &bytes[HEADER_LEN..])
}

#[cfg(test)]
mod tests {
    use super::{
        HEADER_LEN, MAGIC, ProtocolError, ProtocolStreamError, SessionNonce, VERSION,
        WorkerMessage, decode_frame, encode_message, read_message, write_message,
    };
    use crate::hover::{Generation, PhysicalScreenPoint};
    use crate::worker::payload::{PayloadError, PreviewResult, ResolverStatus, TextPreview};
    use std::io::{self, ErrorKind, Read, Write};

    const NONCE: SessionNonce = SessionNonce::from_bytes([
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ]);

    fn messages() -> Vec<WorkerMessage> {
        vec![
            WorkerMessage::Hello { nonce: NONCE },
            WorkerMessage::Ready { nonce: NONCE },
            request_message(),
            WorkerMessage::PreviewResult {
                generation: Generation::from_raw(u64::MAX),
                result: PreviewResult::Status(ResolverStatus::TimedOut),
            },
        ]
    }

    fn request_message() -> WorkerMessage {
        WorkerMessage::ResolvePoint {
            generation: Generation::from_raw(0x0102_0304_0506_0708),
            point: PhysicalScreenPoint::new(-2, 0x0102_0304),
        }
    }

    #[test]
    fn every_control_message_round_trips() {
        for message in messages() {
            let encoded = encode_message(message.clone()).unwrap();
            assert_eq!(decode_frame(encoded.as_bytes()), Ok(message));
        }
    }

    #[test]
    fn every_resolver_status_round_trips_in_the_typed_result_envelope() {
        for status in [
            ResolverStatus::Resolved,
            ResolverStatus::Unsupported,
            ResolverStatus::Ambiguous,
            ResolverStatus::Unavailable,
            ResolverStatus::TimedOut,
        ] {
            let message = WorkerMessage::PreviewResult {
                generation: Generation::from_raw(9),
                result: PreviewResult::Status(status),
            };
            let encoded = encode_message(message.clone()).unwrap();

            assert_eq!(encoded.as_bytes().len(), HEADER_LEN + 8);
            assert_eq!(decode_frame(encoded.as_bytes()), Ok(message));
        }
    }

    #[test]
    fn encoding_has_a_stable_little_endian_layout() {
        let encoded = encode_message(request_message()).unwrap();
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
        let encoded = encode_message(WorkerMessage::Hello { nonce: NONCE }).unwrap();

        let mut bad_magic = encoded.bytes.clone();
        bad_magic[0] ^= 0xff;
        assert_eq!(decode_frame(&bad_magic), Err(ProtocolError::InvalidMagic));

        let mut bad_version = encoded.bytes.clone();
        bad_version[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
        assert_eq!(
            decode_frame(&bad_version),
            Err(ProtocolError::UnsupportedVersion(VERSION + 1))
        );

        let mut bad_kind = encoded.bytes.clone();
        bad_kind[6..8].copy_from_slice(&99_u16.to_le_bytes());
        assert_eq!(
            decode_frame(&bad_kind),
            Err(ProtocolError::UnknownMessageKind(99))
        );

        let mut oversized = encoded.bytes.clone();
        oversized[8..12].copy_from_slice(&(4 * 1024 * 1024_u32 + 1).to_le_bytes());
        assert_eq!(
            decode_frame(&oversized),
            Err(ProtocolError::PayloadTooLarge(4 * 1024 * 1024 + 1))
        );

        let mut reserved = encoded.bytes.clone();
        reserved[12..16].copy_from_slice(&1_u32.to_le_bytes());
        assert_eq!(
            decode_frame(&reserved),
            Err(ProtocolError::ReservedFieldSet(1))
        );

        let mut generated_handshake = encoded.bytes;
        generated_handshake[16..24].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(
            decode_frame(&generated_handshake),
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

        let hello = encode_message(WorkerMessage::Hello { nonce: NONCE }).unwrap();
        let mut bad_payload_len = hello.bytes.clone();
        bad_payload_len[8..12].copy_from_slice(&15_u32.to_le_bytes());
        assert_eq!(
            decode_frame(&bad_payload_len),
            Err(ProtocolError::InvalidPayloadLength {
                minimum: 16,
                maximum: 16,
                actual: 15,
            })
        );

        let mut trailing = hello.as_bytes().to_vec();
        trailing.push(0);
        assert_eq!(
            decode_frame(&trailing),
            Err(ProtocolError::FrameLengthMismatch {
                expected: hello.bytes.len(),
                actual: hello.bytes.len() + 1,
            })
        );

        let result = encode_message(WorkerMessage::PreviewResult {
            generation: Generation::from_raw(7),
            result: PreviewResult::Status(ResolverStatus::Resolved),
        })
        .unwrap();
        let mut bad_status = result.bytes;
        bad_status[HEADER_LEN + 4..HEADER_LEN + 8].copy_from_slice(&99_u32.to_le_bytes());
        assert_eq!(
            decode_frame(&bad_status),
            Err(ProtocolError::Payload(PayloadError::UnknownResolverStatus(
                99
            )))
        );
    }

    #[test]
    fn exact_stream_helpers_tolerate_fragmentation_and_interruption() {
        let message = WorkerMessage::ResolvePoint {
            generation: Generation::from_raw(42),
            point: PhysicalScreenPoint::new(i32::MIN, i32::MAX),
        };
        let mut writer = FragmentedWriter::default();
        write_message(&mut writer, message.clone()).unwrap();
        assert!(writer.flushed);

        let mut reader = FragmentedReader::new(&writer.bytes);
        assert_eq!(read_message(&mut reader).unwrap(), Some(message));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn stream_allocates_only_the_validated_variable_payload() {
        let message = WorkerMessage::PreviewResult {
            generation: Generation::from_raw(77),
            result: PreviewResult::Text(TextPreview {
                file_size: 1_000_000,
                linked_content: false,
                encoding_was_guessed: false,
                truncated: true,
                encoding: "utf-8".to_owned(),
                text: "bounded 世界\n".repeat(128),
            }),
        };
        let mut stream = Vec::new();
        write_message(&mut stream, message.clone()).unwrap();

        let mut stream = stream.as_slice();
        assert_eq!(read_message(&mut stream).unwrap(), Some(message));
        assert_eq!(read_message(&mut stream).unwrap(), None);
    }

    #[test]
    fn stream_distinguishes_clean_eof_from_truncation() {
        assert_eq!(read_message(&mut &[][..]).unwrap(), None);

        let encoded = encode_message(WorkerMessage::Hello { nonce: NONCE }).unwrap();
        for truncated_len in [1, HEADER_LEN - 1, HEADER_LEN, encoded.bytes.len() - 1] {
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
