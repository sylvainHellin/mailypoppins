//! The mailbox catalogue (#0141): every folder the server lists, with the role
//! and the policy the daemon keeps it by.
//!
//! Three layers, from the wire inwards:
//!
//! - [`discover`] asks the server: IMAP `LIST "" "*"` (plus `STATUS` for the
//!   counts of a `status-only` folder) and Graph's folder tree with its
//!   well-known folders. That is the only part that does I/O to a server.
//! - [`listing`] keeps what the server last said, per account, in the store's
//!   `mailbox_listing` table: names, delimiters, attributes, counts, and which
//!   folders went missing. Raw facts only, so a config edit takes effect at the
//!   next read without a new `LIST`.
//! - [`Catalogue::resolve`], here, turns the stored listing and the account's
//!   `config.toml` into the catalogue every consumer reads: the sync targets,
//!   the sidebar rows of both clients, `mp sync --mailbox`, `mp list-mailboxes`
//!   and the `message.fetch` of a server-search hit. It is pure and does no
//!   I/O, which is what keeps every rule below testable offline.
//!
//! Role resolution, first match wins: the config mapping
//! (`[accounts.mailboxes.inbox|archive|sent|drafts|trash|junk]`), the
//! SPECIAL-USE attribute the folder carries in its `LIST` line (both live
//! servers mark `\Sent`, `\Drafts`, `\Trash` and `\Junk` in a plain `LIST`,
//! #0143's probe), then the folder's name against the names Exchange, Proton
//! Bridge and the common servers give the role folders.
//!
//! Policy (Sylvain, 2026-10-22): a role folder (inbox, archive, sent) and every
//! user folder are `sync`; the trash and junk folders and Proton's label
//! folders are `status-only`; Exchange's non-mail folders, virtual views
//! (`\All`, `\Flagged`, Proton's `All Mail` and `Starred`) and `\Noselect`
//! parents are `ignore`. The drafts folder is `ignore` too until #0143 mirrors
//! it, because the Drafts view is the local drafts directory and a second
//! "Drafts" in the sidebar would be the server copy of the same thing. An
//! `[[accounts.mailboxes.override]]` entry changes any of that except a pin, a
//! folder that cannot be selected, and a name that would shadow a role.
//!
//! Store keys: the three product roles keep the keys they always had (`inbox`,
//! `archive`, `sent`); every other folder is keyed by its server name, which
//! is what a `[[accounts.mailboxes.extra]]` entry already keyed it by, so a
//! folder an existing config pinned keeps its rows. A folder whose name would
//! read back as a role key or as the local drafts view is never keyed
//! (pitfall 3).

pub mod discover;
pub mod listing;
mod utf7;

use std::collections::HashSet;

use crate::config::{glob_matches, AccountConfig, AuthMethod, FolderPolicy, MailboxMapping};
use crate::selector::DRAFTS_MAILBOX;
use crate::sync::SyncTarget;
use crate::types::MailboxRole;

pub use listing::ListedFolder;

/// How many of the newest messages a tick downloads from a `status-only`
/// folder that holds a role (the trash and the junk folder).
///
/// A message another client moved there out of a synced folder is the newest
/// in it, so a short tail is what lets the move land as a row in the same pass
/// that prunes the source row (#0072), without downloading the folder's
/// history. A folder with no role (a label) downloads nothing: its messages
/// already live in a real folder.
pub const STATUS_ONLY_WINDOW: usize = 50;

/// A role the catalogue resolves to one server folder per account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FolderRole {
    Inbox,
    Archive,
    Sent,
    Drafts,
    Trash,
    Junk,
}

impl FolderRole {
    pub const ALL: [FolderRole; 6] = [
        FolderRole::Inbox,
        FolderRole::Archive,
        FolderRole::Sent,
        FolderRole::Drafts,
        FolderRole::Trash,
        FolderRole::Junk,
    ];

