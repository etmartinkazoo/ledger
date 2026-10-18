//! vCard text (RFC 6350, and the 3.0 of RFC 2426 most servers still send) as cards whose
//! properties round-trip: Ledger edits only the fields it shows and writes every other line back
//! as it came, so photos, a server's own properties and Apple's custom labels survive an update.

use std::fmt::Write as _;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Card {
    pub properties: Vec<Property>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    /// Apple's `item1.` prefix, which ties a value to its `X-ABLabel`.
    pub group: Option<String>,
    /// Upper-cased.
    pub name: String,
    /// Parameter names upper-cased, values unquoted. vCard 2.1's bare `TEL;WORK:` is `TYPE=WORK`.
    pub params: Vec<(String, String)>,
    /// The raw value, still escaped.
    pub value: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("line {0} is not a property")]
    Line(usize),
    #[error("a card without its END:VCARD")]
    Unfinished,
    #[error("no vCard")]
    Empty,
}

impl Property {
    pub fn new(name: &str, value: impl Into<String>) -> Self {
        Self {
            group: None,
            name: name.to_ascii_uppercase(),
            params: Vec::new(),
            value: value.into(),
        }
    }

    pub fn with_param(mut self, name: &str, value: &str) -> Self {
        self.params.push((name.to_ascii_uppercase(), value.to_owned()));
        self
    }

    pub fn with_group(mut self, group: Option<String>) -> Self {
        self.group = group;
        self
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Every `TYPE`, lower-cased, however the card spelt them (`TYPE=a,b`, `TYPE=a;TYPE=b`).
    pub fn types(&self) -> Vec<String> {
        self.params
            .iter()
            .filter(|(key, _)| key == "TYPE")
            .flat_map(|(_, value)| value.split(','))
            .map(|kind| kind.trim().to_ascii_lowercase())
            .filter(|kind| !kind.is_empty())
            .collect()
    }

    /// The value of a text property, unescaped.
    pub fn text(&self) -> String {
        unescape(&self.value)
    }

    /// The parts of a structured value (`N`, `ADR`, `ORG`), each unescaped.
    pub fn parts(&self) -> Vec<String> {
        split_unescaped(&self.value, ';').into_iter().map(unescape).collect()
    }
}

impl Card {
    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties.iter().find(|property| property.name.eq_ignore_ascii_case(name))
    }

    pub fn properties_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> {
        self.properties.iter().filter(move |property| property.name.eq_ignore_ascii_case(name))
    }

    pub fn text(&self, name: &str) -> Option<String> {
        self.property(name).map(Property::text).filter(|text| !text.trim().is_empty())
    }

    /// The `X-ABLabel` Apple gives a grouped property, if any.
    pub fn group_label(&self, group: &str) -> Option<String> {
        self.properties
            .iter()
            .find(|p| p.name == "X-ABLABEL" && p.group.as_deref().is_some_and(|g| g.eq_ignore_ascii_case(group)))
            .map(Property::text)
    }

    /// What removing every `name` property takes away: those, and the labels of their groups.
    fn removes(&self, name: &str) -> impl Fn(&Property) -> bool + use<> {
        let name = name.to_owned();
        let groups: Vec<String> = self
            .properties_named(&name)
            .filter_map(|property| property.group.clone())
            .collect();
        move |property: &Property| {
            property.name.eq_ignore_ascii_case(&name)
                || (property.name == "X-ABLABEL"
                    && property.group.as_ref().is_some_and(|group| groups.iter().any(|g| g.eq_ignore_ascii_case(group))))
        }
    }

    /// Removes every `name` property, and the labels of the groups they were in.
    pub fn remove(&mut self, name: &str) {
        let gone = self.removes(name);
        self.properties.retain(|property| !gone(property));
    }

