//! The contact editor: a dialog with the names and company, a list each of phones, emails,
//! websites and addresses (each with a label, a standard one or one of your own), the birthday
//! and notes. A new contact also chooses its address book.

use gtk4::{self as gtk, glib, prelude::*};
use ledger_core::contact::{Address, Draft, Field, Kind};
use libadwaita::{self as adw, prelude::*};
use std::cell::RefCell;
use std::rc::Rc;

const CUSTOM: &str = "Custom…";

/// What the editor hands a saved draft to, with the book for a new contact.
type Saved = Box<dyn Fn(Draft, Option<i64>)>;

pub struct Request {
    pub title: &'static str,
    pub draft: Draft,
    /// The address books a new contact may go in, and the one chosen first; empty when editing.
    pub books: Vec<(i64, String)>,
    pub book: Option<i64>,
}

/// A label menu: the kind's labels, the custom ones in use, and "Custom…".
struct Label {
    dropdown: gtk::DropDown,
    model: gtk::StringList,
}

impl Label {
    fn new(kind: Kind, label: &str, dialog: &adw::Dialog) -> Self {
        let model = gtk::StringList::new(kind.labels());
        let label = if label.is_empty() { kind.labels()[0] } else { label };
        if !kind.labels().iter().any(|known| known.eq_ignore_ascii_case(label)) {
            model.append(label);
        }
        model.append(CUSTOM);
        let dropdown = gtk::DropDown::builder().model(&model).valign(gtk::Align::Center).build();
        dropdown.add_css_class("flat");
        let this = Self { dropdown, model };
        this.select(label);
        // "Custom…" asks for a label of your own.
        let (model, last) = (this.model.clone(), Rc::new(std::cell::Cell::new(this.dropdown.selected())));
        let dialog = dialog.downgrade();
        this.dropdown.connect_selected_notify(move |dropdown| {
            let picked = dropdown.selected();
            if model.string(picked).as_deref() != Some(CUSTOM) {
                last.set(picked);
                return;
            }
            dropdown.set_selected(last.get());
            let Some(parent) = dialog.upgrade() else { return };
            let entry = gtk::Entry::builder().activates_default(true).placeholder_text("Label").build();
            let ask = adw::AlertDialog::builder()
                .heading("Custom Label")
                .extra_child(&entry)
                .default_response("add")
                .close_response("cancel")
                .build();
            ask.add_responses(&[("cancel", "Cancel"), ("add", "Add")]);
            ask.set_response_appearance("add", adw::ResponseAppearance::Suggested);
            let (model, dropdown, last) = (model.clone(), dropdown.clone(), Rc::clone(&last));
            ask.connect_response(None, move |_, response| {
                let text = entry.text().trim().to_owned();
                if response != "add" || text.is_empty() {
                    return;
                }
                let at = model.n_items() - 1;
                model.splice(at, 0, &[text.as_str()]);
                dropdown.set_selected(at);
                last.set(at);
            });
            ask.present(Some(&parent));
        });
        this
    }

    fn select(&self, label: &str) {
        let found = (0..self.model.n_items()).find(|&i| self.model.string(i).is_some_and(|s| s.eq_ignore_ascii_case(label)));
        self.dropdown.set_selected(found.unwrap_or(0));
    }

    fn text(&self) -> String {
        self.model.string(self.dropdown.selected()).map(|s| s.to_string()).unwrap_or_default()
    }
}

/// Phones, emails or websites: a group of entry rows, each with its label and a remove button.
struct FieldList {
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<(adw::EntryRow, Label)>>,
    kind: Kind,
    dialog: adw::Dialog,
}

impl FieldList {
    fn new(kind: Kind, title: &str, add: &str, fields: &[Field], dialog: &adw::Dialog) -> Rc<Self> {
        let group = adw::PreferencesGroup::builder().title(title).build();
        let button = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text(add).valign(gtk::Align::Center).build();
        button.add_css_class("flat");
        group.set_header_suffix(Some(&button));
        let this = Rc::new(Self { group, rows: RefCell::default(), kind, dialog: dialog.clone() });
        for field in fields {
            this.add(&field.label, &field.value);
        }
        let weak = Rc::downgrade(&this);
        button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                let label = this.next_label();
                this.add(&label, "").grab_focus();
            }
        });
        this
    }

    /// The first standard label not used yet, as a new row's.
    fn next_label(&self) -> String {
        let used: Vec<String> = self.rows.borrow().iter().map(|(_, label)| label.text().to_lowercase()).collect();
        self.kind
            .labels()
            .iter()
            .find(|label| !used.contains(&label.to_lowercase()))
            .unwrap_or(&self.kind.labels()[0])
            .to_string()
    }

    fn add(self: &Rc<Self>, label: &str, value: &str) -> adw::EntryRow {
        let row = adw::EntryRow::builder()
            .title(match self.kind {
                Kind::Phone => "Phone",
                Kind::Email => "Email",
                _ => "Website",
            })
            .text(value)
            .build();
        row.set_input_purpose(match self.kind {
            Kind::Phone => gtk::InputPurpose::Phone,
            Kind::Email => gtk::InputPurpose::Email,
            _ => gtk::InputPurpose::Url,
        });
        let label = Label::new(self.kind, label, &self.dialog);
        row.add_prefix(&label.dropdown);
        let remove = gtk::Button::builder().icon_name("list-remove-symbolic").tooltip_text("Remove").valign(gtk::Align::Center).build();
        remove.add_css_class("flat");
        row.add_suffix(&remove);
        let weak = Rc::downgrade(self);
        let target = row.clone();
        remove.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.group.remove(&target);
                this.rows.borrow_mut().retain(|(row, _)| row != &target);
            }
        });
        self.group.add(&row);
        self.rows.borrow_mut().push((row.clone(), label));
        row
    }

    fn fields(&self) -> Vec<Field> {
        self.rows.borrow().iter().map(|(row, label)| Field::new(&label.text(), &row.text())).collect()
    }
}

