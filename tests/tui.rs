//! Renders the dashboard into an off-screen buffer and checks its content.
//! Set `HASHI_DUMP_TUI=1` to print the rendered frames.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, RwLock};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hashi::agent::{AgentKind, Usage};
use hashi::api::{AgentInfo, AppState, RateLimiter, Runtime};
use hashi::events::{Bus, Event};
use hashi::queue::Dispatcher;
use hashi::store::{NewRequest, NewToken, Store};
use hashi::token::{Role, SessionMode};
use hashi::tui::app::{App, Popup, Tab};
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
            local_url: "http://127.0.0.1:7878".into(),
            public_url: RwLock::new(None),
            name: "Hashi".into(),
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
    if let Some(dir) = std::env::var_os("HASHI_DUMP_HTML") {
        dump_html(&buf, std::path::Path::new(&dir));
    }
    out
}

/// Writes the frame as coloured HTML, for eyeballing the palette in a browser.
fn dump_html(buf: &ratatui::buffer::Buffer, dir: &std::path::Path) {
    use ratatui::style::{Color, Modifier};
    let css = |c: Color, fallback: &str| match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => fallback.to_owned(),
    };
    let mut html = String::from(
        "<html><body style='margin:0;background:#0e0b16'><pre style='font:15px/1.0 Menlo,monospace;color:#e8e4f5;padding:16px;margin:0'>",
    );
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let mut style = format!("color:{};", css(cell.fg, "inherit"));
            if let Color::Rgb(..) = cell.bg {
                style.push_str(&format!("background:{};", css(cell.bg, "transparent")));
            }
            if cell.modifier.contains(Modifier::BOLD) {
                style.push_str("font-weight:bold;");
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                style.push_str("font-style:italic;");
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                style.push_str("text-decoration:underline;");
            }
            let sym = cell.symbol().replace('&', "&amp;").replace('<', "&lt;");
            html.push_str(&format!("<span style='{style}'>{sym}</span>"));
        }
        html.push('\n');
    }
    html.push_str("</pre></body></html>");
    let name = std::thread::current()
        .name()
        .unwrap_or("frame")
        .replace("::", "_");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{name}.html")), html).unwrap();
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn overview_shows_brand_agents_and_stats() {
    let mut app = App::new(state(), None, true);
    for ev in [
        Event::Log("Claude Code ready (2.1.318)".into()),
        Event::Log("listening on http://127.0.0.1:7878".into()),
        Event::TunnelReady("https://calm-river-bridge.trycloudflare.com".into()),
        Event::Queued {
            id: "00000000-a".into(),
            agent: AgentKind::Claude,
            token: "ci-bot".into(),
        },
        Event::Started {
            id: "00000000-a".into(),
            agent: AgentKind::Claude,
        },
        Event::Finished {
            id: "00000000-a".into(),
            agent: AgentKind::Claude,
            ok: true,
            duration_ms: 5_132,
            error: None,
        },
        Event::Queued {
            id: "00000001-b".into(),
            agent: AgentKind::Codex,
            token: "slack-bot".into(),
        },
        Event::Finished {
            id: "00000001-b".into(),
            agent: AgentKind::Codex,
            ok: false,
            duration_ms: 900,
            error: Some("please sign in again".into()),
        },
        Event::Started {
            id: "00000002-c".into(),
            agent: AgentKind::Claude,
        },
    ] {
        app.on_event(ev);
    }
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("trycloudflare.com"));
    assert!(out.contains("the bridge"));
    assert!(out.contains("Claude Code"));
    assert!(out.contains("not installed"));
    assert!(out.contains("in flight"));
    assert!(out.contains("http://127.0.0.1:7878"));
}

#[test]
fn compact_layout_fits_small_terminals() {
    let mut app = App::new(state(), None, false);
    let out = draw(&mut app, 80, 24);
    assert!(out.contains("hashi"));
}

#[test]
fn requests_view_shows_detail_of_selection() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('2')));
    assert_eq!(app.tab, Tab::Requests);
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("Explain this stack trace"));
    assert!(out.contains("waiting for the agent"));
    app.on_key(key(KeyCode::Down));
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("Here is the answer."));
}

#[test]
fn first_run_reveals_admin_token_once() {
    let mut app = App::new(state(), Some("hashi_secret_value".into()), false);
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("hashi_secret_value"));
    app.on_key(key(KeyCode::Enter));
    assert!(app.popup.is_none());
    assert!(!draw(&mut app, 140, 40).contains("hashi_secret_value"));
}

#[test]
fn token_form_creates_token_and_reveals_it() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('n')));
    for c in "ci-bot".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    draw(&mut app, 140, 40);
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::Reveal { secret, .. }) = &app.popup else {
        panic!("expected reveal popup")
    };
    assert!(secret.starts_with("hashi_"));
    assert!(draw(&mut app, 140, 40).contains("Token for ci-bot"));
    assert_eq!(app.tokens.len(), 2);
}

#[test]
fn last_admin_cannot_be_revoked_from_the_ui() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(app.tokens.len(), 1);
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|(m, _)| m.contains("last admin"))
    );
}

#[test]
fn every_view_renders() {
    let mut app = App::new(state(), None, false);
    for tab in ['1', '2', '3', '4', '?'] {
        app.on_key(key(KeyCode::Char(tab)));
        draw(&mut app, 120, 34);
    }
}

#[test]
fn quitting_with_work_in_flight_asks_first() {
    // The fixture leaves one request running.
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('q')));
    assert!(matches!(app.popup, Some(Popup::ConfirmQuit { pending: 1 })));
    assert!(draw(&mut app, 140, 40).contains("still in flight"));
    app.on_key(key(KeyCode::Char('n')));
    assert!(!app.should_quit && app.popup.is_none());
    app.on_key(key(KeyCode::Char('q')));
    app.on_key(key(KeyCode::Char('y')));
    assert!(app.should_quit);
}

#[test]
fn escape_never_quits() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Esc));
    assert!(!app.should_quit);
}

#[test]
fn empty_token_name_shows_inline_error() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('n')));
    app.on_key(key(KeyCode::Enter));
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("Name is required"));
    app.on_key(key(KeyCode::Char('x')));
    assert!(!draw(&mut app, 140, 40).contains("Name is required"));
}

#[test]
fn running_requests_show_elapsed_time() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('2')));
    // Newest first: the running request is selected.
    assert!(draw(&mut app, 140, 40).contains("so far"));
}

#[test]
fn reduced_motion_freezes_animation() {
    let mut app = App::new(state(), None, false);
    app.reduced_motion = true;
    for _ in 0..10 {
        app.on_tick();
    }
    assert_eq!(app.tick, 0);
}
