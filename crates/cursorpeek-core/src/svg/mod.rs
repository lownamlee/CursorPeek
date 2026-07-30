//! Contained SVG renderer for vector previews.
//!
//! The renderer is a pure function from document bytes to premultiplied BGRA frames. It has no
//! script engine, no network or filesystem access, and no clock input, and every stage is bounded
//! by an explicit limit so a malformed or hostile document fails closed instead of consuming the
//! worker. Callers keep the immediate static SVG visual or fail closed on any error.

mod animate;
mod doc;
mod geom;
mod raster;
mod render;
mod value;
mod xml;

use std::{error::Error, fmt};

use crate::layout::{
    MAX_SOURCE_IMAGE_AXIS, MAX_SOURCE_IMAGE_PIXELS, checked_animation_layout, checked_bgra_layout,
    fitted_animation_dimensions, fitted_preview_dimensions,
};
use geom::Transform;
use render::{Budget, Viewport, parse_view_box, render_frame, view_box_transform};
use value::parse_length;

/// Largest source document accepted for rendering.
pub const MAX_VECTOR_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_VECTOR_FRAMES: u32 = 12;
pub const MIN_VECTOR_FRAME_DELAY_MS: u32 = 40;

const DEFAULT_VIEWPORT_WIDTH: f32 = 300.0;
const DEFAULT_VIEWPORT_HEIGHT: f32 = 150.0;

/// A rendered vector preview: one frame for a static document, several for an animated one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VectorRender {
    pub source_width: u32,
    pub source_height: u32,
    pub width: u32,
    pub height: u32,
    pub frame_delay_ms: u32,
    pub animated: bool,
    pub frames: Vec<Vec<u8>>,
}

pub fn render(source: &str) -> Result<VectorRender, SvgError> {
    if source.len() as u64 > MAX_VECTOR_SOURCE_BYTES {
        return Err(SvgError::TooLarge);
    }

    let document = doc::parse_document(source)?;
    let root = document.element(document.root);
    let view_box = root.attribute("viewbox").and_then(parse_view_box);
    let user_width = intrinsic_length(
        root.attribute("width"),
        view_box.map(|value| value.2),
        DEFAULT_VIEWPORT_WIDTH,
    );
    let user_height = intrinsic_length(
        root.attribute("height"),
        view_box.map(|value| value.3),
        DEFAULT_VIEWPORT_HEIGHT,
    );
    let (source_width, source_height) = source_pixel_size(user_width, user_height)?;

    let timeline = animate::timeline_ms(&document);
    let animated = timeline.is_some();
    let (width, height) = if animated {
        fitted_animation_dimensions(source_width, source_height)
    } else {
        fitted_preview_dimensions(source_width, source_height)
    }
    .ok_or(SvgError::InvalidSize)?;
    let (frame_count, frame_delay_ms) = frame_plan(timeline);
    if frame_count > 1 {
        checked_animation_layout(width, height, frame_count).map_err(|_| SvgError::InvalidSize)?;
    } else {
        checked_bgra_layout(width, height).map_err(|_| SvgError::InvalidSize)?;
    }

    let root_transform = Transform::scale(width as f32 / user_width, height as f32 / user_height)
        .concat(match view_box {
            Some(view_box) => view_box_transform(
                view_box,
                user_width,
                user_height,
                root.attribute("preserveaspectratio"),
            ),
            None => Transform::IDENTITY,
        });
    let viewport = Viewport {
        width: user_width,
        height: user_height,
    };

    let budget = Budget::TOTAL.divided(frame_count);
    let mut frames: Vec<Vec<u8>> = Vec::with_capacity(frame_count as usize);
    for index in 0..frame_count {
        let state = animate::AnimationState::at(&document, index * frame_delay_ms);
        frames.push(render_frame(
            &document,
            &state,
            width,
            height,
            root_transform,
            viewport,
            budget,
        )?);
    }
    if frames
        .iter()
        .all(|frame| frame.iter().all(|byte| *byte == 0))
    {
        // Nothing this renderer supports was painted, so the inert source text is more useful.
        return Err(SvgError::NothingPainted);
    }

    Ok(VectorRender {
        source_width,
        source_height,
        width,
        height,
        frame_delay_ms: if frame_count > 1 { frame_delay_ms } else { 0 },
        animated,
        frames,
    })
}

pub fn is_animated(source: &str) -> Result<bool, SvgError> {
    if source.len() as u64 > MAX_VECTOR_SOURCE_BYTES {
        return Err(SvgError::TooLarge);
    }
    let document = doc::parse_document(source)?;
    Ok(animate::timeline_ms(&document).is_some())
}

