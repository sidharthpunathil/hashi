# Usage limits and account strategies: design

## Intent

With several accounts per agent, the owner wants to see how much of each
account's quota is left. They also want Hashi to choose accounts sensibly:
the one with the most left, or the one whose quota is about to reset unused,
moving off an account at X% before it hits the wall, and starting new
conversations on the best account.

## What each agent can tell us (research, fresh upstream sources)

| Agent | Live quota | Source | Limit signal with a reset time |
|---|---|---|---|
| Claude Code | yes | `GET https://api.anthropic.com/api/oauth/usage` (see below) | `stream-json` `rate_limit_event` with `status`, `rateLimitType`, `utilization` and `resetsAt` (Unix s) |
| Codex | yes | `codex app-server` over stdio, method `account/rateLimits/read` (stable, Codex refreshes its own token) | `turn.failed` "You’ve hit your usage limit… Try again at …" (local time; also re-read) |
| opencode | only for OpenCode Go or Console sign-ins | `GET /api/experimental/usage/limits` through `opencode api` (invocation unverified) | typed `provider.*-limit` errors, `retryAfterMs` |
| Gemini CLI | no (fetched internally, never printed headless) | none | "exhausted your daily quota", "Please retry in Ns" |
| Qwen Code | no (token counts only) | `~/.qwen/usage`-style token log inside the account's `QWEN_HOME` runtime dir | "The quota will reset at MM-DD HH:MM:SS UTC" |
| Cursor CLI | no headless path | none | stderr text plus exit 1, "monthly cycle ends on M/D/YYYY" |

### Claude details
- **Endpoint headers:** `Authorization: Bearer <accessToken>`,
  `anthropic-beta: oauth-2025-04-20`, a 5 s timeout.
- **Response windows:** `five_hour`, `seven_day`, `seven_day_opus`,
  `seven_day_sonnet`, each with `utilization` (0 to 100) and `resets_at`.
  `limits[]` entries are `{kind: "weekly_scoped", scope.model.display_name,
  percent, resets_at}`.
- **Back-off:** on 429 or 403, honour `retry-after` (default 5 minutes,
  at most 1 hour). After an auth rejection, wait 1 hour.
- **Reading the token, read-only:**
  - macOS: the Keychain item with service `Claude Code-credentials-<first 8
    hex of sha256(NFC(config dir string))>` and account `$USER`, read with
    `security find-generic-password -a <user> -w -s <service>`. For the
    default account, the service has no suffix.
  - Elsewhere: `<config dir>/.credentials.json`.
  - JSON shape: `claudeAiOauth.{accessToken, expiresAt (ms)}`.
  - **Never refresh.** When the token is expired or close to it, report the
    account as "unknown, refreshes on its next run".
- **Runs:** switch `claude -p` to `--output-format stream-json --verbose`
  and read the final `result` message for the answer. Keep the current
  parsing semantics, and add tests with recorded stream samples.
  - A `rate_limit_event` updates that account's usage.
  - A `rejected` status, or an assistant message with
    `error: "rate_limit"`, marks the account limited until `resetsAt`.
    Prefer this over text parsing.

## Goals

### 1. Usage records

- A store table keyed by agent, account and window. Each row holds the used
  percent (0 to 100), `resets_at`, the label (for example "5 hours", "week",
  "week, Opus", "Codex primary"), the source (`poll`, `run` or `error`) and
  the time it was observed.
- **Summary per account:**
  - *headroom*: 100 minus the highest used percent across windows;
  - *binding window*: the window with that highest used percent;
  - *next reset*;
  - *state*: `ok`, `warning` (at or above the rotate threshold), `limited`
    (until a time) or `unknown`.
- Accounts without live data show `unknown` headroom, plus runs and reported
  cost today. For Qwen, they also show tokens today from its token log.

### 2. Polling

- A background poller runs, for Claude and Codex accounts that are bound to
  at least one token or project, or are the agent's default, with enabled
  usage polling.
- **Cadence:** every 5 minutes per account with jitter, plus one poll after
  each finished run on that account. It is never more than once a minute,
  and backs off as above.
- **Codex:** one short-lived `codex app-server` per poll with that account's
  `CODEX_HOME`:
  1. send `initialize` and `initialized`;
  2. check that the returned `codexHome` is the account's directory;
  3. call `account/rateLimits/read` with `excludeResetCreditDetails: true`;
  4. exit.

  The call is skipped while a run on the same account is active, to avoid two
  token refreshes at once.
- **Claude:** the endpoint above, with the token read read-only.
- `--no-usage-poll` / `HASHI_USAGE_POLL=0` turns polling off. The data from
  runs and errors is still recorded.
- Polling never logs tokens, and errors are redacted.

### 3. Strategies

The strategy is set per project, with a server default `--account-strategy`.
The threshold is `--rotate-at <pct>` (default 90, 50 to 99), per project with
a server default.

- **`in-order`** (the default): the first available account in the list,
  failing over on limits. This is today's behaviour.
- **`most-left`**: among available accounts, the one with the most headroom.
  Unknown headroom ranks after any known account under the threshold, and
  ahead of accounts over it. List order breaks ties.
- **`soonest-reset`**: among accounts under the threshold, the one whose
  binding weekly window resets soonest, so quota that would expire unused
  is spent first. Otherwise it falls back to `most-left`.
- **Common rules:**
  - An account at or above the threshold is skipped while any account is
    under it.
  - When all accounts are over, the run goes to the one with the most
    headroom rather than stalling.
  - Limited and disabled accounts are never picked.

### 4. Sessions

- A stateful conversation stays on its account until that account reaches
  the threshold or is limited. Only then does the next message start a fresh
  conversation on the newly chosen account.
- A new conversation always starts on the strategy's choice.
- The dashboard and the admin API show which account each conversation is
  on.

### 5. Visibility

- **CLI:**
  - `hashi account usage [--agent a] [--refresh] [--json]` shows bars per
    window with the percent and reset time, the plan where known, the source
    and its age, and the state.
  - `hashi account list` shows the summary column.
- **API:**
  - `GET /v1/accounts` includes `usage` and `summary`.
  - `POST /v1/accounts/{agent}/{name}/refresh`, admin only, rate limited to
    once a minute.
  - Usage is never shown to non-admin tokens: `/v1/me`, MCP and OpenAI
    responses don't get it.
- **Dashboard:**
  - agent cards show the best account's headroom;
  - a usage panel (or a section in the Accounts view the parity builder
    makes) has gauges per account and window, with reset countdowns;
  - a toast and a log line appear when an account crosses the threshold, is
    limited, or recovers.
- **Bus events:** `AccountUsage`, published on change, which the dashboard
  and headless log use.

## Non-goals

- Polling Gemini, Qwen, Cursor or Crush quota through private endpoints.
- Spending tokens to measure quota, for example by sending test prompts.
- Refreshing any OAuth token.

## Anti-goals

- Logging, storing or returning access tokens.
- Tests that hit the network, real Keychain or real CLIs. Use fake
  `security`, fake `codex app-server` scripts, a local HTTP server for the
  Claude endpoint (the base URL is injectable), and recorded `stream-json`
  samples.