    /// The wire spelling, which is the `role` of a sidebar row.
    pub fn as_str(self) -> &'static str {
        match self {
            FolderRole::Inbox => "inbox",
            FolderRole::Archive => "archive",
            FolderRole::Sent => "sent",
            FolderRole::Drafts => "drafts",
            FolderRole::Trash => "trash",
            FolderRole::Junk => "junk",
        }
    }

    /// What the sidebar calls the role's folder, whatever the server calls it.
    pub fn label(self) -> &'static str {
        match self {
            FolderRole::Inbox => "Inbox",
            FolderRole::Archive => "Archive",
            FolderRole::Sent => "Sent",
            FolderRole::Drafts => "Drafts",
            FolderRole::Trash => "Trash",
            FolderRole::Junk => "Junk",
        }
    }

    /// The RFC 6154 attribute that marks the role. `\Inbox` is not one of
    /// RFC 6154's: an IMAP inbox is the name `INBOX`, and the attribute is what
    /// the Graph listing gives the folder behind `/me/mailFolders/inbox`,
    /// whose display name may be localized.
    fn attribute(self) -> Option<&'static str> {
        match self {
            FolderRole::Inbox => Some("\\Inbox"),
            FolderRole::Archive => Some("\\Archive"),
            FolderRole::Sent => Some("\\Sent"),
            FolderRole::Drafts => Some("\\Drafts"),
            FolderRole::Trash => Some("\\Trash"),
            FolderRole::Junk => Some("\\Junk"),
        }
    }

    /// The names the role folder goes by on Exchange, Proton Bridge, Dovecot,
    /// Gmail and the rest, compared case-insensitively against the folder's
    /// name with any `INBOX` parent stripped.
    fn names(self) -> &'static [&'static str] {
        match self {
            FolderRole::Inbox => &["inbox"],
            FolderRole::Archive => &["archive", "archives"],
            FolderRole::Sent => &["sent", "sent items", "sent messages", "sent mail"],
            FolderRole::Drafts => &["drafts"],
            FolderRole::Trash => &["trash", "deleted items", "deleted messages", "bin"],
            FolderRole::Junk => &["junk", "junk email", "junk e-mail", "spam", "bulk mail"],
        }
    }

    /// The config mapping that overrides the role, if the account has one.
    fn configured(self, account: &AccountConfig) -> Option<&MailboxMapping> {
        let m = &account.mailboxes;
        match self {
            FolderRole::Inbox => m.inbox.as_ref(),
            FolderRole::Archive => m.archive.as_ref(),
            FolderRole::Sent => m.sent.as_ref(),
            FolderRole::Drafts => m.drafts.as_ref(),
            FolderRole::Trash => m.trash.as_ref(),
            FolderRole::Junk => m.junk.as_ref(),
        }
    }

    /// The store key of a role that has one of its own; every other role's
    /// folder is keyed by its server name.
    fn store_role(self) -> Option<MailboxRole> {
        match self {
            FolderRole::Inbox => Some(MailboxRole::Inbox),
            FolderRole::Archive => Some(MailboxRole::Archive),
            FolderRole::Sent => Some(MailboxRole::Sent),
            _ => None,
        }
    }
}

/// Which server family a listing came from, which decides the exclude lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// Exchange over IMAP or Graph: calendar, contact and task folders sit
    /// beside the mail folders and hold no mail.
    Exchange,
    /// Proton Mail Bridge: `Labels/*` repeat messages that live in a real
    /// folder, and `All Mail` and `Starred` are union views.
    ProtonBridge,
    Generic,
}

/// Exchange's folders that hold no mail over IMAP, each with its subtree.
const EXCHANGE_NON_MAIL: &[&str] = &[
    "calendar",
    "contacts",
    "tasks",
    "notes",
    "journal",
    "rss feeds",
    "rss subscriptions",
    "outbox",
    "conversation history",
    "sync issues",
];

/// Proton's union views, beside the `\All` and `\Flagged` marks they carry.
const PROTON_VIRTUAL: &[&str] = &["all mail", "starred"];

impl Provider {
    /// The family a listing looks like.
    ///
    /// Graph is always Exchange. Over IMAP, Bridge is told by its two
    /// `\Noselect` parents, `Folders` and `Labels`, and Exchange by at least
    /// two of its non-mail folders at the top level, so a Dovecot user with a
    /// mail folder called `Notes` keeps it.
    pub fn detect(account: &AccountConfig, listed: &[ListedFolder]) -> Provider {
        if account.auth_method == AuthMethod::Graph {
            return Provider::Exchange;
        }
        let top: HashSet<String> = listed
            .iter()
            .filter(|f| !f.missing)
            .map(|f| f.name.to_lowercase())
            .collect();
        if top.contains("folders") && top.contains("labels") {
            return Provider::ProtonBridge;
        }
        let exchange_marks = ["calendar", "contacts", "tasks", "journal", "sync issues"]
            .iter()
            .filter(|name| top.contains(**name))
            .count();
        if exchange_marks >= 2 {
            return Provider::Exchange;
        }
        Provider::Generic
    }
}

