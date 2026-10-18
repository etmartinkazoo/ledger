//! CardDAV (RFC 6352) over HTTPS: finding an account's address books (RFC 6764 and a provider
//! list), listing their cards' ETags so a refresh fetches only what changed, fetching cards in
//! batches, and writing one card at a time with ETags, so an edit never overwrites a change made
//! elsewhere in the meantime.

use reqwest::{Method, StatusCode, header};
use std::sync::Arc;
use std::time::Duration;
use url::Url;
use zeroize::Zeroizing;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;
const DAV: &str = "DAV:";
const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
const CALSERVER: &str = "http://calendarserver.org/ns/";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the server didn't accept the password")]
    Unauthorized,
    #[error("sign in with Google again")]
    SignIn,
    #[error("not found")]
    NotFound,
    #[error("the contact changed on the server; refresh and try again")]
    Changed,
    #[error("the server answered {0}")]
    Status(StatusCode),
    #[error("couldn't reach the server: {0}")]
    Network(String),
    #[error("the server's answer couldn't be read")]
    Answer,
    #[error("no contacts server was found for this account")]
    NoServer,
    #[error("{0}")]
    Unsupported(&'static str),
}

/// An address book collection on the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteBook {
    pub url: Url,
    pub name: String,
    pub ctag: Option<String>,
    pub read_only: bool,
}

/// One card resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub url: Url,
    pub etag: Option<String>,
    pub vcf: String,
}

/// When a PUT may write.
#[derive(Clone, Copy, Debug)]
pub enum Condition<'a> {
    /// Only if nothing has the name yet.
    Create,
    /// Only over exactly this version.
    Match(&'a str),
    /// Over whatever is there: for a card whose server sent no ETag.
    Any,
}

/// How requests prove who they're from.
pub enum Credentials {
    /// A password or app password, sent with HTTP Basic authentication.
    Password { username: String, password: Zeroizing<String> },
    /// An OAuth access token, renewed as it runs out (Google).
    Google(Arc<crate::google::Tokens>),
}

pub struct Client {
    http: reqwest::Client,
    credentials: Credentials,
}

/// The HTTP client every request goes through: rustls with the system's certificates, no
/// redirects followed on its own (`send` follows them), and a 30 s limit.
pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("Ledger/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .tls_backend_preconfigured(tls_config().map_err(|error| error.to_string())?)
        .connect_timeout(TIMEOUT)
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| error.to_string())
}

/// A request error in words: reqwest's own text is only "error sending request".
pub fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() { "timed out".to_owned() } else { chain(error) }
}

impl Client {
    pub fn new(credentials: Credentials) -> Result<Self, Error> {
        Ok(Self {
            http: http_client().map_err(Error::Network)?,
            credentials,
        })
    }

