//! What the views read besides the refreshed model, loaded at most once per
//! refresh instead of once per frame.
//!
//! [`App::refresh`] empties the cache, and so does a key press (it may have
//! added an account or a project). The first frame that needs an entry
//! loads it through [`App::prepare_frame`]; later frames reuse it. The
//! Tokens view reads the use of the rows it draws, and only those, through
//! [`App::load_token_usage`]. A view that finds an entry missing reads the
//! store itself, so a stale cache never shows the wrong thing for longer
//! than one refresh.

use std::borrow::Cow;
use std::collections::HashSet;

use super::app::{App, Popup, Tab};
use super::usage::AccountUsage;
use crate::projects::Project;
use crate::store::AccountState;
use crate::util::now_ms;

/// One row of the Accounts popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    /// The account.
    pub state: AccountState,
    /// Whether it is signed in, as [`App::signed_in`] says.
    pub signed_in: String,
    /// Tokens that list it.
    pub tokens: usize,
}

/// Entries loaded for the frames between two refreshes.
#[derive(Debug, Default)]
pub struct FrameCache {
    /// Every account's usage, under the rotate threshold it was read with.
    usage: Option<(u8, Vec<AccountUsage>)>,
    /// Accounts besides the default ones.
    accounts: Option<Vec<AccountState>>,
    /// The Accounts popup's rows.
    account_rows: Option<Vec<AccountRow>>,
    /// Projects.
    projects: Option<Vec<Project>>,
    /// How many bridges are saved.
    remotes: Option<usize>,
    /// Whether the ask-access link is open.
    link_open: Option<bool>,
    /// The tokens whose [`App::usage`] entry was read since the last
    /// refresh.
    token_usage: HashSet<String>,
}

impl FrameCache {
    /// Forgets everything: the next frame reads afresh.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Forgets what a key press can change: everything but the tokens'
    /// use, which only requests change.
    pub fn clear_after_key(&mut self) {
        let token_usage = std::mem::take(&mut self.token_usage);
        self.clear();
        self.token_usage = token_usage;
    }
}

impl App {
    /// Loads what the current view and popup read and the cache lacks.
    /// Called once at the start of every frame.
    pub fn prepare_frame(&mut self) {
        let accounts_popup = matches!(self.popup, Some(Popup::Accounts { .. }));
        let projects_popup = matches!(self.popup, Some(Popup::Projects { .. }));
        let commands = self.tab == Tab::Commands;
        let rotate_at = self.rotate_at();
        if (self.tab == Tab::Overview || accounts_popup)
            && self
                .cache
                .usage
                .as_ref()
                .is_none_or(|(r, _)| *r != rotate_at)
        {
            let snapshot = super::usage::snapshot(&self.state.store, rotate_at);
            self.cache.usage = Some((rotate_at, snapshot));
        }
        if (commands || accounts_popup) && self.cache.accounts.is_none() {
            self.cache.accounts = Some(self.account_rows());
        }
        if accounts_popup && self.cache.account_rows.is_none() {
            let rows = self.account_table().into_owned();
            self.cache.account_rows = Some(rows);
        }
        if (commands || projects_popup) && self.cache.projects.is_none() {
            self.cache.projects = Some(self.state.store.list_projects().unwrap_or_default());
        }
        if commands {
            if self.cache.remotes.is_none() {
                self.cache.remotes = Some(self.remote_names().len());
            }
            if self.cache.link_open.is_none() {
                let open = crate::access::current_link(&self.state.store)
                    .ok()
                    .flatten()
                    .is_some_and(|l| l.status(now_ms()) == "open");
                self.cache.link_open = Some(open);
            }
        }
    }

    /// Reads today's and this month's use of the tokens in `rows` (indexes
    /// into [`App::tokens`]) that were not read since the last refresh. The
    /// Tokens view calls it for the rows it draws.
    pub fn load_token_usage(&mut self, rows: std::ops::Range<usize>) {
        let now = now_ms();
        let end = rows.end.min(self.tokens.len());
        for tk in self
            .tokens
            .get(rows.start.min(end)..end)
            .unwrap_or_default()
        {
            if self.cache.token_usage.contains(&tk.id) {
                continue;
            }
            match self.state.store.token_usage(&tk.id, now) {
                Ok(u) => {
                    self.usage.insert(tk.id.clone(), u);
                    self.cache.token_usage.insert(tk.id.clone());
                }
                Err(e) => {
                    self.notify(format!("refresh failed: {e}"));
                    return;
                }
            }
        }
    }

    /// The percent used at which runs move off an account.
    pub(crate) fn rotate_at(&self) -> u8 {
        self.state.dispatcher.usage().policy().rotate_at
    }

    /// Every account's usage, under the current rotate threshold.
    #[must_use]
    pub fn usage_snapshot(&self) -> Cow<'_, [AccountUsage]> {
        let rotate_at = self.rotate_at();
        match &self.cache.usage {
            Some((r, v)) if *r == rotate_at => Cow::Borrowed(v),
            _ => Cow::Owned(super::usage::snapshot(&self.state.store, rotate_at)),
        }
    }

    /// Accounts besides the default ones.
    #[must_use]
    pub fn cached_accounts(&self) -> Cow<'_, [AccountState]> {
        match &self.cache.accounts {
            Some(v) => Cow::Borrowed(v),
            None => Cow::Owned(self.account_rows()),
        }
    }

    /// The Accounts popup's rows. The token counts come from the refreshed
    /// token list, as `Store::tokens_using_account` counts them.
    #[must_use]
    pub fn account_table(&self) -> Cow<'_, [AccountRow]> {
        if let Some(v) = &self.cache.account_rows {
            return Cow::Borrowed(v);
        }
        let rows = self
            .cached_accounts()
            .iter()
            .map(|a| AccountRow {
                signed_in: self.signed_in(a),
                tokens: self
                    .tokens
                    .iter()
                    .filter(|t| {
                        t.accounts
                            .get(&a.agent)
                            .is_some_and(|l| l.contains(&a.name))
                    })
                    .count(),
                state: a.clone(),
            })
            .collect();
        Cow::Owned(rows)
    }

    /// Projects.
    #[must_use]
    pub fn cached_projects(&self) -> Cow<'_, [Project]> {
        match &self.cache.projects {
            Some(v) => Cow::Borrowed(v),
            None => Cow::Owned(self.state.store.list_projects().unwrap_or_default()),
        }
    }

    /// How many tokens are bound to project `name`, from the refreshed
    /// token list.
    #[must_use]
    pub fn project_token_count(&self, name: &str) -> usize {
        self.tokens
            .iter()
            .filter(|t| t.project.as_deref() == Some(name))
            .count()
    }

    /// How many bridges are saved.
    #[must_use]
    pub fn remote_count(&self) -> usize {
        self.cache
            .remotes
            .unwrap_or_else(|| self.remote_names().len())
    }

    /// Whether the ask-access link is open.
    #[must_use]
    pub fn link_open(&self) -> bool {
        self.cache.link_open.unwrap_or_else(|| {
            crate::access::current_link(&self.state.store)
                .ok()
                .flatten()
                .is_some_and(|l| l.status(now_ms()) == "open")
        })
    }
}
