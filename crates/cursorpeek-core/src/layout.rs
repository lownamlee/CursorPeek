use std::{error::Error, fmt};

pub const MAX_PREVIEW_PAYLOAD_LEN: usize = 4 * 1024 * 1024;
pub const IMAGE_FIXED_LEN: usize = 52;
pub const BGRA_BYTES_PER_PIXEL: usize = 4;
pub const MAX_SOURCE_IMAGE_AXIS: u32 = 20_000;
pub const MAX_SOURCE_IMAGE_PIXELS: u64 = 40_000_000;
pub const MAX_PREVIEW_IMAGE_WIDTH: u32 = 960;
pub const MAX_PREVIEW_IMAGE_HEIGHT: u32 = 720;

pub const VECTOR_FIXED_LEN: usize = 56;
pub const MAX_VECTOR_PAYLOAD_LEN: usize = 8 * 1024 * 1024;
pub const MAX_VECTOR_FRAMES: u32 = 12;
pub const MIN_VECTOR_FRAME_DELAY_MS: u32 = 40;
pub const MAX_VECTOR_FRAME_DELAY_MS: u32 = 1_000;
pub const MAX_ANIMATED_VECTOR_WIDTH: u32 = 384;
pub const MAX_ANIMATED_VECTOR_HEIGHT: u32 = 288;
/// A vector document has no intrinsic pixel grid, so a small icon is enlarged up to this factor.
pub const MAX_VECTOR_UPSCALE: u32 = 8;

/// Largest encoded preview result of any kind, which also bounds one protocol frame.
pub const MAX_RESULT_PAYLOAD_LEN: usize = if MAX_VECTOR_PAYLOAD_LEN > MAX_PREVIEW_PAYLOAD_LEN {
    MAX_VECTOR_PAYLOAD_LEN
} else {
    MAX_PREVIEW_PAYLOAD_LEN
};

pub fn fitted_preview_dimensions(source_width: u32, source_height: u32) -> Option<(u32, u32)> {
    fit_dimensions(
        source_width,
        source_height,
        MAX_PREVIEW_IMAGE_WIDTH,
        MAX_PREVIEW_IMAGE_HEIGHT,
    )
}

pub fn fit_dimensions(
    source_width: u32,
    source_height: u32,
    max_width: u32,
    max_height: u32,
) -> Option<(u32, u32)> {
    if source_width == 0 || source_height == 0 || max_width == 0 || max_height == 0 {
        return None;
    }

    let width_bound = source_width.min(max_width);
    let height_bound = source_height.min(max_height);
    let round_ratio = |numerator: u32, factor: u32, denominator: u32| {
        u64::from(numerator)
            .checked_mul(u64::from(factor))
            .and_then(|product| product.checked_add(u64::from(denominator) / 2))
            .and_then(|rounded| rounded.checked_div(u64::from(denominator)))
            .and_then(|result| u32::try_from(result.max(1)).ok())
    };
    if u64::from(width_bound) * u64::from(source_height)
        <= u64::from(height_bound) * u64::from(source_width)
    {
        Some((
            width_bound,
            round_ratio(source_height, width_bound, source_width)?.min(height_bound),
        ))
    } else {
        Some((
            round_ratio(source_width, height_bound, source_height)?.min(width_bound),
            height_bound,
        ))
    }
}

pub const fn vector_canvas_bounds(animated: bool) -> (u32, u32) {
    if animated {
        (MAX_ANIMATED_VECTOR_WIDTH, MAX_ANIMATED_VECTOR_HEIGHT)
    } else {
        (MAX_PREVIEW_IMAGE_WIDTH, MAX_PREVIEW_IMAGE_HEIGHT)
    }
}

/// Canvas for a vector preview: an aspect-preserving fit that may enlarge small documents up to
/// [`MAX_VECTOR_UPSCALE`]. Animated documents trade resolution for frames.
pub fn fitted_vector_dimensions(
    source_width: u32,
    source_height: u32,
    animated: bool,
) -> Option<(u32, u32)> {
    if source_width == 0 || source_height == 0 {
        return None;
    }
    let (max_width, max_height) = vector_canvas_bounds(animated);
    let bound_width = source_width.saturating_mul(MAX_VECTOR_UPSCALE).min(max_width);
    let bound_height = source_height
        .saturating_mul(MAX_VECTOR_UPSCALE)
        .min(max_height);
    if bound_width == 0 || bound_height == 0 {
        return None;
    }

    let round_ratio = |numerator: u32, factor: u32, denominator: u32| {
        u64::from(numerator)
            .checked_mul(u64::from(factor))
            .and_then(|product| product.checked_add(u64::from(denominator) / 2))
            .and_then(|rounded| rounded.checked_div(u64::from(denominator)))
            .and_then(|result| u32::try_from(result.max(1)).ok())
    };
    if u64::from(bound_width) * u64::from(source_height)
        <= u64::from(bound_height) * u64::from(source_width)
    {
        Some((
            bound_width,
            round_ratio(source_height, bound_width, source_width)?.min(bound_height),
        ))
    } else {
        Some((
            round_ratio(source_width, bound_height, source_height)?.min(bound_width),
            bound_height,
        ))
    }
}