    /// Replaces every `name` property with `properties`, where the first of them was (or at the
    /// end).
    pub fn replace(&mut self, name: &str, properties: Vec<Property>) {
        let gone = self.removes(name);
        let place = match self.properties.iter().position(|p| p.name.eq_ignore_ascii_case(name)) {
            Some(first) => self.properties[..first].iter().filter(|p| !gone(p)).count(),
            None => usize::MAX,
        };
        self.properties.retain(|property| !gone(property));
        let place = place.min(self.properties.len());
        self.properties.splice(place..place, properties);
    }

    /// Sets a text property, or removes it when `text` is empty.
    pub fn set_text(&mut self, name: &str, text: &str) {
        let properties = if text.trim().is_empty() { Vec::new() } else { vec![Property::new(name, escape(text.trim()))] };
        self.replace(name, properties);
    }

    /// A group name no property uses yet: `item1`, `item2`, …
    pub fn free_group(&self) -> String {
        (1..)
            .map(|n| format!("item{n}"))
            .find(|name| !self.properties.iter().any(|p| p.group.as_deref().is_some_and(|g| g.eq_ignore_ascii_case(name))))
            .expect("a free group")
    }

    pub fn to_vcf(&self) -> String {
        let mut out = String::new();
        fold_into(&mut out, "BEGIN:VCARD");
        for property in &self.properties {
            let mut line = String::new();
            if let Some(group) = &property.group {
                let _ = write!(line, "{group}.");
            }
            line.push_str(&property.name);
            for (name, value) in &property.params {
                if value.contains([':', ';', ',']) && name != "TYPE" {
                    let _ = write!(line, ";{name}=\"{value}\"");
                } else {
                    let _ = write!(line, ";{name}={value}");
                }
            }
            line.push(':');
            line.push_str(&property.value);
            fold_into(&mut out, &line);
        }
        fold_into(&mut out, "END:VCARD");
        out
    }
}

/// Every card in `text`: a server resource holds one, a file to import may hold many.
pub fn parse_all(text: &str) -> Result<Vec<Card>, ParseError> {
    let mut cards = Vec::new();
    let mut open: Option<Card> = None;
    for (number, line) in unfold(text).into_iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let property = parse_line(&line).ok_or(ParseError::Line(number + 1))?;
        match property.name.as_str() {
            "BEGIN" if property.value.eq_ignore_ascii_case("VCARD") => open = Some(Card::default()),
            "END" if property.value.eq_ignore_ascii_case("VCARD") => {
                cards.push(open.take().ok_or(ParseError::Line(number + 1))?);
            }
            _ => {
                if let Some(card) = open.as_mut() {
                    card.properties.push(property);
                }
            }
        }
    }
    if open.is_some() {
        return Err(ParseError::Unfinished);
    }
    if cards.is_empty() {
        return Err(ParseError::Empty);
    }
    Ok(cards)
}

/// The one card of a server resource.
pub fn parse(text: &str) -> Result<Card, ParseError> {
    parse_all(text).map(|mut cards| cards.swap_remove(0))
}

fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match (raw.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ => lines.push(raw.to_owned()),
        }
    }
    lines
}

