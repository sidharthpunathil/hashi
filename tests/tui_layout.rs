//! The dashboard's layout on a realistic store, from 80x24 up: footers,
//! tables and popups that fit, and empty states that say what to do next.
//!
//! `shots` renders every view and popup at three terminal sizes and writes
//! each frame as plain text, for reading the layout as a user would. It is
//! ignored by default; run it with a folder to write into:
//!
//! ```text
//! HASHI_SHOTS=/tmp/tui-shots cargo test --test tui_layout -- --ignored
//! ```
//!
//! The store is a temporary Hashi home with 15 tokens, 200 requests in
//! mixed states, 6 accounts with usage windows, 3 projects, 2 pending
//! requests for access and a saved bridge. A second, empty home shows the
//! first run.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hashi::accounts::AccountKind;
use hashi::agent::{AgentKind, Usage};
use hashi::api::{AgentInfo, AppState, RateLimiter, Runtime};
use hashi::events::{Bus, Event};
use hashi::guard::{GIB, ManualProbe, ResourceGuard, ResourceSettings};
use hashi::projects::Project;
use hashi::queue::Dispatcher;
use hashi::store::{NewRequest, NewToken, Store};
use hashi::token::{Role, SessionMode};
use hashi::tui::app::{Action, App, Popup, Tab};
use hashi::tui::ui;
use hashi::usage::{Source, Window};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const SIZES: [(u16, u16); 3] = [(80, 24), (120, 40), (200, 60)];

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn runtime(home: &Path, agents: Vec<AgentInfo>) -> Arc<Runtime> {
    let missing = home.join("no-such-helper");
    let cancel = tokio_util::sync::CancellationToken::new();
    Arc::new(Runtime {
        started: Instant::now(),
        listen: hashi::net::Listen::new("http://127.0.0.1:7878"),
        public_url: RwLock::new(None),
        agents,
        name: "Hashi".into(),
        tunnel: hashi::tunnel::Control::new(
            hashi::tunnel::Settings {
                bin: Some(missing.clone()),
                port: 7878,
                local_tls: false,
                home: home.to_path_buf(),
                configured_url: None,
                named_token: None,
                configured: false,
                cancel: cancel.clone(),
            },
            hashi::net::TunnelTrust::default(),
        ),
        tailscale: hashi::tailscale::Control::new(
            hashi::tailscale::Settings {
                bin: Some(missing),
                local_port: 7878,
                https_port: 443,
                unreachable: None,
                home: home.to_path_buf(),
                cancel,
            },
            hashi::net::TunnelTrust::default(),
        ),
    })
}

fn installed() -> Vec<AgentInfo> {
    vec![
        AgentInfo {
            agent: AgentKind::Claude,
            available: true,
            version: Some("2.1.318 (Claude Code)".into()),
            path: "claude".into(),
        },
        AgentInfo {
            agent: AgentKind::Codex,
            available: true,
            version: Some("0.50.0".into()),
            path: "codex".into(),
        },
        AgentInfo {
            agent: AgentKind::Gemini,
            available: false,
            version: None,
            path: "gemini".into(),
        },
    ]
}

fn app_state(store: Store, home: &Path, agents: Vec<AgentInfo>) -> AppState {
    AppState {
        store: Arc::new(store),
        bus: Bus::new(1024),
        dispatcher: Dispatcher::new(),
        runtime: runtime(home, agents),
        limiter: Arc::new(RateLimiter::new(0)),
        default_agent: AgentKind::Claude,
        queue_limit: 100,
        run_timeout_secs: 600,
        guard: Arc::new(ResourceGuard::new(
            &ResourceSettings {
                max_running: Some(2),
                ..ResourceSettings::with_probe(Arc::new(ManualProbe::new(16 * GIB, 10 * GIB)))
            },
            home.to_path_buf(),
            Bus::default(),
        )),
        waits: Default::default(),
    }
}

const PROMPTS: [&str; 6] = [
    "Summarise the open pull requests and flag anything that touches auth",
    "Write a migration for the users table that adds a nullable locale column",
    "Explain this stack trace from the nightly build",
    "Refactor the request handler so it streams the response body",
    "Why does the integration test for the billing webhook time out on CI?",
    "Draft release notes for v0.3.1 from the merged pull requests",
];