/// One folder of the catalogue, resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    /// The name `SELECT` takes, as the server spells it.
    pub server: String,
    /// What a person reads: the role's label, or the decoded server name.
    pub label: String,
    /// The store key its rows carry and a selector names.
    pub key: String,
    pub role: Option<FolderRole>,
    pub policy: FolderPolicy,
    /// Its place among the folders `config.toml` names as `inbox`,
    /// `archive`, `sent` or an `extra`, which are always synced; `None` for a
    /// folder only the server named.
    pub pin: Option<usize>,
    /// Listed once, and not by the last `LIST`: kept with its rows, never
    /// selected, never shown, until the server lists it again.
    pub missing: bool,
    /// Why an `ignore` folder is ignored, in words `mp list-mailboxes` prints.
    pub reason: Option<&'static str>,
    pub delimiter: Option<String>,
    pub attributes: Vec<String>,
    /// The server's counts, for a folder whose counts are not the store's.
    pub total: Option<u64>,
    pub unread: Option<u64>,
}

impl Folder {
    /// The target a tick or `mp sync` syncs this folder through.
    pub fn target(&self) -> SyncTarget {
        SyncTarget {
            role: MailboxRole::from_key(&self.key),
            server_name: self.server.clone(),
            window: (self.policy == FolderPolicy::StatusOnly).then_some(STATUS_ONLY_WINDOW),
        }
    }

    /// Whether a tick downloads anything from it.
    fn ticks(&self) -> bool {
        !self.missing
            && match self.policy {
                FolderPolicy::Sync => true,
                FolderPolicy::StatusOnly => self.role.is_some(),
                FolderPolicy::Ignore => false,
            }
    }

    /// Whether it is a mailbox of the sidebar: neither ignored nor missing.
    pub fn listed(&self) -> bool {
        !self.missing && self.policy != FolderPolicy::Ignore
    }

    fn attribute(&self, wanted: &str) -> bool {
        self.attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case(wanted))
    }

    /// Whether its store key is a product role's rather than its server name.
    fn keyed_by_role(&self) -> bool {
        self.role.and_then(FolderRole::store_role).is_some()
    }

    /// Whether `SELECT` can open it at all.
    pub fn selectable(&self) -> bool {
        !self.attribute("\\Noselect") && !self.attribute("\\NonExistent")
    }
}

/// An account's catalogue: every folder, in the order the sidebar lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalogue {
    pub folders: Vec<Folder>,
}

impl Catalogue {
    /// The catalogue of `account` from its store, or from its config alone
    /// when it has no store or the server was never listed: exactly the
    /// mailboxes `config.toml` names, which is the pre-#0141 behaviour.
    pub fn load(account: &AccountConfig) -> Catalogue {
        match crate::store::open_store(&account.name) {
            Some(store) => Catalogue::from_store(&store, account),
            None => Catalogue::resolve(account, &[]),
        }
    }

    /// The catalogue of `account` from a store already open.
    pub fn from_store(store: &crate::store::Store, account: &AccountConfig) -> Catalogue {
        let listed = listing::load(store, &account.name).unwrap_or_else(|e| {
            log::warn!(
                "[catalogue] reading the folder list of {}: {e:#}",
                account.name
            );
            Vec::new()
        });
        Catalogue::resolve(account, &listed)
    }

