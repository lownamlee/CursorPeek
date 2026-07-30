//! Affine transforms, path geometry, flattening, dashing, and stroke outlining.
//!
//! Every routine is bounded arithmetic with no clock input, so a document always flattens to the
//! same polygons.

use super::SvgError;

const MIN_FLATTEN_STEPS: usize = 1;
const MAX_FLATTEN_STEPS: usize = 96;
const MAX_ARC_STEPS: usize = 144;
const JOIN_SEGMENTS: usize = 12;
const MIN_JOIN_WIDTH: f32 = 1.25;
const MAX_DASH_RUNS: usize = 4_096;
const MIN_DASH_LENGTH: f32 = 0.25;
pub(super) const MAX_PATH_SEGMENTS: usize = 40_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Point {
    pub(super) x: f32,
    pub(super) y: f32,
}

impl Point {
    pub(super) const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    fn distance_to(self, other: Self) -> f32 {
        let dx = other.x - self.x;
        let dy = other.y - self.y;
        dx.hypot(dy)
    }
}

/// SVG affine matrix `matrix(a b c d e f)`: `(x, y)` maps to `(a·x + c·y + e, b·x + d·y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Transform {
    pub(super) a: f32,
    pub(super) b: f32,
    pub(super) c: f32,
    pub(super) d: f32,
    pub(super) e: f32,
    pub(super) f: f32,
}

impl Transform {
    pub(super) const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub(super) const fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self { a, b, c, d, e, f }
    }

    pub(super) const fn translate(x: f32, y: f32) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, x, y)
    }

    pub(super) const fn scale(x: f32, y: f32) -> Self {
        Self::new(x, 0.0, 0.0, y, 0.0, 0.0)
    }

    pub(super) fn rotate(degrees: f32) -> Self {
        let radians = degrees.to_radians();
        let (sin, cos) = radians.sin_cos();
        Self::new(cos, sin, -sin, cos, 0.0, 0.0)
    }

    pub(super) fn skew_x(degrees: f32) -> Self {
        Self::new(1.0, 0.0, degrees.to_radians().tan(), 1.0, 0.0, 0.0)
    }

    pub(super) fn skew_y(degrees: f32) -> Self {
        Self::new(1.0, degrees.to_radians().tan(), 0.0, 1.0, 0.0, 0.0)
    }

    /// Returns `self ∘ child`: the child transform is applied to a point first.
    pub(super) fn concat(self, child: Self) -> Self {
        Self {
            a: self.a * child.a + self.c * child.b,
            b: self.b * child.a + self.d * child.b,
            c: self.a * child.c + self.c * child.d,
            d: self.b * child.c + self.d * child.d,
            e: self.a * child.e + self.c * child.f + self.e,
            f: self.b * child.e + self.d * child.f + self.f,
        }
    }

    pub(super) fn apply(self, point: Point) -> Point {
        Point::new(
            self.a * point.x + self.c * point.y + self.e,
            self.b * point.x + self.d * point.y + self.f,
        )
    }

    pub(super) fn invert(self) -> Option<Self> {
        let determinant = self.a * self.d - self.b * self.c;
        if !determinant.is_finite() || determinant.abs() < f32::EPSILON {
            return None;
        }
        Some(Self {
            a: self.d / determinant,
            b: -self.b / determinant,
            c: -self.c / determinant,
            d: self.a / determinant,
            e: (self.c * self.f - self.d * self.e) / determinant,
            f: (self.b * self.e - self.a * self.f) / determinant,
        })
    }

    /// Geometric mean of the axis scales, used to convert user-space widths to device pixels.
    pub(super) fn average_scale(self) -> f32 {
        let determinant = (self.a * self.d - self.b * self.c).abs();
        if determinant.is_finite() && determinant > 0.0 {
            determinant.sqrt()
        } else {
            0.0
        }
    }

    pub(super) fn is_finite(self) -> bool {
        self.a.is_finite()
            && self.b.is_finite()
            && self.c.is_finite()
            && self.d.is_finite()
            && self.e.is_finite()
            && self.f.is_finite()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PathSeg {
    MoveTo(Point),
    LineTo(Point),
    QuadTo(Point, Point),
    CubicTo(Point, Point, Point),
    ArcTo {
        rx: f32,
        ry: f32,
        rotation: f32,
        large_arc: bool,
        sweep: bool,
        to: Point,
    },
    Close,
}

/// A flattened contour in device pixels.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Contour {
    pub(super) points: Vec<Point>,
    pub(super) closed: bool,
}

