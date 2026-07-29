//! Style resolution and painting for one animation frame.

use std::borrow::Cow;

use super::SvgError;
use super::animate::AnimationState;
use super::doc::{Document, Element, matched_declarations};
use super::geom::{
    Bounds, Contour, FillRule, LineCap, PathSeg, Point, Transform, ellipse_segments, flatten,
    parse_path, polygon_segments, rect_segments, stroke_outline,
};
use super::raster::{
    Canvas, GradientGeometry, GradientPaint, GradientStop, ShapePaint, SpreadMethod,
};
use super::value::{
    Rgba, parse_color, parse_length, parse_number, parse_number_list, parse_point_list,
    parse_transform,
};

const MAX_WALK_DEPTH: usize = 96;
const MAX_USE_EXPANSIONS: usize = 1_024;
const MAX_GRADIENT_INHERITANCE: usize = 4;
const MAX_GRADIENT_STOPS: usize = 64;

/// Drawing work one frame may perform. The caller divides a whole-document allowance by the frame
/// count so an animated preview cannot cost more than a still one.
#[derive(Clone, Copy, Debug)]
pub(super) struct Budget {
    pub(super) shapes: usize,
    pub(super) contour_points: usize,
    pub(super) raster_work: usize,
}

impl Budget {
    /// Whole-document allowance, chosen so the worst case stays well inside the worker deadline.
    pub(super) const TOTAL: Self = Self {
        shapes: 20_000,
        contour_points: 200_000,
        raster_work: 8_000_000,
    };

    pub(super) const fn divided(self, frames: u32) -> Self {
        let frames = if frames == 0 { 1 } else { frames as usize };
        Self {
            shapes: self.shapes / frames,
            contour_points: self.contour_points / frames,
            raster_work: self.raster_work / frames,
        }
    }
}

#[derive(Clone, Debug)]
enum PaintSource {
    None,
    Color(Rgba),
    Reference(String),
}

#[derive(Clone, Debug)]
struct Style {
    fill: PaintSource,
    stroke: PaintSource,
    stroke_width: f32,
    stroke_linecap: LineCap,
    stroke_dasharray: Vec<f32>,
    stroke_dashoffset: f32,
    fill_rule: FillRule,
    fill_opacity: f32,
    stroke_opacity: f32,
    group_opacity: f32,
    color: Rgba,
    visible: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill: PaintSource::Color(Rgba::BLACK),
            stroke: PaintSource::None,
            stroke_width: 1.0,
            stroke_linecap: LineCap::Butt,
            stroke_dasharray: Vec::new(),
            stroke_dashoffset: 0.0,
            fill_rule: FillRule::NonZero,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            group_opacity: 1.0,
            color: Rgba::BLACK,
            visible: true,
        }
    }
}

/// Root viewport in user units, used to resolve percentage lengths.
#[derive(Clone, Copy, Debug)]
pub(super) struct Viewport {
    pub(super) width: f32,
    pub(super) height: f32,
}

impl Viewport {
    fn diagonal_basis(self) -> f32 {
        ((self.width * self.width + self.height * self.height) / 2.0).sqrt()
    }
}

pub(super) fn render_frame(
    document: &Document,
    state: &AnimationState,
    canvas_width: u32,
    canvas_height: u32,
    root_transform: Transform,
    viewport: Viewport,
    budget: Budget,
) -> Result<Vec<u8>, SvgError> {
    let canvas = Canvas::new(canvas_width, canvas_height).ok_or(SvgError::InvalidSize)?;
    let mut renderer = Renderer {
        document,
        state,
        canvas,
        viewport,
        budget,
        shapes: 0,
        points: 0,
        work: 0,
        expansions: 0,
    };
    renderer.walk(document.root, root_transform, &Style::default(), 0)?;
    Ok(renderer.canvas.into_pixels())
}

struct Renderer<'a> {
    document: &'a Document,
    state: &'a AnimationState,
    canvas: Canvas,
    viewport: Viewport,
    budget: Budget,
    shapes: usize,
    points: usize,
    work: usize,
    expansions: usize,
}

