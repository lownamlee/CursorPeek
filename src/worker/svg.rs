use std::{
    io::{BufReader, Read},
    os::windows::ffi::OsStrExt,
};

pub(super) use cursorpeek_core::sniff::is_vector_eligible_path as is_eligible_path;
use resvg::{
    tiny_skia::{Pixmap, Transform},
    usvg::{ImageHrefResolver, Options, Tree},
};

use crate::preview_file::{PreviewFile, PreviewFileError};

use super::payload::{
    ImageAnimationPreview, ImageAnimationSource, ImageFormat, ImagePreview, MAX_SOURCE_IMAGE_AXIS,
    MAX_SOURCE_IMAGE_PIXELS, fitted_preview_dimensions,
};

const MAX_SVG_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

pub(super) enum SvgDecodeResult {
    Preview(ImagePreview),
    Unsupported,
}

pub(super) enum SvgAnimationDecodeResult {
    Preview(ImageAnimationPreview),
    Unsupported,
}

pub(super) fn decode(file: &PreviewFile) -> Result<SvgDecodeResult, PreviewFileError> {
    if !is_eligible_path(file.final_path()) || file.file_size() > MAX_SVG_SOURCE_BYTES {
        return Ok(SvgDecodeResult::Unsupported);
    }

    let bytes = read_source(file)?;
    if !file.is_unchanged()? {
        return Err(PreviewFileError::ChangedDuringRead);
    }

    let animated = utf8_source(&bytes).is_some_and(may_have_animation_markup);

    let options = Options {
        resources_dir: None,
        image_href_resolver: ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Options::default()
    };
    let Ok(tree) = Tree::from_data(&bytes, &options) else {
        return Ok(SvgDecodeResult::Unsupported);
    };

    let size = tree.size();
    let source_width = bounded_axis(size.width());
    let source_height = bounded_axis(size.height());
    let (Some(source_width), Some(source_height)) = (source_width, source_height) else {
        return Ok(SvgDecodeResult::Unsupported);
    };
    if u64::from(source_width) * u64::from(source_height) > MAX_SOURCE_IMAGE_PIXELS {
        return Ok(SvgDecodeResult::Unsupported);
    }
    let Some((width, height)) = fitted_preview_dimensions(source_width, source_height) else {
        return Ok(SvgDecodeResult::Unsupported);
    };
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        return Ok(SvgDecodeResult::Unsupported);
    };
    let transform =
        Transform::from_scale(width as f32 / size.width(), height as f32 / size.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let mut premultiplied_bgra = pixmap.take();
    for pixel in premultiplied_bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    if premultiplied_bgra.iter().all(|byte| *byte == 0) {
        return Ok(SvgDecodeResult::Unsupported);
    }
    if !file.is_unchanged()? {
        return Err(PreviewFileError::ChangedDuringRead);
    }

    Ok(SvgDecodeResult::Preview(ImagePreview {
        file_size: file.file_size(),
        last_write_time: file.last_write_time(),
        linked_content: file.is_linked_content(),
        first_frame_only: animated,
        display_name: file.display_name(),
        format: ImageFormat::Svg,
        source_width,
        source_height,
        width,
        height,
        animation_source: animated.then(|| ImageAnimationSource {
            file_size: file.file_size(),
            last_write_time: file.last_write_time(),
            volume_serial_number: file.volume_serial_number(),
            file_id: file.file_id(),
            format: ImageFormat::Svg,
            source_width,
            source_height,
            path: file.final_path().as_os_str().encode_wide().collect(),
        }),
        premultiplied_bgra,
    }))
}

pub(super) fn decode_animation(
    file: &PreviewFile,
) -> Result<SvgAnimationDecodeResult, PreviewFileError> {
    if !is_eligible_path(file.final_path()) || file.file_size() > MAX_SVG_SOURCE_BYTES {
        return Ok(SvgAnimationDecodeResult::Unsupported);
    }
    let bytes = read_source(file)?;
    let Some(source) = utf8_source(&bytes) else {
        return Ok(SvgAnimationDecodeResult::Unsupported);
    };
    let Ok(rendered) = cursorpeek_core::svg::render(source) else {
        return Ok(SvgAnimationDecodeResult::Unsupported);
    };
    if !rendered.animated || rendered.frames.len() < 2 {
        return Ok(SvgAnimationDecodeResult::Unsupported);
    }
    if !file.is_unchanged()? {
        return Err(PreviewFileError::ChangedDuringRead);
    }

    let frame_delays_ms = vec![rendered.frame_delay_ms; rendered.frames.len()];
    Ok(SvgAnimationDecodeResult::Preview(ImageAnimationPreview {
        file_size: file.file_size(),
        last_write_time: file.last_write_time(),
        format: ImageFormat::Svg,
        source_width: rendered.source_width,
        source_height: rendered.source_height,
        width: rendered.width,
        height: rendered.height,
        truncated: rendered.frames.len()
            == usize::try_from(cursorpeek_core::svg::MAX_VECTOR_FRAMES)
                .expect("the SVG frame cap fits usize"),
        frame_delays_ms,
        frames: rendered.frames,
    }))
}

