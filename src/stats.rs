//! The owner's own use of Claude Code, from the session files it keeps.
//!
//! Off until the owner turns it on (`hashi stats on`, or `t` in the
//! dashboard's My usage popup; saved as `trackOwnUse` in `settings.json`).
//! Then Hashi reads the session files Claude Code writes for every
//! interactive session, `~/.claude/projects/<project>/<session>.jsonl` and
//! the same under each Hashi account's folder, a little at a time (it
//! remembers how far it read each file). From each assistant message it
//! keeps only numbers: the tokens, the model, the project folder's name,
//! the session id, the time and an estimated cost at list prices. Never the
//! text. Everything stays in the local database, and `hashi stats forget`
//! deletes it.
//!
//! Which account a session ran on comes from its `session-owner` line (the
//! account UUID Claude Code signed in with), matched against the Hashi
//! accounts' identities, so a `hashi switch` mid-week is credited correctly.
//! Headless sessions (`claude -p` and the SDK, which is how Hashi itself
//! runs prompts) are left out: Hashi's own runs are counted in its requests.
//!
//! The report ([`report`]) adds two things the files alone cannot say: how
//! fast each account's quota windows are filling and when they would run
//! out at that pace (from the readings Hashi keeps in `quota_history`), and
//! the owner's own use next to what Hashi ran for others on each account.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::accounts::{self as acct, AccountKind};
use crate::agent::AgentKind;
use crate::store::{FileState, GroupBy, OwnRow, OwnTotals, SharedTotals, Store};
use crate::util::now_ms;

/// How often the server reads new lines while tracking is on.
pub const SCAN_EVERY: Duration = Duration::from_secs(120);

/// Deepest folder level searched under a `projects` folder.
const MAX_DEPTH: usize = 4;

/// Longest line read; anything longer is skipped (a message with a huge
/// pasted file, say), so one line can never take all the memory.
const MAX_LINE: usize = 32 * 1024 * 1024;

/// Whether tracking is on for `home`.
#[must_use]
pub fn enabled(home: &Path) -> bool {
    crate::settings::load(home)
        .ok()
        .and_then(|s| s.track_own_use)
        .unwrap_or(false)
}

/// Turns tracking on or off.
///
/// # Errors
///
/// When the settings cannot be saved.
pub fn set_enabled(home: &Path, on: bool) -> Result<(), String> {
    let mut settings = crate::settings::load(home).unwrap_or_default();
    settings.track_own_use = on.then_some(true);
    crate::settings::save(home, &settings).map_err(|e| format!("saving settings: {e}"))
}

// ── prices ─────────────────────────────────────────────────────────────────

/// List prices per million tokens, USD.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Price {
    input: f64,
    output: f64,
    cache_read: f64,
}

impl Price {
    const fn new(input: f64, output: f64, cache_read: f64) -> Self {
        Self {
            input,
            output,
            cache_read,
        }
    }
}

/// The list price of `model`: exact families first, then by tier, so a
/// model released after this table still gets a sensible estimate.
fn price(model: &str) -> Price {
    let m = model.to_ascii_lowercase();
    let table: [(&str, Price); 14] = [
        ("fable-5-2", Price::new(8.0, 40.0, 0.20)),
        ("mythos-5-2", Price::new(8.0, 40.0, 0.20)),
        ("fable-5", Price::new(10.0, 50.0, 1.0)),
        ("mythos-5", Price::new(10.0, 50.0, 1.0)),
        ("opus-5-5", Price::new(4.0, 20.0, 0.20)),
        ("opus-5", Price::new(5.0, 25.0, 0.50)),
        ("opus-4-5", Price::new(5.0, 25.0, 0.50)),
        ("opus-4-6", Price::new(5.0, 25.0, 0.50)),
        ("opus-4-7", Price::new(5.0, 25.0, 0.50)),
        ("opus-4-8", Price::new(5.0, 25.0, 0.50)),
        ("sonnet-5-5", Price::new(2.0, 10.0, 0.10)),
        ("sonnet-5", Price::new(2.0, 10.0, 0.20)),
        ("haiku-5-5", Price::new(0.10, 0.50, 0.01)),
        ("haiku-4-5", Price::new(1.0, 5.0, 0.10)),
    ];
    if let Some((_, p)) = table.iter().find(|(k, _)| m.contains(k)) {
        return *p;
    }
    if m.contains("opus") {
        Price::new(15.0, 75.0, 1.50)
    } else if m.contains("haiku") {
        Price::new(0.80, 4.0, 0.08)
    } else if m.contains("fable") || m.contains("mythos") {
        Price::new(8.0, 40.0, 0.20)
    } else {
        // Sonnet and anything unknown.
        Price::new(3.0, 15.0, 0.30)
    }
}

