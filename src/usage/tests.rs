//! Summaries and strategies over synthetic usage, and the poller against a
//! local HTTP server, a fake `security` and a fake `codex app-server`.
//! Nothing here touches the network, a real Keychain or a real CLI.

use super::*;

const NOW: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

fn win(key: &str, used: f64, resets_in: Option<i64>, mins: i64) -> Window {
    Window {
        key: key.to_owned(),
        label: key.to_owned(),
        used_percent: used,
        resets_at: resets_in.map(|d| NOW + d),
        window_mins: Some(mins),
        source: Source::Poll,
        observed_at: Some(NOW - 60_000),
    }
}

/// An available account whose most used window is `used` percent, with a
/// weekly window resetting in `weekly_in`.
fn acct(used: Option<f64>, weekly_in: Option<i64>) -> Candidate {
    let mut windows = Vec::new();
    if let Some(u) = used {
        windows.push(win("five_hour", u, Some(HOUR), 300));
    }
    if let Some(w) = weekly_in {
        windows.push(win("seven_day", 1.0, Some(w), 10_080));
    }
    Candidate {
        available: true,
        summary: summarize(&windows, None, 90, NOW),
    }
}

fn off(mut c: Candidate) -> Candidate {
    c.available = false;
    c
}

fn limited(mut c: Candidate) -> Candidate {
    c.summary.state = UsageState::Limited;
    c
}

// ── summaries ──────────────────────────────────────────────────────────────

#[test]
fn headroom_is_100_minus_the_most_used_window() {
    let s = summarize(
        &[
            win("five_hour", 30.0, Some(HOUR), 300),
            win("seven_day", 72.5, Some(3 * DAY), 10_080),
        ],
        None,
        90,
        NOW,
    );
    assert!((s.headroom.unwrap() - 27.5).abs() < 1e-9);
    assert_eq!(s.headroom_pct(), Some(27));
    assert_eq!(s.binding.as_deref(), Some("seven_day"));
    assert_eq!(s.next_reset, Some(NOW + HOUR));
    assert_eq!(s.weekly_reset, Some(NOW + 3 * DAY));
    assert_eq!(s.state, UsageState::Ok);
}

#[test]
fn the_threshold_makes_a_warning_and_a_full_window_a_limit() {
    let w = [win("five_hour", 90.0, Some(HOUR), 300)];
    assert_eq!(summarize(&w, None, 90, NOW).state, UsageState::Warning);
    assert_eq!(summarize(&w, None, 95, NOW).state, UsageState::Ok);
    let full = [win("five_hour", 100.0, Some(2 * HOUR), 300)];
    let s = summarize(&full, None, 90, NOW);
    assert_eq!(s.state, UsageState::Limited);
    assert_eq!(s.limited_until, Some(NOW + 2 * HOUR));
    assert_eq!(s.headroom_pct(), Some(0));
}

#[test]
fn a_limited_mark_wins_while_it_lasts() {
    let w = [win("five_hour", 10.0, Some(HOUR), 300)];
    let s = summarize(&w, Some(NOW + 5 * HOUR), 90, NOW);
    assert_eq!(
        (s.state, s.limited_until),
        (UsageState::Limited, Some(NOW + 5 * HOUR))
    );
    // An expired mark is nothing.
    assert_eq!(summarize(&w, Some(NOW - 1), 90, NOW).state, UsageState::Ok);
    // The later of a mark and a full window.
    let full = [win("five_hour", 100.0, Some(7 * HOUR), 300)];
    let s = summarize(&full, Some(NOW + HOUR), 90, NOW);
    assert_eq!(s.limited_until, Some(NOW + 7 * HOUR));
}

#[test]
fn reset_windows_count_as_unused_and_old_undated_ones_are_dropped() {
    let past = win("five_hour", 99.0, Some(-1), 300);
    let s = summarize(std::slice::from_ref(&past), None, 90, NOW);
    assert_eq!(s.headroom_pct(), Some(100));
    assert_eq!(s.next_reset, None, "a window that reset has no next reset");
    let mut stale = win("x", 80.0, None, 300);
    stale.observed_at = Some(NOW - 7 * HOUR);
    assert_eq!(
        summarize(&[stale.clone()], None, 90, NOW).state,
        UsageState::Unknown
    );
    stale.observed_at = Some(NOW - HOUR);
    assert_eq!(summarize(&[stale], None, 90, NOW).headroom_pct(), Some(20));
}