fn read_source(file: &PreviewFile) -> Result<Vec<u8>, PreviewFileError> {
    let limit = MAX_SVG_SOURCE_BYTES
        .checked_add(1)
        .expect("the SVG source cap leaves room for one overflow byte");
    let mut bytes = Vec::new();
    BufReader::new(file.duplicate_reader()?)
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| PreviewFileError::Io {
            operation: "read the bounded SVG document",
            source,
        })?;
    if u64::try_from(bytes.len()).ok() != Some(file.file_size()) {
        return Err(PreviewFileError::ChangedDuringRead);
    }
    Ok(bytes)
}

fn utf8_source(bytes: &[u8]) -> Option<&str> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    std::str::from_utf8(bytes).ok()
}

fn may_have_animation_markup(source: &str) -> bool {
    const MARKERS: &[&[u8]] = &[b"<animate", b":animate", b"<set", b":set"];
    MARKERS.iter().any(|marker| {
        source
            .as_bytes()
            .windows(marker.len())
            .any(|window| window.eq_ignore_ascii_case(marker))
    })
}

fn bounded_axis(value: f32) -> Option<u32> {
    if !value.is_finite() || value <= 0.0 || value > MAX_SOURCE_IMAGE_AXIS as f32 {
        return None;
    }
    Some((value.ceil() as u32).max(1))
}

#[cfg(test)]
mod tests {
    use super::{
        SvgAnimationDecodeResult, SvgDecodeResult, decode, decode_animation,
        may_have_animation_markup,
    };
    use crate::preview_file::PreviewFile;
    use std::path::Path;

    #[test]
    fn retained_static_svg_renders_as_a_bounded_visual_preview() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("manual-tests/svg/static-shapes.svg");
        let file = PreviewFile::open(&path).expect("the retained SVG fixture should open");
        let SvgDecodeResult::Preview(preview) =
            decode(&file).expect("the retained SVG fixture should decode")
        else {
            panic!("the retained SVG fixture should render visually");
        };

        assert_eq!(preview.display_name, "static-shapes.svg");
        assert_eq!(preview.format, super::ImageFormat::Svg);
        assert!(!preview.premultiplied_bgra.is_empty());
        assert_eq!(
            preview.premultiplied_bgra.len(),
            usize::try_from(preview.width * preview.height * 4).unwrap()
        );
        assert!(
            preview
                .premultiplied_bgra
                .chunks_exact(4)
                .all(|pixel| pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3])
        );
    }

    #[test]
    fn external_references_are_ignored_while_local_shapes_still_render() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("manual-tests/svg/external-reference.svg");
        let file = PreviewFile::open(&path).expect("the retained SVG fixture should open");
        let SvgDecodeResult::Preview(preview) =
            decode(&file).expect("blocked resources are not file errors")
        else {
            panic!("the local rectangle should render after the external image is ignored");
        };
        assert!(!preview.premultiplied_bgra.is_empty());
    }

    #[test]
    fn retained_animated_svg_produces_a_progressive_frame_upgrade() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("manual-tests/svg/animated-shapes.svg");
        let file = PreviewFile::open(&path).expect("the retained SVG fixture should open");
        let SvgDecodeResult::Preview(still) = decode(&file).expect("the SVG still should decode")
        else {
            panic!("the animated SVG should produce an immediate still");
        };
        assert!(still.first_frame_only);
        assert!(still.animation_source.is_some());

        let SvgAnimationDecodeResult::Preview(animation) =
            decode_animation(&file).expect("the SVG animation should decode")
        else {
            panic!("the animated SVG should produce a frame upgrade");
        };
        assert_eq!(animation.format, super::ImageFormat::Svg);
        assert!(animation.frames.len() >= 2);
        assert_ne!(animation.frames[0], animation.frames[1]);
        assert!(animation.frame_delays_ms.iter().all(|delay| *delay >= 40));
    }

    #[test]
    fn animation_marker_scan_is_case_insensitive_and_prefix_aware() {
        for source in [
            "<svg><animate attributeName='x'/></svg>",
            "<svg><ANIMATETRANSFORM attributeName='transform'/></svg>",
            "<svg><svg:animate attributeName='x'/></svg>",
            "<svg><set attributeName='fill'/></svg>",
            "<svg><svg:set attributeName='fill'/></svg>",
        ] {
            assert!(may_have_animation_markup(source), "{source}");
        }
        assert!(!may_have_animation_markup(
            "<svg><rect aria-label='animate set'/></svg>"
        ));
    }
}
