//! The dashboard's forms beyond "new token" and "share": editing a token,
//! asking an agent, accounts, projects, connecting to a bridge, the report
//! and the listen address. Each parses its own fields; the checks that
//! need the store run through the same library functions as the CLI.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crossterm::event::KeyCode;

use super::app::ProjectChoice;
use crate::accounts::{self as acct, AccountKind};
use crate::agent::AgentKind;
use crate::api::{LimitsPatch, UpdateTokenBody};
use crate::token::Token;
use crate::util::now_ms;

/// A field error: the field's index and the message shown next to it.
pub type FieldError = (usize, String);

/// Typed text that must never be shown or logged, such as an API key. Its
/// `Debug` gives only the length.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The characters typed so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.chars().count()
    }

    /// Whether nothing is typed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The text itself, to hand to the code that stores it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// One mask character per character typed, at most 32.
    #[must_use]
    pub fn masked(&self) -> String {
        let n = self.len();
        if n > 32 {
            format!("{} ({n} characters)", "•".repeat(32))
        } else {
            "•".repeat(n)
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({} chars)", self.len())
    }
}

/// Applies a typing key to `text`: a printable character is added while
/// there is room for it, Backspace removes the last one. Returns whether
/// the key was used.
pub fn edit(text: &mut String, code: KeyCode, max: usize) -> bool {
    match code {
        KeyCode::Backspace => {
            text.pop();
            true
        }
        KeyCode::Char(c) if !c.is_control() => {
            if text.chars().count() < max {
                text.push(c);
            }
            true
        }
        _ => false,
    }
}

/// Like [`edit`], for a [`Secret`].
pub fn edit_secret(secret: &mut Secret, code: KeyCode, max: usize) -> bool {
    edit(&mut secret.0, code, max)
}

/// The next index of `len` in direction `by`, wrapping.
#[must_use]
pub fn cycle(i: usize, len: usize, by: isize) -> usize {
    let n = len.max(1).cast_signed();
    (i.cast_signed() + by).rem_euclid(n).cast_unsigned()
}

/// A count such as a daily prompt cap: empty, `none` or `-` for no cap.
fn parse_count(
    s: &str,
    field: usize,
    range: std::ops::RangeInclusive<u32>,
) -> Result<Option<u32>, FieldError> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") || s == "-" {
        return Ok(None);
    }
    match s.parse::<u32>() {
        Ok(n) if range.contains(&n) => Ok(Some(n)),
        _ => Err((
            field,
            format!(
                "a whole number from {} to {}, or empty for none",
                range.start(),
                range.end()
            ),
        )),
    }
}

/// An amount in USD: empty for none.
fn parse_money(s: &str, field: usize) -> Result<Option<f64>, FieldError> {
    let s = s.trim().trim_start_matches('$');
    if s.is_empty() || s.eq_ignore_ascii_case("none") || s == "-" {
        return Ok(None);
    }
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => Ok(Some(v)),
        _ => Err((
            field,
            "an amount like 5 or 2.50, or empty for none".to_owned(),
        )),
    }
}

fn show_count(v: Option<u32>) -> String {
    v.map(|n| n.to_string()).unwrap_or_default()
}

fn show_money(v: Option<f64>) -> String {
    v.map(|n| format!("{n}")).unwrap_or_default()
}

fn bindings_text(map: &acct::AccountMap) -> String {
    if map.is_empty() {
        String::new()
    } else {
        acct::format_bindings(map)
    }
}

// ── edit token ─────────────────────────────────────────────────────────────

/// The "edit token" form, filled with the token's current settings.
#[derive(Debug, Clone)]
pub struct EditTokenForm {
    /// The token as it was when the form opened.
    pub token: Token,
    /// Focused field, `0..FIELDS`.
    pub field: usize,
    /// Label.
    pub name: String,
    /// Note; empty clears it.
    pub note: String,
    /// Days from now, `never`, or empty to keep the current expiry.
    pub expiry: String,
    /// Prompts per UTC day.
    pub daily: String,
    /// Prompts per UTC month.
    pub monthly: String,
    /// Spend per UTC day, USD.
    pub daily_budget: String,
    /// Spend per UTC month, USD.
    pub monthly_budget: String,
    /// Prompts in flight at once.
    pub max_pending: String,
    /// Accounts per agent, as `claude=work,personal codex=team`.
    pub accounts: String,
    /// The project the token is bound to.
    pub project: ProjectChoice,
    /// The field that failed and why.
    pub error: Option<FieldError>,
}

