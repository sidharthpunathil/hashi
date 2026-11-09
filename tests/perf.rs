//! A repeatable timing run for the HTTP request path and the database.
//!
//! Ignored by default. Run it on a release build:
//!
//! ```text
//! cargo test --release --test perf -- --ignored --nocapture
//! ```
//!
//! It seeds a temporary database (never `~/.hashi`) with 200 tokens, 10
//! projects, 30 accounts with usage rows and 20k finished requests, then
//! drives the router in process, with no socket, and prints the mean and
//! median of each path in microseconds. Set `HASHI_PERF_KEEP` to a file
//! path to keep a copy of the seeded database, for `EXPLAIN QUERY PLAN`.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode, header};
use hashi::accounts::{AccountKind, AccountMap};
use hashi::agent::AgentKind;
use hashi::api::{AgentInfo, AppState, RateLimiter, Runtime};
use hashi::events::Bus;
use hashi::guard::{ManualProbe, ResourceGuard, ResourceSettings};
use hashi::projects::Project;
use hashi::queue::Dispatcher;
use hashi::store::{NewRequest, NewToken, Store};
use hashi::token::{Limits, Role, SessionMode};
use hashi::usage::{Source, Window};
use hashi::util::now_ms;
use rusqlite::params;
use tower_service::Service;

const TOKENS: usize = 200;
const PROJECTS: usize = 10;
const ACCOUNTS: usize = 30;
const REQUESTS: usize = 20_000;

struct Seeded {
    dir: tempfile::TempDir,
    state: AppState,
    admin: String,
    user: String,
    done_id: String,
}

fn agents_for(i: usize) -> AgentKind {
    [AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini][i % 3]
}

/// Account `i`: ten per agent, `acct-0` to `acct-9`.
fn account(i: usize) -> (AgentKind, String) {
    (agents_for(i), format!("acct-{}", i / 3))
}

fn account_map(seed: usize) -> AccountMap {
    let mut map = AccountMap::new();
    for agent in [AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini] {
        map.insert(
            agent,
            (0..3)
                .map(|k| format!("acct-{}", (seed + k) % 10))
                .collect(),
        );
    }
    map
}

