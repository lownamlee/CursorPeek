use std::{
    error::Error,
    fmt,
    io::{self, ErrorKind, Read, Write},
};

use crate::{
    ExplorerWindowId, Generation, LegacyEncoding, PhysicalScreenPoint, PhysicalScreenRect,
    PhysicalScreenSpan,
    payload::{
        MAX_RESULT_PAYLOAD_LEN, MIN_PREVIEW_RESULT_LEN, PayloadError, PreviewResult, decode_result,
        encode_result,
    },
};

const MAGIC: [u8; 4] = *b"CPWK";
const VERSION: u16 = 10;
const HEADER_LEN: usize = 24;
const NONCE_LEN: usize = 16;
const CACHE_ENTRIES_LEN: usize = 2;
const MAX_LEGACY_ENCODING_WIRE_LEN: usize = 40;
const TARGET_BOUNDS_FLAG_LEN: usize = 1;
const TARGET_BOUNDS_LEN: usize = 16;
const MIN_PREVIEW_RESPONSE_LEN: usize = TARGET_BOUNDS_FLAG_LEN + MIN_PREVIEW_RESULT_LEN;
const MAX_PREVIEW_RESPONSE_LEN: usize =
    TARGET_BOUNDS_FLAG_LEN + TARGET_BOUNDS_LEN + MAX_RESULT_PAYLOAD_LEN;
const MAX_PROTOCOL_PAYLOAD_LEN: usize = MAX_PREVIEW_RESPONSE_LEN;

pub const DEFAULT_PREVIEW_CACHE_ENTRIES: u16 = 128;
pub const MAX_PREVIEW_CACHE_ENTRIES: u16 = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionNonce([u8; NONCE_LEN]);

