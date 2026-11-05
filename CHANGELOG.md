# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Go public without a restart: press `t` in the dashboard to start a Cloudflare quick tunnel on a running server. It behaves like `--tunnel`, the header shows its progress and URL, and a missing `cloudflared` gets an install hint. `CF-Connecting-IP` is trusted from loopback only while a tunnel is configured or running.
- The invite popup keeps the invite code and builds the link from the current base URL, so it turns public when a tunnel comes up. A local-only link says "press t to make this link public".
- A running server writes its public base URL to `~/.hashi/public-url` (owner only) and removes it on shutdown. `hashi share` without `--public-url` uses it.
- Update check: on start, Hashi asks GitHub for the latest release at most once a day (cached in `~/.hashi/update-check.json`, failures included) and the dashboard header shows `update vX.Y.Z: hashi update` when one is out. Headless mode logs one line. Turn it off with `--no-update-check` or `HASHI_NO_UPDATE_CHECK=1`.
- `hashi update [--check] [--version vX.Y.Z] [--yes]` downloads the release archive for your platform, verifies its SHA-256 checksum, checks that the new binary reports the expected version and replaces the running one. It refuses Cargo installs and directories you cannot write to.
- `hashi doctor` shows the current and latest version and whether an update is available.
- `hashi connect <invite link>` redeems an invite from the terminal and saves the token in an owner-only `~/.hashi/remotes.json`; `--setup claude` also runs `claude mcp add`. `hashi remote list` and `hashi remote remove` manage saved bridges.
- `hashi ask --remote [<name>] "<prompt>"` sends a prompt to a saved bridge and waits for the answer.
- The claim page offers "Set up from a terminal": one command (sh or PowerShell) that installs Hashi and connects it with the invite link. The installers run `hashi connect "$HASHI_CONNECT"` when `HASHI_CONNECT` is set.
- After claiming, the claim page has Add to Cursor and Add to VS Code buttons and a Codex MCP snippet (`codex mcp add --url ... --bearer-token-env-var`).
- `hashi uninstall [--dry-run] [--keep-data] [--yes]` lists the binary, a leftover `hashi.old.exe`, the data directory and (on Windows) the installer's PATH entry, asks you to type `uninstall`, then removes them. It refuses while a server is running (a running server now writes `~/.hashi/server.pid`, owner only), refuses a data directory that is a symlink or holds files Hashi did not create, leaves Cargo installs to `cargo uninstall hashi`, and prints how to remove cloudflared, tailscale and the MCP entries for your saved remotes.
- Tailscale Serve and Funnel: `hashi serve --tailscale serve|funnel` (`HASHI_TAILSCALE`, `--tailscale-port`, `--tailscale-bin`), or the Commands view, publishes Hashi at `https://<machine>.<tailnet>.ts.net`. Hashi checks the sign-in state and refuses a port that serves something it did not set up, shows any approval link Tailscale prints, and while the route is up uses its URL for invite links, accepts its host name and trusts `X-Forwarded-For` from loopback. On stop and shutdown it runs only the matching `off` command, never `reset`.
- The Cloudflare quick tunnel can be stopped at runtime, and only one public route runs at a time: starting another asks to switch.
- `hashi install cloudflared|tailscale [--yes]` shows the vendor's documented install command for this platform and runs it after you confirm, or prints the download page where there is none. In the dashboard, a missing helper opens a popup that runs the command in the terminal (`enter`), copies it (`c`) or cancels (`esc`). `--cloudflared-bin` / `HASHI_CLOUDFLARED_BIN` point at a specific `cloudflared`.
- `hashi doctor` reports `tailscale` and whether it is signed in, next to `cloudflared`.
- Dashboard tab 5, Commands: every action grouped (go public, share, install, maintenance, help), each with its key, its current state and the equivalent CLI command. `↑` `↓` or `j` `k` select and `enter` runs the row.
- OpenAI-compatible API: `POST /v1/chat/completions` and `GET /v1/models` take Hashi tokens and go through the same submit path as `/v1/ask`, so pause, scopes, limits and the queue apply. `model` picks the agent (`claude`, `codex`, ...) and `agent/model` (such as `claude/opus`) passes the agent's own model. System and developer messages become an instruction block and earlier turns a transcript; only text content is accepted. `stream: true` sends server-sent events with `: queued` / `: running` keep-alives, the whole answer as one chunk, a finish chunk and `[DONE]`. Without streaming a request is held up to 120 seconds, then answers `202` with the Hashi request id. Errors use OpenAI's `{"error": {"message", "type", "code"}}` shape. `docs/api.md` has examples for `curl`, the `openai` Python package and Open WebUI.
- Cancel a request: `DELETE /v1/ask/{id}` (a token's own requests, or any for an admin), the MCP `cancel` tool, and `x` in the dashboard Requests view. A queued request fails with `cancelled`; a running one has its whole process tree stopped through the same path revoke uses. A finished request answers `409`. Cancels by anyone other than the sending token are audited as `request.cancel`.
- Several accounts per agent. `hashi account add|login|list|disable|enable|remove` manages login accounts (a directory under `~/.hashi/accounts/` that the agent CLI signs in to itself, for Claude Code, Codex, Gemini CLI, opencode and Qwen Code) and key accounts (an API key in an owner-only file, for Claude Code, Codex, Gemini CLI and Cursor CLI, read without echo or from `--key-env`). Tokens and invites take an ordered `accounts` list per agent (`accounts` on `POST`/`PATCH /v1/tokens` and `POST /v1/invites`, `--account claude=work,personal` and `--clear-accounts` on the CLI, an accounts field in the dashboard forms); unknown names are refused.
- Failover: when a run hits the agent's usage limit message, the account is skipped until the reset time in the message (or `--account-cooldown`, 60 minutes by default) and the prompt runs again on the next account, without counting as an interruption. With none left it fails with the earliest reset time. A success clears the mark.
- Stateful sessions are kept per token, agent and account. Each request records the account that ran it, shown to admins in `GET /v1/ask/{id}` and the dashboard; `GET /v1/accounts` (admin) lists accounts with their state and counts; agent cards show their account count and the tokens view each token's accounts. `hashi doctor` lists accounts and `hashi uninstall` counts `accounts/` as Hashi's own. Token holders never see account names.
- Runs on a non-default account drop the owner's credential variables for that agent (such as `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`) before setting the account's own. The database moves to schema version 4.
- Crush (`crush`, 0.100.x) as a seventh agent, opt-in with `--enable-crush` / `HASHI_ENABLE_CRUSH`; while off it is not probed or listed and a request for it gets "Crush is not installed or not enabled". A headless `crush run` approves every tool call itself, so before each run Hashi writes a workspace config that removes every tool except `view`, `ls`, `glob`, `grep`, `todos` and the read-only LSP queries, refuses the run when a Crush project config (`.crushrc`, `crushrc`, `.crush.json`, `crush.json`) is in the workspace or a parent, when the workspace has a `.env` or when `/etc/crush/crush.json` exists, and refuses a Crush release newer than 0.100.x unless `--crush-allow-newer` is passed. Every Crush account, `default` included, uses its own config and data directories under `~/.hashi/accounts/crush/`, so your own Crush config and MCP servers never load; sign one in with `hashi account login crush <name>`. Inherited `CRUSH_*` variables are always removed, and the provider keys on non-default accounts. Tokens and cost come from `crush session last --json`, run under a lock per data directory. `--crush-bin` / `HASHI_CRUSH_BIN` set the executable, `hashi install crush` shows the README's install command, and `hashi doctor` says whether Crush is enabled.
- Projects: a folder with its own agents, ordered accounts per agent, default agent, model per agent and note. `hashi project add|update|list|remove` (remove refuses while tokens or pending invites are bound unless `--unbind`), `--project` on `token create|update` and `share`, `--clear-project` on `token update`, and admin `GET|POST /v1/projects` and `PATCH|DELETE /v1/projects/{name}`, with per-project requests and spend today and this month and the number of bound tokens. Paths must be existing folders outside the Hashi home and are stored with symlinks resolved.
- A bound token's runs start in the project folder, with the agents both allow, its own account list only when it is a subset of the project's (else the project's), and the project's model when a request names none. Binding or editing a project can never widen access: a wider account list is refused. Sessions are kept per token, agent, account and project. Hashi never writes to, moves or deletes a project folder, on revoke, remove or uninstall.
- Token holders see only the project's name: `project` in `/v1/me` and the claim preview, and "Works in the project api" on the claim and `/me` pages. Admins see `project` on tokens, invites and requests. The dashboard's token and share forms have a project selector, and the token and request details show the project. The database moves to schema version 5.
- Upstream tracking. `docs/research/verified-versions.json` records the release each agent was last verified against, shown in a new "Verified with" README column that a test keeps in sync. `hashi doctor` and the startup log say when an installed agent is newer than that by major or minor version (Cursor by build date), the dashboard agent card shows a "newer than verified" tag, and `GET /v1/stats` lists each agent's version, verified version and `newerThanVerified`. This never blocks a run. A weekly workflow opens or updates one `upstream` issue per agent with a newer release, listing the Hashi files that depend on it and the re-verify checklist in CONTRIBUTING.md.
- Usage limits: Hashi keeps each account's quota windows (used percent, reset time, label, source `poll`, `run` or `error`, and when it was seen) and summarises them as headroom, binding window, next reset and a state (`ok`, `warning`, `limited` or `unknown`). While the server runs it reads Claude Code accounts from the OAuth usage endpoint (the token read only, from the Keychain or `.credentials.json`, never refreshed or logged) and Codex accounts through a short-lived `codex app-server` with the account's `CODEX_HOME`, every five minutes with jitter and after each run, never more than once a minute, backing off on `429` and `403` and skipping Codex while a run on the account is active. `--no-usage-poll` / `HASHI_USAGE_POLL=0` turns the reads off.
- Account strategies: `in-order` (the default), `most-left` and `soonest-reset`, with a rotate threshold (`--rotate-at`, 90% by default, 50 to 99) past which an account is skipped while another is under it. Set them per project (`--account-strategy` and `--rotate-at` on `project add|update`, `accountStrategy` and `rotateAt` on `/v1/projects`) or as server defaults (`hashi serve --account-strategy`, `--rotate-at`). A stateful conversation stays on its account until that account reaches the threshold or its limit, then starts fresh on the newly chosen one. `GET /v1/tokens` shows which account each conversation is on.
- `hashi account usage [--agent a] [--refresh] [--json]` shows a bar per window with its percent and reset time, the plan, the source and its age, and the state; `hashi account list` has a usage column. `GET /v1/accounts` includes `usage` and `summary`, and admin `POST /v1/accounts/{agent}/{name}/refresh` reads an account now, at most once a minute. Token holders never see usage.
- Claude Code runs use `--output-format stream-json --verbose`. The answer, session and usage come from the `result` message exactly as before; each `rate_limit_event` updates the account's usage, and a `rejected` limit (or an assistant `rate_limit` error) marks the account limited until the event's reset time instead of relying on the message text.
- An `AccountUsage` bus event on each change: agent cards show the best account's headroom, and the dashboard log, a toast and the headless log say when an account crosses the threshold, is limited or recovers. The database moves to schema version 6.
- Nearly everything from the dashboard. A parity table maps every `hashi` subcommand to its place in the dashboard, or to excluded for `serve` and `uninstall`, and a test walks the command tree so a new subcommand cannot be left out. Every Commands row shows the command it stands for, the `?` help and the footers list the new keys, and no two actions in one view share a key.
- Dashboard token edit (`e` in Tokens): name, note, expiry (days from now or never), daily and monthly prompts and budget, prompts in flight, accounts and project, through the same update as `hashi token update` and `PATCH /v1/tokens/{id}`, which now share one library function.
- Dashboard prompt popup (`a` anywhere, or Commands, Ask): pick this Hashi or a saved bridge, the agent and, for this Hashi, an account to pin the run to, then type a multi-line prompt and send it with `Ctrl-S`. A local prompt is queued as the oldest active admin token and shows in Requests, where the answer appears in its detail; a bridge's answer opens in a popup. `SubmitInput` gains an admin-only `account`.
- Dashboard Accounts (add, sign in again, disable, enable, remove, with the sign-in run in the terminal the way installs are and keys typed into a masked field), Projects (add, edit, remove), Bridges I use (list, forget, ask, connect with a pasted invite link), the audit log, a doctor popup (agents with versions, Crush, cloudflared, Tailscale and its sign-in state, accounts, the update state, the home and the resource guard), a diagnostic report popup (prompts off by default; the current folder or `~/.hashi/reports/`, owner-only, with `c` to copy the path) and installing an update with the terminal attached.
- Move a running server to another port or bind address from the dashboard (Commands, Port and bind address). The new address is bound first, then served with the same TLS settings, connection cap and limits while the old one finishes its open requests and closes; a quick tunnel or Tailscale route that is up is restarted on the new port, and a named tunnel gets a warning to update its hostname. A bind outside loopback without TLS asks first. The choice can be kept in an owner-only `~/.hashi/settings.json`, read at startup after the flag and the environment variable; `hashi uninstall` counts it as Hashi's own.
- The invite popup shows the link as a QR code, drawn with half blocks, when the terminal has room for it.

## [0.2.0] - 2026-11-05

### Added

- Four more agents: Gemini CLI (`gemini`), opencode (`opencode`, 2.2 or later), Qwen Code (`qwen`) and Cursor CLI (`cursor`), each with its safest headless settings. `hashi doctor` lists them; the dashboard shows a card per installed agent.
- One-time invite links (`hashi share`, `POST /v1/invites`, dashboard `s`) with a claim page that shows what is shared and hands out a personal token once.
- Per-token limits: prompts per day and month, reported spend per day and month, and prompts in flight. Pause, resume and extend tokens (`PATCH /v1/tokens/{id}`, `hashi token update`, dashboard `p`).
- `GET /v1/me` and a `/me` page so token holders can see their own status, limits and usage.
- MCP server at `/mcp` with `ask_claude`, `ask_codex`, `get_result` and `my_usage`.
- Audit log of token and invite changes (`GET /v1/audit`, `hashi audit`).
- Native HTTPS (`--tls-cert`, `--tls-key`), named Cloudflare tunnels (`--tunnel-token`), `--public-url` and `--name`.
- `hashi report` writes a redacted Markdown diagnostic report (versions, agent detection, settings, counts, recent failures, log tail, crash files) and prints a prefilled GitHub issue link. Nothing is uploaded. `--include-prompts` adds prompt excerpts, `--stdout` prints it.
- Crash reports: a panic saves a redacted `~/.hashi/logs/crash-<time>.txt` with a backtrace.
- API errors with status 500 carry an `errorId` that is also logged, and failed requests log one too.
- Resource guard, so shared tokens cannot make the owner's machine unusable. A global cap on runs across all agents (`--max-running`, default total memory in GiB / 8 between 1 and the CPU count). Memory admission keeps a reserve free for the owner (`--memory-reserve`, default the larger of 2 GiB and 20%) and a disk floor under the Hashi home (`--disk-floor`, default 1 GiB): short of either, requests stay queued in order and run later. A per-run watchdog samples the run's whole process tree every 2 seconds and stops it above `--run-memory-limit` (default the larger of 2 GiB and 40%), failing the request with `run exceeded its memory limit (X of Y)`. Agent runs start at a lower priority (`--nice`, default 10). Sizes accept `512M`, `4G` or `25%`, with matching `HASHI_*` variables.
- Connection cap (`--max-connections`, `HASHI_MAX_CONNECTIONS`, default 512) for plain HTTP and HTTPS. Clients past the cap wait in the backlog until a connection closes.
- Hourly maintenance while the server runs: finished requests past `--retention-days` (never the current month), audit records and claimed or expired invites older than the retention period or 90 days (whichever is longer) are deleted, and freed pages go back to the disk through SQLite incremental auto-vacuum. Existing databases are converted on first start when their live data is under 128 MiB, otherwise by a later pass once pruning gets them under it.
- Schema version 3: a covering index serves usage checks without reading prompt or response text, and requests count how often they interrupted the server.
- `GET /v1/stats` returns a `resources` object (memory, reserve, running and maximum runs, why work is held, free disk) for admins only. The dashboard Overview shows a memory gauge, run slots and "held: low memory" or "held: disk".

### Changed

- Logs are always written to `~/.hashi/logs/hashi.log`, also in `--headless` mode, with tokens, keys and the home directory redacted. The file rotates once it passes 5 MB, at startup and while Hashi runs, and keeps three old copies. Only the newest ten crash files are kept.
- `user` tokens created without limits (`POST /v1/tokens`, `hashi token create`, dashboard `n`) now get `maxPending: 3`, like invite tokens. Admin tokens stay unlimited. Lift it with `PATCH /v1/tokens/{id}` or `hashi token update --clear-limits`.
- 10% of the queue (at least 5 slots, at most half) is reserved for admin tokens.
- A token may hold at most 4 long polls at once across `POST /v1/ask`, `GET /v1/ask/{id}` and MCP `ask_*` and `get_result`. Further waits answer at once with the current state (202).
- The dashboard request list loads only the first 200 characters of each prompt, and the selected request in full.
- A failed run logs only the first 500 characters of the agent's error; the request keeps the full text.
- A run stopped by a clean shutdown is put back in the queue right away instead of on the next start.

### Fixed

- A client that does not send a complete request head within 10 seconds, or leaves a keep-alive connection idle that long, is disconnected. Before, a slow or silent client could hold a socket forever.
- Hashi raises its open-file soft limit toward the hard limit (up to 8192) at startup on Unix, and warns when it is still close to `--max-connections`. macOS starts launchd and GUI processes at 256.
- Invite claims (`/v1/claim/preview`, `/v1/claim/redeem`) are rate limited per client address instead of in one shared bucket, so one client can no longer stop everyone from claiming their invites. Unknown bearer tokens are throttled per client address before the database lookup. Behind a tunnel the address comes from `CF-Connecting-IP`, which is trusted only from loopback.
- opencode runs are now read-only: shell, edits, web fetches and paths outside the workspace are denied. opencode allows most tools by default and has no sandbox of its own.
- Agent runs start in their own process group on Unix, and the whole group is killed on timeout, memory stop, output overflow, cancellation and shutdown; on Windows the process tree is ended with `taskkill /T /F`. Shells and servers an agent started no longer outlive its run. `hashi ask` now stops the agent on Ctrl-C.
- Each agent now gets its own workspace per token, so an agent with write access cannot plant project config (including hooks) that another agent would load. Existing stateful conversations start fresh once after upgrading.
- A `~/.cloudflared/config.yml` from another project no longer hijacks Hashi's tunnel.
- Pruning old requests no longer removes the current month, which monthly caps count from.
- The dashboard no longer redraws ten times a second while idle: it draws on input, events, data refreshes and only animates while something moves (a running request, the tunnel spinner, a toast or a form cursor). The Requests view builds only the visible rows from the first 200 characters of each prompt and keeps the detail pane between frames, so long histories of large prompts no longer cost a CPU core.
- Concurrent submits can no longer overshoot `maxPending` or the queue limit: both are checked in the same transaction as the insert.
- Revoking a token (API, CLI or dashboard) deletes its workspace directory. Symlinks are removed without being followed.
- A request that was running during 3 unclean stops of Hashi (crashes, not normal quits) now fails with "stopped after interrupting the server 3 times" instead of running again on every start.

### Security

Fixes from a full security audit of the codebase.

- A request body must arrive within 10 seconds, and one client address may hold at most an eighth of the connection slots, so a client stalling half-sent invite claims can no longer take every slot. Shutdown waits at most 10 seconds for open requests.
- A valid token or invite code always works, even from an address that sent many bad ones; only failures are throttled. Behind a reverse proxy, `--trust-proxy` keys clients by `X-Forwarded-For` on loopback connections.
- Cross-origin browser access is off by default (`--cors-origin` to allow an origin), and plain HTTP refuses unknown `Host` headers (`--allowed-host` to add names), which stops DNS rebinding.
- opencode runs are default deny: only read, glob, grep, list and lsp are allowed, so the owner's MCP tools, web search and code execution are no longer reachable from shared tokens.
- Claude runs load user settings only, connect no MCP servers and cannot use `WebFetch` unless the owner sets a permission mode or passes the flags. Gemini and Qwen connect no MCP servers, Qwen keeps its memory in the workspace, and Cursor strips `--approve-mcps`. Prompts starting with `/` are neutralised for Claude as well.
- Agent processes that detach into their own session are measured against the run's memory limit and killed when the run ends. Linux adopts orphaned descendants; Windows puts each run in a Job Object and stops npm `.cmd` shims from running a `node` planted in the workspace.
- Hashi's own variables (`HASHI_*`, `TUNNEL_TOKEN`) are removed from agent environments, so a run never sees the tunnel token. Passing `--tunnel-token` as a flag prints a warning.
- Pausing a token holds its queued prompts, expiry fails them without running, and revoking stops running prompts at once (from the API or dashboard) or within about 2 seconds (from `hashi token revoke`).
- Request and spend caps are checked in the same transaction as the insert, so concurrent bursts cannot exceed them. Changing limits keeps `maxPending` unless it is set, redeeming an invite respects the token cap, and a paused admin token can no longer read other tokens' requests.
- Hashi shuts down cleanly on `SIGTERM`, `SIGHUP` and Windows console close, not only Ctrl-C, so agent runs and cloudflared no longer outlive it. A second signal exits at once.
- `hashi report` no longer copies stored agent error text by default; failed runs get a fixed label. Included text cannot break out of its code block. The report, database WAL and SHM files, logs, crash files and tunnel config are created owner-only, and on Windows the Hashi home is limited to the current user, SYSTEM and Administrators.
- Errors returned to token holders mask secrets and the owner's home directory, headless output strips control and text-direction characters, and redaction also covers AWS access keys, Google OAuth tokens and Cloudflare tunnel tokens.
- Release builds grant write access only to the publish job, never persist checkout credentials, and pin every action to a commit. The installers refuse to fall back to a source build when a release archive downloads but its checksum does not.

## [0.1.1] - 2026-11-05

### Added

- One-line installers: `install.sh` for macOS and Linux, `install.ps1` for Windows. Both verify SHA-256 checksums and need no admin rights.
- Prebuilt release binaries for Linux (x64, ARM64), macOS (Intel, Apple Silicon) and Windows (x64).
- Windows support: agents installed as `.exe` or npm `.cmd` shims are found automatically, and the token secret can be copied with `clip`.
- `HASHI_REDUCED_MOTION=1` freezes spinners, shimmer and other animation.

### Changed

- Dashboard body text follows the terminal's foreground colour, so light themes are readable.
- Running requests show live elapsed time in the table, detail pane and agent cards.
- `q` asks for confirmation while requests are in flight. `Esc` only closes popups.
- The new-token form shows validation errors inline under the name field.
- Empty views explain what to do next.
- The logo has transparent light and dark variants.

## [0.1.0] - 2026-11-05

### Added

- HTTP API to send prompts to Claude Code and Codex, with polling and long-polling.
- A separate worker queue per agent, persisted in SQLite, with crash recovery.
- Bearer tokens stored as SHA-256 hashes, with roles, agent scopes, expiry and revocation.
- Stateful sessions that resume each token's Claude or Codex conversation.
- Optional public HTTPS through a Cloudflare quick tunnel.
- A live terminal dashboard covering overview, requests, tokens and logs.
- `hashi doctor`, `hashi token` and `hashi ask` commands.