fn seed() -> Seeded {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hashi.db");
    let store = Arc::new(Store::open(&path).unwrap());
    let now = now_ms();

    for i in 0..ACCOUNTS {
        let (agent, name) = account(i);
        assert!(store.add_account(agent, &name, AccountKind::Login).unwrap());
        let windows = [
            Window {
                key: "five_hour".into(),
                label: "5 hours".into(),
                used_percent: (i * 3 % 100) as f64,
                resets_at: Some(now + 3_600_000),
                window_mins: Some(300),
                source: Source::Poll,
                observed_at: Some(now - 60_000),
            },
            Window {
                key: "seven_day".into(),
                label: "week".into(),
                used_percent: (i * 7 % 100) as f64,
                resets_at: Some(now + 86_400_000),
                window_mins: Some(10_080),
                source: Source::Poll,
                observed_at: Some(now - 60_000),
            },
        ];
        store.replace_usage(agent, &name, &windows).unwrap();
        store
            .set_poll_status(agent, &name, true, Some("max"), None)
            .unwrap();
    }

    for p in 0..PROJECTS {
        let mut models = BTreeMap::new();
        models.insert(AgentKind::Claude, "sonnet".to_owned());
        assert!(
            store
                .add_project(&Project {
                    name: format!("proj-{p}"),
                    path: format!("/tmp/proj-{p}"),
                    agents: vec![AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini],
                    accounts: account_map(p),
                    default_agent: Some(AgentKind::Claude),
                    models,
                    note: Some("benchmark".into()),
                    account_strategy: None,
                    rotate_at: None,
                    created_at: now,
                })
                .unwrap()
        );
    }

    let mut secrets = Vec::new();
    let mut ids = Vec::new();
    for i in 0..TOKENS {
        let (t, secret) = store
            .create_token(&NewToken {
                name: format!("token-{i}"),
                role: if i == 0 { Role::Admin } else { Role::User },
                session_mode: SessionMode::Stateless,
                agents: AgentKind::ALL.to_vec(),
                expires_in_days: None,
                note: Some("benchmark".into()),
                limits: if i == 0 {
                    Limits::default()
                } else {
                    Limits {
                        daily_requests: Some(1_000_000),
                        monthly_requests: Some(10_000_000),
                        daily_budget_usd: Some(1_000_000.0),
                        monthly_budget_usd: Some(10_000_000.0),
                        max_pending: Some(1_000),
                        stop_at_percent: None,
                    }
                },
                accounts: account_map(i),
                project: (i % 2 == 1).then(|| format!("proj-{}", i % PROJECTS)),
            })
            .unwrap();
        if i == 1 {
            store.set_token_share_usage(&t.id, true).unwrap();
        }
        secrets.push(secret.raw);
        ids.push(t.id);
    }

    // Finished requests, written in one transaction on a second connection.
    {
        let mut conn = rusqlite::Connection::open(&path).unwrap();
        let tx = conn.transaction().unwrap();
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO requests (id, token_id, token_name, agent, model, message,
                         stateful, status, response, error, input_tokens, output_tokens, cost_usd,
                         created_at, started_at, completed_at, account, project)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5, 0, ?6, ?7, ?8, 1200, 340, 0.012,
                         ?9, ?10, ?11, ?12, ?13)",
                )
                .unwrap();
            let body = "x".repeat(2_000);
            for n in 0..REQUESTS {
                let t = n % TOKENS;
                let (agent, acct) = account(n % ACCOUNTS);
                // Spread over the last 20 days, newest last.
                let created = now - (REQUESTS - n) as i64 * 86_400;
                let failed = n % 20 == 0;
                stmt.execute(params![
                    format!("{n:08}-0000-4000-8000-000000000000"),
                    ids[t],
                    format!("token-{t}"),
                    agent.as_str(),
                    format!("prompt {n} {body}"),
                    if failed { "failed" } else { "completed" },
                    (!failed).then(|| format!("answer {n} {body}")),
                    failed.then_some("simulated failure"),
                    created,
                    created + 10,
                    created + 5_000,
                    acct,
                    (t % 2 == 1).then(|| format!("proj-{}", t % PROJECTS)),
                ])
                .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    let state = AppState {
        store: Arc::clone(&store),
        bus: Bus::default(),
        dispatcher: Dispatcher::new(),
        runtime: Arc::new(Runtime {
            started: Instant::now(),
            listen: hashi::net::Listen::new("http://127.0.0.1:7878"),
            public_url: RwLock::new(None),
            name: "Bench".into(),
            tunnel: Default::default(),
            tailscale: Default::default(),
            agents: AgentKind::ALL
                .iter()
                .map(|a| AgentInfo {
                    agent: *a,
                    available: true,
                    version: Some("1.0.0".into()),
                    path: a.as_str().into(),
                })
                .collect(),
        }),
        limiter: Arc::new(RateLimiter::new(0)),
        default_agent: AgentKind::Claude,
        queue_limit: 1_000_000,
        run_timeout_secs: 60,
        guard: Arc::new(ResourceGuard::new(
            &ResourceSettings::with_probe(Arc::new(ManualProbe::roomy())),
            dir.path().to_path_buf(),
            Bus::default(),
        )),
        waits: Arc::default(),
    };
    Seeded {
        dir,
        state,
        admin: secrets[0].clone(),
        user: secrets[1].clone(),
        done_id: format!("{:08}-0000-4000-8000-000000000000", REQUESTS - 199),
    }
}

struct Timing {
    name: &'static str,
    mean: f64,
    median: f64,
}

fn summarize(name: &'static str, mut samples: Vec<Duration>) -> Timing {
    samples.sort();
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    let mean = samples.iter().map(|d| us(*d)).sum::<f64>() / samples.len() as f64;
    Timing {
        name,
        mean,
        median: us(samples[samples.len() / 2]),
    }
}

async fn call(app: &mut axum::Router, req: HttpRequest<Body>) -> StatusCode {
    let res = app.call(req).await.unwrap();
    let status = res.status();
    // Read the body so serialisation is part of the timing.
    axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    status
}

fn get(path: &str, secret: &str) -> HttpRequest<Body> {
    HttpRequest::get(path)
        .header(header::HOST, "127.0.0.1")
        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, secret: &str, body: String) -> HttpRequest<Body> {
    HttpRequest::post(path)
        .header(header::HOST, "127.0.0.1")
        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

async fn bench_get(
    app: &mut axum::Router,
    name: &'static str,
    path: &str,
    secret: &str,
    n: usize,
) -> Timing {
    for _ in 0..n / 10 {
        assert_eq!(call(app, get(path, secret)).await, StatusCode::OK, "{name}");
    }
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let req = get(path, secret);
        let t = Instant::now();
        let status = call(app, req).await;
        samples.push(t.elapsed());
        assert_eq!(status, StatusCode::OK, "{name}");
    }
    summarize(name, samples)
}