    /// Sends a request, following redirects with the same method and body (a PROPFIND to
    /// `/.well-known/carddav` is answered with one). Credentials go only over HTTPS.
    async fn send(
        &self,
        method: Method,
        url: &Url,
        depth: Option<&str>,
        body: Option<String>,
        extra: &[(header::HeaderName, String)],
    ) -> Result<(Url, reqwest::Response), Error> {
        let mut url = url.clone();
        for _ in 0..=MAX_REDIRECTS {
            if url.scheme() != "https" {
                return Err(Error::Unsupported("the contacts server must use HTTPS"));
            }
            let mut request = self.http.request(method.clone(), url.clone());
            request = match &self.credentials {
                Credentials::Password { username, password } => request.basic_auth(username, Some(password.as_str())),
                Credentials::Google(tokens) => {
                    let token = tokens.access().await.map_err(|error| match error {
                        crate::google::Error::SignInNeeded => Error::SignIn,
                        other => Error::Network(other.to_string()),
                    })?;
                    request.bearer_auth(token.as_str())
                }
            };
            if let Some(depth) = depth {
                request = request.header("Depth", depth);
            }
            for (name, value) in extra {
                request = request.header(name, value);
            }
            if let Some(body) = &body {
                let kind = if method == Method::PUT { "text/vcard; charset=utf-8" } else { "application/xml; charset=utf-8" };
                request = request.header(header::CONTENT_TYPE, kind).body(body.clone());
            }
            let response = request.send().await.map_err(|error| Error::Network(describe(&error)))?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|location| url.join(location).ok())
                    .ok_or(Error::Answer)?;
                url = location;
                continue;
            }
            return match response.status() {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(Error::Unauthorized),
                StatusCode::NOT_FOUND => Err(Error::NotFound),
                StatusCode::PRECONDITION_FAILED => Err(Error::Changed),
                status if status.is_success() => Ok((url, response)),
                status => Err(Error::Status(status)),
            };
        }
        Err(Error::Network("too many redirects".to_owned()))
    }

    async fn multistatus(&self, method: &str, url: &Url, depth: &str, body: String) -> Result<(Url, String), Error> {
        let method = Method::from_bytes(method.as_bytes()).expect("a valid method");
        let (url, response) = self.send(method, url, Some(depth), Some(body), &[]).await?;
        let text = response.text().await.map_err(|_| Error::Answer)?;
        Ok((url, text))
    }

    /// The address book home of the account behind `start`: a server root, a
    /// `/.well-known/carddav` address, a principal or the home itself.
    pub async fn discover(&self, start: &Url) -> Result<Url, Error> {
        let body = propfind(&["<d:current-user-principal/>", "<card:addressbook-home-set/>"]);
        let (base, text) = match self.multistatus("PROPFIND", start, "0", body.clone()).await {
            Err(Error::NotFound | Error::Status(_)) if start.path() == "/" => {
                let known = start.join("/.well-known/carddav").map_err(|_| Error::Answer)?;
                self.multistatus("PROPFIND", &known, "0", body).await?
            }
            other => other?,
        };
        let document = roxmltree::Document::parse(&text).map_err(|_| Error::Answer)?;
        if let Some(home) = href_in(&document, CARDDAV, "addressbook-home-set", &base) {
            return Ok(home);
        }
        let principal = href_in(&document, DAV, "current-user-principal", &base).ok_or(Error::NoServer)?;
        let body = propfind(&["<card:addressbook-home-set/>"]);
        let (base, text) = self.multistatus("PROPFIND", &principal, "0", body).await?;
        let document = roxmltree::Document::parse(&text).map_err(|_| Error::Answer)?;
        href_in(&document, CARDDAV, "addressbook-home-set", &base).ok_or(Error::NoServer)
    }

    /// The address books in a home (the home itself, when it is one).
    pub async fn books(&self, home: &Url) -> Result<Vec<RemoteBook>, Error> {
        let body = propfind(&[
            "<d:resourcetype/>",
            "<d:displayname/>",
            "<cs:getctag/>",
            "<d:sync-token/>",
            "<d:current-user-privilege-set/>",
        ]);
        let (base, text) = self.multistatus("PROPFIND", home, "1", body).await?;
        parse_books(&text, &base)
    }

    /// Every card in a book, by address, with its ETag.
    pub async fn etags(&self, book: &Url) -> Result<Vec<(Url, Option<String>)>, Error> {
        let body = propfind(&["<d:resourcetype/>", "<d:getetag/>"]);
        let (base, text) = self.multistatus("PROPFIND", book, "1", body).await?;
        parse_etags(&text, &base)
    }

    /// The cards at `hrefs`, in one request.
    pub async fn multiget(&self, book: &Url, hrefs: &[Url]) -> Result<Vec<Resource>, Error> {
        let mut body = String::from(
            r#"<?xml version="1.0" encoding="utf-8"?>
<card:addressbook-multiget xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop><d:getetag/><card:address-data/></d:prop>
"#,
        );
        for href in hrefs {
            body.push_str(&format!("  <d:href>{}</d:href>\n", xml_escape(href.path())));
        }
        body.push_str("</card:addressbook-multiget>");
        let (base, text) = self.multistatus("REPORT", book, "1", body).await?;
        parse_cards(&text, &base)
    }

    /// One card, for servers whose multiget is broken.
    pub async fn get(&self, url: &Url) -> Result<Resource, Error> {
        let (url, response) = self.send(Method::GET, url, None, None, &[]).await?;
        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let vcf = response.text().await.map_err(|_| Error::Answer)?;
        Ok(Resource { url, etag, vcf })
    }

    /// Writes a card under `condition`. Returns the new ETag when the server says it.
    pub async fn put(&self, url: &Url, vcf: &str, condition: Condition<'_>) -> Result<Option<String>, Error> {
        let condition: Vec<_> = match condition {
            Condition::Create => vec![(header::IF_NONE_MATCH, "*".to_owned())],
            Condition::Match(etag) => vec![(header::IF_MATCH, etag.to_owned())],
            Condition::Any => Vec::new(),
        };
        let (_, response) = self.send(Method::PUT, url, None, Some(vcf.to_owned()), &condition).await?;
        Ok(response
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned))
    }

    pub async fn delete(&self, url: &Url, etag: Option<&str>) -> Result<(), Error> {
        let condition: Vec<_> = etag.map(|etag| (header::IF_MATCH, etag.to_owned())).into_iter().collect();
        match self.send(Method::DELETE, url, None, None, &condition).await {
            Ok(_) | Err(Error::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// The address books in a PROPFIND answer for a home.
pub fn parse_books(text: &str, base: &Url) -> Result<Vec<RemoteBook>, Error> {
    let document = roxmltree::Document::parse(text).map_err(|_| Error::Answer)?;
    let mut found = Vec::new();
    for response in responses(&document) {
        let Some(url) = response_url(response, base) else { continue };
        let props = ok_props(response);
        let is_book = props
            .iter()
            .any(|prop| child(*prop, DAV, "resourcetype").is_some_and(|kind| child(kind, CARDDAV, "addressbook").is_some()));
        if !is_book {
            continue;
        }
        let text_of = |ns: &str, name: &str| {
            props
                .iter()
                .find_map(|prop| child(*prop, ns, name))
                .and_then(|node| node.text())
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
        };
        let privileges: Vec<&str> = props
            .iter()
            .filter_map(|prop| child(*prop, DAV, "current-user-privilege-set"))
            .flat_map(|set| set.descendants().filter(|n| n.is_element()).map(|n| n.tag_name().name()))
            .collect();
        let read_only = !privileges.is_empty()
            && !privileges.iter().any(|name| matches!(*name, "write" | "write-content" | "bind" | "all"));
        let name = text_of(DAV, "displayname").unwrap_or_else(|| "Contacts".to_owned());
        found.push(RemoteBook {
            name,
            ctag: text_of(CALSERVER, "getctag").or_else(|| text_of(DAV, "sync-token")),
            read_only,
            url,
        });
    }
    Ok(found)
}

/// The cards in a Depth 1 PROPFIND answer for a book, leaving out the book and any collection.
pub fn parse_etags(text: &str, base: &Url) -> Result<Vec<(Url, Option<String>)>, Error> {
    let document = roxmltree::Document::parse(text).map_err(|_| Error::Answer)?;
    let mut found = Vec::new();
    for response in responses(&document) {
        let Some(url) = response_url(response, base) else { continue };
        let props = ok_props(response);
        let collection = props
            .iter()
            .any(|prop| child(*prop, DAV, "resourcetype").is_some_and(|kind| child(kind, DAV, "collection").is_some()));
        if collection || url.path().trim_end_matches('/') == base.path().trim_end_matches('/') {
            continue;
        }
        let etag = props
            .iter()
            .find_map(|prop| child(*prop, DAV, "getetag"))
            .and_then(|node| node.text())
            .map(|etag| etag.trim().to_owned());
        found.push((url, etag));
    }
    Ok(found)
}

/// The cards in a multiget answer.
pub fn parse_cards(text: &str, base: &Url) -> Result<Vec<Resource>, Error> {
    let document = roxmltree::Document::parse(text).map_err(|_| Error::Answer)?;
    let mut found = Vec::new();
    for response in responses(&document) {
        let Some(url) = response_url(response, base) else { continue };
        let props = ok_props(response);
        let vcf = props
            .iter()
            .find_map(|prop| child(*prop, CARDDAV, "address-data"))
            .and_then(|node| node.text())
            .map(str::to_owned);
        let etag = props
            .iter()
            .find_map(|prop| child(*prop, DAV, "getetag"))
            .and_then(|node| node.text())
            .map(|etag| etag.trim().to_owned());
        if let Some(vcf) = vcf {
            found.push(Resource { url, etag, vcf });
        }
    }
    Ok(found)
}

/// Names a PEM file whose certificates are trusted instead of the system's: for tests against a
/// local server only.
pub const TEST_ROOTS_ENV: &str = "LEDGER_TEST_ROOTS";

fn tls_config() -> Result<rustls::ClientConfig, Error> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| Error::Network(error.to_string()))?;
    let config = match std::env::var_os(TEST_ROOTS_ENV) {
        Some(path) => {
            let mut roots = rustls::RootCertStore::empty();
            for cert in CertificateDer::pem_file_iter(path).map_err(|e| Error::Network(e.to_string()))? {
                roots
                    .add(cert.map_err(|e| Error::Network(e.to_string()))?)
                    .map_err(|e| Error::Network(e.to_string()))?;
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        None => builder
            .with_platform_verifier()
            .map_err(|error| Error::Network(error.to_string()))?
            .with_no_client_auth(),
    };
    Ok(config)
}

/// An error with its causes: reqwest's own text is only "error sending request".
fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn propfind(props: &[&str]) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cs="http://calendarserver.org/ns/">
  <d:prop>{}</d:prop>
</d:propfind>"#,
        props.concat()
    )
}

fn child<'a, 'input>(node: roxmltree::Node<'a, 'input>, ns: &str, name: &str) -> Option<roxmltree::Node<'a, 'input>> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name() == name && n.tag_name().namespace() == Some(ns))
}

