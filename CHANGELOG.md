# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Every local Claude Code and Codex account gets two shell commands that start the CLI signed in as that account: `<name>-claude` and `<name>-dclaude` (with `--dangerously-skip-permissions`), and `<name>-codex` and `<name>-dcodex` (with `--dangerously-bypass-approvals-and-sandbox`) for Codex logins. Arguments pass through, as in `rajat-claude --resume`. They are small scripts next to the `hashi` command your PATH finds (usually `~/.local/bin`), so no shell startup file is edited. Hashi makes them when an account is added or signed in again, deletes them when it is removed or its folder is gone, and catches up on `hashi account list`, `hashi account commands` and when the server starts, so accounts added before this release get theirs too. The names are patterns you can change, or empty to turn one off: `n` in the dashboard's Accounts popup, or `hashi account commands --agent codex --name 'cx-{name}' --skip-name ''`; renaming deletes the old commands. A command is never made over a file Hashi did not write or with the name of a program elsewhere on your PATH. `hashi uninstall` removes them, and `HASHI_NO_ACCOUNT_COMMANDS=1` turns them off.

## [0.3.4] - 2026-11-10

### Added

- `d` on the dashboard's Commands view opens the selected row's details: what it is, how it works, what to know before using it and the same command from a terminal. `←` `→` step through the other rows and `Enter` runs it.

## [0.3.3] - 2026-11-09

### Added

- Two ways to share, both through a one-time link and both in the dashboard:
  - **Bridge share** (`hashi share`, New invite): the person sends prompts to your Hashi and your login never leaves your machine. New `--stop-at PCT` (`stopAtPercent`, "stop at" in the share and token forms) refuses their prompts once your account has used that share of its 5-hour or weekly quota, with when it opens again; a friend's Hashi fails over on it like any other limit.
  - **Login share** (`hashi share-login`, Share a login): for someone you trust, the link hands over the key of an API-key or OAuth-token account, so they run the agent on their own machine. Nothing is enforced; instead their Hashi reads your usage through the link, both of you get a reminder at `--remind-at` percent, and their copy is switched off when `--days` ends. `hashi connect` and the dashboard's Connect add it as an account.
- `hashi account usage` and the Accounts popup show who shared a login, the agreed percent and when the share ends.
- **Pools** (`hashi pool`, Pool in the dashboard): host a party where people lend their agents and everyone uses them. The host opens one join link; each member's Hashi pulls prompts from the host and runs them with its own accounts, stop and end time, so members need no public URL and their keys never leave their machine. `GET /v1/pool` (and `hashi pool status`) shows everyone, online or not, and each agent's headroom. Each member gets a pool token, saved as a remote so `hashi ask --remote <host>-pool` works.
- Every share (`hashi share`, `share-login`, `pool open` and their dashboard forms) takes a route: `--via auto|cloudflare|tailscale|lan|local` ("link via" in the forms). The dashboard starts the Cloudflare tunnel or Tailscale when the route is not up; `lan` makes an `http://<wifi address>:<port>` link when the server listens on the network.
- `hashi connect` and pool links accept plain `http://` on a private network (192.168.x.x, 10.x.x.x, 172.16-31.x.x, `.local`), for sharing on one wifi.

### Fixed

- `hashi account usage` and the dashboard show a plain reason when Codex usage cannot be read (sign-in rejected, busy, down or unreachable) instead of the raw error, and a rejected sign-in says to run `hashi account login codex <name>`.
- When the Claude usage endpoint, a friend's bridge, the GitHub release check or a download cannot be reached, Hashi says what went wrong in plain words; the URL, response body and OS error go to the log.

## [0.3.2] - 2026-11-06

### Fixed

- A request for more sent within an hour of the last (`429 request_too_soon`) now says when the next one can be sent: `Retry-After`, `retryAfter` in the body and "try again in N minutes" in the message, like the other rate-limited errors. MCP `request_more` reports the same wait.

