//! Key handling and actions for the dashboard's management popups: token
//! edit, the prompt popup, accounts, projects, bridges, the audit log, the
//! doctor, the report, updates and the listen address. Each change goes
//! through the library function the CLI uses and is audited as
//! `dashboard`.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{Action, App, Done, PendingCommand, Popup, Tab};
use super::forms::{
    self, AccountForm, AskForm, ConnectForm, EditTokenForm, FieldError, ListenForm, ProjectForm,
    ReportForm, Target, cycle, edit, edit_secret,
};
use crate::accounts::{self as acct, AccountKind};
use crate::agent::{AgentConfig, AgentKind};
use crate::token::{Token, TokenStatus};
use crate::util::now_ms;

/// Who the dashboard's changes are audited as.
const ACTOR: &str = "dashboard";

impl Popup {
    /// Whether the popup has a blinking cursor or a spinner.
    #[must_use]
    pub fn animates(&self) -> bool {
        matches!(
            self,
            Self::Share(_)
                | Self::NewToken(_)
                | Self::EditToken(_)
                | Self::Ask(_)
                | Self::AddAccount(_)
                | Self::Project(_)
                | Self::Connect(_)
                | Self::Listen(_)
                | Self::RemoteAnswer { answer: None, .. }
        )
    }
}

/// Up, down or tab through `n` fields, skipping those `skip` refuses.
fn nav(field: &mut usize, n: usize, code: KeyCode, skip: impl Fn(usize) -> bool) -> bool {
    let by: isize = match code {
        KeyCode::Down | KeyCode::Tab => 1,
        KeyCode::Up | KeyCode::BackTab => -1,
        _ => return false,
    };
    let mut next = *field;
    for _ in 0..n {
        next = cycle(next, n, by);
        if !skip(next) {
            break;
        }
    }
    *field = next;
    true
}

/// Moves a list selection.
fn list_nav(selected: &mut usize, len: usize, code: KeyCode) -> bool {
    match code {
        KeyCode::Down | KeyCode::Char('j') => {
            *selected = (*selected + 1).min(len.saturating_sub(1));
            true
        }
        KeyCode::Up | KeyCode::Char('k') => {
            *selected = selected.saturating_sub(1);
            true
        }
        _ => false,
    }
}

fn closes(code: KeyCode) -> bool {
    matches!(code, KeyCode::Esc | KeyCode::Char('q'))
}

/// The field an API error is most likely about, by its wording.
fn field_for(msg: &str, fields: &[(&str, usize)], fallback: usize) -> usize {
    let lower = msg.to_ascii_lowercase();
    fields
        .iter()
        .find(|(word, _)| lower.contains(word))
        .map_or(fallback, |(_, f)| *f)
}

fn nothing(body: &crate::api::UpdateTokenBody) -> bool {
    body.name.is_none()
        && body.note.is_none()
        && body.paused.is_none()
        && body.expires_in_days.is_none()
        && body.limits.is_none()
        && body.accounts.is_none()
        && body.project.is_none()
}

impl App {
    /// The Hashi home, when the dashboard runs on a real server.
    #[must_use]
    pub fn home(&self) -> Option<PathBuf> {
        self.state.store.data_dir()
    }

    /// Picks up background work that has finished. Returns whether any did.
    pub(crate) fn take_done(&mut self) -> bool {
        let done: Vec<Done> = match self.done.lock() {
            Ok(mut d) => std::mem::take(&mut *d),
            Err(_) => return false,
        };
        let any = !done.is_empty();
        for d in done {
            match d {
                Done::Connected(Ok(name)) => {
                    self.state.bus.log(format!("connected to a bridge: {name}"));
                    self.notify(format!("connected; saved as {name}"));
                    if matches!(self.popup, Some(Popup::Connect(_))) {
                        let selected = self
                            .remote_names()
                            .iter()
                            .position(|n| *n == name)
                            .unwrap_or(0);
                        self.popup = Some(Popup::Remotes { selected });
                    }
                }
                Done::Connected(Err(e)) => match &mut self.popup {
                    Some(Popup::Connect(form)) => {
                        form.busy = false;
                        form.error = Some((0, e));
                    }
                    _ => self.notify(format!("connect failed: {e}")),
                },
                Done::Answer { remote, result } => match &mut self.popup {
                    Some(Popup::RemoteAnswer {
                        remote: r, answer, ..
                    }) if *r == remote => *answer = Some(result),
                    _ => self.notify(format!("{remote} answered; ask again to see it")),
                },
            }
        }
        any
    }

    /// Queues `done` for the next tick, from any thread.
    fn finish(slot: &super::app::Shared<Vec<Done>>, done: Done) {
        if let Ok(mut d) = slot.lock() {
            d.push(done);
        }
    }

    /// The owner's admin token the prompt popup sends as: the oldest active
    /// admin token.
    #[must_use]
    pub fn owner_token(&self) -> Option<Token> {
        let now = now_ms();
        self.tokens
            .iter()
            .filter(|t| t.is_admin() && t.status(now) == TokenStatus::Active)
            .min_by_key(|t| t.created_at)
            .cloned()
    }

    /// The installed agents, the server's default first.
    fn installed_agents(&self) -> Vec<AgentKind> {
        let mut v: Vec<AgentKind> = self
            .state
            .runtime
            .agents
            .iter()
            .filter(|a| a.available)
            .map(|a| a.agent)
            .collect();
        if let Some(i) = v.iter().position(|a| *a == self.state.default_agent) {
            v.swap(0, i);
        }
        v
    }

