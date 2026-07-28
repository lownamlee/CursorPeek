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
    use super::{Generation, LegacyEncoding, PhysicalScreenPoint, PhysicalScreenRect};

    #[test]
    fn protocol_value_types_preserve_extreme_values() {
        assert_eq!(Generation::from_raw(u64::MAX).get(), u64::MAX);
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
}