/// The estimated cost of one message: cache writes at 1.25x the input price
/// for five minutes and 2x for an hour, as the API bills them.
fn cost(model: &str, u: &Usage) -> f64 {
    let p = price(model);
    let (w5, w1h) = match &u.cache_creation {
        Some(c) if c.ephemeral_5m_input_tokens + c.ephemeral_1h_input_tokens > 0 => {
            (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens)
        }
        _ => (u.cache_creation_input_tokens, 0),
    };
    #[allow(clippy::cast_precision_loss)]
    let tokens = |n: i64| n as f64 / 1_000_000.0;
    tokens(u.input_tokens) * p.input
        + tokens(u.output_tokens) * p.output
        + tokens(u.cache_read_input_tokens) * p.cache_read
        + tokens(w5) * p.input * 1.25
        + tokens(w1h) * p.input * 2.0
}

// ── reading the session files ──────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CacheCreation {
    ephemeral_5m_input_tokens: i64,
    ephemeral_1h_input_tokens: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    cache_creation: Option<CacheCreation>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

/// The parts of a session line Hashi reads. Everything else, the text
/// included, is skipped by the parser.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<Message>,
    timestamp: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
    entrypoint: Option<String>,
    request_id: Option<String>,
    account_uuid: Option<String>,
    permission_mode: Option<String>,
}

/// A folder of session files, and the account they all ran on when the
/// folder belongs to one (a Hashi account's own folder).
#[derive(Debug, Clone)]
struct Root {
    dir: PathBuf,
    account: Option<String>,
}

/// What [`scan`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    /// Session files looked at.
    pub files: usize,
    /// Bytes read.
    pub bytes: u64,
    /// Messages added.
    pub added: usize,
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn account_uuid(claude_json: &Path) -> Option<String> {
    read_json(claude_json)?
        .get("oauthAccount")?
        .get("accountUuid")?
        .as_str()
        .map(str::to_owned)
}

/// Which Hashi account each signed-in Claude identity is.
fn owners(
    store: &Store,
    home: &Path,
    live: Option<&acct::switch::Live>,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for a in store.list_accounts().unwrap_or_default() {
        if a.agent == AgentKind::Claude && a.kind == AccountKind::Login {
            let dir = acct::account_dir(home, AgentKind::Claude, &a.name);
            if let Some(uuid) = account_uuid(&dir.join(".claude.json")) {
                map.insert(uuid, a.name);
            }
        }
    }
    let original = acct::root(home)
        .join(AgentKind::Claude.as_str())
        .join(acct::switch::ORIGINAL_DIR)
        .join(".claude.json");
    if let Some(uuid) = account_uuid(&original) {
        map.entry(uuid).or_insert_with(|| acct::DEFAULT.to_owned());
    }
    if let Some(uuid) = live.and_then(|l| account_uuid(&l.claude_json)) {
        map.entry(uuid)
            .or_insert_with(|| acct::switch::active(home, AgentKind::Claude));
    }
    map
}

fn roots(store: &Store, home: &Path, live: Option<&acct::switch::Live>) -> Vec<Root> {
    let mut out = Vec::new();
    if let Some(l) = live {
        out.push(Root {
            dir: l.claude_dir.join("projects"),
            account: None,
        });
    }
    for a in store.list_accounts().unwrap_or_default() {
        if a.agent == AgentKind::Claude && a.kind == AccountKind::Login {
            out.push(Root {
                dir: acct::account_dir(home, AgentKind::Claude, &a.name).join("projects"),
                account: Some(a.name),
            });
        }
    }
    out
}

/// The `.jsonl` files under `dir`, without following links.
fn session_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(t) = e.file_type() else { continue };
        let path = e.path();
        if t.is_dir() && depth < MAX_DEPTH {
            session_files(&path, depth + 1, out);
        } else if t.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
            out.push(path);
        }
    }
}

