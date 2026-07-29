//! Bounded, non-validating XML scanner for vector previews.
//!
//! Only elements, attributes, character data, comments, CDATA, and processing instructions are
//! accepted. Internal subsets, entity declarations, and unknown entity references are rejected, so
//! no external resource or recursive expansion is reachable from a previewed file.

use super::SvgError;

pub(super) const MAX_XML_DEPTH: usize = 64;
pub(super) const MAX_XML_ELEMENTS: usize = 20_000;
pub(super) const MAX_XML_ATTRIBUTES: usize = 96;
pub(super) const MAX_XML_NAME_LEN: usize = 128;
pub(super) const MAX_XML_VALUE_LEN: usize = 64 * 1024;
const MAX_ENTITY_NAME_LEN: usize = 12;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct XmlAttribute {
    pub(super) prefix: String,
    pub(super) name: String,
    pub(super) value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum XmlEvent {
    Start {
        name: String,
        attributes: Vec<XmlAttribute>,
        self_closing: bool,
    },
    End,
    Text(String),
}

pub(super) struct XmlReader<'a> {
    input: &'a str,
    offset: usize,
    elements: usize,
    open: Vec<String>,
}

impl<'a> XmlReader<'a> {
    pub(super) fn new(input: &'a str) -> Self {
        let input = input.strip_prefix('\u{feff}').unwrap_or(input);
        Self {
            input,
            offset: 0,
            elements: 0,
            open: Vec::new(),
        }
    }

    pub(super) fn next_event(&mut self) -> Result<Option<XmlEvent>, SvgError> {
        loop {
            if self.offset >= self.input.len() {
                if self.open.is_empty() {
                    return Ok(None);
                }
                return Err(SvgError::UnclosedElement);
            }
            if self.input.as_bytes()[self.offset] == b'<' {
                if let Some(event) = self.read_markup()? {
                    return Ok(Some(event));
                }
                continue;
            }
            if let Some(text) = self.read_text()? {
                return Ok(Some(XmlEvent::Text(text)));
            }
        }
    }

    fn read_text(&mut self) -> Result<Option<String>, SvgError> {
        let start = self.offset;
        let end = match self.input[start..].find('<') {
            Some(index) => start + index,
            None => self.input.len(),
        };
        self.offset = end;
        let raw = &self.input[start..end];
        if raw.trim().is_empty() {
            return Ok(None);
        }
        if raw.len() > MAX_XML_VALUE_LEN {
            return Err(SvgError::TooLarge);
        }
        Ok(Some(decode_entities(raw)?))
    }

