use std::path::Path;

use image::ImageFormat as DecoderFormat;

use crate::payload::ImageFormat;

const NULL_PATTERN_SAMPLE_LIMIT: usize = 4 * 1024;

pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "webp", "bmp", "dib", "ico", "tif", "tiff",
];

// Every entry is previewed as inert plain text. Nothing here is parsed, rendered, executed, or
// resolved: markup, project, and patch formats reach the preview as source bytes like any log file.
// The list is grouped to match the user-guide and README tables; keep all three in the same order.
pub const TEXT_EXTENSIONS: &[&str] = &[
    // Text, logs, and markup
    "txt",
    "text",
    "log",
    "md",
    "markdown",
    "mdx",
    "rst",
    "adoc",
    "tex",
    // SVG is XML markup. The text provider previews it as inert source rather than rasterizing it,
    // so no vector renderer, font lookup, or external reference handling enters the worker.
    "svg",
    // Data and configuration
    "csv",
    "tsv",
    "json",
    "jsonc",
    "json5",
    "jsonl",
    "ndjson",
    "xml",
    // Only textual property lists qualify. A binary `bplist00` payload fails the content check.
    "plist",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "config",
    "properties",
    "hcl",
    "tf",
    "tfvars",
    "proto",
    "graphql",
    // Source code
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
    "vb",
    "fs",
    "java",
    "kt",
    "kts",
    "scala",
    "groovy",
    "go",
    "swift",
    "dart",
    "py",
    "pyw",
    "rb",
    "php",
    "lua",
    "r",
    // Web, JavaScript, and TypeScript
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "mts",
    "cts",
    "tsx",
    "vue",
    "svelte",
    "astro",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    // Scripts and queries
    "sql",
    "sh",
    "bash",
    "zsh",
    "ps1",
    "psm1",
    "psd1",
    "bat",
    "cmd",
    // Projects and build files
    "sln",
    "csproj",
    "vbproj",
    "vcxproj",
    "props",
    "targets",
    "resx",
    "nuspec",
    "manifest",
    "cmake",
    "mk",
    "gradle",
    // PEM-armored key and certificate material. These preview their contents like any other text
    // file, so the user guide's screen-sharing warning covers them. DER-encoded `.cer`/`.crt` and
    // binary containers such as `.pfx` are rejected by the content check instead.
    "pem",
    "crt",
    "cer",
    "csr",
    "key",
    "pub",
    "ppk",
    "asc",
    // Patches, registry exports, and other plain-text data
    "diff",
    "patch",
    // Registry exports are usually UTF-16 LE with a BOM, which the text decoder already handles.
    "reg",
    "po",
    "srt",
    "vtt",
    "ics",
];

pub const TEXT_NAMES: &[&str] = &[
    "README",
    "LICENSE",
    "COPYING",
    "NOTICE",
    "AUTHORS",
    "CONTRIBUTING",
    "CHANGELOG",
    "CODEOWNERS",
    "VERSION",
    "Makefile",
    "Dockerfile",
    "Gemfile",
    "Rakefile",
    "Procfile",
    "Justfile",
    "Jenkinsfile",
    ".env",
    ".editorconfig",
    ".gitattributes",
    ".gitignore",
    ".gitmodules",
    ".dockerignore",
    ".npmrc",
    ".nvmrc",
    ".prettierrc",
    ".prettierignore",
    ".eslintrc",
    ".eslintignore",
    // Extensionless OpenSSH key material, previewed under the same policy as `.pem` and `.key`.
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "known_hosts",
    "authorized_keys",
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
    fn closely_related_extensions_are_each_listed_explicitly() {
        for path in [
            "app.conf",
            "app.config",
            "module.ps1",
            "module.psm1",
            "module.psd1",
            "Info.plist",
            "bundle.tf",
            "bundle.tfvars",
        ] {
            assert!(is_text_eligible_path(Path::new(path)), "{path}");
        }
    }

    // Eligibility here is a deliberate product decision: previews are local, offline, and shown
    // only to the person at the keyboard, so key material is treated like `.env`.
    #[test]
    fn pem_armored_key_material_is_eligible_by_policy() {
        for path in [
            "server.pem",
            "server.crt",
            "server.cer",
            "server.csr",
            "server.key",
            "id_ed25519.pub",
            "putty.ppk",
            "release.asc",
            "id_rsa",
            "id_ed25519",
            "known_hosts",
            "authorized_keys",
        ] {
            assert!(is_text_eligible_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn deliberate_text_exclusions_stay_ineligible() {
        for path in [
            // Compressed markup rather than markup.
            "logo.svgz",
            // Exact names match the whole file name; suffixed variants are out of scope.
            "Dockerfile.prod",
            "Makefile.am",
            ".env.local",
            // Only the final extension is considered.
            "archive.tar.gz",
            // Binary key containers, unlike their PEM-armored counterparts.
            "store.pfx",
            "store.p12",
            "store.jks",
            // Formats that need a parser the worker does not have.
            "report.pdf",
            "book.docx",
            "notes.rtf",
        ] {
            assert!(!is_text_eligible_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn svg_markup_is_text_eligible_and_never_image_eligible() {
        for path in ["logo.svg", "logo.SVG"] {
            assert!(is_text_eligible_path(Path::new(path)));
            assert!(!is_image_eligible_path(Path::new(path)));
        }
        // Compressed SVG is gzip, not markup, and stays outside both providers.
        assert!(!is_text_eligible_path(Path::new("logo.svgz")));
        assert!(!is_image_eligible_path(Path::new("logo.svgz")));
    }

    #[test]
    fn binary_signatures_and_null_noise_fail_closed() {
        assert_eq!(classify_text_prefix(b"\x89PNG\r\n\x1a\n", false), None);
        assert_eq!(
            classify_text_prefix(b"a\0b\0c\0d\0", false),
            Some(TextByteKind::Utf16LeLikely)
        );
        assert_eq!(classify_text_prefix(b"\0\0\0\0\0\0\0\0", false), None);
        // Binary variants of newly eligible extensions still fail the content check: a `bplist00`
        // property list and a DER-encoded certificate are rejected despite `.plist`/`.cer` names.
        assert_eq!(
            classify_text_prefix(b"bplist00\x00\x08\x00\x00\x00\x00\x00\x01", false),
            None
        );
        assert_eq!(
            classify_text_prefix(b"\x30\x82\x01\x0a\x02\x82\x01\x01\x00", false),
            None
        );
    }

    #[test]
    fn supported_image_magic_maps_to_wire_format() {
        let png = sniff_image_format(b"\x89PNG\r\n\x1a\nrest").unwrap();
        assert_eq!(png.decoder, DecoderFormat::Png);
        assert_eq!(png.preview, ImageFormat::Png);
        assert_eq!(sniff_image_format(b"plain text"), None);
    }
}