fn parse_time(ts: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

fn project_of(cwd: Option<&str>) -> String {
    cwd.and_then(|c| Path::new(c).file_name()).map_or_else(
        || "unknown".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Reads the new lines of one session file.
fn scan_file(
    store: &Store,
    path: &Path,
    fixed: Option<&str>,
    owners: &HashMap<String, String>,
) -> Result<(u64, usize), String> {
    let key = path.to_string_lossy().into_owned();
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let mut state = store
        .own_file_state(&key)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    if state.offset == len {
        return Ok((0, 0));
    }
    if state.offset > len {
        // Rewritten from the start; the message ids keep counts right.
        state = FileState::default();
    }
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(state.offset))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let mut rows = Vec::new();
    let mut buf = Vec::new();
    let start = state.offset;
    loop {
        buf.clear();
        let n = reader
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)
            .map_err(|e| e.to_string())?;
        if n == 0 || buf.last() != Some(&b'\n') {
            if n > MAX_LINE {
                // An endless line: skip to its end.
                let mut skip = Vec::new();
                let more = reader
                    .read_until(b'\n', &mut skip)
                    .map_err(|e| e.to_string())?;
                if skip.last() == Some(&b'\n') {
                    state.offset += (n + more) as u64;
                    continue;
                }
            }
            // A line still being written stays for the next scan.
            break;
        }
        state.offset += n as u64;
        // Cheap filter before parsing: only three kinds of line matter.
        let text = &buf[..];
        let has = |needle: &[u8]| memchr::memmem::find(text, needle).is_some();
        if !(has(b"\"assistant\"") || has(b"session-owner") || has(b"permission-mode")) {
            continue;
        }
        let Ok(line) = serde_json::from_slice::<Line>(text) else {
            continue;
        };
        match line.kind.as_deref() {
            Some("session-owner") => state.owner = line.account_uuid,
            Some("permission-mode") => {
                if line.permission_mode.as_deref() == Some("bypassPermissions") {
                    state.skip_perms = true;
                }
            }
            Some("assistant") => {
                if line
                    .entrypoint
                    .as_deref()
                    .is_some_and(|e| e.starts_with("sdk"))
                {
                    state.sdk = true;
                }
                if state.sdk {
                    continue;
                }
                let Some(msg) = line.message else { continue };
                let (Some(id), Some(model), Some(usage)) = (msg.id, msg.model, msg.usage) else {
                    continue;
                };
                if model.starts_with('<') {
                    // `<synthetic>`: an error shown as a message, not a call.
                    continue;
                }
                let Some(at) = line.timestamp.as_deref().and_then(parse_time) else {
                    continue;
                };
                let account = fixed.map_or_else(
                    || match &state.owner {
                        Some(uuid) => owners
                            .get(uuid)
                            .cloned()
                            .unwrap_or_else(|| "other".to_owned()),
                        None => acct::DEFAULT.to_owned(),
                    },
                    str::to_owned,
                );
                rows.push(OwnRow {
                    msg_id: match line.request_id {
                        Some(r) => format!("{id}:{r}"),
                        None => id,
                    },
                    agent: AgentKind::Claude,
                    account,
                    cost_usd: cost(&model, &usage),
                    model,
                    project: project_of(line.cwd.as_deref()),
                    session: line.session_id.unwrap_or_default(),
                    at,
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                    cache_read: usage.cache_read_input_tokens,
                    cache_write: usage.cache_creation_input_tokens,
                    skip_perms: state.skip_perms,
                });
            }
            _ => {}
        }
    }
    let added = store
        .save_own_rows(&key, &state, &rows)
        .map_err(|e| e.to_string())?;
    Ok((state.offset - start, added))
}

/// Reads whatever was added to the session files since the last scan.
///
/// # Errors
///
/// When the database cannot be written. A file that cannot be read is
/// skipped.
pub fn scan(store: &Store, home: &Path) -> Result<ScanReport, String> {
    let live = acct::switch::Live::from_user().ok();
    scan_with(store, home, live.as_ref())
}

/// [`scan`] with the owner's normal places given, for tests.
///
/// # Errors
///
/// When the database cannot be written.
pub fn scan_with(
    store: &Store,
    home: &Path,
    live: Option<&acct::switch::Live>,
) -> Result<ScanReport, String> {
    let owners = owners(store, home, live);
    let mut report = ScanReport::default();
    for root in roots(store, home, live) {
        let mut files = Vec::new();
        session_files(&root.dir, 0, &mut files);
        for path in files {
            report.files += 1;
            match scan_file(store, &path, root.account.as_deref(), &owners) {
                Ok((bytes, added)) => {
                    report.bytes += bytes;
                    report.added += added;
                }
                Err(e) => tracing::debug!(file = %path.display(), "own usage: {e}"),
            }
        }
    }
    Ok(report)
}

// ── the report ─────────────────────────────────────────────────────────────

/// One account's own use next to what Hashi ran for others on it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Versus {
    /// The account.
    pub account: String,
    /// The owner's own use.
    pub own: OwnTotals,
    /// What Hashi ran for tokens.
    pub shared: SharedTotals,
}

