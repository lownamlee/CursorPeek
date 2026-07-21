use std::path::Path;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{DecoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE, X_USER_DEFINED};
use windows::Win32::Globalization::GetACP;

use crate::settings::LegacyEncoding;

use super::{
    file::{PreviewFile, PreviewFileError},
    payload::{
        MAX_TEXT_LINES, MAX_TEXT_SCALARS, MAX_TEXT_UTF8_LEN, TextPreview,
        is_noncanonical_text_line_break, is_unsafe_text_control,
    },
};

const TEXT_SNIFF_LIMIT: usize = 64 * 1024;
const NULL_PATTERN_SAMPLE_LIMIT: usize = 4 * 1024;
const NEUTRALIZED_CONTROL: char = '\u{fffd}';

const TEXT_EXTENSIONS: &[&str] = &[
    "txt",
    "text",
    "log",
    "md",
    "csv",
    "tsv",
    "json",
    "jsonc",
    "xml",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "properties",
    "rs",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hh",
    "hpp",
    "hxx",
    "ipp",
    "inl",
    "cs",
    "java",
    "kt",
    "kts",
    "go",
    "py",
    "pyw",
    "rb",
    "php",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "mts",
    "cts",
    "tsx",
    "html",
    "htm",
    "css",
    "sql",
    "sh",
    "bash",
    "zsh",
    "ps1",
    "bat",
    "cmd",
];

const TEXT_NAMES: &[&str] = &[
    "README",
    "LICENSE",
    "COPYING",
    "NOTICE",
    "Makefile",
    "Dockerfile",
    "Gemfile",
    ".env",
    ".editorconfig",
    ".gitattributes",
    ".gitignore",
    ".dockerignore",
    ".npmrc",
    ".prettierrc",
    ".prettierignore",
    ".eslintrc",
    ".eslintignore",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum TextDecodeResult {
    Preview(TextPreview),
    Unsupported,
}

pub(super) fn decode(
    file: &PreviewFile,
    legacy_encoding: &LegacyEncoding,
) -> Result<TextDecodeResult, PreviewFileError> {
    if !is_eligible_path(file.final_path()) {
        return Ok(TextDecodeResult::Unsupported);
    }

    let bytes = file.read_prefix(TEXT_SNIFF_LIMIT)?;
    let prefix_truncated = file.file_size()
        > u64::try_from(bytes.len()).expect("the bounded text prefix length fits u64");
    let Some(kind) = classify_prefix(&bytes, prefix_truncated) else {
        return Ok(TextDecodeResult::Unsupported);
    };
    let decoded = if kind == TextByteKind::LegacyCandidate {
        decode_legacy_bytes(&bytes, prefix_truncated, legacy_encoding)
    } else {
        decode_unicode_bytes(&bytes, kind, prefix_truncated)
    };
    let Some(decoded) = decoded else {
        return Ok(TextDecodeResult::Unsupported);
    };
    let (text, output_truncated) = sanitize_and_truncate(&decoded.text);
    Ok(TextDecodeResult::Preview(TextPreview {
        file_size: file.file_size(),
        linked_content: file.is_linked_content(),
        encoding_was_guessed: decoded.encoding_was_guessed,
        truncated: prefix_truncated || decoded.incomplete_tail || output_truncated,
        encoding: decoded.encoding.to_owned(),
        text,
    }))
}

pub(super) fn is_eligible_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if TEXT_NAMES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
    {
        return true;
    }

    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            TEXT_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextByteKind {
    Utf8Bom,
    Utf16LeBom,
    Utf16BeBom,
    Utf32LeBom,
    Utf32BeBom,
    Utf8,
    Utf16LeLikely,
    Utf16BeLikely,
    Utf32LeLikely,
    Utf32BeLikely,
    LegacyCandidate,
}

struct DecodedUnicode {
    text: String,
    encoding: &'static str,
    encoding_was_guessed: bool,
    incomplete_tail: bool,
}

#[cfg(test)]
fn classify_bytes(bytes: &[u8]) -> Option<TextByteKind> {
    classify_prefix(bytes, false)
}

fn classify_prefix(bytes: &[u8], prefix_truncated: bool) -> Option<TextByteKind> {
    if bytes.starts_with(&[0xff, 0xfe, 0x00, 0x00]) {
        return Some(TextByteKind::Utf32LeBom);
    }
    if bytes.starts_with(&[0x00, 0x00, 0xfe, 0xff]) {
        return Some(TextByteKind::Utf32BeBom);
    }
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Some(TextByteKind::Utf8Bom);
    }
    if bytes.starts_with(&[0xff, 0xfe]) {
        return Some(TextByteKind::Utf16LeBom);
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        return Some(TextByteKind::Utf16BeBom);
    }
    if has_known_binary_signature(bytes) {
        return None;
    }
    if bytes.contains(&0) {
        return detect_null_pattern(bytes);
    }
    if !has_acceptable_control_density(bytes) {
        return None;
    }

    match std::str::from_utf8(bytes) {
        Ok(_) => Some(TextByteKind::Utf8),
        Err(error) if prefix_truncated && error.error_len().is_none() => Some(TextByteKind::Utf8),
        Err(_) => Some(TextByteKind::LegacyCandidate),
    }
}

