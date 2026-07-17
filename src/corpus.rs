use std::{
    error::Error,
    ffi::OsStr,
    fmt,
    io::{self, BufRead, Write},
    os::windows::ffi::OsStrExt,
    str,
    time::Instant,
};

use crate::{
    hover::PhysicalScreenPoint,
    resolver::{ExplorerResolver, ResolverError},
};

const MAX_REQUEST_BYTES: usize = 128;
const REQUEST_FIELDS: usize = 3;

pub(crate) fn run_probe<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> Result<(), CorpusError> {
    let mut resolver = ExplorerResolver::initialize()?;

    while let Some(line) = read_bounded_line(reader)? {
        let request = parse_request(&line)?;
        let started = Instant::now();
        let observation = resolver.observe(request.point);
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);

        write!(
            writer,
            "{}\t{}\t{}\t",
            request.case_id, observation.status, elapsed_us
        )?;
        if let Some(path) = observation.path {
            write_utf16_hex(writer, path.as_os_str())?;
        }
        writeln!(
            writer,
            "\t{}\t{}\t{}",
            observation.reason, observation.context_a, observation.context_b
        )?;
        writer.flush()?;
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProbeRequest {
    case_id: u64,
    point: PhysicalScreenPoint,
}

fn parse_request(bytes: &[u8]) -> Result<ProbeRequest, CorpusError> {
    let line = str::from_utf8(bytes).map_err(|_| CorpusError::RequestIsNotUtf8)?;
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != REQUEST_FIELDS {
        return Err(CorpusError::InvalidFieldCount {
            expected: REQUEST_FIELDS,
            actual: fields.len(),
        });
    }

    let case_id = fields[0].parse().map_err(|_| CorpusError::InvalidCaseId)?;
    let x = fields[1]
        .parse()
        .map_err(|_| CorpusError::InvalidCoordinate("x"))?;
    let y = fields[2]
        .parse()
        .map_err(|_| CorpusError::InvalidCoordinate("y"))?;

    Ok(ProbeRequest {
        case_id,
        point: PhysicalScreenPoint::new(x, y),
    })
}

fn read_bounded_line<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>, CorpusError> {
    let mut line = Vec::with_capacity(MAX_REQUEST_BYTES);

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        if line.len() + content_len > MAX_REQUEST_BYTES {
            return Err(CorpusError::RequestTooLong);
        }
        line.extend_from_slice(&available[..content_len]);

        let consumed = content_len + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }

    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

fn write_utf16_hex<W: Write>(writer: &mut W, path: &OsStr) -> io::Result<()> {
    for unit in path.encode_wide() {
        write!(writer, "{unit:04X}")?;
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum CorpusError {
    Io(io::Error),
    Resolver(ResolverError),
    RequestTooLong,
    RequestIsNotUtf8,
    InvalidFieldCount { expected: usize, actual: usize },
    InvalidCaseId,
    InvalidCoordinate(&'static str),
}

impl fmt::Display for CorpusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Resolver(error) => write!(formatter, "{error}"),
            Self::RequestTooLong => {
                write!(
                    formatter,
                    "probe request exceeds the {MAX_REQUEST_BYTES}-byte cap"
                )
            }
            Self::RequestIsNotUtf8 => write!(formatter, "probe request is not valid UTF-8"),
            Self::InvalidFieldCount { expected, actual } => write!(
                formatter,
                "probe request has {actual} fields, expected {expected}"
            ),
            Self::InvalidCaseId => write!(formatter, "probe request has an invalid case ID"),
            Self::InvalidCoordinate(axis) => {
                write!(formatter, "probe request has an invalid {axis} coordinate")
            }
        }
    }
}

impl Error for CorpusError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Resolver(error) => Some(error),
            Self::RequestTooLong
            | Self::RequestIsNotUtf8
            | Self::InvalidFieldCount { .. }
            | Self::InvalidCaseId
            | Self::InvalidCoordinate(_) => None,
        }
    }
}

impl From<io::Error> for CorpusError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ResolverError> for CorpusError {
    fn from(error: ResolverError) -> Self {
        Self::Resolver(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CorpusError, MAX_REQUEST_BYTES, ProbeRequest, parse_request, read_bounded_line,
        write_utf16_hex,
    };
    use crate::hover::PhysicalScreenPoint;
    use std::{ffi::OsStr, io::Cursor};

    #[test]
    fn request_parser_accepts_full_coordinate_range() {
        assert_eq!(
            parse_request(b"18446744073709551615\t-2147483648\t2147483647").unwrap(),
            ProbeRequest {
                case_id: u64::MAX,
                point: PhysicalScreenPoint::new(i32::MIN, i32::MAX),
            }
        );
    }

    #[test]
    fn request_parser_rejects_missing_extra_and_non_numeric_fields() {
        assert!(matches!(
            parse_request(b"1\t2"),
            Err(CorpusError::InvalidFieldCount {
                expected: 3,
                actual: 2
            })
        ));
        assert!(matches!(
            parse_request(b"1\t2\t3\textra"),
            Err(CorpusError::InvalidFieldCount {
                expected: 3,
                actual: 4
            })
        ));
        assert!(matches!(
            parse_request(b"case\t2\t3"),
            Err(CorpusError::InvalidCaseId)
        ));
        assert!(matches!(
            parse_request(b"1\tx\t3"),
            Err(CorpusError::InvalidCoordinate("x"))
        ));
        assert!(matches!(
            parse_request(b"1\t2\ty"),
            Err(CorpusError::InvalidCoordinate("y"))
        ));
    }

    #[test]
    fn bounded_reader_handles_crlf_eof_and_limit() {
        let mut reader = Cursor::new(b"1\t2\t3\r\n4\t5\t6".to_vec());
        assert_eq!(
            read_bounded_line(&mut reader).unwrap(),
            Some(b"1\t2\t3".to_vec())
        );
        assert_eq!(
            read_bounded_line(&mut reader).unwrap(),
            Some(b"4\t5\t6".to_vec())
        );
        assert_eq!(read_bounded_line(&mut reader).unwrap(), None);

        let mut exact = Cursor::new(vec![b'x'; MAX_REQUEST_BYTES]);
        assert_eq!(
            read_bounded_line(&mut exact).unwrap(),
            Some(vec![b'x'; MAX_REQUEST_BYTES])
        );

        let mut oversized = Cursor::new(vec![b'x'; MAX_REQUEST_BYTES + 1]);
        assert!(matches!(
            read_bounded_line(&mut oversized),
            Err(CorpusError::RequestTooLong)
        ));
    }

    #[test]
    fn path_encoding_is_lossless_uppercase_utf16() {
        let mut encoded = Vec::new();
        write_utf16_hex(&mut encoded, OsStr::new(r"C:\样本\😀.txt")).unwrap();
        assert_eq!(
            String::from_utf8(encoded).unwrap(),
            "0043003A005C6837672C005CD83DDE00002E007400780074"
        );
    }
}
