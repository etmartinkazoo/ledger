//! The accounts Ledger shares with Sealwax (and Signet). It reads Sealwax's `accounts.toml`
//! (never writes it) and each account's password from the item Sealwax keeps in the keyring, so an
//! account set up once in Sealwax has its contacts here too. Ledger's own `config.toml` adds what
//! Sealwax has no reason to know: a CardDAV address when discovery can't find one, or an account
//! to leave out.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The app-id Sealwax's release build scopes its keyring items with.
pub const SEALWAX_APP_ID: &str = "io.github.pixdevsapps.Sealwax";

fn base_dir(variable: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(variable).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => std::env::home_dir().unwrap_or_default().join(fallback),
    }
}

pub fn sealwax_accounts_file() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config").join("sealwax/accounts.toml")
}

pub fn ledger_config_file() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config").join("ledger/config.toml")
}

pub fn ledger_data_dir() -> PathBuf {
    base_dir("XDG_DATA_HOME", ".local/share").join("ledger")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    Password,
    Microsoft,
}

/// One Sealwax account, as much of it as an address book needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub key: String,
    pub label: String,
    pub address: String,
    pub username: String,
    pub auth: Auth,
    /// The incoming mail server, a hint for finding the contacts server.
    pub mail_host: Option<String>,
    /// From Ledger's `config.toml`: the CardDAV address to use instead of discovery.
    pub carddav: Option<String>,
}

impl Account {
    /// Gmail, whose contacts are Google Contacts and take Sign in with Google.
    pub fn is_google(&self) -> bool {
        self.carddav.is_none() && crate::google::is_google(&self.address, self.mail_host.as_deref())
    }
}

#[derive(Deserialize)]
struct SealwaxFile {
    #[serde(default)]
    account: Vec<SealwaxAccount>,
}

#[derive(Deserialize)]
struct SealwaxAccount {
    key: String,
    address: String,
    label: Option<String>,
    username: Option<String>,
    auth: Option<String>,
    imap: Option<Server>,
    pop3: Option<Server>,
}

#[derive(Deserialize)]
struct Server {
    host: String,
}

/// Whether `text` looks like an email address: one `@` with text on both sides, no spaces.
pub fn is_address(text: &str) -> bool {
    match text.rsplit_once('@') {
        Some((local, domain)) => {
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
                && text.len() <= 254 && !text.chars().any(|c| c.is_whitespace() || c.is_control())
        }
        None => false,
    }
}

/// Ledger's `config.toml`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// `"family"` (the default) or `"given"`: which name the list sorts by.
    pub sort_by: Option<String>,
    /// The app-id whose keyring items hold the passwords; Sealwax's release build by default.
    pub sealwax_app_id: Option<String>,
    /// Per Sealwax account key.
    pub account: BTreeMap<String, AccountConfig>,
    /// The OAuth client for Sign in with Google (Google Contacts).
    pub google: Option<crate::google::ClientConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AccountConfig {
    /// The CardDAV server's address, for a provider discovery doesn't find.
    pub carddav: Option<String>,
    /// The CardDAV login name, when it isn't the mail login.
    pub username: Option<String>,
    /// `false` leaves the account's contacts out.
    pub enabled: Option<bool>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    pub fn sorts_by_family(&self) -> bool {
        !self.sort_by.as_deref().is_some_and(|by| by.trim().eq_ignore_ascii_case("given"))
    }

    pub fn app_id(&self) -> &str {
        self.sealwax_app_id.as_deref().unwrap_or(SEALWAX_APP_ID)
    }
}

/// Sealwax's accounts with Ledger's settings applied; disabled ones are left out. A missing
/// file is no accounts.
pub fn load(path: &Path, config: &Config) -> Result<Vec<Account>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let file: SealwaxFile =
        toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(file
        .account
        .into_iter()
        .filter_map(|raw| {
            let settings = config.account.get(&raw.key).cloned().unwrap_or_default();
            if settings.enabled == Some(false) {
                return None;
            }
            let auth = match raw.auth.as_deref() {
                Some("microsoft") => Auth::Microsoft,
                _ => Auth::Password,
            };
            Some(Account {
                label: raw.label.unwrap_or_else(|| raw.address.clone()),
                username: settings
                    .username
                    .or(raw.username)
                    .unwrap_or_else(|| raw.address.clone()),
                mail_host: raw.imap.or(raw.pop3).map(|server| server.host),
                carddav: settings.carddav,
                key: raw.key,
                address: raw.address,
                auth,
            })
        })
        .collect())
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("the keyring is locked")]
    Locked,
    #[error("Sealwax has no password for this account in the keyring")]
    Missing,
    #[error("keyring: {0}")]
    Keyring(String),
}