/// Addresses: an expander per address, with its parts as entry rows.
struct AddressList {
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<(adw::ExpanderRow, Label, [adw::EntryRow; 5])>>,
    dialog: adw::Dialog,
}

impl AddressList {
    fn new(addresses: &[Address], dialog: &adw::Dialog) -> Rc<Self> {
        let group = adw::PreferencesGroup::builder().title("Addresses").build();
        let button = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text("Add Address").valign(gtk::Align::Center).build();
        button.add_css_class("flat");
        group.set_header_suffix(Some(&button));
        let this = Rc::new(Self { group, rows: RefCell::default(), dialog: dialog.clone() });
        for address in addresses {
            this.add(address);
        }
        let weak = Rc::downgrade(&this);
        button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                let label = if this.rows.borrow().is_empty() { "home" } else { "work" };
                this.add(&Address { label: label.to_owned(), ..Address::default() });
            }
        });
        this
    }

    fn add(self: &Rc<Self>, address: &Address) {
        let expander = adw::ExpanderRow::builder().title("Address").expanded(true).build();
        let label = Label::new(Kind::Address, &address.label, &self.dialog);
        expander.add_prefix(&label.dropdown);
        let remove = gtk::Button::builder().icon_name("list-remove-symbolic").tooltip_text("Remove").valign(gtk::Align::Center).build();
        remove.add_css_class("flat");
        expander.add_suffix(&remove);
        let parts = [
            ("Street", &address.street),
            ("City", &address.city),
            ("State or Region", &address.region),
            ("Postcode", &address.postcode),
            ("Country", &address.country),
        ]
        .map(|(title, text)| {
            let row = adw::EntryRow::builder().title(title).text(text.replace('\n', ", ")).build();
            expander.add_row(&row);
            row
        });
        let weak = Rc::downgrade(self);
        let target = expander.clone();
        remove.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.group.remove(&target);
                this.rows.borrow_mut().retain(|(row, _, _)| row != &target);
            }
        });
        self.group.add(&expander);
        self.rows.borrow_mut().push((expander, label, parts));
    }

    fn addresses(&self) -> Vec<Address> {
        self.rows
            .borrow()
            .iter()
            .map(|(_, label, [street, city, region, postcode, country])| Address {
                label: label.text(),
                street: street.text().to_string(),
                city: city.text().to_string(),
                region: region.text().to_string(),
                postcode: postcode.text().to_string(),
                country: country.text().to_string(),
            })
            .collect()
    }
}

pub struct Editor {
    pub dialog: adw::Dialog,
    toasts: adw::ToastOverlay,
    given: adw::EntryRow,
    family: adw::EntryRow,
    nickname: adw::EntryRow,
    organization: adw::EntryRow,
    title: adw::EntryRow,
    phones: Rc<FieldList>,
    emails: Rc<FieldList>,
    websites: Rc<FieldList>,
    addresses: Rc<AddressList>,
    birthday: adw::EntryRow,
    note: gtk::TextView,
    book: Option<(adw::ComboRow, Vec<i64>)>,
    saved: RefCell<Option<Saved>>,
}

