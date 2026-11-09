//! Every `hashi` subcommand and where the dashboard does the same thing.
//!
//! A test in `src/main.rs` walks the clap command tree and fails when a
//! subcommand has no row here, or a row names a subcommand that is gone.

use super::app::Action;

/// Where the dashboard does what a subcommand does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// A Commands-tab row, which shows the command it stands for.
    Row(Action),
    /// A key in one of the five views.
    Key {
        /// The view, as its tab names it.
        view: &'static str,
        /// The key.
        key: &'static str,
    },
    /// A key in the popup that a Commands-tab row opens.
    InPopup {
        /// The row that opens the popup.
        row: Action,
        /// The key in the popup.
        key: &'static str,
    },
    /// Deliberately not in the dashboard.
    Excluded(&'static str),
}

/// Each subcommand path, as words separated by spaces, and its entry.
pub const TABLE: &[(&str, Entry)] = &[
    (
        "serve",
        Entry::Excluded("the dashboard is the running server; its flags apply only at start"),
    ),
    (
        "uninstall",
        Entry::Excluded("removes Hashi itself; run it from a terminal once Hashi has stopped"),
    ),
    ("token create", Entry::Row(Action::NewToken)),
    (
        "token list",
        Entry::Key {
            view: "Tokens",
            key: "3",
        },
    ),
    (
        "token update",
        Entry::Key {
            view: "Tokens",
            key: "e (p pauses and resumes)",
        },
    ),
    (
        "token revoke",
        Entry::Key {
            view: "Tokens",
            key: "d",
        },
    ),
    ("share", Entry::Row(Action::NewInvite)),
    ("share-login", Entry::Row(Action::ShareLogin)),
    ("pool status", Entry::Row(Action::Pool)),
    ("pool list", Entry::Row(Action::Pool)),
    (
        "pool open",
        Entry::InPopup {
            row: Action::Pool,
            key: "o",
        },
    ),
    (
        "pool close",
        Entry::InPopup {
            row: Action::Pool,
            key: "c",
        },
    ),
    (
        "pool kick",
        Entry::InPopup {
            row: Action::Pool,
            key: "x",
        },
    ),
    (
        "pool join",
        Entry::InPopup {
            row: Action::Pool,
            key: "a",
        },
    ),
    (
        "pool leave",
        Entry::InPopup {
            row: Action::Pool,
            key: "l",
        },
    ),
    ("requests list", Entry::Row(Action::AccessRequests)),
    (
        "requests accept",
        Entry::InPopup {
            row: Action::AccessRequests,
            key: "a",
        },
    ),
    (
        "requests decline",
        Entry::InPopup {
            row: Action::AccessRequests,
            key: "d",
        },
    ),
    (
        "requests open",
        Entry::InPopup {
            row: Action::AccessRequests,
            key: "o",
        },
    ),
    (
        "requests close",
        Entry::InPopup {
            row: Action::AccessRequests,
            key: "x",
        },
    ),
    (
        "invite list",
        Entry::Key {
            view: "Tokens",
            key: "i",
        },
    ),
    (
        "invite cancel",
        Entry::Key {
            view: "Tokens",
            key: "i, then x",
        },
    ),
    (
        "account add",
        Entry::InPopup {
            row: Action::Accounts,
            key: "a",
        },
    ),
    (
        "account login",
        Entry::InPopup {
            row: Action::Accounts,
            key: "l",
        },
    ),
    ("account list", Entry::Row(Action::Accounts)),
    // Usage gauges show for the selected account in the Accounts popup,
    // and u reads them now (`--refresh`).
    (
        "account usage",
        Entry::InPopup {
            row: Action::Accounts,
            key: "u",
        },
    ),
    (
        "account disable",
        Entry::InPopup {
            row: Action::Accounts,
            key: "d",
        },
    ),
    (
        "account enable",
        Entry::InPopup {
            row: Action::Accounts,
            key: "e",
        },
    ),
    (
        "account clear-limit",
        Entry::InPopup {
            row: Action::Accounts,
            key: "c",
        },
    ),
    (
        "account remove",
        Entry::InPopup {
            row: Action::Accounts,
            key: "x",
        },
    ),
    (
        "project add",
        Entry::InPopup {
            row: Action::Projects,
            key: "a",
        },
    ),
    (
        "project update",
        Entry::InPopup {
            row: Action::Projects,
            key: "e",
        },
    ),
    ("project list", Entry::Row(Action::Projects)),
    (
        "project remove",
        Entry::InPopup {
            row: Action::Projects,
            key: "x",
        },
    ),
    ("audit", Entry::Row(Action::Audit)),
    ("ask", Entry::Row(Action::Ask)),
    ("connect", Entry::Row(Action::Connect)),
    ("remote list", Entry::Row(Action::Bridges)),
    // Requests this machine sent; asking itself needs the link, so it
    // stays `hashi request <link>`.
    (
        "request status",
        Entry::InPopup {
            row: Action::Bridges,
            key: "r",
        },
    ),
    (
        "remote remove",
        Entry::InPopup {
            row: Action::Bridges,
            key: "x",
        },
    ),
    ("doctor", Entry::Row(Action::Doctor)),
    ("report", Entry::Row(Action::Report)),
    ("update", Entry::Row(Action::InstallUpdate)),
    ("install", Entry::Row(Action::InstallCloudflared)),
    ("llm", Entry::Row(Action::CopyGuide)),
    // `hashi guide` is the same command under its alias.
    ("guide", Entry::Row(Action::CopyGuide)),
];

/// The entry for a subcommand path such as `token update`.
#[must_use]
pub fn entry(path: &str) -> Option<Entry> {
    TABLE.iter().find(|(p, _)| *p == path).map(|(_, e)| *e)
}

/// How many entries each kind has: rows, view keys, popup keys, excluded.
#[must_use]
pub fn counts() -> (usize, usize, usize, usize) {
    TABLE
        .iter()
        .fold((0, 0, 0, 0), |(r, k, p, x), (_, e)| match e {
            Entry::Row(_) => (r + 1, k, p, x),
            Entry::Key { .. } => (r, k + 1, p, x),
            Entry::InPopup { .. } => (r, k, p + 1, x),
            Entry::Excluded(_) => (r, k, p, x + 1),
        })
}