/// Resolves `width`/`height`, falling back to the `viewBox` extent and then the SVG default.
fn intrinsic_length(attribute: Option<&str>, view_box: Option<f32>, default: f32) -> f32 {
    let declared = attribute.and_then(|declared| {
        if declared.trim().ends_with('%') {
            None
        } else {
            parse_length(declared, 0.0)
        }
    });
    declared
        .filter(|value| *value > 0.0)
        .or(view_box)
        .filter(|value| *value > 0.0)
        .unwrap_or(default)
}

fn source_pixel_size(user_width: f32, user_height: f32) -> Result<(u32, u32), SvgError> {
    let round = |value: f32| -> Result<u32, SvgError> {
        if !value.is_finite() || value <= 0.0 {
            return Err(SvgError::InvalidSize);
        }
        let rounded = value.ceil();
        if rounded > MAX_SOURCE_IMAGE_AXIS as f32 {
            return Err(SvgError::InvalidSize);
        }
        Ok((rounded as u32).max(1))
    };
    let width = round(user_width)?;
    let height = round(user_height)?;
    if u64::from(width) * u64::from(height) > MAX_SOURCE_IMAGE_PIXELS {
        return Err(SvgError::InvalidSize);
    }
    Ok((width, height))
}

/// Chooses how many frames to sample and how long each is shown.
fn frame_plan(timeline: Option<u32>) -> (u32, u32) {
    let Some(window) = timeline else {
        return (1, 0);
    };
    let count = (window / MIN_VECTOR_FRAME_DELAY_MS).clamp(2, MAX_VECTOR_FRAMES);
    let delay = (window / count).max(MIN_VECTOR_FRAME_DELAY_MS);
    (count, delay)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SvgError {
    MalformedMarkup,
    UnclosedElement,
    MismatchedElement,
    UnknownEntity,
    EntityDeclaration,
    ActiveContent,
    ExternalReference,
    MalformedPath,
    NotSvg,
    TooComplex,
    TooLarge,
    InvalidSize,
    NothingPainted,
}

impl fmt::Display for SvgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedMarkup => write!(formatter, "the document is not well-formed XML"),
            Self::UnclosedElement => write!(formatter, "the document ends inside an element"),
            Self::MismatchedElement => write!(formatter, "an end tag does not match its start tag"),
            Self::UnknownEntity => write!(formatter, "the document references an unknown entity"),
            Self::EntityDeclaration => {
                write!(
                    formatter,
                    "the document declares entities or an internal subset"
                )
            }
            Self::ActiveContent => {
                write!(
                    formatter,
                    "the document contains script or event-handler content"
                )
            }
            Self::ExternalReference => {
                write!(
                    formatter,
                    "the document references a resource outside itself"
                )
            }
            Self::MalformedPath => write!(formatter, "the document contains invalid path data"),
            Self::NotSvg => write!(formatter, "the document root is not an svg element"),
            Self::TooComplex => write!(formatter, "the document exceeds a rendering limit"),
            Self::TooLarge => write!(
                formatter,
                "the document exceeds {MAX_VECTOR_SOURCE_BYTES} bytes"
            ),
            Self::InvalidSize => write!(formatter, "the document has no usable intrinsic size"),
            Self::NothingPainted => {
                write!(
                    formatter,
                    "the document paints nothing this renderer supports"
                )
            }
        }
    }
}

impl Error for SvgError {}

#[cfg(test)]
mod tests {
    use super::{
        MAX_VECTOR_FRAMES, MIN_VECTOR_FRAME_DELAY_MS, SvgError, frame_plan, intrinsic_length,
        render, source_pixel_size,
    };
    use crate::layout::MAX_SOURCE_IMAGE_AXIS;

    #[test]
    fn a_static_document_renders_one_original_size_frame() {
        let rendered = render(
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'>\
             <rect width='16' height='16' fill='#3178c6'/></svg>",
        )
        .unwrap();

        assert_eq!((rendered.source_width, rendered.source_height), (16, 16));
        assert_eq!((rendered.width, rendered.height), (16, 16));
        assert!(!rendered.animated);
        assert_eq!(rendered.frame_delay_ms, 0);
        assert_eq!(rendered.frames.len(), 1);
        assert_eq!(
            rendered.frames[0].len(),
            (rendered.width * rendered.height * 4) as usize
        );
        assert_eq!(&rendered.frames[0][..4], &[198, 120, 49, 255]);
    }