## [0.3.1] - 2026-11-06

### Added

- A `429` whose wait is known sends `Retry-After` and the same seconds as `retryAfter` in the error body: `limit_reached` (until the cap resets), the per-token rate limit, failed sign-in, invite and ask-access attempts from one address, and an account refresh within a minute of the last.
- `GET /v1/models/{model}`, so OpenAI clients that retrieve a model before using it work.
- MCP tools have a `title`, `annotations` and an input schema that describes and bounds every argument (`prompt` up to 32 KB, `model` up to 128 safe characters, request ids, the agent names `request_more` takes).

### Changed

- Usage is read for every enabled login and remote account, not only the ones a token or project already uses, so a new account shows what it has left right away.
- The dashboard Accounts popup reads the selected account's usage now with `u`, as `hashi account usage --refresh` does.
- The claim, ask-access and My access pages were reworked from screenshots at phone and desktop sizes in light and dark mode:
  - agents show as chips with their real names, and only the ones this machine can run;
  - dates read "in 24 hours (Sat, Nov 7, 12:19 PM)";
  - the terminal setup folds away under the main button;
  - after claiming, the page says "You're in" and lists numbered steps;
  - a broken link says "This link doesn't work";
  - commands scroll on one line instead of breaking mid-word, and the page never gets wider than a phone screen;
  - every control is at least 44px tall on touch screens.
- OpenAI-compatible responses carry every field the chat completions spec requires: `system_fingerprint`, `logprobs` and `message.refusal` (all `null`). With `stream_options.include_usage`, every chunk but the last has `usage: null`. Errors set `param` to `model` or `messages` where it applies.
- `GET /v1/models` and the MCP `ask_<agent>` tools list only the agents this bridge can run for the token: installed here, or served by a remote account. Before, MCP also listed agents that were not installed and could only answer `503`.
- The MCP `instructions` and tool descriptions say how long a call waits, when to call `get_result` and `cancel`, and not to send a prompt again. A refused tool call says what to do next and ends with its error code, and a failed run names its request.
- Error messages say what to do next: the agents a token may use when it names another, the size of an oversized prompt, which characters a model name may use, when a cap resets, and how long to wait after a rate limit. `GET /` links to the API reference. No error code changed.
- `hashi llm` (also `hashi guide`) prints a Markdown guide that an LLM agent such as Claude Code, Codex or Cursor can follow to run Hashi for you: rules (secrets stay secret, ask before destructive commands, read first), the commands for common tasks, a command reference generated from the binary's own flags, and HTTP API pointers. `--output <file>` writes it readable only by you. The dashboard's Commands view copies it with "Copy guide for an LLM".
- `--json` on `token list`, `invite list`, `account list`, `project list`, `requests list` and `audit`, with the same camelCase fields as the matching API listing. None prints a secret.
- Examples at the end of `--help` for `share`, `token`, `account`, `project`, `requests`, `connect` and `llm`.
- `hashi token update` and `hashi token revoke` also take a token's name, when no other token has it.
- The `token list`, `invite list`, `account list`, `project list` and `audit` tables size each column to its widest cell, so a long name no longer pushes the columns after it out of line. A token or invite that may use every agent shows `all`. `invite list` also shows the agents, project and expiry.
- Empty lists say how to add the first one, errors for an unknown token, invite, project or request name the command that lists them, and refusing to revoke the last admin token says how to make another.
- `hashi requests open` builds a local-only link with the port saved from the dashboard, as `hashi share` does, instead of always 7878.
- User-given names, labels and audit text in the CLI tables pass through the same terminal-safe filter as the dashboard.
- The dashboard was reviewed frame by frame at 80x24, 120x40 and 200x60 on a realistic store, and reworked where it broke:
  - footers drop whole hints from the end instead of cutting the line, always keep `?` help and `q` quit, and give room to a toast only while it shows;
  - the Tokens table and the Accounts popup drop their least needed columns on a narrow terminal, so the name stays (it used to vanish at 80 columns); the count column reads "req today";
  - the header leaves out the logo below 126 columns, so the tagline no longer runs into the server status, and says when no agent is found;
  - the Commands view shows whole titles at 80 columns, with the terminal command in the panel below, and keeps the key of a row that has one visible against the many `enter` rows;
  - the key list fits a 24-row terminal, and long key hints in popups wrap between hints;
  - on a first run with no coding agent, Overview says to install Claude Code or Codex and points at Doctor; with an agent, it points at `a` to ask;
  - empty lists say what to press next, and the Accounts popup no longer asks to read usage when there are no accounts;
  - agent cards drop the duplicated name from the version, put accounts and the best headroom on their own line and label the run-time bars;
  - Doctor wraps details under themselves and, on a short terminal, keeps its keys on screen with a note of what was cut;
  - the accept and decline popups use the same frame and key chips as every other popup.
