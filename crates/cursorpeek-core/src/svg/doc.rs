//! Element tree, stylesheet, and animation extraction for a previewed SVG document.

use std::collections::HashMap;

use super::SvgError;
use super::xml::{XmlEvent, XmlReader, split_qualified_name};

const MAX_STYLE_RULES: usize = 512;
const MAX_ANIMATIONS: usize = 512;
const MAX_DECLARATIONS: usize = 32;
const MAX_STYLESHEET_BYTES: usize = 64 * 1024;
const MAX_CLOCK_MS: u32 = 600_000;

/// Element names that could reach a script engine, network, or embedded decoder.
const REFUSED_TAGS: &[&str] = &[
    "script",
    "foreignobject",
    "image",
    "iframe",
    "audio",
    "video",
    "handler",
    "listener",
];

#[derive(Clone, Debug, Default)]
pub(super) struct Element {
    pub(super) tag: String,
    pub(super) id: String,
    pub(super) classes: Vec<String>,
    pub(super) attributes: Vec<(String, String)>,
    pub(super) children: Vec<usize>,
    pub(super) parent: Option<usize>,
}

impl Element {
    pub(super) fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Selector {
    Type(String),
    Class(String),
    Id(String),
    Universal,
}

#[derive(Clone, Debug)]
pub(super) struct StyleRule {
    pub(super) selector: Selector,
    pub(super) declarations: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TransformKind {
    Translate,
    Scale,
    Rotate,
    SkewX,
    SkewY,
}

#[derive(Clone, Debug)]
pub(super) struct Animation {
    pub(super) target: usize,
    pub(super) attribute: String,
    pub(super) transform: Option<TransformKind>,
    pub(super) values: Vec<String>,
    pub(super) key_times: Vec<f32>,
    pub(super) discrete: bool,
    pub(super) begin_ms: u32,
    pub(super) duration_ms: u32,
    pub(super) repeat_forever: bool,
    pub(super) repeat_count: f32,
    pub(super) freeze: bool,
    pub(super) additive: bool,
}

impl Animation {
    /// Active length in milliseconds, or `None` when the animation never ends.
    pub(super) fn active_end_ms(&self) -> Option<u32> {
        if self.repeat_forever {
            return None;
        }
        let repeats = if self.repeat_count > 0.0 {
            self.repeat_count
        } else {
            1.0
        };
        let active = (self.duration_ms as f32 * repeats).min(MAX_CLOCK_MS as f32);
        Some(self.begin_ms.saturating_add(active.ceil() as u32))
    }
}

#[derive(Debug)]
pub(super) struct Document {
    pub(super) elements: Vec<Element>,
    pub(super) root: usize,
    pub(super) ids: HashMap<String, usize>,
    pub(super) rules: Vec<StyleRule>,
    pub(super) animations: Vec<Animation>,
}

impl Document {
    pub(super) fn element(&self, index: usize) -> &Element {
        &self.elements[index]
    }