    fn read_markup(&mut self) -> Result<Option<XmlEvent>, SvgError> {
        let rest = &self.input[self.offset..];
        if rest.starts_with("<!--") {
            let end = rest.find("-->").ok_or(SvgError::MalformedMarkup)?;
            self.offset += end + 3;
            return Ok(None);
        }
        if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").ok_or(SvgError::MalformedMarkup)?;
            let text = &rest[9..end];
            self.offset += end + 3;
            if text.len() > MAX_XML_VALUE_LEN {
                return Err(SvgError::TooLarge);
            }
            if text.trim().is_empty() {
                return Ok(None);
            }
            return Ok(Some(XmlEvent::Text(text.to_owned())));
        }
        if rest.starts_with("<?") {
            let end = rest.find("?>").ok_or(SvgError::MalformedMarkup)?;
            self.offset += end + 2;
            return Ok(None);
        }
        if rest.starts_with("<!") {
            let end = rest.find('>').ok_or(SvgError::MalformedMarkup)?;
            // An internal subset or entity declaration can define recursive or external entities.
            let declaration = &rest[..end];
            if declaration.contains('[') || declaration.contains("ENTITY") {
                return Err(SvgError::EntityDeclaration);
            }
            self.offset += end + 1;
            return Ok(None);
        }
        if rest.starts_with("</") {
            return self.read_end_tag().map(Some);
        }
        self.read_start_tag().map(Some)
    }

    fn read_end_tag(&mut self) -> Result<XmlEvent, SvgError> {
        self.offset += 2;
        let name = self.read_name()?;
        self.skip_whitespace();
        if self.peek() != Some(b'>') {
            return Err(SvgError::MalformedMarkup);
        }
        self.offset += 1;
        match self.open.pop() {
            Some(open) if open == name => Ok(XmlEvent::End),
            _ => Err(SvgError::MismatchedElement),
        }
    }

    fn read_start_tag(&mut self) -> Result<XmlEvent, SvgError> {
        self.offset += 1;
        let name = self.read_name()?;
        self.elements += 1;
        if self.elements > MAX_XML_ELEMENTS {
            return Err(SvgError::TooComplex);
        }

        let mut attributes: Vec<XmlAttribute> = Vec::new();
        let self_closing;
        loop {
            let had_whitespace = self.skip_whitespace();
            match self.peek() {
                Some(b'>') => {
                    self.offset += 1;
                    self_closing = false;
                    break;
                }
                Some(b'/') => {
                    self.offset += 1;
                    if self.peek() != Some(b'>') {
                        return Err(SvgError::MalformedMarkup);
                    }
                    self.offset += 1;
                    self_closing = true;
                    break;
                }
                Some(_) => {
                    if !had_whitespace {
                        return Err(SvgError::MalformedMarkup);
                    }
                    if attributes.len() >= MAX_XML_ATTRIBUTES {
                        return Err(SvgError::TooComplex);
                    }
                    attributes.push(self.read_attribute()?);
                }
                None => return Err(SvgError::MalformedMarkup),
            }
        }

        if !self_closing {
            if self.open.len() >= MAX_XML_DEPTH {
                return Err(SvgError::TooComplex);
            }
            self.open.push(name.clone());
        }
        Ok(XmlEvent::Start {
            name,
            attributes,
            self_closing,
        })
    }

    fn read_attribute(&mut self) -> Result<XmlAttribute, SvgError> {
        let qualified = self.read_name()?;
        self.skip_whitespace();
        if self.peek() != Some(b'=') {
            return Err(SvgError::MalformedMarkup);
        }
        self.offset += 1;
        self.skip_whitespace();
        let quote = match self.peek() {
            Some(byte @ (b'"' | b'\'')) => byte,
            _ => return Err(SvgError::MalformedMarkup),
        };
        self.offset += 1;
        let start = self.offset;
        let end = match self.input[start..].find(char::from(quote)) {
            Some(index) => start + index,
            None => return Err(SvgError::MalformedMarkup),
        };
        let raw = &self.input[start..end];
        self.offset = end + 1;
        if raw.len() > MAX_XML_VALUE_LEN {
            return Err(SvgError::TooLarge);
        }

        let (prefix, name) = split_qualified_name(&qualified);
        Ok(XmlAttribute {
            prefix,
            name,
            value: decode_entities(raw)?,
        })
    }

    fn read_name(&mut self) -> Result<String, SvgError> {
        let start = self.offset;
        let bytes = self.input.as_bytes();
        let mut end = start;
        while end < bytes.len() && is_name_byte(bytes[end]) {
            end += 1;
        }
        if end == start || end - start > MAX_XML_NAME_LEN {
            return Err(SvgError::MalformedMarkup);
        }
        self.offset = end;
        Ok(self.input[start..end].to_owned())
    }

    fn skip_whitespace(&mut self) -> bool {
        let bytes = self.input.as_bytes();
        let start = self.offset;
        while self.offset < bytes.len() && bytes[self.offset].is_ascii_whitespace() {
            self.offset += 1;
        }
        self.offset != start
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.offset).copied()
    }
}

/// Splits `xlink:href` into `("xlink", "href")`; unprefixed names keep an empty prefix.
pub(super) fn split_qualified_name(qualified: &str) -> (String, String) {
    match qualified.rsplit_once(':') {
        Some((prefix, local)) => (prefix.to_ascii_lowercase(), local.to_owned()),
        None => (String::new(), qualified.to_owned()),
    }
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':') || byte >= 0x80
}

fn decode_entities(raw: &str) -> Result<String, SvgError> {
    if !raw.contains('&') {
        return Ok(raw.to_owned());
    }

    let mut decoded = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(index) = rest.find('&') {
        decoded.push_str(&rest[..index]);
        let tail = &rest[index + 1..];
        let end = tail.find(';').ok_or(SvgError::UnknownEntity)?;
        if end > MAX_ENTITY_NAME_LEN {
            return Err(SvgError::UnknownEntity);
        }
        decoded.push(decode_entity_name(&tail[..end])?);
        rest = &tail[end + 1..];
    }
    decoded.push_str(rest);
    Ok(decoded)
}

