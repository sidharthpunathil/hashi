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
use hashi::guard::{GIB, HoldReason, ManualProbe, ResourceGuard, ResourceSettings};
use hashi::queue::Dispatcher;
use hashi::store::{NewRequest, NewToken, Store};
use hashi::token::{Role, SessionMode};
use hashi::tui::app::{Action, App, Popup, ShareForm, Tab, TokenForm};
use hashi::tui::forms::EditTokenForm;
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
    assert!(
        !out.contains("not installed"),
        "only installed agents get cards"
    );
    assert!(out.contains("in flight"));
    assert!(out.contains("http://127.0.0.1:7878"));
}

#[test]
fn header_shows_an_available_update() {
    let mut app = App::new(state(), None, false);
    assert!(!draw(&mut app, 140, 40).contains("hashi update"));
    app.on_event(Event::UpdateAvailable {
        version: "9.9.9".into(),
        url: "https://github.com/sidharthpunathil/hashi/releases/tag/v9.9.9".into(),
    });
    assert_eq!(app.update.as_deref(), Some("9.9.9"));
    assert!(draw(&mut app, 140, 40).contains("update v9.9.9: hashi update"));
    assert!(draw(&mut app, 120, 24).contains("update v9.9.9: hashi update"));
}

#[test]
fn header_warns_about_a_bind_outside_loopback() {
    let mut app = App::new(state(), None, false);
    assert!(!draw(&mut app, 140, 40).contains("! exposed"));
    let mut st = state();
    let rt = Arc::get_mut(&mut st.runtime).unwrap();
    rt.listen = hashi::net::Listen::switchable("0.0.0.0:7878".parse().unwrap(), false).0;
    let mut app = App::new(st, None, false);
    assert!(draw(&mut app, 140, 40).contains("! exposed, no TLS"));
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
    for tab in ['1', '2', '3', '4', '5', '?'] {
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

fn add_user(app: &mut App, name: &str) {
    app.state
        .store
        .create_token(&NewToken {
            name: name.into(),
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
    app.refresh();
}

fn select_token(app: &mut App, name: &str) {
    let i = app.tokens.iter().position(|t| t.name == name).unwrap();
    app.tokens_table.select(Some(i));
}

fn token_id(app: &App, name: &str) -> String {
    app.tokens
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
        .clone()
}

fn toast(app: &App) -> String {
    app.toast
        .as_ref()
        .map(|(m, _)| m.clone())
        .unwrap_or_default()
}

fn share_invite(app: &mut App, label: &str) {
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('s')));
    for c in label.chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    app.on_key(key(KeyCode::Enter));
}

#[test]
fn pause_key_toggles_token() {
    let mut app = App::new(state(), None, false);
    add_user(&mut app, "alice");
    app.on_key(key(KeyCode::Char('3')));
    select_token(&mut app, "alice");
    let id = token_id(&app, "alice");

    app.on_key(key(KeyCode::Char('p')));
    assert!(app.state.store.get_token(&id).unwrap().unwrap().paused);
    assert!(toast(&app).contains("paused"));
    assert!(draw(&mut app, 140, 40).contains("‖ paused"));

    app.on_key(key(KeyCode::Char('p')));
    assert!(!app.state.store.get_token(&id).unwrap().unwrap().paused);
    assert!(toast(&app).contains("resumed"));

    let audit = app.state.store.list_audit(10).unwrap();
    assert_eq!(audit.len(), 2);
    assert!(
        audit
            .iter()
            .all(|e| e.actor == "dashboard" && e.action == "token.update" && e.target == "alice")
    );
    assert!(audit.iter().any(|e| e.detail == "paused"));
    assert!(audit.iter().any(|e| e.detail == "resumed"));
}

#[test]
fn last_admin_cannot_be_paused_from_ui() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    select_token(&mut app, "admin");
    app.on_key(key(KeyCode::Char('p')));
    let id = token_id(&app, "admin");
    assert!(!app.state.store.get_token(&id).unwrap().unwrap().paused);
    assert!(toast(&app).contains("last active admin"));
}

#[test]
fn share_form_creates_invite_and_shows_link() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('s')));
    assert!(matches!(app.popup, Some(Popup::Share(_))));
    assert!(draw(&mut app, 140, 40).contains("link valid for"));
    for c in "alice".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    // Set a daily cap of 25 prompts.
    for _ in 0..3 {
        app.on_key(key(KeyCode::Down));
    }
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::InviteReveal { code, .. }) = &app.popup else {
        panic!("expected invite reveal popup")
    };
    assert!(app.invite_url(code).contains("/claim/hashi_inv_"));
    assert!(app.local_only());
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("Invite for alice"));
    assert!(out.contains("/claim/hashi_inv_"));
    assert!(out.contains("expires in 24h"));
    assert!(out.contains("only works on this machine"));

    let invites = app.state.store.list_invites().unwrap();
    assert_eq!(invites.len(), 1);
    assert_eq!(invites[0].label, "alice");
    assert_eq!(invites[0].limits.daily_requests, Some(25));
    assert_eq!(invites[0].limits.max_pending, Some(3));
    let audit = app.state.store.list_audit(10).unwrap();
    assert!(
        audit
            .iter()
            .any(|e| e.actor == "dashboard" && e.action == "invite.create" && e.target == "alice")
    );

    app.on_key(key(KeyCode::Esc));
    assert!(app.popup.is_none());
}

#[test]
fn share_form_requires_label() {
    let mut app = App::new(state(), None, false);
    share_invite(&mut app, "");
    assert!(matches!(app.popup, Some(Popup::Share(_))));
    assert!(draw(&mut app, 140, 40).contains("Label is required"));
    assert!(app.state.store.list_invites().unwrap().is_empty());
    app.on_key(key(KeyCode::Char('b')));
    assert!(!draw(&mut app, 140, 40).contains("Label is required"));
}

#[test]
fn pending_invites_are_listed_and_cancellable() {
    let mut app = App::new(state(), None, false);
    share_invite(&mut app, "bob-laptop");
    app.on_key(key(KeyCode::Enter));
    assert!(app.popup.is_none());
    app.refresh();
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("pending invites"));
    assert!(out.contains("bob-laptop"));

    // x does nothing until the invites table has focus.
    app.on_key(key(KeyCode::Char('x')));
    assert!(app.popup.is_none());
    app.on_key(key(KeyCode::Char('i')));
    assert!(draw(&mut app, 140, 40).contains("cancel invite"));
    app.on_key(key(KeyCode::Char('x')));
    assert!(matches!(app.popup, Some(Popup::ConfirmCancelInvite { .. })));
    assert!(draw(&mut app, 140, 40).contains("Cancel the invite for"));
    app.on_key(key(KeyCode::Char('y')));
    assert!(app.state.store.list_invites().unwrap().is_empty());
    assert!(app.invites.is_empty());
    assert!(!draw(&mut app, 140, 40).contains("pending invites"));
    let audit = app.state.store.list_audit(10).unwrap();
    assert!(
        audit
            .iter()
            .any(|e| e.action == "invite.cancel" && e.target == "bob-laptop")
    );
}

#[test]
fn tokens_table_shows_status_and_usage() {
    let mut app = App::new(state(), None, false);
    let (capped, _) = app
        .state
        .store
        .create_token(&NewToken {
            name: "capped".into(),
            role: Role::User,
            session_mode: SessionMode::Stateless,
            agents: AgentKind::ALL.to_vec(),
            expires_in_days: None,
            note: None,
            limits: hashi::token::Limits {
                daily_requests: Some(100),
                ..Default::default()
            },
            accounts: Default::default(),
            project: None,
        })
        .unwrap();
    for i in 0..2 {
        let id = format!("{i:08}-1111-4000-8000-000000000000");
        app.state
            .store
            .insert_request(NewRequest {
                id: &id,
                token: &capped,
                agent: AgentKind::Claude,
                model: None,
                message: "hi",
            })
            .unwrap();
    }
    app.on_key(key(KeyCode::Char('3')));
    app.refresh();
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("status"));
    assert!(out.contains("today"));
    assert!(out.contains("● active"));
    assert!(out.contains("2/100"));
    // The fixture's admin sent three prompts today and has no daily cap.
    let admin_row = out.lines().find(|l| l.contains(" admin ")).unwrap();
    assert!(admin_row.contains(" 3 "));
}

fn with_agents(installed: &[AgentKind]) -> AppState {
    let mut st = state();
    let agents = AgentKind::ALL
        .into_iter()
        .map(|agent| AgentInfo {
            agent,
            available: installed.contains(&agent),
            version: installed.contains(&agent).then(|| "1.0.0".to_owned()),
            path: agent.binary_name().to_owned(),
        })
        .collect();
    st.runtime = Arc::new(Runtime {
        started: Instant::now(),
        listen: hashi::net::Listen::new("http://127.0.0.1:7878"),
        public_url: RwLock::new(None),
        agents,
        name: "Hashi".into(),
        tunnel: Default::default(),
        tailscale: Default::default(),
    });
    st
}

#[test]
fn agent_card_tags_a_version_newer_than_verified() {
    let mut app = App::new(with_agents(&[AgentKind::Claude]), None, false);
    assert!(!draw(&mut app, 140, 40).contains("newer than verified"));

    let mut st = with_agents(&[AgentKind::Claude]);
    Arc::get_mut(&mut st.runtime).unwrap().agents[0].version = Some("9.9.0 (Claude Code)".into());
    let mut app = App::new(st, None, false);
    assert!(draw(&mut app, 140, 40).contains("newer than verified"));
}

#[test]
fn overview_falls_back_to_claude_and_codex_when_nothing_is_installed() {
    let mut app = App::new(with_agents(&[]), None, false);
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("Claude Code") && out.contains("Codex"));
    assert_eq!(out.matches("not installed").count(), 2);
}

#[test]
fn overview_shows_a_card_per_installed_agent() {
    let installed = [
        AgentKind::Claude,
        AgentKind::Gemini,
        AgentKind::Opencode,
        AgentKind::Qwen,
    ];
    let mut app = App::new(with_agents(&installed), None, false);
    let out = draw(&mut app, 160, 44);
    for agent in installed {
        assert!(
            out.contains(agent.display_name()),
            "missing card for {agent}"
        );
    }
    assert!(!out.contains("Cursor CLI"));
}

#[test]
fn overview_shows_memory_and_run_slots() {
    let mut app = App::new(state(), None, false);
    let out = draw(&mut app, 140, 40);
    let line = out.lines().find(|l| l.contains(" memory ")).unwrap();
    assert!(line.contains("6.0 GiB / 16.0 GiB"), "{line}");
    assert!(line.contains("running 0/2"), "{line}");
    assert!(!line.contains("held"), "{line}");
}

