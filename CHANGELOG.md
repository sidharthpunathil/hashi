# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- One-time invite links (`hashi share`, `POST /v1/invites`, dashboard `s`) with a claim page that shows what is shared and hands out a personal token once.
- Per-token limits: prompts per day and month, reported spend per day and month, and prompts in flight. Pause, resume and extend tokens (`PATCH /v1/tokens/{id}`, `hashi token update`, dashboard `p`).
- `GET /v1/me` and a `/me` page so token holders can see their own status, limits and usage.
- MCP server at `/mcp` with `ask_claude`, `ask_codex`, `get_result` and `my_usage`.
- Audit log of token and invite changes (`GET /v1/audit`, `hashi audit`).
- Native HTTPS (`--tls-cert`, `--tls-key`), named Cloudflare tunnels (`--tunnel-token`), `--public-url` and `--name`.
- `hashi report` writes a redacted Markdown diagnostic report (versions, agent detection, settings, counts, recent failures, log tail, crash files) and prints a prefilled GitHub issue link. Nothing is uploaded. `--include-prompts` adds prompt excerpts, `--stdout` prints it.
- Crash reports: a panic saves a redacted `~/.hashi/logs/crash-<time>.txt` with a backtrace.
- API errors with status 500 carry an `errorId` that is also logged, and failed requests log one too.

### Changed

- Logs are always written to `~/.hashi/logs/hashi.log`, also in `--headless` mode, with tokens, keys and the home directory redacted. The file rotates past 5 MB and keeps three old copies.

### Fixed

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
