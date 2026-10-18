//! The offline copy, in `$XDG_DATA_HOME/ledger/ledger.db`: each account's address book home, its
//! address books, and the vCard text of their cards as the server last sent it, with ETags.
//! Ledger shows this copy at once and refreshes it in the background. The local address book
//! lives only here.

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// The account key of the address book kept only on this computer.
pub const LOCAL_ACCOUNT: &str = "local";
const LOCAL_URL: &str = "local:contacts";

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS account (
    key TEXT PRIMARY KEY,
    home TEXT
);
CREATE TABLE IF NOT EXISTS book (
    id INTEGER PRIMARY KEY,
    account TEXT NOT NULL,
    url TEXT NOT NULL,
    name TEXT NOT NULL,
    ctag TEXT,
    read_only INTEGER NOT NULL DEFAULT 0,
    UNIQUE (account, url)
);
CREATE TABLE IF NOT EXISTS card (
    id INTEGER PRIMARY KEY,
    book INTEGER NOT NULL REFERENCES book (id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    etag TEXT,
    vcf TEXT NOT NULL,
    UNIQUE (book, url)
);
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    pub id: i64,
    pub account: String,
    pub url: String,
    pub name: String,
    pub ctag: Option<String>,
    pub read_only: bool,
}

impl Book {
    pub fn is_local(&self) -> bool {
        self.account == LOCAL_ACCOUNT
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCard {
    pub id: i64,
    pub book: i64,
    pub url: String,
    pub etag: Option<String>,
    pub vcf: String,
}

#[derive(Debug, thiserror::Error)]
#[error("the contact store: {0}")]
pub struct StoreError(#[from] rusqlite::Error);

pub struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::with(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, StoreError> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(connection: Connection) -> Result<Self, StoreError> {
        connection.execute_batch(SCHEMA)?;
        connection.execute(
            "INSERT OR IGNORE INTO book (account, url, name) VALUES (?1, ?2, 'Contacts')",
            params![LOCAL_ACCOUNT, LOCAL_URL],
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.connection.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The local book first, then each account's by name.
    pub fn books(&self) -> Result<Vec<Book>, StoreError> {
        let db = self.db();
        let mut statement = db.prepare(
            "SELECT id, account, url, name, ctag, read_only FROM book
             ORDER BY account = 'local' DESC, account, name COLLATE NOCASE, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Book {
                id: row.get(0)?,
                account: row.get(1)?,
                url: row.get(2)?,
                name: row.get(3)?,
                ctag: row.get(4)?,
                read_only: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn book(&self, id: i64) -> Result<Option<Book>, StoreError> {
        Ok(self.books()?.into_iter().find(|book| book.id == id))
    }

    pub fn home(&self, account: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .db()
            .query_row("SELECT home FROM account WHERE key = ?1", [account], |row| row.get(0))
            .optional()?
            .flatten())
    }

    pub fn set_home(&self, account: &str, home: Option<&str>) -> Result<(), StoreError> {
        self.db().execute(
            "INSERT INTO account (key, home) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET home = excluded.home",
            params![account, home],
        )?;
        Ok(())
    }

    /// Adds a server's address book or refreshes its name and access; its tag is set apart, once
    /// its cards are read.
    pub fn upsert_book(&self, account: &str, url: &str, name: &str, read_only: bool) -> Result<i64, StoreError> {
        let db = self.db();
        db.execute(
            "INSERT INTO book (account, url, name, read_only) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account, url) DO UPDATE SET name = excluded.name, read_only = excluded.read_only",
            params![account, url, name, read_only],
        )?;
        Ok(db.query_row(
            "SELECT id FROM book WHERE account = ?1 AND url = ?2",
            params![account, url],
            |row| row.get(0),
        )?)
    }

    pub fn set_ctag(&self, book: i64, ctag: Option<&str>) -> Result<(), StoreError> {
        self.db().execute("UPDATE book SET ctag = ?2 WHERE id = ?1", params![book, ctag])?;
        Ok(())
    }

    /// Forgets an account's address books that are no longer on the server.
    pub fn retain_books(&self, account: &str, keep: &[i64]) -> Result<(), StoreError> {
        let db = self.db();
        let mut statement = db.prepare("SELECT id FROM book WHERE account = ?1")?;
        let ids: Vec<i64> = statement.query_map([account], |row| row.get(0))?.collect::<Result<_, _>>()?;
        for id in ids.into_iter().filter(|id| !keep.contains(id)) {
            db.execute("DELETE FROM book WHERE id = ?1", [id])?;
        }
        Ok(())
    }

    /// Forgets the address books of accounts that are gone from Sealwax.
    pub fn retain_accounts(&self, keep: &[String]) -> Result<(), StoreError> {
        let db = self.db();
        let mut statement = db.prepare("SELECT DISTINCT account FROM book WHERE account != ?1")?;
        let accounts: Vec<String> = statement.query_map([LOCAL_ACCOUNT], |row| row.get(0))?.collect::<Result<_, _>>()?;
        for account in accounts.into_iter().filter(|account| !keep.contains(account)) {
            db.execute("DELETE FROM book WHERE account = ?1", [&account])?;
            db.execute("DELETE FROM account WHERE key = ?1", [&account])?;
        }
        Ok(())
    }

    /// A book's cards by address: their id and ETag.
    pub fn card_index(&self, book: i64) -> Result<HashMap<String, (i64, Option<String>)>, StoreError> {
        let db = self.db();
        let mut statement = db.prepare("SELECT url, id, etag FROM card WHERE book = ?1")?;
        let rows = statement.query_map([book], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Adds or replaces a card; gives its id.
    pub fn put_card(&self, book: i64, url: &str, etag: Option<&str>, vcf: &str) -> Result<i64, StoreError> {
        let db = self.db();
        db.execute(
            "INSERT INTO card (book, url, etag, vcf) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (book, url) DO UPDATE SET etag = excluded.etag, vcf = excluded.vcf",
            params![book, url, etag, vcf],
        )?;
        Ok(db.query_row("SELECT id FROM card WHERE book = ?1 AND url = ?2", params![book, url], |row| row.get(0))?)
    }

    /// Marks a server book changed, so the next refresh reads it whatever its tag says.
    pub fn touch(&self, book: i64) -> Result<(), StoreError> {
        self.db()
            .execute("UPDATE book SET ctag = NULL WHERE id = ?1 AND account != ?2", params![book, LOCAL_ACCOUNT])?;
        Ok(())
    }

    pub fn delete_card(&self, id: i64) -> Result<(), StoreError> {
        self.db().execute("DELETE FROM card WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn card(&self, id: i64) -> Result<Option<StoredCard>, StoreError> {
        Ok(self
            .db()
            .query_row("SELECT id, book, url, etag, vcf FROM card WHERE id = ?1", [id], card_row)
            .optional()?)
    }

    pub fn cards(&self) -> Result<Vec<StoredCard>, StoreError> {
        let db = self.db();
        let mut statement = db.prepare("SELECT id, book, url, etag, vcf FROM card")?;
        let rows = statement.query_map([], card_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn card_row(row: &rusqlite::Row) -> rusqlite::Result<StoredCard> {
    Ok(StoredCard {
        id: row.get(0)?,
        book: row.get(1)?,
        url: row.get(2)?,
        etag: row.get(3)?,
        vcf: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_books_and_their_cards() {
        let store = Store::in_memory().unwrap();
        let local = store.books().unwrap();
        assert_eq!(local.len(), 1);
        assert!(local[0].is_local());

        let work = store.upsert_book("work", "https://dav/w/", "Work", false).unwrap();
        assert_eq!(store.upsert_book("work", "https://dav/w/", "Work!", true).unwrap(), work);
        let stored = store.book(work).unwrap().unwrap();
        assert_eq!((stored.name.as_str(), stored.read_only), ("Work!", true));

        store.set_ctag(work, Some("7")).unwrap();
        let a = store.put_card(work, "https://dav/w/a.vcf", Some("\"1\""), "A").unwrap();
        assert_eq!(store.put_card(work, "https://dav/w/a.vcf", Some("\"2\""), "A2").unwrap(), a);
        assert_eq!(store.card_index(work).unwrap()["https://dav/w/a.vcf"], (a, Some("\"2\"".to_owned())));
        store.touch(work).unwrap();
        assert_eq!(store.book(work).unwrap().unwrap().ctag, None);
        store.touch(local[0].id).unwrap();

        store.retain_books("work", &[]).unwrap();
        assert!(store.cards().unwrap().is_empty(), "cards go with their book");

        store.set_home("work", Some("https://dav/")).unwrap();
        assert_eq!(store.home("work").unwrap().as_deref(), Some("https://dav/"));
        store.upsert_book("gone", "https://x/", "X", false).unwrap();
        store.retain_accounts(&["work".to_owned()]).unwrap();
        assert_eq!(store.books().unwrap().len(), 1);
    }
}
