use std::path::Path;

use super::file::{PreviewFile, PreviewFileError};

const TEXT_SNIFF_LIMIT: usize = 64 * 1024;
const NULL_PATTERN_SAMPLE_LIMIT: usize = 4 * 1024;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TextClassification {
    Text,
    Unsupported,
}

pub(super) fn classify(file: &PreviewFile) -> Result<TextClassification, PreviewFileError> {
    if !is_eligible_path(file.final_path()) {
        return Ok(TextClassification::Unsupported);
    }

    let bytes = file.read_prefix(TEXT_SNIFF_LIMIT)?;
    let prefix_truncated = file.file_size()
        > u64::try_from(bytes.len()).expect("the bounded text prefix length fits u64");
    Ok(if classify_prefix(&bytes, prefix_truncated).is_some() {
        TextClassification::Text
    } else {
        TextClassification::Unsupported
    })
}

fn is_eligible_path(path: &Path) -> bool {
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
mod tests {
    use super::{
        TEXT_EXTENSIONS, TEXT_NAMES, TextByteKind, TextClassification, classify, classify_bytes,
        classify_prefix, is_eligible_path,
    };
    use crate::worker::file::PreviewFile;
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
    fn classifier_reads_only_eligible_files_and_rejects_disguised_binary() {
        let root = TestDirectory::new("classify");
        let text_path = root.path().join("large.txt");
        fs::write(&text_path, vec![b'a'; 80 * 1024]).unwrap();
        let text = PreviewFile::open(&text_path).unwrap();
        assert_eq!(classify(&text).unwrap(), TextClassification::Text);

        let binary_path = root.path().join("disguised.txt");
        fs::write(&binary_path, b"\x89PNG\r\n\x1a\npayload").unwrap();
        let binary = PreviewFile::open(&binary_path).unwrap();
        assert_eq!(classify(&binary).unwrap(), TextClassification::Unsupported);

        let image_path = root.path().join("image.png");
        fs::write(&image_path, b"not read by the text classifier").unwrap();
        let image = PreviewFile::open(&image_path).unwrap();
        assert_eq!(classify(&image).unwrap(), TextClassification::Unsupported);
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
