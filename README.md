# Ledger

Contacts for [Omarchy](https://omarchy.org), in the manner of Apple Contacts, and a companion to the [Sealwax](https://github.com/PixDevsApps/sealwax) mail client, [Signet](https://github.com/etmartinkazoo/signet) and [Quill](https://github.com/etmartinkazoo/quill): native GTK 4 and libadwaita, written in Rust, following your live Omarchy theme, and using the accounts you already set up in Sealwax.

## What it does

- **Address books** in the sidebar, under each account, with **All Contacts** above them.
- **Contacts** from A to Z, under their letter, sorted by family name (or given name, see below). The name they sort by is in bold.
- **A contact** shows their photo (or initials), name, job and company, and buttons to email, call or open their website. Each phone, email, website and address opens where it should when you click it, and has a button to copy it. Addresses open in OpenStreetMap.
- **Add and edit:** names, company and job title, any number of phones, emails, addresses and websites, each with a label (mobile, home, work, … or **Custom…** for your own), a birthday (with or without a year) and notes. A new contact goes in the address book you choose; Ledger offers a server's book before the local one, so new contacts reach your phone too.
- **Safe edits:** Ledger changes only the fields you edited. Photos, Apple's custom labels, social profiles and whatever else a card carries are written back as they came. Every change goes to the server with the card's ETag, so Ledger never overwrites an edit made elsewhere in the meantime.
- **Move** a contact to another address book, and **delete** one, from the contact's menu and header.
- **Search** finds the contacts that have every word you type, in names, companies, emails, addresses and notes. Phone numbers match however they're spaced: `7946 0001` finds `+44 20 7946-0001`.
- **Import and export vCard files** (`.vcf`): import into an address book from the list's menu (Ctrl+I); export the listed contacts, or one contact from its menu.
- **Offline copy:** contacts are kept on disk, so the window opens at once and is filled in from the server in the background. It refreshes every 5 minutes and on Ctrl+R. A refresh reads only the address books that changed, and in those only the cards that changed.
- **On This Computer:** a local address book for contacts you don't want on any server.

## Accounts shared with Sealwax

Ledger reads Sealwax's `~/.config/sealwax/accounts.toml` and never writes it. It takes each account's password from the keyring item Sealwax already keeps (`application=io.github.pixdevsapps.Sealwax`, `account=<key>`, `kind=password`). An account you added in Sealwax shows its address books here with nothing more to set up. Remove an account in Sealwax, and Ledger forgets its contacts too.

Contacts are read over **CardDAV**. Ledger finds the server from your address:

| Provider | Server |
| --- | --- |
| iCloud | `contacts.icloud.com` (the app password Sealwax uses works) |
| Fastmail | `carddav.fastmail.com` |
| Yahoo, AOL, mailbox.org, Posteo, GMX, web.de, Zoho, Yandex | their CardDAV servers |
| Your own domain (Nextcloud, Radicale, Baïkal, Stalwart, …) | `https://<domain>/.well-known/carddav` (RFC 6764) |

**Gmail accounts** (gmail.com, or a Google Workspace domain on Gmail's servers) get their contacts from Google Contacts through **Sign in with Google**: Google's CardDAV server refuses passwords, app passwords included. Set up an OAuth client once, as below, and then click **Sign In** in Ledger's banner.

Not yet supported: **Microsoft accounts** (`auth = "microsoft"`). These need Microsoft Graph and contacts consent, which Sealwax's mail-only sign-in doesn't grant. Ledger says so in a banner. To hide those accounts, set `enabled = false` for them (see below).

### Setting up Sign in with Google

Google requires each app to have its own OAuth client. If you set one up for Signet, you can use the same client here: enable **CardDAV API** for it (step 2) and copy its `[google]` section.

1. Open [Google Cloud Console](https://console.cloud.google.com/) and create a project, for example "Ledger".
2. In **APIs & Services → Library**, enable **CardDAV API**.
3. In **Google Auth Platform**, click **Get started**. Give the app a name (Ledger) and your address, and choose **External** as the audience.
4. In **Audience**, click **Publish app**. In *Testing*, Google ends sign-ins after 7 days. Published but unverified is fine for your own use.
5. In **Clients**, click **Create client**. Choose **Desktop app**, then copy the client ID and the client secret. Google shows the secret only once.
6. Add the client to `~/.config/ledger/config.toml`:

   ```toml
   [google]
   client_id = "1234…apps.googleusercontent.com"
   client_secret = "GOCSPX-…"
   ```

7. Start Ledger and click **Sign In** in the banner. Your browser opens Google's page. Because the client is yours and unverified, Google warns "Google hasn't verified this app": choose **Advanced → Go to Ledger**. Then allow contacts access.

Ledger asks only for contacts access, plus your address, which it checks against the Sealwax account. The refresh token Google returns is kept in the keyring under Ledger's own name (`application=io.brem.Ledger`, `kind=google-refresh-token`), never in Sealwax's items. To sign out, revoke Ledger's access at [myaccount.google.com/permissions](https://myaccount.google.com/permissions). Ledger then offers to sign in again.

## Settings

`~/.config/ledger/config.toml` is optional:

```toml
sort_by = "family"        # or "given"

# Sign in with Google (see above):
[google]
client_id = "…apps.googleusercontent.com"
client_secret = "GOCSPX-…"

# Per Sealwax account, by its key in accounts.toml:
[account.work]
carddav = "https://dav.example.com/remote.php/dav"  # when discovery can't find the server
username = "ada"                                    # when the contacts login differs from the mail login
enabled = false                                     # leave this account's contacts out
```

A mistake in the file is shown when Ledger starts, and the defaults are used.

## Keyboard

| Key | Action |
| --- | --- |
| Ctrl+N | New contact |
| Ctrl+E | Edit the contact |
| Ctrl+F | Search |
| Ctrl+I | Import vCards |
| Ctrl+R, F5 | Refresh |
| Ctrl+W, Ctrl+Q | Close |

## Build and install

Needs Rust 1.92+, GTK 4.18+, libadwaita 1.9+, SQLite and a Secret Service such as gnome-keyring. All of these are on Omarchy already, except Rust.

```sh
cargo build --release
install -Dm755 target/release/ledger ~/.local/bin/ledger
install -Dm644 packaging/io.brem.Ledger.desktop ~/.local/share/applications/io.brem.Ledger.desktop
```

Debug builds use the app-id `io.brem.Ledger.Devel`, so they run beside an installed build. Ledger's own files are in `~/.local/share/ledger/ledger.db` (the offline copy; your server contacts are safe to delete from it, but the local address book lives only there) and `~/.config/ledger/config.toml`. Set `LEDGER_LOG=debug` to see what it does. Logs never include passwords or what your contacts say.

## Layout and tests

- `core/`: no GTK. `vcard` (a vCard parser and writer that round-trips), `contact` (the fields Ledger shows, and edits that change only them), `carddav`, `accounts` (Sealwax's file and keyring), `google` (Sign in with Google), `store` (SQLite) and `service` (refreshing, writing and importing).
- `app/`: the window, the editor, and the Omarchy theme (shared with Signet and Quill).

`cargo test --workspace` runs the unit tests. Two more checks need a server, or a display:

```sh
# The CardDAV client and the refresh against a real server (writes to the first address book it
# finds, then removes what it wrote):
LEDGER_TEST_ROOTS=ca.pem cargo run -p ledger-core --example carddav_live -- https://localhost:5232/ user password

# The real window, driven by script: add through the editor, import, edit, search, delete; PNGs
# and report.txt into DIR. Empty config and data directories keep it to the local address book.
XDG_CONFIG_HOME=$(mktemp -d) XDG_DATA_HOME=$(mktemp -d) GDK_BACKEND=broadway LEDGER_SELFTEST=DIR cargo run
```

`LEDGER_TEST_ROOTS` names a PEM file trusted in place of the system's certificates, for a local test server only.

License: MIT.