fn filled(home: &Path) -> AppState {
    let store = hashi::server::open_store(home).unwrap();
    hashi::server::ensure_admin(&store).unwrap();
    let now = hashi::util::now_ms();
    let projects = ["docs-site", "billing-api", "mobile-app"];
    for name in projects {
        let path = home.join("work").join(name);
        std::fs::create_dir_all(&path).unwrap();
        store
            .add_project(&Project {
                name: name.into(),
                path: path.display().to_string(),
                agents: vec![AgentKind::Claude, AgentKind::Codex],
                accounts: Default::default(),
                default_agent: None,
                models: Default::default(),
                note: Some(format!("the {name} repository")),
                account_strategy: None,
                rotate_at: None,
                created_at: now,
            })
            .unwrap();
    }
    let names = [
        "ci-bot",
        "alice",
        "bob-laptop",
        "carol",
        "dave-from-the-platform-team",
        "erin",
        "frank",
        "grace-ipad",
        "heidi",
        "ivan",
        "judy",
        "mallory",
        "nightly-builds",
        "oscar",
    ];
    let mut tokens = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let (tk, _) = store
            .create_token(&NewToken {
                name: (*name).into(),
                role: if i == 0 { Role::Admin } else { Role::User },
                session_mode: if i % 3 == 0 {
                    SessionMode::Stateful
                } else {
                    SessionMode::Stateless
                },
                agents: if i % 2 == 0 {
                    AgentKind::ALL.to_vec()
                } else {
                    vec![AgentKind::Claude]
                },
                expires_in_days: if i % 4 == 1 { Some(30) } else { None },
                note: None,
                limits: if i % 3 == 1 {
                    hashi::token::Limits {
                        daily_requests: Some(50),
                        monthly_budget_usd: Some(20.0),
                        ..Default::default()
                    }
                } else {
                    Default::default()
                },
                accounts: Default::default(),
                project: if i % 5 == 2 {
                    Some(projects[i % 3].into())
                } else {
                    None
                },
            })
            .unwrap();
        tokens.push(tk);
    }
    let agents = [AgentKind::Claude, AgentKind::Codex];
    for i in 0..200 {
        let id = format!("{i:08}-0000-4000-8000-000000000000");
        let agent = agents[i % 2];
        store
            .insert_request(NewRequest {
                id: &id,
                token: &tokens[i % tokens.len()],
                agent,
                model: None,
                message: PROMPTS[i % PROMPTS.len()],
            })
            .unwrap();
        if i >= 196 {
            continue; // queued
        }
        store.claim_next(agent).unwrap();
        match i % 10 {
            _ if i >= 194 => {} // running
            7 => store
                .fail_request(&id, "the agent exited with status 1: rate limited")
                .unwrap(),
            9 => {
                store.cancel_running(&id, "cancelled by the owner").unwrap();
            }
            _ => store
                .complete_request(
                    &id,
                    "Here is the change.\n\n```rust\nfn main() {}\n```\nIt compiles.",
                    Some(Usage {
                        input_tokens: 1200 + i as u64 * 7,
                        output_tokens: 340,
                        cost_usd: Some(0.012),
                    }),
                )
                .unwrap(),
        }
    }
    let accounts = [
        (AgentKind::Claude, "default", 12.0, 40.0),
        (AgentKind::Claude, "work", 88.0, 61.0),
        (AgentKind::Claude, "personal-max-plan", 100.0, 97.0),
        (AgentKind::Codex, "default", 3.0, 18.0),
        (AgentKind::Codex, "team", 55.0, 72.0),
        (AgentKind::Codex, "spare", 0.0, 0.0),
    ];
    for (agent, name, five, week) in accounts {
        if name != "default" {
            store.add_account(agent, name, AccountKind::Login).unwrap();
        }
        let windows = [
            ("five_hour", "5 hours", 300, five),
            ("seven_day", "week", 10_080, week),
        ]
        .map(|(key, label, mins, used)| Window {
            key: key.to_owned(),
            label: label.to_owned(),
            used_percent: used,
            resets_at: Some(now + mins * 20_000),
            window_mins: Some(mins),
            source: Source::Poll,
            observed_at: Some(now - 90_000),
        });
        store.replace_usage(agent, name, &windows).unwrap();
        store
            .set_poll_status(agent, name, true, Some("max"), None)
            .unwrap();
    }
    hashi::remote::save(
        home,
        &[hashi::remote::Remote {
            name: "friend".into(),
            base_url: "https://friend.example.com".into(),
            token: "hashi_sk_secret_never_shown".into(),
        }],
    )
    .unwrap();
    {
        use hashi::access::{self, NewPerson, OpenLink};
        let (_, code) = access::open_link(&store, "cli", OpenLink::default()).unwrap();
        for (name, msg) in [
            ("Priya", "for the docs site"),
            ("Tomasz", "to review the billing PRs"),
        ] {
            access::submit_new(
                &store,
                NewPerson {
                    code: code.clone(),
                    name: name.into(),
                    message: Some(msg.into()),
                    agents: Some(vec![AgentKind::Claude]),
                },
            )
            .unwrap();
        }
    }
    store
        .create_invite(&hashi::store::NewInvite {
            label: "zoe".into(),
            session_mode: SessionMode::Stateless,
            agents: vec![AgentKind::Claude],
            token_expires_in_days: None,
            limits: Default::default(),
            link_hours: 24,
            accounts: Default::default(),
            project: None,
        })
        .unwrap();
    app_state(store, home, installed())
}