#[test]
fn no_windows_is_unknown() {
    let s = summarize(&[], None, 90, NOW);
    assert_eq!(s, Summary::unknown());
    assert!(!s.over(50));
}

#[test]
fn weekly_reset_follows_the_most_used_weekly_window() {
    let s = summarize(
        &[
            win("seven_day", 20.0, Some(5 * DAY), 10_080),
            win("seven_day_opus", 60.0, Some(2 * DAY), 10_080),
            win("five_hour", 99.0, Some(HOUR), 300),
        ],
        None,
        90,
        NOW,
    );
    assert_eq!(s.weekly_reset, Some(NOW + 2 * DAY));
    assert_eq!(s.binding.as_deref(), Some("five_hour"));
}

#[test]
fn percentages_are_clamped() {
    assert!((clamp_percent(140.0) - 100.0).abs() < f64::EPSILON);
    assert!(clamp_percent(-3.0).abs() < f64::EPSILON);
    assert!(clamp_percent(f64::NAN).abs() < f64::EPSILON);
}

// ── strategies ─────────────────────────────────────────────────────────────

#[test]
fn in_order_takes_the_first_available_under_the_threshold() {
    let c = [acct(Some(50.0), None), acct(Some(10.0), None)];
    assert_eq!(choose(Strategy::InOrder, &c, 90), Some(0));
    let c = [acct(Some(95.0), None), acct(Some(10.0), None)];
    assert_eq!(
        choose(Strategy::InOrder, &c, 90),
        Some(1),
        "over the threshold is skipped"
    );
    let c = [off(acct(Some(5.0), None)), acct(Some(80.0), None)];
    assert_eq!(choose(Strategy::InOrder, &c, 90), Some(1));
    // Without any figures it is exactly the list order, as before.
    let c = [acct(None, None), acct(None, None)];
    assert_eq!(choose(Strategy::InOrder, &c, 90), Some(0));
}

#[test]
fn most_left_takes_the_most_headroom() {
    let c = [
        acct(Some(70.0), None),
        acct(Some(20.0), None),
        acct(Some(40.0), None),
    ];
    assert_eq!(choose(Strategy::MostLeft, &c, 90), Some(1));
}

#[test]
fn most_left_breaks_ties_by_list_order() {
    let c = [acct(Some(30.0), None), acct(Some(30.0), None)];
    assert_eq!(choose(Strategy::MostLeft, &c, 90), Some(0));
    let c = [
        acct(Some(60.0), None),
        acct(Some(30.0), None),
        acct(Some(30.0), None),
    ];
    assert_eq!(choose(Strategy::MostLeft, &c, 90), Some(1));
}

#[test]
fn unknown_ranks_after_known_under_and_before_over() {
    let c = [acct(None, None), acct(Some(85.0), None)];
    assert_eq!(
        choose(Strategy::MostLeft, &c, 90),
        Some(1),
        "known under beats unknown"
    );
    let c = [acct(Some(95.0), None), acct(None, None)];
    assert_eq!(
        choose(Strategy::MostLeft, &c, 90),
        Some(1),
        "unknown beats over"
    );
    let c = [acct(None, None), acct(None, None)];
    assert_eq!(choose(Strategy::MostLeft, &c, 90), Some(0));
}

#[test]
fn soonest_reset_spends_quota_that_would_expire() {
    let c = [
        acct(Some(10.0), Some(5 * DAY)),
        acct(Some(50.0), Some(DAY)),
        acct(Some(5.0), Some(3 * DAY)),
    ];
    assert_eq!(choose(Strategy::SoonestReset, &c, 90), Some(1));
    // Among those under the threshold only.
    let c = [acct(Some(95.0), Some(HOUR)), acct(Some(50.0), Some(DAY))];
    assert_eq!(choose(Strategy::SoonestReset, &c, 90), Some(1));
    // Ties go to list order.
    let c = [acct(Some(10.0), Some(DAY)), acct(Some(5.0), Some(DAY))];
    assert_eq!(choose(Strategy::SoonestReset, &c, 90), Some(0));
}

#[test]
fn soonest_reset_falls_back_to_most_left() {
    let c = [acct(Some(60.0), None), acct(Some(20.0), None)];
    assert_eq!(choose(Strategy::SoonestReset, &c, 90), Some(1));
    // A known weekly reset still wins over more headroom elsewhere.
    let c = [acct(Some(20.0), None), acct(Some(60.0), Some(6 * DAY))];
    assert_eq!(choose(Strategy::SoonestReset, &c, 90), Some(1));
}