impl Versus {
    /// The owner's share of the estimated cost, 0 to 100, when there is any.
    #[must_use]
    pub fn own_share(&self) -> Option<f64> {
        let total = self.own.cost_usd + self.shared.cost_usd;
        (total > 0.0).then(|| self.own.cost_usd / total * 100.0)
    }
}

/// How fast one quota window is filling.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Forecast {
    /// The agent.
    pub agent: AgentKind,
    /// The account.
    pub account: String,
    /// The window, such as "5 hours" or "week".
    pub window: String,
    /// Percent used now.
    pub used_percent: f64,
    /// When it resets, Unix ms.
    pub resets_at: Option<i64>,
    /// Percent per hour lately, when it can be told.
    pub per_hour: Option<f64>,
    /// When it would be full at that pace, if before the reset.
    pub full_at: Option<i64>,
    /// Where it would be at the reset at that pace.
    pub at_reset: Option<f64>,
}

/// Everything `hashi stats` and the My usage popup show.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Whether tracking is on.
    pub tracking: bool,
    /// Days covered.
    pub days: u32,
    /// Start of the period, Unix ms.
    pub since: i64,
    /// Totals for the period.
    pub total: OwnTotals,
    /// Per UTC day.
    pub by_day: Vec<OwnTotals>,
    /// Per account.
    pub by_account: Vec<OwnTotals>,
    /// Per model.
    pub by_model: Vec<OwnTotals>,
    /// Per project folder.
    pub by_project: Vec<OwnTotals>,
    /// With permission prompts (`ask`) and without (`skip`).
    pub by_prompts: Vec<OwnTotals>,
    /// Own use next to shared use, per Claude Code account.
    pub versus: Vec<Versus>,
    /// Quota windows and how fast they fill.
    pub forecasts: Vec<Forecast>,
}

/// The pace of one window from its readings: the change between the first
/// and last reading of the current window within the last `lookback`.
#[must_use]
pub fn forecast_window(
    points: &[crate::store::QuotaPoint],
    used_now: f64,
    resets_at: Option<i64>,
    now: i64,
) -> (Option<f64>, Option<i64>, Option<f64>) {
    let same: Vec<_> = points.iter().filter(|p| p.resets_at == resets_at).collect();
    let (Some(first), Some(last)) = (same.first(), same.last()) else {
        return (None, None, None);
    };
    let span = last.observed_at - first.observed_at;
    if span < 10 * 60_000 {
        return (None, None, None);
    }
    #[allow(clippy::cast_precision_loss)]
    let per_hour = (last.used_percent - first.used_percent) / (span as f64 / 3_600_000.0);
    if per_hour <= 0.0 {
        return (Some(0.0), None, Some(used_now));
    }
    #[allow(clippy::cast_possible_truncation)]
    let full_at = now + ((100.0 - used_now).max(0.0) / per_hour * 3_600_000.0) as i64;
    let at_reset = resets_at.map(|r| {
        #[allow(clippy::cast_precision_loss)]
        let hours = (r - now).max(0) as f64 / 3_600_000.0;
        (used_now + per_hour * hours).min(100.0)
    });
    let full_at = match resets_at {
        Some(r) if full_at >= r => None,
        _ => Some(full_at),
    };
    (Some(per_hour), full_at, at_reset)
}