- The dashboard benchmark (`tests/tui_bench.rs`) fills the store with 100 tokens, the most it allows; it failed at 200.

## [0.3.0] - 2026-11-06

### Upgrading from 0.2

- **The database migrates itself.** The first start runs the migrations from schema version 3 to 9 (accounts, projects, usage, access requests, remote accounts and a project usage index) in one transaction. From this release on, a binary refuses a database newer than it supports, naming both versions. A 0.2 binary has no such check and would open a version 9 database it does not understand, so back up `~/.hashi` before upgrading if you may go back. `hashi update --version` refuses a downgrade unless you pass `--allow-downgrade`.
- **Claude Code runs use `--output-format stream-json --verbose`.** The answer, session and usage come from the `result` message as before. The `rate_limit_event` messages now feed the account's usage and its limited state.
- **Stateful conversations are kept per token, agent, account and project.** Existing ones move to the `default` account and carry on.
- **New defaults.** On start Hashi asks GitHub for the latest release at most once a day (`--no-update-check` or `HASHI_NO_UPDATE_CHECK=1` turns it off). While it runs it reads Claude Code and Codex quota every five minutes and after each run (`--no-usage-poll` or `HASHI_USAGE_POLL=0`). Accounts are chosen `in-order` and runs move off one at 90% used (`--account-strategy`, `--rotate-at`). An account that hits its limit without a reset time is skipped for 60 minutes (`--account-cooldown`). Crush, access requests and `shareUsage` stay off until you turn them on.

### Added

**Going public**

- Press `t` in the dashboard to start a Cloudflare quick tunnel on a running server, and stop it from the Commands view. A running server writes its public URL to an owner-only `~/.hashi/public-url`, removed on shutdown, which `hashi share` uses when `--public-url` is not set. An open invite popup switches to the public link when the tunnel comes up.
- Tailscale Serve and Funnel: `hashi serve --tailscale serve|funnel` (`HASHI_TAILSCALE`, `--tailscale-port`, `--tailscale-bin`), or the Commands view, publishes Hashi at `https://<machine>.<tailnet>.ts.net`. Hashi checks the sign-in, refuses a port that serves something it did not set up, shows the approval link Tailscale prints, and on stop runs only the matching `off` command, never `reset`. One public route runs at a time; starting another asks to switch.
- `hashi install cloudflared|tailscale|crush [--yes]` shows the vendor's documented install command for this platform and runs it after you confirm, or prints the download page. In the dashboard a missing helper opens a popup that runs (`enter`), copies (`c`) or cancels (`esc`) it. `--cloudflared-bin` / `HASHI_CLOUDFLARED_BIN` point at a specific `cloudflared`, and `hashi doctor` reports Tailscale and its sign-in.

**Friends and invites**