fn decode_unicode_bytes(
    bytes: &[u8],
    kind: TextByteKind,
    prefix_truncated: bool,
) -> Option<DecodedUnicode> {
    let (text, encoding, incomplete_tail) = match kind {
        TextByteKind::Utf8Bom => {
            let (text, incomplete_tail) = decode_utf8(&bytes[3..], prefix_truncated)?;
            (text, "UTF-8", incomplete_tail)
        }
        TextByteKind::Utf16LeBom => {
            let (text, incomplete_tail) = decode_utf16(&bytes[2..], true, prefix_truncated)?;
            (text, "UTF-16 LE", incomplete_tail)
        }
        TextByteKind::Utf16BeBom => {
            let (text, incomplete_tail) = decode_utf16(&bytes[2..], false, prefix_truncated)?;
            (text, "UTF-16 BE", incomplete_tail)
        }
        TextByteKind::Utf32LeBom => {
            let (text, incomplete_tail) = decode_utf32(&bytes[4..], true, prefix_truncated)?;
            (text, "UTF-32 LE", incomplete_tail)
        }
        TextByteKind::Utf32BeBom => {
            let (text, incomplete_tail) = decode_utf32(&bytes[4..], false, prefix_truncated)?;
            (text, "UTF-32 BE", incomplete_tail)
        }
        TextByteKind::Utf8 => {
            let (text, incomplete_tail) = decode_utf8(bytes, prefix_truncated)?;
            (text, "UTF-8", incomplete_tail)
        }
        TextByteKind::Utf16LeLikely => {
            let (text, incomplete_tail) = decode_utf16(bytes, true, prefix_truncated)?;
            (text, "UTF-16 LE", incomplete_tail)
        }
        TextByteKind::Utf16BeLikely => {
            let (text, incomplete_tail) = decode_utf16(bytes, false, prefix_truncated)?;
            (text, "UTF-16 BE", incomplete_tail)
        }
        TextByteKind::Utf32LeLikely => {
            let (text, incomplete_tail) = decode_utf32(bytes, true, prefix_truncated)?;
            (text, "UTF-32 LE", incomplete_tail)
        }
        TextByteKind::Utf32BeLikely => {
            let (text, incomplete_tail) = decode_utf32(bytes, false, prefix_truncated)?;
            (text, "UTF-32 BE", incomplete_tail)
        }
        TextByteKind::LegacyCandidate => return None,
    };

    Some(DecodedUnicode {
        text,
        encoding,
        encoding_was_guessed: false,
        incomplete_tail,
    })
}

