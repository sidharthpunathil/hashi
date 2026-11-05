//! API token generation, hashing and policy types.
//!
//! Raw tokens are shown exactly once at creation. Only their SHA-256 digest is
//! persisted, so a leaked database does not leak usable credentials.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent::AgentKind;

/// Prefix that makes Hashi tokens easy to recognise in logs and secret scanners.
pub const TOKEN_PREFIX: &str = "hashi_";

/// Number of leading characters stored in clear text for identification.
pub const DISPLAY_PREFIX_LEN: usize = 14;

/// A freshly generated token. `raw` must be shown to the user once and then dropped.
#[derive(Clone)]
pub struct GeneratedToken {
    /// The bearer secret.
    pub raw: String,
    /// Hex SHA-256 digest of `raw`.
    pub hash: String,
    /// Non-secret leading slice of `raw`, for display.
    pub prefix: String,
}

impl fmt::Debug for GeneratedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedToken")
            .field("raw", &"<redacted>")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

/// Generates a new token from 32 bytes of OS-seeded randomness.
#[must_use]
pub fn generate() -> GeneratedToken {
    let bytes: [u8; 32] = rand::random();
    let raw = format!("{TOKEN_PREFIX}{}", hex::encode(bytes));
    let hash = hash(&raw);
    let prefix = raw.chars().take(DISPLAY_PREFIX_LEN).collect();
    GeneratedToken { raw, hash, prefix }
}

/// Hex SHA-256 digest of a raw token.
#[must_use]
pub fn hash(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

/// What a token is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Full control, including token management.
    Admin,
    /// May submit prompts and read its own results.
    User,
}

/// Whether consecutive prompts share an agent conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    /// Every prompt starts a fresh conversation.
    #[default]
    Stateless,
    /// Prompts continue the token's previous conversation with that agent.
    Stateful,
}

macro_rules! str_enum {
    ($ty:ty { $($variant:ident => $s:literal),+ $(,)? }) => {
        impl $ty {
            /// Stable lowercase identifier.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }
        }
        impl FromStr for $ty {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s.trim().to_ascii_lowercase().as_str() {
                    $($s => Ok(Self::$variant),)+
                    other => Err(format!("invalid value `{other}`")),
                }
            }
        }
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum!(Role { Admin => "admin", User => "user" });
str_enum!(SessionMode { Stateless => "stateless", Stateful => "stateful" });

/// Usage caps for one token. `None` means unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Limits {
    /// Prompts per UTC day.
    #[serde(default)]
    pub daily_requests: Option<u32>,
    /// Prompts per UTC month.
    #[serde(default)]
    pub monthly_requests: Option<u32>,
    /// Estimated spend per UTC day, as reported by the agent (Claude only).
    #[serde(default)]
    pub daily_budget_usd: Option<f64>,
    /// Estimated spend per UTC month, as reported by the agent (Claude only).
    #[serde(default)]
    pub monthly_budget_usd: Option<f64>,
    /// Prompts queued or running at once.
    #[serde(default)]
    pub max_pending: Option<u32>,
}

impl Limits {
    /// `true` when every budget is non-negative and finite.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        [self.daily_budget_usd, self.monthly_budget_usd]
            .into_iter()
            .flatten()
            .all(|b| b.is_finite() && b >= 0.0)
    }
}

/// Whether a token can be used right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenStatus {
    /// Usable.
    Active,
    /// Temporarily switched off by an admin.
    Paused,
    /// Past its expiry time.
    Expired,
}

impl TokenStatus {
    /// Stable lowercase identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Expired => "expired",
        }
    }
}

/// A stored token record. Never contains the raw secret.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    /// Stable identifier.
    pub id: String,
    /// Human label.
    pub name: String,
    /// Non-secret display prefix.
    pub prefix: String,
    /// Permission level.
    pub role: Role,
    /// Conversation behaviour.
    pub session_mode: SessionMode,
    /// Agents this token may use.
    pub agents: Vec<AgentKind>,
    /// Expiry, in Unix milliseconds.
    pub expires_at: Option<i64>,
    /// Creation time, in Unix milliseconds.
    pub created_at: i64,
    /// Last successful authentication, in Unix milliseconds.
    pub last_used_at: Option<i64>,
    /// Temporarily switched off.
    pub paused: bool,
    /// Who or what the token is for.
    pub note: Option<String>,
    /// Usage caps.
    pub limits: Limits,
    /// Accounts per agent, in order of preference. Only admins see these.
    pub accounts: crate::accounts::AccountMap,
}

impl Token {
    /// `true` for admin tokens.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    /// `true` if this token may drive `agent`.
    #[must_use]
    pub fn allows(&self, agent: AgentKind) -> bool {
        self.agents.contains(&agent)
    }

    /// `true` if the token expired before `now_ms`.
    #[must_use]
    pub fn is_expired(&self, now_ms: i64) -> bool {
        self.expires_at.is_some_and(|t| t <= now_ms)
    }

    /// Current status. Expiry wins over pause.
    #[must_use]
    pub fn status(&self, now_ms: i64) -> TokenStatus {
        if self.is_expired(now_ms) {
            TokenStatus::Expired
        } else if self.paused {
            TokenStatus::Paused
        } else {
            TokenStatus::Active
        }
    }
}

/// Encodes an agent list for storage: `claude,codex`.
#[must_use]
pub fn encode_agents(agents: &[AgentKind]) -> String {
    agents
        .iter()
        .map(|a| a.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Decodes a stored agent list, ignoring unknown entries.
#[must_use]
pub fn decode_agents(s: &str) -> Vec<AgentKind> {
    s.split(',').filter_map(|a| a.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_have_prefix_and_entropy() {
        let t = generate();
        assert!(t.raw.starts_with(TOKEN_PREFIX));
        assert_eq!(t.raw.len(), TOKEN_PREFIX.len() + 64);
        assert_ne!(t.raw, generate().raw);
    }

    #[test]
    fn hash_matches_generated_digest() {
        let t = generate();
        assert_eq!(hash(&t.raw), t.hash);
    }

    #[test]
    fn debug_output_redacts_secret() {
        let t = generate();
        assert!(!format!("{t:?}").contains(&t.raw));
    }

    #[test]
    fn agents_roundtrip_through_storage_encoding() {
        let agents = vec![AgentKind::Claude, AgentKind::Codex];
        assert_eq!(decode_agents(&encode_agents(&agents)), agents);
    }

    #[test]
    fn negative_budgets_are_invalid() {
        let l = Limits {
            daily_budget_usd: Some(-1.0),
            ..Limits::default()
        };
        assert!(!l.is_valid());
        assert!(
            Limits {
                monthly_budget_usd: Some(5.0),
                ..Limits::default()
            }
            .is_valid()
        );
    }

    #[test]
    fn role_parses_case_insensitively() {
        assert_eq!("ADMIN".parse::<Role>(), Ok(Role::Admin));
    }
}