    /// Resolve a stored listing against the account's config. Pure.
    pub fn resolve(account: &AccountConfig, listed: &[ListedFolder]) -> Catalogue {
        let provider = Provider::detect(account, listed);
        let mut folders: Vec<Folder> = listed
            .iter()
            .map(|f| Folder {
                server: f.name.clone(),
                label: utf7::decode(&f.name),
                key: f.name.clone(),
                role: None,
                policy: FolderPolicy::Sync,
                pin: None,
                missing: f.missing,
                reason: None,
                delimiter: f.delimiter.clone(),
                attributes: f.attributes.clone(),
                total: f.total,
                unread: f.unread,
            })
            .collect();

        // A pin the server did not list is still a folder: config says to sync
        // it, and before #0141 nothing else was asked.
        let pins = pinned_names(account);
        for (_, server) in &pins {
            if !folders
                .iter()
                .any(|f| f.server.eq_ignore_ascii_case(server))
            {
                folders.push(Folder {
                    server: server.clone(),
                    label: utf7::decode(server),
                    key: server.clone(),
                    role: None,
                    policy: FolderPolicy::Sync,
                    pin: None,
                    missing: false,
                    reason: None,
                    delimiter: None,
                    attributes: Vec::new(),
                    total: None,
                    unread: None,
                });
            }
        }
        for folder in &mut folders {
            folder.pin = pins
                .iter()
                .position(|(_, server)| folder.server.eq_ignore_ascii_case(server));
            if let Some((role, server)) = folder.pin.and_then(|i| pins.get(i)) {
                // A pinned folder is present whatever the last `LIST` said, and
                // an `extra` keeps the spelling config gave it as both its key
                // and its SELECT name, which is what its rows are filed under.
                folder.missing = false;
                if matches!(role, MailboxRole::Other(_)) {
                    folder.key = server.clone();
                    folder.server = server.clone();
                }
            }
        }

        assign_roles(account, &pins, &mut folders);
        for folder in &mut folders {
            decide(account, provider, folder);
        }

        folders.sort_by_key(sidebar_rank);
        Catalogue { folders }
    }

    /// What a tick syncs: every `sync` folder and the tail of every role's
    /// `status-only` folder, pins first in config order so the pre-#0141
    /// targets keep their places.
    pub fn targets(&self) -> Vec<SyncTarget> {
        let mut ticking: Vec<&Folder> = self.folders.iter().filter(|f| f.ticks()).collect();
        ticking.sort_by_key(|f| target_rank(f));
        ticking.into_iter().map(Folder::target).collect()
    }

    /// The folders of the sidebar beyond the four fixed rows (Inbox, Drafts,
    /// Sent, Archive), in sidebar order: the trash and junk folders, the
    /// pinned extras, the other synced folders, then the counts-only ones.
    pub fn sidebar_extras(&self) -> impl Iterator<Item = &Folder> {
        self.folders.iter().filter(|f| {
            f.listed()
                && !matches!(
                    f.role,
                    Some(FolderRole::Inbox | FolderRole::Archive | FolderRole::Sent)
                )
        })
    }

    /// The folder holding `role`, listed or not.
    pub fn role(&self, role: FolderRole) -> Option<&Folder> {
        self.folders.iter().find(|f| f.role == Some(role))
    }

    /// The folder a person or a client named: its exact server name first,
    /// then its store key, server name or label case-insensitively, a present
    /// folder before a missing one. The exact name leads because a label can
    /// repeat a name: the folder the server marks `\Sent` is labelled `Sent`,
    /// and a second folder may be called that.
    pub fn find(&self, wanted: &str) -> Option<&Folder> {
        let wanted = wanted.trim();
        let present = || self.folders.iter().filter(|f| !f.missing);
        let loose = |f: &&Folder| {
            f.key.eq_ignore_ascii_case(wanted)
                || f.server.eq_ignore_ascii_case(wanted)
                || f.label.eq_ignore_ascii_case(wanted)
        };
        present()
            .find(|f| f.server == wanted)
            .or_else(|| present().find(loose))
            .or_else(|| self.folders.iter().find(|f| f.server == wanted))
            .or_else(|| self.folders.iter().find(loose))
    }

    /// The server name behind a store key, which is what a move, a delete and
    /// a fetch select; the key itself for a key the catalogue does not hold,
    /// which is a server name by construction.
    pub fn server_for_key(&self, key: &str) -> String {
        self.folders
            .iter()
            .find(|f| f.key == key)
            .or_else(|| {
                self.folders
                    .iter()
                    .find(|f| f.key.eq_ignore_ascii_case(key))
            })
            .map(|f| f.server.clone())
            .unwrap_or_else(|| key.to_string())
    }

    /// Whether the catalogue holds anything the server listed, as opposed to
    /// only what config names.
    pub fn is_discovered(&self) -> bool {
        self.folders
            .iter()
            .any(|f| !f.attributes.is_empty() || f.delimiter.is_some())
    }

