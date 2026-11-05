//! Claude Code's live quota: `GET /api/oauth/usage` with the account's own
//! OAuth access token, read without changing anything.
//!
//! Where the token lives:
//!
//! - **macOS**: the Keychain item whose service is
//!   `Claude Code-credentials`, plus `-` and the first 8 hex characters of
//!   `sha256(NFC(config dir))` for any config dir but the default, under
//!   the account `$USER`. It is read with `security find-generic-password
//!   -a <user> -w -s <service>`, which prints the item and changes nothing.
//! - **elsewhere** (and on macOS when the Keychain has no item):
//!   `<config dir>/.credentials.json`.
//!
//! Both hold `{"claudeAiOauth": {"accessToken", "expiresAt" (ms), ...}}`.
//! Hashi never refreshes the token: one that has expired, or will within
//! [`EXPIRY_MARGIN`], makes the account "unknown, refreshes on its next
//! run", since Claude Code renews it itself when it next runs. The token is
//! held only for the one request, never logged, stored or returned.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use super::{Source, Window, clamp_percent};

/// The usage endpoint's host, overridden in tests.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The usage endpoint's path.
pub const USAGE_PATH: &str = "/api/oauth/usage";

/// The beta header the endpoint needs.
pub const BETA: &str = "oauth-2025-04-20";

/// How long the endpoint may take.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// A token that expires sooner than this is treated as expired.
pub const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

/// Wait after a 429 or 403 without `retry-after`.
pub const DEFAULT_RETRY: Duration = Duration::from_secs(5 * 60);

/// The longest `retry-after` honoured.
pub const MAX_RETRY: Duration = Duration::from_secs(3_600);

/// Wait after the endpoint rejects the token.
pub const AUTH_RETRY: Duration = Duration::from_secs(3_600);

/// What an account whose token cannot be used now shows.
pub const REFRESHES_ON_RUN: &str = "unknown, refreshes on its next run";

/// The Keychain service prefix Claude Code uses.
pub const SERVICE: &str = "Claude Code-credentials";

/// The Keychain service for an account whose config dir is `config_dir`,
/// `None` for the default (no `CLAUDE_CONFIG_DIR`).
#[must_use]
pub fn service_name(config_dir: Option<&str>) -> String {
    match config_dir {
        None => SERVICE.to_owned(),
        Some(dir) => {
            let nfc: String = dir.nfc().collect();
            let digest = Sha256::digest(nfc.as_bytes());
            let hex = hex::encode(digest);
            format!("{SERVICE}-{}", &hex[..8])
        }
    }
}

/// An access token. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    /// The token, for the `Authorization` header only.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

/// A usable token and the plan it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    /// The access token.
    pub token: AccessToken,
    /// When it expires, Unix ms.
    pub expires_at: Option<i64>,
    /// The plan, such as `max` or `pro`, when the file says.
    pub plan: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialsFile {
    claude_ai_oauth: Option<OauthEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OauthEntry {
    access_token: Option<String>,
    expires_at: Option<i64>,
    subscription_type: Option<String>,
}

/// Why no token could be used. The message never contains a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// Nothing stored: the account is signed out, or uses an API key.
    Missing,
    /// Stored but expired or about to: Claude Code renews it on its next run.
    Expired,
    /// Stored but unreadable.
    Unreadable,
}

impl TokenError {
    /// What the account shows.
    #[must_use]
    pub const fn note(&self) -> &'static str {
        match self {
            Self::Missing => "unknown, no Claude sign-in found for this account",
            Self::Expired => REFRESHES_ON_RUN,
            Self::Unreadable => "unknown, the stored Claude sign-in could not be read",
        }
    }
}