- `hashi connect <invite link> [--name] [--setup claude]` redeems an invite from a terminal and saves the token in an owner-only `~/.hashi/remotes.json`. `hashi remote list|remove` manages saved bridges, and `hashi ask --remote [<name>] "<prompt>"` asks one.
- The claim page offers one setup command (sh or PowerShell) that installs Hashi and runs `hashi connect`, through the new `HASHI_CONNECT` installer variable. After claiming it has Add to Cursor and Add to VS Code buttons and a `codex mcp add` snippet.
- The invite popup shows the link as a QR code when the terminal has room.
- Access requests, off until you open a link. `hashi requests open [--project] [--note] [--hours N]` prints a public `<base>/ask-access/hashi_ask_<code>` link (open for 168 hours by default), and `hashi requests list|accept|decline|close` decides requests. Accepting creates a single-use invite only the requester can collect, with your flags over what they asked for. `hashi request <link>` and `hashi request status` ask from a terminal. Token holders ask for more limits, agents or time from `/me`, `POST /v1/me/requests` or the MCP tool `request_more`. Admin API: `GET|POST|DELETE /v1/request-link`, `GET /v1/access-requests` and `POST /v1/access-requests/{id}/accept` or `/decline`.

**Accounts, projects and usage**

- Several accounts per agent. `hashi account add|login|list|disable|enable|clear-limit|remove` manages login accounts (a directory under `~/.hashi/accounts/` that the agent signs in to itself) and key accounts (an API key in an owner-only file, read without echo or from `--key-env`). Tokens, invites and projects take an ordered list per agent (`--account claude=work,personal`, `accounts` in the API). When a run hits the agent's usage limit, the account is skipped until the reset time in the message, or for `--account-cooldown`, and the prompt runs again on the next account. Admin `GET /v1/accounts` lists accounts; token holders never see their names.
- Projects: a folder with its own agents, accounts, default agent, models and note. `hashi project add|update|list|remove`, `--project` on `token create|update` and `share`, and admin `GET|POST /v1/projects` and `PATCH|DELETE /v1/projects/{name}`. Bound tokens run in the folder, with the agents both allow and accounts inside the project's, so binding can never widen access. Hashi never writes to, moves or deletes a project folder, and token holders see only its name.
- Usage limits. Hashi keeps each account's quota windows. It reads Claude Code accounts from the OAuth usage endpoint (the token read only, never refreshed or logged) and Codex accounts through a short-lived `codex app-server`. `hashi account usage [--agent] [--refresh] [--json]` shows a bar per window with its reset; `hashi account list`, `GET /v1/accounts` and the agent cards show the headroom; admin `POST /v1/accounts/{agent}/{name}/refresh` reads one now.
- Account strategies `in-order`, `most-left` and `soonest-reset`, and a rotate threshold (`--rotate-at`, 50 to 99), per project or as server defaults. A stateful conversation stays on its account until that account reaches the threshold or its limit.
- Remote accounts: `hashi account add <agent> <name> --remote <remote> [--remote-agent <agent>]` turns access a friend shared with you into one of your accounts, usable in account lists, failover and strategies. Their limits mark the account limited, a paused or revoked token disables it with the reason shown, and an unreachable bridge fails over to the next account. Forwarded prompts carry `X-Hashi-Hops` and `X-Hashi-From`, and a bridge refuses a prompt past 2 hops (`508 hop_limit`) or one it sent itself (`508 loop`). `GET /v1/status` reports the bridge's `instance` id.
- `shareUsage` on tokens and invites (`--share-usage`), off by default, adds `agentUsage` to `/v1/me`: per agent the headroom left, the next reset and the state, never account names or windows.

**API**

- OpenAI-compatible API: `POST /v1/chat/completions` and `GET /v1/models` take Hashi tokens and go through the same queue, limits and scopes as `/v1/ask`. `model` picks the agent, and `agent/model` (such as `claude/opus`) the agent's own model. `stream: true` sends server-sent events with keep-alives and the answer as one chunk. Errors use OpenAI's shape.
- Cancel a request: `DELETE /v1/ask/{id}`, the MCP `cancel` tool and `x` in the dashboard Requests view. A running request has its whole process tree stopped; a finished one answers `409`.