    /// The store key a folder named `wanted` is stored under when a client
    /// fetches a message from it (`message.fetch`), with the server name it is
    /// selected by.
    ///
    /// Any folder the server lists is storable, whatever its policy: a
    /// `status-only` or `ignore` folder keeps the fetched row under its own
    /// key, where the listing and the selector find it. Refused: a name the
    /// last listing did not have (a typo, or a folder created since, which
    /// the next full tick lists), and a name that would read back as a role
    /// key or as the local drafts view, since a row under it would land in the
    /// wrong mailbox.
    pub fn storage_for(&self, wanted: &str) -> Result<(String, String), String> {
        let Some(folder) = self.find(wanted) else {
            return Err(format!(
                "the server listed no folder '{wanted}' at the last sync; it lists {}",
                self.known_names()
            ));
        };
        if !folder.keyed_by_role() && folder.pin.is_none() && shadows_reserved_key(&folder.key) {
            return Err(shadow_refusal(&folder.server));
        }
        Ok((folder.key.clone(), folder.server.clone()))
    }

    /// The target `mp sync --mailbox <name>`, a hook's `mailbox` and a client's
    /// sync of one folder resolve to.
    ///
    /// A folder `config.toml` names resolves exactly as it always did
    /// ([`crate::config::find_sync_target`]); any other folder the server
    /// lists is reachable without a config edit, whatever its policy, and
    /// downloads in full, since a person asked for it. Refused: a name the
    /// server does not list (with the names it does), a folder `SELECT`
    /// cannot open, and a name that would shadow a role or the local Drafts.
    pub fn sync_target(&self, account: &AccountConfig, name: &str) -> Result<SyncTarget, String> {
        if let Some((role, server)) = crate::config::find_sync_target(account, name) {
            return Ok(SyncTarget::new(role, server));
        }
        let Some(folder) = self.find(name) else {
            // The pre-daemon refusal's words, byte for byte (the CLI parity
            // contract), now listing every folder the server named.
            return Err(format!(
                "account '{}' has no mailbox '{name}' configured; it knows {}",
                account.name,
                self.known_names()
            ));
        };
        if !folder.selectable() {
            return Err(format!(
                "'{}' of account '{}' cannot be selected; it only holds other folders",
                folder.server, account.name
            ));
        }
        let (key, server) = self.storage_for(name)?;
        Ok(SyncTarget::new(MailboxRole::from_key(&key), server))
    }

    /// The names a refusal lists: every listed folder by key, then label when
    /// they differ.
    pub fn known_names(&self) -> String {
        let names: Vec<String> = self
            .folders
            .iter()
            .filter(|f| !f.missing && f.selectable())
            .map(|f| {
                if f.key.eq_ignore_ascii_case(&f.server) {
                    f.server.clone()
                } else {
                    format!("{} ({})", f.key, f.server)
                }
            })
            .collect();
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    }
}

/// One row of an account's sidebar hierarchy, as the daemon serves it to
/// every client: `mailbox.list`, the bootstrap snapshot, and the mailbox a
/// `message.list` or a move resolves (#0141).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarRow {
    /// `inbox`, `drafts`, `sent`, `archive`, `trash`, `junk` or `other`.
    pub role: String,
    /// The store key, which is what a client addresses.
    pub slug: String,
    pub label: String,
    /// The server folder, `None` for the local Drafts view and a role the
    /// account has no folder for.
    pub server: Option<String>,
    pub policy: FolderPolicy,
    /// The server's counts, for a `status-only` folder that has been counted.
    pub server_counts: Option<(u64, u64)>,
}

impl SidebarRow {
    /// The counts the sidebar shows: the server's for a `status-only` folder,
    /// whose store holds at most a tail of it, and the store's otherwise.
    pub fn counts(&self, store: Option<(u64, u64)>) -> (u64, u64) {
        match (self.policy, self.server_counts) {
            (FolderPolicy::StatusOnly, Some(server)) => server,
            _ => store.unwrap_or_default(),
        }
    }
}