fn decode_entity_name(name: &str) -> Result<char, SvgError> {
    match name {
        "amp" => return Ok('&'),
        "lt" => return Ok('<'),
        "gt" => return Ok('>'),
        "quot" => return Ok('"'),
        "apos" => return Ok('\''),
        _ => {}
    }

    let digits = name.strip_prefix('#').ok_or(SvgError::UnknownEntity)?;
    let scalar = match digits.strip_prefix('x').or_else(|| digits.strip_prefix('X')) {
        Some(hex) => u32::from_str_radix(hex, 16).map_err(|_| SvgError::UnknownEntity)?,
        None => digits.parse::<u32>().map_err(|_| SvgError::UnknownEntity)?,
    };
    char::from_u32(scalar).ok_or(SvgError::UnknownEntity)
}

#[cfg(test)]
mod tests {
    use super::{MAX_XML_ELEMENTS, SvgError, XmlEvent, XmlReader, split_qualified_name};

    fn events(input: &str) -> Result<Vec<XmlEvent>, SvgError> {
        let mut reader = XmlReader::new(input);
        let mut collected = Vec::new();
        while let Some(event) = reader.next_event()? {
            collected.push(event);
        }
        Ok(collected)
    }

    #[test]
    fn elements_attributes_and_character_data_round_trip() {
        let collected = events(
            "\u{feff}<?xml version=\"1.0\"?><!-- note --><svg width='2'>\
             <g fill=\"#fff\"/><![CDATA[raw]]>text</svg>",
        )
        .unwrap();

        assert_eq!(collected.len(), 5);
        let XmlEvent::Start {
            name,
            attributes,
            self_closing,
        } = &collected[0]
        else {
            panic!("the first event should open the root element");
        };
        assert_eq!(name, "svg");
        assert!(!self_closing);
        assert_eq!(attributes.len(), 1);
        assert_eq!(attributes[0].name, "width");
        assert_eq!(attributes[0].value, "2");
        assert_eq!(collected[2], XmlEvent::Text("raw".to_owned()));
        assert_eq!(collected[3], XmlEvent::Text("text".to_owned()));
        assert_eq!(collected[4], XmlEvent::End);
    }

    #[test]
    fn predefined_and_numeric_entities_decode_but_others_fail_closed() {
        let collected = events("<svg title=\"a&amp;b&#65;&#x42;\"/>").unwrap();
        let XmlEvent::Start { attributes, .. } = &collected[0] else {
            panic!("the fixture opens one element");
        };
        assert_eq!(attributes[0].value, "a&bAB");

        assert_eq!(
            events("<svg title=\"&external;\"/>"),
            Err(SvgError::UnknownEntity)
        );
        assert_eq!(events("<svg title=\"&amp\"/>"), Err(SvgError::UnknownEntity));
    }

    #[test]
    fn doctype_internal_subsets_and_entity_declarations_are_refused() {
        assert_eq!(
            events("<!DOCTYPE svg [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><svg/>"),
            Err(SvgError::EntityDeclaration)
        );
        assert_eq!(
            events("<!ENTITY x \"y\"><svg/>"),
            Err(SvgError::EntityDeclaration)
        );
        // An external identifier carries no subset and is simply skipped.
        assert!(
            events("<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"svg11.dtd\"><svg/>").is_ok()
        );
    }

    #[test]
    fn malformed_markup_and_element_budgets_fail_closed() {
        assert_eq!(events("<svg>"), Err(SvgError::UnclosedElement));
        assert_eq!(events("<svg></g>"), Err(SvgError::MismatchedElement));
        assert_eq!(events("<svg width=2/>"), Err(SvgError::MalformedMarkup));
        assert_eq!(events("<svg width/>"), Err(SvgError::MalformedMarkup));
        assert_eq!(events("<svg"), Err(SvgError::MalformedMarkup));
        assert_eq!(events("<!-- unterminated"), Err(SvgError::MalformedMarkup));
        assert_eq!(events("<svg><g fill='a'x='b'/></svg>"), Err(SvgError::MalformedMarkup));

        let mut oversized = String::from("<svg>");
        for _ in 0..MAX_XML_ELEMENTS {
            oversized.push_str("<g/>");
        }
        oversized.push_str("</svg>");
        assert_eq!(events(&oversized), Err(SvgError::TooComplex));
    }

    #[test]
    fn qualified_names_split_into_lowercase_prefix_and_local_name() {
        assert_eq!(
            split_qualified_name("XLINK:href"),
            ("xlink".to_owned(), "href".to_owned())
        );
        assert_eq!(
            split_qualified_name("href"),
            (String::new(), "href".to_owned())
        );
    }
}