/// Parses the credentials JSON, checking expiry against `now` (Unix ms).
///
/// # Errors
///
/// As [`TokenError`] says.
pub fn parse_credentials(text: &str, now: i64) -> Result<Credentials, TokenError> {
    let file: CredentialsFile =
        serde_json::from_str(text.trim()).map_err(|_| TokenError::Unreadable)?;
    let entry = file.claude_ai_oauth.ok_or(TokenError::Missing)?;
    let token = entry
        .access_token
        .filter(|t| !t.trim().is_empty())
        .ok_or(TokenError::Missing)?;
    let margin = i64::try_from(EXPIRY_MARGIN.as_millis()).unwrap_or(0);
    if entry.expires_at.is_some_and(|e| e <= now + margin) {
        return Err(TokenError::Expired);
    }
    Ok(Credentials {
        token: AccessToken(token.trim().to_owned()),
        expires_at: entry.expires_at,
        plan: entry
            .subscription_type
            .filter(|p| crate::agent::is_safe_arg(p)),
    })
}

/// Where to look for an account's token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSource {
    /// The config dir string exactly as runs pass it in `CLAUDE_CONFIG_DIR`,
    /// or `None` for the default.
    pub config_dir: Option<String>,
    /// The `security` executable, when the Keychain is used.
    pub security: Option<PathBuf>,
    /// The Keychain account name (`$USER`).
    pub user: Option<String>,
    /// The directory holding `.credentials.json` when there is no Keychain
    /// item: the config dir, or `~/.claude` for the default.
    pub file_dir: Option<PathBuf>,
}

/// Reads the account's token without changing anything.
///
/// # Errors
///
/// As [`TokenError`] says.
pub async fn read_token(src: &TokenSource, now: i64) -> Result<Credentials, TokenError> {
    if let (Some(security), Some(user)) = (&src.security, &src.user)
        && let Some(text) = keychain(security, user, &service_name(src.config_dir.as_deref())).await
    {
        return parse_credentials(&text, now);
    }
    let Some(dir) = &src.file_dir else {
        return Err(TokenError::Missing);
    };
    let path = dir.join(".credentials.json");
    // Not following a link swapped in for it.
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return Err(TokenError::Missing);
    }
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| TokenError::Unreadable)?;
    parse_credentials(&text, now)
}

/// The Keychain item's text, or `None` when there is none or it cannot be
/// read. Runs `security find-generic-password -a <user> -w -s <service>`.
async fn keychain(security: &Path, user: &str, service: &str) -> Option<String> {
    let fut = tokio::process::Command::new(security)
        .args(["find-generic-password", "-a", user, "-w", "-s", service])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    (!text.trim().is_empty()).then_some(text)
}

/// What the endpoint said.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// The windows.
    Usage(Vec<Window>),
    /// Rate limited or forbidden: try again after this long.
    Busy(Duration),
    /// The token was rejected.
    Rejected,
    /// Anything else, with the HTTP status.
    Failed(String),
}

