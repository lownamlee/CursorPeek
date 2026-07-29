//! Attribute value parsing: numbers, lengths, colors, and transform lists.

use super::geom::{Point, Transform};

const PX_PER_INCH: f32 = 96.0;
const NOMINAL_FONT_SIZE: f32 = 16.0;
const MAX_TRANSFORM_FUNCTIONS: usize = 32;
const MAX_NUMBER_LIST_LEN: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rgba {
    pub(super) red: u8,
    pub(super) green: u8,
    pub(super) blue: u8,
    pub(super) alpha: f32,
}

impl Rgba {
    pub(super) const BLACK: Self = Self::opaque(0, 0, 0);

    pub(super) const fn opaque(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: 1.0,
        }
    }

    pub(super) fn with_alpha(self, alpha: f32) -> Self {
        Self {
            alpha: (self.alpha * alpha).clamp(0.0, 1.0),
            ..self
        }
    }

    pub(super) fn lerp(self, other: Self, t: f32) -> Self {
        let mix = |from: u8, to: u8| {
            let value = f32::from(from) + (f32::from(to) - f32::from(from)) * t;
            value.clamp(0.0, 255.0).round() as u8
        };
        Self {
            red: mix(self.red, other.red),
            green: mix(self.green, other.green),
            blue: mix(self.blue, other.blue),
            alpha: (self.alpha + (other.alpha - self.alpha) * t).clamp(0.0, 1.0),
        }
    }
}

pub(super) fn parse_number(text: &str) -> Option<f32> {
    let value: f32 = text.trim().parse().ok()?;
    value.is_finite().then_some(value)
}

/// Parses a coordinate or size. Percentages resolve against `basis`; `em`/`ex` use nominal sizes.
///
/// Unit conversion keeps its numerator and denominator separate so exact ratios such as `72pt`
/// stay exact.
pub(super) fn parse_length(text: &str, basis: f32) -> Option<f32> {
    let trimmed = text.trim();
    let (number, multiplier, divisor) = if let Some(stripped) = trimmed.strip_suffix('%') {
        (stripped, basis, 100.0)
    } else if let Some(stripped) = strip_unit_suffix(trimmed, "px") {
        (stripped, 1.0, 1.0)
    } else if let Some(stripped) = strip_unit_suffix(trimmed, "pt") {
        (stripped, PX_PER_INCH, 72.0)
    } else if let Some(stripped) = strip_unit_suffix(trimmed, "pc") {
        (stripped, PX_PER_INCH, 6.0)
    } else if let Some(stripped) = strip_unit_suffix(trimmed, "mm") {
        (stripped, PX_PER_INCH, 25.4)
    } else if let Some(stripped) = strip_unit_suffix(trimmed, "cm") {
        (stripped, PX_PER_INCH, 2.54)
    } else if let Some(stripped) = strip_unit_suffix(trimmed, "in") {
        (stripped, PX_PER_INCH, 1.0)
    } else if let Some(stripped) = strip_unit_suffix(trimmed, "em") {
        (stripped, NOMINAL_FONT_SIZE, 1.0)
    } else if let Some(stripped) = strip_unit_suffix(trimmed, "ex") {
        (stripped, NOMINAL_FONT_SIZE, 2.0)
    } else {
        (trimmed, 1.0, 1.0)
    };

    let value = parse_number(number)? * multiplier / divisor;
    value.is_finite().then_some(value)
}

fn strip_unit_suffix<'a>(text: &'a str, unit: &str) -> Option<&'a str> {
    if text.len() < unit.len() {
        return None;
    }
    let split = text.len() - unit.len();
    text.get(split..)
        .filter(|suffix| suffix.eq_ignore_ascii_case(unit))
        .and_then(|_| text.get(..split))
}

/// Splits a whitespace or comma separated number list.
pub(super) fn parse_number_list(text: &str) -> Option<Vec<f32>> {
    let mut values = Vec::new();
    for field in text
        .split(|scalar: char| scalar.is_ascii_whitespace() || scalar == ',')
        .filter(|field| !field.is_empty())
    {
        if values.len() >= MAX_NUMBER_LIST_LEN {
            return None;
        }
        values.push(parse_number(field)?);
    }
    Some(values)
}

