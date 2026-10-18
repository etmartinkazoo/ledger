//! What the window asks for: refreshing every account's address books into the store, and writing
//! a card to its server (or to the local book) before the store shows it.

use crate::accounts::{self, Account, Auth, Config};
use crate::carddav::{self, Client, Condition, Credentials};
use crate::google;
use crate::store::{Book, StoredCard, Store};
use crate::vcard;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use url::Url;

/// How many cards one multiget asks for.
const BATCH: usize = 100;

/// An account whose contacts couldn't be refreshed, and why, in words for the window.
#[derive(Clone, Debug)]
pub struct Problem {
    pub account: String,
    pub message: String,
    /// The account key when signing in (with Google) would fix it.
    pub sign_in: Option<String>,
}

/// Why an account couldn't be refreshed.
enum Failure {
    Message(String),
    SignIn(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

pub struct Service {
    pub store: Store,
    pub config: Config,
    accounts: Mutex<Vec<Account>>,
    clients: tokio::sync::Mutex<HashMap<String, Arc<Client>>>,
}

impl Service {
    /// Opens the store in Ledger's data directory and reads the settings. A settings file with a
    /// mistake is reported, and the defaults are used meanwhile.
    pub fn open() -> Result<(Self, Vec<String>), String> {
        let dir = accounts::ledger_data_dir();
        std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let store = Store::open(&dir.join("ledger.db")).map_err(|error| error.to_string())?;
        let mut warnings = Vec::new();
        let config = Config::load(&accounts::ledger_config_file()).unwrap_or_else(|error| {
            warnings.push(error);
            Config::default()
        });
        Ok((Self::with(store, config), warnings))
    }

    pub fn with(store: Store, config: Config) -> Self {
        Self {
            store,
            config,
            accounts: Mutex::new(Vec::new()),
            clients: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Sealwax's accounts as last read.
    pub fn accounts(&self) -> Vec<Account> {
        self.accounts.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn account_label(&self, key: &str) -> String {
        if key == crate::store::LOCAL_ACCOUNT {
            return "On This Computer".to_owned();
        }
        self.accounts()
            .into_iter()
            .find(|account| account.key == key)
            .map_or_else(|| key.to_owned(), |account| account.label)
    }

    /// Reads Sealwax's accounts again and refreshes each one's address books, all at once. Books
    /// whose tag is unchanged are not read again; in the others only changed cards are fetched.
    pub async fn refresh(self: &Arc<Self>) -> Vec<Problem> {
        let mut problems = Vec::new();
        let list = match accounts::load(&accounts::sealwax_accounts_file(), &self.config) {
            Ok(list) => list,
            Err(message) => {
                problems.push(Problem { account: "Sealwax accounts".to_owned(), message, sign_in: None });
                self.accounts()
            }
        };
        let keys: Vec<String> = list.iter().map(|account| account.key.clone()).collect();
        if let Err(error) = self.store.retain_accounts(&keys) {
            tracing::warn!("{error}");
        }
        *self.accounts.lock().unwrap_or_else(|p| p.into_inner()) = list.clone();

        let mut tasks = tokio::task::JoinSet::new();
        for account in list {
            let service = Arc::clone(self);
            tasks.spawn(async move {
                let result = service.refresh_account(&account).await;
                (account, result)
            });
        }
        while let Some(done) = tasks.join_next().await {
            match done {
                Ok((account, Err(Failure::Message(message)))) => {
                    problems.push(Problem { account: account.label, message, sign_in: None });
                }
                Ok((account, Err(Failure::SignIn(message)))) => {
                    problems.push(Problem { account: account.label, message, sign_in: Some(account.key) });
                }
                _ => {}
            }
        }
        problems
    }

    async fn client(&self, account: &Account) -> Result<Arc<Client>, String> {
        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(&account.key) {
            return Ok(Arc::clone(client));
        }
        let credentials = if account.is_google() {
            let config = self.config.google.clone().ok_or_else(|| google::Error::NotConfigured.to_string())?;
            let refresh = accounts::google_token(&account.key)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| google::Error::SignInNeeded.to_string())?;
            let tokens = google::Tokens::new(config, &account.key, refresh).map_err(|e| e.to_string())?;
            Credentials::Google(Arc::new(tokens))
        } else {
            let password = accounts::sealwax_password(self.config.app_id(), &account.key)
                .await
                .map_err(|error| error.to_string())?;
            Credentials::Password { username: account.username.clone(), password }
        };
        let client = Arc::new(Client::new(credentials).map_err(|e| e.to_string())?);
        clients.insert(account.key.clone(), Arc::clone(&client));
        Ok(client)
    }

    /// The address book home: the one found before, else found now and kept.
    async fn home(&self, account: &Account, client: &Client) -> Result<Url, Failure> {
        if let Some(home) = self.store.home(&account.key).ok().flatten().and_then(|h| Url::parse(&h).ok()) {
            return Ok(home);
        }
        let candidates =
            carddav::candidates(&account.address, account.mail_host.as_deref(), account.carddav.as_deref(), account.is_google())
                .map_err(|error| error.to_string())?;
        let mut last = carddav::Error::NoServer;
        for start in candidates {
            match client.discover(&start).await {
                Ok(home) => {
                    let _ = self.store.set_home(&account.key, Some(home.as_str()));
                    return Ok(home);
                }
                Err(carddav::Error::Unauthorized) => return Err(unauthorized().into()),
                Err(carddav::Error::SignIn) => return Err(self.signed_out(account).await),
                Err(error) => {
                    tracing::debug!("no CardDAV at {start}: {error}");
                    last = error;
                }
            }
        }
        Err(match last {
            carddav::Error::NotFound | carddav::Error::Status(_) | carddav::Error::Network(_) => {
                "no contacts server was found; set its address as carddav = \"https://…\" under \
                 [account.<key>] in ~/.config/ledger/config.toml"
                    .to_owned()
            }
            other => other.to_string(),
        }
        .into())
    }

    async fn refresh_account(&self, account: &Account) -> Result<(), Failure> {
        if account.auth == Auth::Microsoft {
            return Err("Microsoft contacts aren't supported yet".to_owned().into());
        }
        let google_sign_in = |message: String| {
            if account.is_google() && message == google::Error::SignInNeeded.to_string() {
                Failure::SignIn(message)
            } else {
                Failure::Message(message)
            }
        };
        let client = self.client(account).await.map_err(google_sign_in)?;
        let home = self.home(account, &client).await?;
        let remote = match client.books(&home).await {
            Ok(remote) => remote,
            Err(carddav::Error::Unauthorized) => return Err(unauthorized().into()),
            Err(carddav::Error::SignIn) => return Err(self.signed_out(account).await),
            Err(error) => {
                // The home may have moved: look for it again next time.
                let _ = self.store.set_home(&account.key, None);
                return Err(error.to_string().into());
            }
        };
        let stored: HashMap<String, Book> = self
            .store
            .books()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|book| book.account == account.key)
            .map(|book| (book.url.clone(), book))
            .collect();
        let mut keep = Vec::new();
        for book in remote {
            let id = self
                .store
                .upsert_book(&account.key, book.url.as_str(), &book.name, book.read_only)
                .map_err(|e| e.to_string())?;
            keep.push(id);
            let unchanged = stored
                .get(book.url.as_str())
                .is_some_and(|before| before.ctag.is_some() && before.ctag == book.ctag);
            if unchanged {
                continue;
            }
            match self.sync_book(&client, id, &book.url).await {
                Ok(()) => self.store.set_ctag(id, book.ctag.as_deref()).map_err(|e| e.to_string())?,
                Err(carddav::Error::SignIn) => return Err(self.signed_out(account).await),
                Err(error) => return Err(format!("{}: {error}", book.name).into()),
            }
        }
        Ok(self.store.retain_books(&account.key, &keep).map_err(|e| e.to_string())?)
    }

    /// Brings a book's stored cards up to the server's: fetches the new and changed ones, and
    /// forgets the ones that are gone. Public for the live check against a test server.
    pub async fn sync_book(&self, client: &Client, book: i64, url: &Url) -> Result<(), carddav::Error> {
        let listed = client.etags(url).await?;
        let stored = self.store.card_index(book).map_err(|e| carddav::Error::Network(e.to_string()))?;
        let on_server: HashSet<String> = listed.iter().map(|(url, _)| url.to_string()).collect();
        for (gone, (id, _)) in stored.iter().filter(|(url, _)| !on_server.contains(*url)) {
            tracing::debug!("{gone} is gone from the server");
            let _ = self.store.delete_card(*id);
        }
        let wanted: Vec<Url> = listed
            .into_iter()
            .filter(|(url, etag)| match stored.get(url.as_str()) {
                Some((_, known)) => etag.is_none() || known != etag,
                None => true,
            })
            .map(|(url, _)| url)
            .collect();
        for batch in wanted.chunks(BATCH) {
            let fetched = match client.multiget(url, batch).await {
                Ok(fetched) if !fetched.is_empty() => fetched,
                Ok(_) | Err(carddav::Error::Status(_)) => {
                    // A server without a working multiget: one card at a time.
                    let mut one_by_one = Vec::new();
                    for card in batch {
                        match client.get(card).await {
                            Ok(resource) => one_by_one.push(resource),
                            Err(carddav::Error::NotFound) => {}
                            Err(error) => return Err(error),
                        }
                    }
                    one_by_one
                }
                Err(error) => return Err(error),
            };
            for card in fetched {
                if let Err(error) = vcard::parse(&card.vcf) {
                    tracing::warn!("a card that couldn't be read ({}): {error}", card.url);
                }
                self.store
                    .put_card(book, card.url.as_str(), card.etag.as_deref(), &card.vcf)
                    .map_err(|e| carddav::Error::Network(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Google no longer takes the account's token (revoked, or expired): forget it, so the
    /// window offers to sign in again.
    async fn signed_out(&self, account: &Account) -> Failure {
        self.clients.lock().await.remove(&account.key);
        if let Err(error) = accounts::forget_google_token(&account.key).await {
            tracing::warn!("{error}");
        }
        Failure::SignIn(google::Error::SignInNeeded.to_string())
    }

    /// Signs a Google account in through the browser; `open` opens the sign-in page. The
    /// refresh token goes into the keyring under Ledger's name.
    pub async fn google_sign_in(&self, key: &str, open: impl FnOnce(String)) -> Result<(), String> {
        let account = self
            .accounts()
            .into_iter()
            .find(|account| account.key == key)
            .ok_or_else(|| "the account is no longer in Sealwax".to_owned())?;
        let config = self.config.google.clone().ok_or_else(|| google::Error::NotConfigured.to_string())?;
        let grant = google::sign_in(&config, &account.address, open).await.map_err(|e| e.to_string())?;
        accounts::store_google_token(&account.key, &grant.refresh)
            .await
            .map_err(|error| error.to_string())?;
        self.clients.lock().await.remove(&account.key);
        Ok(())
    }

    fn account_for(&self, book: &Book) -> Result<Account, String> {
        self.accounts()
            .into_iter()
            .find(|account| account.key == book.account)
            .ok_or_else(|| "this address book's account is no longer in Sealwax".to_owned())
    }

    fn book(&self, id: i64) -> Result<Book, String> {
        self.store.book(id).map_err(|e| e.to_string())?.ok_or_else(|| "the address book is gone".to_owned())
    }

    fn stored(&self, id: i64) -> Result<StoredCard, String> {
        self.store.card(id).map_err(|e| e.to_string())?.ok_or_else(|| "the contact is gone".to_owned())
    }

    /// Adds a new card to a book; gives its id.
    pub async fn create(&self, book: i64, uid: &str, vcf: &str) -> Result<i64, String> {
        let book = self.book(book)?;
        if book.read_only {
            return Err(format!("{} is read-only", book.name));
        }
        let name: String = uid.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        let name = if name.is_empty() { crate::contact::new_uid() } else { name };
        if book.is_local() {
            let url = format!("{}/{name}.vcf", book.url);
            return self.store.put_card(book.id, &url, None, vcf).map_err(|e| e.to_string());
        }
        let account = self.account_for(&book)?;
        let client = self.client(&account).await?;
        let base = Url::parse(&book.url).map_err(|e| e.to_string())?;
        let url = base.join(&format!("{name}.vcf")).map_err(|e| e.to_string())?;
        let etag = client.put(&url, vcf, Condition::Create).await.map_err(write_error)?;
        let _ = self.store.touch(book.id);
        self.store.put_card(book.id, url.as_str(), etag.as_deref(), vcf).map_err(|e| e.to_string())
    }

    /// Replaces a card, refusing if it changed on the server meanwhile.
    pub async fn update(&self, card: i64, vcf: &str) -> Result<(), String> {
        let stored = self.stored(card)?;
        let book = self.book(stored.book)?;
        if book.read_only {
            return Err(format!("{} is read-only", book.name));
        }
        let mut etag = None;
        if !book.is_local() {
            let account = self.account_for(&book)?;
            let client = self.client(&account).await?;
            let url = Url::parse(&stored.url).map_err(|e| e.to_string())?;
            let condition = stored.etag.as_deref().map_or(Condition::Any, Condition::Match);
            etag = client.put(&url, vcf, condition).await.map_err(write_error)?;
            let _ = self.store.touch(book.id);
        }
        self.store
            .put_card(book.id, &stored.url, etag.as_deref(), vcf)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    pub async fn delete(&self, card: i64) -> Result<(), String> {
        let stored = self.stored(card)?;
        let book = self.book(stored.book)?;
        if book.read_only {
            return Err(format!("{} is read-only", book.name));
        }
        if !book.is_local() {
            let account = self.account_for(&book)?;
            let client = self.client(&account).await?;
            let url = Url::parse(&stored.url).map_err(|e| e.to_string())?;
            client.delete(&url, stored.etag.as_deref()).await.map_err(write_error)?;
            let _ = self.store.touch(book.id);
        }
        self.store.delete_card(card).map_err(|e| e.to_string())
    }

    /// Moves a card to another book: written there first, then removed here. Gives its new id.
    pub async fn move_to(&self, card: i64, book: i64) -> Result<i64, String> {
        let stored = self.stored(card)?;
        let mut parsed = vcard::parse(&stored.vcf).map_err(|e| e.to_string())?;
        let uid = crate::contact::ensure_uid(&mut parsed);
        let id = self.create(book, &uid, &parsed.to_vcf()).await?;
        self.delete(card).await?;
        Ok(id)
    }

    /// Adds every card in a vCard file to a book, each with a UID; gives how many were added,
    /// and the first failure, if any.
    pub async fn import(&self, book: i64, text: &str) -> Result<(usize, Option<String>), String> {
        let cards = vcard::parse_all(text).map_err(|e| format!("the file isn't a vCard file: {e}"))?;
        let mut added = 0;
        let mut failure = None;
        for mut card in cards {
            let uid = crate::contact::ensure_uid(&mut card);
            match self.create(book, &uid, &card.to_vcf()).await {
                Ok(_) => added += 1,
                Err(error) => {
                    tracing::warn!("a card wasn't imported: {error}");
                    failure.get_or_insert(error);
                }
            }
        }
        Ok((added, failure))
    }
}

fn unauthorized() -> String {
    "the server didn't accept the password Sealwax has for this account (some providers need \
     an app password for contacts too)"
        .to_owned()
}

fn write_error(error: carddav::Error) -> String {
    match error {
        carddav::Error::Network(_) => format!("{error}; the change wasn't saved"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact::{Draft, new_card};

    #[tokio::test]
    async fn the_local_book_needs_no_server() {
        let service = Service::with(Store::in_memory().unwrap(), Config::default());
        let local = service.store.books().unwrap()[0].clone();
        let draft = Draft { given: "Ada".into(), ..Draft::default() };
        let vcf = new_card("u1", &draft).to_vcf();
        let id = service.create(local.id, "u1", &vcf).await.unwrap();
        service.update(id, &vcf.replace("Ada", "Augusta")).await.unwrap();
        assert!(service.store.cards().unwrap()[0].vcf.contains("Augusta"));
        service.delete(id).await.unwrap();
        assert!(service.store.cards().unwrap().is_empty());

        let file = "BEGIN:VCARD\nVERSION:3.0\nFN:One\nEND:VCARD\nBEGIN:VCARD\nVERSION:3.0\nFN:Two\nUID:two\nEND:VCARD\n";
        assert_eq!(service.import(local.id, file).await.unwrap(), (2, None));
        let cards = service.store.cards().unwrap();
        assert!(cards.iter().all(|card| card.vcf.contains("UID:")), "imported cards get a UID");
        assert!(service.import(local.id, "not a card").await.is_err());
    }
}