impl EditTokenForm {
    /// Number of fields.
    pub const FIELDS: usize = 10;
    /// Index of the expiry field.
    pub const EXPIRY: usize = 2;
    /// Index of the accounts field.
    pub const ACCOUNTS: usize = 8;
    /// Index of the project selector.
    pub const PROJECT: usize = 9;
    /// Labels, in field order.
    pub const LABELS: [&'static str; Self::FIELDS] = [
        "name",
        "note",
        "expires",
        "daily prompts",
        "month prompts",
        "daily budget",
        "month budget",
        "max pending",
        "accounts",
        "project",
    ];

    /// The form for `token`, with `projects` to choose from.
    #[must_use]
    pub fn new(token: &Token, projects: Vec<String>) -> Self {
        let l = token.limits;
        Self {
            field: 0,
            name: token.name.clone(),
            note: token.note.clone().unwrap_or_default(),
            expiry: String::new(),
            daily: show_count(l.daily_requests),
            monthly: show_count(l.monthly_requests),
            daily_budget: show_money(l.daily_budget_usd),
            monthly_budget: show_money(l.monthly_budget_usd),
            max_pending: show_count(l.max_pending),
            accounts: bindings_text(&token.accounts),
            project: ProjectChoice::with(projects, token.project.as_deref()),
            token: token.clone(),
            error: None,
        }
    }

    /// The text of the focused field, when it is a text field.
    pub fn text_mut(&mut self) -> Option<&mut String> {
        Some(match self.field {
            0 => &mut self.name,
            1 => &mut self.note,
            2 => &mut self.expiry,
            3 => &mut self.daily,
            4 => &mut self.monthly,
            5 => &mut self.daily_budget,
            6 => &mut self.monthly_budget,
            7 => &mut self.max_pending,
            8 => &mut self.accounts,
            _ => return None,
        })
    }

    /// What the expiry field means when left empty.
    #[must_use]
    pub fn expiry_hint(&self) -> String {
        match self.token.expires_at {
            None => "unchanged: never expires (or days, or never)".to_owned(),
            Some(at) => {
                let days = (at - now_ms()).max(0) / 86_400_000;
                format!("unchanged: {days}d left (or days, or never)")
            }
        }
    }

    /// The changes as a `PATCH` body: only fields that differ from the
    /// token. Shape errors are caught here; the rest by
    /// [`crate::api::apply_token_update`].
    ///
    /// # Errors
    ///
    /// The first field that does not parse.
    pub fn body(&self) -> Result<UpdateTokenBody, FieldError> {
        let t = &self.token;
        let mut body = UpdateTokenBody::default();
        if self.name.trim() != t.name {
            if self.name.trim().is_empty() {
                return Err((0, "name is required".to_owned()));
            }
            body.name = Some(self.name.clone());
        }
        if self.note.trim() != t.note.as_deref().unwrap_or("") {
            body.note = Some(self.note.clone());
        }
        let expiry = self.expiry.trim();
        if !expiry.is_empty() {
            body.expires_in_days = Some(if expiry.eq_ignore_ascii_case("never") {
                0
            } else {
                match expiry.parse::<u32>() {
                    Ok(n) if (1..=3650).contains(&n) => n,
                    _ => {
                        return Err((
                            Self::EXPIRY,
                            "days from now, 1 to 3650, or never".to_owned(),
                        ));
                    }
                }
            });
        }
        let limits = LimitsPatch {
            daily_requests: parse_count(&self.daily, 3, 1..=u32::MAX)?,
            monthly_requests: parse_count(&self.monthly, 4, 1..=u32::MAX)?,
            daily_budget_usd: parse_money(&self.daily_budget, 5)?,
            monthly_budget_usd: parse_money(&self.monthly_budget, 6)?,
            max_pending: Some(parse_count(&self.max_pending, 7, 1..=100)?),
        };
        if limits.apply(t.limits) != t.limits {
            body.limits = Some(limits);
        }
        if self.accounts.trim() != bindings_text(&t.accounts) {
            let map = acct::parse_bindings(&self.accounts).map_err(|e| (Self::ACCOUNTS, e))?;
            if map != t.accounts {
                body.accounts = Some(map);
            }
        }
        let project = self.project.selected().map(str::to_owned);
        if project != t.project {
            body.project = Some(project);
        }
        Ok(body)
    }
}