pub(super) fn parse_point_list(text: &str) -> Option<Vec<Point>> {
    let values = parse_number_list(text)?;
    if values.len() < 4 {
        return None;
    }
    Some(
        values
            .chunks_exact(2)
            .map(|pair| Point::new(pair[0], pair[1]))
            .collect(),
    )
}

/// Parses a color keyword, hex triplet, or `rgb()`/`rgba()` function. `currentColor` resolves to
/// `current`; `none` and unknown values return `None`.
pub(super) fn parse_color(text: &str, current: Rgba) -> Option<Rgba> {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("currentcolor") {
        return Some(current);
    }
    if trimmed.eq_ignore_ascii_case("transparent") {
        return Some(Rgba {
            alpha: 0.0,
            ..Rgba::BLACK
        });
    }
    if let Some(hex) = trimmed.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    if let Some(arguments) = function_arguments(trimmed, "rgb")
        .or_else(|| function_arguments(trimmed, "rgba"))
    {
        return parse_rgb_function(arguments);
    }
    named_color(trimmed)
}

fn parse_hex_color(hex: &str) -> Option<Rgba> {
    let component = |slice: &str| u8::from_str_radix(slice, 16).ok();
    match hex.len() {
        3 | 4 => {
            let bytes = hex.as_bytes();
            let expand = |index: usize| {
                let digit = component(std::str::from_utf8(&bytes[index..index + 1]).ok()?)?;
                Some(digit * 17)
            };
            let alpha = if hex.len() == 4 {
                f32::from(expand(3)?) / 255.0
            } else {
                1.0
            };
            Some(Rgba {
                red: expand(0)?,
                green: expand(1)?,
                blue: expand(2)?,
                alpha,
            })
        }
        6 | 8 => {
            let alpha = if hex.len() == 8 {
                f32::from(component(&hex[6..8])?) / 255.0
            } else {
                1.0
            };
            Some(Rgba {
                red: component(&hex[0..2])?,
                green: component(&hex[2..4])?,
                blue: component(&hex[4..6])?,
                alpha,
            })
        }
        _ => None,
    }
}

fn function_arguments<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let rest = text.strip_suffix(')')?;
    let (head, arguments) = rest.split_once('(')?;
    head.trim().eq_ignore_ascii_case(name).then_some(arguments)
}

fn parse_rgb_function(arguments: &str) -> Option<Rgba> {
    let fields: Vec<&str> = arguments
        .split(|scalar: char| scalar == ',' || scalar.is_ascii_whitespace() || scalar == '/')
        .filter(|field| !field.is_empty())
        .collect();
    if !(3..=4).contains(&fields.len()) {
        return None;
    }

    let channel = |field: &str| -> Option<u8> {
        let value = match field.strip_suffix('%') {
            Some(percent) => parse_number(percent)? * 255.0 / 100.0,
            None => parse_number(field)?,
        };
        Some(value.clamp(0.0, 255.0).round() as u8)
    };
    let alpha = match fields.get(3) {
        Some(field) => match field.strip_suffix('%') {
            Some(percent) => parse_number(percent)? / 100.0,
            None => parse_number(field)?,
        },
        None => 1.0,
    };
    Some(Rgba {
        red: channel(fields[0])?,
        green: channel(fields[1])?,
        blue: channel(fields[2])?,
        alpha: alpha.clamp(0.0, 1.0),
    })
}