    /// The executable of `agent`, as the server found it.
    fn agent_bin(&self, agent: AgentKind) -> PathBuf {
        self.state
            .runtime
            .agents
            .iter()
            .find(|a| a.agent == agent)
            .map_or_else(
                || AgentConfig::default_for(agent).bin,
                |a| PathBuf::from(&a.path),
            )
    }

    /// The bridges saved by `hashi connect`.
    #[must_use]
    pub fn remote_names(&self) -> Vec<String> {
        self.home()
            .and_then(|h| crate::remote::load(&h).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.name)
            .collect()
    }

    /// Accounts besides the default, for the Accounts popup.
    #[must_use]
    pub fn account_rows(&self) -> Vec<crate::store::AccountState> {
        self.state
            .store
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.kind != AccountKind::Default)
            .collect()
    }

    /// Whether the account is signed in, without running anything.
    #[must_use]
    pub fn signed_in(&self, a: &crate::store::AccountState) -> String {
        match self.home() {
            Some(home) => acct::ops::signed_in(
                &home,
                a.agent,
                &a.name,
                a.kind,
                &self.agent_bin(a.agent),
                false,
            ),
            None => "-".to_owned(),
        }
    }

    fn ask_accounts(&self, agent: Option<AgentKind>) -> Vec<String> {
        let Some(agent) = agent else {
            return Vec::new();
        };
        let now = now_ms();
        std::iter::once(acct::DEFAULT.to_owned())
            .chain(
                self.account_rows()
                    .into_iter()
                    .filter(|a| {
                        a.agent == agent && a.status(now) == crate::store::AccountStatus::Ready
                    })
                    .map(|a| a.name),
            )
            .collect()
    }

    /// Opens the prompt popup, aimed at `remote` when given.
    pub fn open_ask(&mut self, remote: Option<&str>) {
        let mut form = AskForm::new(self.installed_agents(), self.remote_names());
        if let Some(r) = remote
            && let Some(i) = form
                .targets
                .iter()
                .position(|t| *t == Target::Remote(r.to_owned()))
        {
            form.target = i;
            form.set_agents(Vec::new());
        }
        form.set_accounts(self.ask_accounts(form.agent()));
        self.popup = Some(Popup::Ask(Box::new(form)));
    }

    /// Opens the edit form on the selected token.
    pub fn open_edit_token(&mut self) {
        if let Some(t) = self.selected_token().cloned() {
            let form = EditTokenForm::new(&t, self.project_names());
            self.popup = Some(Popup::EditToken(Box::new(form)));
        }
    }

    /// Runs a Commands row that opens a management popup.
    pub(crate) fn run_manage_action(&mut self, action: Action) {
        self.popup = match action {
            Action::Ask => {
                self.open_ask(None);
                return;
            }
            Action::Accounts => Some(Popup::Accounts { selected: 0 }),
            Action::Projects => Some(Popup::Projects { selected: 0 }),
            Action::Bridges => Some(Popup::Remotes { selected: 0 }),
            Action::Connect => Some(Popup::Connect(Box::default())),
            Action::Listen => Some(Popup::Listen(Box::new(ListenForm::new(
                self.state.runtime.listen.addr(),
            )))),
            Action::InstallUpdate => Some(Popup::ConfirmUpdate),
            Action::Doctor => {
                self.probe_tailscale();
                Some(Popup::Doctor)
            }
            Action::Report => Some(Popup::Report(Box::default())),
            Action::Audit => Some(Popup::Audit { scroll: 0 }),
            _ => return,
        };
    }

    /// Keys for the management popups. Returns the popup to show next.
    pub(crate) fn on_manage_key(&mut self, popup: Popup, key: KeyEvent) -> Option<Popup> {
        let code = key.code;
        match popup {
            Popup::EditToken(form) => self.edit_token_key(form, code),
            Popup::Ask(form) => self.ask_key(form, key),
            Popup::RemoteAnswer {
                remote,
                answer,
                mut scroll,
            } => {
                match code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return None,
                    KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll = scroll.saturating_add(10),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                    KeyCode::Char('c') => {
                        if let Some(Ok(text)) = &answer {
                            self.copy(text, "answer copied");
                        }
                    }
                    _ => {}
                }
                Some(Popup::RemoteAnswer {
                    remote,
                    answer,
                    scroll,
                })
            }
            Popup::Accounts { selected } => self.accounts_key(selected, code),
            Popup::AddAccount(form) => self.add_account_key(form, code),
            Popup::ConfirmRemoveAccount {
                agent,
                name,
                tokens,
            } => match code {
                KeyCode::Char('y' | 'Y') => {
                    self.remove_account(agent, &name);
                    Some(Popup::Accounts { selected: 0 })
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(Popup::Accounts { selected: 0 }),
                _ => Some(Popup::ConfirmRemoveAccount {
                    agent,
                    name,
                    tokens,
                }),
            },
            Popup::Projects { selected } => self.projects_key(selected, code),
            Popup::Project(form) => self.project_key(form, code),
            Popup::ConfirmRemoveProject { name, tokens } => match code {
                KeyCode::Char('y' | 'Y') => {
                    self.remove_project(&name);
                    Some(Popup::Projects { selected: 0 })
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(Popup::Projects { selected: 0 }),
                _ => Some(Popup::ConfirmRemoveProject { name, tokens }),
            },
            Popup::Remotes { selected } => self.remotes_key(selected, code),
            Popup::Connect(form) => self.connect_key(form, code),
            Popup::ConfirmRemoveRemote { name } => match code {
                KeyCode::Char('y' | 'Y') => {
                    match self.home().map(|h| crate::remote::forget(&h, &name)) {
                        Some(Ok(())) => {
                            self.audit("remote.remove", &name, "");
                            self.notify(format!(
                                "forgot {name}; its token works until its owner revokes it"
                            ));
                        }
                        Some(Err(e)) => self.notify(format!("{e:#}")),
                        None => self.notify("no Hashi home here"),
                    }
                    Some(Popup::Remotes { selected: 0 })
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(Popup::Remotes { selected: 0 }),
                _ => Some(Popup::ConfirmRemoveRemote { name }),
            },
            Popup::Audit { mut scroll } => {
                match code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return None,
                    KeyCode::Down | KeyCode::Char('j') => scroll += 1,
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char('J') => scroll += 10,
                    KeyCode::PageUp | KeyCode::Char('K') => scroll = scroll.saturating_sub(10),
                    _ => {}
                }
                Some(Popup::Audit { scroll })
            }
            Popup::Doctor => match code {
                KeyCode::Char('u') => {
                    self.check_update();
                    Some(Popup::Doctor)
                }
                KeyCode::Char('r') => {
                    self.refresh();
                    self.probe_tailscale();
                    Some(Popup::Doctor)
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => None,
                _ => Some(Popup::Doctor),
            },
            Popup::Report(form) => self.report_key(form, code),
            Popup::ReportSaved {
                path,
                include_prompts,
            } => match code {
                KeyCode::Char('c') => {
                    self.copy(&path, "path copied");
                    Some(Popup::ReportSaved {
                        path,
                        include_prompts,
                    })
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => None,
                _ => Some(Popup::ReportSaved {
                    path,
                    include_prompts,
                }),
            },
            Popup::Listen(form) => self.listen_key(form, code),
            Popup::ConfirmListen {
                form,
                addr,
                warning,
            } => match code {
                KeyCode::Char('y' | 'Y') => self.apply_listen(form, addr),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(Popup::Listen(form)),
                _ => Some(Popup::ConfirmListen {
                    form,
                    addr,
                    warning,
                }),
            },
            Popup::ConfirmUpdate => match code {
                KeyCode::Char('y' | 'Y') => {
                    match self.home() {
                        Some(home) => self.pending_command = Some(PendingCommand::Update { home }),
                        None => self.notify("run hashi update in a terminal"),
                    }
                    None
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                _ => Some(Popup::ConfirmUpdate),
            },
            other => Some(other),
        }
    }

    // ── token edit ─────────────────────────────────────────────────────────

    fn edit_token_key(&mut self, mut form: Box<EditTokenForm>, code: KeyCode) -> Option<Popup> {
        match code {
            KeyCode::Esc => return None,
            KeyCode::Enter => return self.save_token_edit(form),
            _ if nav(&mut form.field, EditTokenForm::FIELDS, code, |_| false) => {}
            KeyCode::Left if form.field == EditTokenForm::PROJECT => form.project.shift(-1),
            KeyCode::Right | KeyCode::Char(' ') if form.field == EditTokenForm::PROJECT => {
                form.project.shift(1);
            }
            _ => {
                let max = match form.field {
                    0 => 100,
                    1 | EditTokenForm::ACCOUNTS => 200,
                    _ => 12,
                };
                if let Some(text) = form.text_mut()
                    && edit(text, code, max)
                {
                    form.error = None;
                }
            }
        }
        Some(Popup::EditToken(form))
    }

    fn save_token_edit(&mut self, mut form: Box<EditTokenForm>) -> Option<Popup> {
        let body = match form.body() {
            Ok(b) => b,
            Err((field, e)) => {
                form.field = field;
                form.error = Some((field, e));
                return Some(Popup::EditToken(form));
            }
        };
        if nothing(&body) {
            self.notify("nothing changed");
            return None;
        }
        let current = match self.state.store.get_token(&form.token.id) {
            Ok(Some(t)) => t,
            _ => {
                self.notify("that token is gone");
                return None;
            }
        };
        match crate::api::apply_token_update(&self.state.store, current, body, None) {
            Ok((token, changes)) => {
                if let Err(e) = self.state.store.save_token_policy(&token) {
                    self.notify(format!("could not save: {e}"));
                    return None;
                }
                let detail = changes.join(", ");
                self.audit("token.update", &token.name, &detail);
                self.state
                    .bus
                    .log(format!("token {}: {detail}", token.name));
                self.notify(format!("updated {}: {detail}", token.name));
                self.refresh();
                None
            }
            Err(e) => {
                let msg = e.to_string();
                let field = field_for(
                    &msg,
                    &[
                        ("project", EditTokenForm::PROJECT),
                        ("account", EditTokenForm::ACCOUNTS),
                        ("note", 1),
                        ("expir", EditTokenForm::EXPIRY),
                        ("pending", 7),
                        ("budget", 5),
                        ("limit", 3),
                    ],
                    0,
                );
                form.field = field;
                form.error = Some((field, msg));
                Some(Popup::EditToken(form))
            }
        }
    }

    // ── ask ────────────────────────────────────────────────────────────────

    fn ask_key(&mut self, mut form: Box<AskForm>, key: KeyEvent) -> Option<Popup> {
        let code = key.code;
        let send = key.modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('s');
        let local = form.has_accounts();
        match code {
            _ if send => return self.send_ask(form),
            KeyCode::Esc => return None,
            KeyCode::Enter if form.field == AskForm::PROMPT => {
                if form.prompt.chars().count() < AskForm::PROMPT_CHARS {
                    form.prompt.push('\n');
                }
            }
            KeyCode::Enter => return self.send_ask(form),
            _ if nav(&mut form.field, AskForm::FIELDS, code, |f| {
                f == AskForm::ACCOUNT && !local
            }) => {}
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if form.field < AskForm::MODEL => {
                let by = if code == KeyCode::Left { -1 } else { 1 };
                match form.field {
                    0 => {
                        form.target = cycle(form.target, form.targets.len(), by);
                        form.set_agents(self.installed_agents());
                        form.set_accounts(self.ask_accounts(form.agent()));
                    }
                    1 => {
                        form.agent = cycle(form.agent, form.agents.len(), by);
                        form.set_accounts(self.ask_accounts(form.agent()));
                    }
                    _ => form.account = cycle(form.account, form.accounts.len(), by),
                }
                form.error = None;
            }
            _ => {
                let used = match form.field {
                    AskForm::MODEL => edit(&mut form.model, code, 100),
                    AskForm::PROMPT => edit(&mut form.prompt, code, AskForm::PROMPT_CHARS),
                    _ => false,
                };
                if used {
                    form.error = None;
                }
            }
        }
        Some(Popup::Ask(form))
    }

    fn send_ask(&mut self, mut form: Box<AskForm>) -> Option<Popup> {
        if let Err((field, e)) = form.check() {
            form.field = field;
            form.error = Some((field, e));
            return Some(Popup::Ask(form));
        }
        let model = Some(form.model.trim().to_owned()).filter(|m| !m.is_empty());
        match form.target().clone() {
            Target::Local => {
                let Some(owner) = self.owner_token() else {
                    form.error = Some((0, "no active admin token to send as".to_owned()));
                    return Some(Popup::Ask(form));
                };
                let agent = form.agent();
                let input = crate::api::SubmitInput {
                    message: form.prompt.clone(),
                    agent: agent.map(|a| a.as_str().to_owned()),
                    model,
                    account: form.account().map(str::to_owned),
                };
                // `submit` only checks and queues; nothing in it waits.
                match futures::executor::block_on(crate::api::submit(&self.state, &owner, input)) {
                    Ok(req) => {
                        self.state.bus.log(format!(
                            "dashboard asked {} as {}",
                            req.agent.display_name(),
                            owner.name
                        ));
                        self.refresh();
                        self.set_tab(Tab::Requests);
                        let at = self.recent.iter().position(|r| r.id == req.id);
                        self.requests_table.select(at.or(Some(0)));
                        self.detail_scroll = 0;
                        self.load_detail();
                        self.notify(format!(
                            "sent to {}; the answer appears in its detail",
                            req.agent.display_name()
                        ));
                        None
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        let field = field_for(
                            &msg,
                            &[
                                ("account", AskForm::ACCOUNT),
                                ("model", AskForm::MODEL),
                                ("agent", 1),
                            ],
                            AskForm::PROMPT,
                        );
                        form.field = field;
                        form.error = Some((field, msg));
                        Some(Popup::Ask(form))
                    }
                }
            }
            Target::Remote(name) => {
                let Some(home) = self.home() else {
                    form.error = Some((0, "no Hashi home here".to_owned()));
                    return Some(Popup::Ask(form));
                };
                let opts = crate::remote::AskOptions {
                    name: Some(name.clone()),
                    message: form.prompt.clone(),
                    agent: form.agent().map(|a| a.as_str().to_owned()),
                    model,
                };
                let slot = std::sync::Arc::clone(&self.done);
                let remote = name.clone();
                let spawned = std::thread::Builder::new()
                    .name("hashi-remote-ask".to_owned())
                    .spawn(move || {
                        let result = crate::remote::ask(&home, &opts)
                            .map(|o| match o.usage {
                                Some(u) => format!("{}\n\n{u}", o.response),
                                None => o.response,
                            })
                            .map_err(|e| format!("{e:#}"));
                        Self::finish(&slot, Done::Answer { remote, result });
                    });
                if spawned.is_err() {
                    form.error = Some((0, "could not start the request".to_owned()));
                    return Some(Popup::Ask(form));
                }
                Some(Popup::RemoteAnswer {
                    remote: name,
                    answer: None,
                    scroll: 0,
                })
            }
        }
    }

    // ── accounts ───────────────────────────────────────────────────────────

    fn accounts_key(&mut self, mut selected: usize, code: KeyCode) -> Option<Popup> {
        let rows = self.account_rows();
        if closes(code) {
            return None;
        }
        if list_nav(&mut selected, rows.len(), code) {
            return Some(Popup::Accounts { selected });
        }
        let row = rows.get(selected).cloned();
        match (code, row) {
            (KeyCode::Char('a'), _) => return Some(Popup::AddAccount(Box::default())),
            (KeyCode::Char('l'), Some(a)) => {
                if a.kind.is_key() {
                    self.notify("a key account has no sign-in; remove it and add the new key");
                } else {
                    return self.sign_in(a.agent, &a.name);
                }
            }
            (KeyCode::Char(c @ ('d' | 'e')), Some(a)) => {
                let disable = c == 'd';
                match acct::ops::set_disabled(&self.state.store, a.agent, &a.name, disable, ACTOR) {
                    Ok(()) => {
                        let what = if disable { "disabled" } else { "enabled" };
                        self.state
                            .bus
                            .log(format!("account {what}: {}:{}", a.agent, a.name));
                        self.notify(format!("{what} {}:{}", a.agent, a.name));
                    }
                    Err(e) => self.notify(format!("{e:#}")),
                }
            }
            (KeyCode::Char('x'), Some(a)) => {
                match acct::ops::check_removable(&self.state.store, a.agent, &a.name) {
                    Ok(tokens) => {
                        return Some(Popup::ConfirmRemoveAccount {
                            agent: a.agent,
                            name: a.name,
                            tokens,
                        });
                    }
                    Err(e) => self.notify(format!("{e:#}")),
                }
            }
            _ => {}
        }
        Some(Popup::Accounts { selected })
    }

    /// Hands the account's sign-in command to the event loop, which runs it
    /// with the terminal attached.
    fn sign_in(&mut self, agent: AgentKind, name: &str) -> Option<Popup> {
        let Some(home) = self.home() else {
            self.notify("no Hashi home here");
            return Some(Popup::Accounts { selected: 0 });
        };
        if acct::sign_in(agent).is_none() {
            self.notify(format!(
                "{} has no sign-in command Hashi knows",
                agent.display_name()
            ));
            return Some(Popup::Accounts { selected: 0 });
        }
        if let Err(e) = acct::ops::prepare_login(&self.state.store, &home, agent, name) {
            self.notify(format!("{e:#}"));
            return Some(Popup::Accounts { selected: 0 });
        }
        self.pending_command = Some(PendingCommand::SignIn {
            home,
            agent,
            name: name.to_owned(),
            bin: self.agent_bin(agent),
        });
        None
    }

    fn add_account_key(&mut self, mut form: Box<AccountForm>, code: KeyCode) -> Option<Popup> {
        let fields = form.fields();
        match code {
            KeyCode::Esc => return Some(Popup::Accounts { selected: 0 }),
            KeyCode::Enter => return self.add_account(form),
            _ if nav(&mut form.field, fields, code, |_| false) => {}
            KeyCode::Left if matches!(form.field, 0 | 2) => form.step(-1),
            KeyCode::Right | KeyCode::Char(' ') if matches!(form.field, 0 | 2) => form.step(1),
            _ => {
                let used = match form.field {
                    1 => edit(&mut form.name, code, 32),
                    AccountForm::KEY => edit_secret(&mut form.key, code, AccountForm::KEY_CHARS),
                    _ => false,
                };
                if used {
                    form.error = None;
                }
            }
        }
        form.field = form.field.min(form.fields() - 1);
        Some(Popup::AddAccount(form))
    }

    fn add_account(&mut self, mut form: Box<AccountForm>) -> Option<Popup> {
        let fail = |mut form: Box<AccountForm>, (field, e): FieldError| {
            form.field = field;
            form.error = Some((field, e));
            Some(Popup::AddAccount(form))
        };
        let Some(home) = self.home() else {
            return fail(form, (1, "no Hashi home here".to_owned()));
        };
        let name = form.name.trim().to_owned();
        let kind = match acct::ops::check_new(
            &self.state.store,
            form.agent,
            &name,
            Some(form.kind),
            form.kind.is_key(),
        ) {
            Ok(k) => k,
            Err(e) => {
                let msg = format!("{e:#}");
                let field = if msg.contains("support") { 2 } else { 1 };
                return fail(form, (field, msg));
            }
        };
        let key = match form.clean_key() {
            Ok(k) => k,
            Err(e) => return fail(form, e),
        };
        if let Err(e) = acct::ops::create(
            &self.state.store,
            &home,
            form.agent,
            &name,
            kind,
            key.as_deref(),
            ACTOR,
        ) {
            return fail(form, (1, format!("{e:#}")));
        }
        // The key is on disk now; the form's copy goes with the form.
        form.key = forms::Secret::default();
        let agent = form.agent;
        self.state
            .bus
            .log(format!("account added: {agent}:{name} ({kind})"));
        if kind == AccountKind::Login {
            return self.sign_in(agent, &name);
        }
        self.notify(format!("added {agent}:{name} ({kind})"));
        let selected = self
            .account_rows()
            .iter()
            .position(|a| a.agent == agent && a.name == name)
            .unwrap_or(0);
        Some(Popup::Accounts { selected })
    }

    fn remove_account(&mut self, agent: AgentKind, name: &str) {
        let Some(home) = self.home() else {
            self.notify("no Hashi home here");
            return;
        };
        match acct::ops::remove(&self.state.store, &home, agent, name, ACTOR) {
            Ok((removed, dir)) => {
                self.state
                    .bus
                    .log(format!("account removed: {agent}:{name}"));
                let mut msg = format!(
                    "removed {agent}:{name}; taken off {} token(s) and {} invite(s)",
                    removed.tokens, removed.invites
                );
                if let Err(e) = dir {
                    msg.push_str(&format!("; its directory stays: {e}"));
                }
                self.notify(msg);
            }
            Err(e) => self.notify(format!("{e:#}")),
        }
        self.refresh();
    }

    // ── projects ───────────────────────────────────────────────────────────

    fn projects_key(&mut self, mut selected: usize, code: KeyCode) -> Option<Popup> {
        let rows = self.state.store.list_projects().unwrap_or_default();
        if closes(code) {
            return None;
        }
        if list_nav(&mut selected, rows.len(), code) {
            return Some(Popup::Projects { selected });
        }
        match (code, rows.get(selected)) {
            (KeyCode::Char('a'), _) => return Some(Popup::Project(Box::new(ProjectForm::add()))),
            (KeyCode::Char('e'), Some(p)) => {
                return Some(Popup::Project(Box::new(ProjectForm::edit(p))));
            }
            (KeyCode::Char('x'), Some(p)) => {
                let tokens = self
                    .state
                    .store
                    .project_stats(&p.name, now_ms())
                    .map_or(0, |s| s.tokens);
                return Some(Popup::ConfirmRemoveProject {
                    name: p.name.clone(),
                    tokens: usize::try_from(tokens).unwrap_or(usize::MAX),
                });
            }
            _ => {}
        }
        Some(Popup::Projects { selected })
    }

    fn project_key(&mut self, mut form: Box<ProjectForm>, code: KeyCode) -> Option<Popup> {
        let editing = form.editing.is_some();
        match code {
            KeyCode::Esc => return Some(Popup::Projects { selected: 0 }),
            KeyCode::Enter => return self.save_project(form),
            _ if nav(&mut form.field, ProjectForm::FIELDS, code, |f| {
                f == 0 && editing
            }) => {}
            KeyCode::Left if form.field == ProjectForm::DEFAULT_AGENT => form.step(-1),
            KeyCode::Right | KeyCode::Char(' ') if form.field == ProjectForm::DEFAULT_AGENT => {
                form.step(1);
            }
            _ => {
                let max = match form.field {
                    0 => 32,
                    1 => 1024,
                    _ => 200,
                };
                if let Some(text) = form.text_mut()
                    && edit(text, code, max)
                {
                    form.error = None;
                }
            }
        }
        Some(Popup::Project(form))
    }

    /// The agents a new project allows when none are typed: the installed
    /// ones, Crush only when listed, or every agent when none is installed.
    fn project_agents(&self) -> Vec<AgentKind> {
        let found: Vec<AgentKind> = self
            .installed_agents()
            .into_iter()
            .filter(|a| *a != AgentKind::Crush)
            .collect();
        if found.is_empty() {
            AgentKind::ALL
                .into_iter()
                .filter(|a| *a != AgentKind::Crush)
                .collect()
        } else {
            found
        }
    }

    fn save_project(&mut self, form: Box<ProjectForm>) -> Option<Popup> {
        let words: &[(&str, usize)] = &[
            ("name", 0),
            ("model", 5),
            ("account", 3),
            ("default agent", 4),
            ("agent", 2),
            ("note", 6),
        ];
        let fail = |mut form: Box<ProjectForm>, (field, e): FieldError| {
            form.field = field;
            form.error = Some((field, e));
            Some(Popup::Project(form))
        };
        let parsed = (|| {
            Ok::<_, FieldError>((
                form.parse_agents()?,
                form.parse_accounts()?,
                form.parse_models()?,
            ))
        })();
        let (agents, accounts, models) = match parsed {
            Ok(v) => v,
            Err(e) => return fail(form, e),
        };
        let home = self.home();
        let store = std::sync::Arc::clone(&self.state.store);
        let note = Some(form.note.trim().to_owned()).filter(|n| !n.is_empty());
        let Some(name) = form.editing.clone() else {
            if store
                .list_projects()
                .is_ok_and(|l| l.len() >= crate::projects::MAX_PROJECTS)
            {
                return fail(
                    form,
                    (0, "project limit reached, remove one first".to_owned()),
                );
            }
            let raw = crate::projects::Project {
                name: form.name.trim().to_owned(),
                path: String::new(),
                agents: agents.unwrap_or_else(|| self.project_agents()),
                accounts,
                default_agent: form.default_agent,
                models,
                note,
                // Strategy and threshold are set with `hashi project update`
                // until the form grows fields for them.
                account_strategy: None,
                rotate_at: None,
                created_at: now_ms(),
            };
            let path = PathBuf::from(form.path.trim());
            let p = match crate::api::validate_project(&store, home.as_deref(), raw, &path) {
                Ok(p) => p,
                Err(e) => {
                    let msg = e.to_string();
                    return fail(form, (field_for(&msg, words, 1), msg));
                }
            };
            return match store.add_project(&p) {
                Ok(true) => {
                    self.audit("project.add", &p.name, &p.path);
                    self.state.bus.log(format!("project added: {}", p.name));
                    self.notify(format!("added project {} in {}", p.name, p.path));
                    let selected = store
                        .list_projects()
                        .unwrap_or_default()
                        .iter()
                        .position(|q| q.name == p.name)
                        .unwrap_or(0);
                    Some(Popup::Projects { selected })
                }
                Ok(false) => fail(
                    form,
                    (0, format!("a project named {} already exists", p.name)),
                ),
                Err(e) => fail(form, (0, e.to_string())),
            };
        };
        let Ok(Some(current)) = store.get_project(&name) else {
            self.notify(format!("project {name} is gone"));
            return Some(Popup::Projects { selected: 0 });
        };
        let body = crate::api::UpdateProjectBody {
            path: (form.path.trim() != current.path).then(|| form.path.trim().to_owned()),
            agents: agents.filter(|a| *a != current.agents),
            accounts: (accounts != current.accounts).then_some(accounts),
            default_agent: (form.default_agent != current.default_agent)
                .then_some(form.default_agent),
            models: (models != current.models).then_some(models),
            note: (note != current.note).then(|| note.unwrap_or_default()),
            account_strategy: None,
            rotate_at: None,
        };
        match crate::api::apply_update(&store, home.as_deref(), current, body) {
            Ok((p, changes)) => {
                if let Err(e) = store.save_project(&p) {
                    return fail(form, (1, e.to_string()));
                }
                let detail = changes.join(", ");
                self.audit("project.update", &p.name, &detail);
                self.state.bus.log(format!("project {}: {detail}", p.name));
                self.notify(format!("updated {}: {detail}", p.name));
                self.refresh();
                Some(Popup::Projects { selected: 0 })
            }
            Err(e) if e.to_string().contains("nothing to update") => {
                self.notify("nothing changed");
                Some(Popup::Projects { selected: 0 })
            }
            Err(e) => {
                let msg = e.to_string();
                fail(form, (field_for(&msg, words, 1), msg))
            }
        }
    }

    fn remove_project(&mut self, name: &str) {
        use crate::store::RemovedProject;
        match self.state.store.remove_project(name, true) {
            Ok(RemovedProject::Removed { tokens, invites }) => {
                self.audit(
                    "project.remove",
                    name,
                    &format!("unbound {tokens} token(s) and {invites} invite(s)"),
                );
                self.state.bus.log(format!("project removed: {name}"));
                self.notify(format!(
                    "removed {name}; unbound {tokens} token(s) and {invites} invite(s). \
                     Its folder was not touched."
                ));
            }
            Ok(RemovedProject::NotFound) => self.notify(format!("no project named {name}")),
            Ok(RemovedProject::Bound { .. }) => self.notify("still bound; try again"),
            Err(e) => self.notify(format!("could not remove {name}: {e}")),
        }
        self.refresh();
    }

    // ── bridges ────────────────────────────────────────────────────────────

    fn remotes_key(&mut self, mut selected: usize, code: KeyCode) -> Option<Popup> {
        let names = self.remote_names();
        if closes(code) {
            return None;
        }
        if list_nav(&mut selected, names.len(), code) {
            return Some(Popup::Remotes { selected });
        }
        match (code, names.get(selected)) {
            (KeyCode::Char('c'), _) => return Some(Popup::Connect(Box::default())),
            (KeyCode::Char('x'), Some(n)) => {
                return Some(Popup::ConfirmRemoveRemote { name: n.clone() });
            }
            (KeyCode::Char('a'), Some(n)) => {
                let n = n.clone();
                self.open_ask(Some(&n));
                return self.popup.take();
            }
            _ => {}
        }
        Some(Popup::Remotes { selected })
    }

    fn connect_key(&mut self, mut form: Box<ConnectForm>, code: KeyCode) -> Option<Popup> {
        if form.busy {
            return if code == KeyCode::Esc {
                None
            } else {
                Some(Popup::Connect(form))
            };
        }
        match code {
            KeyCode::Esc => return None,
            KeyCode::Enter => return self.connect(form),
            _ if nav(&mut form.field, ConnectForm::FIELDS, code, |_| false) => {}
            _ => {
                let used = if form.field == 0 {
                    edit(&mut form.link, code, 600)
                } else {
                    edit(&mut form.name, code, 32)
                };
                if used {
                    form.error = None;
                }
            }
        }
        Some(Popup::Connect(form))
    }

    fn connect(&mut self, mut form: Box<ConnectForm>) -> Option<Popup> {
        if let Err(e) = crate::remote::parse_invite_url(form.link.trim()) {
            form.field = 0;
            form.error = Some((0, format!("{e:#}")));
            return Some(Popup::Connect(form));
        }
        let name = Some(form.name.trim().to_owned()).filter(|n| !n.is_empty());
        if let Some(n) = &name
            && !crate::remote::valid_name(n)
        {
            form.field = 1;
            form.error = Some((
                1,
                "lowercase letters, digits, - or _, starting with a letter or digit".to_owned(),
            ));
            return Some(Popup::Connect(form));
        }
        let Some(home) = self.home() else {
            form.error = Some((0, "no Hashi home here".to_owned()));
            return Some(Popup::Connect(form));
        };
        let link = form.link.trim().to_owned();
        let slot = std::sync::Arc::clone(&self.done);
        let spawned = std::thread::Builder::new()
            .name("hashi-connect".to_owned())
            .spawn(move || {
                let result = crate::remote::redeem(&home, &link, name.as_deref())
                    .map(|(remote, _)| remote.name)
                    .map_err(|e| format!("{e:#}"));
                Self::finish(&slot, Done::Connected(result));
            });
        if spawned.is_err() {
            form.error = Some((0, "could not start connecting".to_owned()));
        } else {
            form.busy = true;
        }
        Some(Popup::Connect(form))
    }

    // ── report ─────────────────────────────────────────────────────────────

    fn report_key(&mut self, mut form: Box<ReportForm>, code: KeyCode) -> Option<Popup> {
        match code {
            KeyCode::Esc => return None,
            KeyCode::Enter => return self.write_report(form),
            _ if nav(&mut form.field, ReportForm::FIELDS, code, |_| false) => {}
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                if form.field == 0 {
                    form.include_prompts = !form.include_prompts;
                } else {
                    form.to_home = !form.to_home;
                }
                form.error = None;
            }
            _ => {}
        }
        Some(Popup::Report(form))
    }

    fn write_report(&mut self, mut form: Box<ReportForm>) -> Option<Popup> {
        let home = self.home();
        let dir = if form.to_home {
            let Some(home) = &home else {
                form.error = Some("no Hashi home here; save it in the current folder".to_owned());
                return Some(Popup::Report(form));
            };
            let dir = home.join(crate::diagnostics::REPORTS_DIR);
            if let Err(e) = crate::server::prepare_home(&dir) {
                form.error = Some(format!("{e:#}"));
                return Some(Popup::Report(form));
            }
            Some(dir)
        } else {
            None
        };
        let text = crate::diagnostics::report_text(
            home.as_deref().unwrap_or_else(|| std::path::Path::new(".")),
            Ok(&self.state.store),
            &self.state.runtime.agents,
            form.include_prompts,
        );
        match crate::diagnostics::save_report(dir.as_deref(), &text) {
            Ok(path) => {
                let path = path.display().to_string();
                self.state.bus.log(format!("report saved: {path}"));
                Some(Popup::ReportSaved {
                    path,
                    include_prompts: form.include_prompts,
                })
            }
            Err(e) => {
                form.error = Some(format!("could not write the report: {e}"));
                Some(Popup::Report(form))
            }
        }
    }

    // ── listen ─────────────────────────────────────────────────────────────

    fn listen_key(&mut self, mut form: Box<ListenForm>, code: KeyCode) -> Option<Popup> {
        let typed = form.bind == forms::BINDS.len();
        match code {
            KeyCode::Esc => return None,
            KeyCode::Enter => return self.submit_listen(form),
            _ if nav(&mut form.field, ListenForm::FIELDS, code, |f| {
                f == ListenForm::OTHER && !typed
            }) => {}
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if matches!(form.field, ListenForm::BIND | ListenForm::KEEP) =>
            {
                form.step(if code == KeyCode::Left { -1 } else { 1 });
            }
            KeyCode::Char(c) if form.field == ListenForm::PORT && !c.is_ascii_digit() => {}
            _ => {
                let used = match form.field {
                    ListenForm::PORT => edit(&mut form.port, code, 5),
                    ListenForm::OTHER => edit(&mut form.other, code, 45),
                    _ => false,
                };
                if used {
                    form.error = None;
                }
            }
        }
        Some(Popup::Listen(form))
    }

    fn submit_listen(&mut self, mut form: Box<ListenForm>) -> Option<Popup> {
        let addr = match form.addr() {
            Ok(a) => a,
            Err((field, e)) => {
                form.field = field;
                form.error = Some((field, e));
                return Some(Popup::Listen(form));
            }
        };
        let tls = self.state.runtime.listen.tls();
        match crate::server::plain_text_warning(addr.ip(), tls) {
            Some(warning) if !tls => Some(Popup::ConfirmListen {
                form,
                addr,
                warning,
            }),
            _ => self.apply_listen(form, addr),
        }
    }

    /// Moves the server to `addr` and, when the form says so, saves the
    /// choice for the next start.
    fn apply_listen(
        &mut self,
        mut form: Box<ListenForm>,
        addr: std::net::SocketAddr,
    ) -> Option<Popup> {
        use crate::server::RebindError;
        let rebound = match crate::server::rebind(&self.state.runtime, &self.state.bus, addr) {
            Ok(r) => r,
            Err(e) => {
                let field = match &e {
                    RebindError::Bind(msg) if !msg.contains("port") => {
                        if form.bind == forms::BINDS.len() {
                            ListenForm::OTHER
                        } else {
                            ListenForm::BIND
                        }
                    }
                    _ => ListenForm::PORT,
                };
                form.field = field;
                form.error = Some((field, e.to_string()));
                return Some(Popup::Listen(form));
            }
        };
        let mut msg = format!("listening on {}", rebound.url);
        if form.keep {
            match self.home() {
                Some(home) => {
                    let mut saved = crate::settings::load(&home).unwrap_or_default();
                    saved.port = Some(addr.port());
                    saved.bind = Some(addr.ip());
                    match crate::settings::save(&home, &saved) {
                        Ok(()) => msg.push_str(", kept for the next start"),
                        Err(e) => msg.push_str(&format!(", but not saved: {e}")),
                    }
                }
                None => msg.push_str(", not saved: no Hashi home"),
            }
        }
        self.audit(
            "server.listen",
            &rebound.url,
            if form.keep { "kept" } else { "" },
        );
        for note in &rebound.notes {
            self.state.bus.log(note.clone());
        }
        if let Some(note) = rebound.notes.first() {
            msg.push_str(&format!("; {note}"));
        }
        self.notify(msg);
        None
    }
}