/// The sidebar of `account`: the four fixed rows (Inbox, the local Drafts,
/// Sent, Archive), present whether or not the account has the folder, then
/// every listed folder of the catalogue in its order. The first nine are the
/// ones the number keys reach, which is why the roles and the pins lead.
pub fn sidebar(account: &AccountConfig, catalogue: &Catalogue) -> Vec<SidebarRow> {
    let role_row = |role: FolderRole| {
        let folder = catalogue.role(role).filter(|f| !f.missing);
        let configured = role.configured(account).map(|m| m.server.clone());
        SidebarRow {
            role: role.as_str().to_string(),
            slug: role
                .store_role()
                .map(|r| r.as_str().to_string())
                .unwrap_or_default(),
            label: role.label().to_string(),
            server: folder.map(|f| f.server.clone()).or(configured),
            policy: FolderPolicy::Sync,
            server_counts: None,
        }
    };
    let mut rows = vec![
        role_row(FolderRole::Inbox),
        SidebarRow {
            role: "drafts".to_string(),
            slug: DRAFTS_MAILBOX.to_string(),
            label: "Drafts".to_string(),
            server: None,
            policy: FolderPolicy::Sync,
            server_counts: None,
        },
        role_row(FolderRole::Sent),
        role_row(FolderRole::Archive),
    ];
    rows.extend(catalogue.sidebar_extras().map(|folder| SidebarRow {
        role: match folder.role {
            Some(role @ (FolderRole::Trash | FolderRole::Junk)) => role.as_str().to_string(),
            // A server drafts folder an override lists is a received mailbox:
            // `drafts` is the local view's role, which clients open as files.
            _ => "other".to_string(),
        },
        slug: folder.key.clone(),
        label: folder.label.clone(),
        server: Some(folder.server.clone()),
        policy: folder.policy,
        server_counts: folder.total.zip(folder.unread),
    }));
    rows
}

fn shadow_refusal(name: &str) -> String {
    format!(
        "'{name}' would be stored under a key a role or the local Drafts already uses; \
         map it with [accounts.mailboxes.<role>] if it is that role's folder"
    )
}

/// Whether `name`, as a store key, would read back as a role or as the local
/// drafts view.
fn shadows_reserved_key(name: &str) -> bool {
    !matches!(MailboxRole::from(name), MailboxRole::Other(_))
        || name.eq_ignore_ascii_case(DRAFTS_MAILBOX)
}

/// Every server name config pins, with its role key.
fn pinned_names(account: &AccountConfig) -> Vec<(MailboxRole, String)> {
    crate::config::all_configured_mailboxes(account)
        .into_iter()
        .map(|(role, mapping)| (role, mapping.server.clone()))
        .collect()
}

/// The folder's name without an `INBOX` parent, which is where Dovecot and
/// Courier put the role folders (`INBOX.Sent`, `INBOX/Trash`).
fn leaf_for_names(folder: &Folder) -> &str {
    if let Some(delimiter) = folder.delimiter.as_deref().filter(|d| !d.is_empty()) {
        let prefix = format!("INBOX{delimiter}");
        if folder.server.len() > prefix.len()
            && folder.server[..prefix.len()].eq_ignore_ascii_case(&prefix)
        {
            return &folder.server[prefix.len()..];
        }
    }
    &folder.server
}

/// Give each role to one folder: config, then attribute, then name; within a
/// tier, the first folder in listing order. A folder holds one role at most.
///
/// A pinned `extra` never takes one of the three keyed roles: its rows are
/// filed under its server name, and re-keying it to `sent` because the server
/// marks it `\Sent` would strand them.
fn assign_roles(account: &AccountConfig, pins: &[(MailboxRole, String)], folders: &mut [Folder]) {
    let extra_pin = |f: &Folder| {
        f.pin
            .and_then(|i| pins.get(i))
            .is_some_and(|(role, _)| matches!(role, MailboxRole::Other(_)))
    };
    let candidates = |role: FolderRole, folders: &[Folder]| -> Option<usize> {
        let free = |f: &Folder| {
            f.role.is_none()
                && !f.missing
                && f.selectable()
                && !(role.store_role().is_some() && extra_pin(f))
        };
        if let Some(mapping) = role.configured(account) {
            if let Some(i) = folders
                .iter()
                .position(|f| f.role.is_none() && f.server.eq_ignore_ascii_case(&mapping.server))
            {
                return Some(i);
            }
            // A role config names keeps the name config gave it, even when
            // the folder it names is not listed: never guessed past.
            return None;
        }
        if let Some(attribute) = role.attribute() {
            if let Some(i) = folders
                .iter()
                .position(|f| free(f) && f.attribute(attribute))
            {
                return Some(i);
            }
        }
        folders.iter().position(|f| {
            free(f)
                && role
                    .names()
                    .iter()
                    .any(|name| leaf_for_names(f).eq_ignore_ascii_case(name))
        })
    };
    for role in FolderRole::ALL {
        if let Some(i) = candidates(role, folders) {
            folders[i].role = Some(role);
            folders[i].label = role.label().to_string();
            if let Some(store_role) = role.store_role() {
                folders[i].key = store_role.as_str().to_string();
            }
        }
    }
}