    pub(super) fn by_reference(&self, reference: &str) -> Option<usize> {
        self.ids.get(reference.trim().strip_prefix('#')?).copied()
    }
}

pub(super) fn parse_document(source: &str) -> Result<Document, SvgError> {
    let mut reader = XmlReader::new(source);
    let mut elements: Vec<Element> = Vec::new();
    let mut ids: HashMap<String, usize> = HashMap::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut root: Option<usize> = None;
    let mut style_text = String::new();

    while let Some(event) = reader.next_event()? {
        match event {
            XmlEvent::Start {
                name,
                attributes,
                self_closing,
            } => {
                let tag = split_qualified_name(&name).1.to_ascii_lowercase();
                if REFUSED_TAGS.contains(&tag.as_str()) {
                    return Err(SvgError::ActiveContent);
                }
                if root.is_none() && tag != "svg" {
                    return Err(SvgError::NotSvg);
                }

                let mut element = Element {
                    tag,
                    ..Element::default()
                };
                for attribute in attributes {
                    let name = attribute.name.to_ascii_lowercase();
                    if name.starts_with("on") && name.len() > 2 {
                        return Err(SvgError::ActiveContent);
                    }
                    reject_external_reference(&name, &attribute.value)?;
                    if !attribute.prefix.is_empty() && attribute.prefix != "xlink" {
                        continue;
                    }
                    if name == "id" {
                        element.id = attribute.value.trim().to_owned();
                    } else if name == "class" {
                        element.classes = attribute
                            .value
                            .split_ascii_whitespace()
                            .map(str::to_owned)
                            .collect();
                    }
                    element.attributes.retain(|(key, _)| *key != name);
                    element.attributes.push((name, attribute.value));
                }

                let index = elements.len();
                if !element.id.is_empty() {
                    ids.entry(element.id.clone()).or_insert(index);
                }
                elements.push(element);
                if let Some(parent) = stack.last().copied() {
                    elements[index].parent = Some(parent);
                    elements[parent].children.push(index);
                } else if root.is_none() {
                    root = Some(index);
                }
                if !self_closing {
                    stack.push(index);
                }
            }
            XmlEvent::End => {
                stack.pop();
            }
            // Only stylesheet text is retained; no other character data affects the drawing.
            XmlEvent::Text(text) => {
                let Some(current) = stack.last().copied() else {
                    continue;
                };
                if elements[current].tag == "style" {
                    if style_text.len() + text.len() > MAX_STYLESHEET_BYTES {
                        return Err(SvgError::TooLarge);
                    }
                    style_text.push_str(&text);
                }
            }
        }
    }

    let root = root.ok_or(SvgError::NotSvg)?;
    let rules = parse_stylesheet(&style_text)?;
    let animations = collect_animations(&elements, &ids);
    Ok(Document {
        elements,
        root,
        ids,
        rules,
        animations,
    })
}

fn reject_external_reference(name: &str, value: &str) -> Result<(), SvgError> {
    if name == "href" || name == "src" {
        if !value.trim_start().starts_with('#') {
            return Err(SvgError::ExternalReference);
        }
        return Ok(());
    }
    if value.to_ascii_lowercase().contains("javascript:") {
        return Err(SvgError::ActiveContent);
    }
    reject_external_url(value)
}

/// Every `url(...)` target must be a same-document fragment.
fn reject_external_url(value: &str) -> Result<(), SvgError> {
    let lower = value.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(index) = rest.find("url(") {
        let tail = &rest[index + 4..];
        let end = tail.find(')').ok_or(SvgError::ExternalReference)?;
        let target = tail[..end].trim().trim_matches(['"', '\'']);
        if !target.starts_with('#') {
            return Err(SvgError::ExternalReference);
        }
        rest = &tail[end + 1..];
    }
    Ok(())
}

fn parse_stylesheet(text: &str) -> Result<Vec<StyleRule>, SvgError> {
    let stripped = strip_css_comments(text);
    let mut rules: Vec<StyleRule> = Vec::new();
    let bytes = stripped.as_bytes();
    let mut offset = 0_usize;

    while offset < bytes.len() {
        let prelude_start = offset;
        while offset < bytes.len() && bytes[offset] != b'{' && bytes[offset] != b'}' {
            offset += 1;
        }
        let prelude = stripped[prelude_start..offset].trim();
        if offset >= bytes.len() {
            break;
        }
        if bytes[offset] == b'}' {
            offset += 1;
            continue;
        }

        let (body, next) = read_balanced_block(&stripped, offset);
        offset = next;
        // At-rules such as `@keyframes` and `@media` are skipped rather than partially applied.
        if prelude.starts_with('@') || prelude.is_empty() {
            continue;
        }
        let declarations = parse_declarations(body);
        if declarations.is_empty() {
            continue;
        }
        for part in prelude.split(',') {
            if rules.len() >= MAX_STYLE_RULES {
                return Err(SvgError::TooComplex);
            }
            if let Some(selector) = parse_selector(part.trim()) {
                rules.push(StyleRule {
                    selector,
                    declarations: declarations.clone(),
                });
            }
        }
    }
    Ok(rules)
}

fn read_balanced_block(text: &str, open: usize) -> (&str, usize) {
    let bytes = text.as_bytes();
    let mut depth = 0_usize;
    let mut offset = open;
    let mut body_start = open + 1;
    while offset < bytes.len() {
        match bytes[offset] {
            b'{' => {
                depth += 1;
                if depth == 1 {
                    body_start = offset + 1;
                }
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (&text[body_start..offset], offset + 1);
                }
            }
            _ => {}
        }
        offset += 1;
    }
    (&text[body_start.min(bytes.len())..], bytes.len())
}

fn strip_css_comments(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find("/*") {
        output.push_str(&rest[..index]);
        match rest[index + 2..].find("*/") {
            Some(end) => rest = &rest[index + 2 + end + 2..],
            None => return output,
        }
    }
    output.push_str(rest);
    output
}

fn parse_declarations(body: &str) -> Vec<(String, String)> {
    let mut declarations = Vec::new();
    for entry in body.split(';') {
        if declarations.len() >= MAX_DECLARATIONS {
            break;
        }
        let Some((property, value)) = entry.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let value = value.trim();
        if property.is_empty() || value.is_empty() || reject_external_url(value).is_err() {
            continue;
        }
        declarations.push((property, value.to_owned()));
    }
    declarations
}

fn parse_selector(text: &str) -> Option<Selector> {
    if text == "*" {
        return Some(Selector::Universal);
    }
    if let Some(class) = text.strip_prefix('.') {
        return is_identifier(class).then(|| Selector::Class(class.to_owned()));
    }
    if let Some(id) = text.strip_prefix('#') {
        return is_identifier(id).then(|| Selector::Id(id.to_owned()));
    }
    is_identifier(text).then(|| Selector::Type(text.to_ascii_lowercase()))
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|scalar| scalar.is_ascii_alphanumeric() || scalar == '-' || scalar == '_')
}