/// Parses an SVG `transform` list into a single matrix. Unknown functions reject the whole list.
pub(super) fn parse_transform(text: &str) -> Option<Transform> {
    let mut result = Transform::IDENTITY;
    let mut rest = text.trim();
    let mut functions = 0_usize;

    while !rest.is_empty() {
        functions += 1;
        if functions > MAX_TRANSFORM_FUNCTIONS {
            return None;
        }
        let open = rest.find('(')?;
        let close = rest.find(')')?;
        if close < open {
            return None;
        }
        let name = rest[..open].trim().to_ascii_lowercase();
        let values = parse_number_list(&rest[open + 1..close])?;
        let function = match (name.as_str(), values.len()) {
            ("matrix", 6) => Transform::new(
                values[0], values[1], values[2], values[3], values[4], values[5],
            ),
            ("translate", 1) => Transform::translate(values[0], 0.0),
            ("translate", 2) => Transform::translate(values[0], values[1]),
            ("scale", 1) => Transform::scale(values[0], values[0]),
            ("scale", 2) => Transform::scale(values[0], values[1]),
            ("rotate", 1) => Transform::rotate(values[0]),
            ("rotate", 3) => Transform::translate(values[1], values[2])
                .concat(Transform::rotate(values[0]))
                .concat(Transform::translate(-values[1], -values[2])),
            ("skewx", 1) => Transform::skew_x(values[0]),
            ("skewy", 1) => Transform::skew_y(values[0]),
            _ => return None,
        };
        result = result.concat(function);
        rest = rest[close + 1..].trim_start_matches([' ', '\t', '\r', '\n', ',']);
    }

    result.is_finite().then_some(result)
}

