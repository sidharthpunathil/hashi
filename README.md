<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.png">
    <img src="assets/logo-light.png" alt="Hashi the Bridge" width="560">
  </picture>
</p>

<p align="center">
  <strong>Call Claude Code and Codex over HTTP.</strong><br>
  Hashi runs on the machine where your agents are already signed in and gives every app, script and bot one authenticated API to reach them.
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#api">API</a> ·
  <a href="#the-dashboard">Dashboard</a> ·
  <a href="#security">Security</a> ·
  <a href="#configuration">Configuration</a>
</p>

<p align="center">
  <a href="https://github.com/sidharthpunathil/hashi/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/sidharthpunathil/hashi/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-7c4dff">
  <img alt="Rust 1.89+" src="https://img.shields.io/badge/rust-1.89%2B-a855f7">
</p>

---

*Hashi* (橋) is Japanese for bridge. You start it once, and from then on a single `curl` sends a prompt to Claude Code or Codex on your machine and gets the answer back as JSON. It uses the CLIs you already installed and logged into, so the work runs under your existing subscription.

```bash
curl http://127.0.0.1:7878/v1/ask \
  -H "Authorization: Bearer $HASHI_TOKEN" \
  -d '{"message": "Explain what src/main.rs does", "agent": "claude", "wait": 60}'
```

```json
{
  "id": "c761eb2f-27b8-41c6-a485-8a475d7cec21",
  "agent": "claude",
  "status": "completed",
  "response": "src/main.rs parses the CLI and starts the server...",
  "usage": { "inputTokens": 36371, "outputTokens": 412, "costUsd": 0.1985 },
  "durationMs": 5132
}
```

## What you get

| | |
|---|---|
| **Two agents, one API** | Send `"agent": "claude"` or `"agent": "codex"`. Each agent has its own worker queue, so a long Claude run never blocks a Codex prompt. |
| **Tokens you can hand out** | Create a token per app. Scope it to one agent, make it expire, revoke it from the dashboard. Only a SHA-256 hash is stored. |
| **Conversations that continue** | A `stateful` token resumes its own Claude or Codex session on every prompt. `DELETE /v1/session/{agent}` starts over. |
| **Answers in one request** | Pass `"wait": 60` to hold the request open until the agent finishes. Or take the `202`, then poll. |
| **Work survives restarts** | Requests live in SQLite. Anything running when Hashi stopped goes back in the queue on the next start. |
| **Share without sending a secret** | `hashi share alice` makes a one-time invite link. Alice opens it, sees exactly what she gets, and claims her own token. |
| **Limits you set** | Cap each token's prompts per day or month, its reported spend, and how many prompts it can have in flight. Pause and resume in one key. |
| **Use it from other agents** | Hashi speaks MCP at `/mcp`, so a teammate can add your Claude Code or Codex as a tool in their own Claude Code or Cursor. |
| **HTTPS your way** | A Cloudflare quick tunnel, a named tunnel on your own domain, or Hashi serving TLS itself with your certificate. All off until you ask. |
| **A live dashboard** | A terminal UI shows each agent's state, queue, latency, throughput, recent requests and tokens. |
| **One binary** | A single Rust executable with SQLite built in. It needs no runtime and no external database. |

## Install