impl<'a> Renderer<'a> {
    fn walk(
        &mut self,
        index: usize,
        parent_transform: Transform,
        inherited: &Style,
        depth: usize,
    ) -> Result<(), SvgError> {
        if depth > MAX_WALK_DEPTH {
            return Err(SvgError::TooComplex);
        }

        let document: &'a Document = self.document;
        let element = document.element(index);
        let properties = collect_properties(document, self.state, index);
        let style = self.resolve_style(inherited, &properties);
        if !style.visible {
            return Ok(());
        }
        let transform = self.element_transform(index, parent_transform, &properties);
        if !transform.is_finite() {
            return Ok(());
        }

        match element.tag.as_str() {
            "svg" => {
                let nested = if depth == 0 {
                    Some(transform)
                } else {
                    self.nested_viewport_transform(&properties, transform)
                };
                match nested {
                    Some(nested) => self.walk_children(index, nested, &style, depth),
                    None => Ok(()),
                }
            }
            "g" | "a" | "switch" => self.walk_children(index, transform, &style, depth),
            "use" => self.walk_use(index, transform, &style, depth, &properties),
            "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path" => {
                self.paint_shape(&element.tag, &properties, transform, &style)
            }
            _ => Ok(()),
        }
    }

    fn walk_children(
        &mut self,
        index: usize,
        transform: Transform,
        style: &Style,
        depth: usize,
    ) -> Result<(), SvgError> {
        let document: &'a Document = self.document;
        let parent = document.element(index);
        let only_first = parent.tag == "switch";
        for child in &parent.children {
            self.walk(*child, transform, style, depth + 1)?;
            if only_first && is_paintable(document.element(*child)) {
                break;
            }
        }
        Ok(())
    }

    fn walk_use(
        &mut self,
        index: usize,
        transform: Transform,
        style: &Style,
        depth: usize,
        properties: &Properties<'a>,
    ) -> Result<(), SvgError> {
        self.expansions += 1;
        if self.expansions > MAX_USE_EXPANSIONS {
            return Err(SvgError::TooComplex);
        }

        let document: &'a Document = self.document;
        let Some(target) = properties
            .get("href")
            .and_then(|reference| document.by_reference(reference))
        else {
            return Ok(());
        };
        if target == index {
            return Ok(());
        }

        let x = self.length(properties, "x", 0.0, self.viewport.width);
        let y = self.length(properties, "y", 0.0, self.viewport.height);
        let placed = transform.concat(Transform::translate(x, y));
        if document.element(target).tag == "symbol" {
            return self.walk_children(target, placed, style, depth + 1);
        }
        self.walk(target, placed, style, depth + 1)
    }

    fn paint_shape(
        &mut self,
        tag: &str,
        properties: &Properties<'a>,
        transform: Transform,
        style: &Style,
    ) -> Result<(), SvgError> {
        self.shapes += 1;
        if self.shapes > self.budget.shapes {
            return Err(SvgError::TooComplex);
        }

        let segments = self.shape_segments(tag, properties)?;
        if segments.is_empty() {
            return Ok(());
        }
        let contours = flatten(&segments, transform);
        if contours.is_empty() {
            return Ok(());
        }
        self.charge_points(&contours)?;

        let bounds = Bounds::of(&segments);
        if let Some(paint) = self.resolve_paint(&style.fill, bounds, transform, style) {
            let work = self.canvas.fill(
                &contours,
                style.fill_rule,
                &paint,
                style.fill_opacity * style.group_opacity,
            );
            self.charge_work(work)?;
        }
        if let Some(paint) = self.resolve_paint(&style.stroke, bounds, transform, style) {
            let scale = transform.average_scale();
            let dashes: Vec<f32> = style
                .stroke_dasharray
                .iter()
                .map(|value| value * scale)
                .collect();
            let outline = stroke_outline(
                &contours,
                style.stroke_width * scale,
                style.stroke_linecap,
                &dashes,
                style.stroke_dashoffset * scale,
            );
            self.charge_points(&outline)?;
            let work = self.canvas.fill(
                &outline,
                FillRule::NonZero,
                &paint,
                style.stroke_opacity * style.group_opacity,
            );
            self.charge_work(work)?;
        }
        Ok(())
    }

