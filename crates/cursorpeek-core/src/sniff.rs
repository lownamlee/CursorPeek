use std::path::Path;

use image::ImageFormat as DecoderFormat;

use crate::payload::ImageFormat;

const NULL_PATTERN_SAMPLE_LIMIT: usize = 4 * 1024;

pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "webp", "bmp", "dib", "ico", "tif", "tiff",
];

pub const TEXT_EXTENSIONS: &[&str] = &[
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

pub const TEXT_NAMES: &[&str] = &[
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
pub enum TextByteKind {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SniffedImageFormat {
    pub decoder: DecoderFormat,
    pub preview: ImageFormat,
}

pub fn is_text_eligible_path(path: &Path) -> bool {
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

pub fn is_image_eligible_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

pub fn sniff_image_format(prefix: &[u8]) -> Option<SniffedImageFormat> {
    let decoder = image::guess_format(prefix).ok()?;
    let preview = match decoder {
        DecoderFormat::Jpeg => ImageFormat::Jpeg,
        DecoderFormat::Png => ImageFormat::Png,
        DecoderFormat::Gif => ImageFormat::Gif,
        DecoderFormat::WebP => ImageFormat::WebP,
        DecoderFormat::Bmp => ImageFormat::Bmp,
        DecoderFormat::Ico => ImageFormat::Ico,
        DecoderFormat::Tiff => ImageFormat::Tiff,
        _ => return None,
    };
    Some(SniffedImageFormat { decoder, preview })
}

pub fn classify_text_prefix(bytes: &[u8], prefix_truncated: bool) -> Option<TextByteKind> {
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
    use std::path::Path;

    use image::ImageFormat as DecoderFormat;

    use super::{
        IMAGE_EXTENSIONS, TEXT_EXTENSIONS, TEXT_NAMES, TextByteKind, classify_text_prefix,
        is_image_eligible_path, is_text_eligible_path, sniff_image_format,
    };
    use crate::payload::ImageFormat;

    #[test]
    fn eligible_paths_are_ascii_case_insensitive() {
        for extension in TEXT_EXTENSIONS {
            assert!(is_text_eligible_path(Path::new(&format!(
                "sample.{}",
                extension.to_ascii_uppercase()
            ))));
        }
        for name in TEXT_NAMES {
            assert!(is_text_eligible_path(Path::new(&name.to_ascii_lowercase())));
        }
        for extension in IMAGE_EXTENSIONS {
            assert!(is_image_eligible_path(Path::new(&format!(
                "sample.{}",
                extension.to_ascii_uppercase()
            ))));
        }
    }

    #[test]
    fn binary_signatures_and_null_noise_fail_closed() {
        assert_eq!(classify_text_prefix(b"\x89PNG\r\n\x1a\n", false), None);
        assert_eq!(
            classify_text_prefix(b"a\0b\0c\0d\0", false),
            Some(TextByteKind::Utf16LeLikely)
        );
        assert_eq!(classify_text_prefix(b"\0\0\0\0\0\0\0\0", false), None);
    }

    #[test]
    fn supported_image_magic_maps_to_wire_format() {
        let png = sniff_image_format(b"\x89PNG\r\n\x1a\nrest").unwrap();
        assert_eq!(png.decoder, DecoderFormat::Png);
        assert_eq!(png.preview, ImageFormat::Png);
        assert_eq!(sniff_image_format(b"plain text"), None);
    }
}