#[test]
fn overview_says_when_work_is_held() {
    let mut st = state();
    let p: Arc<dyn hashi::guard::Probe> = Arc::new(ManualProbe::new(16 * GIB, GIB));
    st.guard = Arc::new(ResourceGuard::new(
        &ResourceSettings::with_probe(p),
        std::path::PathBuf::from("."),
        Bus::default(),
    ));
    let hold = st.guard.check().unwrap_err();
    assert_eq!(hold.reason, HoldReason::Memory);
    st.guard.hold(&hold);
    let mut app = App::new(st, None, false);
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("held: low memory"), "{out}");
}

/// Finishes the fixture's running request so nothing is in flight.
fn finish_running(app: &mut App) {
    app.state
        .store
        .complete_request("00000002-0000-4000-8000-000000000000", "done", None)
        .unwrap();
    app.refresh();
}

#[test]
fn idle_dashboard_does_not_animate() {
    let mut app = App::new(state(), None, false);
    finish_running(&mut app);
    assert!(!app.animating());
    // Data was just loaded, so an idle tick asks for no redraw at all.
    assert!(!app.on_tick());
    assert_eq!(app.tick, 0);

    app.notify("copied");
    assert!(app.animating());
    app.toast = None;
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('n')));
    assert!(app.animating(), "the form cursor blinks");
}

#[test]
fn running_request_animates() {
    // The fixture leaves one request running.
    let mut app = App::new(state(), None, false);
    assert!(app.animating());
    assert!(app.on_tick());
    assert_eq!(app.tick, 1);
    // Reduced motion still redraws for the elapsed time, but keeps the
    // spinners still.
    app.reduced_motion = true;
    assert!(app.on_tick());
    assert_eq!(app.tick, 1);
}

#[test]
fn requests_view_handles_many_large_prompts() {
    let mut app = App::new(state(), None, false);
    let admin = app.state.store.list_tokens().unwrap().remove(0);
    for i in 0..300 {
        // Short lines, one per row in the detail pane, with a narrow
        // multibyte character near the 200 char cut.
        let mut prompt = format!("p{i:03} {} café", "abcdefgh ".repeat(21));
        let mut n = 0;
        while prompt.len() < 32 * 1024 {
            prompt.push_str(&format!("\nline {n:04} of prompt {i:03}"));
            n += 1;
        }
        prompt.push_str(&format!("\nEND OF PROMPT {i:03}"));
        app.state
            .store
            .insert_request(NewRequest {
                id: &format!("{i:08}-2222-4000-8000-000000000000"),
                token: &admin,
                agent: AgentKind::Claude,
                model: None,
                message: &prompt,
            })
            .unwrap();
    }
    app.refresh();
    assert_eq!(app.recent.len(), 300);
    app.on_key(key(KeyCode::Char('2')));
    // Move into the middle so the table has to scroll.
    for _ in 0..150 {
        app.on_key(key(KeyCode::Down));
    }
    let selected = app.selected_request().unwrap().message.clone();
    let head: String = selected.chars().take(200).collect();
    let head = head.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(head.ends_with("caf") || head.ends_with("café"), "{head}");

    let (w, h) = (480, 60);
    let out = draw(&mut app, w, h);
    // The list takes a little over half the width.
    let list_w = usize::from(w) * 56 / 100;
    let list: Vec<String> = out
        .lines()
        .map(|l| l.chars().take(list_w).collect())
        .collect();
    assert!(
        list.iter().any(|l| l.contains(&head)),
        "list row should show the first 200 chars of the prompt"
    );
    assert!(
        !list.iter().any(|l| l.contains("line 0000 of prompt")),
        "the list cell is cut at 200 chars"
    );
    // The detail pane shows the whole prompt when scrolled to the end.
    app.detail_scroll = u16::MAX;
    let out = draw(&mut app, w, h);
    let tag = &selected[1..4];
    assert!(out.contains(&format!("END OF PROMPT {tag}")), "{out}");
    // Drawing again reuses the cached detail and gives the same frame.
    assert_eq!(draw(&mut app, w, h), out);
}

#[test]
fn request_list_is_brief_but_the_selection_is_whole() {
    let st = state();
    let admin = st.store.list_tokens().unwrap().remove(0);
    let long = "word ".repeat(2_000);
    st.store
        .insert_request(NewRequest {
            id: "99999999-0000-4000-8000-000000000000",
            token: &admin,
            agent: AgentKind::Claude,
            model: None,
            message: &long,
        })
        .unwrap();
    let app = App::new(st, None, false);
    assert_eq!(app.recent[0].message.chars().count(), 200);
    assert!(app.recent.iter().all(|r| r.response.is_none()));
    assert_eq!(app.selected_request().unwrap().message, long);
}

#[test]
fn dashboard_user_tokens_get_the_default_pending_cap() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('n')));
    for c in "friend".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    app.on_key(key(KeyCode::Enter));
    let t = app.tokens.iter().find(|t| t.name == "friend").unwrap();
    assert_eq!((t.role, t.limits.max_pending), (Role::User, Some(3)));
}

#[test]
fn invite_popup_turns_public_when_the_tunnel_comes_up() {
    let mut app = App::new(state(), None, false);
    share_invite(&mut app, "carol");
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("http://127.0.0.1:7878/claim/hashi_inv_"));
    assert!(out.contains("press t to make this link public"));

    let public = "https://calm-river-bridge.trycloudflare.com";
    *app.state.runtime.public_url.write().unwrap() = Some(public.to_owned());
    app.on_event(Event::TunnelReady(public.to_owned()));
    let Some(Popup::InviteReveal { code, .. }) = &app.popup else {
        panic!("expected invite reveal popup")
    };
    assert_eq!(app.invite_url(code), format!("{public}/claim/{code}"));
    let out = draw(&mut app, 140, 40);
    assert!(out.contains(&format!("{public}/claim/hashi_inv_")));
    assert!(!out.contains("press t"));
    assert!(!out.contains("only works on this machine"));
}

#[test]
fn go_public_key_reports_why_it_cannot_start() {
    // The test runtime has no listener, so nothing is ever spawned.
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('t')));
    assert!(toast(&app).contains("cannot start a tunnel"));
    assert!(draw(&mut app, 140, 40).contains("press t to go public"));
}

#[test]
fn go_public_key_defers_to_a_configured_public_url() {
    let mut st = state();
    let url = "https://hashi.example.com";
    st.runtime = Arc::new(Runtime {
        started: Instant::now(),
        listen: hashi::net::Listen::new("http://127.0.0.1:7878"),
        public_url: RwLock::new(Some(url.into())),
        agents: Vec::new(),
        name: "Hashi".into(),
        tailscale: Default::default(),
        tunnel: hashi::tunnel::Control::new(
            hashi::tunnel::Settings {
                bin: None,
                port: 7878,
                local_tls: false,
                home: std::env::temp_dir(),
                configured_url: Some(url.into()),
                named_token: None,
                configured: false,
                cancel: tokio_util::sync::CancellationToken::new(),
            },
            hashi::net::TunnelTrust::default(),
        ),
    });
    let mut app = App::new(st, None, false);
    app.on_key(key(KeyCode::Char('t')));
    assert!(
        toast(&app).contains("public URL is configured"),
        "{}",
        toast(&app)
    );
    assert!(!app.state.runtime.tunnel.trust().get());
    assert_eq!(
        app.state.runtime.tunnel.status(),
        hashi::tunnel::Status::Off
    );
    assert!(draw(&mut app, 140, 40).contains(url));
}

/// A server whose helpers are at paths that do not exist, so the Commands
/// view reads the same on every machine.
fn without_helpers() -> AppState {
    let mut st = state();
    let missing = std::env::temp_dir().join("hashi-test-no-such-helper");
    let cancel = tokio_util::sync::CancellationToken::new();
    st.runtime = Arc::new(Runtime {
        started: Instant::now(),
        listen: hashi::net::Listen::new("http://127.0.0.1:7878"),
        public_url: RwLock::new(None),
        agents: Vec::new(),
        name: "Hashi".into(),
        tunnel: hashi::tunnel::Control::new(
            hashi::tunnel::Settings {
                bin: Some(missing.clone()),
                port: 7878,
                local_tls: false,
                home: std::env::temp_dir(),
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
                home: std::env::temp_dir(),
                cancel,
            },
            hashi::net::TunnelTrust::default(),
        ),
    });
    st
}

fn select_action(app: &mut App, action: Action) {
    app.on_key(key(KeyCode::Char('5')));
    for _ in 0..Action::ALL.len() {
        app.on_key(key(KeyCode::Up));
    }
    let i = Action::ALL.iter().position(|a| *a == action).unwrap();
    for _ in 0..i {
        app.on_key(key(KeyCode::Char('j')));
    }
    assert_eq!(app.selected_action(), action);
}

#[test]
fn commands_tab_lists_every_group_with_state_and_cli() {
    let mut app = App::new(without_helpers(), None, false);
    app.on_key(key(KeyCode::Char('5')));
    assert_eq!(app.tab, Tab::Commands);
    let out = draw(&mut app, 170, 50);
    for text in [
        "5 Commands",
        "Go public",
        "Share",
        "Install",
        "Maintenance",
        "Help",
        "Cloudflare quick tunnel",
        "Tailscale serve",
        "Tailscale funnel",
        "Stop going public",
        "nothing is public",
        "hashi serve --tunnel",
        "hashi serve --tailscale serve",
        "hashi serve --tailscale funnel",
        "hashi share <label>",
        "hashi token create <name>",
        "hashi install cloudflared",
        "hashi install tailscale",
        "hashi update --check",
        "hashi --help",
        "not installed",
        "same from a terminal",
    ] {
        assert!(out.contains(text), "missing {text:?}");
    }
}

#[test]
fn commands_tab_selects_with_arrows_and_jk_and_stays_in_bounds() {
    let mut app = App::new(without_helpers(), None, false);
    app.on_key(key(KeyCode::Char('5')));
    assert_eq!(app.selected_action(), Action::Cloudflare);
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.selected_action(), Action::Cloudflare);
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.selected_action(), Action::TailscaleFunnel);
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.selected_action(), Action::TailscaleServe);
    for _ in 0..Action::ALL.len() {
        app.on_key(key(KeyCode::Down));
    }
    assert_eq!(app.selected_action(), Action::Help);
    let out = draw(&mut app, 170, 44);
    assert!(out.contains("every key the dashboard knows"));
}