    fn charge_points(&mut self, contours: &[Contour]) -> Result<(), SvgError> {
        self.points += contours
            .iter()
            .map(|contour| contour.points.len())
            .sum::<usize>();
        if self.points > self.budget.contour_points {
            return Err(SvgError::TooComplex);
        }
        Ok(())
    }

    fn charge_work(&mut self, work: usize) -> Result<(), SvgError> {
        self.work = self.work.saturating_add(work);
        if self.work > self.budget.raster_work {
            return Err(SvgError::TooComplex);
        }
        Ok(())
    }

    fn shape_segments(
        &self,
        tag: &str,
        properties: &Properties<'a>,
    ) -> Result<Vec<PathSeg>, SvgError> {
        let width_basis = self.viewport.width;
        let height_basis = self.viewport.height;
        let segments = match tag {
            "rect" => {
                let rx = properties
                    .get("rx")
                    .and_then(|value| parse_length(value, width_basis));
                let ry = properties
                    .get("ry")
                    .and_then(|value| parse_length(value, height_basis));
                rect_segments(
                    self.length(properties, "x", 0.0, width_basis),
                    self.length(properties, "y", 0.0, height_basis),
                    self.length(properties, "width", 0.0, width_basis),
                    self.length(properties, "height", 0.0, height_basis),
                    rx.or(ry).unwrap_or(0.0),
                    ry.or(rx).unwrap_or(0.0),
                )
            }
            "circle" => {
                let radius = self.length(properties, "r", 0.0, self.viewport.diagonal_basis());
                ellipse_segments(
                    self.length(properties, "cx", 0.0, width_basis),
                    self.length(properties, "cy", 0.0, height_basis),
                    radius,
                    radius,
                )
            }
            "ellipse" => ellipse_segments(
                self.length(properties, "cx", 0.0, width_basis),
                self.length(properties, "cy", 0.0, height_basis),
                self.length(properties, "rx", 0.0, width_basis),
                self.length(properties, "ry", 0.0, height_basis),
            ),
            "line" => polygon_segments(
                &[
                    Point::new(
                        self.length(properties, "x1", 0.0, width_basis),
                        self.length(properties, "y1", 0.0, height_basis),
                    ),
                    Point::new(
                        self.length(properties, "x2", 0.0, width_basis),
                        self.length(properties, "y2", 0.0, height_basis),
                    ),
                ],
                false,
            ),
            "polyline" | "polygon" => match properties.get("points").and_then(parse_point_list) {
                Some(points) => polygon_segments(&points, tag == "polygon"),
                None => Vec::new(),
            },
            "path" => match properties.get("d") {
                Some(data) => parse_path(data)?,
                None => Vec::new(),
            },
            _ => Vec::new(),
        };
        Ok(segments)
    }

