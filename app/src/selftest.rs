//! A scripted run through the real window, for checking a build without a person at the
//! keyboard: `LEDGER_SELFTEST=<dir>` adds a contact through the editor, imports a vCard file,
//! edits a contact with a photo, searches and deletes, saves a PNG of the window after each step
//! into `<dir>`, writes `report.txt` there and quits. It writes to the address books it finds:
//! point `XDG_CONFIG_HOME` and `XDG_DATA_HOME` at empty directories, so only the local one is.

use crate::window::Window;
use gtk4::{self as gtk, glib, prelude::*};
use ledger_core::contact::{Address, Draft, Field};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const ENV: &str = "LEDGER_SELFTEST";

/// A 48 px picture, folded for a vCard line.
const PHOTO: &str = include_str!("selftest-photo.b64");

pub fn start(window: &Rc<Window>, dir: PathBuf) {
    // Dialogs open at once, so a screenshot never catches one on its way in.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(false);
    }
    let window = Rc::clone(window);
    glib::spawn_future_local(async move {
        let mut report = Vec::new();
        let outcome = script(&window, &dir, &mut report).await;
        report.push(match outcome {
            Ok(()) => "PASS".to_owned(),
            Err(error) => format!("FAIL: {error}"),
        });
        let _ = std::fs::write(dir.join("report.txt"), report.join("\n") + "\n");
        if let Some(app) = window.window.application() {
            app.quit();
        }
    });
}

async fn pause(ms: u64) {
    glib::timeout_future(std::time::Duration::from_millis(ms)).await;
}

async fn settle(window: &Window) -> Result<(), String> {
    for _ in 0..100 {
        pause(100).await;
        if !window.is_busy() {
            pause(300).await;
            return Ok(());
        }
    }
    Err("timed out waiting for a write".to_owned())
}

/// Saves the window as it is drawn, background and any dialog included.
async fn shot(window: &Window, dir: &Path, name: &str) {
    pause(300).await;
    let widget = window.window.upcast_ref::<gtk::Widget>();
    save(widget, widget.width(), widget.height(), dir, name).await;
}

/// Saves a widget drawn at a size, laid out first: Broadway with no browser watching stops the
/// frame clock that would lay out a dialog.
async fn save(widget: &gtk::Widget, width: i32, height: i32, dir: &Path, name: &str) {
    if widget.parent().is_some() {
        widget.allocate(width, height, -1, None);
    }
    for _ in 0..40 {
        let paintable = gtk::WidgetPaintable::new(Some(widget));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
        let renderer = widget.native().and_then(|native| native.renderer());
        if let (Some(node), Some(renderer)) = (snapshot.to_node(), renderer) {
            let texture = renderer.render_texture(&node, None);
            if let Err(error) = texture.save_to_png(dir.join(format!("{name}.png"))) {
                eprintln!("selftest: {name}: {error}");
            }
            return;
        }
        pause(50).await;
    }
    eprintln!("selftest: {name}: nothing drawn");
}

fn check(report: &mut Vec<String>, step: &str, ok: bool, detail: impl std::fmt::Display) -> Result<(), String> {
    report.push(format!("{} {step}: {detail}", if ok { "ok " } else { "BAD" }));
    if ok { Ok(()) } else { Err(format!("{step}: {detail}")) }
}

fn names(window: &Window) -> Vec<String> {
    window.listed().iter().map(|entry| entry.contact.display_name()).collect()
}

fn stored(window: &Window, card: i64) -> String {
    window.service.store.card(card).ok().flatten().map(|card| card.vcf).unwrap_or_default()
}