fn decode_legacy_bytes(
    bytes: &[u8],
    prefix_truncated: bool,
    policy: &LegacyEncoding,
) -> Option<DecodedUnicode> {
    let (encoding, encoding_was_guessed) = match policy {
        LegacyEncoding::Off => return None,
        LegacyEncoding::Auto => {
            // Strict Unicode decoding already ran. Keep UTF-8 out of the fallback guess and do
            // not auto-select the stateful ISO-2022-JP encoding for ordinary local files.
            let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
            detector.feed(bytes, !prefix_truncated);
            (detector.guess(None, Utf8Detection::Deny), true)
        }
        LegacyEncoding::System => (system_legacy_encoding()?, false),
        LegacyEncoding::Label(label) => (supported_legacy_encoding(label)?, false),
    };
    let (text, output_truncated) = decode_legacy_with_encoding(bytes, encoding, prefix_truncated)?;
    Some(DecodedUnicode {
        text,
        encoding: encoding.name(),
        encoding_was_guessed,
        incomplete_tail: output_truncated,
    })
}

fn supported_legacy_encoding(label: &str) -> Option<&'static Encoding> {
    let encoding = Encoding::for_label_no_replacement(label.as_bytes())?;
    if encoding == UTF_8
        || encoding == UTF_16LE
        || encoding == UTF_16BE
        || encoding == X_USER_DEFINED
    {
        None
    } else {
        Some(encoding)
    }
}

fn system_legacy_encoding() -> Option<&'static Encoding> {
    // GetACP is used only for the user's explicit `system` compatibility override; all normal
    // application text and rendering remain Unicode.
    // SAFETY: GetACP takes no arguments and returns the process-wide Windows ANSI code page.
    legacy_encoding_for_code_page(unsafe { GetACP() })
}

fn legacy_encoding_for_code_page(code_page: u32) -> Option<&'static Encoding> {
    let code_page = u16::try_from(code_page).ok()?;
    let encoding = codepage::to_encoding_no_replacement(code_page)?;
    if encoding == UTF_8 || encoding == UTF_16LE || encoding == UTF_16BE {
        None
    } else {
        Some(encoding)
    }
}

fn decode_legacy_with_encoding(
    bytes: &[u8],
    encoding: &'static Encoding,
    prefix_truncated: bool,
) -> Option<(String, bool)> {
    let mut decoder = encoding.new_decoder_without_bom_handling();
    let capacity = decoder
        .max_utf8_buffer_length_without_replacement(bytes.len())?
        .min(MAX_TEXT_UTF8_LEN);
    let mut text = String::with_capacity(capacity);
    let (result, read) =
        decoder.decode_to_string_without_replacement(bytes, &mut text, !prefix_truncated);
    match result {
        DecoderResult::InputEmpty if prefix_truncated && read == bytes.len() => {
            let additional = decoder.max_utf8_buffer_length_without_replacement(0)?;
            text.reserve(additional);
            let (finish, finish_read) =
                decoder.decode_to_string_without_replacement(&[], &mut text, true);
            debug_assert_eq!(finish_read, 0);
            match finish {
                DecoderResult::InputEmpty => Some((text, false)),
                DecoderResult::Malformed(_, _) => Some((text, true)),
                DecoderResult::OutputFull => None,
            }
        }
        DecoderResult::InputEmpty => Some((text, read != bytes.len())),
        DecoderResult::OutputFull => Some((text, true)),
        DecoderResult::Malformed(_, _) => None,
    }
}

fn decode_utf8(bytes: &[u8], prefix_truncated: bool) -> Option<(String, bool)> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Some((text.to_owned(), false)),
        Err(error) if prefix_truncated && error.error_len().is_none() => {
            let text = std::str::from_utf8(&bytes[..error.valid_up_to()])
                .expect("the UTF-8 validator reports a valid prefix");
            Some((text.to_owned(), true))
        }
        Err(_) => None,
    }
}

fn decode_utf16(
    bytes: &[u8],
    little_endian: bool,
    prefix_truncated: bool,
) -> Option<(String, bool)> {
    let mut usable_len = bytes.len();
    let mut incomplete_tail = false;
    if !usable_len.is_multiple_of(2) {
        if !prefix_truncated {
            return None;
        }
        usable_len -= 1;
        incomplete_tail = true;
    }

    let mut units = bytes[..usable_len]
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect::<Vec<_>>();
    if prefix_truncated
        && units
            .last()
            .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
    {
        units.pop();
        incomplete_tail = true;
    }

    let text = char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()?;
    Some((text, incomplete_tail))
}

