use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, X_USER_DEFINED};

const MAX_ENCODING_LABEL_BYTES: usize = 40;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Generation(u64);

impl Generation {
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplorerWindowId(u64);

impl ExplorerWindowId {
    pub const fn try_from_raw(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalScreenPoint {
    pub x: i32,
    pub y: i32,
}

impl PhysicalScreenPoint {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalScreenSpan {
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
}

impl PhysicalScreenSpan {
    pub const fn from_point(point: PhysicalScreenPoint) -> Self {
        Self {
            min_x: point.x,
            min_y: point.y,
            max_x: point.x,
            max_y: point.y,
        }
    }

    pub const fn try_new(min_x: i32, min_y: i32, max_x: i32, max_y: i32) -> Option<Self> {
        if min_x <= max_x && min_y <= max_y {
            Some(Self {
                min_x,
                min_y,
                max_x,
                max_y,
            })
        } else {
            None
        }
    }

    pub fn include(&mut self, point: PhysicalScreenPoint) {
        self.min_x = self.min_x.min(point.x);
        self.min_y = self.min_y.min(point.y);
        self.max_x = self.max_x.max(point.x);
        self.max_y = self.max_y.max(point.y);
    }

    pub const fn min_x(self) -> i32 {
        self.min_x
    }

    pub const fn min_y(self) -> i32 {
        self.min_y
    }

    pub const fn max_x(self) -> i32 {
        self.max_x
    }

    pub const fn max_y(self) -> i32 {
        self.max_y
    }

    pub const fn contains(self, point: PhysicalScreenPoint) -> bool {
        self.min_x <= point.x
            && point.x <= self.max_x
            && self.min_y <= point.y
            && point.y <= self.max_y
    }

    pub const fn fits_within(self, bounds: PhysicalScreenRect) -> bool {
        bounds.left <= self.min_x
            && self.max_x < bounds.right
            && bounds.top <= self.min_y
            && self.max_y < bounds.bottom
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalScreenRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl PhysicalScreenRect {
    pub const fn try_new(left: i32, top: i32, right: i32, bottom: i32) -> Option<Self> {
        if left < right && top < bottom {
            Some(Self {
                left,
                top,
                right,
                bottom,
            })
        } else {
            None
        }
    }

    pub const fn left(self) -> i32 {
        self.left
    }

    pub const fn top(self) -> i32 {
        self.top
    }

    pub const fn right(self) -> i32 {
        self.right
    }

    pub const fn bottom(self) -> i32 {
        self.bottom
    }

    pub const fn contains(self, point: PhysicalScreenPoint) -> bool {
        self.left <= point.x && point.x < self.right && self.top <= point.y && point.y < self.bottom
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyEncoding {
    Auto,
    System,
    Off,
    Label(String),
}

impl LegacyEncoding {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "system" => Some(Self::System),
            "off" => Some(Self::Off),
            _ => supported_legacy_encoding(value)
                .map(|encoding| Self::Label(encoding.name().to_ascii_lowercase())),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Auto => "auto",
            Self::System => "system",
            Self::Off => "off",
            Self::Label(label) => label,
        }
    }
}

fn supported_legacy_encoding(value: &str) -> Option<&'static Encoding> {
    if !is_encoding_label(value) {
        return None;
    }
    let encoding = Encoding::for_label_no_replacement(value.as_bytes())?;
    if encoding == UTF_8
        || encoding == UTF_16LE
        || encoding == UTF_16BE
        || encoding == X_USER_DEFINED
    {
        None
    } else {
        Some(encoding)
    }
}

fn is_encoding_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ENCODING_LABEL_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::{
        ExplorerWindowId, Generation, LegacyEncoding, PhysicalScreenPoint, PhysicalScreenRect,
        PhysicalScreenSpan,
    };

    #[test]
    fn protocol_value_types_preserve_extreme_values() {
        assert_eq!(Generation::from_raw(u64::MAX).get(), u64::MAX);
        assert_eq!(ExplorerWindowId::try_from_raw(0), None);
        assert_eq!(
            ExplorerWindowId::try_from_raw(u64::MAX).map(ExplorerWindowId::get),
            Some(u64::MAX)
        );
        assert_eq!(
            PhysicalScreenPoint::new(i32::MIN, i32::MAX),
            PhysicalScreenPoint {
                x: i32::MIN,
                y: i32::MAX,
            }
        );
    }

    #[test]
    fn legacy_encoding_policy_accepts_only_supported_canonical_labels() {
        assert_eq!(LegacyEncoding::parse("auto"), Some(LegacyEncoding::Auto));
        assert_eq!(
            LegacyEncoding::parse("latin1"),
            Some(LegacyEncoding::Label("windows-1252".to_owned()))
        );
        assert_eq!(LegacyEncoding::parse("utf-8"), None);
        assert_eq!(LegacyEncoding::parse("x-user-defined"), None);
        assert_eq!(LegacyEncoding::parse("not-an-encoding"), None);
    }

    #[test]
    fn physical_rectangles_are_ordered_and_use_half_open_edges() {
        assert_eq!(PhysicalScreenRect::try_new(1, 2, 1, 3), None);
        assert_eq!(PhysicalScreenRect::try_new(1, 2, 3, 2), None);

        let bounds = PhysicalScreenRect::try_new(-10, -20, 30, 40).unwrap();
        assert!(bounds.contains(PhysicalScreenPoint::new(-10, -20)));
        assert!(bounds.contains(PhysicalScreenPoint::new(29, 39)));
        assert!(!bounds.contains(PhysicalScreenPoint::new(30, 39)));
        assert!(!bounds.contains(PhysicalScreenPoint::new(29, 40)));
        assert!(!bounds.contains(PhysicalScreenPoint::new(-11, -20)));
    }

    #[test]
    fn physical_spans_accumulate_inclusive_pointer_extremes() {
        let mut span = PhysicalScreenSpan::from_point(PhysicalScreenPoint::new(20, -10));
        span.include(PhysicalScreenPoint::new(-5, 30));
        span.include(PhysicalScreenPoint::new(8, 4));

        assert_eq!(span, PhysicalScreenSpan::try_new(-5, -10, 20, 30).unwrap());
        assert!(span.contains(PhysicalScreenPoint::new(8, 4)));
        assert!(!span.contains(PhysicalScreenPoint::new(21, 4)));
        assert!(span.fits_within(PhysicalScreenRect::try_new(-5, -10, 21, 31).unwrap()));
        assert!(!span.fits_within(PhysicalScreenRect::try_new(-4, -10, 21, 31).unwrap()));
        assert!(!span.fits_within(PhysicalScreenRect::try_new(-5, -10, 20, 31).unwrap()));
        assert_eq!(PhysicalScreenSpan::try_new(1, 0, 0, 1), None);
    }
}