impl SessionNonce {
    pub const fn from_bytes(bytes: [u8; NONCE_LEN]) -> Self {
        Self(bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerMessage {
    Hello {
        nonce: SessionNonce,
        cache_entries: u16,
        legacy_encoding: LegacyEncoding,
    },
    Ready {
        nonce: SessionNonce,
    },
    ResolvePoint {
        generation: Generation,
        point: PhysicalScreenPoint,
        explorer_window: Option<ExplorerWindowId>,
        pointer_span: PhysicalScreenSpan,
    },
    PreviewResult {
        generation: Generation,
        target_bounds: Option<PhysicalScreenRect>,
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
            Self::Hello => (
                NONCE_LEN + CACHE_ENTRIES_LEN + 3,
                NONCE_LEN + CACHE_ENTRIES_LEN + MAX_LEGACY_ENCODING_WIRE_LEN,
            ),
            Self::Ready => (NONCE_LEN, NONCE_LEN),
            Self::ResolvePoint => (32, 32),
            Self::PreviewResult => (MIN_PREVIEW_RESPONSE_LEN, MAX_PREVIEW_RESPONSE_LEN),
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
        if payload_len > MAX_PROTOCOL_PAYLOAD_LEN as u32 {
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
pub enum ProtocolError {
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
    InvalidLegacyEncoding,
    InvalidCacheEntries(u16),
    InvalidPointerSpan,
    InvalidTargetBoundsFlag(u8),
    InvalidTargetBounds,
    MissingTargetBounds,
    UnexpectedTargetBounds,
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
            Self::InvalidLegacyEncoding => {
                write!(formatter, "invalid legacy encoding in worker handshake")
            }
            Self::InvalidCacheEntries(entries) => {
                write!(
                    formatter,
                    "worker cache entry limit {entries} exceeds {MAX_PREVIEW_CACHE_ENTRIES}"
                )
            }
            Self::InvalidPointerSpan => {
                write!(
                    formatter,
                    "worker request contains an unordered pointer span"
                )
            }
            Self::InvalidTargetBoundsFlag(flag) => {
                write!(formatter, "invalid target-bounds presence flag {flag}")
            }
            Self::InvalidTargetBounds => {
                write!(formatter, "worker returned an unordered target rectangle")
            }
            Self::MissingTargetBounds => {
                write!(
                    formatter,
                    "successful preview result omitted its target rectangle"
                )
            }
            Self::UnexpectedTargetBounds => {
                write!(
                    formatter,
                    "status result unexpectedly included a target rectangle"
                )
            }
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
pub enum ProtocolStreamError {
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

pub fn read_message<R: Read>(reader: &mut R) -> Result<Option<WorkerMessage>, ProtocolStreamError> {
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

pub fn write_message<W: Write>(
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
        WorkerMessage::Hello {
            nonce,
            cache_entries,
            legacy_encoding,
        } => {
            if cache_entries > MAX_PREVIEW_CACHE_ENTRIES {
                return Err(ProtocolError::InvalidCacheEntries(cache_entries));
            }
            let label = legacy_encoding.as_str().as_bytes();
            if !(3..=MAX_LEGACY_ENCODING_WIRE_LEN).contains(&label.len())
                || LegacyEncoding::parse(legacy_encoding.as_str()).as_ref()
                    != Some(&legacy_encoding)
            {
                return Err(ProtocolError::InvalidLegacyEncoding);
            }
            let mut payload = Vec::with_capacity(NONCE_LEN + CACHE_ENTRIES_LEN + label.len());
            payload.extend_from_slice(&nonce.0);
            payload.extend_from_slice(&cache_entries.to_le_bytes());
            payload.extend_from_slice(label);
            payload
        }
        WorkerMessage::Ready { nonce } => nonce.0.to_vec(),
        WorkerMessage::ResolvePoint {
            point,
            explorer_window,
            pointer_span,
            ..
        } => {
            if !pointer_span.contains(point) {
                return Err(ProtocolError::InvalidPointerSpan);
            }
            let mut payload = Vec::with_capacity(32);
            payload.extend_from_slice(&point.x.to_le_bytes());
            payload.extend_from_slice(&point.y.to_le_bytes());
            payload.extend_from_slice(
                &explorer_window
                    .map_or(0, ExplorerWindowId::get)
                    .to_le_bytes(),
            );
            payload.extend_from_slice(&pointer_span.min_x().to_le_bytes());
            payload.extend_from_slice(&pointer_span.min_y().to_le_bytes());
            payload.extend_from_slice(&pointer_span.max_x().to_le_bytes());
            payload.extend_from_slice(&pointer_span.max_y().to_le_bytes());
            payload
        }
        WorkerMessage::PreviewResult {
            target_bounds,
            result,
            ..
        } => encode_preview_response(target_bounds, &result)?,
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
        MessageKind::Hello => {
            let mut nonce = [0_u8; NONCE_LEN];
            nonce.copy_from_slice(&payload[..NONCE_LEN]);
            let nonce = SessionNonce(nonce);
            let cache_entries = u16::from_le_bytes([payload[NONCE_LEN], payload[NONCE_LEN + 1]]);
            if cache_entries > MAX_PREVIEW_CACHE_ENTRIES {
                return Err(ProtocolError::InvalidCacheEntries(cache_entries));
            }
            let label = std::str::from_utf8(&payload[NONCE_LEN + CACHE_ENTRIES_LEN..])
                .map_err(|_| ProtocolError::InvalidLegacyEncoding)?;
            let legacy_encoding =
                LegacyEncoding::parse(label).ok_or(ProtocolError::InvalidLegacyEncoding)?;
            Ok(WorkerMessage::Hello {
                nonce,
                cache_entries,
                legacy_encoding,
            })
        }
        MessageKind::Ready => {
            let mut nonce = [0_u8; NONCE_LEN];
            nonce.copy_from_slice(payload);
            Ok(WorkerMessage::Ready {
                nonce: SessionNonce(nonce),
            })
        }
        MessageKind::ResolvePoint => {
            let point = PhysicalScreenPoint::new(read_i32(payload, 0), read_i32(payload, 4));
            let explorer_window = ExplorerWindowId::try_from_raw(read_u64(payload, 8));
            let pointer_span = PhysicalScreenSpan::try_new(
                read_i32(payload, 16),
                read_i32(payload, 20),
                read_i32(payload, 24),
                read_i32(payload, 28),
            )
            .ok_or(ProtocolError::InvalidPointerSpan)?;
            if !pointer_span.contains(point) {
                return Err(ProtocolError::InvalidPointerSpan);
            }
            Ok(WorkerMessage::ResolvePoint {
                generation: header.generation,
                point,
                explorer_window,
                pointer_span,
            })
        }
        MessageKind::PreviewResult => {
            let (target_bounds, result) = decode_preview_response(payload)?;
            Ok(WorkerMessage::PreviewResult {
                generation: header.generation,
                target_bounds,
                result,
            })
        }
    }
}

fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated fixed-size protocol payload contains a complete i32"),
    )
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("validated fixed-size protocol payload contains a complete u64"),
    )
}

fn encode_preview_response(
    target_bounds: Option<PhysicalScreenRect>,
    result: &PreviewResult,
) -> Result<Vec<u8>, ProtocolError> {
    validate_target_bounds(target_bounds, result)?;
    let encoded_result = encode_result(result)?;
    let mut payload = Vec::with_capacity(
        TARGET_BOUNDS_FLAG_LEN
            + target_bounds.map_or(0, |_| TARGET_BOUNDS_LEN)
            + encoded_result.len(),
    );
    match target_bounds {
        Some(bounds) => {
            payload.push(1);
            payload.extend_from_slice(&bounds.left().to_le_bytes());
            payload.extend_from_slice(&bounds.top().to_le_bytes());
            payload.extend_from_slice(&bounds.right().to_le_bytes());
            payload.extend_from_slice(&bounds.bottom().to_le_bytes());
        }
        None => payload.push(0),
    }
    payload.extend_from_slice(&encoded_result);
    Ok(payload)
}

fn decode_preview_response(
    payload: &[u8],
) -> Result<(Option<PhysicalScreenRect>, PreviewResult), ProtocolError> {
    let (target_bounds, result_offset) = match payload[0] {
        0 => (None, TARGET_BOUNDS_FLAG_LEN),
        1 => {
            if payload.len() < TARGET_BOUNDS_FLAG_LEN + TARGET_BOUNDS_LEN {
                return Err(ProtocolError::InvalidTargetBounds);
            }
            let edge = |offset| {
                i32::from_le_bytes(
                    payload[offset..offset + 4]
                        .try_into()
                        .expect("the target-bounds length check guarantees four bytes"),
                )
            };
            let bounds = PhysicalScreenRect::try_new(edge(1), edge(5), edge(9), edge(13))
                .ok_or(ProtocolError::InvalidTargetBounds)?;
            (Some(bounds), TARGET_BOUNDS_FLAG_LEN + TARGET_BOUNDS_LEN)
        }
        flag => return Err(ProtocolError::InvalidTargetBoundsFlag(flag)),
    };
    let result = decode_result(&payload[result_offset..])?;
    validate_target_bounds(target_bounds, &result)?;
    Ok((target_bounds, result))
}

fn validate_target_bounds(
    target_bounds: Option<PhysicalScreenRect>,
    result: &PreviewResult,
) -> Result<(), ProtocolError> {
    match (target_bounds, result) {
        (
            Some(_),
            PreviewResult::Text(_) | PreviewResult::Image(_) | PreviewResult::Vector(_),
        )
        | (None, PreviewResult::Status(_)) => Ok(()),
        (None, PreviewResult::Text(_) | PreviewResult::Image(_) | PreviewResult::Vector(_)) => {
            Err(ProtocolError::MissingTargetBounds)
        }
        (Some(_), PreviewResult::Status(_)) => Err(ProtocolError::UnexpectedTargetBounds),
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
        DEFAULT_PREVIEW_CACHE_ENTRIES, HEADER_LEN, MAGIC, MAX_PREVIEW_CACHE_ENTRIES,
        MAX_PROTOCOL_PAYLOAD_LEN, NONCE_LEN, ProtocolError, ProtocolStreamError, SessionNonce,
        VERSION, WorkerMessage, decode_frame, encode_message, read_message, write_message,
    };
    use crate::{
        ExplorerWindowId, Generation, LegacyEncoding, PhysicalScreenPoint, PhysicalScreenRect,
        PhysicalScreenSpan,
        payload::{PayloadError, PreviewResult, ResolverStatus, TextPreview},
    };
    use std::io::{self, ErrorKind, Read, Write};

    const NONCE: SessionNonce = SessionNonce::from_bytes([
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ]);

    fn messages() -> Vec<WorkerMessage> {
        vec![
            hello(LegacyEncoding::Auto),
            WorkerMessage::Ready { nonce: NONCE },
            request_message(),
            WorkerMessage::PreviewResult {
                generation: Generation::from_raw(u64::MAX),
                target_bounds: None,
                result: PreviewResult::Status(ResolverStatus::TimedOut),
            },
        ]
    }

    fn hello(legacy_encoding: LegacyEncoding) -> WorkerMessage {
        hello_with_cache(legacy_encoding, DEFAULT_PREVIEW_CACHE_ENTRIES)
    }

    fn hello_with_cache(legacy_encoding: LegacyEncoding, cache_entries: u16) -> WorkerMessage {
        WorkerMessage::Hello {
            nonce: NONCE,
            cache_entries,
            legacy_encoding,
        }
    }

    fn request_message() -> WorkerMessage {
        WorkerMessage::ResolvePoint {
            generation: Generation::from_raw(0x0102_0304_0506_0708),
            point: PhysicalScreenPoint::new(-2, 0x0102_0304),
            explorer_window: ExplorerWindowId::try_from_raw(0x1112_1314_1516_1718),
            pointer_span: PhysicalScreenSpan::try_new(-10, -20, 30, 0x0102_0304).unwrap(),
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
    fn handshake_round_trips_each_legacy_encoding_policy() {
        for cache_entries in [0, DEFAULT_PREVIEW_CACHE_ENTRIES, MAX_PREVIEW_CACHE_ENTRIES] {
            for policy in [
                LegacyEncoding::Auto,
                LegacyEncoding::System,
                LegacyEncoding::Off,
                LegacyEncoding::Label("windows-1252".to_owned()),
            ] {
                let message = hello_with_cache(policy, cache_entries);
                let encoded = encode_message(message.clone()).unwrap();
                assert_eq!(decode_frame(encoded.as_bytes()), Ok(message));
            }
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
            ResolverStatus::PointerMoved,
        ] {
            let message = WorkerMessage::PreviewResult {
                generation: Generation::from_raw(9),
                target_bounds: None,
                result: PreviewResult::Status(status),
            };
            let encoded = encode_message(message.clone()).unwrap();

            assert_eq!(encoded.as_bytes().len(), HEADER_LEN + 9);
            assert_eq!(decode_frame(encoded.as_bytes()), Ok(message));
        }
    }

    #[test]
    fn successful_previews_require_ordered_target_bounds() {
        let preview = PreviewResult::Text(TextPreview {
            file_size: 6,
            last_write_time: 10,
            linked_content: false,
            encoding_was_guessed: false,
            truncated: false,
            display_name: "sample.txt".to_owned(),
            encoding: "utf-8".to_owned(),
            text: "sample".to_owned(),
        });
        let bounds = PhysicalScreenRect::try_new(-100, 20, 300, 400).unwrap();
        let message = WorkerMessage::PreviewResult {
            generation: Generation::from_raw(11),
            target_bounds: Some(bounds),
            result: preview.clone(),
        };
        let encoded = encode_message(message.clone()).unwrap();
        assert_eq!(decode_frame(encoded.as_bytes()), Ok(message));

        assert!(matches!(
            encode_message(WorkerMessage::PreviewResult {
                generation: Generation::from_raw(11),
                target_bounds: None,
                result: preview,
            }),
            Err(ProtocolError::MissingTargetBounds)
        ));
        assert!(matches!(
            encode_message(WorkerMessage::PreviewResult {
                generation: Generation::from_raw(11),
                target_bounds: Some(bounds),
                result: PreviewResult::Status(ResolverStatus::Unavailable),
            }),
            Err(ProtocolError::UnexpectedTargetBounds)
        ));
    }

    #[test]
    fn malformed_target_bound_envelopes_fail_closed() {
        let status = encode_message(WorkerMessage::PreviewResult {
            generation: Generation::from_raw(1),
            target_bounds: None,
            result: PreviewResult::Status(ResolverStatus::Unavailable),
        })
        .unwrap();
        let mut bad_flag = status.bytes;
        bad_flag[HEADER_LEN] = 2;
        assert_eq!(
            decode_frame(&bad_flag),
            Err(ProtocolError::InvalidTargetBoundsFlag(2))
        );

        let preview = PreviewResult::Text(TextPreview {
            file_size: 1,
            last_write_time: 0,
            linked_content: false,
            encoding_was_guessed: false,
            truncated: false,
            display_name: "a.txt".to_owned(),
            encoding: "utf-8".to_owned(),
            text: "a".to_owned(),
        });
        let bounds = PhysicalScreenRect::try_new(10, 20, 30, 40).unwrap();
        let encoded = encode_message(WorkerMessage::PreviewResult {
            generation: Generation::from_raw(2),
            target_bounds: Some(bounds),
            result: preview,
        })
        .unwrap();
        let mut inverted = encoded.bytes;
        inverted[HEADER_LEN + 9..HEADER_LEN + 13].copy_from_slice(&10_i32.to_le_bytes());
        assert_eq!(
            decode_frame(&inverted),
            Err(ProtocolError::InvalidTargetBounds)
        );
    }

    #[test]
    fn unordered_pointer_spans_fail_closed() {
        let encoded = encode_message(request_message()).unwrap();
        let mut inverted = encoded.bytes;
        inverted[HEADER_LEN + 16..HEADER_LEN + 20].copy_from_slice(&31_i32.to_le_bytes());

        assert_eq!(
            decode_frame(&inverted),
            Err(ProtocolError::InvalidPointerSpan)
        );

        let mut missing_point = encode_message(request_message()).unwrap().bytes;
        missing_point[HEADER_LEN + 16..HEADER_LEN + 20].copy_from_slice(&(-1_i32).to_le_bytes());
        assert_eq!(
            decode_frame(&missing_point),
            Err(ProtocolError::InvalidPointerSpan)
        );
    }

    #[test]
    fn encoding_has_a_stable_little_endian_layout() {
        let encoded = encode_message(request_message()).unwrap();
        let bytes = encoded.as_bytes();

        assert_eq!(&bytes[..4], &MAGIC);
        assert_eq!(&bytes[4..6], &VERSION.to_le_bytes());
        assert_eq!(&bytes[6..8], &3_u16.to_le_bytes());
        assert_eq!(&bytes[8..12], &32_u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &[0; 4]);
        assert_eq!(&bytes[16..24], &0x0102_0304_0506_0708_u64.to_le_bytes());
        assert_eq!(&bytes[24..28], &(-2_i32).to_le_bytes());
        assert_eq!(&bytes[28..32], &0x0102_0304_i32.to_le_bytes());
        assert_eq!(&bytes[32..40], &0x1112_1314_1516_1718_u64.to_le_bytes());
        assert_eq!(&bytes[40..44], &(-10_i32).to_le_bytes());
        assert_eq!(&bytes[44..48], &(-20_i32).to_le_bytes());
        assert_eq!(&bytes[48..52], &30_i32.to_le_bytes());
        assert_eq!(&bytes[52..56], &0x0102_0304_i32.to_le_bytes());

        let encoded = encode_message(hello(LegacyEncoding::Auto)).unwrap();
        let bytes = encoded.as_bytes();
        assert_eq!(&bytes[8..12], &22_u32.to_le_bytes());
        assert_eq!(
            &bytes[HEADER_LEN + NONCE_LEN..HEADER_LEN + NONCE_LEN + 2],
            &DEFAULT_PREVIEW_CACHE_ENTRIES.to_le_bytes()
        );
        assert_eq!(&bytes[HEADER_LEN + NONCE_LEN + 2..], b"auto");
    }

    #[test]
    fn malformed_headers_are_rejected_before_payload_use() {
        let encoded = encode_message(hello(LegacyEncoding::Auto)).unwrap();

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
        let oversized_length = u32::try_from(MAX_PROTOCOL_PAYLOAD_LEN + 1).unwrap();
        oversized[8..12].copy_from_slice(&oversized_length.to_le_bytes());
        assert_eq!(
            decode_frame(&oversized),
            Err(ProtocolError::PayloadTooLarge(oversized_length))
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

        let encoded_hello = encode_message(hello(LegacyEncoding::Auto)).unwrap();
        let mut bad_payload_len = encoded_hello.bytes.clone();
        bad_payload_len[8..12].copy_from_slice(&15_u32.to_le_bytes());
        assert_eq!(
            decode_frame(&bad_payload_len),
            Err(ProtocolError::InvalidPayloadLength {
                minimum: 21,
                maximum: 58,
                actual: 15,
            })
        );

        let mut invalid_legacy = encoded_hello.bytes.clone();
        invalid_legacy[HEADER_LEN + NONCE_LEN + 2..].copy_from_slice(b"nope");
        assert_eq!(
            decode_frame(&invalid_legacy),
            Err(ProtocolError::InvalidLegacyEncoding)
        );
        assert!(matches!(
            encode_message(hello(LegacyEncoding::Label("nope".to_owned()))),
            Err(ProtocolError::InvalidLegacyEncoding)
        ));

        let invalid_cache_entries = MAX_PREVIEW_CACHE_ENTRIES + 1;
        let mut invalid_cache = encoded_hello.bytes.clone();
        invalid_cache[HEADER_LEN + NONCE_LEN..HEADER_LEN + NONCE_LEN + 2]
            .copy_from_slice(&invalid_cache_entries.to_le_bytes());
        assert_eq!(
            decode_frame(&invalid_cache),
            Err(ProtocolError::InvalidCacheEntries(invalid_cache_entries))
        );
        assert!(matches!(
            encode_message(WorkerMessage::Hello {
                nonce: NONCE,
                cache_entries: invalid_cache_entries,
                legacy_encoding: LegacyEncoding::Auto,
            }),
            Err(ProtocolError::InvalidCacheEntries(entries))
                if entries == invalid_cache_entries
        ));

        let mut trailing = encoded_hello.as_bytes().to_vec();
        trailing.push(0);
        assert_eq!(
            decode_frame(&trailing),
            Err(ProtocolError::FrameLengthMismatch {
                expected: encoded_hello.bytes.len(),
                actual: encoded_hello.bytes.len() + 1,
            })
        );

        let result = encode_message(WorkerMessage::PreviewResult {
            generation: Generation::from_raw(7),
            target_bounds: None,
            result: PreviewResult::Status(ResolverStatus::Resolved),
        })
        .unwrap();
        let mut bad_status = result.bytes;
        bad_status[HEADER_LEN + 5..HEADER_LEN + 9].copy_from_slice(&99_u32.to_le_bytes());
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
            explorer_window: None,
            pointer_span: PhysicalScreenSpan::from_point(PhysicalScreenPoint::new(
                i32::MIN,
                i32::MAX,
            )),
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
            target_bounds: PhysicalScreenRect::try_new(-20, 30, 200, 300),
            result: PreviewResult::Text(TextPreview {
                file_size: 1_000_000,
                last_write_time: 133_000_000_000_000_000,
                linked_content: false,
                encoding_was_guessed: false,
                truncated: true,
                display_name: "sample.txt".to_owned(),
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

        let encoded = encode_message(hello(LegacyEncoding::Auto)).unwrap();
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