fn decode_utf32(
    bytes: &[u8],
    little_endian: bool,
    prefix_truncated: bool,
) -> Option<(String, bool)> {
    let mut usable_len = bytes.len();
    let remainder = usable_len % 4;
    if remainder != 0 {
        if !prefix_truncated {
            return None;
        }
        usable_len -= remainder;
    }

    let mut text = String::with_capacity(usable_len);
    for quad in bytes[..usable_len].chunks_exact(4) {
        let value = if little_endian {
            u32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]])
        } else {
            u32::from_be_bytes([quad[0], quad[1], quad[2], quad[3]])
        };
        text.push(char::from_u32(value)?);
    }
    Some((text, remainder != 0))
}

fn sanitize_and_truncate(text: &str) -> (String, bool) {
    sanitize_and_truncate_with_limits(text, MAX_TEXT_UTF8_LEN, MAX_TEXT_SCALARS, MAX_TEXT_LINES)
}

fn sanitize_and_truncate_with_limits(
    text: &str,
    max_bytes: usize,
    max_scalars: usize,
    max_lines: usize,
) -> (String, bool) {
    if text.is_empty() {
        return (String::new(), false);
    }
    if max_bytes == 0 || max_scalars == 0 || max_lines == 0 {
        return (String::new(), true);
    }

    let mut output = String::with_capacity(text.len().min(max_bytes));
    let mut input = text.chars().peekable();
    let mut scalar_count = 0;
    let mut line_count = 1;

    while let Some(scalar) = input.next() {
        let sanitized = if scalar == '\r' {
            if input.peek() == Some(&'\n') {
                input.next();
            }
            '\n'
        } else if is_noncanonical_text_line_break(scalar) {
            '\n'
        } else if is_unsafe_text_control(scalar) {
            NEUTRALIZED_CONTROL
        } else {
            scalar
        };

        let scalar_len = sanitized.len_utf8();
        if scalar_count == max_scalars
            || output
                .len()
                .checked_add(scalar_len)
                .is_none_or(|length| length > max_bytes)
            || (sanitized == '\n' && line_count == max_lines)
        {
            return (output, true);
        }

        output.push(sanitized);
        scalar_count += 1;
        line_count += usize::from(sanitized == '\n');
    }

    (output, false)
}

fn has_known_binary_signature(bytes: &[u8]) -> bool {
    const PREFIXES: &[&[u8]] = &[
        b"\x89PNG\r\n\x1a\n",
        b"\xff\xd8\xff",
        b"GIF87a",
        b"GIF89a",
        b"%PDF-",
        b"PK\x03\x04",
        b"PK\x05\x06",
        b"PK\x07\x08",
        b"\x7fELF",
        b"\x1f\x8b",
        b"7z\xbc\xaf\x27\x1c",
        b"Rar!\x1a\x07",
        b"\x00asm",
        b"II\x2a\x00",
        b"MM\x00\x2a",
        b"\x00\x00\x01\x00",
        b"\x00\x00\x02\x00",
    ];

    PREFIXES.iter().any(|prefix| bytes.starts_with(prefix))
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12).is_some_and(|format| format == b"WEBP"))
}

fn has_acceptable_control_density(bytes: &[u8]) -> bool {
    let disallowed = bytes
        .iter()
        .filter(|byte| matches!(byte, 0x01..=0x08 | 0x0b | 0x0e..=0x1f | 0x7f))
        .count();
    disallowed <= 1 + bytes.len() / 20
}