/// Fails what the submit runs left queued or running, outside the timing.
fn finish_pending(store: &Store) {
    while let Some(r) = store.claim_next(AgentKind::Claude).unwrap() {
        store.complete_request(&r.id, "done", None).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "timing run; use --release -- --ignored --nocapture"]
async fn perf() {
    let s = seed();
    if let Ok(p) = std::env::var("HASHI_PERF_KEEP") {
        let c = rusqlite::Connection::open(s.dir.path().join("hashi.db")).unwrap();
        c.execute("VACUUM INTO ?1", [p]).unwrap();
    }
    let store = Arc::clone(&s.state.store);
    let mut app = hashi::api::router(s.state.clone(), &hashi::net::NetLimits::default());
    let mut out = Vec::new();

    // Auth lookup, as every authenticated request does it.
    let n = 5_000;
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        assert!(store.authenticate(&s.user).unwrap().is_some());
        samples.push(t.elapsed());
    }
    out.push(summarize("auth lookup (store)", samples));

    // Submit through the store (the IMMEDIATE transaction with cap
    // checks), then claim, in rounds so the queue stays short.
    let user = store.authenticate(&s.user).unwrap().unwrap();
    let limits = user.limits;
    let (mut submit, mut claim) = (Vec::new(), Vec::new());
    for round in 0..40 {
        for k in 0..50 {
            let id = format!("bench-{round}-{k}");
            let t = Instant::now();
            let admitted = store
                .insert_request_checked(
                    NewRequest {
                        id: &id,
                        token: &user,
                        agent: AgentKind::Claude,
                        model: None,
                        message: "benchmark prompt",
                    },
                    &limits,
                    1_000_000,
                    2,
                )
                .unwrap();
            submit.push(t.elapsed());
            assert_eq!(admitted, hashi::store::Admission::Accepted);
        }
        for _ in 0..50 {
            let t = Instant::now();
            let r = store.claim_next(AgentKind::Claude).unwrap();
            claim.push(t.elapsed());
            store
                .complete_request(&r.unwrap().id, "done", None)
                .unwrap();
        }
    }
    out.push(summarize("insert_request_checked (store)", submit));
    out.push(summarize("claim_next (store)", claim));

    // POST /v1/ask through the router.
    let mut samples = Vec::new();
    for round in 0..20 {
        for _ in 0..50 {
            let req = post(
                "/v1/ask",
                &s.user,
                r#"{"message":"benchmark prompt","agent":"claude"}"#.to_owned(),
            );
            let t = Instant::now();
            let status = call(&mut app, req).await;
            if round > 0 {
                samples.push(t.elapsed());
            }
            assert_eq!(status, StatusCode::ACCEPTED);
        }
        finish_pending(&store);
    }
    out.push(summarize("POST /v1/ask", samples));

    // POST /v1/chat/completions, answering at once (up to queue insertion).
    let mut samples = Vec::new();
    for round in 0..20 {
        for _ in 0..50 {
            let req = post(
                "/v1/chat/completions",
                &s.user,
                r#"{"model":"claude","wait":0,"messages":[{"role":"system","content":"be brief"},{"role":"user","content":"benchmark prompt"}]}"#.to_owned(),
            );
            let t = Instant::now();
            let status = call(&mut app, req).await;
            if round > 0 {
                samples.push(t.elapsed());
            }
            assert_eq!(status, StatusCode::ACCEPTED);
        }
        finish_pending(&store);
    }
    out.push(summarize("POST /v1/chat/completions", samples));

    out.push(
        bench_get(
            &mut app,
            "GET /v1/me (shareUsage)",
            "/v1/me",
            &s.user,
            2_000,
        )
        .await,
    );
    let poll = format!("/v1/ask/{}", s.done_id);
    out.push(bench_get(&mut app, "GET /v1/ask/{id} (admin)", &poll, &s.admin, 2_000).await);
    out.push(bench_get(&mut app, "GET /v1/stats", "/v1/stats", &s.admin, 500).await);
    out.push(bench_get(&mut app, "GET /v1/accounts", "/v1/accounts", &s.admin, 200).await);
    out.push(bench_get(&mut app, "GET /v1/projects", "/v1/projects", &s.admin, 500).await);

    // MCP tools/list.
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let mut samples = Vec::new();
    for i in 0..2_200 {
        let req = post("/mcp", &s.user, body.to_owned());
        let t = Instant::now();
        let status = call(&mut app, req).await;
        if i >= 200 {
            samples.push(t.elapsed());
        }
        assert_eq!(status, StatusCode::OK);
    }
    out.push(summarize("POST /mcp tools/list", samples));

    println!();
    println!("{:<34} {:>12} {:>12}", "path", "mean us", "median us");
    for t in &out {
        println!("{:<34} {:>12.1} {:>12.1}", t.name, t.mean, t.median);
    }
}