#[test]
fn enter_runs_the_selected_command() {
    let mut app = App::new(without_helpers(), None, false);
    select_action(&mut app, Action::NewInvite);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::Share(_))));
    app.on_key(key(KeyCode::Esc));

    select_action(&mut app, Action::NewToken);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::NewToken(_))));
    app.on_key(key(KeyCode::Esc));

    select_action(&mut app, Action::Help);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::Help)));
    assert!(draw(&mut app, 140, 44).contains("Commands"));
    app.on_key(key(KeyCode::Esc));

    select_action(&mut app, Action::StopPublic);
    app.on_key(key(KeyCode::Enter));
    assert!(toast(&app).contains("nothing is public"));

    // A missing helper opens the install popup with its command.
    select_action(&mut app, Action::InstallTailscale);
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::Install(plan)) = &app.popup else {
        panic!("expected the install popup")
    };
    assert_eq!(plan.helper(), hashi::install::Helper::Tailscale);
    let shown = plan.text();
    let out = draw(&mut app, 170, 44);
    assert!(out.contains("install tailscale"));
    assert!(out.contains(shown.lines().next().unwrap()));
    app.on_key(key(KeyCode::Esc));
    assert!(app.popup.is_none());
    assert!(app.take_pending_command().is_none(), "esc runs nothing");

    // Starting Tailscale without it installed also offers the install.
    select_action(&mut app, Action::TailscaleFunnel);
    app.on_key(key(KeyCode::Enter));
    assert!(
        matches!(&app.popup, Some(Popup::Install(p)) if p.helper() == hashi::install::Helper::Tailscale)
    );
}

#[test]
fn go_public_without_cloudflared_offers_its_install() {
    let mut app = App::new(without_helpers(), None, false);
    app.on_key(key(KeyCode::Char('t')));
    assert!(
        matches!(&app.popup, Some(Popup::Install(p)) if p.helper() == hashi::install::Helper::Cloudflared)
    );
    assert!(!app.state.runtime.tunnel.trust().get());
}

#[test]
fn enter_in_the_install_popup_hands_the_command_to_the_terminal() {
    use hashi::install::{Helper, Platform, plan};
    let mut app = App::new(without_helpers(), None, false);
    let linux = plan(Helper::Tailscale, Platform::Linux, |_| true);
    app.popup = Some(Popup::Install(linux.clone()));
    let out = draw(&mut app, 170, 44);
    assert!(out.contains("curl -fsSL https://tailscale.com/install.sh | sh"));
    assert!(out.contains("run it"));
    app.on_key(key(KeyCode::Enter));
    assert!(app.popup.is_none());
    assert_eq!(
        app.take_pending_command(),
        Some(hashi::tui::PendingCommand::Install(linux))
    );
    assert_eq!(app.take_pending_command(), None);

    // A download page has nothing to run.
    let page = plan(Helper::Tailscale, Platform::Windows, |_| true);
    app.popup = Some(Popup::Install(page));
    assert!(draw(&mut app, 170, 44).contains("https://tailscale.com/download/windows"));
}

#[test]
fn s_and_n_still_open_their_forms() {
    let mut app = App::new(without_helpers(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('s')));
    assert!(matches!(app.popup, Some(Popup::Share(_))));
    app.on_key(key(KeyCode::Esc));
    app.on_key(key(KeyCode::Char('n')));
    assert!(matches!(app.popup, Some(Popup::NewToken(_))));
    app.on_key(key(KeyCode::Esc));
    // And from the Commands view, whose rows name them.
    app.on_key(key(KeyCode::Char('5')));
    app.on_key(key(KeyCode::Char('s')));
    assert!(matches!(app.popup, Some(Popup::Share(_))));
}

#[test]
fn an_approval_url_opens_a_popup() {
    let mut app = App::new(without_helpers(), None, false);
    let url = "https://login.tailscale.com/f/funnel?node=abc";
    app.on_event(Event::ApprovalNeeded(url.into()));
    assert!(matches!(&app.popup, Some(Popup::Approval { url: u }) if u == url));
    assert!(draw(&mut app, 140, 40).contains(url));
    app.on_key(key(KeyCode::Enter));
    assert!(app.popup.is_none());
}

#[test]
fn a_deliberate_stop_reads_as_off_not_failed() {
    let mut app = App::new(without_helpers(), None, false);
    app.on_event(Event::TunnelReady("https://a.trycloudflare.com".into()));
    app.on_event(Event::TunnelDown("the Cloudflare tunnel is stopped".into()));
    let out = draw(&mut app, 140, 40);
    assert!(out.contains("press t to go public"), "{out}");
}

fn add_project(app: &App, name: &str) {
    app.state
        .store
        .add_project(&hashi::projects::Project {
            name: name.to_owned(),
            path: "/srv/never-touched".to_owned(),
            agents: vec![AgentKind::Claude],
            accounts: hashi::accounts::AccountMap::new(),
            default_agent: None,
            models: hashi::projects::ModelMap::new(),
            note: None,
            account_strategy: None,
            rotate_at: None,
            created_at: 0,
        })
        .unwrap();
}

#[test]
fn token_and_share_forms_pick_a_project() {
    let mut app = App::new(state(), None, false);
    add_project(&app, "api");
    app.on_key(key(KeyCode::Char('3')));

    app.on_key(key(KeyCode::Char('n')));
    for c in "bot".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    // The project selector is the last field.
    app.on_key(key(KeyCode::Up));
    assert!(draw(&mut app, 140, 40).contains("none (own workspace)"));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::Reveal { .. })));
    app.on_key(key(KeyCode::Esc));
    let bot = app
        .state
        .store
        .list_tokens()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "bot")
        .unwrap();
    assert_eq!(bot.project.as_deref(), Some("api"));
    app.refresh();
    select_token(&mut app, "bot");
    assert!(draw(&mut app, 140, 40).contains("project api"));

    app.on_key(key(KeyCode::Char('s')));
    for c in "carol".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::InviteReveal { .. })));
    let invites = app.state.store.list_invites().unwrap();
    assert_eq!(invites[0].project.as_deref(), Some("api"));
}

#[test]
fn a_binding_outside_the_project_is_refused_in_the_form() {
    let mut app = App::new(state(), None, false);
    add_project(&app, "api");
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('n')));
    for c in "bot".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    // Agents: codex only, which the project does not allow.
    for _ in 0..3 {
        app.on_key(key(KeyCode::Down));
    }
    for _ in 0..AgentKind::ALL.len() + 1 {
        if draw(&mut app, 140, 40).contains("codex only") {
            break;
        }
        app.on_key(key(KeyCode::Right));
    }
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::NewToken(_))));
    assert!(draw(&mut app, 140, 40).contains("allows none"));
    assert!(
        !app.state
            .store
            .list_tokens()
            .unwrap()
            .iter()
            .any(|t| t.name == "bot")
    );
}

#[test]
fn qr_code_renders_at_a_fixed_size_with_its_finder_patterns() {
    let url = "https://calm-river-bridge.trycloudflare.com/claim/hashi_inv_0123456789abcdef";
    let lines = hashi::tui::qr::lines(url).unwrap();
    let (w, h) = hashi::tui::qr::size(url).unwrap();
    // Version 5 (37 modules) at the default error correction, plus a quiet
    // zone of two modules on each side, two modules to a cell vertically.
    assert_eq!((w, h), (41, 21));
    assert_eq!(lines.len(), usize::from(h));
    assert!(lines.iter().all(|l| l.chars().count() == usize::from(w)));
    // The first cell row is quiet zone only.
    assert!(lines[0].chars().all(|c| c == ' '));
    // The second holds module rows 0 and 1: the top of the two upper finder
    // patterns, seven dark modules over dark, five light, dark.
    let row: Vec<char> = lines[1].chars().collect();
    let finder = ['█', '▀', '▀', '▀', '▀', '▀', '█'];
    assert_eq!(row[..2], [' ', ' ']);
    assert_eq!(row[2..9], finder);
    assert_eq!(row[usize::from(w) - 9..usize::from(w) - 2], finder);
    assert_eq!(row[usize::from(w) - 2..], [' ', ' ']);
    // The same text always gives the same code.
    assert_eq!(hashi::tui::qr::lines(url).unwrap(), lines);
}

#[test]
fn invite_popup_shows_a_qr_code_when_the_terminal_has_room() {
    let mut app = App::new(state(), None, false);
    share_invite(&mut app, "alice");
    let Some(Popup::InviteReveal { code, .. }) = &app.popup else {
        panic!("expected the invite popup")
    };
    let url = app.invite_url(code);
    let qr = hashi::tui::qr::lines(&url).unwrap();
    let big = draw(&mut app, 160, 60);
    assert!(big.contains(qr[1].trim()), "{big}");
    assert!(!big.contains("larger to see the link as a QR code"));
    let small = draw(&mut app, 140, 30);
    assert!(
        small.contains("larger to see the link as a QR code"),
        "{small}"
    );
    assert!(!small.contains(qr[1].trim()));
}

// ── parity with the CLI ────────────────────────────────────────────────────

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
}

/// Like [`state`], with its store in a real Hashi home under `dir`, so
/// files (keys, reports, settings, remotes) have somewhere to go.
fn state_at(dir: &std::path::Path) -> AppState {
    let mut st = state();
    let store = hashi::server::open_store(dir).unwrap();
    hashi::server::ensure_admin(&store).unwrap();
    st.store = Arc::new(store);
    st
}

#[test]
fn every_commands_row_shows_its_cli_and_every_cli_command_has_a_place() {
    let mut app = App::new(without_helpers(), None, false);
    app.on_key(key(KeyCode::Char('5')));
    let out = draw(&mut app, 190, 50);
    for action in Action::ALL {
        let cli = ui::action_cli(&app, action);
        assert!(
            cli.contains("hashi") || cli.contains("tailscale"),
            "{action:?}: {cli}"
        );
        assert!(
            out.contains(action.title()),
            "row {action:?} is not on screen"
        );
    }
    for text in [
        "Ask",
        "Manage",
        "Server",
        "hashi account list",
        "hashi doctor",
    ] {
        assert!(out.contains(text), "missing {text:?}");
    }
    let (rows, keys, popups, excluded) = hashi::tui::parity::counts();
    assert_eq!(excluded, 2);
    assert_eq!(
        rows + keys + popups + excluded,
        hashi::tui::parity::TABLE.len()
    );
}

