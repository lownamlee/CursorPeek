use std::io::{BufReader, Read};

#[cfg(test)]
pub(super) mod corpus;

pub(super) use cursorpeek_core::sniff::is_vector_eligible_path as is_eligible_path;
use cursorpeek_core::svg::{MAX_VECTOR_SOURCE_BYTES, SvgError};

use super::{
    file::{PreviewFile, PreviewFileError},
    payload::VectorPreview,
};

#[derive(Debug)]
pub(super) enum VectorDecodeResult {
    Preview(VectorPreview),
    /// The document was refused; the caller falls back to the inert text preview.
    Fallback(&'static str),
}

/// Rasterizes an eligible SVG document inside the contained worker.
pub(super) fn decode(file: &PreviewFile) -> Result<VectorDecodeResult, PreviewFileError> {
    if !is_eligible_path(file.final_path()) {
        return Ok(VectorDecodeResult::Fallback("ineligible"));
    }
    if file.file_size() > MAX_VECTOR_SOURCE_BYTES {
        return Ok(VectorDecodeResult::Fallback("file_too_large"));
    }

    let bytes = read_source(file)?;
    if !file.is_unchanged()? {
        return Err(PreviewFileError::ChangedDuringRead);
    }
    // Only UTF-8 documents reach the renderer. Other encodings keep the text provider's decoder.
    let Some(source) = utf8_source(&bytes) else {
        return Ok(VectorDecodeResult::Fallback("not_utf8"));
    };

    match cursorpeek_core::svg::render(source) {
        Ok(rendered) => Ok(VectorDecodeResult::Preview(VectorPreview {
            file_size: file.file_size(),
            last_write_time: file.last_write_time(),
            linked_content: file.is_linked_content(),
            animated: rendered.animated && rendered.frames.len() > 1,
            display_name: file.display_name(),
            source_width: rendered.source_width,
            source_height: rendered.source_height,
            width: rendered.width,
            height: rendered.height,
            frame_delay_ms: rendered.frame_delay_ms,
            frames: rendered.frames,
        })),
        Err(error) => Ok(VectorDecodeResult::Fallback(refusal(error))),
    }
}

fn read_source(file: &PreviewFile) -> Result<Vec<u8>, PreviewFileError> {
    let limit = MAX_VECTOR_SOURCE_BYTES
        .checked_add(1)
        .expect("the source cap leaves room for the overflow probe");
    let mut bytes = Vec::new();
    BufReader::new(file.duplicate_reader()?)
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| PreviewFileError::Io {
            operation: "read the bounded vector document",
            source,
        })?;
    Ok(bytes)
}

fn utf8_source(bytes: &[u8]) -> Option<&str> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    std::str::from_utf8(bytes).ok()
}

const fn refusal(error: SvgError) -> &'static str {
    match error {
        SvgError::MalformedMarkup => "malformed_markup",
        SvgError::UnclosedElement => "unclosed_element",
        SvgError::MismatchedElement => "mismatched_element",
        SvgError::UnknownEntity => "unknown_entity",
        SvgError::EntityDeclaration => "entity_declaration",
        SvgError::ActiveContent => "active_content",
        SvgError::ExternalReference => "external_reference",
        SvgError::MalformedPath => "malformed_path",
        SvgError::NotSvg => "not_svg",
        SvgError::TooComplex => "too_complex",
        SvgError::TooLarge => "too_large",
        SvgError::InvalidSize => "invalid_size",
        SvgError::NothingPainted => "nothing_painted",
    }
}