    fn resolve_style(&self, inherited: &Style, properties: &Properties<'a>) -> Style {
        let mut style = inherited.clone();
        if let Some(color) = properties
            .get("color")
            .and_then(|value| parse_color(value, inherited.color))
        {
            style.color = color;
        }
        if let Some(value) = properties.get("fill") {
            style.fill = paint_source(value, style.color);
        }
        if let Some(value) = properties.get("stroke") {
            style.stroke = paint_source(value, style.color);
        }
        if let Some(width) = properties
            .get("stroke-width")
            .and_then(|value| parse_length(value, self.viewport.diagonal_basis()))
        {
            style.stroke_width = width.max(0.0);
        }
        if let Some(cap) = properties.get("stroke-linecap") {
            style.stroke_linecap = match cap.trim().to_ascii_lowercase().as_str() {
                "round" => LineCap::Round,
                "square" => LineCap::Square,
                _ => LineCap::Butt,
            };
        }
        if let Some(value) = properties.get("stroke-dasharray") {
            style.stroke_dasharray = if value.trim().eq_ignore_ascii_case("none") {
                Vec::new()
            } else {
                parse_number_list(value).unwrap_or_default()
            };
        }
        if let Some(offset) = properties
            .get("stroke-dashoffset")
            .and_then(|value| parse_length(value, self.viewport.diagonal_basis()))
        {
            style.stroke_dashoffset = offset;
        }
        if let Some(rule) = properties.get("fill-rule") {
            style.fill_rule = if rule.trim().eq_ignore_ascii_case("evenodd") {
                FillRule::EvenOdd
            } else {
                FillRule::NonZero
            };
        }
        if let Some(opacity) = properties.get("fill-opacity").and_then(parse_ratio) {
            style.fill_opacity = opacity;
        }
        if let Some(opacity) = properties.get("stroke-opacity").and_then(parse_ratio) {
            style.stroke_opacity = opacity;
        }
        if let Some(opacity) = properties.get("opacity").and_then(parse_ratio) {
            // Group opacity scales descendant alpha instead of compositing the subtree offscreen.
            style.group_opacity = inherited.group_opacity * opacity;
        }
        if properties
            .get("display")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("none"))
            || properties.get("visibility").is_some_and(|value| {
                let value = value.trim();
                value.eq_ignore_ascii_case("hidden") || value.eq_ignore_ascii_case("collapse")
            })
        {
            style.visible = false;
        }
        style
    }

    fn element_transform(
        &self,
        index: usize,
        parent: Transform,
        properties: &Properties<'a>,
    ) -> Transform {
        let mut transform = parent;
        if let Some(local) = properties.get("transform").and_then(parse_transform) {
            transform = transform.concat(local);
        }
        if let Some(animated) = self.state.transform(index) {
            transform = transform.concat(animated);
        }
        transform
    }

    fn nested_viewport_transform(
        &self,
        properties: &Properties<'a>,
        transform: Transform,
    ) -> Option<Transform> {
        let x = self.length(properties, "x", 0.0, self.viewport.width);
        let y = self.length(properties, "y", 0.0, self.viewport.height);
        let width = self.length(properties, "width", self.viewport.width, self.viewport.width);
        let height = self.length(
            properties,
            "height",
            self.viewport.height,
            self.viewport.height,
        );
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let placed = transform.concat(Transform::translate(x, y));
        Some(match properties.get("viewbox").and_then(parse_view_box) {
            Some(view_box) => placed.concat(view_box_transform(
                view_box,
                width,
                height,
                properties.get("preserveaspectratio"),
            )),
            None => placed,
        })
    }

    fn length(&self, properties: &Properties<'a>, name: &str, default: f32, basis: f32) -> f32 {
        properties
            .get(name)
            .and_then(|value| parse_length(value, basis))
            .unwrap_or(default)
    }

    fn resolve_paint(
        &self,
        source: &PaintSource,
        bounds: Option<Bounds>,
        transform: Transform,
        style: &Style,
    ) -> Option<ShapePaint> {
        match source {
            PaintSource::None => None,
            PaintSource::Color(color) => Some(ShapePaint::Solid(*color)),
            // An unresolvable reference keeps the shape visible in the inherited color.
            PaintSource::Reference(reference) => Some(
                self.gradient_paint(reference, bounds, transform)
                    .unwrap_or(ShapePaint::Solid(style.color)),
            ),
        }
    }

    fn gradient_paint(
        &self,
        reference: &str,
        bounds: Option<Bounds>,
        transform: Transform,
    ) -> Option<ShapePaint> {
        let document: &'a Document = self.document;
        let index = document.by_reference(reference)?;
        let linear = match document.element(index).tag.as_str() {
            "lineargradient" => true,
            "radialgradient" => false,
            _ => return None,
        };

        let stops = self.gradient_stops(index, 0)?;
        let object_units = !self
            .gradient_attribute(index, "gradientunits", 0)
            .is_some_and(|units| units.trim().eq_ignore_ascii_case("userSpaceOnUse"));
        let (width_basis, height_basis) = if object_units {
            (1.0, 1.0)
        } else {
            (self.viewport.width, self.viewport.height)
        };
        let radius_basis = if object_units {
            1.0
        } else {
            self.viewport.diagonal_basis()
        };
        let coordinate = |name: &str, default: f32, basis: f32| {
            self.gradient_attribute(index, name, 0)
                .and_then(|value| parse_length(value, basis))
                .unwrap_or(default)
        };

        let geometry = if linear {
            GradientGeometry::Linear {
                x1: coordinate("x1", 0.0, width_basis),
                y1: coordinate("y1", 0.0, height_basis),
                x2: coordinate("x2", width_basis, width_basis),
                y2: coordinate("y2", 0.0, height_basis),
            }
        } else {
            GradientGeometry::Radial {
                cx: coordinate("cx", width_basis / 2.0, width_basis),
                cy: coordinate("cy", height_basis / 2.0, height_basis),
                radius: coordinate("r", radius_basis / 2.0, radius_basis),
            }
        };

        let mut gradient_to_device = transform;
        if object_units {
            let bounds = bounds?;
            if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
                return None;
            }
            gradient_to_device = gradient_to_device
                .concat(Transform::translate(bounds.min_x, bounds.min_y))
                .concat(Transform::scale(bounds.width(), bounds.height()));
        }
        if let Some(local) = self
            .gradient_attribute(index, "gradienttransform", 0)
            .and_then(parse_transform)
        {
            gradient_to_device = gradient_to_device.concat(local);
        }

        let spread = match self.gradient_attribute(index, "spreadmethod", 0) {
            Some(value) if value.trim().eq_ignore_ascii_case("reflect") => SpreadMethod::Reflect,
            Some(value) if value.trim().eq_ignore_ascii_case("repeat") => SpreadMethod::Repeat,
            _ => SpreadMethod::Pad,
        };
        Some(ShapePaint::Gradient(GradientPaint {
            geometry,
            spread,
            stops,
            device_to_gradient: gradient_to_device.invert()?,
        }))
    }

    /// Reads a gradient attribute, following `href` inheritance when it is absent.
    fn gradient_attribute(&self, index: usize, name: &str, depth: usize) -> Option<&'a str> {
        if depth > MAX_GRADIENT_INHERITANCE {
            return None;
        }
        let document: &'a Document = self.document;
        let element = document.element(index);
        if let Some(value) = element.attribute(name) {
            return Some(value);
        }
        let parent = document.by_reference(element.attribute("href")?)?;
        if parent == index {
            return None;
        }
        self.gradient_attribute(parent, name, depth + 1)
    }

    fn gradient_stops(&self, index: usize, depth: usize) -> Option<Vec<GradientStop>> {
        if depth > MAX_GRADIENT_INHERITANCE {
            return None;
        }
        let document: &'a Document = self.document;
        let element = document.element(index);
        let mut stops: Vec<GradientStop> = Vec::new();
        let mut previous = 0.0_f32;
        for child in &element.children {
            let stop = document.element(*child);
            if stop.tag != "stop" || stops.len() >= MAX_GRADIENT_STOPS {
                continue;
            }
            let properties = collect_properties(document, self.state, *child);
            let offset = properties
                .get("offset")
                .and_then(|value| parse_length(value, 1.0))
                .unwrap_or(0.0)
                .clamp(0.0, 1.0)
                .max(previous);
            previous = offset;
            let color = properties
                .get("stop-color")
                .and_then(|value| parse_color(value, Rgba::BLACK))
                .unwrap_or(Rgba::BLACK);
            let opacity = properties
                .get("stop-opacity")
                .and_then(parse_ratio)
                .unwrap_or(1.0);
            stops.push(GradientStop {
                offset,
                color: color.with_alpha(opacity),
            });
        }
        if !stops.is_empty() {
            return Some(stops);
        }
        let parent = document.by_reference(element.attribute("href")?)?;
        if parent == index {
            return None;
        }
        self.gradient_stops(parent, depth + 1)
    }
}