#[test]
fn no_two_actions_in_a_view_share_a_key() {
    let mut app = App::new(state(), None, false);
    share_invite(&mut app, "bob");
    app.on_key(key(KeyCode::Esc));
    for tab in ['1', '2', '3', '4', '5'] {
        app.on_key(key(KeyCode::Char(tab)));
        let mut keys: Vec<&str> = ui::footer_keys(&app).iter().map(|(k, _)| *k).collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n, "{:?}", app.tab);
    }
    // Commands rows with their own key use different ones.
    let mut row_keys: Vec<&str> = Action::ALL
        .iter()
        .map(|a| a.key())
        .filter(|k| *k != "enter")
        .collect();
    let n = row_keys.len();
    row_keys.sort_unstable();
    row_keys.dedup();
    assert_eq!(row_keys.len(), n);
    // The help lists the new keys.
    app.on_key(key(KeyCode::Char('?')));
    let help = draw(&mut app, 140, 44);
    for text in ["ask an agent", "edit token", "ctrl-s"] {
        assert!(help.contains(text), "help lacks {text:?}");
    }
}

#[test]
fn edit_token_form_checks_each_field_and_saves_through_the_shared_update() {
    let mut app = App::new(state(), None, false);
    add_user(&mut app, "alice");
    app.on_key(key(KeyCode::Char('3')));
    select_token(&mut app, "alice");
    let id = token_id(&app, "alice");
    app.on_key(key(KeyCode::Char('e')));
    assert!(matches!(app.popup, Some(Popup::EditToken(_))));
    assert!(draw(&mut app, 140, 44).contains("edit alice"));

    // Daily prompts: not a number.
    for _ in 0..3 {
        app.on_key(key(KeyCode::Down));
    }
    typed(&mut app, "abc");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::EditToken(_))));
    assert!(draw(&mut app, 140, 44).contains("a whole number"));
    for _ in 0..3 {
        app.on_key(key(KeyCode::Backspace));
    }
    typed(&mut app, "50");

    // Expiry: out of range, then a valid number of days.
    app.on_key(key(KeyCode::Up));
    typed(&mut app, "9999");
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("1 to 3650"));
    for _ in 0..4 {
        app.on_key(key(KeyCode::Backspace));
    }
    typed(&mut app, "7");

    // Stop at 70% of the owner's quota.
    for _ in 0..6 {
        app.on_key(key(KeyCode::Down));
    }
    typed(&mut app, "70");

    // Accounts that do not exist are refused next to the field.
    app.on_key(key(KeyCode::Down));
    typed(&mut app, "claude=ghost");
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::EditToken(form)) = &app.popup else {
        panic!("expected the edit form")
    };
    assert_eq!(form.field, hashi::tui::forms::EditTokenForm::ACCOUNTS);
    assert!(draw(&mut app, 140, 44).contains("ghost"));
    for _ in 0.."claude=ghost".len() {
        app.on_key(key(KeyCode::Backspace));
    }

    // A note, then save.
    for _ in 0..8 {
        app.on_key(key(KeyCode::Up));
    }
    typed(&mut app, "for the CI");
    app.on_key(key(KeyCode::Enter));
    assert!(app.popup.is_none(), "{:?}", app.popup);
    let t = app.state.store.get_token(&id).unwrap().unwrap();
    assert_eq!(t.note.as_deref(), Some("for the CI"));
    assert_eq!(t.limits.daily_requests, Some(50));
    assert_eq!(t.limits.stop_at_percent, Some(70));
    assert!(t.expires_at.is_some());
    assert!(toast(&app).contains("updated alice"));
    let audit = app.state.store.list_audit(10).unwrap();
    assert!(audit.iter().any(|e| e.actor == "dashboard"
        && e.action == "token.update"
        && e.detail.contains("limits")));

    // Saving with no change says so.
    app.on_key(key(KeyCode::Char('e')));
    app.on_key(key(KeyCode::Enter));
    assert!(toast(&app).contains("nothing changed"));
}

#[test]
fn the_ask_popup_queues_as_the_owner_and_the_answer_shows_in_requests() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('a')));
    let Some(Popup::Ask(_)) = &app.popup else {
        panic!("expected the ask popup")
    };
    // Nothing typed yet.
    app.on_key(ctrl('s'));
    assert!(draw(&mut app, 140, 44).contains("type a prompt first"));
    typed(&mut app, "Explain the build");
    app.on_key(key(KeyCode::Enter));
    typed(&mut app, "in two lines");
    let out = draw(&mut app, 140, 44);
    assert!(out.contains("Explain the build") && out.contains("in two lines"));
    app.on_key(ctrl('s'));
    assert!(app.popup.is_none(), "{:?}", app.popup);
    assert_eq!(app.tab, Tab::Requests);

    // It is an ordinary queued request of the admin token.
    let r = app.selected_request().unwrap().clone();
    assert_eq!(r.message, "Explain the build\nin two lines");
    assert_eq!(r.token_name, "admin");
    assert_eq!(r.agent, AgentKind::Claude);
    assert_eq!(r.status, hashi::store::RequestStatus::Queued);

    // A fake agent answers it; the detail shows the answer.
    let claimed = app
        .state
        .store
        .claim_next(AgentKind::Claude)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, r.id);
    app.state
        .store
        .complete_request(&r.id, "Fake agent says hi.", None)
        .unwrap();
    app.refresh();
    assert!(draw(&mut app, 160, 44).contains("Fake agent says hi."));
}

#[test]
fn an_admin_can_pin_the_ask_to_one_account() {
    let mut app = App::new(state(), None, false);
    app.state
        .store
        .add_account(
            AgentKind::Claude,
            "work",
            hashi::accounts::AccountKind::Login,
        )
        .unwrap();
    app.on_key(key(KeyCode::Char('a')));
    typed(&mut app, "hi");
    // The account field, then "default", then "work".
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Right));
    assert!(draw(&mut app, 140, 44).contains("work"));
    app.on_key(ctrl('s'));
    let r = app.selected_request().unwrap().clone();
    assert_eq!(
        app.state.dispatcher.pinned_account(&r.id).as_deref(),
        Some("work")
    );
}

#[test]
fn a_key_account_is_added_with_its_key_masked_and_never_rendered() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    let secret = "sk-ant-api03-VerySecretValue-1234567890";
    select_action(&mut app, Action::Accounts);
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("No accounts yet"));
    app.on_key(key(KeyCode::Char('a')));
    typed(&mut app, "work");
    // Kind: api-key.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Down));
    typed(&mut app, secret);
    let out = draw(&mut app, 160, 44);
    assert!(!out.contains(secret), "the key is on screen");
    assert!(!out.contains("VerySecret"));
    assert!(out.contains("••••"), "{out}");
    assert!(!format!("{:?}", app.popup).contains("VerySecret"));
    assert!(!format!("{app:?}").contains("VerySecret"));

    app.on_key(key(KeyCode::Enter));
    assert!(
        matches!(app.popup, Some(Popup::Accounts { .. })),
        "{:?}",
        app.popup
    );
    let a = app
        .state
        .store
        .get_account(AgentKind::Claude, "work")
        .unwrap()
        .unwrap();
    assert_eq!(a.kind, hashi::accounts::AccountKind::ApiKey);
    assert_eq!(
        hashi::accounts::read_key(dir.path(), AgentKind::Claude, "work").unwrap(),
        secret
    );
    let out = draw(&mut app, 160, 44);
    assert!(out.contains("key set") && !out.contains("VerySecret"));

    // Disable, enable, then remove, each audited as the dashboard.
    app.on_key(key(KeyCode::Char('d')));
    assert!(
        app.state
            .store
            .get_account(AgentKind::Claude, "work")
            .unwrap()
            .unwrap()
            .disabled
    );
    app.on_key(key(KeyCode::Char('e')));
    app.on_key(key(KeyCode::Char('x')));
    assert!(matches!(
        app.popup,
        Some(Popup::ConfirmRemoveAccount { .. })
    ));
    app.on_key(key(KeyCode::Char('y')));
    assert!(
        app.state
            .store
            .get_account(AgentKind::Claude, "work")
            .unwrap()
            .is_none()
    );
    let actions: Vec<String> = app
        .state
        .store
        .list_audit(10)
        .unwrap()
        .into_iter()
        .filter(|e| e.actor == "dashboard")
        .map(|e| e.action)
        .collect();
    for a in [
        "account.add",
        "account.disable",
        "account.enable",
        "account.remove",
    ] {
        assert!(
            actions.iter().any(|x| x == a),
            "{a} not audited: {actions:?}"
        );
    }
}

#[test]
fn c_in_the_accounts_popup_clears_a_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    let store = &app.state.store;
    store
        .add_account(
            AgentKind::Claude,
            "work",
            hashi::accounts::AccountKind::Login,
        )
        .unwrap();
    store
        .mark_account_limited(AgentKind::Claude, "work", hashi::util::now_ms() + 3_600_000)
        .unwrap();
    select_action(&mut app, Action::Accounts);
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 160, 44).contains("clear limit"));
    app.on_key(key(KeyCode::Char('c')));
    let a = app
        .state
        .store
        .get_account(AgentKind::Claude, "work")
        .unwrap()
        .unwrap();
    assert_eq!(a.limited_until, None);
    assert!(matches!(app.popup, Some(Popup::Accounts { .. })));
    assert!(
        app.state
            .store
            .list_audit(5)
            .unwrap()
            .iter()
            .any(|e| e.action == "account.clear-limit" && e.actor == "dashboard")
    );
}

#[test]
fn the_account_form_checks_the_name_and_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    select_action(&mut app, Action::Accounts);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('a')));
    typed(&mut app, "Bad Name");
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 160, 44).contains("must be 1 to 32"));
    for _ in 0.."Bad Name".len() {
        app.on_key(key(KeyCode::Backspace));
    }
    typed(&mut app, "team");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 160, 44).contains("paste the key"));
    assert!(
        app.state
            .store
            .get_account(AgentKind::Claude, "team")
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_login_account_steps_aside_for_its_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    select_action(&mut app, Action::Accounts);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('a')));
    typed(&mut app, "personal");
    app.on_key(key(KeyCode::Enter));
    assert!(app.popup.is_none());
    match app.take_pending_command() {
        Some(hashi::tui::PendingCommand::SignIn { agent, name, .. }) => {
            assert_eq!((agent, name.as_str()), (AgentKind::Claude, "personal"));
        }
        other => panic!("expected a sign-in, got {other:?}"),
    }
    assert!(dir.path().join("accounts/claude/personal").is_dir());
}

