//! Renders the dashboard into an off-screen buffer and checks its content.
//! Set `HASHI_DUMP_TUI=1` to print the rendered frames.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(
    dead_code,
    clippy::redundant_pattern_matching,
    clippy::field_reassign_with_default
)]

use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hashi::agent::{AgentKind, Usage};
use hashi::api::{AgentInfo, AppState, RateLimiter, Runtime};
use hashi::events::{Bus, Event};
use hashi::guard::{GIB, ManualProbe, ResourceGuard, ResourceSettings};
use hashi::queue::Dispatcher;
use hashi::store::{NewRequest, NewToken, Store};
use hashi::token::{Role, SessionMode};
use hashi::tui::app::{App, Popup};
use hashi::tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn state() -> AppState {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let (admin, _) = store
        .create_token(&NewToken {
            name: "admin".into(),
            role: Role::Admin,
            session_mode: SessionMode::Stateless,
            agents: AgentKind::ALL.to_vec(),
            expires_in_days: None,
            note: None,
            limits: Default::default(),
            accounts: Default::default(),
            project: None,
        })
        .unwrap();
    for (i, (agent, msg)) in [
        (AgentKind::Claude, "Summarise the open pull requests"),
        (AgentKind::Codex, "Write a migration for the users table"),
        (AgentKind::Claude, "Explain this stack trace"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("{i:08}-0000-4000-8000-000000000000");
        store
            .insert_request(NewRequest {
                id: &id,
                token: &admin,
                agent,
                model: None,
                message: msg,
            })
            .unwrap();
        store.claim_next(agent).unwrap();
        if i < 2 {
            let usage = Usage {
                input_tokens: 1200,
                output_tokens: 340,
                cost_usd: Some(0.012),
            };
            store
                .complete_request(&id, "Here is the answer.\nWith two lines.", Some(usage))
                .unwrap();
        }
    }
    AppState {
        store,
        bus: Bus::default(),
        dispatcher: Dispatcher::new(),
        runtime: Arc::new(Runtime {
            started: Instant::now(),
            listen: hashi::net::Listen::new("http://127.0.0.1:7878"),
            public_url: RwLock::new(None),
            name: "Hashi".into(),
            tunnel: Default::default(),
            tailscale: Default::default(),
            agents: vec![
                AgentInfo {
                    agent: AgentKind::Claude,
                    available: true,
                    version: Some("2.1.318 (Claude Code)".into()),
                    path: "claude".into(),
                },
                AgentInfo {
                    agent: AgentKind::Codex,
                    available: false,
                    version: None,
                    path: "codex".into(),
                },
            ],
        }),
        limiter: Arc::new(RateLimiter::new(0)),
        default_agent: AgentKind::Claude,
        queue_limit: 100,
        run_timeout_secs: 600,
        guard: Arc::new(ResourceGuard::new(
            &ResourceSettings {
                max_running: Some(2),
                ..ResourceSettings::with_probe(Arc::new(ManualProbe::new(16 * GIB, 10 * GIB)))
            },
            std::path::PathBuf::from("."),
            Bus::default(),
        )),
        waits: Default::default(),
    }
}

fn draw(app: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::render(app, f)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    if std::env::var_os("HASHI_DUMP_TUI").is_some() {
        println!("{out}");
    }
    out
}

/// Writes the frame as coloured HTML, for eyeballing the palette in a browser.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

const EVIL: &str = "\u{1b}]52;c;aGFjaw==\u{7}\u{1b}]0;pwned\u{7}\u{1b}[2J\u{9b}31m\u{202e}gnp.exe\u{2066}\u{200b}\u{200d}\u{feff}名前👨‍👩‍👧‍👦\u{fe0f}🇺🇸\r\n\tX\u{0301}\u{0301}\u{0301}";

fn evil_long() -> String {
    format!("{}{}", EVIL, "W名".repeat(20_000))
}

fn hostile_state() -> AppState {
    let st = state();
    let store = &st.store;
    let (tok, _) = store
        .create_token(&NewToken {
            name: evil_long().chars().take(200).collect(),
            role: Role::User,
            session_mode: SessionMode::Stateless,
            agents: AgentKind::ALL.to_vec(),
            expires_in_days: None,
            note: Some(evil_long()),
            limits: Default::default(),
            accounts: Default::default(),
            project: None,
        })
        .unwrap();
    for i in 0..3 {
        let id = format!("{i:08}-1111-4000-8000-000000000000");
        let msg = evil_long();
        store
            .insert_request(NewRequest {
                id: &id,
                token: &tok,
                agent: AgentKind::Claude,
                model: None,
                message: &msg,
            })
            .unwrap();
        store.claim_next(AgentKind::Claude).unwrap();
        if i == 0 {
            store.complete_request(&id, &evil_long(), None).unwrap();
        }
        if i == 1 {
            let _ = store.fail_request(&id, &evil_long());
        }
    }
    let (_, code) = hashi::access::open_link(store, "cli", Default::default()).unwrap();
    let _ = hashi::access::submit_new(
        store,
        hashi::access::NewPerson {
            code,
            name: EVIL.chars().take(60).collect(),
            message: Some(EVIL.into()),
            agents: None,
        },
    );
    st
}

thread_local!(static CTX: std::cell::RefCell<String> = Default::default());
fn check(buf: &ratatui::buffer::Buffer) {
    let w = buf.area.width as usize;
    for (i, c) in buf.content.iter().enumerate() {
        let s = c.symbol();
        if s.chars().any(|ch| {
            ch.is_control()
                || hashi::diagnostics::is_bidi_control(ch)
                || matches!(ch, '\u{200b}' | '\u{feff}')
        }) {
            let row: String = buf.content[(i / w) * w..(i / w + 1) * w]
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            CTX.with(|c| {
                eprintln!(
                    "LEAK [{}] {}x{} cell {s:?} row: {}",
                    c.borrow(),
                    buf.area.width,
                    buf.area.height,
                    row.escape_debug()
                        .to_string()
                        .chars()
                        .take(160)
                        .collect::<String>()
                )
            });
            return;
        }
    }
}
fn ctx(s: String) {
    CTX.with(|c| *c.borrow_mut() = s);
}

fn draw_buf(app: &mut App, w: u16, h: u16) {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::render(app, f)).unwrap();
    check(term.backend().buffer());
}