// ── ask ────────────────────────────────────────────────────────────────────

/// Where a prompt goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Through this server's queue.
    Local,
    /// To a bridge saved by `hashi connect`.
    Remote(String),
}

/// The prompt popup.
#[derive(Debug, Clone)]
pub struct AskForm {
    /// Focused field, `0..FIELDS`.
    pub field: usize,
    /// Where it can go: this server first, then each saved bridge.
    pub targets: Vec<Target>,
    /// Index into `targets`.
    pub target: usize,
    /// The agents to choose from for this target. `None` is the default.
    pub agents: Vec<Option<AgentKind>>,
    /// Index into `agents`.
    pub agent: usize,
    /// Accounts to pin the run to, for the chosen agent. Index 0 is "the
    /// token's list", which pins nothing.
    pub accounts: Vec<String>,
    /// Index into `accounts`.
    pub account: usize,
    /// Model override; empty for the default.
    pub model: String,
    /// The prompt, possibly several lines.
    pub prompt: String,
    /// The field that failed and why.
    pub error: Option<FieldError>,
}

impl AskForm {
    /// Number of fields.
    pub const FIELDS: usize = 5;
    /// Index of the account selector.
    pub const ACCOUNT: usize = 2;
    /// Index of the model field.
    pub const MODEL: usize = 3;
    /// Index of the prompt field.
    pub const PROMPT: usize = 4;
    /// Longest prompt the popup takes, in characters.
    pub const PROMPT_CHARS: usize = 16_000;

    /// A form for this server's `installed` agents and the saved `remotes`.
    #[must_use]
    pub fn new(installed: Vec<AgentKind>, remotes: Vec<String>) -> Self {
        let mut targets = vec![Target::Local];
        targets.extend(remotes.into_iter().map(Target::Remote));
        let mut form = Self {
            field: Self::PROMPT,
            targets,
            target: 0,
            agents: Vec::new(),
            agent: 0,
            accounts: Vec::new(),
            account: 0,
            model: String::new(),
            prompt: String::new(),
            error: None,
        };
        form.set_agents(installed);
        form
    }

    /// The chosen target.
    #[must_use]
    pub fn target(&self) -> &Target {
        self.targets.get(self.target).unwrap_or(&Target::Local)
    }

    /// The chosen agent; `None` is the default.
    #[must_use]
    pub fn agent(&self) -> Option<AgentKind> {
        self.agents.get(self.agent).copied().flatten()
    }

    /// The account to pin to, if one is chosen.
    #[must_use]
    pub fn account(&self) -> Option<&str> {
        (self.account > 0)
            .then(|| self.accounts.get(self.account).map(String::as_str))
            .flatten()
    }

    /// Replaces the agent choices: `installed` for this server (the default
    /// first), every agent for a bridge.
    pub fn set_agents(&mut self, installed: Vec<AgentKind>) {
        self.agents = match self.target() {
            Target::Local => installed.into_iter().map(Some).collect(),
            Target::Remote(_) => std::iter::once(None)
                .chain(AgentKind::ALL.into_iter().map(Some))
                .collect(),
        };
        if self.agents.is_empty() {
            self.agents.push(None);
        }
        self.agent = 0;
    }

    /// Replaces the account choices for the chosen agent.
    pub fn set_accounts(&mut self, names: Vec<String>) {
        self.accounts = std::iter::once("the token's list".to_owned())
            .chain(names)
            .collect();
        self.account = 0;
    }

    /// Whether the account field applies: only for this server.
    #[must_use]
    pub fn has_accounts(&self) -> bool {
        *self.target() == Target::Local
    }