#[test]
fn the_project_form_validates_and_adds_edits_and_removes() {
    let dir = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    select_action(&mut app, Action::Projects);
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("No projects yet"));
    app.on_key(key(KeyCode::Char('a')));
    typed(&mut app, "api");
    app.on_key(key(KeyCode::Down));
    typed(&mut app, "relative/folder");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::Project(_))));
    assert!(draw(&mut app, 140, 44).contains("absolute"));
    for _ in 0.."relative/folder".len() {
        app.on_key(key(KeyCode::Backspace));
    }
    typed(&mut app, &folder.path().display().to_string());
    // An unknown agent.
    app.on_key(key(KeyCode::Down));
    typed(&mut app, "claude,nope");
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("nope"));
    for _ in 0..5 {
        app.on_key(key(KeyCode::Backspace));
    }
    app.on_key(key(KeyCode::Enter));
    assert!(
        matches!(app.popup, Some(Popup::Projects { .. })),
        "{:?}",
        app.popup
    );
    let p = app.state.store.get_project("api").unwrap().unwrap();
    assert_eq!(p.agents, vec![AgentKind::Claude]);

    // Edit its note.
    app.on_key(key(KeyCode::Char('e')));
    for _ in 0..5 {
        app.on_key(key(KeyCode::Down));
    }
    typed(&mut app, "the API team");
    app.on_key(key(KeyCode::Enter));
    let p = app.state.store.get_project("api").unwrap().unwrap();
    assert_eq!(p.note.as_deref(), Some("the API team"));

    app.on_key(key(KeyCode::Char('x')));
    app.on_key(key(KeyCode::Char('y')));
    assert!(app.state.store.get_project("api").unwrap().is_none());
    assert!(folder.path().is_dir(), "the folder is never touched");
}

#[test]
fn the_listen_form_validates_confirms_and_moves_a_live_listener() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = state_at(dir.path());
    let start: std::net::SocketAddr = "127.0.0.1:7878".parse().unwrap();
    let (listen, mut next) = hashi::net::Listen::switchable(start, false);
    let rt = Arc::get_mut(&mut st.runtime).unwrap();
    rt.listen = listen;
    let mut app = App::new(st, None, false);
    select_action(&mut app, Action::Listen);
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("port and bind address"));

    // Port 0 is refused.
    for _ in 0..4 {
        app.on_key(key(KeyCode::Backspace));
    }
    typed(&mut app, "0");
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("a port from 1 to 65535"));
    // Letters never get in.
    typed(&mut app, "x");
    let Some(Popup::Listen(form)) = &app.popup else {
        panic!("expected the listen form")
    };
    assert_eq!(form.port, "0");

    // A typed address that is not one.
    app.on_key(key(KeyCode::Backspace));
    typed(&mut app, "8080");
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Left));
    app.on_key(key(KeyCode::Down));
    typed(&mut app, "not-an-ip");
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("an IP address"));

    // All interfaces without TLS asks first, with the startup warning.
    app.on_key(key(KeyCode::Up));
    app.on_key(key(KeyCode::Left));
    app.on_key(key(KeyCode::Left));
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::ConfirmListen { warning, .. }) = &app.popup else {
        panic!("expected the confirmation, got {:?}", app.popup)
    };
    assert!(warning.contains("without TLS"), "{warning}");
    app.on_key(key(KeyCode::Char('n')));
    assert!(matches!(app.popup, Some(Popup::Listen(_))));

    // Back to loopback on a free port: it moves and is kept.
    app.on_key(key(KeyCode::Left));
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    app.on_key(key(KeyCode::Up));
    for _ in 0..4 {
        app.on_key(key(KeyCode::Backspace));
    }
    typed(&mut app, &port.to_string());
    app.on_key(key(KeyCode::Enter));
    assert!(app.popup.is_none(), "{:?}", app.popup);
    let moved = next.try_recv().unwrap();
    assert_eq!(moved.local_addr().unwrap().port(), port);
    assert_eq!(
        app.state.runtime.local_url(),
        format!("http://127.0.0.1:{port}")
    );
    let saved = hashi::settings::load(dir.path()).unwrap();
    assert_eq!(saved.port, Some(port));
    assert_eq!(saved.bind, Some("127.0.0.1".parse().unwrap()));
    assert!(toast(&app).contains("kept for the next start"));
}

#[test]
fn the_listen_form_says_when_a_flag_wins_and_reports_a_server_it_cannot_move() {
    let st = state();
    st.runtime.listen.set_overrides(hashi::net::Overrides {
        port: true,
        bind: false,
    });
    let mut app = App::new(st, None, false);
    select_action(&mut app, Action::Listen);
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("--port or HASHI_PORT is set"));
    for _ in 0..4 {
        app.on_key(key(KeyCode::Backspace));
    }
    typed(&mut app, "8123");
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("not attached to a running server"));
}

#[test]
fn the_connect_form_checks_the_link_before_any_network() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    select_action(&mut app, Action::Connect);
    app.on_key(key(KeyCode::Enter));
    typed(&mut app, "not a link");
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::Connect(form)) = &app.popup else {
        panic!("expected the connect form")
    };
    assert!(form.error.is_some() && !form.busy);
    for _ in 0.."not a link".len() {
        app.on_key(key(KeyCode::Backspace));
    }
    typed(
        &mut app,
        "https://hashi.example.com/claim/hashi_inv_0123456789abcdef0123456789abcdef",
    );
    app.on_key(key(KeyCode::Down));
    typed(&mut app, "Bad Name");
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::Connect(form)) = &app.popup else {
        panic!("expected the connect form")
    };
    assert_eq!(form.error.as_ref().map(|e| e.0), Some(1), "{form:?}");
    assert!(!form.busy);
}

#[test]
fn bridges_list_names_and_urls_never_tokens_and_forget_one() {
    let dir = tempfile::tempdir().unwrap();
    hashi::remote::save(
        dir.path(),
        &[hashi::remote::Remote {
            name: "friend".into(),
            base_url: "https://friend.example.com".into(),
            token: "hashi_sk_secret_never_shown".into(),
        }],
    )
    .unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    select_action(&mut app, Action::Bridges);
    app.on_key(key(KeyCode::Enter));
    let out = draw(&mut app, 140, 44);
    assert!(out.contains("friend") && out.contains("https://friend.example.com"));
    assert!(!out.contains("secret_never_shown"));
    // Ask it: the prompt popup aims at the bridge.
    app.on_key(key(KeyCode::Char('a')));
    assert!(draw(&mut app, 140, 44).contains("bridge friend"));
    app.on_key(key(KeyCode::Esc));
    select_action(&mut app, Action::Bridges);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('x')));
    app.on_key(key(KeyCode::Char('y')));
    assert!(hashi::remote::load(dir.path()).unwrap().is_empty());
}

#[test]
fn the_report_is_written_owner_only_with_prompts_off_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    select_action(&mut app, Action::Report);
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::Report(form)) = &app.popup else {
        panic!("expected the report popup")
    };
    assert!(!form.include_prompts);
    // Save in the home's reports folder, not the current one.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::ReportSaved {
        path,
        include_prompts,
    }) = &app.popup
    else {
        panic!("expected the saved popup, got {:?}", app.popup)
    };
    assert!(!include_prompts);
    let path = std::path::PathBuf::from(path);
    assert!(
        path.starts_with(std::fs::canonicalize(dir.path()).unwrap_or(dir.path().into()))
            || path.starts_with(dir.path())
    );
    assert!(path.parent().unwrap().ends_with("reports"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("left out"), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert!(draw(&mut app, 160, 44).contains("report saved"));
}

#[test]
fn doctor_audit_and_update_popups_open_from_commands() {
    let mut app = App::new(without_helpers(), None, false);
    select_action(&mut app, Action::Doctor);
    app.on_key(key(KeyCode::Enter));
    let out = draw(&mut app, 140, 44);
    for text in [
        "doctor",
        "cloudflared",
        "tailscale",
        "accounts",
        "home",
        "guard",
    ] {
        assert!(out.contains(text), "doctor lacks {text:?}");
    }
    app.on_key(key(KeyCode::Esc));

    // A change, then the audit log shows it.
    add_user(&mut app, "carol");
    app.on_key(key(KeyCode::Char('3')));
    select_token(&mut app, "carol");
    app.on_key(key(KeyCode::Char('p')));
    select_action(&mut app, Action::Audit);
    app.on_key(key(KeyCode::Enter));
    let out = draw(&mut app, 140, 44);
    assert!(
        out.contains("token.update") && out.contains("carol"),
        "{out}"
    );
    app.on_key(key(KeyCode::Esc));

    // Installing an update asks first and never runs in a test dashboard.
    select_action(&mut app, Action::InstallUpdate);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::ConfirmUpdate)));
    app.on_key(key(KeyCode::Char('y')));
    assert!(app.take_pending_command().is_none());
    assert!(toast(&app).contains("hashi update"));
}

#[test]
fn my_usage_opens_from_commands_cycles_days_and_asks_before_forgetting() {
    let mut app = App::new(without_helpers(), None, false);
    select_action(&mut app, Action::MyUsage);
    app.on_key(key(KeyCode::Enter));
    let out = draw(&mut app, 140, 44);
    assert!(
        out.contains("my usage") && out.contains("last 7 days"),
        "{out}"
    );
    assert!(
        out.contains("1/7/30 days") && out.contains("forget"),
        "{out}"
    );
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(app.popup, Some(Popup::MyUsage { days: 30, .. })));
    app.on_key(key(KeyCode::Char('f')));
    assert!(matches!(app.popup, Some(Popup::ConfirmForgetUsage)));
    app.on_key(key(KeyCode::Char('n')));
    assert!(matches!(app.popup, Some(Popup::MyUsage { .. })));
    app.on_key(key(KeyCode::Esc));
    assert!(app.popup.is_none());
}

// ── access requests ─────────────────────────────────────────────────────────

/// A pending "new person" request from `name`, through an ask-access link.
fn ask_for_access(app: &App, name: &str) -> hashi::store::AccessRequest {
    use hashi::access::{self, NewPerson, OpenLink};
    let store = &app.state.store;
    let (_, code) = access::open_link(store, "cli", OpenLink::default()).unwrap();
    access::submit_new(
        store,
        NewPerson {
            code,
            name: name.into(),
            message: Some("for the docs site".into()),
            agents: Some(vec![AgentKind::Claude]),
        },
    )
    .unwrap()
    .0
}

fn open_access_popup(app: &mut App) {
    select_action(app, Action::AccessRequests);
    app.on_key(key(KeyCode::Enter));
    assert!(
        matches!(app.popup, Some(Popup::AccessRequests { .. })),
        "{:?}",
        app.popup
    );
}