impl Editor {
    /// Opens the editor over `parent`; `saved` gets the draft, and the book for a new contact.
    pub fn open(parent: &impl IsA<gtk::Widget>, request: Request, saved: impl Fn(Draft, Option<i64>) + 'static) -> Rc<Self> {
        let dialog = adw::Dialog::builder().title(request.title).content_width(520).content_height(720).build();
        let draft = request.draft;
        let entry = |title: &str, text: &str| adw::EntryRow::builder().title(title).text(text).build();
        let given = entry("First Name", &draft.given);
        let family = entry("Last Name", &draft.family);
        let nickname = entry("Nickname", &draft.nickname);
        let organization = entry("Company", &draft.organization);
        let title = entry("Job Title", &draft.title);
        let names = adw::PreferencesGroup::new();
        for row in [&given, &family, &nickname, &organization, &title] {
            names.add(row);
        }
        let page = adw::PreferencesPage::new();
        let book = (!request.books.is_empty()).then(|| {
            let labels: Vec<&str> = request.books.iter().map(|(_, name)| name.as_str()).collect();
            let combo = adw::ComboRow::builder().title("Address Book").model(&gtk::StringList::new(&labels)).build();
            let ids: Vec<i64> = request.books.iter().map(|(id, _)| *id).collect();
            if let Some(index) = request.book.and_then(|book| ids.iter().position(|id| *id == book)) {
                combo.set_selected(index as u32);
            }
            let group = adw::PreferencesGroup::new();
            group.add(&combo);
            page.add(&group);
            (combo, ids)
        });
        page.add(&names);
        let phones = FieldList::new(Kind::Phone, "Phone Numbers", "Add Phone", &draft.phones, &dialog);
        let emails = FieldList::new(Kind::Email, "Email Addresses", "Add Email", &draft.emails, &dialog);
        let addresses = AddressList::new(&draft.addresses, &dialog);
        let websites = FieldList::new(Kind::Website, "Websites", "Add Website", &draft.websites, &dialog);
        page.add(&phones.group);
        page.add(&emails.group);
        page.add(&addresses.group);
        page.add(&websites.group);
        let birthday = entry("Birthday", &draft.birthday);
        birthday.set_tooltip_text(Some("1990-03-12, or 03-12 without a year"));
        let more = adw::PreferencesGroup::new();
        more.add(&birthday);
        page.add(&more);
        let note = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(10)
            .bottom_margin(10)
            .left_margin(12)
            .right_margin(12)
            .height_request(96)
            .accepts_tab(false)
            .build();
        note.buffer().set_text(&draft.note);
        let frame = gtk::Frame::builder().child(&note).build();
        frame.add_css_class("view");
        let notes = adw::PreferencesGroup::builder().title("Notes").build();
        notes.add(&frame);
        page.add(&notes);

        let cancel = gtk::Button::builder().label("Cancel").build();
        let save = gtk::Button::builder().label("Save").build();
        save.add_css_class("suggested-action");
        let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
        header.pack_start(&cancel);
        header.pack_end(&save);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&page));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));
        dialog.set_child(Some(&view));
        dialog.set_default_widget(Some(&save));

        let this = Rc::new(Self {
            dialog,
            toasts,
            given,
            family,
            nickname,
            organization,
            title,
            phones,
            emails,
            websites,
            addresses,
            birthday,
            note,
            book,
            saved: RefCell::new(Some(Box::new(saved))),
        });
        let weak = Rc::downgrade(&this);
        cancel.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.dialog.close();
            }
        });
        let weak = Rc::downgrade(&this);
        save.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.save();
            }
        });
        // The dialog holds the editor until it closes.
        let held = RefCell::new(Some(Rc::clone(&this)));
        this.dialog.connect_closed(move |_| {
            held.borrow_mut().take();
        });
        this.dialog.present(Some(parent));
        this.given.grab_focus();
        this
    }

    pub fn draft(&self) -> Draft {
        let buffer = self.note.buffer();
        let (start, end) = buffer.bounds();
        Draft {
            given: self.given.text().to_string(),
            family: self.family.text().to_string(),
            nickname: self.nickname.text().to_string(),
            organization: self.organization.text().to_string(),
            title: self.title.text().to_string(),
            phones: self.phones.fields(),
            emails: self.emails.fields(),
            addresses: self.addresses.addresses(),
            websites: self.websites.fields(),
            birthday: self.birthday.text().to_string(),
            note: buffer.text(&start, &end, false).to_string(),
        }
    }

    /// Hands the draft over and closes, or says what's wrong with it.
    pub fn save(&self) {
        let draft = self.draft();
        if let Some(problem) = draft.problem() {
            self.toasts.add_toast(adw::Toast::builder().title(glib::markup_escape_text(problem)).timeout(5).build());
            return;
        }
        let book = self.book.as_ref().and_then(|(combo, ids)| ids.get(combo.selected() as usize).copied());
        if let Some(saved) = self.saved.borrow_mut().take() {
            saved(draft, book);
        }
        self.dialog.close();
    }

    /// Fills the fields as a person would type them; for the selftest.
    pub fn fill(&self, draft: &Draft) {
        self.given.set_text(&draft.given);
        self.family.set_text(&draft.family);
        self.nickname.set_text(&draft.nickname);
        self.organization.set_text(&draft.organization);
        self.title.set_text(&draft.title);
        for (list, fields) in [(&self.phones, &draft.phones), (&self.emails, &draft.emails), (&self.websites, &draft.websites)] {
            for (row, label) in list.rows.borrow_mut().drain(..) {
                list.group.remove(&row);
                drop(label);
            }
            for field in fields {
                list.add(&field.label, &field.value);
            }
        }
        for address in &draft.addresses {
            self.addresses.add(address);
        }
        self.birthday.set_text(&draft.birthday);
        self.note.buffer().set_text(&draft.note);
    }
}
