# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
- `GET /v1/stats` returns a `resources` object (memory, reserve, running and maximum runs, why work is held, free disk) for admins only. The dashboard Overview shows a memory gauge, run slots and "held: low memory" or "held: disk".

### Changed

- Logs are always written to `~/.hashi/logs/hashi.log`, also in `--headless` mode, with tokens, keys and the home directory redacted. The file rotates past 5 MB and keeps three old copies.

### Fixed

- opencode runs are now read-only: shell, edits, web fetches and paths outside the workspace are denied. opencode allows most tools by default and has no sandbox of its own.
- Agent runs start in their own process group on Unix, and the whole group is killed on timeout, memory stop, output overflow, cancellation and shutdown; on Windows the process tree is ended with `taskkill /T /F`. Shells and servers an agent started no longer outlive its run. `hashi ask` now stops the agent on Ctrl-C.
- Each agent now gets its own workspace per token, so an agent with write access cannot plant project config (including hooks) that another agent would load. Existing stateful conversations start fresh once after upgrading.
- A `~/.cloudflared/config.yml` from another project no longer hijacks Hashi's tunnel.
- Pruning old requests no longer removes the current month, which monthly caps count from.

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