#[test]
fn when_every_account_is_over_the_most_headroom_runs() {
    for s in Strategy::ALL {
        let c = [
            acct(Some(97.0), Some(DAY)),
            acct(Some(92.0), Some(5 * DAY)),
            acct(Some(99.0), None),
        ];
        assert_eq!(choose(s, &c, 90), Some(1), "{s}");
    }
}

#[test]
fn limited_and_unavailable_accounts_are_never_picked() {
    for s in Strategy::ALL {
        let c = [
            limited(acct(Some(1.0), Some(HOUR))),
            off(acct(Some(2.0), Some(HOUR))),
            acct(Some(99.0), None),
        ];
        assert_eq!(choose(s, &c, 90), Some(2), "{s}");
        let none = [limited(acct(Some(1.0), None)), off(acct(None, None))];
        assert_eq!(choose(s, &none, 90), None, "{s}");
        assert_eq!(choose(s, &[], 90), None, "{s}");
    }
}

#[test]
fn the_threshold_is_the_rotate_at_setting() {
    let c = [acct(Some(60.0), None), acct(Some(10.0), None)];
    assert_eq!(choose(Strategy::InOrder, &c, 90), Some(0));
    assert_eq!(
        choose(Strategy::InOrder, &c, 60),
        Some(1),
        "at the threshold counts as over"
    );
    assert_eq!(choose(Strategy::InOrder, &c, 61), Some(0));
}

#[test]
fn many_synthetic_lists_obey_the_common_rules() {
    // A small deterministic generator, so the cases are the same each run.
    let mut seed: u64 = 0x5eed;
    let mut next = move |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n
    };
    for _ in 0..2_000 {
        let len = usize::try_from(next(6)).unwrap_or(0) + 1;
        let rotate_at = u8::try_from(50 + next(50)).unwrap_or(90);
        let c: Vec<Candidate> = (0..len)
            .map(|_| {
                #[allow(clippy::cast_precision_loss)]
                let used = (next(5) > 0).then(|| next(101) as f64);
                let weekly = (next(2) == 0).then(|| i64::try_from(next(7) + 1).unwrap_or(1) * DAY);
                let mut a = acct(used, weekly);
                a.summary = summarize(
                    &[
                        used.map(|u| win("five_hour", u, Some(HOUR), 300)),
                        weekly.map(|w| win("seven_day", 1.0, Some(w), 10_080)),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>(),
                    None,
                    rotate_at,
                    NOW,
                );
                if next(6) == 0 {
                    a.available = false;
                }
                a
            })
            .collect();
        let usable = |i: usize| c[i].available && c[i].summary.state != UsageState::Limited;
        let any_under = (0..len).any(|i| usable(i) && !c[i].summary.over(rotate_at));
        for s in Strategy::ALL {
            match choose(s, &c, rotate_at) {
                None => assert!(
                    (0..len).all(|i| !usable(i)),
                    "{s}: nothing picked from {c:?}"
                ),
                Some(i) => {
                    assert!(usable(i), "{s} picked an unusable account");
                    if any_under {
                        assert!(
                            !c[i].summary.over(rotate_at),
                            "{s} picked one over while one is under"
                        );
                    } else {
                        let best = (0..len)
                            .filter(|j| usable(*j))
                            .filter_map(|j| c[j].summary.headroom)
                            .fold(f64::MIN, f64::max);
                        assert!(
                            c[i].summary
                                .headroom
                                .is_some_and(|h| (h - best).abs() < 1e-9),
                            "{s}: all over, the most headroom runs"
                        );
                    }
                    if s == Strategy::InOrder && any_under {
                        let first = (0..len).find(|j| usable(*j) && !c[*j].summary.over(rotate_at));
                        assert_eq!(Some(i), first);
                    }
                }
            }
        }
    }
}

// ── sessions ───────────────────────────────────────────────────────────────

#[test]
fn a_conversation_stays_until_its_account_reaches_the_threshold() {
    assert!(keep_session(&acct(Some(50.0), None), 90));
    assert!(
        keep_session(&acct(None, None), 90),
        "unknown usage keeps it"
    );
    assert!(!keep_session(&acct(Some(90.0), None), 90));
    assert!(!keep_session(&limited(acct(Some(1.0), None)), 90));
    assert!(!keep_session(&off(acct(Some(1.0), None)), 90));
}

// ── names and settings ─────────────────────────────────────────────────────