/// One element's declarations, ordered lowest to highest precedence.
struct Properties<'a> {
    entries: Vec<(Cow<'a, str>, &'a str)>,
}

impl<'a> Properties<'a> {
    fn get(&self, name: &str) -> Option<&'a str> {
        self.entries
            .iter()
            .rev()
            .find(|(key, _)| key.as_ref() == name)
            .map(|(_, value)| *value)
    }
}

fn collect_properties<'a>(
    document: &'a Document,
    state: &'a AnimationState,
    index: usize,
) -> Properties<'a> {
    let element = document.element(index);
    let mut entries: Vec<(Cow<'a, str>, &'a str)> = Vec::new();
    for (name, value) in &element.attributes {
        entries.push((Cow::Borrowed(name.as_str()), value.as_str()));
    }
    for (property, value) in matched_declarations(&document.rules, element) {
        entries.push((Cow::Borrowed(property), value));
    }
    if let Some(inline) = element.attribute("style") {
        for entry in inline.split(';') {
            let Some((property, value)) = entry.split_once(':') else {
                continue;
            };
            let property = property.trim();
            let value = value.trim();
            if property.is_empty() || value.is_empty() {
                continue;
            }
            let property = if property.bytes().any(|byte| byte.is_ascii_uppercase()) {
                Cow::Owned(property.to_ascii_lowercase())
            } else {
                Cow::Borrowed(property)
            };
            entries.push((property, value));
        }
    }
    for (property, value) in state.animated(index) {
        entries.push((Cow::Borrowed(property.as_str()), value.as_str()));
    }
    Properties { entries }
}

