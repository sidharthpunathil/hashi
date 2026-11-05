//! Dashboard cost on a large store, measured off-screen on a `TestBackend`.
//!
//! Ignored by default because it takes a while to fill the store. Run it
//! optimised and with output:
//!
//! ```text
//! cargo test --release --test tui_bench -- --ignored --nocapture
//! ```
//!
//! The store holds 200 tokens, 20,000 requests with large prompts and
//! responses, 30 accounts with usage figures and 10 projects. It prints the
//! frame time for each view and popup, the cost of `refresh()`, of applying
//! a burst of 10,000 bus events, and of an idle tick.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

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
use hashi::tui::app::{Action, App, Tab};
use hashi::tui::ui;
use hashi::usage::{Source, Window};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const TOKENS: usize = 200;
const REQUESTS: usize = 20_000;
const ACCOUNTS: usize = 30;
const PROJECTS: usize = 10;
const PROMPT_BYTES: usize = 6 * 1024;
const RESPONSE_BYTES: usize = 12 * 1024;

fn filled_state(dir: &std::path::Path) -> AppState {
    let store = Arc::new(Store::open(&dir.join("hashi.db")).unwrap());
    let mut tokens = Vec::new();
    for i in 0..TOKENS {
        let (tk, _) = store
            .create_token(&NewToken {
                name: format!("token-{i:03}"),
                role: if i == 0 { Role::Admin } else { Role::User },
                session_mode: SessionMode::Stateless,
                agents: AgentKind::ALL.to_vec(),
                expires_in_days: None,
                note: None,
                limits: Default::default(),
                accounts: Default::default(),
                project: None,
            })
            .unwrap();
        tokens.push(tk);
    }
    let prompt: String = "Refactor the request handler so it streams. "
        .repeat(PROMPT_BYTES / 44 + 1)
        .chars()
        .take(PROMPT_BYTES)
        .collect();
    let response: String = "Here is the change, line by line.\n"
        .repeat(RESPONSE_BYTES / 34 + 1)
        .chars()
        .take(RESPONSE_BYTES)
        .collect();
    let agents = [AgentKind::Claude, AgentKind::Codex];
    for i in 0..REQUESTS {
        let id = format!("{i:08}-0000-4000-8000-000000000000");
        let agent = agents[i % agents.len()];
        store
            .insert_request(NewRequest {
                id: &id,
                token: &tokens[i % TOKENS],
                agent,
                model: None,
                message: &prompt,
            })
            .unwrap();
        store.claim_next(agent).unwrap();
        store
            .complete_request(
                &id,
                &response,
                Some(Usage {
                    input_tokens: 1200,
                    output_tokens: 340,
                    cost_usd: Some(0.012),
                }),
            )
            .unwrap();
    }
    let now = hashi::util::now_ms();
    for i in 0..ACCOUNTS {
        let agent = agents[i % agents.len()];
        let name = format!("acct-{i:02}");
        store.add_account(agent, &name, AccountKind::Login).unwrap();
        let windows = [("five_hour", "5 hours", 300), ("seven_day", "week", 10_080)].map(
            |(key, label, mins)| Window {
                key: key.to_owned(),
                label: label.to_owned(),
                used_percent: (i * 3) as f64 % 100.0,
                resets_at: Some(now + 3_600_000),
                window_mins: Some(mins),
                source: Source::Poll,
                observed_at: Some(now),
            },
        );
        store.replace_usage(agent, &name, &windows).unwrap();
        store
            .set_poll_status(agent, &name, true, Some("max"), None)
            .unwrap();
    }
    for i in 0..PROJECTS {
        let path = dir.join(format!("project-{i}"));
        std::fs::create_dir_all(&path).unwrap();
        store
            .add_project(&Project {
                name: format!("project-{i}"),
                path: path.display().to_string(),
                agents: AgentKind::ALL.to_vec(),
                accounts: Default::default(),
                default_agent: None,
                models: Default::default(),
                note: None,
                account_strategy: None,
                rotate_at: None,
                created_at: now,
            })
            .unwrap();
    }
    AppState {
        store,
        bus: Bus::new(16_384),
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
                    available: true,
                    version: Some("0.50.0".into()),
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
            dir.to_path_buf(),
            Bus::default(),
        )),
        waits: Default::default(),
    }
}

