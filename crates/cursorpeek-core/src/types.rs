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
    use super::{Generation, LegacyEncoding, PhysicalScreenPoint};

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
}
