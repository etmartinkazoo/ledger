//! The main window: address books in the sidebar, their contacts from A to Z, and the contact
//! chosen. Changes go to the server first and show once it has them; a refresh runs at start,
//! every five minutes and on Ctrl+R.

use crate::editor::{Editor, Request};
use crate::runtime::run;
use gtk4::{self as gtk, gdk, gio, glib, prelude::*};
use ledger_core::contact::{self, Contact, Draft, Kind};
use ledger_core::service::{Problem, Service};
use ledger_core::store::Book;
use ledger_core::vcard;
use libadwaita::{self as adw, prelude::*};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// How often contacts are refreshed while the window is open.
const REFRESH_EVERY: u32 = 300;

/// What each sidebar row shows (`None`: every book), and the account it's under.
type BookRows = Rc<RefCell<Vec<(Option<i64>, String)>>>;

#[derive(Clone)]
pub(crate) struct Entry {
    pub card: i64,
    pub book: i64,
    pub contact: Contact,
}

pub struct Window {
    pub window: adw::ApplicationWindow,
    pub(crate) service: Arc<Service>,
    book_list: gtk::ListBox,
    book_rows: BookRows,
    contact_list: gtk::ListBox,
    /// Every contact, and the ones listed, by row index, with each one's letter.
    entries: RefCell<Vec<Entry>>,
    listed: RefCell<Vec<Entry>>,
    sections: Rc<RefCell<Vec<String>>>,
    books: RefCell<Vec<Book>>,
    outer: adw::OverlaySplitView,
    inner: adw::NavigationSplitView,
    list_page: adw::NavigationPage,
    pub(crate) search: gtk::SearchEntry,
    list_stack: gtk::Stack,
    list_empty: adw::StatusPage,
    detail_stack: gtk::Stack,
    detail: gtk::Box,
    edit: gtk::Button,
    delete: gtk::Button,
    more: gtk::MenuButton,
    refresh: gtk::Button,
    spinner: adw::Spinner,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    view: Cell<Option<i64>>,
    current: Cell<Option<i64>>,
    problems: RefCell<Vec<Problem>>,
    refreshing: Cell<bool>,
    writes: Cell<usize>,
    /// The book the last new contact went in, offered first next time.
    last_book: Cell<Option<i64>>,
    /// Set while the window itself changes the lists, so their signals don't act.
    quiet: Cell<bool>,
}

impl Window {
    pub fn new(app: &adw::Application, service: Arc<Service>, warnings: Vec<String>) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Ledger")
            .default_width(1100)
            .default_height(720)
            .width_request(360)
            .height_request(320)
            .build();