/// Returns `(stride, frame_bytes, total_frame_bytes)` for a bounded vector preview.
pub fn checked_vector_layout(
    width: u32,
    height: u32,
    frames: u32,
) -> Result<(usize, usize, usize), LayoutError> {
    if width == 0
        || height == 0
        || width > MAX_PREVIEW_IMAGE_WIDTH
        || height > MAX_PREVIEW_IMAGE_HEIGHT
    {
        return Err(LayoutError::InvalidDimensions { width, height });
    }
    if frames == 0 || frames > MAX_VECTOR_FRAMES {
        return Err(LayoutError::InvalidFrameCount { actual: frames });
    }

    let stride = usize::try_from(width)
        .ok()
        .and_then(|value| value.checked_mul(BGRA_BYTES_PER_PIXEL))
        .ok_or(LayoutError::ArithmeticOverflow)?;
    let frame_bytes = usize::try_from(height)
        .ok()
        .and_then(|value| stride.checked_mul(value))
        .ok_or(LayoutError::ArithmeticOverflow)?;
    let total = usize::try_from(frames)
        .ok()
        .and_then(|value| frame_bytes.checked_mul(value))
        .ok_or(LayoutError::ArithmeticOverflow)?;
    let wire_length = VECTOR_FIXED_LEN
        .checked_add(total)
        .ok_or(LayoutError::ArithmeticOverflow)?;
    if wire_length > MAX_VECTOR_PAYLOAD_LEN {
        return Err(LayoutError::PayloadTooLarge {
            actual: wire_length,
        });
    }
    Ok((stride, frame_bytes, total))
}

pub fn checked_bgra_layout(width: u32, height: u32) -> Result<(usize, usize), LayoutError> {
    if width == 0
        || height == 0
        || width > MAX_PREVIEW_IMAGE_WIDTH
        || height > MAX_PREVIEW_IMAGE_HEIGHT
    {
        return Err(LayoutError::InvalidDimensions { width, height });
    }
    let stride = usize::try_from(width)
        .ok()
        .and_then(|value| value.checked_mul(BGRA_BYTES_PER_PIXEL))
        .ok_or(LayoutError::ArithmeticOverflow)?;
    let length = usize::try_from(height)
        .ok()
        .and_then(|value| stride.checked_mul(value))
        .ok_or(LayoutError::ArithmeticOverflow)?;
    let wire_length = IMAGE_FIXED_LEN
        .checked_add(length)
        .ok_or(LayoutError::ArithmeticOverflow)?;
    if wire_length > MAX_PREVIEW_PAYLOAD_LEN {
        return Err(LayoutError::PayloadTooLarge {
            actual: wire_length,
        });
    }
    Ok((stride, length))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutError {
    InvalidDimensions { width: u32, height: u32 },
    InvalidFrameCount { actual: u32 },
    ArithmeticOverflow,
    PayloadTooLarge { actual: usize },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions { width, height } => {
                write!(formatter, "invalid preview dimensions {width}x{height}")
            }
            Self::InvalidFrameCount { actual } => write!(
                formatter,
                "invalid preview frame count {actual}; the limit is {MAX_VECTOR_FRAMES}"
            ),
            Self::ArithmeticOverflow => write!(formatter, "preview layout arithmetic overflow"),
            Self::PayloadTooLarge { actual } => write!(
                formatter,
                "preview layout length {actual} exceeds {MAX_RESULT_PAYLOAD_LEN} bytes"
            ),
        }
    }
}

impl Error for LayoutError {}