/// Applies the stylesheet to one element, lowest precedence first.
pub(super) fn matched_declarations<'a>(
    rules: &'a [StyleRule],
    element: &Element,
) -> Vec<(&'a str, &'a str)> {
    let mut matched: Vec<(&str, &str)> = Vec::new();
    for order in 0..3_u8 {
        for rule in rules {
            let selected = match &rule.selector {
                Selector::Universal => order == 0,
                Selector::Type(tag) => order == 0 && *tag == element.tag,
                Selector::Class(class) => order == 1 && element.classes.iter().any(|c| c == class),
                Selector::Id(id) => order == 2 && *id == element.id,
            };
            if selected {
                for (property, value) in &rule.declarations {
                    matched.push((property.as_str(), value.as_str()));
                }
            }
        }
    }
    matched
}

fn collect_animations(elements: &[Element], ids: &HashMap<String, usize>) -> Vec<Animation> {
    let mut animations: Vec<Animation> = Vec::new();
    for (index, element) in elements.iter().enumerate() {
        let transform = match element.tag.as_str() {
            "animate" | "set" => None,
            "animatetransform" => Some(
                element
                    .attribute("type")
                    .and_then(transform_kind)
                    .unwrap_or(TransformKind::Translate),
            ),
            _ => continue,
        };
        if animations.len() >= MAX_ANIMATIONS {
            break;
        }

        let Some(target) = animation_target(ids, element) else {
            continue;
        };
        let attribute = element
            .attribute("attributename")
            .unwrap_or(if transform.is_some() { "transform" } else { "" })
            .trim()
            .to_ascii_lowercase();
        if attribute.is_empty() {
            continue;
        }
        let Some(values) = animation_values(element) else {
            continue;
        };
        let duration_ms = element
            .attribute("dur")
            .and_then(parse_clock_ms)
            .unwrap_or(0);
        let is_set = element.tag == "set";
        if duration_ms == 0 && !is_set {
            continue;
        }
        let (repeat_forever, repeat_count) = match element.attribute("repeatcount") {
            Some(value) if value.trim().eq_ignore_ascii_case("indefinite") => (true, 0.0),
            Some(value) => (false, value.trim().parse::<f32>().unwrap_or(1.0).max(0.0)),
            None => (false, 1.0),
        };

        animations.push(Animation {
            target,
            attribute,
            transform,
            values,
            key_times: element
                .attribute("keytimes")
                .and_then(super::value::parse_number_list)
                .unwrap_or_default(),
            discrete: is_set
                || element
                    .attribute("calcmode")
                    .is_some_and(|mode| mode.trim().eq_ignore_ascii_case("discrete")),
            begin_ms: element
                .attribute("begin")
                .and_then(parse_clock_ms)
                .unwrap_or(0),
            duration_ms,
            repeat_forever: repeat_forever && !is_set,
            repeat_count,
            freeze: is_set
                || element
                    .attribute("fill")
                    .is_some_and(|fill| fill.trim().eq_ignore_ascii_case("freeze")),
            additive: element
                .attribute("additive")
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("sum")),
        });
    }
    animations
}