fn detect_null_pattern(bytes: &[u8]) -> Option<TextByteKind> {
    let sample = &bytes[..bytes.len().min(NULL_PATTERN_SAMPLE_LIMIT)];
    if sample.len() >= 8 {
        let quads = sample.len() / 4;
        let utf32_le_zeros = sample
            .chunks_exact(4)
            .filter(|quad| quad[2] == 0 && quad[3] == 0)
            .count();
        let utf32_le_content = sample
            .chunks_exact(4)
            .filter(|quad| quad[0] != 0 || quad[1] != 0)
            .count();
        if at_least(utf32_le_zeros, quads, 7, 8) && at_least(utf32_le_content, quads, 1, 2) {
            return Some(TextByteKind::Utf32LeLikely);
        }

        let utf32_be_zeros = sample
            .chunks_exact(4)
            .filter(|quad| quad[0] == 0 && quad[1] == 0)
            .count();
        let utf32_be_content = sample
            .chunks_exact(4)
            .filter(|quad| quad[2] != 0 || quad[3] != 0)
            .count();
        if at_least(utf32_be_zeros, quads, 7, 8) && at_least(utf32_be_content, quads, 1, 2) {
            return Some(TextByteKind::Utf32BeLikely);
        }
    }

    if sample.len() >= 8 {
        let pairs = sample.len() / 2;
        let odd_zeros = sample.chunks_exact(2).filter(|pair| pair[1] == 0).count();
        let even_content = sample.chunks_exact(2).filter(|pair| pair[0] != 0).count();
        if at_least(odd_zeros, pairs, 3, 4) && at_least(even_content, pairs, 1, 2) {
            return Some(TextByteKind::Utf16LeLikely);
        }

        let even_zeros = sample.chunks_exact(2).filter(|pair| pair[0] == 0).count();
        let odd_content = sample.chunks_exact(2).filter(|pair| pair[1] != 0).count();
        if at_least(even_zeros, pairs, 3, 4) && at_least(odd_content, pairs, 1, 2) {
            return Some(TextByteKind::Utf16BeLikely);
        }
    }

    None
}

const fn at_least(count: usize, total: usize, numerator: usize, denominator: usize) -> bool {
    count * denominator >= total * numerator
}

#[cfg(test)]
mod corpus;