#[test]
fn strategies_parse_and_print() {
    for s in Strategy::ALL {
        assert_eq!(s.as_str().parse::<Strategy>(), Ok(s));
        assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
    }
    assert_eq!("Most_Left".parse::<Strategy>(), Ok(Strategy::MostLeft));
    assert!("round-robin".parse::<Strategy>().is_err());
    assert_eq!(Strategy::default(), Strategy::InOrder);
}

#[test]
fn rotate_at_is_50_to_99() {
    assert_eq!(check_rotate_at(50), Ok(50));
    assert_eq!(check_rotate_at(99), Ok(99));
    assert!(check_rotate_at(49).is_err());
    assert!(check_rotate_at(100).is_err());
}

#[test]
fn duration_labels_read_well() {
    assert_eq!(duration_label(300), "5 hours");
    assert_eq!(duration_label(10_080), "week");
    assert_eq!(duration_label(1_440), "day");
    assert_eq!(duration_label(43_200), "30 days");
    assert_eq!(duration_label(60), "hour");
    assert_eq!(duration_label(45), "45 minutes");
}

#[test]
fn the_hub_publishes_only_changes() {
    let hub = Hub::default();
    assert_eq!(
        hub.note(AgentKind::Claude, "w", UsageState::Ok, Some(50)),
        Ok(None)
    );
    assert_eq!(
        hub.note(AgentKind::Claude, "w", UsageState::Ok, Some(50)),
        Err(())
    );
    assert_eq!(
        hub.note(AgentKind::Claude, "w", UsageState::Warning, Some(5)),
        Ok(Some(UsageState::Ok))
    );
    assert_eq!(hub.policy(), Policy::default());
    hub.set_policy(Policy {
        strategy: Strategy::MostLeft,
        rotate_at: 80,
    });
    assert_eq!(hub.policy().rotate_at, 80);
    // No poller: a finished run asks nothing.
    hub.ran(AgentKind::Claude, "w");
    assert!(hub.poller().is_none());
}

#[test]
fn alerts_fire_on_crossing_limiting_and_recovering() {
    let ev = |previous, state| crate::events::Event::AccountUsage {
        agent: AgentKind::Claude,
        account: "work".into(),
        state,
        previous,
        headroom: Some(7),
        until: None,
    };
    use UsageState::{Limited, Ok, Unknown, Warning};
    assert!(
        ev(Some(Ok), Warning)
            .usage_alert()
            .unwrap()
            .contains("crossed")
    );
    assert!(
        ev(Some(Warning), Limited)
            .usage_alert()
            .unwrap()
            .contains("at its limit")
    );
    assert!(
        ev(Some(Limited), Ok)
            .usage_alert()
            .unwrap()
            .contains("recovered")
    );
    assert!(ev(Some(Ok), Ok).usage_alert().is_none());
    assert!(ev(None, Ok).usage_alert().is_none());
    assert!(ev(Some(Unknown), Ok).usage_alert().is_none());
    let line = crate::diagnostics::headless_line(&ev(Some(Ok), Warning));
    assert!(line.contains("crossed its rotate threshold"), "{line}");
}

// ── the poller ─────────────────────────────────────────────────────────────