fn animation_target(ids: &HashMap<String, usize>, element: &Element) -> Option<usize> {
    match element.attribute("href") {
        Some(reference) => ids.get(reference.trim().strip_prefix('#')?).copied(),
        None => element.parent,
    }
}

fn animation_values(element: &Element) -> Option<Vec<String>> {
    if let Some(values) = element.attribute("values") {
        let list: Vec<String> = values
            .split(';')
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .collect();
        return (!list.is_empty()).then_some(list);
    }
    let to = element.attribute("to").map(str::trim);
    let from = element.attribute("from").map(str::trim);
    match (from, to) {
        (Some(from), Some(to)) => Some(vec![from.to_owned(), to.to_owned()]),
        (None, Some(to)) => Some(vec![to.to_owned()]),
        _ => None,
    }
}

fn transform_kind(text: &str) -> Option<TransformKind> {
    match text.trim().to_ascii_lowercase().as_str() {
        "translate" => Some(TransformKind::Translate),
        "scale" => Some(TransformKind::Scale),
        "rotate" => Some(TransformKind::Rotate),
        "skewx" => Some(TransformKind::SkewX),
        "skewy" => Some(TransformKind::SkewY),
        _ => None,
    }
}

/// Parses an SMIL clock value. Only offsets are supported; event and syncbase begins are ignored.
fn parse_clock_ms(text: &str) -> Option<u32> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("indefinite") {
        return None;
    }
    if trimmed.contains(':') {
        let mut seconds = 0.0_f64;
        for field in trimmed.split(':') {
            let value: f64 = field.trim().parse().ok()?;
            seconds = seconds * 60.0 + value;
        }
        return clamp_clock(seconds * 1000.0);
    }

    let (number, scale) = if let Some(value) = trimmed.strip_suffix("ms") {
        (value, 1.0)
    } else if let Some(value) = trimmed.strip_suffix('s') {
        (value, 1000.0)
    } else if let Some(value) = trimmed.strip_suffix("min") {
        (value, 60_000.0)
    } else if let Some(value) = trimmed.strip_suffix('h') {
        (value, 3_600_000.0)
    } else {
        (trimmed, 1000.0)
    };
    let value: f64 = number.trim().parse().ok()?;
    clamp_clock(value * scale)
}