#[test]
fn the_access_popup_lists_a_pending_request() {
    let mut app = App::new(state(), None, false);
    let r = ask_for_access(&app, "alice");
    app.refresh();
    open_access_popup(&mut app);
    let out = draw(&mut app, 150, 44);
    for text in [
        "access requests",
        "alice",
        "pending",
        "for the docs site",
        "The ask-access link is open",
        "accept",
        "decline",
    ] {
        assert!(out.contains(text), "missing {text:?}\n{out}");
    }
    assert!(!out.contains(&r.id), "ids stay out of the list");
}

#[test]
fn the_commands_tab_badge_counts_pending_requests() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('5')));
    let out = draw(&mut app, 170, 44);
    assert!(!out.contains("Commands ·"), "no badge without requests");
    assert!(out.contains("hashi requests list"));
    let r = ask_for_access(&app, "alice");
    // The event the API emits refreshes the count and raises a toast.
    app.on_event(Event::AccessRequest {
        id: r.id,
        kind: r.kind,
        name: r.name,
    });
    assert!(
        toast(&app).contains("alice asks for access"),
        "{}",
        toast(&app)
    );
    app.on_tick();
    assert_eq!(app.access_pending, 1);
    let out = draw(&mut app, 170, 44);
    assert!(out.contains("Commands ·1"), "{out}");
    assert!(out.contains("1 pending"), "{out}");
    // The regular refresh picks up a request without an event too.
    ask_for_access(&app, "bob");
    app.refresh();
    assert!(draw(&mut app, 170, 44).contains("Commands ·2"));
}

#[test]
fn accepting_a_new_person_creates_their_invite() {
    let mut app = App::new(state(), None, false);
    let r = ask_for_access(&app, "alice");
    app.refresh();
    open_access_popup(&mut app);
    app.on_key(key(KeyCode::Char('a')));
    let Some(Popup::AcceptRequest(form)) = &app.popup else {
        panic!("expected the accept popup, got {:?}", app.popup)
    };
    assert_eq!(form.label, "alice");
    assert_eq!(form.agents, "claude");
    assert!(draw(&mut app, 150, 44).contains("accept request"));
    // Prompts a day: the third field.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    typed(&mut app, "25");
    app.on_key(key(KeyCode::Enter));
    assert!(
        matches!(app.popup, Some(Popup::AccessRequests { .. })),
        "{:?}",
        app.popup
    );
    assert!(toast(&app).contains("accepted alice"), "{}", toast(&app));
    let store = &app.state.store;
    let decided = store.get_access_request(&r.id).unwrap().unwrap();
    assert_eq!(decided.state(hashi::util::now_ms()), "accepted");
    let invite = store
        .list_invites()
        .unwrap()
        .into_iter()
        .find(|i| Some(&i.id) == decided.invite_id.as_ref())
        .expect("an invite for the request");
    assert_eq!(invite.label, "alice");
    assert_eq!(invite.limits.daily_requests, Some(25));
    assert_eq!(app.access_pending, 0);
}

#[test]
fn accepting_a_request_for_more_applies_the_limits() {
    let mut app = App::new(state(), None, false);
    add_user(&mut app, "carol");
    let token = app
        .state
        .store
        .list_tokens()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "carol")
        .unwrap();
    let r = hashi::access::submit_more(
        &app.state.store,
        &token,
        hashi::access::MoreInput {
            daily_requests: Some(400),
            ..Default::default()
        },
    )
    .unwrap();
    app.refresh();
    open_access_popup(&mut app);
    assert!(draw(&mut app, 150, 44).contains("carol"));
    app.on_key(key(KeyCode::Char('a')));
    let out = draw(&mut app, 150, 44);
    assert!(out.contains("More for") && out.contains("now:"), "{out}");
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::AccessRequests { .. })));
    let saved = app.state.store.get_token(&token.id).unwrap().unwrap();
    assert_eq!(saved.limits.daily_requests, Some(400));
    let decided = app.state.store.get_access_request(&r.id).unwrap().unwrap();
    assert_eq!(decided.state(hashi::util::now_ms()), "accepted");
}

#[test]
fn declining_stores_the_reason_and_show_all_lists_it() {
    let mut app = App::new(state(), None, false);
    let r = ask_for_access(&app, "bob");
    app.refresh();
    open_access_popup(&mut app);
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(app.popup, Some(Popup::DeclineRequest(_))));
    typed(&mut app, "we are full");
    assert!(draw(&mut app, 150, 44).contains("we are full"));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(
        app.popup,
        Some(Popup::AccessRequests { all: false, .. })
    ));
    let decided = app.state.store.get_access_request(&r.id).unwrap().unwrap();
    assert_eq!(decided.reason.as_deref(), Some("we are full"));
    assert_eq!(decided.state(hashi::util::now_ms()), "declined");
    // Pending only: the list is empty; show all brings it back.
    assert!(draw(&mut app, 150, 44).contains("No requests"));
    app.on_key(key(KeyCode::Char('s')));
    assert!(matches!(
        app.popup,
        Some(Popup::AccessRequests { all: true, .. })
    ));
    let out = draw(&mut app, 150, 44);
    assert!(out.contains("declined") && out.contains("bob"), "{out}");
    // A decided request cannot be decided again.
    app.on_key(key(KeyCode::Char('a')));
    assert!(toast(&app).contains("not pending"), "{}", toast(&app));
}

#[test]
fn the_access_popup_opens_and_closes_the_link() {
    let mut app = App::new(state(), None, false);
    open_access_popup(&mut app);
    assert!(draw(&mut app, 150, 44).contains("link is closed"));
    app.on_key(key(KeyCode::Char('o')));
    let Some(Popup::AccessLink { code, .. }) = &app.popup else {
        panic!("expected the link popup, got {:?}", app.popup)
    };
    let url = app.access_link_url(code);
    assert!(
        url.starts_with("http://127.0.0.1:7878/ask-access/hashi_ask_"),
        "{url}"
    );
    let out = draw(&mut app, 150, 60);
    assert!(out.contains(&url), "{out}");
    assert!(out.contains("only works on this machine"), "{out}");
    assert!(out.contains('█'), "a QR code when there is room");
    let link = hashi::access::current_link(&app.state.store)
        .unwrap()
        .unwrap();
    assert_eq!(link.status(hashi::util::now_ms()), "open");
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 150, 44).contains("link is open"));
    app.on_key(key(KeyCode::Char('x')));
    assert!(toast(&app).contains("closed the ask-access link"));
    let link = hashi::access::current_link(&app.state.store)
        .unwrap()
        .unwrap();
    assert_eq!(link.status(hashi::util::now_ms()), "closed");
    app.on_key(key(KeyCode::Char('x')));
    assert!(toast(&app).contains("no ask-access link was open"));
}

#[test]
fn bridges_show_the_requests_this_machine_sent_and_check_them() {
    let dir = tempfile::tempdir().unwrap();
    // A bridge that is not listening: the check reports it and keeps the
    // request for the next try.
    hashi::access_client::save(
        dir.path(),
        &[hashi::access_client::PendingRequest {
            base_url: "http://127.0.0.1:1".into(),
            id: "1".into(),
            pickup: "hashi_pick_never_shown".into(),
            name: "dana".into(),
            server: "friend-bridge".into(),
            remote_name: None,
        }],
    )
    .unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    select_action(&mut app, Action::Bridges);
    app.on_key(key(KeyCode::Enter));
    let out = draw(&mut app, 140, 44);
    for text in [
        "Requests for access you sent",
        "friend-bridge",
        "asked as dana",
        "request status",
    ] {
        assert!(out.contains(text), "missing {text:?}\n{out}");
    }
    assert!(
        !out.contains("never_shown"),
        "the pickup secret stays hidden"
    );
    app.on_key(key(KeyCode::Char('r')));
    assert!(app.request_checking);
    assert!(app.animating(), "the check shows a spinner");
    let started = Instant::now();
    while app.request_checking && started.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.on_tick();
    }
    assert!(!app.request_checking, "the check never finished");
    let checks = app.request_checks.clone().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].state, "error");
    let out = draw(&mut app, 140, 44);
    assert!(out.contains("friend-bridge: error"), "{out}");
    assert_eq!(
        hashi::access_client::load(dir.path()).unwrap().len(),
        1,
        "kept for the next check"
    );
}

#[test]
fn the_project_form_sets_the_account_strategy_and_rotate_threshold() {
    use hashi::tui::forms::ProjectForm;
    use hashi::usage::Strategy;
    let dir = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    select_action(&mut app, Action::Projects);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('a')));
    typed(&mut app, "api");
    app.on_key(key(KeyCode::Down));
    typed(&mut app, &folder.path().display().to_string());
    for _ in 1..ProjectForm::STRATEGY {
        app.on_key(key(KeyCode::Down));
    }
    let out = draw(&mut app, 140, 44);
    assert!(out.contains("account strategy") && out.contains("server default"));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Down));
    typed(&mut app, "30");
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::Project(form)) = &app.popup else {
        panic!("expected the form, got {:?}", app.popup)
    };
    assert_eq!(form.field, ProjectForm::ROTATE_AT);
    assert_eq!(
        form.error.as_ref().map(|e| e.0),
        Some(ProjectForm::ROTATE_AT)
    );
    let out = draw(&mut app, 140, 44);
    assert!(out.contains("a percent from 50 to 99"), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let field = lines
        .iter()
        .position(|l| l.contains("rotate at %"))
        .unwrap();
    assert!(
        lines[field + 1].contains("a percent from 50 to 99"),
        "the error sits under its field"
    );
    assert!(app.state.store.get_project("api").unwrap().is_none());
    app.on_key(key(KeyCode::Backspace));
    app.on_key(key(KeyCode::Backspace));
    typed(&mut app, "80");
    app.on_key(key(KeyCode::Enter));
    assert!(
        matches!(app.popup, Some(Popup::Projects { .. })),
        "{:?}",
        app.popup
    );
    let p = app.state.store.get_project("api").unwrap().unwrap();
    assert_eq!(p.account_strategy, Some(Strategy::InOrder));
    assert_eq!(p.rotate_at, Some(80));

    // Edit: soonest-reset and back to the server's threshold.
    app.on_key(key(KeyCode::Char('e')));
    let Some(Popup::Project(form)) = &app.popup else {
        panic!("expected the form")
    };
    assert_eq!(form.strategy, Some(Strategy::InOrder));
    assert_eq!(form.rotate_at, "80");
    for _ in 1..ProjectForm::STRATEGY {
        app.on_key(key(KeyCode::Down));
    }
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Backspace));
    app.on_key(key(KeyCode::Backspace));
    app.on_key(key(KeyCode::Enter));
    assert!(toast(&app).contains("soonest-reset"), "{}", toast(&app));
    let p = app.state.store.get_project("api").unwrap().unwrap();
    assert_eq!(p.account_strategy, Some(Strategy::SoonestReset));
    assert_eq!(p.rotate_at, None);
    // Left from in-order goes back to the server default.
    app.on_key(key(KeyCode::Char('e')));
    for _ in 1..ProjectForm::STRATEGY {
        app.on_key(key(KeyCode::Down));
    }
    for _ in 0..3 {
        app.on_key(key(KeyCode::Left));
    }
    app.on_key(key(KeyCode::Enter));
    let p = app.state.store.get_project("api").unwrap().unwrap();
    assert_eq!(p.account_strategy, None);
}