const SIZES: &[(u16, u16)] = &[
    (0, 0),
    (1, 1),
    (2, 1),
    (1, 2),
    (3, 3),
    (5, 2),
    (10, 4),
    (20, 6),
    (40, 10),
    (80, 24),
    (300, 90),
];

fn popups(app: &App) -> Vec<Popup> {
    use hashi::tui::app::*;
    vec![
        Popup::Help,
        Popup::Reveal {
            title: EVIL.into(),
            secret: evil_long(),
        },
        Popup::ConfirmQuit { pending: u64::MAX },
        Popup::ConfirmRevoke {
            id: "x".into(),
            name: evil_long(),
        },
        Popup::InviteReveal {
            label: evil_long(),
            code: "a".repeat(10_000),
            link_hours: u32::MAX,
            via: hashi::links::Via::Auto,
            pool: false,
        },
        Popup::InviteReveal {
            label: EVIL.into(),
            code: "abc".into(),
            link_hours: 1,
            via: hashi::links::Via::Auto,
            pool: false,
        },
        Popup::ConfirmCancelRequest {
            id: "x".into(),
            token: evil_long(),
        },
        Popup::ConfirmCancelInvite {
            id: "x".into(),
            label: evil_long(),
        },
        Popup::Approval { url: evil_long() },
        Popup::RemoteAnswer {
            remote: evil_long(),
            answer: Some(Err(evil_long())),
            scroll: u16::MAX,
        },
        Popup::RemoteAnswer {
            remote: EVIL.into(),
            answer: Some(Ok(evil_long())),
            scroll: 3,
        },
        Popup::RemoteAnswer {
            remote: EVIL.into(),
            answer: None,
            scroll: 0,
        },
        Popup::Accounts {
            selected: usize::MAX,
        },
        Popup::Projects {
            selected: usize::MAX,
        },
        Popup::Remotes {
            selected: usize::MAX,
        },
        Popup::Audit { scroll: usize::MAX },
        Popup::ConfirmRemoveAccount {
            agent: AgentKind::Claude,
            name: evil_long(),
            tokens: usize::MAX,
        },
        Popup::ConfirmRemoveProject {
            name: evil_long(),
            tokens: 9,
        },
        Popup::ConfirmRemoveRemote { name: evil_long() },
        Popup::ReportSaved {
            path: evil_long(),
            include_prompts: true,
        },
        Popup::ConfirmUpdate,
        Popup::AccessRequests {
            selected: usize::MAX,
            all: true,
        },
        Popup::AccessRequests {
            selected: 0,
            all: false,
        },
        Popup::AccessLink {
            code: "c".repeat(5000),
            hours: u32::MAX,
            project: Some(evil_long()),
        },
        Popup::AddAccount(Box::default()),
        Popup::Ask(Box::new(hashi::tui::forms::AskForm::new(
            vec![],
            vec![evil_long()],
        ))),
    ]
    .into_iter()
    .chain(app.tokens.first().map(|t| {
        Popup::EditToken(Box::new(hashi::tui::forms::EditTokenForm::new(
            t,
            vec![evil_long()],
        )))
    }))
    .collect()
}