/// The app-id Ledger's own keyring items are kept under, in debug and release builds alike.
pub const LEDGER_APP_ID: &str = "io.brem.Ledger";
const GOOGLE_TOKEN: &str = "google-refresh-token";

async fn keyring() -> Result<oo7::Keyring, SecretError> {
    let keyring = tokio::time::timeout(std::time::Duration::from_secs(5), oo7::Keyring::new())
        .await
        .map_err(|_| SecretError::Keyring("no keyring answered".to_owned()))?
        .map_err(|error| SecretError::Keyring(error.to_string()))?;
    if keyring.is_locked().await.map_err(|e| SecretError::Keyring(e.to_string()))? {
        keyring.unlock().await.map_err(|_| SecretError::Locked)?;
    }
    Ok(keyring)
}

/// The Google refresh token Ledger keeps for a Sealwax account, if it signed in.
pub async fn google_token(account: &str) -> Result<Option<Zeroizing<String>>, SecretError> {
    let attributes = [("application", LEDGER_APP_ID), ("account", account), ("kind", GOOGLE_TOKEN)];
    let keyring = keyring().await?;
    let items = keyring.search_items(&attributes).await.map_err(|e| SecretError::Keyring(e.to_string()))?;
    let Some(item) = items.first() else { return Ok(None) };
    let secret = item.secret().await.map_err(|e| SecretError::Keyring(e.to_string()))?;
    Ok(String::from_utf8(secret.as_bytes().to_vec()).ok().map(Zeroizing::new))
}

pub async fn store_google_token(account: &str, token: &str) -> Result<(), SecretError> {
    let attributes = [("application", LEDGER_APP_ID), ("account", account), ("kind", GOOGLE_TOKEN)];
    keyring()
        .await?
        .create_item(&format!("Ledger: Google Contacts ({account})"), &attributes, oo7::Secret::text(token), true)
        .await
        .map_err(|e| SecretError::Keyring(e.to_string()))
}

pub async fn forget_google_token(account: &str) -> Result<(), SecretError> {
    let attributes = [("application", LEDGER_APP_ID), ("account", account), ("kind", GOOGLE_TOKEN)];
    keyring().await?.delete(&attributes).await.map_err(|e| SecretError::Keyring(e.to_string()))
}

/// The password Sealwax keeps for `account`: the Secret Service item with Sealwax's attributes.
pub async fn sealwax_password(app_id: &str, account: &str) -> Result<Zeroizing<String>, SecretError> {
    let keyring = keyring().await?;
    let attributes = [("application", app_id), ("account", account), ("kind", "password")];
    let items = keyring
        .search_items(&attributes)
        .await
        .map_err(|error| SecretError::Keyring(error.to_string()))?;
    let item = items.first().ok_or(SecretError::Missing)?;
    let secret = item
        .secret()
        .await
        .map_err(|error| SecretError::Keyring(error.to_string()))?;
    let text = String::from_utf8(secret.as_bytes().to_vec())
        .map_err(|_| SecretError::Keyring("the password isn't text".to_owned()))?;
    Ok(Zeroizing::new(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEALWAX: &str = r#"
[[account]]
key = "work"
name = "Ada Lovelace"
address = "ada@fastmail.com"
label = "Work"
color = "sage"

[account.imap]
host = "imap.fastmail.com"

[account.smtp]
host = "smtp.fastmail.com"

[[account.identity]]
address = "board@example.com"

[[account]]
key = "old"
name = "Ada"
address = "ada@example.org"
auth = "microsoft"
[account.pop3]
host = "pop.example.org"
[account.smtp]
host = "smtp.example.org"
"#;

    #[test]
    fn reads_sealwax_accounts_with_ledgers_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.toml");
        std::fs::write(&path, SEALWAX).unwrap();
        let config: Config = toml::from_str(
            "sort_by = \"given\"\n[account.work]\ncarddav = \"https://dav.example\"\n",
        )
        .unwrap();
        assert!(!config.sorts_by_family());
        let accounts = load(&path, &config).unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].label, "Work");
        assert_eq!(accounts[0].username, "ada@fastmail.com");
        assert_eq!(accounts[0].mail_host.as_deref(), Some("imap.fastmail.com"));
        assert_eq!(accounts[0].carddav.as_deref(), Some("https://dav.example"));
        assert_eq!(accounts[1].auth, Auth::Microsoft);
        assert_eq!(accounts[1].label, "ada@example.org");

        let off: Config = toml::from_str("[account.old]\nenabled = false\n").unwrap();
        assert_eq!(load(&path, &off).unwrap().len(), 1);
        assert!(load(&dir.path().join("none.toml"), &off).unwrap().is_empty());
    }

    #[test]
    fn a_misspelt_setting_is_reported() {
        assert!(toml::from_str::<Config>("sort_bye = \"given\"\n").is_err());
    }
}