    /// Checks the fields that need no store.
    ///
    /// # Errors
    ///
    /// The first field that is wrong.
    pub fn check(&self) -> Result<(), FieldError> {
        if self.prompt.trim().is_empty() {
            return Err((Self::PROMPT, "type a prompt first".to_owned()));
        }
        let model = self.model.trim();
        if !model.is_empty() && !crate::agent::is_safe_arg(model) {
            return Err((
                Self::MODEL,
                "model contains unsupported characters".to_owned(),
            ));
        }
        if *self.target() == Target::Local && self.agent().is_none() {
            return Err((1, "no agent is installed on this machine".to_owned()));
        }
        Ok(())
    }
}

// ── accounts ───────────────────────────────────────────────────────────────

/// The "add account" form.
#[derive(Debug, Clone)]
pub struct AccountForm {
    /// Focused field, `0..FIELDS`.
    pub field: usize,
    /// The agent.
    pub agent: AgentKind,
    /// The account name.
    pub name: String,
    /// How it signs in.
    pub kind: AccountKind,
    /// The key, for a key account. Masked on screen.
    pub key: Secret,
    /// The field that failed and why.
    pub error: Option<FieldError>,
}

impl AccountForm {
    /// Number of fields.
    pub const FIELDS: usize = 4;
    /// Index of the key field.
    pub const KEY: usize = 3;
    /// Longest key accepted, in characters.
    pub const KEY_CHARS: usize = 4096;

    /// An empty form for Claude Code's login accounts.
    #[must_use]
    pub fn new() -> Self {
        Self {
            field: 1,
            agent: AgentKind::Claude,
            name: String::new(),
            kind: AccountKind::Login,
            key: Secret::default(),
            error: None,
        }
    }

    /// The kinds `agent` supports.
    #[must_use]
    pub fn kinds(agent: AgentKind) -> Vec<AccountKind> {
        [
            AccountKind::Login,
            AccountKind::ApiKey,
            AccountKind::OauthToken,
        ]
        .into_iter()
        .filter(|k| acct::supports(agent, *k))
        .collect()
    }

    /// Moves the focused choice by `by`.
    pub fn step(&mut self, by: isize) {
        match self.field {
            0 => {
                let all = AgentKind::ALL;
                let i = all.iter().position(|a| *a == self.agent).unwrap_or(0);
                self.agent = all[cycle(i, all.len(), by)];
                if !acct::supports(self.agent, self.kind) {
                    self.kind = Self::kinds(self.agent)
                        .first()
                        .copied()
                        .unwrap_or(AccountKind::Login);
                }
            }
            2 => {
                let kinds = Self::kinds(self.agent);
                let i = kinds.iter().position(|k| *k == self.kind).unwrap_or(0);
                if let Some(k) = kinds.get(cycle(i, kinds.len(), by)) {
                    self.kind = *k;
                }
            }
            _ => {}
        }
        self.error = None;
    }

    /// The fields shown: the key only for a key account.
    #[must_use]
    pub fn fields(&self) -> usize {
        if self.kind.is_key() {
            Self::FIELDS
        } else {
            Self::FIELDS - 1
        }
    }

    /// The cleaned key, for a key account.
    ///
    /// # Errors
    ///
    /// When the key is missing or malformed.
    pub fn clean_key(&self) -> Result<Option<String>, FieldError> {
        if !self.kind.is_key() {
            return Ok(None);
        }
        if self.key.is_empty() {
            return Err((Self::KEY, "paste the key".to_owned()));
        }
        acct::clean_key(self.key.expose())
            .map(Some)
            .map_err(|e| (Self::KEY, e))
    }
}

impl Default for AccountForm {
    fn default() -> Self {
        Self::new()
    }
}

// ── projects ───────────────────────────────────────────────────────────────

/// The "add project" and "edit project" form.
#[derive(Debug, Clone)]
pub struct ProjectForm {
    /// The project's name when editing; `None` when adding.
    pub editing: Option<String>,
    /// Focused field, `0..FIELDS`.
    pub field: usize,
    /// Name (fixed when editing).
    pub name: String,
    /// Absolute folder path.
    pub path: String,
    /// Agents, comma separated. Empty when adding: the installed ones.
    pub agents: String,
    /// Accounts per agent, as `claude=work,personal`.
    pub accounts: String,
    /// The default agent, if any.
    pub default_agent: Option<AgentKind>,
    /// Models per agent, as `claude=sonnet codex=gpt-5`.
    pub models: String,
    /// Who or what it is for.
    pub note: String,
    /// The field that failed and why.
    pub error: Option<FieldError>,
}

