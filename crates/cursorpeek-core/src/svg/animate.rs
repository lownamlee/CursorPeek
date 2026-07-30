//! SMIL timing and value interpolation.
//!
//! Sampling is a pure function of the document and a millisecond offset, so the same file always
//! produces the same frames.

use std::collections::HashMap;

use super::MIN_VECTOR_FRAME_DELAY_MS;

use super::doc::{Animation, Document, TransformKind};
use super::geom::Transform;
use super::value::{Rgba, parse_color, parse_number, parse_number_list};

pub(super) const MIN_TIMELINE_MS: u32 = 200;
pub(super) const MAX_TIMELINE_MS: u32 = 4_000;

/// Sampling window for one loop of the document's animations.
pub(super) fn timeline_ms(document: &Document) -> Option<u32> {
    if document.animations.is_empty() {
        return None;
    }

    let mut window = 0_u32;
    let mut loops = false;
    for animation in &document.animations {
        let candidate = match animation.active_end_ms() {
            Some(end) => end,
            None => {
                loops = true;
                animation.begin_ms.saturating_add(animation.duration_ms)
            }
        };
        window = window.max(candidate);
    }
    // A timeline that ends adds one frame so a frozen or one-shot end state is actually sampled.
    if !loops {
        window = window.saturating_add(MIN_VECTOR_FRAME_DELAY_MS);
    }
    Some(window.clamp(MIN_TIMELINE_MS, MAX_TIMELINE_MS))
}

#[derive(Debug, Default)]
pub(super) struct AnimationState {
    attributes: HashMap<usize, Vec<(String, String)>>,
    transforms: HashMap<usize, Transform>,
}

impl AnimationState {
    pub(super) fn at(document: &Document, time_ms: u32) -> Self {
        let mut state = Self::default();
        for animation in &document.animations {
            let Some(progress) = sample_progress(animation, time_ms) else {
                continue;
            };
            match animation.transform {
                Some(kind) => {
                    if let Some(transform) = sampled_transform(animation, kind, progress) {
                        let combined = match state.transforms.get(&animation.target) {
                            Some(existing) if animation.additive => existing.concat(transform),
                            _ => transform,
                        };
                        state.transforms.insert(animation.target, combined);
                    }
                }
                None => {
                    let value = sampled_value(animation, progress);
                    let entry = state.attributes.entry(animation.target).or_default();
                    entry.retain(|(name, _)| *name != animation.attribute);
                    entry.push((animation.attribute.clone(), value));
                }
            }
        }
        state
    }

    /// Sampled attribute overrides for one element, highest precedence in the cascade.
    pub(super) fn animated(&self, target: usize) -> &[(String, String)] {
        match self.attributes.get(&target) {
            Some(entries) => entries.as_slice(),
            None => &[],
        }
    }

    pub(super) fn transform(&self, target: usize) -> Option<Transform> {
        self.transforms.get(&target).copied()
    }

    #[cfg(test)]
    fn attribute(&self, target: usize, name: &str) -> Option<&str> {
        self.animated(target)
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.attributes.is_empty() && self.transforms.is_empty()
    }
}

/// Returns the animation's progress in `0.0..=1.0`, or `None` while it is inactive.
fn sample_progress(animation: &Animation, time_ms: u32) -> Option<f32> {
    if time_ms < animation.begin_ms {
        return None;
    }
    let local = time_ms - animation.begin_ms;
    if animation.duration_ms == 0 {
        return Some(1.0);
    }

    if !animation.repeat_forever {
        let repeats = if animation.repeat_count > 0.0 {
            animation.repeat_count
        } else {
            1.0
        };
        let active = (animation.duration_ms as f32 * repeats).max(0.0);
        if local as f32 >= active {
            return animation.freeze.then_some(1.0);
        }
    }
    let cycle = local % animation.duration_ms;
    Some(cycle as f32 / animation.duration_ms as f32)
}