/// Builds the report for the last `days` days (1 to 365).
///
/// # Errors
///
/// Fails on database errors.
pub fn report(store: &Store, home: &Path, days: u32) -> Result<Report, String> {
    let days = days.clamp(1, 365);
    let now = now_ms();
    let since = crate::util::day_start_utc(now) - i64::from(days - 1) * 86_400_000;
    let e = |e: crate::store::StoreError| e.to_string();
    let totals = |by| store.own_totals(since, by).map_err(e);
    let total = totals(GroupBy::All)?.into_iter().next().unwrap_or_default();
    let by_account = totals(GroupBy::Account)?;

    let shared = store.shared_totals(AgentKind::Claude, since).map_err(e)?;
    let mut names: Vec<String> = by_account.iter().map(|t| t.key.clone()).collect();
    for s in &shared {
        if !names.contains(&s.account) {
            names.push(s.account.clone());
        }
    }
    let versus = names
        .into_iter()
        .map(|name| Versus {
            own: by_account
                .iter()
                .find(|t| t.key == name)
                .cloned()
                .unwrap_or_else(|| OwnTotals {
                    key: name.clone(),
                    ..OwnTotals::default()
                }),
            shared: shared
                .iter()
                .find(|s| s.account == name)
                .cloned()
                .unwrap_or_else(|| SharedTotals {
                    account: name.clone(),
                    ..SharedTotals::default()
                }),
            account: name,
        })
        .collect();

    let mut forecasts = Vec::new();
    for (agent, account) in store.usage_accounts().map_err(e)? {
        if !matches!(agent, AgentKind::Claude | AgentKind::Codex) {
            continue;
        }
        for w in store.usage_windows(agent, &account).map_err(e)? {
            if w.resets_at.is_some_and(|r| r <= now) {
                continue;
            }
            let points = store
                .quota_history(agent, &account, &w.key, now - 6 * 3_600_000)
                .map_err(e)?;
            let (per_hour, full_at, at_reset) =
                forecast_window(&points, w.used_percent, w.resets_at, now);
            forecasts.push(Forecast {
                agent,
                account: account.clone(),
                window: w.label.clone(),
                used_percent: w.used_percent,
                resets_at: w.resets_at,
                per_hour,
                full_at,
                at_reset,
            });
        }
    }

    Ok(Report {
        tracking: enabled(home),
        days,
        since,
        total,
        by_day: totals(GroupBy::Day)?,
        by_account,
        by_model: totals(GroupBy::Model)?,
        by_project: totals(GroupBy::Project)?,
        by_prompts: totals(GroupBy::SkipPerms)?,
        versus,
        forecasts,
    })
}

// ── text ───────────────────────────────────────────────────────────────────

