use crate::hover::PhysicalScreenPoint;

const BASE_DPI: i64 = 96;
const DIAGNOSTIC_WIDTH: i64 = 320;
const DIAGNOSTIC_HEIGHT: i64 = 240;
const POINTER_GAP: i64 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScreenRect {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PreviewPlacement {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

pub(crate) fn place_diagnostic_preview(
    anchor: PhysicalScreenPoint,
    work_area: ScreenRect,
    dpi: u32,
) -> Option<PreviewPlacement> {
    let work_left = i64::from(work_area.left);
    let work_top = i64::from(work_area.top);
    let work_right = i64::from(work_area.right);
    let work_bottom = i64::from(work_area.bottom);
    let work_width = work_right.checked_sub(work_left)?;
    let work_height = work_bottom.checked_sub(work_top)?;
    if work_width <= 0 || work_height <= 0 || dpi == 0 {
        return None;
    }

    let width = scale_from_96_dpi(DIAGNOSTIC_WIDTH, dpi)?.min(work_width);
    let height = scale_from_96_dpi(DIAGNOSTIC_HEIGHT, dpi)?.min(work_height);
    let gap = scale_from_96_dpi(POINTER_GAP, dpi)?;
    let anchor_x = i64::from(anchor.x);
    let anchor_y = i64::from(anchor.y);

    let candidates = [
        (anchor_x + gap, anchor_y + gap),
        (anchor_x - gap - width, anchor_y + gap),
        (anchor_x + gap, anchor_y - gap - height),
        (anchor_x - gap - width, anchor_y - gap - height),
    ];

    let max_x = work_right - width;
    let max_y = work_bottom - height;
    let mut best = None;
    let mut best_area = -1_i64;
    for candidate in candidates {
        let area = visible_area(
            candidate.0,
            candidate.1,
            width,
            height,
            work_left,
            work_top,
            work_right,
            work_bottom,
        );
        let clamped = (
            candidate.0.clamp(work_left, max_x),
            candidate.1.clamp(work_top, max_y),
        );
        if preserves_pointer_gap(clamped.0, clamped.1, width, height, anchor_x, anchor_y, gap)
            && area > best_area
        {
            best = Some(clamped);
            best_area = area;
        }
    }
    let (x, y) = best?;

    Some(PreviewPlacement {
        x: i32::try_from(x).ok()?,
        y: i32::try_from(y).ok()?,
        width: i32::try_from(width).ok()?,
        height: i32::try_from(height).ok()?,
    })
}

#[allow(clippy::too_many_arguments)]
fn preserves_pointer_gap(
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    anchor_x: i64,
    anchor_y: i64,
    gap: i64,
) -> bool {
    let horizontal_gap = if anchor_x < x {
        x - anchor_x
    } else if anchor_x >= x + width {
        anchor_x - (x + width)
    } else {
        0
    };
    let vertical_gap = if anchor_y < y {
        y - anchor_y
    } else if anchor_y >= y + height {
        anchor_y - (y + height)
    } else {
        0
    };

    horizontal_gap >= gap || vertical_gap >= gap
}

fn scale_from_96_dpi(value: i64, dpi: u32) -> Option<i64> {
    value
        .checked_mul(i64::from(dpi))?
        .checked_add(BASE_DPI - 1)
        .map(|scaled| scaled / BASE_DPI)
}

#[allow(clippy::too_many_arguments)]
fn visible_area(
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    work_left: i64,
    work_top: i64,
    work_right: i64,
    work_bottom: i64,
) -> i64 {
    let visible_width = (x + width).min(work_right) - x.max(work_left);
    let visible_height = (y + height).min(work_bottom) - y.max(work_top);
    visible_width.max(0) * visible_height.max(0)
}

#[cfg(test)]
mod tests {
    use super::{PreviewPlacement, ScreenRect, place_diagnostic_preview};
    use crate::hover::PhysicalScreenPoint;

    const PRIMARY_WORK_AREA: ScreenRect = ScreenRect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };

    #[test]
    fn prefers_right_below_when_the_preview_fits() {
        assert_eq!(
            place_diagnostic_preview(PhysicalScreenPoint::new(200, 100), PRIMARY_WORK_AREA, 96),
            Some(PreviewPlacement {
                x: 208,
                y: 108,
                width: 320,
                height: 240,
            })
        );
    }

    #[test]
    fn chooses_the_largest_visible_quadrant_near_an_edge() {
        assert_eq!(
            place_diagnostic_preview(PhysicalScreenPoint::new(1900, 1020), PRIMARY_WORK_AREA, 96),
            Some(PreviewPlacement {
                x: 1572,
                y: 772,
                width: 320,
                height: 240,
            })
        );
    }

    #[test]
    fn scales_up_and_handles_negative_monitor_coordinates() {
        let placement = place_diagnostic_preview(
            PhysicalScreenPoint::new(-1800, 100),
            ScreenRect {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1080,
            },
            144,
        )
        .expect("the negative-coordinate work area is valid");

        assert_eq!(placement.width, 480);
        assert_eq!(placement.height, 360);
        assert!(placement.x >= -1920);
        assert!(placement.x + placement.width <= 0);
        assert!(placement.y >= 0);
        assert!(placement.y + placement.height <= 1080);
    }

    #[test]
    fn caps_the_preview_to_a_small_work_area_and_rejects_invalid_input() {
        assert_eq!(
            place_diagnostic_preview(
                PhysicalScreenPoint::new(1_000, 1_000),
                ScreenRect {
                    left: -100,
                    top: -50,
                    right: 100,
                    bottom: 50,
                },
                192,
            ),
            Some(PreviewPlacement {
                x: -100,
                y: -50,
                width: 200,
                height: 100,
            })
        );
        assert_eq!(
            place_diagnostic_preview(
                PhysicalScreenPoint::new(0, 0),
                ScreenRect {
                    left: -100,
                    top: -50,
                    right: 100,
                    bottom: 50,
                },
                192,
            ),
            None,
            "a work-area-sized popup must not cover its anchor"
        );
        assert_eq!(
            place_diagnostic_preview(
                PhysicalScreenPoint::new(0, 0),
                ScreenRect {
                    left: 1,
                    top: 0,
                    right: 1,
                    bottom: 10,
                },
                96,
            ),
            None
        );
        assert_eq!(
            place_diagnostic_preview(PhysicalScreenPoint::new(0, 0), PRIMARY_WORK_AREA, 0),
            None
        );
    }
}