#[cfg(test)]
mod tests {
    use super::{
        BGRA_BYTES_PER_PIXEL, LayoutError, MAX_ANIMATED_VECTOR_HEIGHT, MAX_ANIMATED_VECTOR_WIDTH,
        MAX_PREVIEW_IMAGE_HEIGHT, MAX_PREVIEW_IMAGE_WIDTH, MAX_VECTOR_FRAMES, MAX_VECTOR_UPSCALE,
        checked_bgra_layout, checked_vector_layout, fit_dimensions, fitted_preview_dimensions,
        fitted_vector_dimensions,
    };

    #[test]
    fn fitted_dimensions_preserve_bounds_and_aspect_direction() {
        assert_eq!(fitted_preview_dimensions(1_920, 1_080), Some((960, 540)));
        assert_eq!(fitted_preview_dimensions(100, 200), Some((100, 200)));
        assert_eq!(fitted_preview_dimensions(0, 1), None);
    }

    #[test]
    fn arbitrary_fits_never_upscale_and_preserve_aspect_ratio() {
        assert_eq!(fit_dimensions(480, 300, 632, 416), Some((480, 300)));
        assert_eq!(fit_dimensions(1_920, 1_080, 632, 416), Some((632, 356)));
        assert_eq!(fit_dimensions(1_080, 1_920, 632, 416), Some((234, 416)));
        assert_eq!(fit_dimensions(1, 1, 0, 416), None);
    }

    #[test]
    fn bgra_layout_uses_checked_bounded_arithmetic() {
        let (stride, length) =
            checked_bgra_layout(MAX_PREVIEW_IMAGE_WIDTH, MAX_PREVIEW_IMAGE_HEIGHT).unwrap();
        assert_eq!(
            stride,
            MAX_PREVIEW_IMAGE_WIDTH as usize * BGRA_BYTES_PER_PIXEL
        );
        assert_eq!(length, stride * MAX_PREVIEW_IMAGE_HEIGHT as usize);
        assert_eq!(
            checked_bgra_layout(0, 1),
            Err(LayoutError::InvalidDimensions {
                width: 0,
                height: 1,
            })
        );
        assert!(matches!(
            checked_bgra_layout(u32::MAX, u32::MAX),
            Err(LayoutError::InvalidDimensions { .. })
        ));
    }

    #[test]
    fn vector_canvases_enlarge_small_documents_within_a_capped_factor() {
        // A tiny icon is enlarged by the factor cap rather than to the full canvas.
        assert_eq!(fitted_vector_dimensions(16, 16, false), Some((128, 128)));
        assert_eq!(
            fitted_vector_dimensions(16, 16, false).map(|(width, _)| width),
            Some(16 * MAX_VECTOR_UPSCALE)
        );
        // A large document is reduced to the still bound, preserving aspect ratio.
        assert_eq!(fitted_vector_dimensions(1_920, 1_080, false), Some((960, 540)));
        // Animation trades resolution for frames.
        assert_eq!(fitted_vector_dimensions(1_920, 1_080, true), Some((384, 216)));
        assert_eq!(
            fitted_vector_dimensions(4_000, 4_000, true),
            Some((MAX_ANIMATED_VECTOR_HEIGHT, MAX_ANIMATED_VECTOR_HEIGHT))
        );
        assert!(fitted_vector_dimensions(1, 0, false).is_none());
        assert!(
            fitted_vector_dimensions(4_000, 1, true)
                .is_some_and(|(width, height)| width <= MAX_ANIMATED_VECTOR_WIDTH && height >= 1)
        );
    }

    #[test]
    fn vector_layout_bounds_frames_and_total_bytes() {
        let (stride, frame_bytes, total) = checked_vector_layout(384, 288, 12).unwrap();
        assert_eq!(stride, 384 * BGRA_BYTES_PER_PIXEL);
        assert_eq!(frame_bytes, stride * 288);
        assert_eq!(total, frame_bytes * 12);

        assert_eq!(
            checked_vector_layout(0, 10, 1),
            Err(LayoutError::InvalidDimensions {
                width: 0,
                height: 10
            })
        );
        assert_eq!(
            checked_vector_layout(10, 10, 0),
            Err(LayoutError::InvalidFrameCount { actual: 0 })
        );
        assert_eq!(
            checked_vector_layout(10, 10, MAX_VECTOR_FRAMES + 1),
            Err(LayoutError::InvalidFrameCount {
                actual: MAX_VECTOR_FRAMES + 1
            })
        );
        assert!(matches!(
            checked_vector_layout(MAX_PREVIEW_IMAGE_WIDTH, MAX_PREVIEW_IMAGE_HEIGHT, 12),
            Err(LayoutError::PayloadTooLarge { .. })
        ));
    }
}