/// Axis-aligned bounds in user space, used for `objectBoundingBox` gradient units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Bounds {
    pub(super) min_x: f32,
    pub(super) min_y: f32,
    pub(super) max_x: f32,
    pub(super) max_y: f32,
}

impl Bounds {
    pub(super) fn of(segments: &[PathSeg]) -> Option<Self> {
        let mut bounds: Option<Self> = None;
        let mut include = |point: Point| {
            if !point.x.is_finite() || !point.y.is_finite() {
                return;
            }
            bounds = Some(match bounds {
                None => Self {
                    min_x: point.x,
                    min_y: point.y,
                    max_x: point.x,
                    max_y: point.y,
                },
                Some(current) => Self {
                    min_x: current.min_x.min(point.x),
                    min_y: current.min_y.min(point.y),
                    max_x: current.max_x.max(point.x),
                    max_y: current.max_y.max(point.y),
                },
            });
        };

        for segment in segments {
            match *segment {
                PathSeg::MoveTo(point) | PathSeg::LineTo(point) => include(point),
                PathSeg::QuadTo(control, point) => {
                    include(control);
                    include(point);
                }
                PathSeg::CubicTo(first, second, point) => {
                    include(first);
                    include(second);
                    include(point);
                }
                PathSeg::ArcTo { to, .. } => include(to),
                PathSeg::Close => {}
            }
        }
        bounds
    }

    pub(super) fn width(self) -> f32 {
        self.max_x - self.min_x
    }

    pub(super) fn height(self) -> f32 {
        self.max_y - self.min_y
    }
}

pub(super) fn rect_segments(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    rx: f32,
    ry: f32,
) -> Vec<PathSeg> {
    if width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }

    let rx = rx.clamp(0.0, width / 2.0);
    let ry = ry.clamp(0.0, height / 2.0);
    if rx <= 0.0 || ry <= 0.0 {
        return vec![
            PathSeg::MoveTo(Point::new(x, y)),
            PathSeg::LineTo(Point::new(x + width, y)),
            PathSeg::LineTo(Point::new(x + width, y + height)),
            PathSeg::LineTo(Point::new(x, y + height)),
            PathSeg::Close,
        ];
    }

    let arc = |to: Point| PathSeg::ArcTo {
        rx,
        ry,
        rotation: 0.0,
        large_arc: false,
        sweep: true,
        to,
    };
    vec![
        PathSeg::MoveTo(Point::new(x + rx, y)),
        PathSeg::LineTo(Point::new(x + width - rx, y)),
        arc(Point::new(x + width, y + ry)),
        PathSeg::LineTo(Point::new(x + width, y + height - ry)),
        arc(Point::new(x + width - rx, y + height)),
        PathSeg::LineTo(Point::new(x + rx, y + height)),
        arc(Point::new(x, y + height - ry)),
        PathSeg::LineTo(Point::new(x, y + ry)),
        arc(Point::new(x + rx, y)),
        PathSeg::Close,
    ]
}