**Agents**

- Crush (`crush`, 0.100.x) as a seventh agent, opt-in with `--enable-crush` / `HASHI_ENABLE_CRUSH`. Before each run Hashi writes a config that removes every tool except `view`, `ls`, `glob`, `grep`, `todos` and the read-only LSP queries. It refuses a workspace with a Crush project config or a `.env`, any run while `/etc/crush/crush.json` exists, and a newer release unless `--crush-allow-newer` is passed. Every Crush account, `default` included, has its own config under `~/.hashi/accounts/crush/`.
- Upstream tracking: `docs/research/verified-versions.json` records the release each agent was last verified against, shown in the README's "Verified with" column. `hashi doctor`, the startup log, the agent cards and `GET /v1/stats` say when an installed agent is newer, and a weekly workflow opens an `upstream` issue for each new release.

**Lifecycle and dashboard**

- `hashi update [--check] [--version vX.Y.Z] [--yes]` downloads the release for your platform, verifies its checksum and the new binary's version, and replaces the running one. The dashboard header and `hashi doctor` say when a release is out.
- `hashi uninstall [--dry-run] [--keep-data] [--yes]` removes the binary, the data directory and the Windows PATH entry after you type `uninstall`, and prints how to remove cloudflared, Tailscale and the MCP entries of your saved remotes. It refuses while a server runs, and refuses a data directory that is a symlink or holds files Hashi did not create.
- Dashboard tab 5, Commands, lists every action with its key, its state and the command that does the same; a test fails when a subcommand has no place in the dashboard. New popups: ask an agent here or on a bridge (`a`), edit a token (`e`), accounts, projects (with their strategy), bridges I use, access requests (with a badge for the number waiting), the audit log, doctor, a diagnostic report and installing an update.
- Move a running server to another port or bind address from the Commands view without stopping it. The choice can be kept in an owner-only `~/.hashi/settings.json`, which a flag or variable overrides.

### Changed

- Faster request path and dashboard: cached prepared statements, lighter `GET /v1/stats`, `/v1/projects` and `/v1/accounts`, agent versions probed at once on startup, agent output decoded without a copy, no request copy held by long polls and streams, a watchdog that reads memory only for a run's own processes, and a dashboard that reads the store once per refresh instead of once per frame. The numbers are in `docs/performance.md`.
- Release binaries keep their symbol names (`strip = "debuginfo"`), so crash reports name the functions. They are about 1.4 MB larger on macOS and 2.1 MB on Linux.
- Runs on a non-default account drop the owner's credential and provider variables for that agent before setting the account's own.

### Fixed

- `curl -d` without a JSON content type got a `400` on every authenticated endpoint, the README's examples included. A body with an `Authorization` header and no content type, or curl's form default, is now read as JSON.
- A Ctrl-C meant for an install, sign-in or update run from the dashboard no longer shuts Hashi down once the dashboard is back, and keys typed ahead are dropped before and after such a run.
- The dashboard picked up finished background work only while nothing animated, so a bridge's answer could wait forever behind its own spinner.
- OpenAI-compatible streams on a request that stays queued now end with an error chunk and `[DONE]` when the token stops being active, or after twice the run timeout plus 10 seconds.
- The ask-access page no longer forgets the pickup secret when a status check fails on the network or with a server error. It retries after 10, 20, 40 and then every 60 seconds.
- Requests for more from token holders have their own cap of 200 pending, so new-person requests can no longer crowd them out.
- A busy or unreachable remote bridge (`too_many_pending`, `rate_limited`, no answer) backs the account off for 5 seconds, doubling up to 60, instead of marking it limited for an hour. Only `limit_reached` marks it limited until its reset.
- Token holders see "the run failed" instead of a remote bridge's own error. A prompt that would pass the friend's 64 KB body limit once forwarded is refused locally with `413 too_large`.
- A limit message with a huge relative time or a far date panicked while its reset time was read. Reset times are now capped at 31 days, and one that cannot be represented falls back to the cooldown.
- Qwen Code's reset time has no year, and one a few seconds ago could shelve an account for 31 days. Codex's "Try again at 3:45 PM." used a UTC offset cached at start, which went stale across a daylight saving change.
- A poll that reported a limit far in the future kept the account limited forever. `hashi account clear-limit` (`c` in the Accounts popup) now also clears a mark by hand.
- A run or usage read that finished after its account was removed recreated the account. The hourly maintenance now also deletes what removed accounts left behind.
- The docs said the MCP tools carry no usage figures; `my_usage` includes `agentUsage` when `shareUsage` is on.