fn clamp_clock(milliseconds: f64) -> Option<u32> {
    if !milliseconds.is_finite() || milliseconds < 0.0 {
        return None;
    }
    Some(milliseconds.min(f64::from(MAX_CLOCK_MS)).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::{
        Selector, SvgError, TransformKind, matched_declarations, parse_clock_ms, parse_document,
    };

    #[test]
    fn documents_build_a_tree_with_identifiers_and_attributes() {
        let document = parse_document(
            "<svg viewBox='0 0 10 10'><g id='wrap' class='a b'><rect x='1' width='2'/></g></svg>",
        )
        .unwrap();

        assert_eq!(document.element(document.root).tag, "svg");
        assert_eq!(document.element(document.root).children.len(), 1);
        let wrap = document.by_reference("#wrap").expect("the id is indexed");
        assert_eq!(document.element(wrap).classes, vec!["a", "b"]);
        let rect = document.element(wrap).children[0];
        assert_eq!(document.element(rect).attribute("x"), Some("1"));
        assert_eq!(document.element(rect).attribute("y"), None);
        assert_eq!(document.by_reference("#missing"), None);
    }

    #[test]
    fn active_content_and_external_references_are_refused() {
        for source in [
            "<svg><script>alert(1)</script></svg>",
            "<svg><foreignObject/></svg>",
            "<svg><image href='#a'/></svg>",
            "<svg onload='x()'/>",
            "<svg><a href='https://example.test'/></svg>",
            "<svg><rect fill='url(https://example.test/g.svg#g)'/></svg>",
            "<svg><rect fill='url(data:image/png;base64,AAA)'/></svg>",
            "<svg><rect style='fill:url(http://example.test#g)'/></svg>",
            "<svg><a xlink:href='javascript:alert(1)'/></svg>",
        ] {
            assert!(
                matches!(
                    parse_document(source),
                    Err(SvgError::ActiveContent | SvgError::ExternalReference)
                ),
                "{source}"
            );
        }

        // Same-document fragments stay allowed.
        assert!(parse_document("<svg><use href='#a'/><rect fill='url(#g)'/></svg>").is_ok());
        assert_eq!(parse_document("<html/>"), Err(SvgError::NotSvg));
    }

    #[test]
    fn stylesheets_apply_type_class_and_id_rules_in_precedence_order() {
        let document = parse_document(
            "<svg><style>/* c */ rect, .fill { fill: red } .fill { fill: green } \
             #one { fill: blue } @media print { rect { fill: black } } \
             @keyframes spin { from { fill: pink } }</style>\
             <rect id='one' class='fill'/></svg>",
        )
        .unwrap();

        let rect = document.element(document.root).children[1];
        let declarations = matched_declarations(&document.rules, document.element(rect));
        let values: Vec<&str> = declarations
            .iter()
            .filter(|(property, _)| *property == "fill")
            .map(|(_, value)| *value)
            .collect();
        assert_eq!(values, vec!["red", "red", "green", "blue"]);
        assert!(
            document
                .rules
                .iter()
                .all(|rule| rule.selector != Selector::Type("rect".to_owned())
                    || rule.declarations.iter().all(|(_, value)| value != "black")),
            "at-rule blocks must not contribute declarations"
        );
    }

    #[test]
    fn animation_elements_bind_to_their_target_and_timing() {
        let document = parse_document(
            "<svg><rect><animate attributeName='x' from='0' to='10' dur='2s' \
             repeatCount='indefinite'/><animateTransform attributeName='transform' \
             type='rotate' values='0;360' dur='500ms'/></rect>\
             <set href='#late' attributeName='fill' to='red' begin='1s'/>\
             <circle id='late'/></svg>",
        )
        .unwrap();

        assert_eq!(document.animations.len(), 3);
        let slide = &document.animations[0];
        assert_eq!(slide.attribute, "x");
        assert_eq!(slide.values, vec!["0", "10"]);
        assert_eq!(slide.duration_ms, 2_000);
        assert!(slide.repeat_forever);
        assert_eq!(slide.active_end_ms(), None);

        let spin = &document.animations[1];
        assert_eq!(spin.transform, Some(TransformKind::Rotate));
        assert_eq!(spin.duration_ms, 500);
        assert_eq!(spin.active_end_ms(), Some(500));

        let late = &document.animations[2];
        assert_eq!(late.begin_ms, 1_000);
        assert!(late.discrete);
        assert!(late.freeze);
        assert_eq!(
            document.element(late.target).attribute("id"),
            Some("late"),
            "an href animation must retarget away from its parent"
        );
    }

    #[test]
    fn clock_values_cover_the_supported_units() {
        assert_eq!(parse_clock_ms("2"), Some(2_000));
        assert_eq!(parse_clock_ms("2s"), Some(2_000));
        assert_eq!(parse_clock_ms("250ms"), Some(250));
        assert_eq!(parse_clock_ms("1min"), Some(60_000));
        assert_eq!(parse_clock_ms("00:00:01.5"), Some(1_500));
        assert_eq!(parse_clock_ms("indefinite"), None);
        assert_eq!(parse_clock_ms("later"), None);
        assert_eq!(parse_clock_ms("-1s"), None);
        assert_eq!(parse_clock_ms("99h"), Some(600_000));
    }
}
