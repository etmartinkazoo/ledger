//! Exercises the CardDAV client against a real server: discovery, the address book list, a card
//! created, listed, fetched, changed and removed, then the refresh that copies a book into the
//! store, as cards are added, changed and removed. For a test server only; it writes to the first
//! address book it finds.
//!
//! `LEDGER_TEST_ROOTS=cert.pem cargo run -p ledger-core --example carddav_live -- URL USER PASSWORD`

use ledger_core::accounts::Config;
use ledger_core::carddav::{Client, Condition, Credentials, Error};
use ledger_core::contact::{self, Contact, Draft, Field};
use ledger_core::service::Service;
use ledger_core::store::Store;
use ledger_core::vcard;
use zeroize::Zeroizing;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [url, user, password] = args.as_slice() else {
        anyhow::bail!("usage: carddav_live URL USER PASSWORD");
    };
    let client = Client::new(Credentials::Password { username: user.clone(), password: Zeroizing::new(password.clone()) })?;
    let home = client.discover(&url.parse()?).await?;
    println!("home: {home}");
    let books = client.books(&home).await?;
    for book in &books {
        println!("book: {} ctag={:?} ro={}", book.name, book.ctag, book.read_only);
    }
    let book = books.first().ok_or_else(|| anyhow::anyhow!("no address book"))?;

    let draft = Draft {
        given: "Ledger".into(),
        family: "Live Check".into(),
        phones: vec![Field::new("mobile", "+44 20 7946 0000"), Field::new("Lab", "+44 1")],
        note: "first line\nsecond, line".into(),
        ..Draft::default()
    };
    let uid = contact::new_uid();
    let vcf = contact::new_card(&uid, &draft).to_vcf();
    let url = book.url.join(&format!("{uid}.vcf"))?;
    let etag = client.put(&url, &vcf, Condition::Create).await?;
    println!("created, etag {etag:?}");
    assert!(client.put(&url, &vcf, Condition::Create).await.is_err(), "a second create must fail");

    let listed = client.etags(&book.url).await?;
    let (_, listed_etag) = listed.iter().find(|(u, _)| *u == url).ok_or_else(|| anyhow::anyhow!("created card not listed"))?;
    println!("listed with etag {listed_etag:?} among {}", listed.len());
    let fetched = client.multiget(&book.url, std::slice::from_ref(&url)).await?;
    let ours = fetched.first().ok_or_else(|| anyhow::anyhow!("multiget found nothing"))?;
    let shown = Contact::from_card(&vcard::parse(&ours.vcf)?);
    assert_eq!(shown.display_name(), "Ledger Live Check");
    assert_eq!(shown.note, "first line\nsecond, line");
    assert_eq!(shown.phones[1], Field::new("Lab", "+44 1"), "a custom label survives the server");
    let single = client.get(&url).await?;
    assert_eq!(vcard::parse(&single.vcf)?, vcard::parse(&ours.vcf)?);
    println!("read back: {}", shown.display_name());

    let mut card = vcard::parse(&ours.vcf)?;
    contact::apply(&mut card, &Draft { title: "Tester".into(), ..Draft::of(&shown) });
    let current = ours.etag.clone().or(etag).ok_or_else(|| anyhow::anyhow!("no etag"))?;
    assert!(
        matches!(client.put(&url, &card.to_vcf(), Condition::Match("\"stale\"")).await, Err(Error::Changed)),
        "a stale etag must be refused"
    );
    let new_etag = client.put(&url, &card.to_vcf(), Condition::Match(&current)).await?;
    println!("updated, etag {new_etag:?}");
    let after = client.etags(&book.url).await?;
    let changed = after.iter().find(|(u, _)| *u == url).and_then(|(_, e)| e.clone());
    assert_ne!(changed.as_ref(), listed_etag.as_ref(), "the etag changes with the card");

    client.delete(&url, changed.as_deref()).await?;
    let gone = client.etags(&book.url).await?;
    assert!(gone.iter().all(|(u, _)| *u != url), "deleted card still listed");
    println!("deleted");

    // The refresh: three cards copied into an empty store, then one changed and one removed.
    let service = Service::with(Store::in_memory()?, Config::default());
    let id = service.store.upsert_book("test", book.url.as_str(), &book.name, false)?;
    let mut urls = Vec::new();
    for name in ["Ada", "Charles", "Mary"] {
        let uid = contact::new_uid();
        let url = book.url.join(&format!("{uid}.vcf"))?;
        client.put(&url, &contact::new_card(&uid, &Draft { given: name.into(), ..Draft::default() }).to_vcf(), Condition::Create).await?;
        urls.push(url);
    }
    service.sync_book(&client, id, &book.url).await?;
    let names = |service: &Service| -> anyhow::Result<Vec<String>> {
        let mut names: Vec<String> = service
            .store
            .cards()?
            .iter()
            .map(|card| Ok(Contact::from_card(&vcard::parse(&card.vcf)?).display_name()))
            .collect::<anyhow::Result<_>>()?;
        names.sort();
        Ok(names)
    };
    assert_eq!(names(&service)?, ["Ada", "Charles", "Mary"]);
    let fetched = client.get(&urls[1]).await?;
    let mut card = vcard::parse(&fetched.vcf)?;
    contact::apply(&mut card, &Draft { given: "Charles".into(), family: "Babbage".into(), ..Draft::default() });
    client.put(&urls[1], &card.to_vcf(), Condition::Match(fetched.etag.as_deref().unwrap_or("*"))).await?;
    client.delete(&urls[2], None).await?;
    service.sync_book(&client, id, &book.url).await?;
    assert_eq!(names(&service)?, ["Ada", "Charles Babbage"]);
    println!("refresh: added, changed and removed cards follow the server");
    for url in &urls[..2] {
        client.delete(url, None).await?;
    }
    println!("all checks passed");
    Ok(())
}