        // Address books.
        let book_list = gtk::ListBox::new();
        book_list.add_css_class("navigation-sidebar");
        let books_view = adw::ToolbarView::new();
        books_view.add_top_bar(
            &adw::HeaderBar::builder()
                .show_end_title_buttons(false)
                .title_widget(&adw::WindowTitle::new("Ledger", ""))
                .build(),
        );
        books_view.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&book_list)
                .vexpand(true)
                .build(),
        ));

        // The contact list.
        let contact_list = gtk::ListBox::new();
        contact_list.add_css_class("navigation-sidebar");
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search")
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(6)
            .build();
        let list_empty = adw::StatusPage::builder().icon_name("system-users-symbolic").build();
        list_empty.add_css_class("compact");
        let list_stack = gtk::Stack::new();
        list_stack.add_named(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&contact_list)
                .vexpand(true)
                .build(),
            Some("contacts"),
        );
        list_stack.add_named(&list_empty, Some("empty"));
        let toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Show Address Books")
            .active(true)
            .build();
        let add = gtk::Button::builder()
            .icon_name("contact-new-symbolic")
            .tooltip_text("New Contact (Ctrl+N)")
            .action_name("win.new")
            .build();
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh (Ctrl+R)")
            .action_name("win.refresh")
            .build();
        let spinner = adw::Spinner::builder().visible(false).margin_start(8).margin_end(8).build();
        let list_menu = gio::Menu::new();
        list_menu.append(Some("Import vCards…"), Some("win.import"));
        list_menu.append(Some("Export These Contacts…"), Some("win.export-all"));
        let list_more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More")
            .menu_model(&list_menu)
            .build();
        let list_header = adw::HeaderBar::builder().show_end_title_buttons(false).build();
        list_header.pack_start(&toggle);
        list_header.pack_end(&add);
        list_header.pack_end(&list_more);
        list_header.pack_end(&refresh);
        list_header.pack_end(&spinner);
        let list_view = adw::ToolbarView::new();
        list_view.add_top_bar(&list_header);
        list_view.add_top_bar(&search);
        list_view.set_content(Some(&list_stack));
        let list_page = adw::NavigationPage::builder().title("All Contacts").tag("list").child(&list_view).build();

        // The contact.
        let detail = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(24)
            .margin_bottom(32)
            .margin_start(12)
            .margin_end(12)
            .build();
        let clamp = adw::Clamp::builder().maximum_size(560).child(&detail).build();
        let detail_stack = gtk::Stack::new();
        detail_stack.add_named(
            &gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build(),
            Some("contact"),
        );
        detail_stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("avatar-default-symbolic")
                .title("No Contact Selected")
                .description("Press Ctrl+N for a new contact")
                .build(),
            Some("empty"),
        );
        let edit = gtk::Button::builder().label("Edit").tooltip_text("Edit (Ctrl+E)").action_name("win.edit").build();
        let delete = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Delete Contact").action_name("win.delete").build();
        let more = gtk::MenuButton::builder().icon_name("view-more-symbolic").tooltip_text("More").build();
        let detail_header = adw::HeaderBar::builder().title_widget(&adw::WindowTitle::new("", "")).build();
        detail_header.pack_end(&edit);
        detail_header.pack_end(&more);
        detail_header.pack_end(&delete);
        let detail_view = adw::ToolbarView::new();
        detail_view.add_top_bar(&detail_header);
        detail_view.set_content(Some(&detail_stack));
        let detail_page = adw::NavigationPage::builder().title("Contact").tag("contact").child(&detail_view).build();

        let inner = adw::NavigationSplitView::builder()
            .sidebar(&list_page)
            .content(&detail_page)
            .min_sidebar_width(280.0)
            .max_sidebar_width(360.0)
            .sidebar_width_fraction(0.35)
            .build();
        let banner = adw::Banner::builder().button_label("Details").build();
        let content = adw::ToolbarView::new();
        content.add_top_bar(&banner);
        content.set_content(Some(&inner));
        let outer = adw::OverlaySplitView::builder()
            .sidebar(&books_view)
            .content(&content)
            .min_sidebar_width(200.0)
            .max_sidebar_width(260.0)
            .build();
        toggle.bind_property("active", &outer, "show-sidebar").bidirectional().sync_create().build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&outer));
        window.set_content(Some(&toasts));
        let breakpoint = |width: f64| {
            adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                width,
                adw::LengthUnit::Sp,
            ))
        };
        // Narrower: the books slide over the list; narrower still, one pane at a time.
        let narrow = breakpoint(900.0);
        narrow.add_setter(&outer, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&outer, "show-sidebar", Some(&false.to_value()));
        window.add_breakpoint(narrow);
        let phone = breakpoint(560.0);
        phone.add_setter(&outer, "collapsed", Some(&true.to_value()));
        phone.add_setter(&outer, "show-sidebar", Some(&false.to_value()));
        phone.add_setter(&inner, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(phone);

        let this = Rc::new(Self {
            window,
            service,
            book_list,
            book_rows: Rc::default(),
            contact_list,
            entries: RefCell::default(),
            listed: RefCell::default(),
            sections: Rc::default(),
            books: RefCell::default(),
            outer,
            inner,
            list_page,
            search,
            list_stack,
            list_empty,
            detail_stack,
            detail,
            edit,
            delete,
            more,
            refresh,
            spinner,
            banner,
            toasts,
            view: Cell::new(None),
            current: Cell::new(None),
            problems: RefCell::default(),
            refreshing: Cell::new(false),
            writes: Cell::new(0),
            last_book: Cell::new(None),
            quiet: Cell::new(false),
        });
        this.connect(&this);
        crate::theme::follow();
        this.reload();
        for warning in warnings {
            this.toast(&warning);
        }
        this.refresh_now();
        let weak = Rc::downgrade(&this);
        glib::timeout_add_seconds_local(REFRESH_EVERY, move || match weak.upgrade() {
            Some(this) => {
                this.refresh_now();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        this
    }

    fn connect(&self, this: &Rc<Self>) {
        let action = |name: &str, run: fn(&Rc<Self>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(this);
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    run(&this);
                }
            });
            self.window.add_action(&action);
        };
        action("new", |this| drop(this.new_contact()));
        action("edit", |this| drop(this.edit_contact()));
        action("delete", |this| this.delete_contact());
        action("refresh", |this| this.refresh_now());
        action("import", |this| this.import());
        action("export-all", |this| {
            let cards: Vec<i64> = this.listed.borrow().iter().map(|entry| entry.card).collect();
            this.export(cards, "Contacts.vcf");
        });
        action("export-contact", |this| {
            if let Some(entry) = this.current_entry() {
                this.export(vec![entry.card], &format!("{}.vcf", entry.contact.display_name().replace('/', "-")));
            }
        });
        action("search", |this| {
            this.inner.set_show_content(false);
            this.search.grab_focus();
        });
        let move_to = gio::SimpleAction::new("move-to", Some(glib::VariantTy::INT64));
        let weak = Rc::downgrade(this);
        move_to.connect_activate(move |_, target| {
            if let (Some(this), Some(book)) = (weak.upgrade(), target.and_then(|t| t.get::<i64>())) {
                this.move_contact(book);
            }
        });
        self.window.add_action(&move_to);

        let weak = Rc::downgrade(this);
        self.book_list.connect_row_selected(move |_, row| {
            let Some(this) = weak.upgrade().filter(|this| !this.quiet.get()) else { return };
            let Some((view, _)) = row.and_then(|row| this.book_rows.borrow().get(row.index() as usize).cloned()) else { return };
            this.view.set(view);
            this.reload_list();
            this.keep_current_listed();
        });
        let weak = Rc::downgrade(this);
        self.book_list.connect_row_activated(move |_, _| {
            if let Some(this) = weak.upgrade().filter(|this| this.outer.is_collapsed()) {
                this.outer.set_show_sidebar(false);
            }
        });
        let rows = Rc::clone(&self.book_rows);
        self.book_list.set_header_func(move |row, before| {
            let rows = rows.borrow();
            let account = rows.get(row.index() as usize).map(|(_, account)| account.as_str()).filter(|a| !a.is_empty());
            let previous = before.and_then(|before| rows.get(before.index() as usize)).map(|(_, account)| account.as_str());
            match account.filter(|account| Some(*account) != previous) {
                Some(account) => row.set_header(Some(&section_label(account, 14))),
                None => row.set_header(None::<&gtk::Widget>),
            }
        });

        let weak = Rc::downgrade(this);
        self.contact_list.connect_row_selected(move |_, row| {
            let Some(this) = weak.upgrade().filter(|this| !this.quiet.get()) else { return };
            let Some(card) = row.and_then(|row| this.listed.borrow().get(row.index() as usize).map(|entry| entry.card)) else { return };
            this.current.set(Some(card));
            this.show_detail();
        });
        let weak = Rc::downgrade(this);
        self.contact_list.connect_row_activated(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.inner.set_show_content(true);
            }
        });
        let sections = Rc::clone(&self.sections);
        self.contact_list.set_header_func(move |row, before| {
            let sections = sections.borrow();
            let section = sections.get(row.index() as usize);
            let previous = before.and_then(|before| sections.get(before.index() as usize));
            match section.filter(|section| Some(*section) != previous) {
                Some(section) => row.set_header(Some(&section_label(section, if row.index() == 0 { 4 } else { 12 }))),
                None => row.set_header(None::<&gtk::Widget>),
            }
        });

        let weak = Rc::downgrade(this);
        self.search.connect_search_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.reload_list();
                this.keep_current_listed();
            }
        });
        self.search.connect_stop_search(|search| search.set_text(""));

        let weak = Rc::downgrade(this);
        self.banner.connect_button_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let sign_in = this.problems.borrow().iter().find_map(|p| p.sign_in.clone());
            match sign_in {
                Some(key) => this.sign_in(key),
                None => this.show_problems(),
            }
        });
    }

    fn toast(&self, message: &str) {
        let toast = adw::Toast::builder().title(glib::markup_escape_text(message)).timeout(5).build();
        self.toasts.add_toast(toast);
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.refreshing.get() || self.writes.get() > 0
    }

    pub(crate) fn listed(&self) -> Vec<Entry> {
        self.listed.borrow().clone()
    }

    pub(crate) fn current_entry(&self) -> Option<Entry> {
        let current = self.current.get()?;
        self.entries.borrow().iter().find(|entry| entry.card == current).cloned()
    }

    fn book(&self, id: i64) -> Option<Book> {
        self.books.borrow().iter().find(|book| book.id == id).cloned()
    }

    /// "Contacts · iCloud", or "On This Computer" for the local book.
    fn book_label(&self, book: &Book) -> String {
        let account = self.service.account_label(&book.account);
        if book.is_local() { account } else { format!("{} · {account}", book.name) }
    }

    /// The books a contact can be written to, as the editor offers them.
    fn writable(&self) -> Vec<(i64, String)> {
        self.books
            .borrow()
            .iter()
            .filter(|book| !book.read_only)
            .map(|book| (book.id, self.book_label(book)))
            .collect()
    }

    /// Reads every card from the store again and shows them.
    pub(crate) fn reload(&self) {
        let books = match self.service.store.books() {
            Ok(books) => books,
            Err(error) => {
                self.toast(&error.to_string());
                return;
            }
        };
        let cards = self.service.store.cards().unwrap_or_default();
        let entries = cards
            .into_iter()
            .filter_map(|card| match vcard::parse(&card.vcf) {
                Ok(parsed) => Some(Entry { card: card.id, book: card.book, contact: Contact::from_card(&parsed) }),
                Err(error) => {
                    tracing::warn!("a card that couldn't be read ({}): {error}", card.url);
                    None
                }
            })
            .collect();
        *self.entries.borrow_mut() = entries;
        if self.view.get().is_some_and(|view| !books.iter().any(|book| book.id == view)) {
            self.view.set(None);
        }
        *self.books.borrow_mut() = books;
        self.reload_books();
        self.reload_list();
        self.keep_current_listed();
    }

    fn reload_books(&self) {
        let books = self.books.borrow();
        let entries = self.entries.borrow();
        let count = |book: Option<i64>| entries.iter().filter(|entry| book.is_none_or(|book| entry.book == book)).count();
        let mut rows = vec![(None, String::new())];
        self.quiet.set(true);
        self.book_list.remove_all();
        self.book_list.append(&book_row("system-users-symbolic", "All Contacts", count(None)));
        for book in books.iter() {
            let icon = if book.is_local() { "computer-symbolic" } else { "x-office-address-book-symbolic" };
            self.book_list.append(&book_row(icon, &book.name, count(Some(book.id))));
            rows.push((Some(book.id), self.service.account_label(&book.account)));
        }
        let selected = rows.iter().position(|(view, _)| *view == self.view.get()).unwrap_or(0);
        *self.book_rows.borrow_mut() = rows;
        self.book_list.invalidate_headers();
        self.book_list.select_row(self.book_list.row_at_index(selected as i32).as_ref());
        self.quiet.set(false);
    }

    /// Lists the chosen book's contacts, or the search's, keeping the open one selected.
    fn reload_list(&self) {
        let by_family = self.service.config.sorts_by_family();
        let search = self.search.text().trim().to_owned();
        let mut listed: Vec<Entry> = self
            .entries
            .borrow()
            .iter()
            .filter(|entry| self.view.get().is_none_or(|view| entry.book == view))
            .filter(|entry| search.is_empty() || entry.contact.matches(&search))
            .cloned()
            .collect();
        listed.sort_by_cached_key(|entry| {
            let section = entry.contact.section(by_family);
            // Letters first, then the rest under #.
            (section == "#", section, entry.contact.sort_key(by_family), entry.card)
        });
        *self.sections.borrow_mut() = listed.iter().map(|entry| entry.contact.section(by_family)).collect();
        self.quiet.set(true);
        self.contact_list.remove_all();
        for entry in &listed {
            self.contact_list.append(&contact_row(&entry.contact, by_family));
        }
        if let Some(index) = listed.iter().position(|entry| Some(entry.card) == self.current.get()) {
            self.contact_list.select_row(self.contact_list.row_at_index(index as i32).as_ref());
        }
        self.quiet.set(false);
        self.contact_list.invalidate_headers();

        let (title, description) = if !search.is_empty() {
            ("No Results", "No contact has all those words")
        } else if self.refreshing.get() && self.entries.borrow().is_empty() {
            ("Loading Contacts", "Reading your address books")
        } else {
            ("No Contacts", "Press Ctrl+N for a new contact, or import a vCard file")
        };
        self.list_empty.set_title(title);
        self.list_empty.set_description(Some(description));
        self.list_stack.set_visible_child_name(if listed.is_empty() { "empty" } else { "contacts" });
        let title = match self.view.get().and_then(|view| self.book(view)) {
            Some(book) if book.is_local() => "On This Computer".to_owned(),
            Some(book) => book.name,
            None => "All Contacts".to_owned(),
        };
        self.list_page.set_title(&title);
        *self.listed.borrow_mut() = listed;
    }

    /// When the open contact isn't listed, opens the first one listed.
    fn keep_current_listed(&self) {
        let listed = self.listed.borrow();
        if !self.current.get().is_some_and(|card| listed.iter().any(|entry| entry.card == card)) {
            self.current.set(listed.first().map(|entry| entry.card));
            if let Some(row) = self.contact_list.row_at_index(0) {
                self.quiet.set(true);
                self.contact_list.select_row(Some(&row));
                self.quiet.set(false);
            }
        }
        drop(listed);
        self.show_detail();
    }

    pub(crate) fn select(&self, card: i64) {
        self.current.set(Some(card));
        if !self.listed.borrow().iter().any(|entry| entry.card == card) {
            self.view.set(None);
            self.search.set_text("");
            self.reload_books();
            self.reload_list();
        }
        if let Some(index) = self.listed.borrow().iter().position(|entry| entry.card == card) {
            self.quiet.set(true);
            self.contact_list.select_row(self.contact_list.row_at_index(index as i32).as_ref());
            self.quiet.set(false);
        }
        self.show_detail();
    }

    fn show_detail(&self) {
        while let Some(child) = self.detail.first_child() {
            self.detail.remove(&child);
        }
        let entry = self.current_entry();
        let book = entry.as_ref().and_then(|entry| self.book(entry.book));
        let writable = book.as_ref().is_some_and(|book| !book.read_only);
        self.edit.set_visible(writable);
        self.delete.set_visible(writable);
        self.more.set_visible(entry.is_some());
        let (Some(entry), Some(book)) = (entry, book) else {
            self.detail_stack.set_visible_child_name("empty");
            return;
        };
        self.detail_stack.set_visible_child_name("contact");
        let contact = &entry.contact;

        // Who: the photo or initials, the name, the job.
        let avatar = adw::Avatar::new(96, Some(&contact.display_name()), true);
        if let Some(texture) = contact.photo.as_deref().and_then(|bytes| gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok()) {
            avatar.set_custom_image(Some(&texture));
        }
        avatar.set_halign(gtk::Align::Center);
        self.detail.append(&avatar);
        let name = gtk::Label::builder()
            .label(contact.display_name())
            .wrap(true)
            .justify(gtk::Justification::Center)
            .selectable(true)
            .build();
        name.add_css_class("title-1");
        let heading = gtk::Box::new(gtk::Orientation::Vertical, 4);
        heading.append(&name);
        self.detail.append(&heading);
        let about: Vec<String> = [
            (!contact.nickname.is_empty()).then(|| format!("“{}”", contact.nickname)),
            Some(contact.job()).filter(|job| !job.is_empty()),
            Some(contact.department.clone()).filter(|d| !d.is_empty() && !contact.is_company),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !about.is_empty() {
            let label = gtk::Label::builder().label(about.join("\n")).justify(gtk::Justification::Center).wrap(true).build();
            label.add_css_class("dim-label");
            heading.append(&label);
        }

        // Quick actions: write, call, open the website.
        let quick = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
        for (icon, tooltip, kind, value) in [
            ("mail-unread-symbolic", "Email", Kind::Email, contact.emails.first()),
            ("call-start-symbolic", "Call", Kind::Phone, contact.phones.first()),
            ("web-browser-symbolic", "Open Website", Kind::Website, contact.websites.first()),
        ] {
            let Some(link) = value.and_then(|field| contact::link(kind, &field.value)) else { continue };
            let button = gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).width_request(48).height_request(48).build();
            button.add_css_class("circular");
            button.connect_clicked(move |button| launch(button, &link));
            quick.append(&button);
        }
        if quick.first_child().is_some() {
            self.detail.append(&quick);
        }

        // The fields, each a row that opens it and a button that copies it.
        let group = |title: &str| {
            let group = adw::PreferencesGroup::builder().title(title).build();
            self.detail.append(&group);
            group
        };
        let lists = [
            ("Phone", Kind::Phone, &contact.phones),
            ("Email", Kind::Email, &contact.emails),
            ("Website", Kind::Website, &contact.websites),
        ];
        let any = lists.iter().any(|(_, _, fields)| !fields.is_empty());
        if any {
            let fields = group("");
            for (_, kind, list) in lists {
                for field in list {
                    fields.add(&self.field_row(&field.label, &field.value, contact::link(kind, &field.value)));
                }
            }
        }
        if !contact.addresses.is_empty() {
            let addresses = group("");
            for address in &contact.addresses {
                let text = address.lines().join("\n");
                addresses.add(&self.field_row(&format!("{} address", address.label), &text, contact::link(Kind::Address, &text)));
            }
        }
        if let Some(birthday) = contact.birthday {
            let mut text = birthday.to_text();
            if contact::is_birthday_today(birthday) {
                text.push_str(" · today");
            }
            group("").add(&self.field_row("birthday", &text, None));
        }
        if !contact.note.trim().is_empty() {
            group("").add(&self.field_row("notes", contact.note.trim(), None));
        }
        let place = gtk::Label::new(Some(&format!(
            "{}{}",
            self.book_label(&book),
            if book.read_only { " (read-only)" } else { "" }
        )));
        place.add_css_class("dim-label");
        place.add_css_class("caption");
        self.detail.append(&place);

        // The menu: export, and move to another book.
        let menu = gio::Menu::new();
        menu.append(Some("Export vCard…"), Some("win.export-contact"));
        if writable {
            let books = gio::Menu::new();
            for (id, label) in self.writable().into_iter().filter(|(id, _)| *id != book.id) {
                let item = gio::MenuItem::new(Some(&label.replace('_', "__")), None);
                item.set_action_and_target_value(Some("win.move-to"), Some(&id.to_variant()));
                books.append_item(&item);
            }
            if books.n_items() > 0 {
                menu.append_submenu(Some("Move To"), &books);
            }
        }
        self.more.set_menu_model(Some(&menu));
    }

    fn field_row(&self, label: &str, value: &str, link: Option<String>) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(label))
            .subtitle(glib::markup_escape_text(value))
            .subtitle_lines(0)
            .subtitle_selectable(true)
            .build();
        row.add_css_class("property");
        let copy = gtk::Button::builder().icon_name("edit-copy-symbolic").tooltip_text("Copy").valign(gtk::Align::Center).build();
        copy.add_css_class("flat");
        let (text, toasts) = (value.to_owned(), self.toasts.clone());
        copy.connect_clicked(move |button| {
            button.clipboard().set_text(&text);
            toasts.add_toast(adw::Toast::builder().title("Copied").timeout(2).build());
        });
        row.add_suffix(&copy);
        if let Some(link) = link {
            row.set_activatable(true);
            row.connect_activated(move |row| launch(row, &link));
        }
        row
    }

    fn set_refreshing(&self, refreshing: bool) {
        self.refreshing.set(refreshing);
        self.spinner.set_visible(refreshing);
        self.refresh.set_visible(!refreshing);
    }

    pub(crate) fn refresh_now(self: &Rc<Self>) {
        if self.refreshing.get() {
            return;
        }
        self.set_refreshing(true);
        let service = Arc::clone(&self.service);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let problems = run(async move { service.refresh().await }).await;
            if let Some(this) = weak.upgrade() {
                this.refreshed(problems);
            }
        });
    }

    fn refreshed(&self, problems: Vec<Problem>) {
        self.set_refreshing(false);
        let signing_in = problems.iter().find(|p| p.sign_in.is_some());
        self.banner.set_button_label(Some(if signing_in.is_some() { "Sign In" } else { "Details" }));
        match problems.as_slice() {
            [] => self.banner.set_revealed(false),
            _ if let Some(one) = signing_in => {
                self.banner.set_title(&glib::markup_escape_text(&format!("Sign in with Google to see the contacts of {}", one.account)));
                self.banner.set_revealed(true);
            }
            [one] => {
                self.banner.set_title(&glib::markup_escape_text(&format!("{}: {}", one.account, one.message)));
                self.banner.set_revealed(true);
            }
            many => {
                self.banner.set_title(&format!("{} accounts couldn't be refreshed", many.len()));
                self.banner.set_revealed(true);
            }
        }
        *self.problems.borrow_mut() = problems;
        self.reload();
    }

    /// Signs a Google account in through the browser, then refreshes.
    fn sign_in(self: &Rc<Self>, key: String) {
        self.toast("Finish signing in with Google in your browser");
        let service = Arc::clone(&self.service);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = run(async move {
                service
                    .google_sign_in(&key, |url| {
                        glib::MainContext::default().invoke(move || {
                            if let Err(error) = gio::AppInfo::launch_default_for_uri(&url, gio::AppLaunchContext::NONE) {
                                tracing::warn!("couldn't open the browser: {error}");
                            }
                        });
                    })
                    .await
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            match result {
                Ok(()) => {
                    this.toast("Signed in with Google");
                    this.refresh_now();
                }
                Err(error) => this.toast(&error),
            }
        });
    }

    fn show_problems(&self) {
        let body = self
            .problems
            .borrow()
            .iter()
            .map(|p| format!("{}: {}", p.account, p.message))
            .collect::<Vec<_>>()
            .join("\n\n");
        let alert = adw::AlertDialog::new(Some("Contacts That Couldn't Be Refreshed"), Some(&body));
        alert.add_response("close", "_Close");
        alert.present(Some(&self.window));
    }

    /// Runs a write in the background, then reloads, and hands its result to `done`, or shows
    /// why it failed.
    fn write<T, F>(self: &Rc<Self>, future: F, done: impl FnOnce(&Rc<Self>, T) + 'static)
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
    {
        self.writes.set(self.writes.get() + 1);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = run(future).await;
            let Some(this) = weak.upgrade() else { return };
            this.writes.set(this.writes.get() - 1);
            this.reload();
            match result {
                Ok(value) => done(&this, value),
                Err(error) => this.toast(&format!("Not saved: {error}")),
            }
        });
    }

    pub(crate) fn new_contact(self: &Rc<Self>) -> Rc<Editor> {
        let books = self.writable();
        let preferred = [self.view.get(), self.last_book.get()]
            .into_iter()
            .flatten()
            .find(|id| books.iter().any(|(book, _)| book == id));
        // A server's book before the local one, so new contacts reach your phone too.
        let book = preferred.or_else(|| {
            let local: Vec<i64> = self.books.borrow().iter().filter(|b| b.is_local()).map(|b| b.id).collect();
            books.iter().map(|(id, _)| *id).find(|id| !local.contains(id)).or(books.first().map(|(id, _)| *id))
        });
        let request = Request { title: "New Contact", draft: Draft::default(), books, book };
        let weak = Rc::downgrade(self);
        Editor::open(&self.window, request, move |draft, book| {
            let (Some(this), Some(book)) = (weak.upgrade(), book) else { return };
            this.last_book.set(Some(book));
            let uid = contact::new_uid();
            let vcf = contact::new_card(&uid, &draft).to_vcf();
            let service = Arc::clone(&this.service);
            this.write(async move { service.create(book, &uid, &vcf).await }, |this, id| {
                this.select(id);
                this.toast("Contact added");
            });
        })
    }

    pub(crate) fn edit_contact(self: &Rc<Self>) -> Option<Rc<Editor>> {
        let entry = self.current_entry()?;
        if self.book(entry.book).is_none_or(|book| book.read_only) {
            return None;
        }
        let request = Request { title: "Edit Contact", draft: Draft::of(&entry.contact), books: Vec::new(), book: None };
        let weak = Rc::downgrade(self);
        Some(Editor::open(&self.window, request, move |draft, _| {
            let Some(this) = weak.upgrade() else { return };
            let Some(stored) = this.service.store.card(entry.card).ok().flatten() else { return };
            let mut card = match vcard::parse(&stored.vcf) {
                Ok(card) => card,
                Err(error) => return this.toast(&format!("Not saved: {error}")),
            };
            contact::apply(&mut card, &draft);
            let (service, id, vcf) = (Arc::clone(&this.service), entry.card, card.to_vcf());
            this.write(async move { service.update(id, &vcf).await }, |this, ()| this.toast("Contact saved"));
        }))
    }

    fn delete_contact(self: &Rc<Self>) {
        let Some(entry) = self.current_entry() else { return };
        let alert = adw::AlertDialog::new(
            Some("Delete Contact?"),
            Some(&format!("“{}” is deleted from {} everywhere it syncs.", entry.contact.display_name(), self.book(entry.book).map(|b| self.book_label(&b)).unwrap_or_default())),
        );
        alert.add_responses(&[("cancel", "_Cancel"), ("delete", "_Delete")]);
        alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        alert.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        alert.connect_response(Some("delete"), move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.delete_now(entry.card);
            }
        });
        alert.present(Some(&self.window));
    }

    pub(crate) fn delete_now(self: &Rc<Self>, card: i64) {
        let index = self.listed.borrow().iter().position(|entry| entry.card == card).unwrap_or(0);
        let service = Arc::clone(&self.service);
        self.write(async move { service.delete(card).await }, move |this, ()| {
            let next = {
                let listed = this.listed.borrow();
                listed.get(index).or(listed.last()).map(|entry| entry.card)
            };
            if let Some(next) = next {
                this.select(next);
            }
            this.toast("Contact deleted");
        });
    }

    fn move_contact(self: &Rc<Self>, book: i64) {
        let Some(entry) = self.current_entry() else { return };
        let service = Arc::clone(&self.service);
        let label = self.book(book).map(|b| self.book_label(&b)).unwrap_or_default();
        self.write(async move { service.move_to(entry.card, book).await }, move |this, id| {
            this.select(id);
            this.toast(&format!("Moved to {label}"));
        });
    }

    /// Asks for a vCard file, then the book to add its contacts to.
    fn import(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("vCard files"));
        filter.add_mime_type("text/vcard");
        filter.add_mime_type("text/x-vcard");
        filter.add_suffix("vcf");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title("Import vCards").filters(&filters).modal(true).build();
        let weak = Rc::downgrade(self);
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |file| {
            let (Some(this), Ok(file)) = (weak.upgrade(), file) else { return };
            glib::spawn_future_local(async move {
                let text = match file.load_contents_future().await {
                    Ok((bytes, _)) => String::from_utf8_lossy(&bytes).into_owned(),
                    Err(error) => return this.toast(&error.to_string()),
                };
                if let Some(book) = this.choose_book("Import Contacts To").await {
                    this.import_text(book, text);
                }
            });
        });
    }

    pub(crate) fn import_text(self: &Rc<Self>, book: i64, text: String) {
        let service = Arc::clone(&self.service);
        self.write(async move { service.import(book, &text).await }, |this, (added, failure)| match failure {
            None => this.toast(&format!("Imported {added} contact{}", if added == 1 { "" } else { "s" })),
            Some(error) => this.toast(&format!("Imported {added}; some weren't: {error}")),
        });
    }

    /// The chosen book when it can be written to, else asks which.
    async fn choose_book(&self, heading: &str) -> Option<i64> {
        let books = self.writable();
        if let Some(view) = self.view.get().filter(|view| books.iter().any(|(id, _)| id == view)) {
            return Some(view);
        }
        if books.len() == 1 {
            return Some(books[0].0);
        }
        let labels: Vec<&str> = books.iter().map(|(_, label)| label.as_str()).collect();
        let dropdown = gtk::DropDown::from_strings(&labels);
        let alert = adw::AlertDialog::builder().heading(heading).extra_child(&dropdown).close_response("cancel").build();
        alert.add_responses(&[("cancel", "_Cancel"), ("ok", "_Import")]);
        alert.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("ok"));
        (alert.choose_future(Some(&self.window)).await == "ok")
            .then(|| books.get(dropdown.selected() as usize).map(|(id, _)| *id))
            .flatten()
    }

    /// Saves cards to a vCard file the person names.
    fn export(self: &Rc<Self>, cards: Vec<i64>, name: &str) {
        if cards.is_empty() {
            return self.toast("No contacts to export");
        }
        let dialog = gtk::FileDialog::builder().title("Export vCards").initial_name(name).modal(true).build();
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |file| {
            let (Some(this), Ok(file)) = (weak.upgrade(), file) else { return };
            let text: String = cards
                .iter()
                .filter_map(|id| this.service.store.card(*id).ok().flatten())
                .map(|card| {
                    let vcf = card.vcf.trim_end().to_owned();
                    vcf + "\r\n"
                })
                .collect();
            let count = cards.len();
            glib::spawn_future_local(async move {
                let saved = file
                    .replace_contents_future(text.into_bytes(), None, false, gio::FileCreateFlags::REPLACE_DESTINATION)
                    .await;
                match saved {
                    Ok(_) => this.toast(&format!("Exported {count} contact{}", if count == 1 { "" } else { "s" })),
                    Err((_, error)) => this.toast(&error.to_string()),
                }
            });
        });
    }
}