/// Mean and median of `n` runs of `f`, in microseconds.
fn time(n: usize, mut f: impl FnMut()) -> (f64, f64) {
    f();
    let mut v: Vec<Duration> = (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    v.sort();
    let mean = v.iter().sum::<Duration>().as_secs_f64() * 1e6 / n as f64;
    let median = v[n / 2].as_secs_f64() * 1e6;
    (mean, median)
}

fn report(label: &str, (mean, median): (f64, f64)) {
    println!("{label:<34} mean {mean:>11.1} us   median {median:>11.1} us");
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn burst(i: usize) -> Event {
    let id = format!("{i:08}-1111-4000-8000-000000000000");
    match i % 4 {
        0 => Event::Queued {
            id,
            agent: AgentKind::Claude,
            token: "token-001".into(),
        },
        1 => Event::Started {
            id,
            agent: AgentKind::Claude,
        },
        2 => Event::Finished {
            id,
            agent: AgentKind::Claude,
            ok: i % 8 == 2,
            duration_ms: 1234,
            error: Some("the agent exited with status 1".into()),
        },
        _ => Event::Log(format!("log line {i}")),
    }
}

#[test]
#[ignore = "benchmark: cargo test --release --test tui_bench -- --ignored --nocapture"]
fn dashboard_costs() {
    let dir = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let state = filled_state(dir.path());
    println!("filled the store in {:.1}s", t.elapsed().as_secs_f64());

    let mut app = App::new(state, None, false);
    for i in 0..600 {
        app.on_event(Event::Log(format!("startup line {i}")));
    }
    app.refresh();
    let mut term = Terminal::new(TestBackend::new(200, 50)).unwrap();
    let frames = 200;

    // Each view: a frame between refreshes, and a refresh with the frame
    // after it, which is what every refresh cycle pays.
    let mut views: Vec<(String, Tab, Option<Action>)> = Tab::ALL
        .into_iter()
        .map(|t| (t.title().to_owned(), t, None))
        .collect();
    views.push((
        "Accounts popup".into(),
        Tab::Commands,
        Some(Action::Accounts),
    ));
    views.push((
        "Projects popup".into(),
        Tab::Commands,
        Some(Action::Projects),
    ));
    for (label, tab, action) in views {
        app.popup = None;
        app.tab = tab;
        if tab == Tab::Requests {
            app.on_key(key(KeyCode::Down));
        }
        if let Some(action) = action {
            app.run_action(action);
            assert!(app.popup.is_some(), "{label} did not open");
        }
        let r = time(frames, || {
            term.draw(|f| ui::render(&mut app, f)).unwrap();
        });
        report(&format!("frame: {label}"), r);
        let r = time(50, || {
            app.refresh();
            term.draw(|f| ui::render(&mut app, f)).unwrap();
        });
        report(&format!("refresh + frame: {label}"), r);
    }
    app.popup = None;
    app.tab = Tab::Overview;

    report("refresh()", time(50, || app.refresh()));

    // A burst of bus events as the event loop applies it: one through
    // `on_event`, then up to 256 more through `drain`, then a frame, until
    // the burst is through; then the refresh it asks for on the next tick.
    let bus = app.state.bus.clone();
    let mut rx = bus.subscribe();
    let r = time(5, || {
        for i in 0..10_000 {
            bus.emit(burst(i));
        }
        while let Ok(ev) = rx.try_recv() {
            app.on_event(ev);
            assert!(hashi::tui::drain(&mut app, &mut rx));
        }
    });
    report("10k events: on_event + drain", r);
    let mut batches = 0;
    let r = time(5, || {
        batches = 0;
        for i in 0..10_000 {
            bus.emit(burst(i));
        }
        while let Ok(ev) = rx.try_recv() {
            app.on_event(ev);
            assert!(hashi::tui::drain(&mut app, &mut rx));
            term.draw(|f| ui::render(&mut app, f)).unwrap();
            batches += 1;
        }
        app.on_tick();
    });
    report(&format!("10k events, {batches} frames, tick"), r);

    // Idle: nothing runs, no toast, no popup. A tick between refreshes,
    // then one second of the event loop's ticks with its redraws.
    app.toast = None;
    app.refresh();
    let r = time(1000, || {
        app.on_tick();
    });
    report("idle tick (no refresh due)", r);
    for tab in [Tab::Overview, Tab::Tokens] {
        app.tab = tab;
        let mut redraws = 0;
        let t = Instant::now();
        let mut busy = Duration::ZERO;
        while t.elapsed() < Duration::from_secs(3) {
            let s = Instant::now();
            if app.on_tick() {
                term.draw(|f| ui::render(&mut app, f)).unwrap();
                redraws += 1;
            }
            busy += s.elapsed();
            std::thread::sleep(Duration::from_millis(100));
        }
        println!(
            "idle on {}, 3s of 100ms ticks: {redraws} redraws, {:.2} ms busy per second",
            tab.title(),
            busy.as_secs_f64() * 1e3 / 3.0
        );
    }
}