fn named_color(name: &str) -> Option<Rgba> {
    let lower = name.to_ascii_lowercase();
    let (red, green, blue) = match lower.as_str() {
        "aliceblue" => (240, 248, 255),
        "antiquewhite" => (250, 235, 215),
        "aqua" => (0, 255, 255),
        "aquamarine" => (127, 255, 212),
        "azure" => (240, 255, 255),
        "beige" => (245, 245, 220),
        "bisque" => (255, 228, 196),
        "black" => (0, 0, 0),
        "blanchedalmond" => (255, 235, 205),
        "blue" => (0, 0, 255),
        "blueviolet" => (138, 43, 226),
        "brown" => (165, 42, 42),
        "burlywood" => (222, 184, 135),
        "cadetblue" => (95, 158, 160),
        "chartreuse" => (127, 255, 0),
        "chocolate" => (210, 105, 30),
        "coral" => (255, 127, 80),
        "cornflowerblue" => (100, 149, 237),
        "cornsilk" => (255, 248, 220),
        "crimson" => (220, 20, 60),
        "cyan" => (0, 255, 255),
        "darkblue" => (0, 0, 139),
        "darkcyan" => (0, 139, 139),
        "darkgoldenrod" => (184, 134, 11),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "darkgreen" => (0, 100, 0),
        "darkkhaki" => (189, 183, 107),
        "darkmagenta" => (139, 0, 139),
        "darkolivegreen" => (85, 107, 47),
        "darkorange" => (255, 140, 0),
        "darkorchid" => (153, 50, 204),
        "darkred" => (139, 0, 0),
        "darksalmon" => (233, 150, 122),
        "darkseagreen" => (143, 188, 143),
        "darkslateblue" => (72, 61, 139),
        "darkslategray" | "darkslategrey" => (47, 79, 79),
        "darkturquoise" => (0, 206, 209),
        "darkviolet" => (148, 0, 211),
        "deeppink" => (255, 20, 147),
        "deepskyblue" => (0, 191, 255),
        "dimgray" | "dimgrey" => (105, 105, 105),
        "dodgerblue" => (30, 144, 255),
        "firebrick" => (178, 34, 34),
        "floralwhite" => (255, 250, 240),
        "forestgreen" => (34, 139, 34),
        "fuchsia" => (255, 0, 255),
        "gainsboro" => (220, 220, 220),
        "ghostwhite" => (248, 248, 255),
        "gold" => (255, 215, 0),
        "goldenrod" => (218, 165, 32),
        "gray" | "grey" => (128, 128, 128),
        "green" => (0, 128, 0),
        "greenyellow" => (173, 255, 47),
        "honeydew" => (240, 255, 240),
        "hotpink" => (255, 105, 180),
        "indianred" => (205, 92, 92),
        "indigo" => (75, 0, 130),
        "ivory" => (255, 255, 240),
        "khaki" => (240, 230, 140),
        "lavender" => (230, 230, 250),
        "lavenderblush" => (255, 240, 245),
        "lawngreen" => (124, 252, 0),
        "lemonchiffon" => (255, 250, 205),
        "lightblue" => (173, 216, 230),
        "lightcoral" => (240, 128, 128),
        "lightcyan" => (224, 255, 255),
        "lightgoldenrodyellow" => (250, 250, 210),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "lightgreen" => (144, 238, 144),
        "lightpink" => (255, 182, 193),
        "lightsalmon" => (255, 160, 122),
        "lightseagreen" => (32, 178, 170),
        "lightskyblue" => (135, 206, 250),
        "lightslategray" | "lightslategrey" => (119, 136, 153),
        "lightsteelblue" => (176, 196, 222),
        "lightyellow" => (255, 255, 224),
        "lime" => (0, 255, 0),
        "limegreen" => (50, 205, 50),
        "linen" => (250, 240, 230),
        "magenta" => (255, 0, 255),
        "maroon" => (128, 0, 0),
        "mediumaquamarine" => (102, 205, 170),
        "mediumblue" => (0, 0, 205),
        "mediumorchid" => (186, 85, 211),
        "mediumpurple" => (147, 112, 219),
        "mediumseagreen" => (60, 179, 113),
        "mediumslateblue" => (123, 104, 238),
        "mediumspringgreen" => (0, 250, 154),
        "mediumturquoise" => (72, 209, 204),
        "mediumvioletred" => (199, 21, 133),
        "midnightblue" => (25, 25, 112),
        "mintcream" => (245, 255, 250),
        "mistyrose" => (255, 228, 225),
        "moccasin" => (255, 228, 181),
        "navajowhite" => (255, 222, 173),
        "navy" => (0, 0, 128),
        "oldlace" => (253, 245, 230),
        "olive" => (128, 128, 0),
        "olivedrab" => (107, 142, 35),
        "orange" => (255, 165, 0),
        "orangered" => (255, 69, 0),
        "orchid" => (218, 112, 214),
        "palegoldenrod" => (238, 232, 170),
        "palegreen" => (152, 251, 152),
        "paleturquoise" => (175, 238, 238),
        "palevioletred" => (219, 112, 147),
        "papayawhip" => (255, 239, 213),
        "peachpuff" => (255, 218, 185),
        "peru" => (205, 133, 63),
        "pink" => (255, 192, 203),
        "plum" => (221, 160, 221),
        "powderblue" => (176, 224, 230),
        "purple" => (128, 0, 128),
        "rebeccapurple" => (102, 51, 153),
        "red" => (255, 0, 0),
        "rosybrown" => (188, 143, 143),
        "royalblue" => (65, 105, 225),
        "saddlebrown" => (139, 69, 19),
        "salmon" => (250, 128, 114),
        "sandybrown" => (244, 164, 96),
        "seagreen" => (46, 139, 87),
        "seashell" => (255, 245, 238),
        "sienna" => (160, 82, 45),
        "silver" => (192, 192, 192),
        "skyblue" => (135, 206, 235),
        "slateblue" => (106, 90, 205),
        "slategray" | "slategrey" => (112, 128, 144),
        "snow" => (255, 250, 250),
        "springgreen" => (0, 255, 127),
        "steelblue" => (70, 130, 180),
        "tan" => (210, 180, 140),
        "teal" => (0, 128, 128),
        "thistle" => (216, 191, 216),
        "tomato" => (255, 99, 71),
        "turquoise" => (64, 224, 208),
        "violet" => (238, 130, 238),
        "wheat" => (245, 222, 179),
        "white" => (255, 255, 255),
        "whitesmoke" => (245, 245, 245),
        "yellow" => (255, 255, 0),
        "yellowgreen" => (154, 205, 50),
        _ => return None,
    };
    Some(Rgba::opaque(red, green, blue))
}

#[cfg(test)]
mod tests {
    use super::{
        Rgba, parse_color, parse_length, parse_number, parse_number_list, parse_point_list,
        parse_transform,
    };
    use crate::svg::geom::{Point, Transform};