**macOS and Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.sh | sh
```

**Windows** (PowerShell)

```powershell
irm https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.ps1 | iex
```

That's the whole install. The script picks the right build for your computer and checks its SHA-256 checksum before installing. It needs no admin rights. Hashi lands in `~/.local/bin` on macOS and Linux, or `%LOCALAPPDATA%\Programs\hashi\bin` on Windows, where it is added to your PATH. The script finishes by running `hashi doctor` to show which agents it found.

<details>
<summary>Other ways to install, and installer options</summary>

| Variable | Effect |
|---|---|
| `HASHI_VERSION=v0.1.1` | Install a specific release instead of the latest |
| `HASHI_INSTALL_DIR=/some/dir` | Install somewhere else |
| `HASHI_NO_DOCTOR=1` | Skip the `hashi doctor` check at the end |

```bash
# pin a version on macOS / Linux
curl -fsSL https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.sh | HASHI_VERSION=v0.1.1 sh
```

```powershell
# pin a version on Windows
$env:HASHI_VERSION = "v0.1.1"; irm https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.ps1 | iex
```

**Download by hand.** Every [release](https://github.com/sidharthpunathil/hashi/releases) has archives for Linux (x64, ARM64), macOS (Intel, Apple Silicon) and Windows (x64), each with a `.sha256` file.

**With Cargo.** `cargo install --locked --git https://github.com/sidharthpunathil/hashi`

**From source.** Clone the repo and run `cargo build --release`. The binary is `target/release/hashi`.

**Uninstall.** Delete the `hashi` binary, and `~/.hashi` if you also want to remove tokens and history.

</details>

## Quick start

**1. Have at least one agent installed and signed in.**

```bash
claude --version      # Claude Code
codex --version       # Codex
```

**2. Install Hashi** with the one-liner above.

**3. Start the bridge.**

```bash
hashi
```

On the first start Hashi creates an admin token and shows it once. Copy it with `c` in the dashboard (or from the terminal output in `--headless` mode).

**4. Create a token for your app and send a prompt.**

```bash
export HASHI_ADMIN=hashi_...   # from step 3

curl http://127.0.0.1:7878/v1/tokens \
  -H "Authorization: Bearer $HASHI_ADMIN" \
  -d '{"name": "my-app", "sessionMode": "stateful"}'

curl http://127.0.0.1:7878/v1/ask \
  -H "Authorization: Bearer hashi_<the token you just got>" \
  -d '{"message": "Write a haiku about bridges", "wait": 60}'
```

You can also manage tokens from the dashboard (`3`, then `n`) or the CLI (`hashi token create my-app --stateful`).

## Sharing

Hashi is built to be shared with people and apps you trust, without handing over your account.

**Invite a person.** From the dashboard press `3`, then `s`. Or from a terminal:

```bash
hashi share alice --daily-requests 50 --daily-budget 5 --expires-in-days 30
```

You get a link like `https://your-host/claim/hashi_inv_…`. It works once and expires after 24 hours (`--link-hours 1|24|168`). Send it however you like. When Alice opens it, she sees who is sharing, which agents she gets, her limits, and a notice that you can see her prompts and answers. One click gives her a personal token with ready-made curl, Python, TypeScript and MCP snippets. Only the link's hash is stored, and invited tokens are never admins.

**Bound every token.** Each token can carry:

| Limit | Flag | API field |
|---|---|---|
| Prompts per UTC day / month | `--daily-requests`, `--monthly-requests` | `dailyRequests`, `monthlyRequests` |
| Reported spend per UTC day / month (USD) | `--daily-budget`, `--monthly-budget` | `dailyBudgetUsd`, `monthlyBudgetUsd` |
| Prompts queued or running at once | `--max-pending` | `maxPending` |

Spend uses the cost estimate the agent reports. Claude Code reports one; Codex does not, so budgets only bound Claude. Invited tokens default to 3 prompts in flight. A token over a cap gets `429 limit_reached` with the `limit` it hit and `resetAt`.

**Pause, extend, revoke.** `p` in the Tokens view, `hashi token update <prefix> --pause`, or `PATCH /v1/tokens/{id}` with `{"paused": true}`. A paused token is refused on its very next prompt and can still read its earlier results. Resuming keeps every setting.

**Let people check themselves.** `/me` is a page where a token holder pastes their token to see their status, limits and today's use. Apps can call `GET /v1/me`.

**Plug Hashi into another agent.** The claim page shows the exact command. For Claude Code:

```bash
claude mcp add --transport http hashi https://your-host/mcp --header "Authorization: Bearer hashi_…"
```