/// `1.2M`, `34k` or `512`.
#[must_use]
pub fn tokens(n: i64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    if n >= 1_000_000_000 {
        format!("{:.1}B", f / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", f / 1e6)
    } else if n >= 1_000 {
        format!("{:.0}k", f / 1e3)
    } else {
        n.to_string()
    }
}

/// `$3.40`.
#[must_use]
pub fn usd(v: f64) -> String {
    if v >= 100.0 {
        format!("${v:.0}")
    } else {
        format!("${v:.2}")
    }
}

/// A short time, `Thu 14:00 UTC`, or a date when more than a week away.
#[must_use]
pub fn when(ms: i64) -> String {
    let Ok(t) = time::OffsetDateTime::from_unix_timestamp(ms / 1000) else {
        return String::new();
    };
    let fmt = if (ms - now_ms()).abs() < 6 * 86_400_000 {
        time::macros::format_description!("[weekday repr:short] [hour]:[minute] UTC")
    } else {
        time::macros::format_description!("[year]-[month]-[day]")
    };
    t.format(&fmt).unwrap_or_default()
}

/// One line about a forecast, such as "62% used, about 4%/h: full Thu
/// 14:00 UTC, before it resets".
#[must_use]
pub fn forecast_line(f: &Forecast) -> String {
    let mut s = format!("{:.0}% used", f.used_percent);
    match (f.per_hour, f.full_at, f.at_reset) {
        (Some(rate), Some(full), _) if rate > 0.0 => {
            s.push_str(&format!(
                ", {rate:.1}%/h: full {}, before it resets",
                when(full)
            ));
        }
        (Some(rate), None, Some(end)) if rate > 0.0 => {
            s.push_str(&format!(", {rate:.1}%/h: about {end:.0}% when it resets"));
        }
        (Some(_), _, _) => s.push_str(", not moving lately"),
        (None, _, _) => s.push_str(", pace not known yet"),
    }
    if let Some(r) = f.resets_at {
        s.push_str(&format!(" (resets {})", when(r)));
    }
    s
}

/// The report as plain text, for `hashi stats`.
#[must_use]
pub fn render(r: &Report) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "Your own Claude Code use, last {} day{} (tracking {}):",
        r.days,
        if r.days == 1 { "" } else { "s" },
        if r.tracking { "on" } else { "off" }
    );
    if r.total.messages == 0 {
        let _ = writeln!(
            s,
            "  nothing recorded yet{}",
            if r.tracking {
                ""
            } else {
                ": turn it on with `hashi stats on`"
            }
        );
    } else {
        let t = &r.total;
        let _ = writeln!(
            s,
            "  {} est. at list prices  {} tokens ({} out, {} from cache)  {} sessions, {} messages",
            usd(t.cost_usd),
            tokens(t.tokens()),
            tokens(t.output),
            tokens(t.cache_read),
            t.sessions,
            t.messages
        );
        let section = |s: &mut String, title: &str, rows: &[OwnTotals], max: usize| {
            if rows.is_empty() {
                return;
            }
            let _ = writeln!(s, "\n{title}");
            for row in rows.iter().take(max) {
                let _ = writeln!(
                    s,
                    "  {:<24} {:>9}  {:>8} tokens  {:>4} sessions",
                    crate::util::truncate(&row.key, 24),
                    usd(row.cost_usd),
                    tokens(row.tokens()),
                    row.sessions
                );
            }
        };
        section(&mut s, "By day", &r.by_day, 31);
        section(&mut s, "By account", &r.by_account, 10);
        section(&mut s, "By model", &r.by_model, 10);
        section(&mut s, "Top projects", &r.by_project, 10);
        section(
            &mut s,
            "Permission prompts (skip: without prompts, as dclaude runs)",
            &r.by_prompts,
            2,
        );
    }
    if r.versus.iter().any(|v| v.shared.requests > 0) {
        let _ = writeln!(s, "\nYou vs shared (Claude Code, same period)");
        for v in &r.versus {
            let _ = writeln!(
                s,
                "  {:<16} you {:>9}  shared {:>9} in {} request{}{}",
                crate::util::truncate(&v.account, 16),
                usd(v.own.cost_usd),
                usd(v.shared.cost_usd),
                v.shared.requests,
                if v.shared.requests == 1 { "" } else { "s" },
                v.own_share()
                    .map(|p| format!("  ({p:.0}% yours)"))
                    .unwrap_or_default()
            );
        }
    }
    if !r.forecasts.is_empty() {
        let _ = writeln!(s, "\nQuota burn");
        for f in &r.forecasts {
            let _ = writeln!(
                s,
                "  {:<22} {}",
                format!("{}:{} {}", f.agent, f.account, f.window),
                forecast_line(f)
            );
        }
    }
    let _ = writeln!(
        s,
        "\nCosts are estimates at API list prices; on a subscription you pay the plan, not this."
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::QuotaPoint;

    #[test]
    fn prices_match_what_claude_code_reported() {
        // A real session: Claude Code's own cost-state said $11.2028.
        let u = Usage {
            input_tokens: 7615,
            output_tokens: 117_247,
            cache_read_input_tokens: 31_950_906,
            cache_creation_input_tokens: 357_298,
            cache_creation: Some(CacheCreation {
                ephemeral_5m_input_tokens: 140_402,
                ephemeral_1h_input_tokens: 216_896,
            }),
        };
        let c = cost("claude-opus-5-5", &u);
        assert!((c - 11.2028).abs() < 0.01, "{c}");
        assert!(
            price("claude-sonnet-9-9").input > 0.0,
            "unknown models still priced"
        );
    }

    #[test]
    fn forecasts_from_the_pace_of_the_current_window() {
        let h = 3_600_000;
        let reset = 10 * h;
        let p = |pct, at| QuotaPoint {
            used_percent: pct,
            resets_at: Some(reset),
            observed_at: at,
        };
        // 10%/h from 40% at hour 2: full at hour 8, before the reset at 10.
        let pts = [p(30.0, h), p(40.0, 2 * h)];
        let (rate, full, _) = forecast_window(&pts, 40.0, Some(reset), 2 * h);
        assert!((rate.unwrap() - 10.0).abs() < 1e-9);
        assert_eq!(full, Some(8 * h));
        // Slower: ends at 60% when it resets.
        let pts = [p(30.0, h), p(32.0, 2 * h)];
        let (_, full, end) = forecast_window(&pts, 32.0, Some(reset), 2 * h);
        assert_eq!(full, None);
        assert!((end.unwrap() - 48.0).abs() < 1e-9);
        // Too few readings.
        assert_eq!(forecast_window(&pts[..1], 32.0, Some(reset), 2 * h).0, None);
    }

    fn line(v: serde_json::Value) -> String {
        format!("{v}\n")
    }

    #[test]
    fn reads_only_numbers_attributes_accounts_and_skips_headless_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("hashi");
        std::fs::create_dir_all(&home).unwrap();
        let store = Store::open(&home.join("hashi.db")).unwrap();
        let user = tmp.path().join("u");
        let live = acct::switch::Live {
            claude_dir: user.join(".claude"),
            claude_json: user.join(".claude.json"),
            codex_home: user.join(".codex"),
            gemini_dir: user.join(".gemini"),
            opencode_dir: user.join(".local/share/opencode"),
            env_override: std::collections::BTreeMap::new(),
        };
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            &live.claude_json,
            r#"{"oauthAccount":{"accountUuid":"me-uuid"}}"#,
        )
        .unwrap();
        // A Hashi account "work" whose identity is "work-uuid".
        store
            .add_account(AgentKind::Claude, "work", AccountKind::Login)
            .unwrap();
        let work = acct::create_dir(&home, AgentKind::Claude, "work").unwrap();
        std::fs::write(
            work.join(".claude.json"),
            r#"{"oauthAccount":{"accountUuid":"work-uuid"}}"#,
        )
        .unwrap();

        let proj = live.claude_dir.join("projects").join("-x-myapp");
        std::fs::create_dir_all(&proj).unwrap();
        let msg = |id: &str, ts: &str| {
            line(serde_json::json!({
                "type": "assistant", "entrypoint": "cli", "cwd": "/x/myapp", "sessionId": "s1",
                "requestId": format!("r-{id}"), "timestamp": ts,
                "message": {"id": id, "model": "claude-opus-5-5",
                    "content": [{"type": "text", "text": "SECRET TEXT"}],
                    "usage": {"input_tokens": 10, "output_tokens": 1000,
                              "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}
            }))
        };
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let mine = format!(
            "{}{}{}{}",
            line(serde_json::json!({"type": "session-owner", "accountUuid": "me-uuid"})),
            msg("m1", &now),
            // The same message written twice counts once.
            msg("m1", &now),
            line(serde_json::json!({"type": "user", "message": {"content": "SECRET PROMPT"}})),
        );
        std::fs::write(proj.join("s1.jsonl"), &mine).unwrap();
        // A session switched to work, without prompts.
        let theirs = format!(
            "{}{}{}",
            line(serde_json::json!({"type": "session-owner", "accountUuid": "work-uuid"})),
            line(
                serde_json::json!({"type": "permission-mode", "permissionMode": "bypassPermissions"})
            ),
            msg("m2", &now),
        );
        std::fs::write(proj.join("s2.jsonl"), &theirs).unwrap();
        // A headless run, left out.
        let sdk = line(serde_json::json!({
            "type": "assistant", "entrypoint": "sdk-cli", "timestamp": now,
            "message": {"id": "m3", "model": "claude-opus-5-5", "usage": {"output_tokens": 5}}
        }));
        std::fs::write(proj.join("s3.jsonl"), &sdk).unwrap();
        // A line still being written (no newline yet) waits.
        let partial = msg("m4", &now);
        std::fs::write(proj.join("s4.jsonl"), partial.trim_end()).unwrap();

        let r = scan_with(&store, &home, Some(&live)).unwrap();
        assert_eq!(r.added, 2, "{r:?}");
        // Nothing new on a second pass.
        assert_eq!(scan_with(&store, &home, Some(&live)).unwrap().added, 0);
        // The partial line completes.
        std::fs::write(proj.join("s4.jsonl"), &partial).unwrap();
        assert_eq!(scan_with(&store, &home, Some(&live)).unwrap().added, 1);

        let rep = report(&store, &home, 7).unwrap();
        assert_eq!(rep.total.messages, 3);
        let acc: Vec<_> = rep.by_account.iter().map(|t| t.key.as_str()).collect();
        assert!(acc.contains(&"default") && acc.contains(&"work"), "{acc:?}");
        assert_eq!(rep.by_project[0].key, "myapp");
        assert!(
            rep.by_prompts
                .iter()
                .any(|t| t.key == "skip" && t.messages == 1)
        );
        // No text was kept anywhere.
        let text = render(&rep);
        assert!(!text.contains("SECRET"));
        assert_eq!(store.forget_own_usage().unwrap(), 3);
    }
}