/// The folder's policy, its reason, and the refusal of a key that shadows a
/// role.
fn decide(account: &AccountConfig, provider: Provider, folder: &mut Folder) {
    let ignore = |folder: &mut Folder, reason: &'static str| {
        folder.policy = FolderPolicy::Ignore;
        folder.reason = Some(reason);
    };
    if !folder.selectable() {
        return ignore(folder, "it cannot be selected");
    }
    if folder.pin.is_some() {
        folder.policy = FolderPolicy::Sync;
        return;
    }
    if folder.role == Some(FolderRole::Drafts) {
        return ignore(
            folder,
            "the Drafts view is local until #0143 mirrors the server's",
        );
    }
    if !folder.keyed_by_role() && shadows_reserved_key(&folder.key) {
        return ignore(folder, "its name would shadow a role or the local Drafts");
    }

    let lower = folder.server.to_lowercase();
    let delimiter = folder.delimiter.clone().unwrap_or_else(|| "/".to_string());
    let top = lower.split(delimiter.as_str()).next().unwrap_or_default();
    match folder.role {
        Some(FolderRole::Inbox | FolderRole::Archive | FolderRole::Sent) => {
            folder.policy = FolderPolicy::Sync;
        }
        Some(FolderRole::Drafts) => unreachable!("decided above"),
        Some(FolderRole::Trash | FolderRole::Junk) => folder.policy = FolderPolicy::StatusOnly,
        None if folder.attribute("\\All") || folder.attribute("\\Flagged") => {
            ignore(folder, "a virtual view of other folders")
        }
        None if provider == Provider::Exchange && EXCHANGE_NON_MAIL.contains(&top) => {
            ignore(folder, "not a mail folder")
        }
        None if provider == Provider::ProtonBridge && PROTON_VIRTUAL.contains(&lower.as_str()) => {
            ignore(folder, "a virtual view of other folders")
        }
        None if provider == Provider::ProtonBridge && top == "labels" => {
            folder.policy = FolderPolicy::StatusOnly;
        }
        None => folder.policy = FolderPolicy::Sync,
    }

    if let Some(rule) = account
        .mailboxes
        .overrides
        .iter()
        .find(|rule| glob_matches(&rule.pattern, &folder.server))
    {
        folder.policy = rule.policy;
        folder.reason = (rule.policy == FolderPolicy::Ignore).then_some("config overrides it");
    }
}

/// Sidebar order: the role folders, the trash and the junk folder, the pins in
/// config order, the other synced folders by name, the counts-only folders by
/// name, and the ignored and missing ones last.
fn sidebar_rank(folder: &Folder) -> (u8, usize, String) {
    let class = match (folder.role, folder.policy) {
        _ if folder.missing => 7,
        (_, FolderPolicy::Ignore) => 6,
        (Some(FolderRole::Inbox | FolderRole::Archive | FolderRole::Sent), _) => 0,
        (Some(FolderRole::Trash), _) => 1,
        (Some(FolderRole::Junk), _) => 2,
        _ if folder.pin.is_some() => 3,
        (_, FolderPolicy::Sync) => 4,
        (_, FolderPolicy::StatusOnly) => 5,
    };
    (class, folder.pin.unwrap_or(0), folder.server.to_lowercase())
}

/// Target order: the pins in `all_configured_mailboxes` order, then every
/// other ticking folder in sidebar order.
fn target_rank(folder: &Folder) -> (u8, usize, String) {
    let role_rank = match folder.role {
        Some(FolderRole::Inbox) => Some(0),
        Some(FolderRole::Archive) => Some(1),
        Some(FolderRole::Sent) => Some(2),
        _ => None,
    };
    match (role_rank, folder.pin) {
        (Some(rank), _) => (0, rank, String::new()),
        (None, Some(pin)) => (1, pin, String::new()),
        (None, None) => {
            let (class, _, name) = sidebar_rank(folder);
            (2 + class, 0, name)
        }
    }
}

#[cfg(test)]
mod tests;