pub(super) fn ellipse_segments(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<PathSeg> {
    if rx <= 0.0 || ry <= 0.0 {
        return Vec::new();
    }
    vec![
        PathSeg::MoveTo(Point::new(cx + rx, cy)),
        PathSeg::ArcTo {
            rx,
            ry,
            rotation: 0.0,
            large_arc: true,
            sweep: true,
            to: Point::new(cx - rx, cy),
        },
        PathSeg::ArcTo {
            rx,
            ry,
            rotation: 0.0,
            large_arc: true,
            sweep: true,
            to: Point::new(cx + rx, cy),
        },
        PathSeg::Close,
    ]
}

pub(super) fn polygon_segments(points: &[Point], closed: bool) -> Vec<PathSeg> {
    if points.len() < 2 {
        return Vec::new();
    }
    let mut segments = Vec::with_capacity(points.len() + 1);
    segments.push(PathSeg::MoveTo(points[0]));
    for point in &points[1..] {
        segments.push(PathSeg::LineTo(*point));
    }
    if closed {
        segments.push(PathSeg::Close);
    }
    segments
}

/// Flattens user-space segments into device-space contours under `transform`.
pub(super) fn flatten(segments: &[PathSeg], transform: Transform) -> Vec<Contour> {
    let mut contours: Vec<Contour> = Vec::new();
    let mut current: Vec<Point> = Vec::new();
    let mut start = Point::new(0.0, 0.0);
    let mut cursor = Point::new(0.0, 0.0);
    let scale = transform.average_scale();

    for segment in segments {
        match *segment {
            PathSeg::MoveTo(point) => {
                flush_contour(&mut current, false, &mut contours);
                start = point;
                cursor = point;
                current.push(transform.apply(point));
            }
            PathSeg::LineTo(point) => {
                if current.is_empty() {
                    current.push(transform.apply(cursor));
                }
                current.push(transform.apply(point));
                cursor = point;
            }
            PathSeg::QuadTo(control, point) => {
                if current.is_empty() {
                    current.push(transform.apply(cursor));
                }
                append_quad(
                    &mut current,
                    transform.apply(cursor),
                    transform.apply(control),
                    transform.apply(point),
                );
                cursor = point;
            }
            PathSeg::CubicTo(first, second, point) => {
                if current.is_empty() {
                    current.push(transform.apply(cursor));
                }
                append_cubic(
                    &mut current,
                    transform.apply(cursor),
                    transform.apply(first),
                    transform.apply(second),
                    transform.apply(point),
                );
                cursor = point;
            }
            PathSeg::ArcTo {
                rx,
                ry,
                rotation,
                large_arc,
                sweep,
                to,
            } => {
                if current.is_empty() {
                    current.push(transform.apply(cursor));
                }
                append_arc(
                    &mut current,
                    transform,
                    scale,
                    cursor,
                    rx,
                    ry,
                    rotation,
                    large_arc,
                    sweep,
                    to,
                );
                cursor = to;
            }
            PathSeg::Close => {
                flush_contour(&mut current, true, &mut contours);
                cursor = start;
                current.push(transform.apply(start));
            }
        }
    }
    flush_contour(&mut current, false, &mut contours);
    contours.retain(|contour| {
        contour
            .points
            .iter()
            .all(|point| point.x.is_finite() && point.y.is_finite())
    });
    contours
}

fn flush_contour(points: &mut Vec<Point>, closed: bool, contours: &mut Vec<Contour>) {
    if points.len() >= 2 {
        contours.push(Contour {
            points: std::mem::take(points),
            closed,
        });
    } else {
        points.clear();
    }
}

fn curve_steps(control_length: f32) -> usize {
    if !control_length.is_finite() || control_length <= 0.0 {
        return MIN_FLATTEN_STEPS;
    }
    let steps = (control_length / 3.0).ceil();
    if steps >= MAX_FLATTEN_STEPS as f32 {
        MAX_FLATTEN_STEPS
    } else {
        (steps as usize).max(MIN_FLATTEN_STEPS)
    }
}

fn append_quad(output: &mut Vec<Point>, from: Point, control: Point, to: Point) {
    let steps = curve_steps(from.distance_to(control) + control.distance_to(to));
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let inverse = 1.0 - t;
        let x = inverse * inverse * from.x + 2.0 * inverse * t * control.x + t * t * to.x;
        let y = inverse * inverse * from.y + 2.0 * inverse * t * control.y + t * t * to.y;
        output.push(Point::new(x, y));
    }
}