#[test]
fn views_pick_up_store_changes_on_the_next_refresh_or_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    select_action(&mut app, Action::Accounts);
    assert!(draw(&mut app, 190, 44).contains("only the default logins"));
    // Added behind the dashboard's back: frames reuse what they read until
    // the next refresh.
    app.state
        .store
        .add_account(
            AgentKind::Claude,
            "work",
            hashi::accounts::AccountKind::Login,
        )
        .unwrap();
    assert!(draw(&mut app, 190, 44).contains("only the default logins"));
    app.refresh();
    assert!(draw(&mut app, 190, 44).contains("1 account"));
    // A key press reads afresh too, so the dashboard's own changes show at
    // once.
    app.state
        .store
        .add_account(
            AgentKind::Codex,
            "team",
            hashi::accounts::AccountKind::Login,
        )
        .unwrap();
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('k')));
    assert!(draw(&mut app, 190, 44).contains("2 accounts"));
}

#[test]
fn a_long_token_list_draws_and_reads_only_the_rows_on_screen() {
    let st = state();
    for i in 0..80 {
        st.store
            .create_token(&NewToken {
                name: format!("user-{i:02}"),
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
    }
    let mut app = App::new(st, None, false);
    app.on_key(key(KeyCode::Char('3')));
    let first = app.tokens[0].clone();
    let last = app.tokens[80].clone();
    let out = draw(&mut app, 160, 40);
    assert!(out.contains(&first.name), "{out}");
    assert!(!out.contains(&last.name));
    assert!(app.usage.len() < 40, "read {} tokens' use", app.usage.len());
    assert!(!app.usage.contains_key(&last.id));
    // Down to the last row: the table scrolls with the selection and reads
    // the use of the rows that came into view.
    for _ in 0..80 {
        app.on_key(key(KeyCode::Char('j')));
    }
    let out = draw(&mut app, 160, 40);
    assert!(out.contains(&format!("▌{} ", last.name)), "{out}");
    assert!(!out.contains(&first.name));
    let now = hashi::util::now_ms();
    assert_eq!(
        app.usage[&last.id],
        app.state.store.token_usage(&last.id, now).unwrap()
    );
}

/// The token named `name`, read from the store.
fn stored_token(app: &App, name: &str) -> hashi::token::Token {
    app.state
        .store
        .list_tokens()
        .unwrap()
        .into_iter()
        .find(|t| t.name == name)
        .unwrap()
}

#[test]
fn the_token_form_can_share_usage() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('n')));
    typed(&mut app, "quiet");
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Esc));
    assert!(!stored_token(&app, "quiet").share_usage);

    app.on_key(key(KeyCode::Char('n')));
    typed(&mut app, "open");
    for _ in 0..TokenForm::SHARE_USAGE {
        app.on_key(key(KeyCode::Down));
    }
    assert!(draw(&mut app, 140, 40).contains("not shared"));
    app.on_key(key(KeyCode::Right));
    assert!(draw(&mut app, 140, 40).contains("shared: headroom, reset and state"));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::Reveal { .. })));
    assert!(stored_token(&app, "open").share_usage);
}

#[test]
fn the_share_form_can_share_usage() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('s')));
    typed(&mut app, "dave");
    for _ in 0..ShareForm::SHARE_USAGE {
        app.on_key(key(KeyCode::Down));
    }
    app.on_key(key(KeyCode::Char(' ')));
    assert!(draw(&mut app, 140, 40).contains("shared: headroom, reset and state"));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::InviteReveal { .. })));
    let invite = app.state.store.list_invites().unwrap().remove(0);
    assert_eq!(invite.label, "dave");
    assert!(invite.share_usage);

    // Left as it is, the toggle shares nothing.
    app.on_key(key(KeyCode::Esc));
    share_invite(&mut app, "erin");
    let erin = app
        .state
        .store
        .list_invites()
        .unwrap()
        .into_iter()
        .find(|i| i.label == "erin")
        .unwrap();
    assert!(!erin.share_usage);
}

#[test]
fn the_edit_form_turns_usage_sharing_on_and_off() {
    let mut app = App::new(state(), None, false);
    add_user(&mut app, "bob");
    app.on_key(key(KeyCode::Char('3')));
    select_token(&mut app, "bob");
    for (on, change) in [(true, "usage shared"), (false, "usage not shared")] {
        app.on_key(key(KeyCode::Char('e')));
        for _ in 0..EditTokenForm::SHARE_USAGE {
            app.on_key(key(KeyCode::Down));
        }
        app.on_key(key(KeyCode::Right));
        app.on_key(key(KeyCode::Enter));
        assert!(app.popup.is_none(), "{:?}", app.popup);
        assert!(toast(&app).contains(change), "{}", toast(&app));
        assert_eq!(stored_token(&app, "bob").share_usage, on);
        select_token(&mut app, "bob");
    }
    // Opened and saved untouched, nothing changes.
    app.on_key(key(KeyCode::Char('e')));
    app.on_key(key(KeyCode::Enter));
    assert!(toast(&app).contains("nothing changed"));
}

/// Opens the add-account form, types `name` and picks the remote kind.
fn open_remote_account_form(app: &mut App, name: &str) {
    select_action(app, Action::Accounts);
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('a')));
    typed(app, name);
    // Kind: remote is the last kind, left of login.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Left));
    let Some(Popup::AddAccount(form)) = &app.popup else {
        panic!("expected the add-account form, got {:?}", app.popup)
    };
    assert_eq!(form.kind, hashi::accounts::AccountKind::Remote);
}

#[test]
fn the_account_form_adds_a_remote_account_through_a_saved_bridge() {
    let dir = tempfile::tempdir().unwrap();
    hashi::remote::save(
        dir.path(),
        &[
            hashi::remote::Remote {
                name: "friend".into(),
                // Nothing listens here, so the loop check cannot run.
                base_url: "http://127.0.0.1:9".into(),
                token: "hashi_sk_secret_never_shown".into(),
            },
            hashi::remote::Remote {
                name: "other".into(),
                base_url: "http://127.0.0.1:9".into(),
                token: "hashi_sk_other".into(),
            },
        ],
    )
    .unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    open_remote_account_form(&mut app, "pal");
    let out = draw(&mut app, 160, 44);
    assert!(
        out.contains("friend") && out.contains("same (claude)"),
        "{out}"
    );
    assert!(out.contains("privacy: the friend who runs that bridge can see every"));
    assert!(
        out.contains("never see that this account is remote."),
        "{out}"
    );
    assert!(!out.contains("secret_never_shown"));

    // The second bridge, and codex on it.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Down));
    for _ in 0..=AgentKind::ALL.len() {
        if draw(&mut app, 160, 44).contains("‹ codex ›") {
            break;
        }
        app.on_key(key(KeyCode::Right));
    }
    assert!(draw(&mut app, 160, 44).contains("other"));
    app.on_key(key(KeyCode::Enter));
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    while matches!(app.popup, Some(Popup::AddAccount(_))) {
        assert!(Instant::now() < deadline, "the account was never added");
        app.on_tick();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        matches!(app.popup, Some(Popup::Accounts { .. })),
        "{:?}",
        app.popup
    );
    // Unreachable, so added with the CLI's warning.
    assert!(toast(&app).contains("could not reach"), "{}", toast(&app));
    let a = app
        .state
        .store
        .get_account(AgentKind::Claude, "pal")
        .unwrap()
        .unwrap();
    assert_eq!(a.kind, hashi::accounts::AccountKind::Remote);
    assert_eq!(a.remote.as_deref(), Some("other"));
    assert_eq!(a.remote_agent, Some(AgentKind::Codex));
    let audited = app
        .state
        .store
        .list_audit(10)
        .unwrap()
        .into_iter()
        .any(|e| e.actor == "dashboard" && e.action == "account.add" && e.target == "claude:pal");
    assert!(audited);
    let out = draw(&mut app, 160, 44);
    assert!(out.contains("pal") && out.contains("remote"), "{out}");
}

#[test]
fn a_remote_account_needs_a_saved_bridge_and_a_good_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    open_remote_account_form(&mut app, "Bad Name");
    assert!(draw(&mut app, 160, 44).contains("none saved (connect to a bridge first)"));
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 160, 44).contains("must be 1 to 32"));
    let Some(Popup::AddAccount(form)) = &mut app.popup else {
        panic!("expected the add-account form")
    };
    form.name = "pal".into();
    app.on_key(key(KeyCode::Enter));
    let out = draw(&mut app, 160, 44);
    assert!(out.contains("no bridges saved"), "{out}");
    assert!(
        app.state
            .store
            .get_account(AgentKind::Claude, "pal")
            .unwrap()
            .is_none()
    );
}

// ── hostile text ───────────────────────────────────────────────────────────

/// Every cell symbol that would send a control, bidi or other format
/// character to the terminal.
fn invisible_cells(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    buf.content
        .iter()
        .map(|c| c.symbol().to_owned())
        .filter(|s| {
            s.chars().any(|c| {
                c.is_control()
                    || hashi::diagnostics::is_bidi_control(c)
                    || hashi::diagnostics::is_format_char(c)
            })
        })
        .collect()
}

