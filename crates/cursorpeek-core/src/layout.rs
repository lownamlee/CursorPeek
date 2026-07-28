use std::{error::Error, fmt};

pub const MAX_PREVIEW_PAYLOAD_LEN: usize = 4 * 1024 * 1024;
pub const IMAGE_FIXED_LEN: usize = 52;
pub const BGRA_BYTES_PER_PIXEL: usize = 4;
pub const MAX_SOURCE_IMAGE_AXIS: u32 = 20_000;
pub const MAX_SOURCE_IMAGE_PIXELS: u64 = 40_000_000;
pub const MAX_PREVIEW_IMAGE_WIDTH: u32 = 960;
pub const MAX_PREVIEW_IMAGE_HEIGHT: u32 = 720;

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
    ArithmeticOverflow,
    PayloadTooLarge { actual: usize },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions { width, height } => {
                write!(formatter, "invalid preview dimensions {width}x{height}")
            }
            Self::ArithmeticOverflow => write!(formatter, "preview layout arithmetic overflow"),
            Self::PayloadTooLarge { actual } => write!(
                formatter,
                "preview layout length {actual} exceeds {MAX_PREVIEW_PAYLOAD_LEN} bytes"
            ),
        }
    }
}

impl Error for LayoutError {}

#[cfg(test)]
mod tests {
    use super::{
        BGRA_BYTES_PER_PIXEL, LayoutError, MAX_PREVIEW_IMAGE_HEIGHT, MAX_PREVIEW_IMAGE_WIDTH,
        checked_bgra_layout, fit_dimensions, fitted_preview_dimensions,
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
}