fn is_paintable(element: &Element) -> bool {
    matches!(
        element.tag.as_str(),
        "svg"
            | "g"
            | "a"
            | "use"
            | "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "path"
    )
}

fn paint_source(value: &str, current: Rgba) -> PaintSource {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("none") {
        return PaintSource::None;
    }
    if trimmed.starts_with("url(") {
        let reference = trimmed
            .trim_start_matches("url(")
            .trim_end_matches(')')
            .trim()
            .trim_matches(['"', '\''])
            .to_owned();
        return PaintSource::Reference(reference);
    }
    match parse_color(trimmed, current) {
        Some(color) => PaintSource::Color(color),
        None => PaintSource::None,
    }
}

fn parse_ratio(value: &str) -> Option<f32> {
    let trimmed = value.trim();
    let ratio = match trimmed.strip_suffix('%') {
        Some(percent) => parse_number(percent)? / 100.0,
        None => parse_number(trimmed)?,
    };
    Some(ratio.clamp(0.0, 1.0))
}

pub(super) fn parse_view_box(value: &str) -> Option<(f32, f32, f32, f32)> {
    let values = parse_number_list(value)?;
    if values.len() != 4 || values[2] <= 0.0 || values[3] <= 0.0 {
        return None;
    }
    Some((values[0], values[1], values[2], values[3]))
}

/// Maps a `viewBox` onto a viewport, honouring `preserveAspectRatio`.
pub(super) fn view_box_transform(
    view_box: (f32, f32, f32, f32),
    width: f32,
    height: f32,
    preserve: Option<&str>,
) -> Transform {
    let (min_x, min_y, box_width, box_height) = view_box;
    let lower = preserve.unwrap_or("xMidYMid meet").trim().to_ascii_lowercase();
    let mut fields = lower.split_ascii_whitespace();
    let mut align = fields.next().unwrap_or("xmidymid");
    if align == "defer" {
        align = fields.next().unwrap_or("xmidymid");
    }
    let slice = fields.next().is_some_and(|value| value == "slice");

    let scale_x = width / box_width;
    let scale_y = height / box_height;
    let (scale_x, scale_y) = if align == "none" {
        (scale_x, scale_y)
    } else if slice {
        let scale = scale_x.max(scale_y);
        (scale, scale)
    } else {
        let scale = scale_x.min(scale_y);
        (scale, scale)
    };

    let extra_x = width - box_width * scale_x;
    let extra_y = height - box_height * scale_y;
    let translate_x = match align {
        "xminymin" | "xminymid" | "xminymax" | "none" => 0.0,
        "xmaxymin" | "xmaxymid" | "xmaxymax" => extra_x,
        _ => extra_x / 2.0,
    };
    let translate_y = match align {
        "xminymin" | "xmidymin" | "xmaxymin" | "none" => 0.0,
        "xminymax" | "xmidymax" | "xmaxymax" => extra_y,
        _ => extra_y / 2.0,
    };

    Transform::translate(translate_x, translate_y)
        .concat(Transform::scale(scale_x, scale_y))
        .concat(Transform::translate(-min_x, -min_y))
}

