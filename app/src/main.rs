//! Ledger: contacts for Omarchy that share their accounts with Sealwax.

mod editor;
mod runtime;
mod selftest;
mod theme;
mod window;

use gtk4::{glib, prelude::*};
use ledger_core::service::Service;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// Debug builds use `.Devel`, so they run beside an installed build.
const APP_ID: &str = if cfg!(debug_assertions) { "io.brem.Ledger.Devel" } else { "io.brem.Ledger" };

thread_local! {
    static WINDOW: RefCell<Option<Rc<window::Window>>> = const { RefCell::new(None) };
}

/// The window, made on first use.
fn window(app: &adw::Application) -> Option<Rc<window::Window>> {
    if let Some(window) = WINDOW.with(|w| w.borrow().clone()) {
        return Some(window);
    }
    match Service::open() {
        Ok((service, warnings)) => {
            let window = window::Window::new(app, Arc::new(service), warnings);
            if let Some(dir) = std::env::var_os(selftest::ENV) {
                selftest::start(&window, dir.into());
            }
            WINDOW.with(|w| *w.borrow_mut() = Some(Rc::clone(&window)));
            Some(window)
        }
        Err(error) => {
            eprintln!("ledger: {error}");
            app.quit();
            None
        }
    }
}

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LEDGER_LOG").unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.set_accels_for_action("win.new", &["<Control>n"]);
    app.set_accels_for_action("win.edit", &["<Control>e"]);
    app.set_accels_for_action("win.search", &["<Control>f"]);
    app.set_accels_for_action("win.refresh", &["<Control>r", "F5"]);
    app.set_accels_for_action("win.import", &["<Control>i"]);
    app.set_accels_for_action("window.close", &["<Control>w", "<Control>q"]);
    app.connect_activate(|app| {
        if let Some(window) = window(app) {
            window.window.present();
        }
    });
    app.run()
}