    #[test]
    fn numbers_and_lengths_convert_units_and_percentages() {
        assert_eq!(parse_number("-1.5e1"), Some(-15.0));
        assert_eq!(parse_number(" 2 "), Some(2.0));
        assert_eq!(parse_number("2px"), None);
        assert_eq!(parse_number("inf"), None);

        assert_eq!(parse_length("50%", 200.0), Some(100.0));
        assert_eq!(parse_length("12", 0.0), Some(12.0));
        assert_eq!(parse_length("12px", 0.0), Some(12.0));
        assert_eq!(parse_length("1in", 0.0), Some(96.0));
        assert_eq!(parse_length("72pt", 0.0), Some(96.0));
        assert_eq!(parse_length("1em", 0.0), Some(16.0));
        assert_eq!(parse_length("1EX", 0.0), Some(8.0));
        assert_eq!(parse_length("1vw", 0.0), None);
    }

    #[test]
    fn number_and_point_lists_tolerate_mixed_separators() {
        assert_eq!(
            parse_number_list("1, 2\n3\t4"),
            Some(vec![1.0, 2.0, 3.0, 4.0])
        );
        assert_eq!(parse_number_list("1,x"), None);
        assert_eq!(
            parse_point_list("0,0 10,0 10,10"),
            Some(vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(10.0, 10.0)
            ])
        );
        assert_eq!(parse_point_list("0,0"), None);
    }

    #[test]
    fn colors_cover_keywords_hex_and_functions() {
        assert_eq!(parse_color("red", Rgba::BLACK), Some(Rgba::opaque(255, 0, 0)));
        assert_eq!(
            parse_color("REBECCAPURPLE", Rgba::BLACK),
            Some(Rgba::opaque(102, 51, 153))
        );
        assert_eq!(
            parse_color("#0f8", Rgba::BLACK),
            Some(Rgba::opaque(0, 255, 136))
        );
        assert_eq!(
            parse_color("#00ff88", Rgba::BLACK),
            Some(Rgba::opaque(0, 255, 136))
        );
        assert_eq!(
            parse_color("rgb(10, 20, 30)", Rgba::BLACK),
            Some(Rgba::opaque(10, 20, 30))
        );
        assert_eq!(
            parse_color("rgb(100%,0%,0%)", Rgba::BLACK),
            Some(Rgba::opaque(255, 0, 0))
        );
        assert_eq!(
            parse_color("rgba(0,0,0,0.5)", Rgba::BLACK),
            Some(Rgba {
                alpha: 0.5,
                ..Rgba::BLACK
            })
        );
        assert_eq!(
            parse_color("currentColor", Rgba::opaque(1, 2, 3)),
            Some(Rgba::opaque(1, 2, 3))
        );
        assert_eq!(
            parse_color("transparent", Rgba::BLACK),
            Some(Rgba {
                alpha: 0.0,
                ..Rgba::BLACK
            })
        );
        assert_eq!(parse_color("none", Rgba::BLACK), None);
        assert_eq!(parse_color("#12345", Rgba::BLACK), None);
        assert_eq!(parse_color("url(#gradient)", Rgba::BLACK), None);
    }

    #[test]
    fn transform_lists_compose_left_to_right_and_reject_unknown_functions() {
        let combined = parse_transform("translate(10 20) scale(2)").unwrap();
        let mapped = combined.apply(Point::new(1.0, 1.0));
        assert!((mapped.x - 12.0).abs() < 1e-4);
        assert!((mapped.y - 22.0).abs() < 1e-4);

        let rotated = parse_transform("rotate(90 1 1)").unwrap();
        let pivot = rotated.apply(Point::new(1.0, 1.0));
        assert!((pivot.x - 1.0).abs() < 1e-3);
        assert!((pivot.y - 1.0).abs() < 1e-3);

        assert_eq!(parse_transform(""), Some(Transform::IDENTITY));
        assert!(parse_transform("skewX(10)").is_some());
        assert_eq!(parse_transform("translate(1,2,3)"), None);
        assert_eq!(parse_transform("perspective(10)"), None);
        assert_eq!(parse_transform("translate(1"), None);
    }

    #[test]
    fn color_interpolation_and_alpha_composition_stay_bounded() {
        let mixed = Rgba::opaque(0, 0, 0).lerp(Rgba::opaque(255, 100, 50), 0.5);
        assert_eq!((mixed.red, mixed.green, mixed.blue), (128, 50, 25));
        let faded = Rgba::opaque(10, 10, 10).with_alpha(0.5).with_alpha(0.5);
        assert!((faded.alpha - 0.25).abs() < 1e-6);
    }
}