#[cfg(test)]
mod tests {
    use super::{
        Budget, PaintSource, Rgba, SvgError, Viewport, paint_source, parse_ratio, parse_view_box,
        render_frame, view_box_transform,
    };
    use crate::svg::animate::AnimationState;
    use crate::svg::doc::parse_document;
    use crate::svg::geom::{Point, Transform};

    fn frame(source: &str, size: u32) -> Vec<u8> {
        let document = parse_document(source).unwrap();
        let state = AnimationState::at(&document, 0);
        render_frame(
            &document,
            &state,
            size,
            size,
            Transform::scale(size as f32 / 10.0, size as f32 / 10.0),
            Viewport {
                width: 10.0,
                height: 10.0,
            },
            Budget::TOTAL,
        )
        .unwrap()
    }

    fn alpha(pixels: &[u8], size: u32, x: u32, y: u32) -> u8 {
        pixels[((y as usize * size as usize) + x as usize) * 4 + 3]
    }

    fn red(pixels: &[u8], size: u32, x: u32, y: u32) -> u8 {
        pixels[((y as usize * size as usize) + x as usize) * 4 + 2]
    }

    #[test]
    fn shapes_paint_with_their_default_and_declared_fills() {
        let pixels = frame(
            "<svg viewBox='0 0 10 10'><rect width='5' height='5'/></svg>",
            20,
        );
        assert_eq!(alpha(&pixels, 20, 2, 2), 255);
        assert_eq!(red(&pixels, 20, 2, 2), 0);
        assert_eq!(alpha(&pixels, 20, 18, 18), 0);

        let colored = frame(
            "<svg viewBox='0 0 10 10'><circle cx='5' cy='5' r='4' fill='red'/></svg>",
            20,
        );
        assert_eq!(red(&colored, 20, 10, 10), 255);
        assert_eq!(alpha(&colored, 20, 0, 0), 0);

        let hidden = frame(
            "<svg viewBox='0 0 10 10'><rect width='10' height='10' fill='none'/>\
             <rect width='10' height='10' display='none' fill='red'/></svg>",
            8,
        );
        assert!(hidden.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn presentation_attributes_css_inline_style_and_animation_cascade() {
        let document = parse_document(
            "<svg viewBox='0 0 10 10'><style>rect { fill: blue }</style>\
             <rect width='10' height='10' fill='red' style='fill:lime'>\
             <animate attributeName='fill' values='white;white' dur='1s'/></rect></svg>",
        )
        .unwrap();
        let pixels = render_frame(
            &document,
            &AnimationState::at(&document, 500),
            4,
            4,
            Transform::scale(0.4, 0.4),
            Viewport {
                width: 10.0,
                height: 10.0,
            },
            Budget::TOTAL,
        )
        .unwrap();
        assert_eq!(&pixels[..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn groups_transforms_and_use_references_compose() {
        let pixels = frame(
            "<svg viewBox='0 0 10 10'><defs><rect id='unit' width='2' height='2'/></defs>\
             <g transform='translate(4 4)'><use href='#unit'/></g></svg>",
            10,
        );
        assert_eq!(alpha(&pixels, 10, 5, 5), 255);
        assert_eq!(alpha(&pixels, 10, 1, 1), 0);

        let cyclic = frame(
            "<svg viewBox='0 0 10 10'><use id='loop' href='#loop'/></svg>",
            4,
        );
        assert!(cyclic.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn strokes_scale_with_the_viewport_transform() {
        let pixels = frame(
            "<svg viewBox='0 0 10 10'><line x1='0' y1='5' x2='10' y2='5' stroke='red' \
             stroke-width='2'/></svg>",
            20,
        );
        assert_eq!(alpha(&pixels, 20, 10, 10), 255);
        assert_eq!(alpha(&pixels, 20, 10, 1), 0);
    }

    #[test]
    fn gradients_resolve_object_bounding_box_units() {
        let pixels = frame(
            "<svg viewBox='0 0 10 10'><defs><linearGradient id='g'>\
             <stop offset='0' stop-color='black'/><stop offset='1' stop-color='white'/>\
             </linearGradient></defs><rect width='10' height='10' fill='url(#g)'/></svg>",
            16,
        );
        assert!(red(&pixels, 16, 15, 8) > red(&pixels, 16, 0, 8));

        let missing = frame(
            "<svg viewBox='0 0 10 10'><rect width='10' height='10' fill='url(#absent)'/></svg>",
            8,
        );
        assert_eq!(alpha(&missing, 8, 4, 4), 255);
    }

    #[test]
    fn view_box_alignment_covers_meet_slice_and_none() {
        let meet = view_box_transform((0.0, 0.0, 10.0, 5.0), 100.0, 100.0, None);
        assert!((meet.a - 10.0).abs() < 1e-4);
        assert!((meet.f - 25.0).abs() < 1e-4);

        let slice =
            view_box_transform((0.0, 0.0, 10.0, 5.0), 100.0, 100.0, Some("xMinYMin slice"));
        assert!((slice.a - 20.0).abs() < 1e-4);
        assert!(slice.f.abs() < 1e-4);

        let stretched = view_box_transform((0.0, 0.0, 10.0, 5.0), 100.0, 100.0, Some("none"));
        assert!((stretched.a - 10.0).abs() < 1e-4);
        assert!((stretched.d - 20.0).abs() < 1e-4);

        let offset = view_box_transform((2.0, 4.0, 10.0, 10.0), 10.0, 10.0, None);
        let mapped = offset.apply(Point::new(2.0, 4.0));
        assert!(mapped.x.abs() < 1e-4);
        assert!(mapped.y.abs() < 1e-4);

        assert_eq!(parse_view_box("0 0 10 10"), Some((0.0, 0.0, 10.0, 10.0)));
        assert_eq!(parse_view_box("0 0 0 10"), None);
        assert_eq!(parse_view_box("0 0 10"), None);
    }

    #[test]
    fn an_exhausted_budget_fails_closed_and_animation_divides_it() {
        let document = parse_document(
            "<svg viewBox='0 0 10 10'><rect width='10' height='10'/>\
             <rect width='9' height='9'/></svg>",
        )
        .unwrap();
        let render_with = |budget| {
            render_frame(
                &document,
                &AnimationState::at(&document, 0),
                8,
                8,
                Transform::scale(0.8, 0.8),
                Viewport {
                    width: 10.0,
                    height: 10.0,
                },
                budget,
            )
        };

        assert!(render_with(Budget::TOTAL).is_ok());
        assert_eq!(
            render_with(Budget {
                shapes: 1,
                ..Budget::TOTAL
            }),
            Err(SvgError::TooComplex)
        );
        assert_eq!(
            render_with(Budget {
                contour_points: 1,
                ..Budget::TOTAL
            }),
            Err(SvgError::TooComplex)
        );
        assert_eq!(
            render_with(Budget {
                raster_work: 1,
                ..Budget::TOTAL
            }),
            Err(SvgError::TooComplex)
        );

        let divided = Budget::TOTAL.divided(4);
        assert_eq!(divided.shapes, Budget::TOTAL.shapes / 4);
        assert_eq!(divided.raster_work, Budget::TOTAL.raster_work / 4);
        assert_eq!(Budget::TOTAL.divided(0).shapes, Budget::TOTAL.shapes);
    }

    #[test]
    fn paint_and_ratio_parsing_fail_closed() {
        assert!(matches!(
            paint_source("none", Rgba::BLACK),
            PaintSource::None
        ));
        assert!(matches!(
            paint_source("url(#g)", Rgba::BLACK),
            PaintSource::Reference(_)
        ));
        assert!(matches!(
            paint_source("#123456", Rgba::BLACK),
            PaintSource::Color(_)
        ));
        assert!(matches!(
            paint_source("not-a-color", Rgba::BLACK),
            PaintSource::None
        ));

        assert_eq!(parse_ratio("0.5"), Some(0.5));
        assert_eq!(parse_ratio("50%"), Some(0.5));
        assert_eq!(parse_ratio("2"), Some(1.0));
        assert_eq!(parse_ratio("-1"), Some(0.0));
        assert_eq!(parse_ratio("half"), None);
    }
}