/// Maps progress onto the value list, honouring `keyTimes` and `calcMode="discrete"`.
fn segment(animation: &Animation, progress: f32) -> (usize, usize, f32) {
    let last = animation.values.len().saturating_sub(1);
    if last == 0 {
        return (0, 0, 0.0);
    }
    if animation.discrete {
        let index = ((progress * animation.values.len() as f32).floor() as usize).min(last);
        return (index, index, 0.0);
    }

    let times = &animation.key_times;
    if times.len() == animation.values.len() && times.len() >= 2 {
        for index in 0..last {
            let start = times[index];
            let end = times[index + 1];
            if progress <= end || index + 1 == last {
                let span = end - start;
                let local = if span.abs() <= f32::EPSILON {
                    0.0
                } else {
                    ((progress - start) / span).clamp(0.0, 1.0)
                };
                return (index, index + 1, local);
            }
        }
    }

    let scaled = (progress * last as f32).clamp(0.0, last as f32);
    let index = (scaled.floor() as usize).min(last.saturating_sub(1));
    (index, index + 1, scaled - index as f32)
}

fn sampled_value(animation: &Animation, progress: f32) -> String {
    let (from_index, to_index, local) = segment(animation, progress);
    let from = animation.values[from_index].as_str();
    let to = animation.values[to_index].as_str();
    if from_index == to_index || local <= 0.0 {
        return from.to_owned();
    }
    if local >= 1.0 {
        return to.to_owned();
    }
    interpolate(from, to, local).unwrap_or_else(|| from.to_owned())
}

fn interpolate(from: &str, to: &str, t: f32) -> Option<String> {
    if let (Some(start), Some(end)) = (parse_number(from), parse_number(to)) {
        return Some(format_number(start + (end - start) * t));
    }
    if let (Some(start), Some(end)) = (parse_color(from, Rgba::BLACK), parse_color(to, Rgba::BLACK))
    {
        let mixed = start.lerp(end, t);
        return Some(format!(
            "rgba({},{},{},{})",
            mixed.red,
            mixed.green,
            mixed.blue,
            format_number(mixed.alpha)
        ));
    }
    let (start, end) = (parse_number_list(from)?, parse_number_list(to)?);
    if start.is_empty() || start.len() != end.len() {
        return None;
    }
    let mixed: Vec<String> = start
        .iter()
        .zip(end.iter())
        .map(|(a, b)| format_number(a + (b - a) * t))
        .collect();
    Some(mixed.join(" "))
}

fn sampled_transform(
    animation: &Animation,
    kind: TransformKind,
    progress: f32,
) -> Option<Transform> {
    let (from_index, to_index, local) = segment(animation, progress);
    let from = parse_number_list(&animation.values[from_index])?;
    let to = parse_number_list(&animation.values[to_index])?;
    if from.is_empty() {
        return None;
    }

    let arity = from.len().max(to.len());
    let mut values = Vec::with_capacity(arity);
    for index in 0..arity {
        let start = from.get(index).copied().unwrap_or(0.0);
        let end = to.get(index).copied().unwrap_or(start);
        values.push(start + (end - start) * local.clamp(0.0, 1.0));
    }

    let transform = match kind {
        TransformKind::Translate => {
            Transform::translate(values[0], values.get(1).copied().unwrap_or(0.0))
        }
        TransformKind::Scale => {
            let y = values.get(1).copied().unwrap_or(values[0]);
            Transform::scale(values[0], y)
        }
        TransformKind::Rotate => {
            let rotate = Transform::rotate(values[0]);
            match (values.get(1), values.get(2)) {
                (Some(x), Some(y)) => Transform::translate(*x, *y)
                    .concat(rotate)
                    .concat(Transform::translate(-*x, -*y)),
                _ => rotate,
            }
        }
        TransformKind::SkewX => Transform::skew_x(values[0]),
        TransformKind::SkewY => Transform::skew_y(values[0]),
    };
    transform.is_finite().then_some(transform)
}