### Security

- One server per home. A server holds an OS lock on `~/.hashi/server.lock` before it touches the database, so a second `hashi serve` refuses to start instead of requeueing the first one's prompts. Only the lock holder writes and removes `server.pid`.
- A database with a newer schema than this build supports is refused instead of opened, and migrations run in one `BEGIN IMMEDIATE` transaction, so two processes opening an old database at once both succeed.
- Release archives get GitHub build provenance attestations; check one with `gh attestation verify <archive> --repo sidharthpunathil/hashi`. The docs say what the checksum proves (an intact download) and what the attestation proves (where it was built).
- `hashi update` refuses a downgrade without `--allow-downgrade`, needs `--yes` without a terminal, downloads over HTTPS only and installs only a regular file from the archive, never a symlink or hard link.
- The installers accept `HASHI_CONNECT` only in the shape the claim page builds and pass it after `--`. The PowerShell installer splats its arguments so Windows PowerShell 5.1 cannot split them.
- Runs on non-default accounts also drop the variables that point an agent at another provider, endpoint or cloud account (Bedrock, Vertex, Foundry, base URLs, `AWS_*`, `GOOGLE_CLOUD_*` and more, listed in `docs/security-model.md`). Key accounts point `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or `GEMINI_CLI_HOME` at their own directory, so your settings cannot outrank the key, and non-default Gemini accounts set `advanced.ignoreLocalEnv`, so your `~/.env` never loads.
- Crush's config check runs again under the data directory's lock right before each spawn and walks to the filesystem root, and pre-release or suffixed Crush versions need `--crush-allow-newer`.
- The `codex app-server` that reads quota runs in its own process group (a Job Object on Windows) that is killed on exit, timeout or drop, has its output lines capped at 1 MiB, and runs in a fresh owner-only directory.
- `hashi remote remove` switches off the accounts that used the remote, and each forward checks the bridge's instance id recorded when the account was added, so a reused remote name cannot redirect prompts.
- Token, pending invite and project caps are enforced inside the transaction that inserts the record. Removing a project with `--unbind` fails prompts still queued under it instead of running them in the token's workspace.
- Access requests have abuse limits: per-address rate limits, throttled lookups of codes and pickup secrets, at most 50 pending per link and 200 in total, 3 open per token and one an hour, a 7-day expiry, and every field length-limited and stripped of control, bidi and zero-width characters.
- The dashboard removes bidi and other format characters from every cell and popup title, refuses token names with them, makes a copied bridge answer safe to paste, and wipes a key typed into the add-account form when it closes.
- Tailscale approval links are shown only for `login.tailscale.com` or `HASHI_TAILSCALE_CONTROL_HOST`, a quick tunnel URL must be exactly `https://<label>.trycloudflare.com`, and shutdown runs `tailscale ... off` only while the port still points at this server. `CF-Connecting-IP` is trusted from loopback only while a tunnel is configured or running.
- `settings.json` refuses port `0`, and the dashboard header warns while the server listens outside loopback. On Windows, `hashi uninstall` compares canonical paths to find the running binary.

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
