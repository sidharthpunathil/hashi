# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Update check: on start, Hashi asks GitHub for the latest release at most once a day (cached in `~/.hashi/update-check.json`, failures included) and the dashboard header shows `update vX.Y.Z: hashi update` when one is out. Headless mode logs one line. Turn it off with `--no-update-check` or `HASHI_NO_UPDATE_CHECK=1`.
- `hashi update [--check] [--version vX.Y.Z] [--yes]` downloads the release archive for your platform, verifies its SHA-256 checksum, checks that the new binary reports the expected version and replaces the running one. It refuses Cargo installs and directories you cannot write to.
- `hashi doctor` shows the current and latest version and whether an update is available.

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