#[test]
fn bidi_and_zero_width_characters_never_reach_the_terminal() {
    const EVIL: &str = "x \u{202e}cod.exe\u{2066}\u{200b}\u{200d}\u{feff}\u{200f} ok";
    let st = state();
    let (tok, _) = st
        .store
        .create_token(&NewToken {
            name: format!("bob{EVIL}"),
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
            message: &format!("hi {EVIL}"),
        })
        .unwrap();
    let mut app = App::new(st, None, false);
    let edit = app
        .tokens
        .iter()
        .find(|t| t.name.starts_with("bob"))
        .map(|t| Popup::EditToken(Box::new(EditTokenForm::new(t, Vec::new()))))
        .unwrap();
    let popups = [
        Popup::Reveal {
            title: EVIL.into(),
            secret: "s".into(),
        },
        Popup::InviteReveal {
            label: EVIL.into(),
            code: "abc".into(),
            link_hours: 1,
            via: hashi::links::Via::Auto,
            pool: false,
        },
        Popup::RemoteAnswer {
            remote: EVIL.into(),
            answer: Some(Ok(EVIL.into())),
            scroll: 0,
        },
        edit,
    ];
    let check = |app: &mut App, what: &str| {
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        term.draw(|f| ui::render(app, f)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("cod.exe"), "{what}: the text is still shown");
        let bad = invisible_cells(term.backend().buffer());
        assert!(
            bad.is_empty(),
            "{what}: invisible characters drawn: {bad:?}"
        );
    };
    for tab in ['2', '3'] {
        app.on_key(key(KeyCode::Char(tab)));
        check(&mut app, &format!("tab {tab}"));
    }
    for popup in popups {
        let what = format!("{popup:?}");
        app.popup = Some(popup);
        check(&mut app, &what);
    }
}

#[test]
fn scrub_removes_format_characters_from_any_widget() {
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 10, 1));
    buf.set_string(0, 0, "a\u{202e}b\u{200b}c", ratatui::style::Style::new());
    ui::scrub(&mut buf);
    assert!(invisible_cells(&buf).is_empty());
    let text: String = buf.content.iter().map(|c| c.symbol()).collect();
    assert!(text.starts_with("abc"), "{text:?}");
}

/// `u` in the Accounts popup reads the selected account's usage now, or says
/// why it cannot; either way the owner gets an answer instead of silence.
#[test]
fn u_in_the_accounts_popup_asks_for_a_usage_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    app.state
        .store
        .add_account(
            AgentKind::Claude,
            "work",
            hashi::accounts::AccountKind::Login,
        )
        .unwrap();
    select_action(&mut app, Action::Accounts);
    app.on_key(key(KeyCode::Enter));
    let frame = draw(&mut app, 160, 44);
    assert!(frame.contains("read usage"), "{frame}");
    assert!(frame.contains("press u to read them now"), "{frame}");
    app.on_key(key(KeyCode::Char('u')));
    assert!(matches!(app.popup, Some(Popup::Accounts { .. })));
    let frame = draw(&mut app, 160, 44);
    assert!(
        frame.contains("reading usage for claude:work")
            || frame.contains("usage reads are off")
            || frame.contains("usage reads need the running server"),
        "{frame}"
    );
}

#[test]
fn the_copy_guide_row_copies_the_llm_guide_and_says_how_many_lines() {
    let clipboard = hashi::tui::clipboard::capture();
    let mut app = App::new(without_helpers(), None, false);
    select_action(&mut app, Action::CopyGuide);
    assert_eq!(Action::CopyGuide.group(), "Help");
    assert_eq!(ui::action_cli(&app, Action::CopyGuide), "hashi llm");
    assert!(draw(&mut app, 190, 44).contains("Copy guide for an LLM"));
    app.on_key(key(KeyCode::Enter));
    let guide = hashi::llm::guide();
    assert_eq!(clipboard.copied(), std::slice::from_ref(&guide));
    let lines = guide.lines().count();
    assert!(lines > 100, "{lines}");
    assert_eq!(toast(&app), format!("LLM guide copied ({lines} lines)"));
}

#[test]
fn a_login_is_shared_from_the_dashboard_with_reminders_and_no_key_on_screen() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(state_at(dir.path()), None, false);
    let secret = "sk-ant-oat01-DashboardShare-123456";

    // Without a key account, the form says what to add.
    select_action(&mut app, Action::ShareLogin);
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::ShareLogin(_))));
    typed(&mut app, "dana");
    app.on_key(key(KeyCode::Enter));
    let out = draw(&mut app, 140, 44);
    assert!(out.contains("add a key account first"), "{out}");
    app.on_key(key(KeyCode::Esc));

    let home = app.home().unwrap();
    hashi::accounts::ops::create(
        &app.state.store,
        &home,
        AgentKind::Claude,
        "bot",
        hashi::accounts::AccountKind::OauthToken,
        Some(secret),
        "test",
    )
    .unwrap();
    select_action(&mut app, Action::ShareLogin);
    app.on_key(key(KeyCode::Enter));
    typed(&mut app, "dana");
    let out = draw(&mut app, 140, 44);
    assert!(out.contains("claude:bot (oauth-token)"), "{out}");
    assert!(out.contains("70% of your quota"), "{out}");
    assert!(out.contains("7 days"), "{out}");
    assert!(!out.contains("DashboardShare"));
    app.on_key(key(KeyCode::Enter));
    assert!(
        matches!(app.popup, Some(Popup::InviteReveal { .. })),
        "{:?}",
        app.popup
    );
    assert!(!draw(&mut app, 140, 44).contains("DashboardShare"));
    let invite = app
        .state
        .store
        .list_invites()
        .unwrap()
        .into_iter()
        .find(|i| i.label == "dana")
        .unwrap();
    assert_eq!(invite.login_account.as_deref(), Some("claude:bot"));
    assert_eq!(invite.remind_at, Some(70));
    assert_eq!(invite.token_expires_in_days, Some(7));
    assert!(invite.share_usage);
}

#[test]
fn the_share_form_sets_a_stop_on_the_owners_quota() {
    let mut app = App::new(state(), None, false);
    app.on_key(key(KeyCode::Char('3')));
    app.on_key(key(KeyCode::Char('s')));
    assert!(matches!(app.popup, Some(Popup::Share(_))));
    typed(&mut app, "carol");
    for _ in 0..hashi::tui::app::ShareForm::STOP_AT {
        app.on_key(key(KeyCode::Down));
    }
    app.on_key(key(KeyCode::Right));
    app.on_key(key(KeyCode::Right));
    assert!(draw(&mut app, 140, 44).contains("at 70% of your quota"));
    app.on_key(key(KeyCode::Enter));
    let invite = app
        .state
        .store
        .list_invites()
        .unwrap()
        .into_iter()
        .find(|i| i.label == "carol")
        .unwrap();
    assert_eq!(invite.limits.stop_at_percent, Some(70));
}

#[test]
fn a_pool_link_is_opened_from_the_dashboard_and_members_show_up() {
    let mut app = App::new(state(), None, false);
    select_action(&mut app, Action::Pool);
    app.on_key(key(KeyCode::Enter));
    assert!(draw(&mut app, 140, 44).contains("Nobody is in your pool yet"));
    app.on_key(key(KeyCode::Char('o')));
    // Link via: this machine only, so nothing has to start.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    for _ in 0..4 {
        app.on_key(key(KeyCode::Right));
    }
    assert!(draw(&mut app, 140, 44).contains("this machine only"));
    app.on_key(key(KeyCode::Enter));
    let Some(Popup::InviteReveal { code, pool, .. }) = &app.popup else {
        panic!("expected the link, got {:?}", app.popup)
    };
    assert!(*pool && code.starts_with("hashi_pool_"));
    let out = draw(&mut app, 160, 44);
    assert!(out.contains("/pool/hashi_pool_"), "{out}");
    let link = app.state.store.pool_link().unwrap().unwrap();
    app.state
        .store
        .add_pool_member(&link, "sid", &[AgentKind::Claude], None)
        .unwrap();
    app.on_key(key(KeyCode::Esc));
    select_action(&mut app, Action::Pool);
    app.on_key(key(KeyCode::Enter));
    let out = draw(&mut app, 140, 44);
    assert!(out.contains("sid") && out.contains("offline"), "{out}");
    app.on_key(key(KeyCode::Char('x')));
    assert!(app.state.store.list_pool_members().unwrap().is_empty());
}

#[test]
fn d_explains_the_selected_command_at_length() {
    let mut app = App::new(without_helpers(), None, false);
    select_action(&mut app, Action::NewInvite);
    app.on_key(key(KeyCode::Char('d')));
    assert!(matches!(
        app.popup,
        Some(Popup::Details {
            action: Action::NewInvite,
            scroll: 0
        })
    ));
    let out = draw(&mut app, 140, 60);
    for text in [
        "New invite",
        "What it is",
        "How it works",
        "Good to know",
        "one-time link",
        "Stop at",
        "hashi share <label>",
    ] {
        assert!(out.contains(text), "missing {text:?}:\n{out}");
    }

    // → and ← step through the rows, and the Commands selection follows.
    app.on_key(key(KeyCode::Right));
    assert!(matches!(
        app.popup,
        Some(Popup::Details {
            action: Action::ShareLogin,
            ..
        })
    ));
    assert_eq!(app.selected_action(), Action::ShareLogin);
    assert!(draw(&mut app, 140, 60).contains("someone you trust"));
    app.on_key(key(KeyCode::Left));
    app.on_key(key(KeyCode::Left));
    assert_eq!(app.selected_action(), Action::StopPublic);

    // enter runs it; esc closes.
    select_action(&mut app, Action::Help);
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Enter));
    assert!(matches!(app.popup, Some(Popup::Help)));
    app.on_key(key(KeyCode::Esc));
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Esc));
    assert!(app.popup.is_none());
}

#[test]
fn details_scroll_stops_at_the_end() {
    let mut app = App::new(without_helpers(), None, false);
    select_action(&mut app, Action::Pool);
    app.on_key(key(KeyCode::Char('d')));
    let first = draw(&mut app, 80, 24);
    assert!(first.contains("↓ more"), "{first}");
    for _ in 0..200 {
        app.on_key(key(KeyCode::Down));
    }
    let Some(Popup::Details { scroll, .. }) = app.popup else {
        panic!("closed")
    };
    let last = draw(&mut app, 80, 24);
    assert!(!last.contains("↓ more"), "{last}");
    assert!(last.contains("hashi pool status"), "{last}");
    // One up from the end moves at once.
    app.on_key(key(KeyCode::Up));
    let Some(Popup::Details { scroll: up, .. }) = app.popup else {
        panic!("closed")
    };
    assert_eq!(up + 1, scroll);
}

#[test]
fn every_command_has_details_without_banned_dashes() {
    for action in Action::ALL {
        let d = hashi::tui::details::details(action);
        assert!(!d.about.is_empty() && !d.how.is_empty() && !d.know.is_empty());
        for text in std::iter::once(d.about)
            .chain(d.how.iter().copied())
            .chain(d.know.iter().copied())
        {
            assert!(
                !text.contains(['\u{2014}', '\u{2015}', '\u{2212}', '\u{2012}']),
                "{action:?}: {text}"
            );
        }
    }
}