#[cfg(test)]
mod tests {
    use super::{VectorDecodeResult, decode, is_eligible_path, refusal, utf8_source};
    use crate::worker::file::PreviewFile;
    use cursorpeek_core::svg::{MAX_VECTOR_SOURCE_BYTES, SvgError};
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "cursorpeek-vector-{label}-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("the vector test directory should be created");
            Self { path }
        }

        fn write(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.path.join(name);
            fs::write(&path, contents).expect("the vector fixture should be written");
            path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).expect("the vector test directory should be removed");
        }
    }

    fn decoded(path: &Path) -> VectorDecodeResult {
        let file = PreviewFile::open(path).expect("the vector fixture should open");
        decode(&file).expect("decoding should not fail the file layer")
    }

    #[test]
    fn eligibility_is_limited_to_svg_documents() {
        assert!(is_eligible_path(Path::new("logo.SVG")));
        for path in ["logo.svgz", "logo.png", "logo.xml", "logo"] {
            assert!(!is_eligible_path(Path::new(path)));
        }
    }

    #[test]
    fn a_static_document_produces_one_bounded_frame() {
        let root = TestDirectory::new("static");
        let path = root.write(
            "logo.svg",
            b"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24'>\
              <circle cx='12' cy='12' r='10' fill='#2f81f7'/></svg>",
        );

        let VectorDecodeResult::Preview(preview) = decoded(&path) else {
            panic!("a well-formed SVG should render");
        };
        assert_eq!((preview.source_width, preview.source_height), (24, 24));
        assert_eq!(preview.frames.len(), 1);
        assert!(!preview.animated);
        assert_eq!(preview.frame_delay_ms, 0);
        assert_eq!(preview.display_name, "logo.svg");
        assert_eq!(preview.file_size, fs::metadata(&path).unwrap().len());
        assert_eq!(
            preview.frames[0].len(),
            (preview.width * preview.height * 4) as usize
        );
    }

    #[test]
    fn an_animated_document_produces_looping_frames() {
        let root = TestDirectory::new("animated");
        let path = root.write(
            "spinner.svg",
            b"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 40 10'>\
              <rect width='10' height='10' fill='black'>\
              <animate attributeName='x' from='0' to='30' dur='800ms' \
              repeatCount='indefinite'/></rect></svg>",
        );

        let VectorDecodeResult::Preview(preview) = decoded(&path) else {
            panic!("an animated SVG should render");
        };
        assert!(preview.animated);
        assert!(preview.frames.len() >= 2);
        assert!(preview.frame_delay_ms >= 40);
        assert_ne!(preview.frames[0], preview.frames[preview.frames.len() - 1]);
    }

    #[test]
    fn refused_documents_report_a_reason_for_the_text_fallback() {
        let root = TestDirectory::new("fallback");
        let cases: [(&str, &[u8], &str); 5] = [
            (
                "script.svg",
                b"<svg><script>fetch('https://example.test')</script></svg>",
                "active_content",
            ),
            (
                "external.svg",
                b"<svg><rect fill='url(https://example.test#g)'/></svg>",
                "external_reference",
            ),
            (
                "entity.svg",
                b"<!DOCTYPE svg [<!ENTITY x SYSTEM 'file:///c:/secret'>]><svg/>",
                "entity_declaration",
            ),
            ("truncated.svg", b"<svg><rect width='1'", "malformed_markup"),
            (
                "text-only.svg",
                b"<svg viewBox='0 0 10 10'><text y='5'>hi</text></svg>",
                "nothing_painted",
            ),
        ];

        for (name, contents, expected) in cases {
            let path = root.write(name, contents);
            let VectorDecodeResult::Fallback(reason) = decoded(&path) else {
                panic!("{name} should be refused");
            };
            assert_eq!(reason, expected, "{name}");
        }
    }

    #[test]
    fn oversized_and_non_utf8_documents_fall_back_without_rendering() {
        let root = TestDirectory::new("bounds");
        let oversized = root.path.join("huge.svg");
        let sparse = fs::File::create(&oversized).unwrap();
        sparse.set_len(MAX_VECTOR_SOURCE_BYTES + 1).unwrap();
        drop(sparse);
        assert!(matches!(
            decoded(&oversized),
            VectorDecodeResult::Fallback("file_too_large")
        ));

        let utf16 = root.write("utf16.svg", b"\xff\xfe<\0s\0v\0g\0/\0>\0");
        assert!(matches!(
            decoded(&utf16),
            VectorDecodeResult::Fallback("not_utf8")
        ));

        assert_eq!(utf8_source(b"\xef\xbb\xbf<svg/>"), Some("<svg/>"));
        assert_eq!(utf8_source(b"\xff\xfe"), None);
    }

    #[test]
    fn every_refusal_maps_to_a_distinct_diagnostic_label() {
        let errors = [
            SvgError::MalformedMarkup,
            SvgError::UnclosedElement,
            SvgError::MismatchedElement,
            SvgError::UnknownEntity,
            SvgError::EntityDeclaration,
            SvgError::ActiveContent,
            SvgError::ExternalReference,
            SvgError::MalformedPath,
            SvgError::NotSvg,
            SvgError::TooComplex,
            SvgError::TooLarge,
            SvgError::InvalidSize,
            SvgError::NothingPainted,
        ];
        let mut labels: Vec<&str> = errors.into_iter().map(refusal).collect();
        labels.sort_unstable();
        let total = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), total);
    }
}