/// Calls the usage endpoint at `base` with `token`. Blocking; run it off
/// the async threads.
#[must_use]
pub fn fetch(base: &str, token: &AccessToken) -> Reply {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent(format!("hashi/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let url = format!("{}{USAGE_PATH}", base.trim_end_matches('/'));
    let res = agent
        .get(&url)
        .header("Authorization", &format!("Bearer {}", token.expose()))
        .header("anthropic-beta", BETA)
        .header("Accept", "application/json")
        .call();
    let mut res = match res {
        Ok(r) => r,
        // The error names the host at most, never the header.
        Err(e) => return Reply::Failed(format!("the usage endpoint could not be reached: {e}")),
    };
    let status = res.status().as_u16();
    match status {
        200 => {}
        401 => return Reply::Rejected,
        403 | 429 => {
            let after = res
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map_or(DEFAULT_RETRY, Duration::from_secs);
            return Reply::Busy(after.min(MAX_RETRY));
        }
        other => return Reply::Failed(format!("the usage endpoint answered HTTP {other}")),
    }
    match res.body_mut().read_to_string() {
        Ok(text) => match parse_usage(&text) {
            Some(windows) => Reply::Usage(windows),
            None => Reply::Failed("the usage endpoint answered something unreadable".to_owned()),
        },
        Err(e) => Reply::Failed(format!(
            "the usage endpoint's answer could not be read: {e}"
        )),
    }
}

#[derive(Deserialize)]
struct RawWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct RawLimit {
    kind: Option<String>,
    scope: Option<serde_json::Value>,
    percent: Option<f64>,
    resets_at: Option<String>,
}

/// The fixed windows the endpoint returns, with their labels and lengths.
const WINDOWS: [(&str, &str, i64); 4] = [
    ("five_hour", "5 hours", 300),
    ("seven_day", "week", 10_080),
    ("seven_day_opus", "week, Opus", 10_080),
    ("seven_day_sonnet", "week, Sonnet", 10_080),
];

/// An RFC 3339 time as Unix ms.
fn parse_time(s: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(s.trim(), &time::format_description::well_known::Rfc3339)
        .ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

/// Parses the endpoint's JSON into windows: `five_hour`, `seven_day`,
/// `seven_day_opus` and `seven_day_sonnet` (each `{utilization 0 to 100,
/// resets_at}`), and every `limits[]` entry of kind `weekly_scoped`
/// (`{scope.model.display_name, percent, resets_at}`). `None` when it is
/// not a JSON object.
#[must_use]
pub fn parse_usage(text: &str) -> Option<Vec<Window>> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = v.as_object()?;
    let mut out = Vec::new();
    for (key, label, mins) in WINDOWS {
        let Some(raw) = obj
            .get(key)
            .filter(|w| !w.is_null())
            .and_then(|w| serde_json::from_value::<RawWindow>(w.clone()).ok())
        else {
            continue;
        };
        let Some(used) = raw.utilization else {
            continue;
        };
        out.push(Window::new(
            key,
            label,
            clamp_percent(used),
            raw.resets_at.as_deref().and_then(parse_time),
            Some(mins),
            Source::Poll,
        ));
    }
    let limits = obj.get("limits").and_then(|l| l.as_array());
    for raw in limits.into_iter().flatten() {
        let Ok(l) = serde_json::from_value::<RawLimit>(raw.clone()) else {
            continue;
        };
        if l.kind.as_deref() != Some("weekly_scoped") {
            continue;
        }
        let Some(used) = l.percent else {
            continue;
        };
        let name = l
            .scope
            .as_ref()
            .and_then(|s| s.pointer("/model/display_name"))
            .and_then(|n| n.as_str())
            .map(|n| crate::util::truncate(n.trim(), 40))
            .filter(|n| !n.is_empty() && !n.chars().any(char::is_control));
        let Some(name) = name else {
            continue;
        };
        let key = format!(
            "weekly_{}",
            name.to_ascii_lowercase()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect::<String>()
        );
        // The fixed windows already cover these.
        if out
            .iter()
            .any(|w| w.key == key || w.label == format!("week, {name}"))
        {
            continue;
        }
        out.push(Window::new(
            key,
            format!("week, {name}"),
            clamp_percent(used),
            l.resets_at.as_deref().and_then(parse_time),
            Some(10_080),
            Source::Poll,
        ));
    }
    Some(out)
}

/// A `rateLimitType` from a run's `rate_limit_event`, as a window key,
/// label and length.
#[must_use]
pub fn event_window(kind: &str) -> Option<(&'static str, &'static str, i64)> {
    WINDOWS
        .iter()
        .copied()
        .find(|(k, _, _)| *k == kind)
        .or(match kind {
            "seven_day_overage_included" => Some(("seven_day", "week", 10_080)),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names_follow_the_config_dir() {
        assert_eq!(service_name(None), "Claude Code-credentials");
        // The FIPS 180-2 test vector: sha256("abc") = ba7816bf...
        assert_eq!(
            service_name(Some("abc")),
            "Claude Code-credentials-ba7816bf"
        );
        let s = service_name(Some("/tmp/x"));
        assert!(s.starts_with("Claude Code-credentials-"), "{s}");
        assert_eq!(s.len(), SERVICE.len() + 9);
        let hex = hex::encode(Sha256::digest(b"/tmp/x"));
        assert_eq!(&s[SERVICE.len() + 1..], &hex[..8]);
        // NFC: a decomposed e-acute hashes like the composed one.
        assert_eq!(
            service_name(Some("/Users/re\u{301}my/.hashi")),
            service_name(Some("/Users/r\u{e9}my/.hashi"))
        );
        assert_ne!(service_name(Some("/a")), service_name(Some("/b")));
    }

    #[test]
    fn credentials_are_read_and_expiry_is_respected() {
        let now = 1_800_000_000_000;
        let ok = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat01-secret","refreshToken":"r","expiresAt":{},"subscriptionType":"max"}}}}"#,
            now + 3_600_000
        );
        let c = parse_credentials(&ok, now).unwrap();
        assert_eq!(c.token.expose(), "sk-ant-oat01-secret");
        assert_eq!(c.plan.as_deref(), Some("max"));
        assert!(!format!("{c:?}").contains("secret"));
        let soon = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"t","expiresAt":{}}}}}"#,
            now + 30_000
        );
        assert_eq!(parse_credentials(&soon, now), Err(TokenError::Expired));
        assert_eq!(TokenError::Expired.note(), REFRESHES_ON_RUN);
        assert_eq!(parse_credentials("{}", now), Err(TokenError::Missing));
        assert_eq!(parse_credentials("nope", now), Err(TokenError::Unreadable));
        let blank = r#"{"claudeAiOauth":{"accessToken":"  "}}"#;
        assert_eq!(parse_credentials(blank, now), Err(TokenError::Missing));
    }

    #[test]
    fn usage_json_becomes_windows() {
        let text = r#"{
            "five_hour": {"utilization": 42.5, "resets_at": "2027-01-15T10:00:00+00:00"},
            "seven_day": {"utilization": 80, "resets_at": "2027-01-20T00:00:00Z"},
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": null, "resets_at": null},
            "limits": [
                {"kind": "weekly_scoped", "scope": {"model": {"display_name": "Fable"}}, "percent": 12, "resets_at": "2027-01-21T00:00:00Z"},
                {"kind": "other", "percent": 99}
            ]
        }"#;
        let w = parse_usage(text).unwrap();
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(
            (w[0].key.as_str(), w[0].label.as_str()),
            ("five_hour", "5 hours")
        );
        assert!((w[0].used_percent - 42.5).abs() < f64::EPSILON);
        assert_eq!(w[0].resets_at, Some(1_800_000_000_000 + 2 * 3_600_000));
        assert_eq!(w[1].label, "week");
        assert!(w[1].is_weekly());
        assert_eq!(w[2].label, "week, Fable");
        assert_eq!(w[2].key, "weekly_fable");
        assert!(w.iter().all(|x| x.source == Source::Poll));
        assert!(parse_usage("[]").is_none());
        assert_eq!(parse_usage("{}").unwrap(), Vec::new());
    }

    #[test]
    fn event_kinds_map_to_windows() {
        assert_eq!(
            event_window("five_hour"),
            Some(("five_hour", "5 hours", 300))
        );
        assert_eq!(event_window("seven_day_opus").unwrap().1, "week, Opus");
        assert_eq!(event_window("overage"), None);
    }

    #[tokio::test]
    async fn a_missing_file_and_no_keychain_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let src = TokenSource {
            config_dir: Some(dir.path().display().to_string()),
            security: None,
            user: None,
            file_dir: Some(dir.path().to_path_buf()),
        };
        assert_eq!(read_token(&src, 0).await, Err(TokenError::Missing));
        std::fs::write(
            dir.path().join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"file-token","expiresAt":99999999999999}}"#,
        )
        .unwrap();
        assert_eq!(
            read_token(&src, 0).await.unwrap().token.expose(),
            "file-token"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_keychain_is_read_with_a_fake_security() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls");
        let fake = dir.path().join("security");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nprintf '%s' '{{\"claudeAiOauth\":{{\"accessToken\":\"kc-token\",\"expiresAt\":99999999999999}}}}'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let src = TokenSource {
            config_dir: Some("/srv/hashi/accounts/claude/work".to_owned()),
            security: Some(fake),
            user: Some("owner".to_owned()),
            file_dir: None,
        };
        let c = read_token(&src, 0).await.unwrap();
        assert_eq!(c.token.expose(), "kc-token");
        let calls = std::fs::read_to_string(&log).unwrap();
        let service = service_name(Some("/srv/hashi/accounts/claude/work"));
        assert_eq!(
            calls.trim(),
            format!("find-generic-password -a owner -w -s {service}")
        );
    }
}