impl ProjectForm {
    /// Number of fields.
    pub const FIELDS: usize = 7;
    /// Index of the default agent selector.
    pub const DEFAULT_AGENT: usize = 4;
    /// Labels, in field order.
    pub const LABELS: [&'static str; Self::FIELDS] = [
        "name",
        "folder",
        "agents",
        "accounts",
        "default agent",
        "models",
        "note",
    ];

    /// An empty form for a new project.
    #[must_use]
    pub fn add() -> Self {
        Self {
            editing: None,
            field: 0,
            name: String::new(),
            path: String::new(),
            agents: String::new(),
            accounts: String::new(),
            default_agent: None,
            models: String::new(),
            note: String::new(),
            error: None,
        }
    }

    /// A form filled with `p`'s settings.
    #[must_use]
    pub fn edit(p: &crate::projects::Project) -> Self {
        Self {
            editing: Some(p.name.clone()),
            field: 1,
            name: p.name.clone(),
            path: p.path.clone(),
            agents: crate::token::encode_agents(&p.agents),
            accounts: bindings_text(&p.accounts),
            default_agent: p.default_agent,
            models: p
                .models
                .iter()
                .map(|(a, m)| format!("{a}={m}"))
                .collect::<Vec<_>>()
                .join(" "),
            note: p.note.clone().unwrap_or_default(),
            error: None,
        }
    }

    /// The text of the focused field, when it is an editable text field.
    pub fn text_mut(&mut self) -> Option<&mut String> {
        Some(match self.field {
            0 if self.editing.is_none() => &mut self.name,
            1 => &mut self.path,
            2 => &mut self.agents,
            3 => &mut self.accounts,
            5 => &mut self.models,
            6 => &mut self.note,
            _ => return None,
        })
    }

    /// Moves the default agent choice by `by`.
    pub fn step(&mut self, by: isize) {
        if self.field != Self::DEFAULT_AGENT {
            return;
        }
        let options: Vec<Option<AgentKind>> = std::iter::once(None)
            .chain(AgentKind::ALL.into_iter().map(Some))
            .collect();
        let i = options
            .iter()
            .position(|a| *a == self.default_agent)
            .unwrap_or(0);
        self.default_agent = options[cycle(i, options.len(), by)];
    }