#[cfg(unix)]
mod poller_tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;

    use super::super::poller::{MIN_GAP, PollError};
    use super::super::*;
    use super::NOW;
    use crate::accounts::AccountKind;
    use crate::store::Store;

    /// The `authorization` and `anthropic-beta` headers of one call.
    type Seen = (Option<String>, Option<String>);

    /// What the fake endpoint answers, and what it saw.
    #[derive(Clone, Default)]
    struct Endpoint {
        reply: Arc<Mutex<(u16, Option<String>, String)>>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Endpoint {
        fn answer(&self, status: u16, retry_after: Option<&str>, body: &str) {
            *self.reply.lock().unwrap() = (status, retry_after.map(str::to_owned), body.to_owned());
        }

        fn calls(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    /// A local stand-in for the usage endpoint on a random port.
    async fn endpoint() -> (String, Endpoint) {
        let ep = Endpoint::default();
        ep.answer(200, None, "{}");
        let state = ep.clone();
        let app = axum::Router::new().route(
            claude::USAGE_PATH,
            axum::routing::get(move |headers: HeaderMap| {
                let ep = state.clone();
                async move {
                    let h = |k: &str| {
                        headers
                            .get(k)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned)
                    };
                    ep.seen
                        .lock()
                        .unwrap()
                        .push((h("authorization"), h("anthropic-beta")));
                    let (status, retry, body) = ep.reply.lock().unwrap().clone();
                    let mut res = (StatusCode::from_u16(status).unwrap(), body).into_response();
                    if let Some(r) = retry {
                        res.headers_mut().insert("retry-after", r.parse().unwrap());
                    }
                    res
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), ep)
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// A fake `security` that logs its arguments and prints credentials
    /// with `token` expiring at `expires_at`.
    fn fake_security(dir: &Path, token: &str, expires_at: i64) -> PathBuf {
        let log = dir.join("security.log");
        script(
            dir,
            "security",
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nprintf '%s' '{{\"claudeAiOauth\":{{\"accessToken\":\"{token}\",\"refreshToken\":\"never-used\",\"expiresAt\":{expires_at},\"subscriptionType\":\"max\"}}}}'\n",
                log.display()
            ),
        )
    }

    struct Rig {
        dir: tempfile::TempDir,
        store: Arc<Store>,
        hub: Arc<Hub>,
        bus: crate::events::Bus,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        Rig {
            store: Arc::new(Store::open_in_memory().unwrap()),
            hub: Arc::new(Hub::default()),
            bus: crate::events::Bus::default(),
            dir,
        }
    }

    impl Rig {
        fn home(&self) -> PathBuf {
            self.dir.path().join("home")
        }

        fn poller(&self, settings: UsageSettings, codex: Option<PathBuf>) -> Poller {
            Poller::new(
                Arc::clone(&self.store),
                self.bus.clone(),
                &self.hub,
                self.home(),
                settings,
                codex,
                &[AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini],
            )
        }

        fn claude_settings(&self, base: &str, security: PathBuf) -> UsageSettings {
            UsageSettings {
                claude_base_url: base.to_owned(),
                security_bin: Some(security),
                keychain_user: Some("owner".to_owned()),
                claude_home: Some(self.dir.path().join("no-claude-home")),
                codex_home: Some(self.dir.path().join("no-codex-home")),
                ..UsageSettings::default()
            }
        }

        /// Settings that reach nothing real: no Keychain, no endpoint, no
        /// owner's directories.
        fn safe(&self) -> UsageSettings {
            UsageSettings {
                claude_base_url: "http://127.0.0.1:1".to_owned(),
                security_bin: None,
                keychain_user: None,
                claude_home: Some(self.dir.path().join("no-claude-home")),
                codex_home: Some(self.dir.path().join("no-codex-home")),
                ..UsageSettings::default()
            }
        }

        fn login(&self, agent: AgentKind, name: &str) -> PathBuf {
            crate::accounts::create_dir(&self.home(), agent, name).unwrap();
            self.store
                .add_account(agent, name, AccountKind::Login)
                .unwrap();
            // As runs see it, once the home exists.
            crate::accounts::account_dir(&self.home(), agent, name)
        }
    }

    const USAGE: &str = r#"{"five_hour":{"utilization":35,"resets_at":"2099-01-01T05:00:00Z"},"seven_day":{"utilization":91.5,"resets_at":"2099-01-05T00:00:00Z"},"seven_day_opus":null}"#;

    #[tokio::test]
    async fn claude_usage_is_read_with_the_keychain_token_for_the_accounts_dir() {
        let r = rig();
        let (base, ep) = endpoint().await;
        ep.answer(200, None, USAGE);
        let dir = r.login(AgentKind::Claude, "work");
        let security = fake_security(r.dir.path(), "tok-work", NOW * 10);
        let mut events = r.bus.subscribe();
        let p = r.poller(r.claude_settings(&base, security), None);
        p.poll(AgentKind::Claude, "work").await.unwrap();

        assert_eq!(
            ep.calls(),
            [(
                Some("Bearer tok-work".to_owned()),
                Some(claude::BETA.to_owned())
            )]
        );
        let log = std::fs::read_to_string(r.dir.path().join("security.log")).unwrap();
        let service = claude::service_name(Some(&dir.to_string_lossy()));
        assert_eq!(
            log.trim(),
            format!("find-generic-password -a owner -w -s {service}")
        );
        let windows = r.store.usage_windows(AgentKind::Claude, "work").unwrap();
        assert_eq!(windows.len(), 2);
        let status = r
            .store
            .poll_status(AgentKind::Claude, "work")
            .unwrap()
            .unwrap();
        assert_eq!(
            (status.plan.as_deref(), status.note.as_deref()),
            (Some("max"), None)
        );
        let s = r
            .store
            .account_summary(AgentKind::Claude, "work", 90, crate::util::now_ms())
            .unwrap();
        assert_eq!((s.state, s.headroom_pct()), (UsageState::Warning, Some(8)));
        match events.try_recv().unwrap() {
            crate::events::Event::AccountUsage {
                state, headroom, ..
            } => {
                assert_eq!((state, headroom), (UsageState::Warning, Some(8)));
            }
            other => panic!("{other:?}"),
        }
        // Nothing stored or published holds the token.
        let all = format!("{windows:?}{status:?}");
        assert!(!all.contains("tok-work"));
    }

    #[tokio::test]
    async fn the_default_account_uses_the_unsuffixed_service() {
        let r = rig();
        let (base, ep) = endpoint().await;
        ep.answer(200, None, USAGE);
        let security = fake_security(r.dir.path(), "tok-default", NOW * 10);
        let p = r.poller(r.claude_settings(&base, security), None);
        if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
            return;
        }
        p.poll(AgentKind::Claude, "default").await.unwrap();
        let log = std::fs::read_to_string(r.dir.path().join("security.log")).unwrap();
        assert!(log.trim().ends_with("-s Claude Code-credentials"), "{log}");
        assert_eq!(ep.calls()[0].0.as_deref(), Some("Bearer tok-default"));
    }

    #[tokio::test]
    async fn an_expired_token_is_never_refreshed_or_sent() {
        let r = rig();
        let (base, ep) = endpoint().await;
        r.login(AgentKind::Claude, "work");
        let security = fake_security(r.dir.path(), "tok-old", 1_000);
        let p = r.poller(r.claude_settings(&base, security), None);
        let err = p.poll(AgentKind::Claude, "work").await.unwrap_err();
        assert!(matches!(err, PollError::Failed { .. }), "{err:?}");
        assert!(
            ep.calls().is_empty(),
            "the endpoint is not called with an expired token"
        );
        let st = r
            .store
            .poll_status(AgentKind::Claude, "work")
            .unwrap()
            .unwrap();
        assert_eq!(st.note.as_deref(), Some(claude::REFRESHES_ON_RUN));
        let log = std::fs::read_to_string(r.dir.path().join("security.log")).unwrap();
        assert!(
            !log.contains("delete") && !log.contains("add-generic"),
            "{log}"
        );
    }

    fn retry_of(e: &PollError) -> Duration {
        match e {
            PollError::Failed { retry, .. } => *retry,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn back_off_honours_retry_after_and_waits_after_a_rejection() {
        let r = rig();
        let (base, ep) = endpoint().await;
        r.login(AgentKind::Claude, "work");
        let security = fake_security(r.dir.path(), "tok", NOW * 10);
        let settings = r.claude_settings(&base, security);
        let cases = [
            (429, Some("120"), Duration::from_secs(120)),
            (403, None, claude::DEFAULT_RETRY),
            (429, Some("999999"), claude::MAX_RETRY),
            (401, None, claude::AUTH_RETRY),
            (500, None, claude::DEFAULT_RETRY),
        ];
        for (status, after, want) in cases {
            // A fresh poller each time: one account is read at most once a
            // minute.
            let p = r.poller(settings.clone(), None);
            ep.answer(status, after, "{}");
            let before = std::time::Instant::now();
            let err = p.poll(AgentKind::Claude, "work").await.unwrap_err();
            assert_eq!(retry_of(&err), want, "HTTP {status}");
            let due = p.next_due(&(AgentKind::Claude, "work".to_owned())).unwrap();
            assert!(due >= before + want.max(MIN_GAP), "HTTP {status}");
            assert!(
                due <= std::time::Instant::now() + want.max(MIN_GAP),
                "HTTP {status}"
            );
            let note = r
                .store
                .poll_status(AgentKind::Claude, "work")
                .unwrap()
                .unwrap()
                .note
                .unwrap();
            assert!(note.starts_with("unknown"), "{note}");
            assert!(!note.contains("tok"), "{note}");
        }
    }

    #[tokio::test]
    async fn one_account_is_read_at_most_once_a_minute() {
        let r = rig();
        let (base, ep) = endpoint().await;
        ep.answer(200, None, USAGE);
        r.login(AgentKind::Claude, "work");
        let security = fake_security(r.dir.path(), "tok", NOW * 10);
        let p = r.poller(r.claude_settings(&base, security), None);
        p.poll(AgentKind::Claude, "work").await.unwrap();
        assert!(matches!(
            p.poll(AgentKind::Claude, "work").await,
            Err(PollError::TooSoon(_))
        ));
        assert_eq!(ep.calls().len(), 1);
        let due = p.next_due(&(AgentKind::Claude, "work".to_owned())).unwrap();
        assert!(
            due >= std::time::Instant::now() + Duration::from_secs(4 * 60),
            "about five minutes"
        );
    }

    #[tokio::test]
    async fn a_read_is_skipped_while_a_run_on_the_account_is_active() {
        let r = rig();
        r.login(AgentKind::Codex, "team");
        let (t, _) = r
            .store
            .create_token(&crate::store::NewToken {
                name: "t".into(),
                role: crate::token::Role::User,
                session_mode: crate::token::SessionMode::Stateless,
                agents: vec![AgentKind::Codex],
                expires_in_days: None,
                note: None,
                limits: crate::token::Limits::default(),
                accounts: crate::accounts::AccountMap::new(),
                project: None,
            })
            .unwrap();
        r.store
            .insert_request(crate::store::NewRequest {
                id: "r1",
                token: &t,
                agent: AgentKind::Codex,
                model: None,
                message: "hi",
            })
            .unwrap();
        r.store.claim_next(AgentKind::Codex).unwrap().unwrap();
        r.store.set_request_account("r1", "team").unwrap();
        let fake = script(
            r.dir.path(),
            "codex",
            "#!/bin/sh\necho called >> \"$0.log\"\n",
        );
        let p = r.poller(r.safe(), Some(fake.clone()));
        assert_eq!(p.poll(AgentKind::Codex, "team").await, Err(PollError::Busy));
        assert!(
            !fake.with_extension("log").exists(),
            "the app-server never started"
        );
    }

    /// Answers the handshake, checks the read and reports `home` (or the
    /// account's `CODEX_HOME`) as its home.
    fn fake_app_server(dir: &Path, home: Option<&str>) -> PathBuf {
        let home = home.map_or_else(|| "$CODEX_HOME".to_owned(), str::to_owned);
        script(
            dir,
            "codex",
            &format!(
                r#"#!/bin/sh
[ "$1" = "app-server" ] || exit 2
log="$(dirname "$0")/app-server.log"
read -r init
case "$init" in *'"method":"initialize"'*'"name":"hashi"'*) ;; *) echo "bad init: $init" >> "$log"; exit 3 ;; esac
printf '{{"id":0,"result":{{"userAgent":"fake/0","codexHome":"%s","platformFamily":"unix","platformOs":"macos"}}}}\n' "{home}"
read -r ready
case "$ready" in *'"method":"initialized"'*) ;; *) echo "bad initialized: $ready" >> "$log"; exit 4 ;; esac
read -r req
case "$req" in *'"excludeResetCreditDetails":true'*) ;; *) echo "bad read: $req" >> "$log"; exit 5 ;; esac
case "$req" in *'"method":"account/rateLimits/read"'*) ;; *) exit 6 ;; esac
echo "CODEX_HOME=$CODEX_HOME OPENAI_API_KEY=${{OPENAI_API_KEY:-unset}}" >> "$log"
echo '{{"method":"account/rateLimits/updated","params":{{"rateLimits":{{}}}}}}'
echo '{{"id":1,"result":{{"ordinaryUsageAllowed":true,"rateLimits":{{"limitId":"codex","primary":{{"usedPercent":42,"windowDurationMins":300,"resetsAt":4102444800}},"secondary":{{"usedPercent":12,"windowDurationMins":10080,"resetsAt":4102900000}},"planType":"pro","credits":null,"rateLimitReachedType":null}},"rateLimitsByLimitId":null}}}}'
read -r _rest
"#
            ),
        )
    }

    #[tokio::test]
    async fn codex_usage_comes_from_a_short_lived_app_server() {
        let r = rig();
        let dir = r.login(AgentKind::Codex, "team");
        let fake = fake_app_server(r.dir.path(), None);
        let p = r.poller(r.safe(), Some(fake));
        p.poll(AgentKind::Codex, "team").await.unwrap();
        let log = std::fs::read_to_string(r.dir.path().join("app-server.log")).unwrap();
        assert_eq!(
            log.trim(),
            format!("CODEX_HOME={} OPENAI_API_KEY=unset", dir.display())
        );
        let w = r.store.usage_windows(AgentKind::Codex, "team").unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].label, "5 hours");
        let st = r
            .store
            .poll_status(AgentKind::Codex, "team")
            .unwrap()
            .unwrap();
        assert_eq!(st.plan.as_deref(), Some("pro"));
        let s = r
            .store
            .account_summary(AgentKind::Codex, "team", 90, crate::util::now_ms())
            .unwrap();
        assert_eq!(s.headroom_pct(), Some(58));
    }

    #[tokio::test]
    async fn an_app_server_on_another_home_is_refused() {
        let r = rig();
        r.login(AgentKind::Codex, "team");
        let fake = fake_app_server(r.dir.path(), Some("/somewhere/else"));
        let p = r.poller(r.safe(), Some(fake));
        let err = p.poll(AgentKind::Codex, "team").await.unwrap_err();
        assert!(err.to_string().contains("another CODEX_HOME"), "{err}");
        assert!(
            r.store
                .usage_windows(AgentKind::Codex, "team")
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn key_accounts_and_other_agents_have_no_live_usage() {
        let r = rig();
        r.store
            .add_account(AgentKind::Claude, "ci", AccountKind::ApiKey)
            .unwrap();
        let p = r.poller(r.safe(), None);
        assert!(matches!(
            p.poll(AgentKind::Claude, "ci").await,
            Err(PollError::NotSupported(_))
        ));
        assert!(matches!(
            p.poll(AgentKind::Gemini, "default").await,
            Err(PollError::NotSupported(_))
        ));
    }

    #[tokio::test]
    async fn defaults_and_every_enabled_login_account_are_polled() {
        let r = rig();
        r.login(AgentKind::Claude, "bound");
        r.login(AgentKind::Claude, "loose");
        r.login(AgentKind::Claude, "off");
        r.login(AgentKind::Codex, "team");
        r.store
            .add_account(AgentKind::Claude, "ci", AccountKind::ApiKey)
            .unwrap();
        r.store
            .set_account_disabled(AgentKind::Claude, "off", true)
            .unwrap();
        let map = crate::accounts::parse_bindings("claude=bound,off,ci").unwrap();
        r.store
            .create_token(&crate::store::NewToken {
                name: "t".into(),
                role: crate::token::Role::User,
                session_mode: crate::token::SessionMode::Stateless,
                agents: vec![AgentKind::Claude],
                expires_in_days: None,
                note: None,
                limits: crate::token::Limits::default(),
                accounts: map,
                project: None,
            })
            .unwrap();
        let folder = tempfile::tempdir().unwrap();
        r.store
            .add_project(&crate::projects::Project {
                name: "api".into(),
                path: folder.path().display().to_string(),
                agents: vec![AgentKind::Codex],
                accounts: crate::accounts::parse_bindings("codex=team").unwrap(),
                default_agent: None,
                models: crate::projects::ModelMap::new(),
                note: None,
                account_strategy: None,
                rotate_at: None,
                created_at: 0,
            })
            .unwrap();
        let p = r.poller(r.safe(), None);
        let names: Vec<String> = p
            .eligible()
            .iter()
            .map(|(a, n)| format!("{a}:{n}"))
            .collect();
        assert_eq!(
            names,
            [
                "claude:default",
                "claude:bound",
                "claude:loose",
                "codex:default",
                "codex:team"
            ]
        );
    }

    #[tokio::test]
    async fn a_finished_run_brings_the_next_read_forward() {
        let r = rig();
        let (base, ep) = endpoint().await;
        ep.answer(200, None, USAGE);
        r.login(AgentKind::Claude, "work");
        let security = fake_security(r.dir.path(), "tok", NOW * 10);
        let p = Arc::new(r.poller(r.claude_settings(&base, security), None));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        r.hub.install(Arc::clone(&p), Some(tx));
        p.poll(AgentKind::Claude, "work").await.unwrap();
        let key = (AgentKind::Claude, "work".to_owned());
        let far = p.next_due(&key).unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(Arc::clone(&p).run(rx, cancel.clone()));
        r.hub.ran(AgentKind::Claude, "work");
        for _ in 0..50 {
            if p.next_due(&key).unwrap() < far {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let soon = p.next_due(&key).unwrap();
        assert!(soon < far, "a run moves the read forward");
        assert!(
            soon <= std::time::Instant::now() + MIN_GAP,
            "to a minute after the last read"
        );
        cancel.cancel();
        task.await.unwrap();
    }
}