async fn script(window: &Rc<Window>, dir: &Path, report: &mut Vec<String>) -> Result<(), String> {
    settle(window).await?;
    shot(window, dir, "1-start").await;

    // A contact through the editor, as a person fills it in.
    let editor = window.new_contact();
    editor.fill(&Draft {
        given: "Ada".into(),
        family: "Lovelace".into(),
        organization: "Analytical Engines".into(),
        title: "Programmer".into(),
        phones: vec![Field::new("mobile", "+44 20 7946 0000"), Field::new("Lab", "+44 20 7946 0001")],
        emails: vec![Field::new("home", "ada@example.com")],
        addresses: vec![Address {
            label: "home".into(),
            street: "12 St James's Square".into(),
            city: "London".into(),
            postcode: "SW1Y 4JH".into(),
            country: "United Kingdom".into(),
            ..Address::default()
        }],
        websites: vec![Field::new("homepage", "ada.example")],
        birthday: "1815-12-10".into(),
        note: "Wrote the first program, in 1843.".into(),
        ..Draft::default()
    });
    if let Some(content) = libadwaita::prelude::AdwDialogExt::child(&editor.dialog) {
        save(&content, 520, 900, dir, "2-editor").await;
    }
    editor.save();
    settle(window).await?;
    let ada = window.current_entry().ok_or("no contact after saving")?;
    let vcf = stored(window, ada.card);
    check(report, "new contact saved", ada.contact.display_name() == "Ada Lovelace", ada.contact.display_name())?;
    check(report, "custom label kept", vcf.contains("X-ABLABEL:Lab"), "")?;
    check(report, "birthday written", vcf.contains("BDAY:1815-12-10"), "")?;
    shot(window, dir, "3-contact").await;

    // A vCard file: a photo, a company, and one without a family name.
    let local = window.service.store.books().map_err(|e| e.to_string())?[0].id;
    let file = format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Babbage;Charles;;;\r\nFN:Charles Babbage\r\nTEL;TYPE=WORK:+44 20 7946 0100\r\n\
         PHOTO;ENCODING=b;TYPE=PNG:{PHOTO}\r\nX-CUSTOM:kept\r\nEND:VCARD\r\n\
         BEGIN:VCARD\r\nVERSION:3.0\r\nN:;;;;\r\nFN:Analytical Engines Ltd\r\nORG:Analytical Engines Ltd;\r\nX-ABShowAs:COMPANY\r\nEND:VCARD\r\n\
         BEGIN:VCARD\r\nVERSION:3.0\r\nN:;Mary;;;\r\nFN:Mary\r\nEMAIL:mary@example.org\r\nEND:VCARD\r\n"
    );
    window.import_text(local, file);
    settle(window).await?;
    check(
        report,
        "imported and sorted by family name",
        names(window) == ["Analytical Engines Ltd", "Charles Babbage", "Ada Lovelace", "Mary"],
        format!("{:?}", names(window)),
    )?;
    shot(window, dir, "4-imported").await;

    // An edit keeps what the editor doesn't show.
    let babbage = window.listed().into_iter().find(|e| e.contact.family == "Babbage").ok_or("no Babbage")?;
    window.select(babbage.card);
    let editor = window.edit_contact().ok_or("couldn't edit")?;
    let mut draft = editor.draft();
    draft.title = "Mathematician".into();
    draft.emails.push(Field::new("work", "charles@engines.example"));
    editor.fill(&draft);
    editor.save();
    settle(window).await?;
    let vcf = stored(window, babbage.card);
    check(report, "edit saved", vcf.contains("TITLE:Mathematician") && vcf.contains("charles@engines.example"), "")?;
    check(report, "photo and unknown lines kept", vcf.contains("PHOTO;ENCODING=b;TYPE=PNG:") && vcf.contains("X-CUSTOM:kept"), "")?;
    shot(window, dir, "5-photo").await;

    window.search.set_text("engines");
    pause(600).await;
    check(report, "search", names(window) == ["Analytical Engines Ltd", "Charles Babbage", "Ada Lovelace"], format!("{:?}", names(window)))?;
    window.search.set_text("7946 0001");
    pause(600).await;
    check(report, "search by number", names(window) == ["Ada Lovelace"], format!("{:?}", names(window)))?;
    shot(window, dir, "6-search").await;
    window.search.set_text("");
    pause(400).await;

    window.delete_now(ada.card);
    settle(window).await?;
    check(report, "deleted", !names(window).contains(&"Ada Lovelace".to_owned()), format!("{:?}", names(window)))?;
    Ok(())
}