fn empty(home: &Path) -> AppState {
    let store = hashi::server::open_store(home).unwrap();
    hashi::server::ensure_admin(&store).unwrap();
    app_state(store, home, Vec::new())
}

fn feed(app: &mut App) {
    for ev in [
        Event::Log("Claude Code ready (2.1.318)".into()),
        Event::Log("listening on http://127.0.0.1:7878".into()),
        Event::Queued {
            id: "00000196-0000-4000-8000-000000000000".into(),
            agent: AgentKind::Claude,
            token: "ci-bot".into(),
        },
        Event::Started {
            id: "00000194-0000-4000-8000-000000000000".into(),
            agent: AgentKind::Claude,
        },
        Event::Finished {
            id: "00000193-0000-4000-8000-000000000000".into(),
            agent: AgentKind::Codex,
            ok: true,
            duration_ms: 5_132,
            error: None,
        },
        Event::Finished {
            id: "00000187-0000-4000-8000-000000000000".into(),
            agent: AgentKind::Codex,
            ok: false,
            duration_ms: 812,
            error: Some("the agent exited with status 1: rate limited".into()),
        },
    ] {
        app.on_event(ev);
    }
}

fn text(app: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::render(app, f)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn shoot(dir: &Path, name: &str, app: &mut App) {
    for (w, h) in SIZES {
        let out = text(app, w, h);
        std::fs::write(dir.join(format!("{name}.{w}x{h}.txt")), out).unwrap();
    }
}

/// Opens one popup on the dashboard.
type Opener = Box<dyn Fn(&mut App)>;

fn open(app: &mut App, action: Action) {
    app.popup = None;
    app.tab = Tab::Commands;
    app.run_action(action);
}

#[test]
#[ignore = "writes frames: HASHI_SHOTS=dir cargo test --test tui_layout -- --ignored"]
fn shots() {
    let dir = PathBuf::from(std::env::var_os("HASHI_SHOTS").expect("set HASHI_SHOTS"));
    std::fs::create_dir_all(&dir).unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(filled(home.path()), None, false);
    app.reduced_motion = true;
    feed(&mut app);
    app.refresh();

    for tab in Tab::ALL {
        app.popup = None;
        app.tab = tab;
        if tab == Tab::Requests {
            app.on_key(key(KeyCode::Down));
            app.on_key(key(KeyCode::Down));
        }
        shoot(
            &dir,
            &format!("view-{}", tab.title().to_lowercase()),
            &mut app,
        );
    }
    // Tokens with the invites list focused.
    app.tab = Tab::Tokens;
    app.on_key(key(KeyCode::Char('i')));
    shoot(&dir, "view-tokens-invites", &mut app);
    app.on_key(key(KeyCode::Char('i')));

    let popups: Vec<(&str, Opener)> = vec![
        ("help", Box::new(|a| open(a, Action::Help))),
        ("new-token", Box::new(|a| open(a, Action::NewToken))),
        ("share", Box::new(|a| open(a, Action::NewInvite))),
        ("ask", Box::new(|a| open(a, Action::Ask))),
        ("accounts", Box::new(|a| open(a, Action::Accounts))),
        (
            "add-account",
            Box::new(|a| {
                open(a, Action::Accounts);
                a.on_key(key(KeyCode::Char('a')));
            }),
        ),
        ("projects", Box::new(|a| open(a, Action::Projects))),
        (
            "project-form",
            Box::new(|a| {
                open(a, Action::Projects);
                a.on_key(key(KeyCode::Char('e')));
            }),
        ),
        ("bridges", Box::new(|a| open(a, Action::Bridges))),
        ("connect", Box::new(|a| open(a, Action::Connect))),
        ("listen", Box::new(|a| open(a, Action::Listen))),
        ("doctor", Box::new(|a| open(a, Action::Doctor))),
        ("report", Box::new(|a| open(a, Action::Report))),
        ("audit", Box::new(|a| open(a, Action::Audit))),
        ("access", Box::new(|a| open(a, Action::AccessRequests))),
        (
            "access-accept",
            Box::new(|a| {
                open(a, Action::AccessRequests);
                a.on_key(key(KeyCode::Char('a')));
            }),
        ),
        (
            "access-decline",
            Box::new(|a| {
                open(a, Action::AccessRequests);
                a.on_key(key(KeyCode::Char('d')));
            }),
        ),
        (
            "access-link",
            Box::new(|a| {
                open(a, Action::AccessRequests);
                a.on_key(key(KeyCode::Char('o')));
            }),
        ),
        ("install", Box::new(|a| open(a, Action::InstallCloudflared))),
        (
            "confirm-update",
            Box::new(|a| open(a, Action::InstallUpdate)),
        ),
        (
            "edit-token",
            Box::new(|a| {
                a.popup = None;
                a.tab = Tab::Tokens;
                a.on_key(key(KeyCode::Down));
                a.on_key(key(KeyCode::Char('e')));
            }),
        ),
        (
            "reveal",
            Box::new(|a| {
                a.popup = Some(Popup::Reveal {
                    title: "Token for alice".into(),
                    secret: "hashi_sk_0123456789abcdef0123456789abcdef".into(),
                });
            }),
        ),
        (
            "invite-reveal",
            Box::new(|a| {
                a.popup = Some(Popup::InviteReveal {
                    label: "zoe".into(),
                    code: "hashi_inv_0123456789abcdef0123456789".into(),
                    link_hours: 24,
                    via: hashi::links::Via::Auto,
                    pool: false,
                });
            }),
        ),
        (
            "confirm-quit",
            Box::new(|a| a.popup = Some(Popup::ConfirmQuit { pending: 6 })),
        ),
        (
            "confirm-revoke",
            Box::new(|a| {
                a.popup = Some(Popup::ConfirmRevoke {
                    id: "x".into(),
                    name: "carol".into(),
                });
            }),
        ),
        (
            "confirm-cancel-request",
            Box::new(|a| {
                a.popup = Some(Popup::ConfirmCancelRequest {
                    id: "x".into(),
                    token: "ci-bot".into(),
                });
            }),
        ),
        (
            "confirm-cancel-invite",
            Box::new(|a| {
                a.popup = Some(Popup::ConfirmCancelInvite {
                    id: "x".into(),
                    label: "zoe".into(),
                });
            }),
        ),
        (
            "confirm-remove-account",
            Box::new(|a| {
                a.popup = Some(Popup::ConfirmRemoveAccount {
                    agent: AgentKind::Claude,
                    name: "work".into(),
                    tokens: 2,
                });
            }),
        ),
        (
            "confirm-remove-project",
            Box::new(|a| {
                a.popup = Some(Popup::ConfirmRemoveProject {
                    name: "docs-site".into(),
                    tokens: 1,
                });
            }),
        ),
        (
            "confirm-remove-remote",
            Box::new(|a| {
                a.popup = Some(Popup::ConfirmRemoveRemote {
                    name: "friend".into(),
                });
            }),
        ),
        (
            "approval",
            Box::new(|a| {
                a.popup = Some(Popup::Approval {
                    url: "https://login.tailscale.com/a/1234567890abcdef".into(),
                });
            }),
        ),
        (
            "remote-answer-wait",
            Box::new(|a| {
                a.popup = Some(Popup::RemoteAnswer {
                    remote: "friend".into(),
                    answer: None,
                    scroll: 0,
                });
            }),
        ),
        (
            "remote-answer",
            Box::new(|a| {
                a.popup = Some(Popup::RemoteAnswer {
                    remote: "friend".into(),
                    answer: Some(Ok("The tests pass.\nTwo warnings remain.".into())),
                    scroll: 0,
                });
            }),
        ),
        (
            "remote-error",
            Box::new(|a| {
                a.popup = Some(Popup::RemoteAnswer {
                    remote: "friend".into(),
                    answer: Some(Err("the bridge did not answer within 30s".into())),
                    scroll: 0,
                });
            }),
        ),
        (
            "report-saved",
            Box::new(|a| {
                a.popup = Some(Popup::ReportSaved {
                    path: "/Users/someone/.hashi/reports/hashi-report-20261106.txt".into(),
                    include_prompts: false,
                });
            }),
        ),
    ];
    for (name, f) in &popups {
        f(&mut app);
        assert!(app.popup.is_some(), "{name} did not open");
        shoot(&dir, &format!("popup-{name}"), &mut app);
        app.popup = None;
    }

    // First run: an empty home, no agents found, a token revealed once.
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(
        empty(home.path()),
        Some("hashi_sk_first_run_0123456789abcdef".into()),
        false,
    );
    app.reduced_motion = true;
    shoot(&dir, "first-run-reveal", &mut app);
    app.popup = None;
    for tab in Tab::ALL {
        app.tab = tab;
        shoot(
            &dir,
            &format!("empty-{}", tab.title().to_lowercase()),
            &mut app,
        );
    }
    for (name, action) in [
        ("accounts", Action::Accounts),
        ("projects", Action::Projects),
        ("bridges", Action::Bridges),
        ("access", Action::AccessRequests),
        ("audit", Action::Audit),
    ] {
        open(&mut app, action);
        shoot(&dir, &format!("empty-popup-{name}"), &mut app);
        app.popup = None;
    }
}

// ── layout checks ───────────────────────────────────────────────────────────

fn filled_app(home: &Path) -> App {
    let mut app = App::new(filled(home), None, false);
    app.reduced_motion = true;
    feed(&mut app);
    app.refresh();
    app
}

/// The last row of a frame: the footer.
fn footer(frame: &str) -> &str {
    frame.lines().last().unwrap_or_default()
}

#[test]
fn every_footer_keeps_help_and_quit_at_80_columns() {
    let home = tempfile::tempdir().unwrap();
    let mut app = filled_app(home.path());
    for tab in Tab::ALL {
        app.tab = tab;
        let line = text(&mut app, 80, 24);
        let line = footer(&line);
        assert!(
            line.contains("?  help") && line.ends_with("q  quit"),
            "{tab:?} footer: {line:?}"
        );
    }
    // The tokens view with invites keeps revoke too.
    app.tab = Tab::Tokens;
    let frame = text(&mut app, 80, 24);
    assert!(footer(&frame).contains("d  revoke"), "{}", footer(&frame));
}

#[test]
fn footer_hints_are_dropped_whole_never_cut() {
    let keys = [
        ("n", "new"),
        ("e", "edit"),
        ("s", "share"),
        ("p", "pause"),
        ("?", "help"),
        ("q", "quit"),
    ];
    let all = ui::fit_hints(&keys, 200);
    assert_eq!(all.len(), keys.len());
    // Narrow: the first hints and help and quit, in order.
    let some = ui::fit_hints(&keys, 40);
    assert_eq!(some.first(), Some(&("n", "new")));
    assert_eq!(&some[some.len() - 2..], &[("?", "help"), ("q", "quit")]);
    assert!(some.len() < keys.len());
    // A toast takes room from the hints rather than covering them.
    let home = tempfile::tempdir().unwrap();
    let mut app = filled_app(home.path());
    app.tab = Tab::Overview;
    app.notify("URL copied");
    let frame = text(&mut app, 80, 24);
    let line = footer(&frame);
    assert!(
        line.contains("URL copied") && line.contains("1-5  views"),
        "{line:?}"
    );
}

#[test]
fn the_token_table_keeps_names_on_a_narrow_terminal() {
    let home = tempfile::tempdir().unwrap();
    let mut app = filled_app(home.path());
    app.tab = Tab::Tokens;
    let narrow = text(&mut app, 80, 30);
    for name in ["name", "alice", "ci-bot", "req today", "status"] {
        assert!(narrow.contains(name), "80 columns lack {name:?}:\n{narrow}");
    }
    // The prefix goes first; a wide terminal has every column.
    assert!(!narrow.contains("prefix"));
    let wide = text(&mut app, 200, 40);
    for col in ["prefix", "session", "created", "last used", "expires"] {
        assert!(wide.contains(col), "200 columns lack {col:?}");
    }
}

#[test]
fn the_accounts_popup_keeps_names_and_every_key_at_80_columns() {
    let home = tempfile::tempdir().unwrap();
    let mut app = filled_app(home.path());
    open(&mut app, Action::Accounts);
    let frame = text(&mut app, 80, 24);
    for s in ["personal-max", "spare", "x  remove", "esc  close"] {
        assert!(frame.contains(s), "lacks {s:?}:\n{frame}");
    }
}

#[test]
fn the_help_fits_a_24_row_terminal() {
    let home = tempfile::tempdir().unwrap();
    let mut app = filled_app(home.path());
    open(&mut app, Action::Help);
    let frame = text(&mut app, 80, 24);
    for s in [
        "switch view",
        "revoke",
        "ctrl-s",
        "quit (q asks",
        "press any key",
    ] {
        assert!(frame.contains(s), "help at 80x24 lacks {s:?}:\n{frame}");
    }
}

#[test]
fn the_header_never_runs_the_brand_into_the_status() {
    let home = tempfile::tempdir().unwrap();
    let mut app = filled_app(home.path());
    for (w, h) in [(100, 32), (120, 40), (200, 60)] {
        let frame = text(&mut app, w, h);
        assert!(
            frame.contains("to your coding agents  "),
            "{w}x{h}:\n{frame}"
        );
        assert!(frame.contains("local   http"), "{w}x{h}");
    }
}

#[test]
fn commands_titles_stay_whole_at_80_columns() {
    let home = tempfile::tempdir().unwrap();
    let mut app = filled_app(home.path());
    app.tab = Tab::Commands;
    let frame = text(&mut app, 80, 30);
    for action in &Action::ALL[..8] {
        assert!(
            frame.contains(action.title()),
            "{action:?} is cut:\n{frame}"
        );
    }
    // The terminal command is in the panel below instead.
    assert!(frame.contains("cli    hashi serve --tunnel"));
}

#[test]
fn a_first_run_without_agents_says_what_to_do() {
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(empty(home.path()), None, false);
    app.reduced_motion = true;
    for (w, h) in SIZES {
        let frame = text(&mut app, w, h);
        assert!(frame.contains("No coding agent found"), "{w}x{h}:\n{frame}");
        assert!(frame.contains("Install Claude Code"), "{w}x{h}");
    }
    let wide = text(&mut app, 200, 60);
    assert!(wide.contains("none found: see Doctor"));
    // An empty list says how to fill it.
    app.tab = Tab::Requests;
    let frame = text(&mut app, 80, 24);
    assert!(frame.contains("Press a to ask an agent"), "{frame}");
    open(&mut app, Action::Accounts);
    let frame = text(&mut app, 80, 24);
    assert!(frame.contains("Press a to add one"), "{frame}");
    assert!(!frame.contains("for this account"), "{frame}");
}

#[test]
fn a_first_run_with_an_agent_points_at_ask_and_tokens() {
    let home = tempfile::tempdir().unwrap();
    let store = hashi::server::open_store(home.path()).unwrap();
    hashi::server::ensure_admin(&store).unwrap();
    let mut app = App::new(app_state(store, home.path(), installed()), None, false);
    let frame = text(&mut app, 120, 40);
    assert!(frame.contains("Press a to ask from here"), "{frame}");
    assert!(frame.contains("3 Tokens"), "{frame}");
}

#[test]
fn both_share_forms_fit_a_24_row_terminal() {
    let home = tempfile::tempdir().unwrap();
    let mut app = filled_app(home.path());
    open(&mut app, Action::NewInvite);
    let frame = text(&mut app, 80, 24);
    for want in ["label", "stop at", "project", "create link"] {
        assert!(frame.contains(want), "{want} missing:\n{frame}");
    }
    open(&mut app, Action::ShareLogin);
    let frame = text(&mut app, 80, 24);
    for want in [
        "label",
        "login",
        "usage from",
        "remind at",
        "share for",
        "link valid",
        "create link",
    ] {
        assert!(frame.contains(want), "{want} missing:\n{frame}");
    }
}

#[test]
fn the_pool_popups_fit_a_24_row_terminal() {
    let home = tempfile::tempdir().unwrap();
    let mut app = filled_app(home.path());
    open(&mut app, Action::Pool);
    let frame = text(&mut app, 80, 24);
    for want in ["pool", "open link", "join a pool"] {
        assert!(frame.contains(want), "{want} missing:\n{frame}");
    }
    app.on_key(key(KeyCode::Char('o')));
    let frame = text(&mut app, 80, 24);
    for want in ["joins for", "people", "link via", "open link"] {
        assert!(frame.contains(want), "{want} missing:\n{frame}");
    }
    app.on_key(key(KeyCode::Esc));
    app.on_key(key(KeyCode::Char('a')));
    let frame = text(&mut app, 80, 24);
    for want in ["link", "your name", "agents", "stop at", "stay for", "join"] {
        assert!(frame.contains(want), "{want} missing:\n{frame}");
    }
}