The MCP server offers `ask_claude` and `ask_codex` (only the agents that token may use), `get_result` for long runs, and `my_usage`. Limits and pause apply exactly as they do over HTTP.

**See what happened.** Every token and invite change is recorded with who made it: `hashi audit`, `GET /v1/audit`, or the Logs view.

For links that work from other machines, give Hashi a stable address. See [Remote access and HTTPS](#remote-access-and-https).

## The dashboard

<p align="center">
  <img src="assets/dashboard.png" alt="Hashi dashboard showing both agents, request counters and live activity" width="900">
</p>

Run `hashi` in a terminal and the dashboard takes over the screen. When stdout is not a terminal, or with `--headless`, Hashi prints plain log lines instead.

The dashboard is built for long agent runs. Running requests count up their elapsed time, so a two-minute answer never looks frozen. Every status pairs a symbol with its colour (`✓` done, `✗` failed, `◌` queued, spinner for running). Body text uses your terminal's own foreground colour, so light and dark themes both stay readable. Set `HASHI_REDUCED_MOTION=1` to stop the spinners and shimmer.

| Key | Action |
|---|---|
| `1` `2` `3` `4` | Overview, Requests, Tokens, Logs |
| `↑` `↓` / `j` `k` | Move the selection |
| `PgUp` `PgDn` | Scroll the request detail |
| `n` / `d` | Create or revoke a token (Tokens view) |
| `s` | Share: create an invite link (Tokens view) |
| `p` | Pause or resume the selected token |
| `i` / `x` | Switch to pending invites / cancel one |
| `c` | Copy the API URL, or the token being shown |
| `?` | All key bindings |
| `Esc` | Close a popup |
| `q` | Quit and stop the bridge. Asks first while requests are in flight |
| `Ctrl-C` | Quit immediately |

## API

Every endpoint except `/healthz` and `/v1/status` needs `Authorization: Bearer <token>`. Request and response bodies are JSON with camelCase keys.

| Method | Path | Auth | Purpose |
|---|---|---|---|
| `GET` | `/healthz` | | Liveness probe |
| `GET` | `/v1/status` | | Version, uptime, which agents are installed |
| `POST` | `/v1/ask` | token | Queue a prompt |
| `GET` | `/v1/ask/{id}` | token | Read a result. Add `?wait=30` to long-poll |
| `DELETE` | `/v1/session/{agent}` | token | Forget this token's conversation with an agent |
| `GET` | `/v1/stats` | admin | Request counters |
| `GET` | `/v1/tokens` | admin | List tokens |
| `POST` | `/v1/tokens` | admin | Create a token |
| `PATCH` | `/v1/tokens/{id}` | admin | Pause, resume, extend, rename or set limits |
| `DELETE` | `/v1/tokens/{id}` | admin | Revoke a token |
| `GET` | `/v1/me` | any token | The token's own status, limits and usage (works when paused or expired) |
| `GET` `POST` | `/v1/invites` | admin | List or create invite links |
| `DELETE` | `/v1/invites/{id}` | admin | Cancel a pending invite |
| `POST` | `/v1/claim/preview`, `/v1/claim/redeem` | invite code | What an invite grants; use it once |
| `GET` | `/v1/audit` | admin | Recent sharing and access changes |
| `POST` | `/mcp` | token | MCP server (JSON-RPC over streamable HTTP) |
| `GET` | `/claim/{code}`, `/me` | | Pages for the people you share with |

### `POST /v1/ask`

| Field | Type | Default | Notes |
|---|---|---|---|
| `message` | string | required | Up to 32 KB |
| `agent` | `"claude"` or `"codex"` | server default (`claude`) | Must be allowed for the token |
| `model` | string | agent default | Passed to the agent's `--model` / `-m` |
| `wait` | seconds | none | Hold the request open up to this long (max 120) |

It returns `200` with the result when the run finished within `wait`, or `202` with a `poll` path otherwise. `status` is one of `queued`, `running`, `completed` or `failed`. A queued request also reports its `position`.

### `POST /v1/tokens`

| Field | Type | Default |
|---|---|---|
| `name` | string, 1 to 100 chars | required |
| `role` | `"user"` or `"admin"` | `"user"` |
| `sessionMode` | `"stateless"` or `"stateful"` | `"stateless"` |
| `agents` | array of agent names | both |
| `expiresInDays` | 1 to 3650 | never expires |
| `note` | string, up to 200 chars | none |
| `limits` | object, see [Sharing](#sharing) | no limits |

The response contains the raw `token` exactly once.

`PATCH /v1/tokens/{id}` accepts `name`, `note`, `paused`, `expiresInDays` (from now; `0` removes the expiry) and `limits` (replaces the whole set). `POST /v1/invites` accepts `label`, `sessionMode`, `agents`, `expiresInDays` (counted from the claim), `limits` and `linkHours` (`1`, `24` or `168`), and answers with the one-time `url` and `localOnly: true` when no public URL is configured.

### Errors

Errors share one shape: `{"error": {"code": "rate_limited", "message": "..."}}`.

| Status | Code | When |
|---|---|---|
| 400 | `bad_request` | Missing or invalid fields, unknown agent, unsafe model name |
| 401 | `unauthorized` | Missing, unknown or expired token |
| 403 | `forbidden` | Admin required, or the token may not use that agent |
| 403 | `token_paused` | The token is paused by the owner |
| 404 | `not_found` | Unknown request, or a request owned by another token |
| 404 | `invite_not_found` | Unknown or cancelled invite link |
| 409 | `conflict` | Revoking the last admin token, pausing your own token, or the token limit is reached |
| 409 | `already_claimed` | The invite link was already used |
| 410 | `invite_expired` | The invite link is past its expiry |
| 413 | | Body larger than 64 KB |
| 429 | `limit_reached` | A daily or monthly cap was hit; `limit` and `resetAt` say which and until when |
| 429 | `too_many_pending` | The token already has `maxPending` prompts in flight |
| 429 | `rate_limited` | Per-token rate limit hit, or the queue is full |
| 503 | `unavailable` | That agent is not installed on this machine |

### From code

```python
import requests

HASHI = "http://127.0.0.1:7878"
HEADERS = {"Authorization": "Bearer hashi_..."}

r = requests.post(f"{HASHI}/v1/ask", headers=HEADERS,
                  json={"message": "List three uses for a bridge", "agent": "codex", "wait": 90})
print(r.json()["response"])
```

```ts
const res = await fetch("http://127.0.0.1:7878/v1/ask", {
  method: "POST",
  headers: { Authorization: "Bearer hashi_...", "Content-Type": "application/json" },
  body: JSON.stringify({ message: "Summarise README.md", wait: 90 }),
});
const { status, response, error } = await res.json();
```

## Remote access and HTTPS

Hashi listens on `127.0.0.1` over plain HTTP by default, which is fine for apps on the same machine. To reach it from elsewhere, pick one of these. Each gives you HTTPS.

| Option | Command | Address | Good for |
|---|---|---|---|
| Cloudflare quick tunnel | `hashi serve --tunnel` | random `https://*.trycloudflare.com`, new on every start | trying it out, short sessions |
| Cloudflare named tunnel | `hashi serve --tunnel-token <token> --public-url https://hashi.example.com` | your own hostname, stable | sharing invite links that last |
| Hashi serves HTTPS itself | `hashi serve --tls-cert cert.pem --tls-key key.pem` | wherever you point DNS or your VPN | LANs, Tailscale, your own certificates |
| Your reverse proxy | run Caddy or nginx in front, then `--public-url https://...` | your proxy's address | existing setups |

**Quick tunnel.** Install `cloudflared` (`brew install cloudflared` or your platform's package) and add `--tunnel`. The URL appears in the dashboard header. Because it changes on restart, invite links made against it stop working after a restart.

**Named tunnel.** In the Cloudflare dashboard, create a tunnel under Zero Trust, add a public hostname pointing at `http://localhost:7878` (or `https://localhost:7878` with "No TLS Verify" when Hashi serves TLS itself), and copy the tunnel token. Hashi passes the token to cloudflared through an environment variable, so it never shows up in the process list. Put the token in `HASHI_TUNNEL_TOKEN` rather than your shell history.

**Native HTTPS.** Give Hashi a PEM certificate chain and key. Hashi speaks TLS 1.2 and 1.3 through rustls and sends `Strict-Transport-Security`.

- Your own domain: get a certificate from Let's Encrypt (for example `certbot certonly --standalone -d hashi.example.com`).
- A Tailscale network: `tailscale cert <machine>.<tailnet>.ts.net` gives a trusted certificate for your machine's tailnet name. Bind to the tailnet address with `--bind`.
- A home network: `mkcert` creates a local CA your own devices can trust.

Then share links with `--public-url` set to the address people will use, so invites point at the right place.

Hashi always gives cloudflared its own empty config file, so an existing `~/.cloudflared/config.yml` from another project cannot redirect Hashi's tunnel. Hashi also never downloads `cloudflared` for you, because running an unverified binary fetched at runtime is a supply-chain risk.

## Security

Anyone holding a token can make your agent run on your computer with your account, so Hashi starts locked down.

- **Tokens.** Each token is 256 bits from the OS random generator, prefixed `hashi_` so secret scanners recognise it. The database stores only its SHA-256 hash.
- **Least privilege.** Tokens default to the `user` role. Only admins manage tokens, and the last admin cannot be revoked. Requests are visible only to the token that made them, and to admins.
- **No shell.** Agents are started directly with an argument vector. The prompt always follows `--`, so it can never be read as a flag. Model names and session ids are checked against a strict alphabet and may not start with `-`.
- **Sandboxed by default.** Codex runs with `sandbox_mode="read-only"`. Claude Code runs in print mode with its normal permission rules and a turn limit. Loosen these deliberately with `--codex-sandbox` and `--claude-permission-mode`.
- **Isolated workspaces.** Every token gets its own working directory under `~/.hashi/workspaces/`.
- **Limits.** Bodies over 64 KB and prompts over 32 KB are refused. Each run is killed after its timeout (default 5 minutes) or once it prints more than 10 MB. There is a per-token rate limit (default 60 requests per minute) and a global queue cap.
- **Private data.** `~/.hashi` is created `0700` and the database `0600`.
- **Loopback first.** Binding to a non-loopback address without TLS prints a warning. Tunnels and HTTPS are opt-in.
- **Sharing is bounded.** Invite links are single-use, hashed and short-lived, and create `user` tokens only. Usage caps and pause apply to HTTP and MCP alike, and every change is audited.
- **Pages are locked down.** The claim and My access pages load nothing from other sites, run no inline script, and are served with a strict Content Security Policy, `no-referrer` and `no-store`, so an invite code never leaks through a Referer header or a cache.
- **Supply chain.** `unsafe` is forbidden in the crate. CI runs `cargo deny` for advisories, licences and sources.

Read [SECURITY.md](SECURITY.md) to report a vulnerability.

## Configuration

Every flag also reads an environment variable.

| Flag | Env | Default |
|---|---|---|
| `--home` | `HASHI_HOME` | `~/.hashi` |
| `--bind` | `HASHI_BIND` | `127.0.0.1` |
| `--port` | `HASHI_PORT` | `7878` |
| `--tunnel` | `HASHI_TUNNEL` | off |
| `--tunnel-token` | `HASHI_TUNNEL_TOKEN` | none. Runs a named tunnel; needs `--public-url` |
| `--public-url` | `HASHI_PUBLIC_URL` | none. Base for invite links, for example `https://hashi.example.com` |
| `--tls-cert`, `--tls-key` | `HASHI_TLS_CERT`, `HASHI_TLS_KEY` | none. Serve HTTPS with these PEM files |
| `--name` | `HASHI_NAME` | `Hashi`. Shown on invite and My access pages |
| `--headless` | | off (on when stdout is not a terminal) |
| | `HASHI_REDUCED_MOTION` | off. Set to `1` to freeze dashboard animation |
| `--default-agent` | `HASHI_DEFAULT_AGENT` | `claude` |
| `--concurrency` | `HASHI_CONCURRENCY` | `1` run per agent |
| `--timeout` | `HASHI_TIMEOUT` | `300` seconds |
| `--rate-limit` | `HASHI_RATE_LIMIT` | `60` per minute per token (`0` disables) |
| `--queue-limit` | `HASHI_QUEUE_LIMIT` | `100` pending requests |
| `--retention-days` | `HASHI_RETENTION_DAYS` | `14` |
| `--claude-bin` | `HASHI_CLAUDE_BIN` | found on `PATH` |
| `--codex-bin` | `HASHI_CODEX_BIN` | found on `PATH` |
| `--max-turns` | `HASHI_MAX_TURNS` | `10` (Claude) |
| `--claude-permission-mode` | `HASHI_CLAUDE_PERMISSION_MODE` | Claude's default |
| `--codex-sandbox` | `HASHI_CODEX_SANDBOX` | `read-only` |

Pass these to `hashi serve` (for example `hashi serve --port 9000 --tunnel`). Running bare `hashi` starts the server with defaults and environment variables. Logs go to `~/.hashi/logs/hashi.log` while the dashboard is open. Set `HASHI_LOG=hashi=debug` for more detail.

### Other commands

```bash
hashi doctor                               # are the agents and cloudflared installed?
hashi token create ci --agents codex --expires-in-days 30
hashi token list                           # status and today's use per token
hashi token update hashi_1a2b3c4d --pause  # or --resume, --daily-requests 100, --clear-limits
hashi token revoke hashi_1a2b3c4d
hashi share alice --agents claude          # one-time invite link
hashi invite list                          # pending, claimed and expired invites
hashi audit                                # who changed what
hashi ask "What does this repo do?" -a claude   # one prompt, no server
```

## How it works

```
 your app ──HTTPS──▶ cloudflared (optional)
                          │
                          ▼
┌──────────────────────── hashi ────────────────────────┐
│  axum API ─▶ bearer auth ─▶ SQLite queue              │
│                               │                       │
│                ┌──────────────┴──────────────┐        │
│                ▼                             ▼        │
│         claude workers                codex workers   │
│     claude -p --output-format     codex exec --json   │
│              json                                     │
└───────────────────────────────────────────────────────┘
```

A request is written to SQLite and the matching agent's worker is woken. The worker claims the oldest queued request in one atomic `UPDATE ... RETURNING`, runs the CLI in that token's workspace and parses the output. Claude prints one JSON result and Codex streams JSONL events. The worker then stores the answer. Long-polling clients and the dashboard follow along on an in-process event bus.

## Limits worth knowing

- The machine running Hashi has to stay on, with the agents signed in.
- Each run is a full agent session, so answers take seconds to minutes.
- With the default `--concurrency 1`, prompts for the same agent run one at a time.
- On Windows, npm installs Codex as a `codex.cmd` batch shim. Rust refuses to pass arguments it cannot quote safely for a batch file, so a prompt containing such characters fails with a clear error instead of running.
- Using your subscription this way is subject to each provider's terms. Check them for your plan before you share access.

## Contributing

Bug reports and pull requests are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers the local setup and the checks CI runs.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.

Hashi is an independent project. It is not affiliated with Anthropic or OpenAI. Claude Code and Codex are trademarks of their respective owners.