#[test]
fn hostile_everything_every_size() {
    let mut app = App::new(hostile_state(), None, false);
    app.on_event(Event::Log(evil_long()));
    app.on_event(Event::Finished {
        id: evil_long(),
        agent: AgentKind::Claude,
        ok: false,
        duration_ms: u64::MAX,
        error: Some(evil_long()),
    });
    app.on_event(Event::TunnelDown(evil_long()));
    app.on_event(Event::ApprovalNeeded(evil_long()));
    app.on_event(Event::TunnelReady(evil_long()));
    app.notify(evil_long());
    for tab in ['1', '2', '3', '4', '5'] {
        app.popup = None;
        app.on_key(key(KeyCode::Char(tab)));
        ctx(format!("tab {tab}"));
        for _ in 0..3 {
            app.on_key(key(KeyCode::Down));
        }
        for &(w, h) in SIZES {
            if let Err(_) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| draw_buf(&mut app, w, h)))
            {
                panic!("PANIC tab at {w}x{h}");
            }
        }
        app.on_key(key(KeyCode::Char('J')));
        for &(w, h) in SIZES {
            if let Err(_) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| draw_buf(&mut app, w, h)))
            {
                panic!("PANIC tab at {w}x{h}");
            }
        }
    }
    let ps = popups(&app);
    for p in ps {
        let name = format!("{:?}", p).chars().take(40).collect::<String>();
        ctx(name.clone());
        app.popup = Some(p);
        for &(w, h) in SIZES {
            let r =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| draw_buf(&mut app, w, h)));
            if r.is_err() {
                panic!("PANIC popup {name} at {w}x{h}");
            }
        }
    }
}

#[test]
fn popup_keys_with_stale_index() {
    let mut app = App::new(hostile_state(), None, false);
    for p in [
        Popup::Accounts {
            selected: usize::MAX,
        },
        Popup::Projects {
            selected: usize::MAX,
        },
        Popup::Remotes {
            selected: usize::MAX,
        },
        Popup::AccessRequests {
            selected: usize::MAX,
            all: true,
        },
        Popup::Audit { scroll: usize::MAX },
    ] {
        for k in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Enter,
            KeyCode::Char('d'),
            KeyCode::Char('a'),
            KeyCode::Char('e'),
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::PageDown,
            KeyCode::End,
        ] {
            app.popup = Some(p.clone());
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                app.on_key(key(k));
                draw_buf(&mut app, 80, 24);
            }));
            if r.is_err() {
                panic!("PANIC key {k:?} on {p:?}");
            }
        }
    }
}

#[test]
fn toast_and_feed() {
    let mut app = App::new(state(), None, false);
    let e = "fail \u{202e}txt.exe\u{2066} end";
    app.on_event(Event::Finished {
        id: "00000000-0000".into(),
        agent: AgentKind::Claude,
        ok: false,
        duration_ms: 1,
        error: Some(e.into()),
    });
    app.on_event(Event::Log(e.into()));
    ctx("feed".into());
    draw_buf(&mut app, 140, 40);
    app.notify(format!("connect failed: {e}"));
    ctx("toast".into());
    draw_buf(&mut app, 140, 40);
    app.on_key(key(KeyCode::Char('2')));
    ctx("toast tab2".into());
    app.notify(format!("connect failed: {e}"));
    draw_buf(&mut app, 140, 40);
}

#[test]
fn user_prompt_bidi_reaches_requests_table() {
    let st = state();
    let (tok, _) = st
        .store
        .create_token(&NewToken {
            name: "bob".into(),
            role: Role::User,
            session_mode: SessionMode::Stateless,
            agents: AgentKind::ALL.to_vec(),
            expires_in_days: None,
            note: None,
            limits: Default::default(),
            accounts: Default::default(),
            project: None,
        })
        .unwrap();
    st.store
        .insert_request(NewRequest {
            id: "99999999-0000-4000-8000-000000000000",
            token: &tok,
            agent: AgentKind::Claude,
            model: None,
            message: "hi \u{202e}cod.exe\u{feff}\u{200b} ok",
        })
        .unwrap();
    let mut app = App::new(st, None, false);
    app.on_key(key(KeyCode::Char('2')));
    let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
    term.draw(|f| ui::render(&mut app, f)).unwrap();
    let bad: Vec<String> = term
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_owned())
        .filter(|s| {
            s.chars().any(|c| {
                hashi::diagnostics::is_bidi_control(c) || c == '\u{feff}' || c == '\u{200b}'
            })
        })
        .collect();
    assert!(
        bad.is_empty(),
        "cells carrying invisible controls to the terminal: {bad:?}"
    );
}

#[test]
fn connect_error_from_remote() {
    let mut app = App::new(state(), None, false);
    let mut f = hashi::tui::forms::ConnectForm::default();
    f.error = Some((0, "remote said: \u{202e}gnp.exe\u{200b}".into()));
    f.link = "http://x/\u{202e}abc".into();
    app.popup = Some(Popup::Connect(Box::new(f)));
    ctx("connect".into());
    draw_buf(&mut app, 140, 40);
}