fn parse_line(line: &str) -> Option<Property> {
    // The name and parameters end at the first colon outside quotes.
    let mut in_quotes = false;
    let mut colon = None;
    for (index, character) in line.char_indices() {
        match character {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(index);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = split_unquoted(head, ';').into_iter();
    let full = parts.next()?.trim();
    let (group, name) = match full.split_once('.') {
        Some((group, name)) => (Some(group.to_owned()), name),
        None => (None, full),
    };
    let name = name.to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter(|part| !part.trim().is_empty())
        .map(|part| match part.split_once('=') {
            Some((key, value)) => (key.trim().to_ascii_uppercase(), value.trim().trim_matches('"').to_owned()),
            None => ("TYPE".to_owned(), part.trim().to_owned()),
        })
        .collect();
    Some(Property { group, name, params, value: value.to_owned() })
}

fn split_unquoted(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if character == '"' {
            in_quotes = !in_quotes;
        } else if character == separator && !in_quotes {
            parts.push(&text[start..index]);
            start = index + 1;
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Splits at each `separator` that isn't escaped with a backslash.
fn split_unescaped(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut escaped = false;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        match character {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            c if c == separator => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// A structured value from its parts, each escaped.
pub fn join_parts(parts: &[&str]) -> String {
    parts.iter().map(|part| escape(part.trim())).collect::<Vec<_>>().join(";")
}

/// Folds a content line at 75 octets, never inside a UTF-8 character, ending it with CRLF.
fn fold_into(out: &mut String, line: &str) {
    let mut width = 0;
    for character in line.chars() {
        let size = character.len_utf8();
        if width + size > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(character);
        width += size;
    }
    out.push_str("\r\n");
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

pub fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const APPLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//iOS 18//EN\r\n\
        N:Lovelace;Ada;Augusta;Countess\\, Lady;\r\nFN:Ada Lovelace\r\nORG:Analytical Engines;Research\r\n\
        item1.EMAIL;type=INTERNET;type=pref:ada@example.com\r\nitem1.X-ABLabel:_$!<Other>!$_\r\n\
        TEL;type=CELL;type=VOICE;type=pref:+44 20 7946 0000\r\nTEL;WORK:+44 20 7946 0001\r\n\
        item2.ADR;type=HOME:;;12 St James\\'s Square;London;;SW1Y 4JH;United Kingdom\r\n\
        NOTE:Wrote the first program\\, in 1843.\\nPoet's daughter.\r\n\
        X-SOCIALPROFILE;type=twitter:x-apple:ada\r\nUID:ada-1\r\nEND:VCARD\r\n";

    #[test]
    fn parses_groups_types_and_structured_values() {
        let card = parse(APPLE).unwrap();
        let email = card.property("EMAIL").unwrap();
        assert_eq!(email.group.as_deref(), Some("item1"));
        assert_eq!(email.types(), ["internet", "pref"]);
        assert_eq!(card.group_label("ITEM1").as_deref(), Some("_$!<Other>!$_"));
        assert_eq!(card.property("N").unwrap().parts(), ["Lovelace", "Ada", "Augusta", "Countess, Lady", ""]);
        let tels: Vec<_> = card.properties_named("TEL").collect();
        assert_eq!(tels[1].types(), ["work"], "vCard 2.1's bare type");
        assert_eq!(card.text("NOTE").unwrap(), "Wrote the first program, in 1843.\nPoet's daughter.");
        assert_eq!(card.property("ADR").unwrap().parts()[2], "12 St James's Square");
    }

    #[test]
    fn round_trips() {
        let card = parse(APPLE).unwrap();
        assert_eq!(parse(&card.to_vcf()).unwrap(), card);
        for line in card.to_vcf().split("\r\n") {
            assert!(line.len() <= 75, "{line}");
        }
    }

    #[test]
    fn replacing_a_property_drops_its_label() {
        let mut card = parse(APPLE).unwrap();
        card.replace("EMAIL", vec![Property::new("EMAIL", "ada@new.example").with_param("TYPE", "HOME")]);
        assert!(card.group_label("item1").is_none());
        assert_eq!(card.property("EMAIL").unwrap().value, "ada@new.example");
        assert!(card.property("UID").is_some());
        assert_eq!(card.free_group(), "item1");
        card.set_text("NOTE", "");
        assert!(card.property("NOTE").is_none());
    }

    #[test]
    fn many_cards_and_errors() {
        let two = format!("{APPLE}{}", APPLE.replace("ada-1", "ada-2"));
        assert_eq!(parse_all(&two).unwrap().len(), 2);
        assert_eq!(parse_all("hello"), Err(ParseError::Line(1)));
        assert_eq!(parse_all(""), Err(ParseError::Empty));
        assert_eq!(parse_all("BEGIN:VCARD\nFN:x\n"), Err(ParseError::Unfinished));
        assert_eq!(join_parts(&["a;b", "c"]), "a\\;b;c");
    }
}