    /// The agents typed, or `None` when the field is empty.
    ///
    /// # Errors
    ///
    /// An unknown agent.
    pub fn parse_agents(&self) -> Result<Option<Vec<AgentKind>>, FieldError> {
        let text = self.agents.trim();
        if text.is_empty() {
            return Ok(None);
        }
        text.split([',', ' '])
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.trim()
                    .parse::<AgentKind>()
                    .map_err(|e| (2, e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// The accounts typed.
    ///
    /// # Errors
    ///
    /// A malformed binding.
    pub fn parse_accounts(&self) -> Result<acct::AccountMap, FieldError> {
        acct::parse_bindings(&self.accounts).map_err(|e| (3, e))
    }

    /// The models typed.
    ///
    /// # Errors
    ///
    /// A malformed `agent=model` pair.
    pub fn parse_models(&self) -> Result<crate::projects::ModelMap, FieldError> {
        let mut map = crate::projects::ModelMap::new();
        for part in self
            .models
            .split([' ', ';'])
            .filter(|p| !p.trim().is_empty())
        {
            let (agent, model) = crate::projects::parse_model(part.trim()).map_err(|e| (5, e))?;
            map.insert(agent, model);
        }
        Ok(map)
    }
}

// ── connect ────────────────────────────────────────────────────────────────

/// The "connect to a bridge" form.
#[derive(Debug, Clone, Default)]
pub struct ConnectForm {
    /// Focused field: 0 the link, 1 the name.
    pub field: usize,
    /// The pasted invite link.
    pub link: String,
    /// Name to save it under; empty for the server's name.
    pub name: String,
    /// Waiting for the server to answer.
    pub busy: bool,
    /// The field that failed and why.
    pub error: Option<FieldError>,
}

impl ConnectForm {
    /// Number of fields.
    pub const FIELDS: usize = 2;
}

// ── report ─────────────────────────────────────────────────────────────────

/// The report popup's choices.
#[derive(Debug, Clone, Default)]
pub struct ReportForm {
    /// Focused field: 0 prompts, 1 destination.
    pub field: usize,
    /// Include prompt excerpts and agent error output (`--include-prompts`).
    pub include_prompts: bool,
    /// Save in `<home>/reports/` instead of the current directory.
    pub to_home: bool,
    /// Why writing failed.
    pub error: Option<String>,
}

impl ReportForm {
    /// Number of fields.
    pub const FIELDS: usize = 2;
}

// ── listen ─────────────────────────────────────────────────────────────────

/// The bind addresses offered besides a typed one.
pub const BINDS: [(IpAddr, &str); 3] = [
    (IpAddr::V4(Ipv4Addr::LOCALHOST), "this machine only"),
    (IpAddr::V4(Ipv4Addr::UNSPECIFIED), "all IPv4 interfaces"),
    (IpAddr::V6(Ipv6Addr::UNSPECIFIED), "all interfaces"),
];

/// The "port and bind address" form.
#[derive(Debug, Clone)]
pub struct ListenForm {
    /// Focused field, `0..FIELDS`.
    pub field: usize,
    /// Port being typed.
    pub port: String,
    /// Index into [`BINDS`], or `BINDS.len()` for a typed address.
    pub bind: usize,
    /// The typed address, such as a Tailscale IP.
    pub other: String,
    /// Save the choice in `<home>/settings.json` for the next start.
    pub keep: bool,
    /// The field that failed and why.
    pub error: Option<FieldError>,
}

impl ListenForm {
    /// Number of fields.
    pub const FIELDS: usize = 4;
    /// Index of the port field.
    pub const PORT: usize = 0;
    /// Index of the bind selector.
    pub const BIND: usize = 1;
    /// Index of the typed address.
    pub const OTHER: usize = 2;
    /// Index of the "keep for next start" toggle.
    pub const KEEP: usize = 3;

    /// The form for a server listening on `current`.
    #[must_use]
    pub fn new(current: Option<SocketAddr>) -> Self {
        let addr = current.unwrap_or_else(|| {
            SocketAddr::new(crate::settings::DEFAULT_BIND, crate::settings::DEFAULT_PORT)
        });
        let known = BINDS.iter().position(|(ip, _)| *ip == addr.ip());
        Self {
            field: Self::PORT,
            port: addr.port().to_string(),
            bind: known.unwrap_or(BINDS.len()),
            other: if known.is_some() {
                String::new()
            } else {
                addr.ip().to_string()
            },
            keep: true,
            error: None,
        }
    }

    /// Moves the focused choice by `by`.
    pub fn step(&mut self, by: isize) {
        match self.field {
            Self::BIND => self.bind = cycle(self.bind, BINDS.len() + 1, by),
            Self::KEEP => self.keep = !self.keep,
            _ => {}
        }
        self.error = None;
    }

    /// The label of the bind choice.
    #[must_use]
    pub fn bind_label(&self) -> String {
        match BINDS.get(self.bind) {
            Some((ip, what)) => format!("{ip}  {what}"),
            None => "another address (type it below)".to_owned(),
        }
    }

    /// The address chosen.
    ///
    /// # Errors
    ///
    /// A port outside 1 to 65535 or an address that does not parse.
    pub fn addr(&self) -> Result<SocketAddr, FieldError> {
        let port = match self.port.trim().parse::<u16>() {
            Ok(p) if p >= 1 => p,
            _ => return Err((Self::PORT, "a port from 1 to 65535".to_owned())),
        };
        let ip = match BINDS.get(self.bind) {
            Some((ip, _)) => *ip,
            None => self
                .other
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .map_err(|_| {
                    (
                        Self::OTHER,
                        "an IP address such as 100.64.0.7 or fd7a::1".to_owned(),
                    )
                })?,
        };
        Ok(SocketAddr::new(ip, port))
    }
}
