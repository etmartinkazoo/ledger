//! A card as the window shows it (names, company, phones, emails, addresses, websites, birthday,
//! notes, photo) and the edits the editor makes. An edit rewrites only the fields that changed:
//! a phone number left alone keeps its line as the server sent it, labels and all.

use crate::vcard::{self, Card, Property};
use chrono::{Datelike, Utc};

/// Apple's way of keeping a birthday without a year in vCard 3.0, which has no form for one.
const NO_YEAR: i32 = 1604;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Phone,
    Email,
    Address,
    Website,
}

impl Kind {
    /// The labels the editor offers, first the default; anything else is a custom label.
    pub fn labels(self) -> &'static [&'static str] {
        match self {
            Kind::Phone => &["mobile", "home", "work", "main", "iPhone", "fax", "pager", "other"],
            Kind::Email | Kind::Address => &["home", "work", "other"],
            Kind::Website => &["homepage", "home", "work", "other"],
        }
    }

    /// The vCard `TYPE` a standard label is written as.
    fn type_of(self, label: &str) -> Option<&'static str> {
        let label = label.to_ascii_lowercase();
        Some(match (self, label.as_str()) {
            (Kind::Phone, "mobile") => "CELL",
            (Kind::Phone, "iphone") => "IPHONE",
            (Kind::Phone, "main") => "MAIN",
            (Kind::Phone, "fax") => "FAX",
            (Kind::Phone, "pager") => "PAGER",
            (Kind::Email, "home") => "INTERNET,HOME",
            (Kind::Email, "work") => "INTERNET,WORK",
            (Kind::Email, "other") => "INTERNET",
            (Kind::Website, "homepage") => "HOMEPAGE",
            (_, "home") => "HOME",
            (_, "work") => "WORK",
            (_, "other") => "OTHER",
            _ => return None,
        })
    }

    fn property(self) -> &'static str {
        match self {
            Kind::Phone => "TEL",
            Kind::Email => "EMAIL",
            Kind::Address => "ADR",
            Kind::Website => "URL",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Field {
    pub label: String,
    pub value: String,
}

impl Field {
    pub fn new(label: &str, value: &str) -> Self {
        Self { label: label.to_owned(), value: value.to_owned() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Address {
    pub label: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postcode: String,
    pub country: String,
}

impl Address {
    pub fn is_empty(&self) -> bool {
        [&self.street, &self.city, &self.region, &self.postcode, &self.country].iter().all(|part| part.trim().is_empty())
    }

    /// The address as lines to show: the street, then the town, then the country.
    pub fn lines(&self) -> Vec<String> {
        let town = [&self.city, &self.region, &self.postcode]
            .iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        self.street
            .lines()
            .chain([town.as_str(), self.country.trim()])
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Birthday {
    pub year: Option<i32>,
    pub month: u32,
    pub day: u32,
}

impl Birthday {
    /// `1990-03-12`, `19900312`, `--03-12`, `--0312`, with or without a time after it; a year
    /// of 1604, or the one `omit` names, is no year (Apple's way).
    pub fn parse(text: &str, omit: Option<&str>) -> Option<Self> {
        let date = text.trim().split('T').next()?.replace('-', "");
        let (year, rest) = if text.trim().starts_with("--") {
            (None, date.as_str())
        } else {
            (Some(date.get(..4)?.parse::<i32>().ok()?), date.get(4..)?)
        };
        if rest.len() != 4 {
            return None;
        }
        let (month, day) = (rest[..2].parse().ok()?, rest[2..].parse().ok()?);
        chrono::NaiveDate::from_ymd_opt(2000, month, day)?;
        let omitted = |year: i32| year == NO_YEAR || omit.is_some_and(|omit| omit.trim() == year.to_string());
        Some(Self { year: year.filter(|year| !omitted(*year)), month, day })
    }

    /// What a person types: `1990-03-12`, or `03-12` without a year.
    pub fn parse_input(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() == 5 && text.as_bytes()[2] == b'-' {
            return Self::parse(&format!("--{text}"), None);
        }
        Self::parse(text, None).filter(|b| b.year.is_some_and(|year| (1000..=9999).contains(&year)))
    }

    pub fn to_input(self) -> String {
        match self.year {
            Some(year) => format!("{year:04}-{:02}-{:02}", self.month, self.day),
            None => format!("{:02}-{:02}", self.month, self.day),
        }
    }

    /// `12 March 1990`, or `12 March`.
    pub fn to_text(self) -> String {
        let date = chrono::NaiveDate::from_ymd_opt(self.year.unwrap_or(2000), self.month, self.day);
        let month = date.map(|date| date.format("%B").to_string()).unwrap_or_default();
        match self.year {
            Some(year) => format!("{} {month} {year}", self.day),
            None => format!("{} {month}", self.day),
        }
    }

    fn property(self) -> Property {
        let year = self.year.unwrap_or(NO_YEAR);
        let property = Property::new("BDAY", format!("{year:04}-{:02}-{:02}", self.month, self.day));
        if self.year.is_none() { property.with_param("X-APPLE-OMIT-YEAR", &NO_YEAR.to_string()) } else { property }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contact {
    pub uid: Option<String>,
    /// `FN`.
    pub formatted: String,
    pub prefix: String,
    pub given: String,
    pub additional: String,
    pub family: String,
    pub suffix: String,
    pub nickname: String,
    pub organization: String,
    pub department: String,
    pub title: String,
    pub phones: Vec<Field>,
    pub emails: Vec<Field>,
    pub addresses: Vec<Address>,
    pub websites: Vec<Field>,
    pub birthday: Option<Birthday>,
    pub note: String,
    pub photo: Option<Vec<u8>>,
    /// Shown by its company's name (Apple's `X-ABShowAs:COMPANY`, vCard 4's `KIND:org`).
    pub is_company: bool,
}

/// A label as Ledger names it, from Apple's `X-ABLabel` or the property's types.
fn label(card: &Card, property: &Property, kind: Kind) -> String {
    if let Some(label) = property.group.as_deref().and_then(|group| card.group_label(group)) {
        let label = label.trim();
        let plain = label.strip_prefix("_$!<").and_then(|l| l.strip_suffix(">!$_")).unwrap_or(label);
        return match plain.to_ascii_lowercase().as_str() {
            known @ ("mobile" | "home" | "work" | "main" | "other" | "homepage" | "pager") => known.to_owned(),
            "iphone" => "iPhone".to_owned(),
            "homefax" | "workfax" | "otherfax" => "fax".to_owned(),
            _ => plain.to_owned(),
        };
    }
    let types = property.types();
    let has = |name: &str| types.iter().any(|t| t == name);
    let found = match kind {
        Kind::Phone if has("cell") => "mobile",
        Kind::Phone if has("iphone") => "iPhone",
        Kind::Phone if has("fax") => "fax",
        Kind::Phone if has("pager") => "pager",
        Kind::Phone if has("main") => "main",
        Kind::Website if has("homepage") => "homepage",
        _ if has("home") => "home",
        _ if has("work") => "work",
        _ => "other",
    };
    found.to_owned()
}

fn decode_photo(property: &Property) -> Option<Vec<u8>> {
    use base64::Engine;
    let value = property.value.trim();
    let data = if let Some(rest) = value.strip_prefix("data:") {
        let (head, data) = rest.split_once(',')?;
        head.ends_with(";base64").then_some(data)?
    } else if property.param("ENCODING").is_some_and(|e| e.eq_ignore_ascii_case("b") || e.eq_ignore_ascii_case("base64")) {
        value
    } else {
        return None;
    };
    let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(clean.trim_end_matches('='))
        .ok()
}

impl Contact {
    pub fn from_card(card: &Card) -> Self {
        let name = card.property("N").map(Property::parts).unwrap_or_default();
        let part = |index: usize| name.get(index).map(|part| part.trim().to_owned()).unwrap_or_default();
        let org = card.property("ORG").map(Property::parts).unwrap_or_default();
        let fields = |kind: Kind| -> Vec<Field> {
            card.properties_named(kind.property())
                .map(|property| Field { label: label(card, property, kind), value: property.text().trim().to_owned() })
                .filter(|field| !field.value.is_empty())
                .collect()
        };
        let addresses = card
            .properties_named("ADR")
            .map(|property| {
                let parts = property.parts();
                let part = |index: usize| parts.get(index).map(|p| p.trim().to_owned()).unwrap_or_default();
                Address {
                    label: label(card, property, Kind::Address),
                    street: [part(0), part(1), part(2)].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n"),
                    city: part(3),
                    region: part(4),
                    postcode: part(5),
                    country: part(6),
                }
            })
            .filter(|address| !address.is_empty())
            .collect();
        let shows_company = card.text("X-ABSHOWAS").is_some_and(|show| show.eq_ignore_ascii_case("company"))
            || card.text("KIND").is_some_and(|kind| kind.eq_ignore_ascii_case("org"));
        Self {
            uid: card.text("UID"),
            formatted: card.text("FN").unwrap_or_default().trim().to_owned(),
            family: part(0),
            given: part(1),
            additional: part(2),
            prefix: part(3),
            suffix: part(4),
            nickname: card.text("NICKNAME").unwrap_or_default().trim().to_owned(),
            organization: org.first().map(|o| o.trim().to_owned()).unwrap_or_default(),
            department: org.get(1..).map(|rest| rest.join(", ").trim().to_owned()).unwrap_or_default(),
            title: card.text("TITLE").unwrap_or_default().trim().to_owned(),
            phones: fields(Kind::Phone),
            emails: fields(Kind::Email),
            addresses,
            websites: fields(Kind::Website),
            birthday: card
                .property("BDAY")
                .and_then(|bday| Birthday::parse(&bday.value, bday.param("X-APPLE-OMIT-YEAR"))),
            note: card.text("NOTE").unwrap_or_default(),
            photo: card.property("PHOTO").and_then(decode_photo),
            is_company: shows_company,
        }
    }

    /// The given and family names with the prefix and suffix, as a card's `FN` is usually made.
    fn composed(&self) -> String {
        [&self.prefix, &self.given, &self.additional, &self.family, &self.suffix]
            .iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The name the list shows: a person's name, a company's, or failing both an address or
    /// number.
    pub fn display_name(&self) -> String {
        if self.is_company && !self.organization.is_empty() {
            return self.organization.clone();
        }
        [self.formatted.clone(), self.composed(), self.organization.clone()]
            .into_iter()
            .chain(self.emails.first().map(|f| f.value.clone()))
            .chain(self.phones.first().map(|f| f.value.clone()))
            .find(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "No Name".to_owned())
    }

    /// What the list sorts by: the family name first, or the given name first.
    pub fn sort_key(&self, by_family: bool) -> String {
        let named = !self.is_company && (!self.given.is_empty() || !self.family.is_empty());
        let key = match (named, by_family) {
            (true, true) if !self.family.is_empty() => format!("{} {}", self.family, self.given),
            (true, _) => format!("{} {}", self.given, self.family),
            (false, _) => self.display_name(),
        };
        key.trim().to_lowercase()
    }

    /// The letter the list files the contact under, or `#`.
    pub fn section(&self, by_family: bool) -> String {
        match self.sort_key(by_family).chars().next() {
            Some(c) if c.is_alphabetic() => c.to_uppercase().collect(),
            _ => "#".to_owned(),
        }
    }

    /// The job and company on one line: `Engineer, Acme`.
    pub fn job(&self) -> String {
        if self.is_company {
            return String::new();
        }
        [&self.title, &self.organization]
            .iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Whether every word of a search is somewhere in the contact; a word with digits in it also
    /// matches a phone number written with spaces or dashes.
    pub fn matches(&self, search: &str) -> bool {
        let text = [
            self.formatted.as_str(),
            &self.given,
            &self.family,
            &self.nickname,
            &self.organization,
            &self.department,
            &self.title,
            &self.note,
        ]
        .into_iter()
        .map(str::to_owned)
        .chain(self.emails.iter().map(|f| f.value.clone()))
        .chain(self.websites.iter().map(|f| f.value.clone()))
        .chain(self.addresses.iter().flat_map(Address::lines))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
        let digits: Vec<String> = self.phones.iter().map(|f| f.value.chars().filter(char::is_ascii_digit).collect()).collect();
        search.split_whitespace().all(|word| {
            let word = word.to_lowercase();
            let word_digits: String = word.chars().filter(char::is_ascii_digit).collect();
            text.contains(&word)
                || self.phones.iter().any(|f| f.value.to_lowercase().contains(&word))
                || (word_digits.len() >= 3 && digits.iter().any(|d| d.contains(&word_digits)))
        })
    }
}

/// What the editor edits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub given: String,
    pub family: String,
    pub nickname: String,
    pub organization: String,
    pub title: String,
    pub phones: Vec<Field>,
    pub emails: Vec<Field>,
    pub addresses: Vec<Address>,
    pub websites: Vec<Field>,
    /// As typed: `1990-03-12`, or `03-12`.
    pub birthday: String,
    pub note: String,
}

impl Draft {
    pub fn of(contact: &Contact) -> Self {
        Self {
            given: contact.given.clone(),
            family: contact.family.clone(),
            nickname: contact.nickname.clone(),
            organization: contact.organization.clone(),
            title: contact.title.clone(),
            phones: contact.phones.clone(),
            emails: contact.emails.clone(),
            addresses: contact.addresses.clone(),
            websites: contact.websites.clone(),
            birthday: contact.birthday.map(Birthday::to_input).unwrap_or_default(),
            note: contact.note.clone(),
        }
    }

    /// Without blank fields and stray spaces: what is saved.
    pub fn cleaned(&self) -> Self {
        let fields = |list: &[Field]| -> Vec<Field> {
            list.iter()
                .filter(|f| !f.value.trim().is_empty())
                .map(|f| Field::new(f.label.trim(), f.value.trim()))
                .collect()
        };
        Self {
            given: self.given.trim().to_owned(),
            family: self.family.trim().to_owned(),
            nickname: self.nickname.trim().to_owned(),
            organization: self.organization.trim().to_owned(),
            title: self.title.trim().to_owned(),
            phones: fields(&self.phones),
            emails: fields(&self.emails),
            addresses: self.addresses.iter().filter(|a| !a.is_empty()).cloned().collect(),
            websites: fields(&self.websites),
            birthday: self.birthday.trim().to_owned(),
            note: self.note.trim_end().to_owned(),
        }
    }

    /// Nothing to name the contact by.
    pub fn is_empty(&self) -> bool {
        let draft = self.cleaned();
        draft.given.is_empty()
            && draft.family.is_empty()
            && draft.organization.is_empty()
            && draft.nickname.is_empty()
            && draft.phones.is_empty()
            && draft.emails.is_empty()
    }

    /// Why the draft can't be saved, in words for the editor.
    pub fn problem(&self) -> Option<&'static str> {
        let birthday = self.birthday.trim();
        if !birthday.is_empty() && Birthday::parse_input(birthday).is_none() {
            return Some("Write the birthday as 1990-03-12, or 03-12 without a year");
        }
        if self.is_empty() {
            return Some("Give the contact a name, a company, a phone number or an email address");
        }
        None
    }
}

/// The properties for a list of labelled values. Standard labels become `TYPE`s; a custom one
/// goes in an `X-ABLabel` of its own group, as Apple writes it.
fn labelled(card: &Card, kind: Kind, values: Vec<(String, String)>) -> Vec<Property> {
    let mut taken = Vec::new();
    let mut out = Vec::new();
    for (label, value) in values {
        let property = Property::new(kind.property(), value);
        match kind.type_of(&label) {
            Some(kind) => out.push(property.with_param("TYPE", kind)),
            None => {
                let group = (1..)
                    .map(|n| format!("item{n}"))
                    .find(|name| {
                        !taken.contains(name)
                            && !card.properties.iter().any(|p| p.group.as_deref().is_some_and(|g| g.eq_ignore_ascii_case(name)))
                    })
                    .expect("a free group");
                taken.push(group.clone());
                out.push(property.with_group(Some(group.clone())));
                out.push(Property::new("X-ABLABEL", vcard::escape(&label)).with_group(Some(group)));
            }
        }
    }
    out
}

/// Writes a draft into a card, changing only the fields that differ from what the card says.
pub fn apply(card: &mut Card, draft: &Draft) {
    let contact = Contact::from_card(card);
    let before = Draft::of(&contact);
    let draft = draft.cleaned();
    let names_changed = (&draft.given, &draft.family, &draft.organization) != (&before.given, &before.family, &before.organization);
    if names_changed || card.property("N").is_none() {
        let n = vcard::join_parts(&[&draft.family, &draft.given, &contact.additional, &contact.prefix, &contact.suffix]);
        card.replace("N", vec![Property::new("N", n)]);
    }
    if names_changed || card.property("FN").is_none() {
        let named = Contact { given: draft.given.clone(), family: draft.family.clone(), ..contact.clone() };
        let formatted = match named.composed() {
            name if !name.is_empty() => name,
            _ => draft.organization.clone(),
        };
        card.replace("FN", vec![Property::new("FN", vcard::escape(&formatted))]);
    }
    if draft.nickname != before.nickname {
        card.set_text("NICKNAME", &draft.nickname);
    }
    if draft.organization != before.organization {
        let properties = if draft.organization.is_empty() && contact.department.is_empty() {
            Vec::new()
        } else {
            vec![Property::new("ORG", vcard::join_parts(&[&draft.organization, &contact.department]))]
        };
        card.replace("ORG", properties);
    }
    if draft.title != before.title {
        card.set_text("TITLE", &draft.title);
    }
    for (kind, now, then) in [
        (Kind::Phone, &draft.phones, &before.phones),
        (Kind::Email, &draft.emails, &before.emails),
        (Kind::Website, &draft.websites, &before.websites),
    ] {
        if now != then {
            // A URL is a URI, not text: it isn't escaped.
            let escape = |value: &str| if kind == Kind::Website { value.to_owned() } else { vcard::escape(value) };
            let properties = labelled(card, kind, now.iter().map(|f| (f.label.clone(), escape(&f.value))).collect());
            card.replace(kind.property(), properties);
        }
    }
    if draft.addresses != before.addresses {
        let values = draft
            .addresses
            .iter()
            .map(|a| (a.label.clone(), vcard::join_parts(&["", "", &a.street, &a.city, &a.region, &a.postcode, &a.country])))
            .collect();
        let properties = labelled(card, Kind::Address, values);
        card.replace("ADR", properties);
    }
    if draft.birthday != before.birthday {
        let properties = Birthday::parse_input(&draft.birthday).map(Birthday::property).into_iter().collect();
        card.replace("BDAY", properties);
    }
    if draft.note != before.note {
        card.set_text("NOTE", &draft.note);
    }
    card.replace("REV", vec![Property::new("REV", Utc::now().format("%Y%m%dT%H%M%SZ").to_string())]);
}

pub fn new_uid() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}

/// A new vCard 3.0 card for a draft: the version every CardDAV server takes.
pub fn new_card(uid: &str, draft: &Draft) -> Card {
    let mut card = Card {
        properties: vec![
            Property::new("VERSION", "3.0"),
            Property::new("PRODID", "-//brem.io//Ledger//EN"),
            Property::new("UID", vcard::escape(uid)),
        ],
    };
    let draft = draft.cleaned();
    if draft.given.is_empty() && draft.family.is_empty() && !draft.organization.is_empty() {
        card.properties.push(Property::new("X-ABSHOWAS", "COMPANY"));
    }
    apply(&mut card, &draft);
    card
}

/// Makes sure a card has a UID, as CardDAV servers want; gives it.
pub fn ensure_uid(card: &mut Card) -> String {
    if let Some(uid) = card.text("UID") {
        return uid;
    }
    let uid = new_uid();
    card.properties.push(Property::new("UID", uid.clone()));
    uid
}

/// Where a contact's phone, email, website or address opens.
pub fn link(kind: Kind, value: &str) -> Option<String> {
    let value = value.trim();
    match kind {
        Kind::Phone => {
            let number: String = value.chars().filter(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#')).collect();
            (!number.is_empty()).then(|| format!("tel:{number}"))
        }
        Kind::Email => crate::accounts::is_address(value).then(|| format!("mailto:{value}")),
        Kind::Website if value.contains("://") => Some(value.to_owned()),
        Kind::Website => (!value.is_empty() && !value.contains(' ')).then(|| format!("https://{value}")),
        Kind::Address => {
            let query: String = url::form_urlencoded::byte_serialize(value.replace('\n', ", ").as_bytes()).collect();
            Some(format!("https://www.openstreetmap.org/search?query={query}"))
        }
    }
}

/// Today's month and day, for "birthday today".
pub fn is_birthday_today(birthday: Birthday) -> bool {
    let today = chrono::Local::now().date_naive();
    today.month() == birthday.month && today.day() == birthday.day
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Lovelace;Ada;Augusta;Countess\\, Lady;\r\nFN:Ada Lovelace\r\n\
        ORG:Analytical Engines;Research\r\nTITLE:Programmer\r\n\
        item1.EMAIL;type=INTERNET;type=pref:ada@example.com\r\nitem1.X-ABLabel:_$!<Other>!$_\r\n\
        TEL;type=CELL;type=VOICE;type=pref:+44 20 7946 0000\r\nitem2.TEL:+44 20 7946 0001\r\nitem2.X-ABLabel:Lab\r\n\
        ADR;type=HOME:;;12 St James's Square;London;;SW1Y 4JH;United Kingdom\r\n\
        BDAY;X-APPLE-OMIT-YEAR=1604:1604-12-10\r\nPHOTO;ENCODING=b;TYPE=JPEG:aGVsbG8=\r\n\
        X-SOCIALPROFILE;type=twitter:x-apple:ada\r\nUID:ada-1\r\nEND:VCARD\r\n";

    fn ada() -> Card {
        vcard::parse(APPLE).unwrap()
    }

    #[test]
    fn reads_a_contact() {
        let contact = Contact::from_card(&ada());
        assert_eq!(contact.display_name(), "Ada Lovelace");
        assert_eq!((contact.given.as_str(), contact.family.as_str(), contact.prefix.as_str()), ("Ada", "Lovelace", "Countess, Lady"));
        assert_eq!((contact.organization.as_str(), contact.department.as_str()), ("Analytical Engines", "Research"));
        assert_eq!(contact.job(), "Programmer, Analytical Engines");
        assert_eq!(contact.emails, [Field::new("other", "ada@example.com")]);
        assert_eq!(contact.phones, [Field::new("mobile", "+44 20 7946 0000"), Field::new("Lab", "+44 20 7946 0001")]);
        assert_eq!(contact.addresses[0].lines(), ["12 St James's Square", "London SW1Y 4JH", "United Kingdom"]);
        assert_eq!(contact.birthday, Some(Birthday { year: None, month: 12, day: 10 }));
        assert_eq!(contact.birthday.unwrap().to_text(), "10 December");
        assert_eq!(contact.photo.as_deref(), Some(&b"hello"[..]));
        assert_eq!((contact.sort_key(true), contact.section(true)), ("lovelace ada".to_owned(), "L".to_owned()));
        assert_eq!(contact.section(false), "A");
    }

    #[test]
    fn searches_names_companies_and_numbers() {
        let contact = Contact::from_card(&ada());
        assert!(contact.matches("ada engines"));
        assert!(contact.matches("7946-0001"), "digits match however they're spaced");
        assert!(contact.matches("LONDON"));
        assert!(!contact.matches("ada babbage"));
    }

    #[test]
    fn an_edit_changes_only_what_changed() {
        let mut card = ada();
        let mut draft = Draft::of(&Contact::from_card(&card));
        draft.emails.push(Field::new("work", "ada@engines.example"));
        draft.title = String::new();
        draft.birthday = "1815-12-10".to_owned();
        apply(&mut card, &draft);
        let text = card.to_vcf();
        assert!(text.contains("TEL;TYPE=CELL;TYPE=VOICE;TYPE=pref:+44 20 7946 0000"), "untouched lines keep their values and types");
        assert!(text.contains("item2.X-ABLABEL:Lab"));
        assert!(text.contains("X-SOCIALPROFILE;TYPE=twitter:x-apple:ada"));
        assert!(text.contains("PHOTO;ENCODING=b;TYPE=JPEG:aGVsbG8="));
        assert!(!text.contains("TITLE"));
        assert!(text.contains("BDAY:1815-12-10"));
        assert!(text.contains("FN:Ada Lovelace"), "names didn't change");
        let again = Contact::from_card(&vcard::parse(&text).unwrap());
        assert_eq!(again.emails, [Field::new("other", "ada@example.com"), Field::new("work", "ada@engines.example")]);
        assert_eq!(again.birthday.unwrap().year, Some(1815));
    }

    #[test]
    fn custom_labels_and_new_cards() {
        let draft = Draft {
            given: "Charles".into(),
            family: "Babbage".into(),
            phones: vec![Field::new("Workshop", "+44 1"), Field::new("mobile", " +44 2 ")],
            addresses: vec![Address { label: "work".into(), street: "1 Dorset St".into(), city: "London".into(), ..Address::default() }],
            websites: vec![Field::new("homepage", "babbage.example/a,b")],
            birthday: "12-26".into(),
            ..Draft::default()
        };
        let card = new_card("C-1", &draft);
        let text = card.to_vcf();
        assert!(text.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n"));
        assert!(text.contains("N:Babbage;Charles;;;\r\n"));
        assert!(text.contains("item1.TEL:+44 1\r\nitem1.X-ABLABEL:Workshop\r\n"));
        assert!(text.contains("TEL;TYPE=CELL:+44 2\r\n"));
        assert!(text.contains("URL;TYPE=HOMEPAGE:babbage.example/a,b"));
        assert!(text.contains("BDAY;X-APPLE-OMIT-YEAR=1604:1604-12-26"));
        let contact = Contact::from_card(&vcard::parse(&text).unwrap());
        assert_eq!(Draft::of(&contact), draft.cleaned());

        let company = new_card("C-2", &Draft { organization: "Engines Ltd".into(), ..Draft::default() });
        let contact = Contact::from_card(&company);
        assert!(contact.is_company);
        assert_eq!(contact.display_name(), "Engines Ltd");
        assert_eq!(contact.section(true), "E");
    }

    #[test]
    fn drafts_and_birthdays() {
        assert!(Draft::default().is_empty());
        assert_eq!(Draft { birthday: "march".into(), given: "A".into(), ..Draft::default() }.problem().map(|p| &p[..10]), Some("Write the "));
        assert_eq!(Birthday::parse("19900312", None), Some(Birthday { year: Some(1990), month: 3, day: 12 }));
        assert_eq!(Birthday::parse("--0312", None), Some(Birthday { year: None, month: 3, day: 12 }));
        assert_eq!(Birthday::parse("2001-02-30", None), None);
        assert_eq!(Birthday::parse_input("03-12").unwrap().to_input(), "03-12");
        assert_eq!(Birthday::parse_input("90-03-12"), None);
        assert_eq!(link(Kind::Phone, "+44 (20) 7946-0000").as_deref(), Some("tel:+442079460000"));
        assert_eq!(link(Kind::Email, "not an address"), None);
        assert_eq!(link(Kind::Website, "example.com").as_deref(), Some("https://example.com"));
    }
}
