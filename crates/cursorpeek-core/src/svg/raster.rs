//! Anti-aliased scanline rasterizer producing premultiplied BGRA.

use super::geom::{Contour, FillRule, Point, Transform};
use super::value::Rgba;

const SUB_SCANLINES: usize = 4;
const BYTES_PER_PIXEL: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GradientStop {
    pub(super) offset: f32,
    pub(super) color: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum GradientGeometry {
    Linear { x1: f32, y1: f32, x2: f32, y2: f32 },
    Radial { cx: f32, cy: f32, radius: f32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SpreadMethod {
    Pad,
    Reflect,
    Repeat,
}

#[derive(Clone, Debug)]
pub(super) struct GradientPaint {
    pub(super) geometry: GradientGeometry,
    pub(super) spread: SpreadMethod,
    pub(super) stops: Vec<GradientStop>,
    pub(super) device_to_gradient: Transform,
}

#[derive(Clone, Debug)]
pub(super) enum ShapePaint {
    Solid(Rgba),
    Gradient(GradientPaint),
}

impl ShapePaint {
    fn color_at(&self, x: f32, y: f32) -> Rgba {
        match self {
            Self::Solid(color) => *color,
            Self::Gradient(gradient) => gradient.color_at(x, y),
        }
    }

    fn is_uniform(&self) -> bool {
        matches!(self, Self::Solid(_))
    }
}

impl GradientPaint {
    fn color_at(&self, x: f32, y: f32) -> Rgba {
        let local = self.device_to_gradient.apply(Point::new(x, y));
        let raw = match self.geometry {
            GradientGeometry::Linear { x1, y1, x2, y2 } => {
                let dx = x2 - x1;
                let dy = y2 - y1;
                let length_squared = dx * dx + dy * dy;
                if length_squared <= f32::EPSILON {
                    1.0
                } else {
                    ((local.x - x1) * dx + (local.y - y1) * dy) / length_squared
                }
            }
            GradientGeometry::Radial { cx, cy, radius } => {
                if radius <= f32::EPSILON {
                    1.0
                } else {
                    (local.x - cx).hypot(local.y - cy) / radius
                }
            }
        };
        self.stop_color(self.spread_offset(raw))
    }

    fn spread_offset(&self, raw: f32) -> f32 {
        if !raw.is_finite() {
            return 0.0;
        }
        match self.spread {
            SpreadMethod::Pad => raw.clamp(0.0, 1.0),
            SpreadMethod::Repeat => raw.rem_euclid(1.0),
            SpreadMethod::Reflect => {
                let cycle = raw.rem_euclid(2.0);
                if cycle > 1.0 { 2.0 - cycle } else { cycle }
            }
        }
    }

    fn stop_color(&self, offset: f32) -> Rgba {
        let Some(first) = self.stops.first() else {
            return Rgba::BLACK;
        };
        if offset <= first.offset {
            return first.color;
        }
        for pair in self.stops.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            if offset <= end.offset {
                let span = end.offset - start.offset;
                let local = if span <= f32::EPSILON {
                    1.0
                } else {
                    (offset - start.offset) / span
                };
                return start.color.lerp(end.color, local);
            }
        }
        self.stops[self.stops.len() - 1].color
    }
}

pub(super) struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    pub(super) fn new(width: u32, height: u32) -> Option<Self> {
        let length = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(BYTES_PER_PIXEL)?;
        if width == 0 || height == 0 {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels: vec![0; length],
        })
    }

    pub(super) fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /// Fills `contours` (device pixels) with `paint`, scaled by `opacity`, and returns the
    /// scanline work performed so the caller can charge it against a budget.
    pub(super) fn fill(
        &mut self,
        contours: &[Contour],
        rule: FillRule,
        paint: &ShapePaint,
        opacity: f32,
    ) -> usize {
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            return 0;
        }
        let mut edges = collect_edges(contours);
        if edges.is_empty() {
            return 0;
        }
        edges.sort_by(|left, right| {
            left.min_y
                .partial_cmp(&right.min_y)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let (min_x, max_x, min_y, max_y) = edge_bounds(&edges);
        let row_start = min_y.floor().max(0.0) as u32;
        let row_end = (max_y.ceil().max(0.0) as u32).min(self.height);
        let column_start = min_x.floor().max(0.0) as u32;
        let column_end = (max_x.ceil().max(0.0) as u32).min(self.width);
        if row_start >= row_end || column_start >= column_end {
            return 0;
        }

        let span = (column_end - column_start) as usize;
        let mut coverage = vec![0.0_f32; span];
        let mut crossings: Vec<(f32, i32)> = Vec::new();
        let mut active: Vec<usize> = Vec::new();
        let mut next_edge = 0_usize;
        let mut work = 0_usize;

        for row in row_start..row_end {
            let top = row as f32;
            let bottom = top + 1.0;
            while next_edge < edges.len() && edges[next_edge].min_y < bottom {
                active.push(next_edge);
                next_edge += 1;
            }
            active.retain(|index| edges[*index].max_y > top);
            if active.is_empty() {
                continue;
            }
            work += active.len();

            coverage.fill(0.0);
            let weight = 1.0 / SUB_SCANLINES as f32;
            for sub in 0..SUB_SCANLINES {
                let sample_y = top + (sub as f32 + 0.5) * weight;
                crossings.clear();
                for index in &active {
                    if let Some(crossing) = edges[*index].crossing(sample_y) {
                        crossings.push(crossing);
                    }
                }
                if crossings.len() < 2 {
                    continue;
                }
                crossings.sort_by(|left, right| {
                    left.0
                        .partial_cmp(&right.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                let mut winding = 0_i32;
                for pair in 0..crossings.len() - 1 {
                    winding += crossings[pair].1;
                    if !is_inside(rule, winding) {
                        continue;
                    }
                    add_span(
                        &mut coverage,
                        crossings[pair].0,
                        crossings[pair + 1].0,
                        weight,
                        column_start,
                    );
                }
            }
            self.composite_row(row, column_start, &coverage, paint, opacity);
        }
        work
    }

    fn composite_row(
        &mut self,
        row: u32,
        column_start: u32,
        coverage: &[f32],
        paint: &ShapePaint,
        opacity: f32,
    ) {
        let uniform = paint.is_uniform().then(|| paint.color_at(0.0, 0.0));
        let row_offset = row as usize * self.width as usize * BYTES_PER_PIXEL;
        let center_y = row as f32 + 0.5;

        for (index, value) in coverage.iter().enumerate() {
            let alpha = value.clamp(0.0, 1.0) * opacity;
            if alpha <= 1.0 / 512.0 {
                continue;
            }
            let column = column_start as usize + index;
            let color = match uniform {
                Some(color) => color,
                None => paint.color_at(column as f32 + 0.5, center_y),
            };
            let source_alpha = alpha * color.alpha;
            if source_alpha <= 0.0 {
                continue;
            }
            let offset = row_offset + column * BYTES_PER_PIXEL;
            blend_pixel(
                &mut self.pixels[offset..offset + BYTES_PER_PIXEL],
                color,
                source_alpha,
            );
        }
    }
}

fn blend_pixel(pixel: &mut [u8], color: Rgba, source_alpha: f32) {
    let inverse = 1.0 - source_alpha;
    let channel = |source: u8, destination: u8| {
        let source = f32::from(source) / 255.0 * source_alpha;
        let destination = f32::from(destination) / 255.0 * inverse;
        ((source + destination) * 255.0).round().clamp(0.0, 255.0) as u8
    };
    let alpha = {
        let value = source_alpha + f32::from(pixel[3]) / 255.0 * inverse;
        (value * 255.0).round().clamp(0.0, 255.0) as u8
    };
    let blue = channel(color.blue, pixel[0]).min(alpha);
    let green = channel(color.green, pixel[1]).min(alpha);
    let red = channel(color.red, pixel[2]).min(alpha);
    pixel[0] = blue;
    pixel[1] = green;
    pixel[2] = red;
    pixel[3] = alpha;
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    min_y: f32,
    max_y: f32,
    direction: i32,
}

impl Edge {
    fn crossing(&self, sample_y: f32) -> Option<(f32, i32)> {
        if sample_y < self.min_y || sample_y >= self.max_y {
            return None;
        }
        let span = self.y1 - self.y0;
        if span.abs() <= f32::EPSILON {
            return None;
        }
        let x = self.x0 + (sample_y - self.y0) * (self.x1 - self.x0) / span;
        x.is_finite().then_some((x, self.direction))
    }
}

fn collect_edges(contours: &[Contour]) -> Vec<Edge> {
    let mut edges = Vec::new();
    for contour in contours {
        let points = &contour.points;
        if points.len() < 2 {
            continue;
        }
        for index in 0..points.len() {
            let from = points[index];
            let to = points[(index + 1) % points.len()];
            if index + 1 == points.len() && from == to {
                continue;
            }
            if (to.y - from.y).abs() <= f32::EPSILON {
                continue;
            }
            edges.push(Edge {
                x0: from.x,
                y0: from.y,
                x1: to.x,
                y1: to.y,
                min_y: from.y.min(to.y),
                max_y: from.y.max(to.y),
                direction: if to.y > from.y { 1 } else { -1 },
            });
        }
    }
    edges
}

fn edge_bounds(edges: &[Edge]) -> (f32, f32, f32, f32) {
    let mut min_x = f32::MAX;
    let mut max_x = f32::MIN;
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;
    for edge in edges {
        min_x = min_x.min(edge.x0).min(edge.x1);
        max_x = max_x.max(edge.x0).max(edge.x1);
        min_y = min_y.min(edge.min_y);
        max_y = max_y.max(edge.max_y);
    }
    (min_x, max_x, min_y, max_y)
}

const fn is_inside(rule: FillRule, winding: i32) -> bool {
    match rule {
        FillRule::NonZero => winding != 0,
        FillRule::EvenOdd => winding % 2 != 0,
    }
}

fn add_span(coverage: &mut [f32], start: f32, end: f32, weight: f32, column_start: u32) {
    if !start.is_finite() || !end.is_finite() || end <= start {
        return;
    }
    let origin = column_start as f32;
    let limit = origin + coverage.len() as f32;
    let start = start.max(origin);
    let end = end.min(limit);
    if end <= start {
        return;
    }

    let first = (start - origin).floor() as usize;
    let last = ((end - origin).ceil() as usize).min(coverage.len());
    for (index, value) in coverage.iter_mut().enumerate().take(last).skip(first) {
        let pixel_left = origin + index as f32;
        let overlap = (end.min(pixel_left + 1.0) - start.max(pixel_left)).clamp(0.0, 1.0);
        *value += overlap * weight;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Canvas, GradientGeometry, GradientPaint, GradientStop, ShapePaint, SpreadMethod, add_span,
        is_inside,
    };
    use crate::svg::geom::{Contour, FillRule, Point, Transform};
    use crate::svg::value::Rgba;

    fn square(size: f32) -> Vec<Contour> {
        vec![Contour {
            points: vec![
                Point::new(0.0, 0.0),
                Point::new(size, 0.0),
                Point::new(size, size),
                Point::new(0.0, size),
            ],
            closed: true,
        }]
    }

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = (y as usize * width as usize + x as usize) * 4;
        [
            pixels[offset],
            pixels[offset + 1],
            pixels[offset + 2],
            pixels[offset + 3],
        ]
    }

    #[test]
    fn canvas_rejects_degenerate_sizes_and_starts_transparent() {
        assert!(Canvas::new(0, 4).is_none());
        assert!(Canvas::new(4, 0).is_none());
        let canvas = Canvas::new(2, 2).expect("a small canvas is valid");
        assert_eq!(canvas.into_pixels(), vec![0; 16]);
    }

    #[test]
    fn solid_fill_covers_interior_and_leaves_the_outside_transparent() {
        let mut canvas = Canvas::new(8, 8).unwrap();
        canvas.fill(
            &square(4.0),
            FillRule::NonZero,
            &ShapePaint::Solid(Rgba::opaque(255, 0, 0)),
            1.0,
        );
        let pixels = canvas.into_pixels();

        assert_eq!(pixel(&pixels, 8, 1, 1), [0, 0, 255, 255]);
        assert_eq!(pixel(&pixels, 8, 6, 6), [0, 0, 0, 0]);
    }

    #[test]
    fn premultiplied_output_never_exceeds_its_alpha() {
        let mut canvas = Canvas::new(6, 6).unwrap();
        canvas.fill(
            &square(6.0),
            FillRule::NonZero,
            &ShapePaint::Solid(Rgba {
                alpha: 0.5,
                ..Rgba::opaque(255, 255, 255)
            }),
            0.5,
        );
        let pixels = canvas.into_pixels();
        for chunk in pixels.chunks_exact(4) {
            assert!(chunk[0] <= chunk[3]);
            assert!(chunk[1] <= chunk[3]);
            assert!(chunk[2] <= chunk[3]);
        }
        assert!(pixel(&pixels, 6, 3, 3)[3] > 0);
        assert!(pixel(&pixels, 6, 3, 3)[3] < 255);
    }

    #[test]
    fn anti_aliasing_produces_partial_edge_coverage() {
        let mut canvas = Canvas::new(4, 4).unwrap();
        canvas.fill(
            &[Contour {
                points: vec![
                    Point::new(0.0, 0.0),
                    Point::new(2.5, 0.0),
                    Point::new(2.5, 4.0),
                    Point::new(0.0, 4.0),
                ],
                closed: true,
            }],
            FillRule::NonZero,
            &ShapePaint::Solid(Rgba::opaque(0, 0, 0)),
            1.0,
        );
        let pixels = canvas.into_pixels();
        let edge_alpha = pixel(&pixels, 4, 2, 1)[3];
        assert!(
            edge_alpha > 0 && edge_alpha < 255,
            "edge alpha {edge_alpha}"
        );
        assert_eq!(pixel(&pixels, 4, 1, 1)[3], 255);
    }

    #[test]
    fn even_odd_and_non_zero_rules_disagree_on_nested_contours() {
        let nested = vec![
            Contour {
                points: vec![
                    Point::new(0.0, 0.0),
                    Point::new(8.0, 0.0),
                    Point::new(8.0, 8.0),
                    Point::new(0.0, 8.0),
                ],
                closed: true,
            },
            Contour {
                points: vec![
                    Point::new(2.0, 2.0),
                    Point::new(6.0, 2.0),
                    Point::new(6.0, 6.0),
                    Point::new(2.0, 6.0),
                ],
                closed: true,
            },
        ];

        let mut even_odd = Canvas::new(8, 8).unwrap();
        even_odd.fill(
            &nested,
            FillRule::EvenOdd,
            &ShapePaint::Solid(Rgba::opaque(0, 0, 0)),
            1.0,
        );
        let mut non_zero = Canvas::new(8, 8).unwrap();
        non_zero.fill(
            &nested,
            FillRule::NonZero,
            &ShapePaint::Solid(Rgba::opaque(0, 0, 0)),
            1.0,
        );

        assert_eq!(pixel(&even_odd.into_pixels(), 8, 4, 4)[3], 0);
        assert_eq!(pixel(&non_zero.into_pixels(), 8, 4, 4)[3], 255);
    }

    #[test]
    fn linear_gradients_vary_along_their_axis_and_honour_spread() {
        let paint = GradientPaint {
            geometry: GradientGeometry::Linear {
                x1: 0.0,
                y1: 0.0,
                x2: 8.0,
                y2: 0.0,
            },
            spread: SpreadMethod::Pad,
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Rgba::opaque(0, 0, 0),
                },
                GradientStop {
                    offset: 1.0,
                    color: Rgba::opaque(255, 255, 255),
                },
            ],
            device_to_gradient: Transform::IDENTITY,
        };
        let mut canvas = Canvas::new(8, 2).unwrap();
        canvas.fill(
            &[Contour {
                points: vec![
                    Point::new(0.0, 0.0),
                    Point::new(8.0, 0.0),
                    Point::new(8.0, 2.0),
                    Point::new(0.0, 2.0),
                ],
                closed: true,
            }],
            FillRule::NonZero,
            &ShapePaint::Gradient(paint),
            1.0,
        );
        let pixels = canvas.into_pixels();
        assert!(pixel(&pixels, 8, 7, 0)[2] > pixel(&pixels, 8, 0, 0)[2]);
    }

    #[test]
    fn radial_gradients_and_spread_methods_stay_in_range() {
        let paint = GradientPaint {
            geometry: GradientGeometry::Radial {
                cx: 0.0,
                cy: 0.0,
                radius: 1.0,
            },
            spread: SpreadMethod::Reflect,
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Rgba::opaque(0, 0, 0),
                },
                GradientStop {
                    offset: 1.0,
                    color: Rgba::opaque(255, 255, 255),
                },
            ],
            device_to_gradient: Transform::IDENTITY,
        };
        for distance in [0.0_f32, 0.5, 1.5, 2.5, 100.0] {
            let color = paint.color_at(distance, 0.0);
            assert!(color.alpha > 0.0);
        }

        let empty = GradientPaint {
            stops: Vec::new(),
            ..paint
        };
        assert_eq!(empty.color_at(0.0, 0.0), Rgba::BLACK);
    }

    #[test]
    fn span_coverage_is_clipped_to_the_row_window() {
        let mut coverage = vec![0.0_f32; 4];
        add_span(&mut coverage, -10.0, 10.0, 1.0, 0);
        assert_eq!(coverage, vec![1.0, 1.0, 1.0, 1.0]);

        let mut partial = vec![0.0_f32; 4];
        add_span(&mut partial, 0.5, 1.5, 1.0, 0);
        assert!((partial[0] - 0.5).abs() < 1e-5);
        assert!((partial[1] - 0.5).abs() < 1e-5);

        let mut offset = vec![0.0_f32; 2];
        add_span(&mut offset, 5.0, 6.0, 1.0, 5);
        assert_eq!(offset, vec![1.0, 0.0]);

        let mut empty = vec![0.0_f32; 2];
        add_span(&mut empty, 1.0, 1.0, 1.0, 0);
        add_span(&mut empty, f32::NAN, 1.0, 1.0, 0);
        assert_eq!(empty, vec![0.0, 0.0]);
    }

    #[test]
    fn fill_rule_predicate_matches_its_definition() {
        assert!(!is_inside(FillRule::NonZero, 0));
        assert!(is_inside(FillRule::NonZero, -1));
        assert!(is_inside(FillRule::EvenOdd, 1));
        assert!(!is_inside(FillRule::EvenOdd, 2));
        assert!(is_inside(FillRule::EvenOdd, -1));
    }
}