#[cfg(test)]
mod tests {
    use super::{
        TEXT_EXTENSIONS, TEXT_NAMES, TextByteKind, TextDecodeResult, classify_bytes,
        classify_prefix, decode, decode_legacy_bytes, decode_legacy_with_encoding,
        decode_unicode_bytes, is_eligible_path, legacy_encoding_for_code_page,
        sanitize_and_truncate, sanitize_and_truncate_with_limits,
    };
    use crate::settings::LegacyEncoding;
    use crate::worker::{
        file::PreviewFile,
        payload::{MAX_TEXT_LINES, MAX_TEXT_SCALARS},
    };
    use encoding_rs::{SHIFT_JIS, WINDOWS_1252};
    use std::{
        env, fs, io,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn eligibility_is_case_insensitive_and_bounded_to_the_declared_names() {
        for extension in TEXT_EXTENSIONS {
            let path = PathBuf::from(format!(r"C:\preview.{extension}"));
            assert!(is_eligible_path(&path), "missing extension {extension}");
        }
        for name in TEXT_NAMES {
            let path = Path::new(r"C:\preview").join(name);
            assert!(is_eligible_path(&path), "missing name {name}");
        }

        assert!(is_eligible_path(Path::new(r"C:\PREVIEW.RS")));
        assert!(is_eligible_path(Path::new(r"C:\readme")));
        assert!(is_eligible_path(Path::new(r"C:\.GITIGNORE")));
        assert!(!is_eligible_path(Path::new(r"C:\preview.png")));
        assert!(!is_eligible_path(Path::new(r"C:\unknown")));
        assert!(!is_eligible_path(Path::new(r"C:\.env.local")));
    }

    #[test]
    fn bom_and_strict_utf8_candidates_are_retained_for_unicode_decoding() {
        assert_eq!(
            classify_bytes(b"\xff\xfe\x00\x00A\x00\x00\x00"),
            Some(TextByteKind::Utf32LeBom)
        );
        assert_eq!(
            classify_bytes(b"\x00\x00\xfe\xff\x00\x00\x00A"),
            Some(TextByteKind::Utf32BeBom)
        );
        assert_eq!(
            classify_bytes(b"\xef\xbb\xbfhello"),
            Some(TextByteKind::Utf8Bom)
        );
        assert_eq!(
            classify_bytes(b"\xff\xfeA\x00"),
            Some(TextByteKind::Utf16LeBom)
        );
        assert_eq!(
            classify_bytes(b"\xfe\xff\x00A"),
            Some(TextByteKind::Utf16BeBom)
        );
        assert_eq!(
            classify_bytes("hello 世界".as_bytes()),
            Some(TextByteKind::Utf8)
        );
        assert_eq!(
            classify_prefix(b"prefix \xe2\x82", true),
            Some(TextByteKind::Utf8)
        );
    }

    #[test]
    fn strong_bomless_unicode_null_patterns_are_not_called_binary() {
        assert_eq!(
            classify_bytes(b"A\x00B\x00C\x00D\x00"),
            Some(TextByteKind::Utf16LeLikely)
        );
        assert_eq!(
            classify_bytes(b"\x00A\x00B\x00C\x00D"),
            Some(TextByteKind::Utf16BeLikely)
        );
        assert_eq!(
            classify_bytes(b"A\x00\x00\x00B\x00\x00\x00"),
            Some(TextByteKind::Utf32LeLikely)
        );
        assert_eq!(
            classify_bytes(b"\x00\x00\x00A\x00\x00\x00B"),
            Some(TextByteKind::Utf32BeLikely)
        );
    }

    #[test]
    fn known_binary_null_and_control_heavy_inputs_are_rejected() {
        for bytes in [
            &b"\x89PNG\r\n\x1a\npayload"[..],
            &b"\xff\xd8\xff\xe0payload"[..],
            &b"PK\x03\x04payload"[..],
            &b"%PDF-1.7"[..],
            &b"RIFF\x01\x02\x03\x04WEBP"[..],
            &b"plain\x00binary"[..],
            &b"\x01\x02\x03\x04\x05\x06\x07\x08"[..],
        ] {
            assert_eq!(classify_bytes(bytes), None);
        }
    }

    #[test]
    fn isolated_controls_and_legacy_high_bytes_remain_sanitizer_candidates() {
        assert_eq!(classify_bytes(b"color\x1b[31m"), Some(TextByteKind::Utf8));
        assert_eq!(
            classify_bytes(b"caf\xe9"),
            Some(TextByteKind::LegacyCandidate)
        );
    }

    #[test]
    fn decoder_reads_only_eligible_files_and_rejects_disguised_binary() {
        let root = TestDirectory::new("classify");
        let text_path = root.path().join("large.txt");
        fs::write(&text_path, vec![b'a'; 80 * 1024]).unwrap();
        let text = PreviewFile::open(&text_path).unwrap();
        let TextDecodeResult::Preview(preview) = decode(&text, &LegacyEncoding::Auto).unwrap()
        else {
            panic!("the bounded UTF-8 file should produce a preview");
        };
        assert_eq!(preview.text, "a".repeat(MAX_TEXT_SCALARS));
        assert_eq!(preview.encoding, "UTF-8");
        assert_eq!(preview.file_size, 80 * 1024);
        assert!(preview.truncated);
        assert!(!preview.encoding_was_guessed);

        let binary_path = root.path().join("disguised.txt");
        fs::write(&binary_path, b"\x89PNG\r\n\x1a\npayload").unwrap();
        let binary = PreviewFile::open(&binary_path).unwrap();
        assert_eq!(
            decode(&binary, &LegacyEncoding::Auto).unwrap(),
            TextDecodeResult::Unsupported
        );

        let image_path = root.path().join("image.png");
        fs::write(&image_path, b"not read by the text classifier").unwrap();
        let image = PreviewFile::open(&image_path).unwrap();
        assert_eq!(
            decode(&image, &LegacyEncoding::Auto).unwrap(),
            TextDecodeResult::Unsupported
        );
    }

    #[test]
    fn decoder_returns_only_canonical_bounded_text() {
        let root = TestDirectory::new("sanitize");
        let controls_path = root.path().join("controls.txt");
        fs::write(
            &controls_path,
            "one\r\ntwo\u{001b}[31m\u{202e}three".as_bytes(),
        )
        .unwrap();
        let controls = PreviewFile::open(&controls_path).unwrap();
        let TextDecodeResult::Preview(preview) = decode(&controls, &LegacyEncoding::Auto).unwrap()
        else {
            panic!("the sanitized UTF-8 file should produce a preview");
        };
        assert_eq!(preview.text, "one\ntwo\u{fffd}[31m\u{fffd}three");
        assert!(!preview.truncated);

        let long_path = root.path().join("long.txt");
        fs::write(&long_path, "x".repeat(MAX_TEXT_SCALARS + 1)).unwrap();
        let long = PreviewFile::open(&long_path).unwrap();
        let TextDecodeResult::Preview(preview) = decode(&long, &LegacyEncoding::Auto).unwrap()
        else {
            panic!("the bounded UTF-8 file should produce a preview");
        };
        assert_eq!(preview.text.len(), MAX_TEXT_SCALARS);
        assert!(preview.truncated);
    }

    #[test]
    fn legacy_policy_supports_auto_override_and_off_modes() {
        let auto = decode_legacy_bytes(b"I\x92", false, &LegacyEncoding::Auto).unwrap();
        assert_eq!(auto.text, "I’");
        assert_eq!(auto.encoding, "windows-1252");
        assert!(auto.encoding_was_guessed);

        let source = "Þetta er kóðunarpróf. Straße, café, naïve, résumé, voilà.";
        let (bytes, _, had_errors) = WINDOWS_1252.encode(source);
        assert!(!had_errors);

        let explicit = decode_legacy_bytes(
            bytes.as_ref(),
            false,
            &LegacyEncoding::Label("windows-1252".to_owned()),
        )
        .unwrap();
        assert_eq!(explicit.text, source);
        assert_eq!(explicit.encoding, "windows-1252");
        assert!(!explicit.encoding_was_guessed);

        assert!(decode_legacy_bytes(bytes.as_ref(), false, &LegacyEncoding::Off).is_none());
    }

    #[test]
    fn strict_legacy_decode_accepts_only_an_incomplete_bounded_tail() {
        let (text, truncated) =
            decode_legacy_with_encoding(b"\x82\xa0\x82", SHIFT_JIS, true).unwrap();
        assert_eq!(text, "あ");
        assert!(truncated);
        assert!(decode_legacy_with_encoding(b"\x82\xa0\x82", SHIFT_JIS, false).is_none());
        assert!(
            decode_legacy_with_encoding(b"\x82\x20", SHIFT_JIS, false).is_none(),
            "malformed bytes inside a complete file must fail closed"
        );
    }

    #[test]
    fn system_code_page_mapping_excludes_unicode_and_unknown_values() {
        assert_eq!(legacy_encoding_for_code_page(1252), Some(WINDOWS_1252));
        assert_eq!(legacy_encoding_for_code_page(65001), None);
        assert_eq!(legacy_encoding_for_code_page(1200), None);
        assert_eq!(legacy_encoding_for_code_page(u32::MAX), None);
    }

    #[test]
    fn unicode_boms_take_precedence_and_are_not_displayed() {
        for (bytes, kind, encoding) in [
            (
                &b"\xff\xfe\x00\x00A\x00\x00\x00"[..],
                TextByteKind::Utf32LeBom,
                "UTF-32 LE",
            ),
            (
                &b"\x00\x00\xfe\xff\x00\x00\x00A"[..],
                TextByteKind::Utf32BeBom,
                "UTF-32 BE",
            ),
            (&b"\xef\xbb\xbfA"[..], TextByteKind::Utf8Bom, "UTF-8"),
            (&b"\xff\xfeA\x00"[..], TextByteKind::Utf16LeBom, "UTF-16 LE"),
            (&b"\xfe\xff\x00A"[..], TextByteKind::Utf16BeBom, "UTF-16 BE"),
        ] {
            let decoded = decode_unicode_bytes(bytes, kind, false).unwrap();
            assert_eq!(decoded.text, "A");
            assert_eq!(decoded.encoding, encoding);
            assert!(!decoded.incomplete_tail);
        }
    }

    #[test]
    fn unicode_decoding_is_strict_except_for_an_incomplete_bounded_tail() {
        let decoded = decode_unicode_bytes(b"prefix \xe2\x82", TextByteKind::Utf8, true).unwrap();
        assert_eq!(decoded.text, "prefix ");
        assert!(decoded.incomplete_tail);
        assert!(
            decode_unicode_bytes(b"\xff", TextByteKind::Utf8, false).is_none(),
            "malformed complete UTF-8 must fail closed"
        );

        let decoded =
            decode_unicode_bytes(b"A\x00=\xd8", TextByteKind::Utf16LeLikely, true).unwrap();
        assert_eq!(decoded.text, "A");
        assert!(decoded.incomplete_tail);
        assert!(decode_unicode_bytes(b"A\x00=\xd8", TextByteKind::Utf16LeLikely, false).is_none());

        let decoded =
            decode_unicode_bytes(b"\x00\x00\x00A\x00\x00", TextByteKind::Utf32BeLikely, true)
                .unwrap();
        assert_eq!(decoded.text, "A");
        assert!(decoded.incomplete_tail);
        assert!(
            decode_unicode_bytes(b"\x00\x00\x00A\x00\x00", TextByteKind::Utf32BeLikely, false)
                .is_none()
        );
        assert!(
            decode_unicode_bytes(b"\x00\x00\x11\x00", TextByteKind::Utf32LeLikely, false).is_none(),
            "out-of-range Unicode scalars must fail closed"
        );
    }

    #[test]
    fn sanitizer_normalizes_every_hard_line_break() {
        let (text, truncated) =
            sanitize_and_truncate("one\r\ntwo\rthree\nfour\u{0085}five\u{2028}six\u{2029}seven");
        assert_eq!(text, "one\ntwo\nthree\nfour\nfive\nsix\nseven");
        assert!(!truncated);
    }

    #[test]
    fn sanitizer_neutralizes_controls_and_bidi_formatting_but_preserves_tabs() {
        let input =
            "ok\t\n\0\u{001b}\u{007f}\u{009f}\u{061c}\u{200e}\u{202e}\u{2066}\u{206f}\u{feff}done";
        let (text, truncated) = sanitize_and_truncate(input);
        assert_eq!(text, format!("ok\t\n{}done", "\u{fffd}".repeat(10)));
        assert!(!truncated);
    }

    #[test]
    fn sanitizer_enforces_exact_scalar_and_line_boundaries() {
        let exact_scalars = "世".repeat(MAX_TEXT_SCALARS);
        assert_eq!(
            sanitize_and_truncate(&exact_scalars),
            (exact_scalars.clone(), false)
        );
        let (text, truncated) = sanitize_and_truncate(&(exact_scalars.clone() + "界"));
        assert_eq!(text, exact_scalars);
        assert!(truncated);

        let exact_lines = (1..=MAX_TEXT_LINES)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            sanitize_and_truncate(&exact_lines),
            (exact_lines.clone(), false)
        );
        let (text, truncated) = sanitize_and_truncate(&(exact_lines.clone() + "\nover"));
        assert_eq!(text, exact_lines);
        assert!(truncated);
    }

    #[test]
    fn sanitizer_enforces_utf8_bytes_without_splitting_a_scalar() {
        let (text, truncated) = sanitize_and_truncate_with_limits("ab世cd", 4, usize::MAX, 1);
        assert_eq!(text, "ab");
        assert!(truncated);

        let (text, truncated) = sanitize_and_truncate_with_limits("世界", 6, usize::MAX, 1);
        assert_eq!(text, "世界");
        assert!(!truncated);
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            for _ in 0..32 {
                let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = env::temp_dir().join(format!(
                    "cursorpeek-text-{label}-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("test directory `{}` failed: {error}", path.display()),
                }
            }
            panic!("could not reserve a unique text test directory");
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap_or_else(|error| {
                panic!("test cleanup `{}` failed: {error}", self.0.display())
            });
        }
    }
}