fn launch(widget: &impl IsA<gtk::Widget>, uri: &str) {
    let launcher = gtk::UriLauncher::new(uri);
    let root = widget.root().and_downcast::<gtk::Window>();
    launcher.launch(root.as_ref(), gio::Cancellable::NONE, |result| {
        if let Err(error) = result {
            tracing::warn!("couldn't open a link: {error}");
        }
    });
}

fn section_label(text: &str, top: i32) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).margin_start(12).margin_top(top).margin_bottom(4).build();
    label.add_css_class("caption-heading");
    label.add_css_class("dim-label");
    label
}

fn book_row(icon: &str, name: &str, contacts: usize) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    line.append(&gtk::Image::from_icon_name(icon));
    line.append(&gtk::Label::builder().label(name).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build());
    let count = gtk::Label::new(Some(&contacts.to_string()));
    count.add_css_class("dim-label");
    count.add_css_class("numeric");
    line.append(&count);
    gtk::ListBoxRow::builder().child(&line).build()
}

/// A contact in the list: the photo or initials, the name with the sorting name in bold, and
/// the company.
fn contact_row(contact: &Contact, by_family: bool) -> gtk::ListBoxRow {
    let avatar = adw::Avatar::new(32, Some(&contact.display_name()), true);
    if let Some(texture) = contact.photo.as_deref().and_then(|bytes| gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok()) {
        avatar.set_custom_image(Some(&texture));
    }
    let escape = |text: &str| glib::markup_escape_text(text).to_string();
    let markup = if shows_parts(contact) {
        if by_family {
            format!("{} <b>{}</b>", escape(&contact.given), escape(&contact.family))
        } else {
            format!("<b>{}</b> {}", escape(&contact.given), escape(&contact.family))
        }
    } else {
        format!("<b>{}</b>", escape(&contact.display_name()))
    };
    let name = gtk::Label::builder().use_markup(true).label(markup).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).build();
    column.append(&name);
    let job = contact.job();
    if !job.is_empty() {
        let label = gtk::Label::builder().label(&job).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        column.append(&label);
    }
    let line = gtk::Box::builder().spacing(10).margin_top(4).margin_bottom(4).margin_start(2).build();
    line.append(&avatar);
    line.append(&column);
    gtk::ListBoxRow::builder().child(&line).build()
}

/// Whether the list shows the given and family names as the card has them (its `FN` is those
/// two), so one of them can be in bold.
fn shows_parts(contact: &Contact) -> bool {
    let composed = format!("{} {}", contact.given, contact.family);
    !contact.is_company
        && !contact.given.is_empty()
        && !contact.family.is_empty()
        && (contact.formatted.is_empty() || contact.formatted.split_whitespace().eq(composed.split_whitespace()))
}