fn responses<'a, 'input>(document: &'a roxmltree::Document<'input>) -> impl Iterator<Item = roxmltree::Node<'a, 'input>> {
    document
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "response" && n.tag_name().namespace() == Some(DAV))
}

fn response_url(response: roxmltree::Node, base: &Url) -> Option<Url> {
    let href = child(response, DAV, "href")?.text()?.trim();
    base.join(href).ok()
}

/// The `prop` elements of a response's `propstat`s with a 200 status.
fn ok_props<'a, 'input>(response: roxmltree::Node<'a, 'input>) -> Vec<roxmltree::Node<'a, 'input>> {
    response
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "propstat")
        .filter(|propstat| {
            child(*propstat, DAV, "status")
                .and_then(|status| status.text())
                .is_none_or(|status| status.contains(" 200"))
        })
        .filter_map(|propstat| child(propstat, DAV, "prop"))
        .collect()
}

/// The first href inside a property anywhere in the answer.
fn href_in(document: &roxmltree::Document, ns: &str, name: &str, base: &Url) -> Option<Url> {
    let property = document
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == name && n.tag_name().namespace() == Some(ns))?;
    let href = child(property, DAV, "href")?.text()?.trim();
    base.join(href).ok()
}

/// Where an account's address books may be, most likely first: the configured address, Google,
/// a known provider's server, or RFC 6764's well-known address at the mail domain.
pub fn candidates(address: &str, mail_host: Option<&str>, configured: Option<&str>, google: bool) -> Result<Vec<Url>, Error> {
    if let Some(configured) = configured {
        return Url::parse(configured)
            .map(|url| vec![url])
            .map_err(|_| Error::Unsupported("the carddav address in Ledger's config.toml isn't a URL"));
    }
    if google {
        return Ok(vec![Url::parse(crate::google::CARDDAV_START).expect("a valid URL")]);
    }
    let domain = address.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()).unwrap_or_default();
    let host = mail_host.unwrap_or_default().to_ascii_lowercase();
    let either = |suffixes: &[&str]| {
        suffixes
            .iter()
            .any(|s| domain == *s || domain.ends_with(&format!(".{s}")) || host.ends_with(s))
    };
    if either(&["outlook.com", "hotmail.com", "live.com", "msn.com", "office365.com"]) {
        return Err(Error::Unsupported("Outlook contacts need Microsoft Graph, which Ledger doesn't support yet"));
    }
    let known: &[(&[&str], &str)] = &[
        (&["icloud.com", "me.com", "mac.com", "mail.me.com"], "https://contacts.icloud.com/"),
        (&["fastmail.com", "fastmail.fm", "messagingengine.com"], "https://carddav.fastmail.com/"),
        (&["yahoo.com", "ymail.com", "rocketmail.com"], "https://carddav.address.yahoo.com/"),
        (&["aol.com"], "https://carddav.aol.com/"),
        (&["mailbox.org"], "https://dav.mailbox.org/"),
        (&["posteo.de", "posteo.net"], "https://posteo.de:8843/"),
        (&["gmx.net", "gmx.de", "gmx.at", "gmx.ch"], "https://carddav.gmx.net/"),
        (&["gmx.com"], "https://carddav.gmx.com/"),
        (&["web.de"], "https://carddav.web.de/"),
        (&["zoho.com", "zohomail.com"], "https://contacts.zoho.com/"),
        (&["yandex.ru", "yandex.com"], "https://carddav.yandex.ru/"),
    ];
    if let Some((_, url)) = known.iter().find(|(suffixes, _)| either(suffixes)) {
        return Ok(vec![Url::parse(url).expect("a valid URL")]);
    }
    let mut domains = vec![domain.clone()];
    if let Some((_, parent)) = host.split_once('.')
        && parent.contains('.')
        && !domains.contains(&parent.to_owned())
    {
        domains.push(parent.to_owned());
    }
    let mut urls = Vec::new();
    for domain in domains.iter().filter(|d| !d.is_empty()) {
        for form in ["https://{}/", "https://carddav.{}/", "https://dav.{}/"] {
            if let Ok(url) = Url::parse(&form.replace("{}", domain)) {
                urls.push(url);
            }
        }
    }
    if urls.is_empty() {
        return Err(Error::NoServer);
    }
    Ok(urls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_and_well_known_guesses() {
        let fastmail = candidates("ada@example.com", Some("imap.fastmail.com"), None, false).unwrap();
        assert_eq!(fastmail[0].as_str(), "https://carddav.fastmail.com/");
        let icloud = candidates("ada@icloud.com", None, None, false).unwrap();
        assert_eq!(icloud[0].as_str(), "https://contacts.icloud.com/");
        let own = candidates("ada@example.org", Some("mail.example.net"), None, false).unwrap();
        assert_eq!(own[0].as_str(), "https://example.org/");
        assert!(own.iter().any(|url| url.as_str() == "https://example.net/"));
        let set = candidates("ada@gmail.com", None, Some("https://dav.example/card/"), true).unwrap();
        assert_eq!(set[0].as_str(), "https://dav.example/card/");
        let google = candidates("ada@gmail.com", None, None, true).unwrap();
        assert_eq!(google[0].as_str(), "https://www.googleapis.com/.well-known/carddav");
    }

    #[test]
    fn reads_a_book_listing() {
        let xml = r##"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cs="http://calendarserver.org/ns/">
 <d:response><d:href>/dav/ada/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/dav/ada/contacts/</d:href><d:propstat><d:prop>
   <d:resourcetype><d:collection/><card:addressbook/></d:resourcetype><d:displayname>Contacts</d:displayname>
   <cs:getctag>42</cs:getctag>
   <d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>
 </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 <d:propstat><d:prop><d:sync-token/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
 <d:response><d:href>/dav/ada/shared/</d:href><d:propstat><d:prop>
   <d:resourcetype><d:collection/><card:addressbook/></d:resourcetype><d:sync-token>http://x/sync/7</d:sync-token>
   <d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set>
 </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"##;
        let base = Url::parse("https://dav.example/dav/ada/").unwrap();
        let books = parse_books(xml, &base).unwrap();
        assert_eq!(books.len(), 2, "the home itself is left out");
        assert_eq!(books[0].url.as_str(), "https://dav.example/dav/ada/contacts/");
        assert_eq!((books[0].name.as_str(), books[0].ctag.as_deref(), books[0].read_only), ("Contacts", Some("42"), false));
        assert_eq!((books[1].name.as_str(), books[1].ctag.as_deref(), books[1].read_only), ("Contacts", Some("http://x/sync/7"), true));
    }

    #[test]
    fn reads_etags_and_cards() {
        let listing = r#"<d:multistatus xmlns:d="DAV:">
 <d:response><d:href>/dav/ada/contacts/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/dav/ada/contacts/a%20b.vcf</d:href><d:propstat><d:prop><d:resourcetype/><d:getetag>"e1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;
        let base = Url::parse("https://dav.example/dav/ada/contacts/").unwrap();
        let etags = parse_etags(listing, &base).unwrap();
        assert_eq!(etags, [(base.join("a%20b.vcf").unwrap(), Some("\"e1\"".to_owned()))]);

        let cards = r#"<d:multistatus xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
 <d:response><d:href>/dav/ada/contacts/a%20b.vcf</d:href><d:propstat><d:prop>
  <d:getetag>"e1"</d:getetag><card:address-data>BEGIN:VCARD&#13;
FN:Ada&#13;
END:VCARD&#13;
</card:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/dav/ada/contacts/gone.vcf</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
</d:multistatus>"#;
        let found = parse_cards(cards, &base).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].vcf.starts_with("BEGIN:VCARD"));
        assert_eq!(xml_escape("/a&b/"), "/a&amp;b/");
    }
}