    #[test]
    fn an_animated_document_renders_distinct_bounded_frames() {
        let rendered = render(
            "<svg viewBox='0 0 40 10'><rect width='10' height='10'>\
             <animate attributeName='x' from='0' to='30' dur='1s' \
             repeatCount='indefinite'/></rect></svg>",
        )
        .unwrap();

        assert!(rendered.animated);
        assert!(rendered.frames.len() >= 2);
        assert!(rendered.frames.len() <= MAX_VECTOR_FRAMES as usize);
        assert!(rendered.frame_delay_ms >= MIN_VECTOR_FRAME_DELAY_MS);
        assert!(rendered.width <= 384 && rendered.height <= 288);
        assert_ne!(
            rendered.frames[0],
            rendered.frames[rendered.frames.len() - 1],
            "a moving shape must produce different frames"
        );
        for frame in &rendered.frames {
            assert_eq!(frame.len(), (rendered.width * rendered.height * 4) as usize);
            for pixel in frame.chunks_exact(4) {
                assert!(pixel[0] <= pixel[3]);
                assert!(pixel[1] <= pixel[3]);
                assert!(pixel[2] <= pixel[3]);
            }
        }
    }

    #[test]
    fn rendering_is_deterministic() {
        let source = "<svg viewBox='0 0 20 20'><circle cx='10' cy='10' r='8' fill='red'>\
                      <animate attributeName='r' values='2;8' dur='400ms'/></circle></svg>";
        assert_eq!(render(source), render(source));
    }

    #[test]
    fn unsupported_and_hostile_documents_fail_closed() {
        assert_eq!(
            render("<svg><script>fetch('x')</script></svg>"),
            Err(SvgError::ActiveContent)
        );
        assert_eq!(
            render("<!DOCTYPE svg [<!ENTITY a SYSTEM 'file:///x'>]><svg/>"),
            Err(SvgError::EntityDeclaration)
        );
        assert_eq!(
            render("<svg><image href='https://example.test/a.png'/></svg>"),
            Err(SvgError::ActiveContent)
        );
        assert_eq!(render("not markup"), Err(SvgError::NotSvg));
        assert_eq!(render("<svg><rect"), Err(SvgError::MalformedMarkup));
        assert_eq!(
            render("<svg viewBox='0 0 10 10'><path d='M0 0 Q'/></svg>"),
            Err(SvgError::MalformedPath)
        );
        // A document with only text content paints nothing this renderer supports.
        assert_eq!(
            render("<svg viewBox='0 0 10 10'><text x='0' y='5'>hello</text></svg>"),
            Err(SvgError::NothingPainted)
        );
        assert_eq!(
            render(&format!("<svg>{}</svg>", " ".repeat(5 * 1024 * 1024))),
            Err(SvgError::TooLarge)
        );
    }

    #[test]
    fn intrinsic_size_prefers_declared_lengths_then_view_box_then_the_default() {
        assert_eq!(intrinsic_length(Some("24"), Some(16.0), 300.0), 24.0);
        assert_eq!(intrinsic_length(Some("2in"), None, 300.0), 192.0);
        assert_eq!(intrinsic_length(Some("100%"), Some(16.0), 300.0), 16.0);
        assert_eq!(intrinsic_length(Some("0"), Some(16.0), 300.0), 16.0);
        assert_eq!(intrinsic_length(None, None, 300.0), 300.0);

        assert_eq!(source_pixel_size(10.4, 10.0), Ok((11, 10)));
        assert_eq!(source_pixel_size(0.0, 10.0), Err(SvgError::InvalidSize));
        assert_eq!(
            source_pixel_size(MAX_SOURCE_IMAGE_AXIS as f32 + 10.0, 10.0),
            Err(SvgError::InvalidSize)
        );
        assert_eq!(
            source_pixel_size(MAX_SOURCE_IMAGE_AXIS as f32, MAX_SOURCE_IMAGE_AXIS as f32),
            Err(SvgError::InvalidSize)
        );
    }

    #[test]
    fn frame_plans_stay_within_the_frame_and_delay_budget() {
        assert_eq!(frame_plan(None), (1, 0));
        let (count, delay) = frame_plan(Some(1_000));
        assert_eq!(count, MAX_VECTOR_FRAMES);
        assert!(delay >= MIN_VECTOR_FRAME_DELAY_MS);
        assert_eq!(frame_plan(Some(200)), (5, 40));
        let (short_count, short_delay) = frame_plan(Some(40));
        assert_eq!(short_count, 2);
        assert_eq!(short_delay, MIN_VECTOR_FRAME_DELAY_MS);
    }
}