fn append_cubic(output: &mut Vec<Point>, from: Point, first: Point, second: Point, to: Point) {
    let steps =
        curve_steps(from.distance_to(first) + first.distance_to(second) + second.distance_to(to));
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let inverse = 1.0 - t;
        let w0 = inverse * inverse * inverse;
        let w1 = 3.0 * inverse * inverse * t;
        let w2 = 3.0 * inverse * t * t;
        let w3 = t * t * t;
        output.push(Point::new(
            w0 * from.x + w1 * first.x + w2 * second.x + w3 * to.x,
            w0 * from.y + w1 * first.y + w2 * second.y + w3 * to.y,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn append_arc(
    output: &mut Vec<Point>,
    transform: Transform,
    scale: f32,
    from: Point,
    rx: f32,
    ry: f32,
    rotation: f32,
    large_arc: bool,
    sweep: bool,
    to: Point,
) {
    let rx = rx.abs();
    let ry = ry.abs();
    if rx <= 0.0 || ry <= 0.0 || (from.x == to.x && from.y == to.y) {
        output.push(transform.apply(to));
        return;
    }

    // Endpoint to center parameterization (SVG 1.1 implementation notes).
    let radians = rotation.to_radians();
    let (sin_phi, cos_phi) = radians.sin_cos();
    let dx2 = (from.x - to.x) / 2.0;
    let dy2 = (from.y - to.y) / 2.0;
    let x1 = cos_phi * dx2 + sin_phi * dy2;
    let y1 = -sin_phi * dx2 + cos_phi * dy2;

    let mut rx = rx;
    let mut ry = ry;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        let correction = lambda.sqrt();
        rx *= correction;
        ry *= correction;
    }

    let numerator = (rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1).max(0.0);
    let denominator = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    if denominator <= 0.0 {
        output.push(transform.apply(to));
        return;
    }
    let mut coefficient = (numerator / denominator).sqrt();
    if large_arc == sweep {
        coefficient = -coefficient;
    }
    let cx1 = coefficient * rx * y1 / ry;
    let cy1 = -coefficient * ry * x1 / rx;
    let cx = cos_phi * cx1 - sin_phi * cy1 + (from.x + to.x) / 2.0;
    let cy = sin_phi * cx1 + cos_phi * cy1 + (from.y + to.y) / 2.0;

    let start_angle = ((y1 - cy1) / ry).atan2((x1 - cx1) / rx);
    let end_angle = ((-y1 - cy1) / ry).atan2((-x1 - cx1) / rx);
    let mut delta = end_angle - start_angle;
    let full_turn = std::f32::consts::TAU;
    if sweep && delta < 0.0 {
        delta += full_turn;
    } else if !sweep && delta > 0.0 {
        delta -= full_turn;
    }

    let device_radius = rx.max(ry) * if scale > 0.0 { scale } else { 1.0 };
    let arc_length = delta.abs() * device_radius;
    let steps = if arc_length.is_finite() && arc_length > 0.0 {
        ((arc_length / 3.0).ceil() as usize).clamp(2, MAX_ARC_STEPS)
    } else {
        2
    };
    for step in 1..=steps {
        let angle = start_angle + delta * (step as f32 / steps as f32);
        let (sin_angle, cos_angle) = angle.sin_cos();
        let x = cx + cos_phi * rx * cos_angle - sin_phi * ry * sin_angle;
        let y = cy + sin_phi * rx * cos_angle + cos_phi * ry * sin_angle;
        output.push(transform.apply(Point::new(x, y)));
    }
}

/// Builds a filled stroke outline in device pixels. Pieces share one winding direction so the
/// non-zero rasterizer unions them, which avoids miter and self-intersection special cases.
pub(super) fn stroke_outline(
    contours: &[Contour],
    width: f32,
    cap: LineCap,
    dash_pattern: &[f32],
    dash_offset: f32,
) -> Vec<Contour> {
    let half = width / 2.0;
    if !half.is_finite() || half <= 0.0 {
        return Vec::new();
    }

    let mut outline: Vec<Contour> = Vec::new();
    for contour in contours {
        let mut closed_points = contour.points.clone();
        if contour.closed
            && closed_points
                .first()
                .zip(closed_points.last())
                .is_some_and(|(first, last)| first != last)
        {
            closed_points.push(contour.points[0]);
        }

        let solid = dash_pattern.is_empty();
        let runs = if solid {
            vec![closed_points]
        } else {
            dash_polyline(&closed_points, dash_pattern, dash_offset)
        };
        for run in &runs {
            stroke_polyline(&mut outline, run, half, cap, contour.closed && solid);
        }
    }
    outline
}

fn stroke_polyline(
    outline: &mut Vec<Contour>,
    points: &[Point],
    half: f32,
    cap: LineCap,
    closed: bool,
) {
    if points.len() < 2 {
        if let Some(point) = points.first() {
            if cap == LineCap::Round {
                outline.push(round_join(*point, half));
            } else if cap == LineCap::Square {
                outline.push(square_cap(*point, half));
            }
        }
        return;
    }

    for window in points.windows(2) {
        let (from, to) = (window[0], window[1]);
        let dx = to.x - from.x;
        let dy = to.y - from.y;
        let length = dx.hypot(dy);
        if !length.is_finite() || length <= f32::EPSILON {
            continue;
        }
        let nx = -dy / length * half;
        let ny = dx / length * half;
        outline.push(canonical_contour(vec![
            Point::new(from.x + nx, from.y + ny),
            Point::new(to.x + nx, to.y + ny),
            Point::new(to.x - nx, to.y - ny),
            Point::new(from.x - nx, from.y - ny),
        ]));
    }

    if half * 2.0 < MIN_JOIN_WIDTH {
        return;
    }
    let interior = if closed {
        &points[..points.len().saturating_sub(1)]
    } else if points.len() > 2 {
        &points[1..points.len() - 1]
    } else {
        &[][..]
    };
    for point in interior {
        outline.push(round_join(*point, half));
    }
    if !closed {
        match cap {
            LineCap::Butt => {}
            LineCap::Round => {
                outline.push(round_join(points[0], half));
                outline.push(round_join(points[points.len() - 1], half));
            }
            LineCap::Square => {
                outline.push(square_cap(points[0], half));
                outline.push(square_cap(points[points.len() - 1], half));
            }
        }
    }
}

fn round_join(center: Point, radius: f32) -> Contour {
    let mut points = Vec::with_capacity(JOIN_SEGMENTS);
    for step in 0..JOIN_SEGMENTS {
        let angle = std::f32::consts::TAU * step as f32 / JOIN_SEGMENTS as f32;
        let (sin_angle, cos_angle) = angle.sin_cos();
        points.push(Point::new(
            center.x + radius * cos_angle,
            center.y + radius * sin_angle,
        ));
    }
    canonical_contour(points)
}

fn square_cap(center: Point, half: f32) -> Contour {
    canonical_contour(vec![
        Point::new(center.x - half, center.y - half),
        Point::new(center.x + half, center.y - half),
        Point::new(center.x + half, center.y + half),
        Point::new(center.x - half, center.y + half),
    ])
}

fn canonical_contour(mut points: Vec<Point>) -> Contour {
    if signed_area(&points) > 0.0 {
        points.reverse();
    }
    Contour {
        points,
        closed: true,
    }
}

fn signed_area(points: &[Point]) -> f32 {
    let mut total = 0.0;
    for index in 0..points.len() {
        let current = points[index];
        let next = points[(index + 1) % points.len()];
        total += current.x * next.y - next.x * current.y;
    }
    total / 2.0
}

fn dash_polyline(points: &[Point], pattern: &[f32], offset: f32) -> Vec<Vec<Point>> {
    // Entries are clamped to a visible minimum so a zero-length pattern cannot spin the walk.
    let mut lengths: Vec<f32> = pattern
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.max(MIN_DASH_LENGTH))
        .collect();
    if lengths.is_empty()
        || pattern
            .iter()
            .all(|value| !value.is_finite() || *value <= 0.0)
    {
        return vec![points.to_vec()];
    }
    if lengths.len() % 2 == 1 {
        lengths.extend_from_within(..);
    }
    let cycle: f32 = lengths.iter().sum();
    if !cycle.is_finite() || cycle <= 0.0 {
        return vec![points.to_vec()];
    }

    let mut index = 0_usize;
    let mut remaining = lengths[0];
    let mut drawing = true;
    let mut skipped = if offset.is_finite() {
        offset.rem_euclid(cycle)
    } else {
        0.0
    };
    while skipped > 0.0 {
        if skipped < remaining {
            remaining -= skipped;
            break;
        }
        skipped -= remaining;
        index = (index + 1) % lengths.len();
        remaining = lengths[index];
        drawing = !drawing;
    }

    let mut runs: Vec<Vec<Point>> = Vec::new();
    let mut current: Vec<Point> = Vec::new();
    let mut splits = 0_usize;
    if drawing {
        current.push(points[0]);
    }
    for window in points.windows(2) {
        let (from, to) = (window[0], window[1]);
        let total = from.distance_to(to);
        if !total.is_finite() || total <= 0.0 {
            continue;
        }
        let mut travelled = 0.0;
        while total - travelled > remaining {
            travelled += remaining;
            let ratio = travelled / total;
            let split = Point::new(
                from.x + (to.x - from.x) * ratio,
                from.y + (to.y - from.y) * ratio,
            );
            if drawing {
                current.push(split);
                if current.len() >= 2 {
                    runs.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            } else {
                current.clear();
                current.push(split);
            }
            drawing = !drawing;
            index = (index + 1) % lengths.len();
            remaining = lengths[index];
            splits += 1;
            if splits >= MAX_DASH_RUNS {
                return runs;
            }
        }
        remaining -= total - travelled;
        if drawing {
            current.push(to);
        }
    }
    if current.len() >= 2 {
        runs.push(current);
    }
    runs
}

pub(super) fn parse_path(data: &str) -> Result<Vec<PathSeg>, SvgError> {
    let mut scanner = PathScanner::new(data);
    let mut segments: Vec<PathSeg> = Vec::new();
    let mut cursor = Point::new(0.0, 0.0);
    let mut start = Point::new(0.0, 0.0);
    let mut previous_cubic_control: Option<Point> = None;
    let mut previous_quad_control: Option<Point> = None;
    let mut command = 0_u8;

    loop {
        scanner.skip_separators();
        if scanner.is_empty() {
            break;
        }
        if segments.len() > MAX_PATH_SEGMENTS {
            return Err(SvgError::TooComplex);
        }
        if let Some(next) = scanner.peek_command() {
            command = next;
            scanner.advance();
            if matches!(command, b'z' | b'Z') {
                segments.push(PathSeg::Close);
                cursor = start;
                previous_cubic_control = None;
                previous_quad_control = None;
                continue;
            }
        } else if command == 0 {
            return Err(SvgError::MalformedPath);
        } else if matches!(command, b'M' | b'm') {
            // Extra pairs after a move-to are implicit line-to commands.
            command = if command == b'M' { b'L' } else { b'l' };
        }

        let relative = command.is_ascii_lowercase();
        let base = if relative {
            cursor
        } else {
            Point::new(0.0, 0.0)
        };
        match command.to_ascii_uppercase() {
            b'M' => {
                let point = scanner.point(base)?;
                segments.push(PathSeg::MoveTo(point));
                start = point;
                cursor = point;
                previous_cubic_control = None;
                previous_quad_control = None;
            }
            b'L' => {
                let point = scanner.point(base)?;
                segments.push(PathSeg::LineTo(point));
                cursor = point;
                previous_cubic_control = None;
                previous_quad_control = None;
            }
            b'H' => {
                let x = scanner.number()? + base.x;
                let point = Point::new(x, cursor.y);
                segments.push(PathSeg::LineTo(point));
                cursor = point;
                previous_cubic_control = None;
                previous_quad_control = None;
            }
            b'V' => {
                let y = scanner.number()? + base.y;
                let point = Point::new(cursor.x, y);
                segments.push(PathSeg::LineTo(point));
                cursor = point;
                previous_cubic_control = None;
                previous_quad_control = None;
            }
            b'C' => {
                let first = scanner.point(base)?;
                let second = scanner.point(base)?;
                let to = scanner.point(base)?;
                segments.push(PathSeg::CubicTo(first, second, to));
                previous_cubic_control = Some(second);
                previous_quad_control = None;
                cursor = to;
            }
            b'S' => {
                let reflected = reflect(cursor, previous_cubic_control);
                let second = scanner.point(base)?;
                let to = scanner.point(base)?;
                segments.push(PathSeg::CubicTo(reflected, second, to));
                previous_cubic_control = Some(second);
                previous_quad_control = None;
                cursor = to;
            }
            b'Q' => {
                let control = scanner.point(base)?;
                let to = scanner.point(base)?;
                segments.push(PathSeg::QuadTo(control, to));
                previous_quad_control = Some(control);
                previous_cubic_control = None;
                cursor = to;
            }
            b'T' => {
                let control = reflect(cursor, previous_quad_control);
                let to = scanner.point(base)?;
                segments.push(PathSeg::QuadTo(control, to));
                previous_quad_control = Some(control);
                previous_cubic_control = None;
                cursor = to;
            }
            b'A' => {
                let rx = scanner.number()?;
                let ry = scanner.number()?;
                let rotation = scanner.number()?;
                let large_arc = scanner.flag()?;
                let sweep = scanner.flag()?;
                let to = scanner.point(base)?;
                segments.push(PathSeg::ArcTo {
                    rx,
                    ry,
                    rotation,
                    large_arc,
                    sweep,
                    to,
                });
                previous_cubic_control = None;
                previous_quad_control = None;
                cursor = to;
            }
            _ => return Err(SvgError::MalformedPath),
        }
    }
    Ok(segments)
}

fn reflect(cursor: Point, control: Option<Point>) -> Point {
    match control {
        Some(control) => Point::new(2.0 * cursor.x - control.x, 2.0 * cursor.y - control.y),
        None => cursor,
    }
}

struct PathScanner<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> PathScanner<'a> {
    fn new(data: &'a str) -> Self {
        Self {
            bytes: data.as_bytes(),
            offset: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.offset >= self.bytes.len()
    }

    fn advance(&mut self) {
        self.offset += 1;
    }

    fn peek_command(&self) -> Option<u8> {
        let byte = *self.bytes.get(self.offset)?;
        if matches!(
            byte,
            b'M' | b'm'
                | b'L'
                | b'l'
                | b'H'
                | b'h'
                | b'V'
                | b'v'
                | b'C'
                | b'c'
                | b'S'
                | b's'
                | b'Q'
                | b'q'
                | b'T'
                | b't'
                | b'A'
                | b'a'
                | b'Z'
                | b'z'
        ) {
            Some(byte)
        } else {
            None
        }
    }

    fn skip_separators(&mut self) {
        while self.offset < self.bytes.len()
            && (self.bytes[self.offset].is_ascii_whitespace() || self.bytes[self.offset] == b',')
        {
            self.offset += 1;
        }
    }

    fn number(&mut self) -> Result<f32, SvgError> {
        self.skip_separators();
        let start = self.offset;
        if self.offset < self.bytes.len() && matches!(self.bytes[self.offset], b'+' | b'-') {
            self.offset += 1;
        }
        let mut digits = false;
        while self.offset < self.bytes.len() && self.bytes[self.offset].is_ascii_digit() {
            self.offset += 1;
            digits = true;
        }
        if self.offset < self.bytes.len() && self.bytes[self.offset] == b'.' {
            self.offset += 1;
            while self.offset < self.bytes.len() && self.bytes[self.offset].is_ascii_digit() {
                self.offset += 1;
                digits = true;
            }
        }
        if !digits {
            return Err(SvgError::MalformedPath);
        }
        if self.offset < self.bytes.len() && matches!(self.bytes[self.offset], b'e' | b'E') {
            let exponent_start = self.offset;
            self.offset += 1;
            if self.offset < self.bytes.len() && matches!(self.bytes[self.offset], b'+' | b'-') {
                self.offset += 1;
            }
            let mut exponent_digits = false;
            while self.offset < self.bytes.len() && self.bytes[self.offset].is_ascii_digit() {
                self.offset += 1;
                exponent_digits = true;
            }
            if !exponent_digits {
                self.offset = exponent_start;
            }
        }

        let text = std::str::from_utf8(&self.bytes[start..self.offset])
            .map_err(|_| SvgError::MalformedPath)?;
        let value: f32 = text.parse().map_err(|_| SvgError::MalformedPath)?;
        if value.is_finite() {
            Ok(value)
        } else {
            Err(SvgError::MalformedPath)
        }
    }

    fn point(&mut self, base: Point) -> Result<Point, SvgError> {
        let x = self.number()? + base.x;
        let y = self.number()? + base.y;
        Ok(Point::new(x, y))
    }

    fn flag(&mut self) -> Result<bool, SvgError> {
        self.skip_separators();
        match self.bytes.get(self.offset) {
            Some(b'0') => {
                self.offset += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.offset += 1;
                Ok(true)
            }
            _ => Err(SvgError::MalformedPath),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Bounds, Contour, FillRule, LineCap, PathSeg, Point, SvgError, Transform, ellipse_segments,
        flatten, parse_path, rect_segments, signed_area, stroke_outline,
    };

    #[test]
    fn transforms_compose_invert_and_report_scale() {
        let translate = Transform::translate(10.0, 20.0);
        let scale = Transform::scale(2.0, 4.0);
        let combined = translate.concat(scale);
        let mapped = combined.apply(Point::new(1.0, 1.0));
        assert!((mapped.x - 12.0).abs() < 1e-4);
        assert!((mapped.y - 24.0).abs() < 1e-4);
        assert!((combined.average_scale() - 8.0_f32.sqrt()).abs() < 1e-4);

        let inverse = combined
            .invert()
            .expect("a scaled translation is invertible");
        let round_trip = inverse.apply(mapped);
        assert!((round_trip.x - 1.0).abs() < 1e-3);
        assert!((round_trip.y - 1.0).abs() < 1e-3);
        assert_eq!(Transform::scale(0.0, 1.0).invert(), None);

        let rotated = Transform::rotate(90.0).apply(Point::new(1.0, 0.0));
        assert!(rotated.x.abs() < 1e-4);
        assert!((rotated.y - 1.0).abs() < 1e-4);
    }

    #[test]
    fn path_grammar_supports_relative_implicit_and_reflected_commands() {
        let segments = parse_path("M 1 2 3 4 l 1 1 H 9 V 8 z").unwrap();
        assert_eq!(
            segments,
            vec![
                PathSeg::MoveTo(Point::new(1.0, 2.0)),
                PathSeg::LineTo(Point::new(3.0, 4.0)),
                PathSeg::LineTo(Point::new(4.0, 5.0)),
                PathSeg::LineTo(Point::new(9.0, 5.0)),
                PathSeg::LineTo(Point::new(9.0, 8.0)),
                PathSeg::Close,
            ]
        );

        let reflected = parse_path("M0 0 C1 1 2 2 3 3 S4 4 5 5").unwrap();
        assert_eq!(
            reflected[2],
            PathSeg::CubicTo(
                Point::new(4.0, 4.0),
                Point::new(4.0, 4.0),
                Point::new(5.0, 5.0)
            )
        );

        let exponent = parse_path("M1e1 2.5e-1L-.5.5").unwrap();
        assert_eq!(exponent[0], PathSeg::MoveTo(Point::new(10.0, 0.25)));
        assert_eq!(exponent[1], PathSeg::LineTo(Point::new(-0.5, 0.5)));

        let arc = parse_path("M0 0A5 5 0 1 0 10 0").unwrap();
        assert_eq!(
            arc[1],
            PathSeg::ArcTo {
                rx: 5.0,
                ry: 5.0,
                rotation: 0.0,
                large_arc: true,
                sweep: false,
                to: Point::new(10.0, 0.0),
            }
        );
    }

    #[test]
    fn malformed_path_data_fails_closed() {
        assert_eq!(parse_path("1 2 3"), Err(SvgError::MalformedPath));
        assert_eq!(parse_path("M0 0 X1 1"), Err(SvgError::MalformedPath));
        assert_eq!(parse_path("M0 0 L1"), Err(SvgError::MalformedPath));
        assert_eq!(
            parse_path("M0 0A5 5 0 2 0 1 1"),
            Err(SvgError::MalformedPath)
        );
    }

    #[test]
    fn shape_helpers_produce_closed_geometry_and_bounds() {
        assert!(rect_segments(0.0, 0.0, 0.0, 5.0, 0.0, 0.0).is_empty());
        let square = rect_segments(1.0, 2.0, 4.0, 6.0, 0.0, 0.0);
        assert_eq!(square.len(), 5);
        let bounds = Bounds::of(&square).expect("a rectangle has bounds");
        assert_eq!((bounds.width(), bounds.height()), (4.0, 6.0));

        let rounded = rect_segments(0.0, 0.0, 10.0, 10.0, 20.0, 20.0);
        assert!(matches!(
            rounded[2],
            PathSeg::ArcTo {
                rx: 5.0,
                ry: 5.0,
                ..
            }
        ));
        assert!(ellipse_segments(1.0, 1.0, 0.0, 2.0).is_empty());
        assert_eq!(ellipse_segments(0.0, 0.0, 2.0, 2.0).len(), 4);
    }

    #[test]
    fn flattening_produces_device_space_contours() {
        let contours = flatten(
            &rect_segments(0.0, 0.0, 2.0, 2.0, 0.0, 0.0),
            Transform::scale(10.0, 10.0),
        );
        assert_eq!(contours.len(), 1);
        assert!(contours[0].closed);
        assert_eq!(contours[0].points[0], Point::new(0.0, 0.0));
        assert_eq!(contours[0].points[1], Point::new(20.0, 0.0));

        let curved = flatten(
            &parse_path("M0 0 C0 10 10 10 10 0").unwrap(),
            Transform::IDENTITY,
        );
        assert_eq!(curved.len(), 1);
        assert!(curved[0].points.len() > 2);
        assert!(!curved[0].closed);

        // A non-invertible transform still yields finite geometry rather than NaN contours.
        let degenerate = flatten(
            &rect_segments(0.0, 0.0, 2.0, 2.0, 0.0, 0.0),
            Transform::scale(0.0, 0.0),
        );
        assert!(
            degenerate
                .iter()
                .all(|contour| contour.points.iter().all(|point| point.x.is_finite()))
        );
    }

    #[test]
    fn stroke_pieces_share_one_winding_direction() {
        let contours = flatten(
            &parse_path("M0 0 L10 0 L10 10").unwrap(),
            Transform::IDENTITY,
        );
        let outline = stroke_outline(&contours, 4.0, LineCap::Round, &[], 0.0);
        assert!(outline.len() >= 3);
        for piece in &outline {
            assert!(piece.closed);
            assert!(
                signed_area(&piece.points) <= 0.0,
                "every stroke piece must be wound so the non-zero rule unions them"
            );
        }

        assert!(stroke_outline(&contours, 0.0, LineCap::Butt, &[], 0.0).is_empty());
    }

    #[test]
    fn dashes_split_a_polyline_into_bounded_runs() {
        let contours = vec![Contour {
            points: vec![Point::new(0.0, 0.0), Point::new(10.0, 0.0)],
            closed: false,
        }];
        let solid = stroke_outline(&contours, 2.0, LineCap::Butt, &[], 0.0);
        let dashed = stroke_outline(&contours, 2.0, LineCap::Butt, &[2.0, 2.0], 0.0);
        assert!(dashed.len() > solid.len());

        // A degenerate pattern must not produce an empty or unbounded stroke.
        let zero = stroke_outline(&contours, 2.0, LineCap::Butt, &[0.0], 0.0);
        assert!(!zero.is_empty());
    }

    #[test]
    fn fill_rules_and_caps_are_plain_data() {
        assert_ne!(FillRule::NonZero, FillRule::EvenOdd);
        assert_ne!(LineCap::Butt, LineCap::Square);
    }
}