fn format_number(value: f32) -> String {
    if !value.is_finite() {
        return "0".to_owned();
    }
    let rounded = (value * 1_000.0).round() / 1_000.0;
    let mut text = format!("{rounded}");
    if text == "-0" {
        text = "0".to_owned();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{
        AnimationState, MAX_TIMELINE_MS, MIN_TIMELINE_MS, format_number, interpolate, timeline_ms,
    };
    use crate::svg::doc::parse_document;
    use crate::svg::geom::Point;

    #[test]
    fn static_documents_have_no_timeline_or_state() {
        let document = parse_document("<svg><rect width='1' height='1'/></svg>").unwrap();
        assert_eq!(timeline_ms(&document), None);
        assert!(AnimationState::at(&document, 0).is_empty());
    }

    #[test]
    fn timeline_covers_one_loop_and_stays_bounded() {
        let short = parse_document(
            "<svg><rect><animate attributeName='x' from='0' to='1' dur='50ms'/></rect></svg>",
        )
        .unwrap();
        assert_eq!(timeline_ms(&short), Some(MIN_TIMELINE_MS));

        let long = parse_document(
            "<svg><rect><animate attributeName='x' from='0' to='1' dur='30s'/></rect></svg>",
        )
        .unwrap();
        assert_eq!(timeline_ms(&long), Some(MAX_TIMELINE_MS));

        let looping = parse_document(
            "<svg><rect><animate attributeName='x' from='0' to='1' dur='1s' \
             repeatCount='indefinite'/></rect></svg>",
        )
        .unwrap();
        assert_eq!(timeline_ms(&looping), Some(1_000));

        // A one-shot timeline gains one frame so its end state is sampled.
        let frozen = parse_document(
            "<svg><rect><set attributeName='fill' to='red' begin='400ms'/></rect></svg>",
        )
        .unwrap();
        assert_eq!(timeline_ms(&frozen), Some(440));
    }

    #[test]
    fn numeric_animation_samples_across_its_active_interval() {
        let document = parse_document(
            "<svg><rect><animate attributeName='x' from='0' to='100' dur='1s' \
             begin='500ms'/></rect></svg>",
        )
        .unwrap();
        let rect = document.element(document.root).children[0];

        assert_eq!(AnimationState::at(&document, 0).attribute(rect, "x"), None);
        assert_eq!(
            AnimationState::at(&document, 500).attribute(rect, "x"),
            Some("0")
        );
        assert_eq!(
            AnimationState::at(&document, 1_000).attribute(rect, "x"),
            Some("50")
        );
        // Without `fill="freeze"` the animation stops contributing after its active end.
        assert_eq!(
            AnimationState::at(&document, 2_000).attribute(rect, "x"),
            None
        );
    }

    #[test]
    fn frozen_repeating_and_discrete_animations_hold_their_endpoints() {
        let document = parse_document(
            "<svg><rect><animate attributeName='x' values='0;10;20' dur='300ms' \
             fill='freeze'/><animate attributeName='fill' values='red;blue' dur='400ms' \
             calcMode='discrete' repeatCount='indefinite'/></rect></svg>",
        )
        .unwrap();
        let rect = document.element(document.root).children[0];

        assert_eq!(
            AnimationState::at(&document, 150).attribute(rect, "x"),
            Some("10")
        );
        assert_eq!(
            AnimationState::at(&document, 5_000).attribute(rect, "x"),
            Some("20")
        );
        assert_eq!(
            AnimationState::at(&document, 100).attribute(rect, "fill"),
            Some("red")
        );
        assert_eq!(
            AnimationState::at(&document, 300).attribute(rect, "fill"),
            Some("blue")
        );
    }

    #[test]
    fn transform_animation_produces_a_matrix_for_its_target() {
        let document = parse_document(
            "<svg><g><animateTransform attributeName='transform' type='translate' \
             from='0 0' to='10 20' dur='1s'/></g></svg>",
        )
        .unwrap();
        let group = document.element(document.root).children[0];

        let state = AnimationState::at(&document, 500);
        let transform = state.transform(group).expect("a transform is sampled");
        let mapped = transform.apply(Point::new(0.0, 0.0));
        assert!((mapped.x - 5.0).abs() < 1e-3);
        assert!((mapped.y - 10.0).abs() < 1e-3);
        assert_eq!(state.attribute(group, "transform"), None);
    }

    #[test]
    fn interpolation_handles_numbers_colors_lists_and_unknown_values() {
        assert_eq!(interpolate("0", "10", 0.25).as_deref(), Some("2.5"));
        assert_eq!(
            interpolate("#000000", "#ffffff", 0.5).as_deref(),
            Some("rgba(128,128,128,1)")
        );
        assert_eq!(interpolate("0 0", "10 20", 0.5).as_deref(), Some("5 10"));
        assert_eq!(interpolate("visible", "hidden", 0.5), None);
        assert_eq!(interpolate("0 0", "1 2 3", 0.5), None);
        assert_eq!(format_number(-0.0004), "0");
    }
}
