# Configuration

Pass flags to `hashi serve` (for example `hashi serve --port 9000 --tunnel`). Running bare `hashi` starts the server with defaults and environment variables. Every flag also reads an environment variable.

| Flag | Env | Default |
|---|---|---|
| `--home` | `HASHI_HOME` | `~/.hashi` |
| `--bind` | `HASHI_BIND` | `127.0.0.1`, or the one saved from the dashboard in `~/.hashi/settings.json` (see [settings.json](#settingsjson)) |
| `--port` | `HASHI_PORT` | `7878`, or the one saved from the dashboard in `~/.hashi/settings.json` |
| `--tunnel` | `HASHI_TUNNEL` | off |
| `--tailscale serve\|funnel` | `HASHI_TAILSCALE` | off. Go public through Tailscale Serve (your tailnet) or Funnel (the internet); see [remote access](remote-access.md#tailscale-serve-and-funnel). Not with `--tunnel` |
| `--tailscale-port` | `HASHI_TAILSCALE_PORT` | `443`. HTTPS port for `--tailscale` (Funnel: 443, 8443 or 10000) |
| `--tailscale-bin` | `HASHI_TAILSCALE_BIN` | found on `PATH`. Also read by `hashi doctor` |
| `--cloudflared-bin` | `HASHI_CLOUDFLARED_BIN` | found on `PATH`. Also read by `hashi doctor` |
| `--tunnel-token` | `HASHI_TUNNEL_TOKEN` | none. Runs a named tunnel; needs `--public-url`. Prefer the variable: the flag shows in the process list, and Hashi warns |
| `--trust-proxy` | `HASHI_TRUST_PROXY` | off. Read client addresses from `X-Forwarded-For` on loopback connections |
| `--cors-origin` | `HASHI_CORS_ORIGINS` (comma separated) | none. Origins allowed to call Hashi from a browser |
| `--allowed-host` | `HASHI_ALLOWED_HOSTS` (comma separated) | none. Extra host names accepted over plain HTTP |
| `--public-url` | `HASHI_PUBLIC_URL` | none. Base for invite links, for example `https://hashi.example.com`. Without it, `hashi share` uses the running server's public URL from `~/.hashi/public-url` when there is one |
| `--tls-cert`, `--tls-key` | `HASHI_TLS_CERT`, `HASHI_TLS_KEY` | none. Serve HTTPS with these PEM files |
| `--name` | `HASHI_NAME` | `Hashi`. Shown on invite and My access pages |
| `--headless` | | off (on when stdout is not a terminal) |
| `--no-update-check` | `HASHI_NO_UPDATE_CHECK` | off. Set to skip the daily check for a newer release (also skipped by `hashi doctor`) |
| | `HASHI_REDUCED_MOTION` | off. Set to `1` to freeze dashboard animation |
| `--default-agent` | `HASHI_DEFAULT_AGENT` | `claude` |
| `--concurrency` | `HASHI_CONCURRENCY` | `1` run per agent |
| `--max-running` | `HASHI_MAX_RUNNING` | total memory in GiB / 8, between 1 and the CPU count. Runs at once across all agents |
| `--memory-reserve` | `HASHI_MEMORY_RESERVE` | the larger of `2G` and `20%`. Kept free for you; new runs wait while less would be left (`0` disables) |
| `--run-memory-limit` | `HASHI_RUN_MEMORY_LIMIT` | the larger of `2G` and `40%`. One run's whole process tree is stopped above this |
| `--disk-floor` | `HASHI_DISK_FLOOR` | `1G`. New runs wait while the volume holding the Hashi home has less free (`0` disables) |
| `--nice` | `HASHI_NICE` | `10`. Lower priority for agent runs, `0` to `19` (`0` disables; on Windows any value above `0` means below-normal priority) |
| `--timeout` | `HASHI_TIMEOUT` | `300` seconds |
| `--rate-limit` | `HASHI_RATE_LIMIT` | `60` per minute per token (`0` disables) |
| `--max-connections` | `HASHI_MAX_CONNECTIONS` | `512` open connections; more wait until one closes |
| `--queue-limit` | `HASHI_QUEUE_LIMIT` | `100` pending requests |
| `--retention-days` | `HASHI_RETENTION_DAYS` | `14` |
| `--account-cooldown` | `HASHI_ACCOUNT_COOLDOWN` | `60` minutes. How long an account that hit its usage limit is skipped when the agent's message gives no reset time (1 to 10080) |
| `--claude-bin` | `HASHI_CLAUDE_BIN` | found on `PATH` |
| `--codex-bin` | `HASHI_CODEX_BIN` | found on `PATH` |
| `--gemini-bin`, `--opencode-bin`, `--qwen-bin`, `--cursor-bin` | `HASHI_GEMINI_BIN`, `HASHI_OPENCODE_BIN`, `HASHI_QWEN_BIN`, `HASHI_CURSOR_BIN` | found on `PATH` |
| `--enable-crush` | `HASHI_ENABLE_CRUSH` | off. Offer Crush, which is opt-in: while off it is not probed, not listed and a request for it answers "Crush is not installed or not enabled". See [security model](security-model.md#crush) |
| `--crush-bin` | `HASHI_CRUSH_BIN` | found on `PATH` |
| `--crush-allow-newer` | `HASHI_CRUSH_ALLOW_NEWER` | off. Run a Crush release newer than 0.100.x, the newest whose tool list Hashi has checked. Without it such a release is refused |
| `--max-turns` | `HASHI_MAX_TURNS` | `10` (Claude) |
| `--claude-permission-mode` | `HASHI_CLAUDE_PERMISSION_MODE` | Claude's default |
| `--codex-sandbox` | `HASHI_CODEX_SANDBOX` | `read-only` |

Sizes take binary units or a share of the total: `512M`, `4G`, `1.5G`, `25%`.

Logs always go to `~/.hashi/logs/hashi.log` with secrets redacted (rotated past 5 MB, three old files kept), and with `--headless` also to stderr. Set `HASHI_LOG=hashi=debug` for more detail.

## Commands

```bash
hashi                                      # start the bridge with the dashboard
hashi serve --headless                     # start it with plain log lines instead
hashi doctor                               # agents, cloudflared, tailscale and its sign-in
hashi install cloudflared                  # show the official install command, run it on yes
hashi install tailscale --yes              # run it without asking
hashi install crush                        # Crush, from its README; then start with --enable-crush
hashi token create ci --agents codex --expires-in-days 30
hashi token list                           # status and today's use per token
hashi token update hashi_1a2b3c4d --pause  # or --resume, --daily-requests 100, --clear-limits
hashi token revoke hashi_1a2b3c4d
hashi share alice --agents claude          # one-time invite link
hashi account add claude work              # another login for an agent (see Accounts)
hashi project add api ~/code/api           # a folder to share (see Projects)
hashi share bob --project api              # an invite that works in that folder
hashi invite list                          # pending, claimed and expired invites
hashi audit                                # who changed what
hashi ask "What does this repo do?" -a claude   # one prompt, no server
hashi report                               # redacted diagnostics for a bug report
hashi update                               # install the latest release (--check, --version v0.3.0, --yes)
hashi uninstall                            # remove the binary and ~/.hashi (--dry-run, --keep-data, --yes)
```

`hashi install` prints the vendor's documented command for your platform (Homebrew on macOS, Cloudflare's package repository or Tailscale's install script on Linux) and runs it with your terminal attached, so `sudo` can prompt. Where the vendor documents no command for your system or its package manager is not on `PATH` (Windows, for both helpers), it prints the download page instead. Without a terminal it refuses unless you pass `--yes`. The commands and their sources are listed in [remote access](remote-access.md#installing-the-helpers).

The update check sends one request to api.github.com a day, carrying only the Hashi version in its User-Agent (`hashi/0.2.0`), and caches the answer in `~/.hashi/update-check.json`. `hashi update` downloads the archive for your platform from the GitHub release, verifies its SHA-256 checksum, checks that the new binary reports the expected version and swaps it in; restart Hashi afterwards. It refuses a binary installed by Cargo (update that with `cargo install --locked --git https://github.com/sidharthpunathil/hashi --force`) or one in a directory you cannot write to, and asks before replacing unless you pass `--yes`.

### Accounts

An account is another login, or an API key, for one agent. Tokens and invites choose which accounts they use per agent, in order; an agent they do not list uses `default`, your own login, exactly as before.

```bash
hashi account add <agent> <name>                     # a login account: prints the sign-in command, runs it after you confirm
hashi account add <agent> <name> --key-stdin         # an API key, typed or pasted without echo (or piped in)
hashi account add <agent> <name> --key-env VAR       # an API key from an environment variable
hashi account add claude bot --kind oauth-token --key-stdin   # a token from `claude setup-token`
hashi account login <agent> <name>                   # sign a login account in again
hashi account list                                   # kind, signed in, state, last use, runs today, tokens
hashi account disable <agent> <name>                 # skip it until `hashi account enable`
hashi account remove <agent> <name>                  # asks first (--yes), deletes it and takes it off every token and invite

hashi token create ci --account claude=work,personal --account codex=ci
hashi token update hashi_1a2b3c4d --account claude=personal   # replaces that agent's list
hashi token update hashi_1a2b3c4d --clear-accounts            # every agent back on default
hashi share alice --account claude=work
```

Names are 1 to 32 characters from `a-z`, `0-9` and `-`; `default` is reserved. `--account` is repeatable, takes `<agent>=<name>[,<name>...]` and refuses names that do not exist. In the dashboard, the new token and share forms have an accounts field that takes the same form (`claude=work,personal codex=ci`).

| Agent | Login account (directory variable, sign-in command) | Key account (variable) |
|---|---|---|
| Claude Code | `CLAUDE_CONFIG_DIR`, `claude auth login` | `--kind api-key`: `ANTHROPIC_API_KEY`; `--kind oauth-token`: `CLAUDE_CODE_OAUTH_TOKEN` |
| Codex | `CODEX_HOME`, `codex login` (or `codex login --device-auth`) | `CODEX_API_KEY` |
| Gemini CLI | `GEMINI_CLI_HOME`, `NO_BROWSER=true gemini`, then Sign in with Google | `GEMINI_API_KEY` |
| opencode | `XDG_DATA_HOME`, `opencode auth login --standalone` | not supported |
| Qwen Code | `QWEN_HOME`, `qwen`, then `/auth` | not supported |
| Cursor CLI | not supported | `CURSOR_API_KEY` |
| Crush | `HOME` with `CRUSH_GLOBAL_CONFIG`, `CRUSH_GLOBAL_DATA`, `CRUSH_CACHE_DIR` and `CRUSH_SKILLS_DIR` under it, `crush login` (Charm Hyper; `crush login copilot`, `openai` or `grok` for the others) | not supported |

Crush's `default` account is not your own Crush login: it is `~/.hashi/accounts/crush/default/`, so your `~/.config/crush` (with its MCP servers and hooks) never loads in a Hashi run. Sign it in with `hashi account login crush default`, or leave your provider variables (such as `ANTHROPIC_API_KEY`) in Hashi's environment, which the default account still sees. Other Crush accounts see none of the provider variables in Crush's README, and no Crush run sees an inherited `CRUSH_*` or `XDG_*` directory variable.

A login account is the directory `~/.hashi/accounts/<agent>/<name>/` (mode `0700`). `hashi account add` prints the sign-in command with that variable set and, on a terminal, runs it with the terminal attached once you confirm (`--yes` skips the question); without a terminal it only prints it. It then checks the result with `claude auth status --json` for Claude Code, and for the other CLIs that the CLI wrote something into the directory. Every run on the account sets the variable to the same absolute path, which matters to Claude Code on macOS, whose Keychain entry is tied to that exact string. Hashi never reads, copies or refreshes what the CLI keeps there.

A key account keeps its key in `~/.hashi/accounts/<agent>/<name>/key` (mode `0600`) and passes it in the variable above. The key is never accepted as an argument and never printed, logged or returned by the API.

**Failover.** A run uses the first account in the token's list that is enabled and not limited. When it fails with that agent's usage limit message, the account is marked limited until the reset time in the message, or for `--account-cooldown`, and the prompt goes back to the front of the queue to run on the next account. This is not counted as an interruption, and each account is tried at most once per prompt. With none left, the prompt fails with "every account for <agent> this token can use is at its limit until <time>". A successful run clears the account's limited mark. Messages recognised:

- Claude Code: the session, weekly and Opus limits, "Credit balance is too low" and "Request rejected (429)".
- Codex: "hit your usage limit", "out of credits" and "spend cap". The reset comes from "Try again at 3:45 PM." or "Try again at Nov 7th, 2026 3:45 PM.", read in this machine's time zone; "Try again later." uses the cooldown.
- Gemini CLI: "exhausted your daily quota", "exhausted your capacity", "quota exceeded", `QUOTA_EXHAUSTED`, `RATE_LIMIT_EXCEEDED` and "Please retry in Ns". N is the reset when it is over 300 seconds; a shorter wait uses the cooldown.
- Qwen Code: an exhausted token plan ("The quota will reset at MM-DD HH:MM:SS UTC.", the next such time), "free allocated quota exceeded", `insufficient_quota` and "Qwen OAuth free tier has been discontinued".
- opencode: its typed limit errors (`provider.rate-limit`, `provider.quota`, `provider.go-limit`, `provider.budget-limit`, `provider.model-budget-limit`, `provider.free-limit`, `provider.chatgpt-limit`), with `retryAfterMs` as the reset, and "usage limit reached".
- Crush: Crush retries provider errors itself and then reports the provider's own message, so only generic wording is matched: "rate limit" (also `rate-limit`, `rate_limit`), "too many requests", "quota", "usage limit", `resource_exhausted` and a bare 429. Crush reports no quota of its own.
- Cursor CLI: "usage limit", "rate limit", `resource_exhausted`, "quota", its limit codes (`FREE_USER_USAGE_LIMIT`, `PRO_USER_USAGE_LIMIT`, `FREE_USER_RATE_LIMIT_EXCEEDED`, `PRO_USER_RATE_LIMIT_EXCEEDED`, `RATE_LIMITED`, `GENERIC_RATE_LIMIT_EXCEEDED`) and a bare 429. "monthly cycle ends on M/D/YYYY" gives the reset date.

Limits are tracked only for tokens that list accounts for the agent.

**Sessions.** A stateful token keeps one conversation per agent and account, because the conversation lives in the account's directory. Failing over starts it fresh on the new account; `DELETE /v1/session/{agent}` clears it on every account.

**Seeing it.** Admins see the account that ran each request in `GET /v1/ask/{id}` and the dashboard's request detail, each token's accounts at the foot of the tokens view, and the number of accounts on each agent card. `GET /v1/accounts` returns the list with its state and counts.

### Projects

A project is a folder on this machine with its own agents, accounts and defaults. Bind tokens and invites to it instead of choosing those on each one.

| Command | What it does |
|---|---|
| `hashi project add <name> <path> [--agents claude,codex] [--account claude=work,personal]... [--default-agent claude] [--model claude=sonnet]... [--note ..]` | Add a project. `<path>` must be an existing folder, outside the Hashi home; symlinks are resolved. Without `--agents`, the installed agents |
| `hashi project update <name> [--path ..] [--agents ..] [--account ..]... [--clear-accounts] [--default-agent ..] [--clear-default-agent] [--model ..]... [--clear-models] [--note ..]` | Change a project. Refused if a bound token or pending invite would be left with accounts outside it |
| `hashi project list` | Projects with their bound tokens, prompts today and this month, spend this month, agents, accounts, models and folder |
| `hashi project remove <name> [--unbind]` | Remove a project. Refused while tokens or pending invites are bound, unless `--unbind` puts them back on their own workspace. The folder is never touched |
| `hashi token create <name> --project <name>` | Bind a new token. Also on `hashi share` |
| `hashi token update <id> --project <name>` or `--clear-project` | Bind a token, or put it back on its own workspace |

A bound token's run starts in the project folder. It may use only the agents both the token and the project allow. Its account list for an agent, when it has one, must be a subset of the project's list for that agent (`default` when the project has none), or the command is refused; without a list it uses the project's. When a prompt names no model, the project's model for that agent is used. Limits work as before. Stateful conversations are kept per token, agent, account and project. The name `default` is reserved for no project. The dashboard's token and share forms have a project selector, and the request and token details show the project.

### Connecting to someone else's Hashi

A person you invite does not run a server. With Hashi installed, these commands redeem the invite link and talk to your bridge:

```bash
hashi connect 'https://host/claim/hashi_inv_...'   # redeem the link and save the token
hashi connect '<link>' --name work                 # save it under your own name
hashi connect '<link>' --setup claude              # also run `claude mcp add` for it
hashi remote list                                  # saved bridges: names and URLs, never tokens
hashi remote remove work                           # forget one (the owner can still revoke the token)
hashi ask --remote "What does this repo do?"       # one saved bridge: the name can be left out
hashi ask --remote work "Explain main.rs" -a codex
```

- `hashi connect` accepts only `https://` links, or `http://` for `localhost` and loopback addresses, with the path `/claim/hashi_inv_<code>`. It saves `{name, baseUrl, token}` in `~/.hashi/remotes.json`, readable only by you, and prints the `claude mcp add` and `codex mcp add` commands, a first `hashi ask --remote` and your `/me` link. The name defaults to the server's name in lowercase with dashes.
- It never runs another program unless you pass `--setup claude`, which runs `claude mcp add --transport http <name> <base>/mcp --header "Authorization: Bearer <token>"` (set `HASHI_CLAUDE_BIN` to use a different executable).
- `hashi ask --remote` sends the prompt, waits up to 60 seconds at a time until it completes or fails, and prints the answer with control characters removed. A failure prints the server's error and exits non-zero. Without `-a`, the server's default agent answers.

## Dashboard keys

Run `hashi` in a terminal and the dashboard takes over the screen. When stdout is not a terminal, or with `--headless`, Hashi prints plain log lines instead.

Nearly everything the CLI does can be done from the dashboard, through the same library code. The two exceptions are `hashi serve` (the dashboard is the running server, and its flags apply only at start) and `hashi uninstall`. A test walks the command tree and fails when a new subcommand has no place in the dashboard.

| Key | Action |
|---|---|
| `1` `2` `3` `4` `5`, `Tab`, `h` `l` | Overview, Requests, Tokens, Logs, Commands |
| `↑` `↓` / `j` `k` | Move the selection |
| `PgUp` `PgDn` / `J` `K` | Scroll the request detail |
| `Enter` | Run the selected row (Commands view) |
| `a` | Ask an agent: the prompt popup (any view) |
| `n` / `d` | Create a token (Tokens and Commands views) / revoke one (Tokens view) |
| `e` | Edit the selected token: name, note, expiry, limits, accounts and project (Tokens view) |
| `s` | Share: create an invite link (Tokens and Commands views) |
| `p` | Pause or resume the selected token |
| `i` / `x` | Switch to pending invites / cancel one |
| `x` | Cancel the selected queued or running request, after confirmation (Requests view). Audited as `dashboard` |
| `c` | Copy the API URL, or the token or invite link being shown |
| `t` | Go public: start a Cloudflare quick tunnel on the running server. Without `cloudflared`, opens its install popup; with Tailscale public, asks to switch |
| `r` | Refresh |
| `?` | All key bindings |
| `Esc` | Close a popup |
| `q` | Quit and stop the bridge. Asks first while requests are in flight |
| `Ctrl-C` | Quit immediately |

No two actions in one view share a key. Forms move between fields with `↑` `↓` or `Tab`, change a choice with `←` `→` or space, save with `Enter` and cancel with `Esc`; a field that fails shows why next to it. Keys and other secrets typed into a form are masked and never reach the log.

### Commands view

Tab `5` lists everything the owner can do, grouped, one row each with its key (or `enter`), what it does, its state right now and the command that does the same from a terminal:

| Group | Rows | Same from a terminal |
|---|---|---|
| Go public | Cloudflare quick tunnel (`t`), Tailscale serve, Tailscale funnel, stop going public | `hashi serve --tunnel`, `--tailscale serve`, `--tailscale funnel` |
| Share | new invite (`s`), new token (`n`) | `hashi share`, `hashi token create` |
| Ask | ask an agent (`a`) | `hashi ask`, `hashi ask --remote` |
| Manage | accounts, projects, bridges I use, connect to a bridge | `hashi account`, `hashi project`, `hashi remote`, `hashi connect` |
| Server | port and bind address, copy the API URL (`c`) | `hashi serve --port --bind` |
| Install | cloudflared, tailscale, Crush | `hashi install cloudflared\|tailscale\|crush` |
| Maintenance | check for updates, install the update, doctor, diagnostic report, audit log | `hashi update [--check]`, `hashi doctor`, `hashi report`, `hashi audit` |
| Help | all key bindings (`?`) | `hashi --help` |

States include "live at <url>", "starting", "approve at <url>", "not installed" and "signed out". In an install popup, `enter` runs the command with the terminal attached and brings the dashboard back afterwards, `c` copies it and `esc` cancels. Ctrl-C while it runs goes to the install, not to Hashi. Signing an account in and installing an update step aside the same way.

### What each popup does

| Popup | Keys | Notes |
|---|---|---|
| Ask | `Tab` field, `←` `→` change, `Enter` new line in the prompt, `Ctrl-S` send | Choose this Hashi or a saved bridge, the agent, and (here) the account. A local prompt goes through the normal queue as the oldest active admin token, so limits and the queue apply and it shows in Requests, where its answer appears in the detail. Picking an account runs that prompt on it only. A bridge's answer opens in its own popup |
| Edit token | `↑` `↓` field, `←` `→` project, `Enter` save | Expiry takes days from now or `never`; empty keeps it. Empty limits mean none. Uses the same update as `hashi token update` and `PATCH /v1/tokens/{id}` |
| Accounts | `a` add, `l` sign in again, `d` disable, `e` enable, `x` remove | Adding a login account steps aside for the agent's own sign-in, as `hashi account add` does. A key account takes the key in a masked field and stores it owner-only |
| Projects | `a` add, `e` edit, `x` remove | The same checks as `hashi project add` and `update`. Removing a project with bound tokens puts them back on their own workspace; its folder is never touched |
| Bridges I use | `c` connect, `a` ask it, `x` forget | Names and URLs only, never tokens. Connect takes a pasted invite link and saves it as `hashi connect` does |
| Audit log | `↑` `↓`, `PgUp` `PgDn` | The last 500 entries, newest first |
| Doctor | `u` check for updates, `r` recheck | Agents with versions, Crush, cloudflared, Tailscale and its sign-in state, accounts, the update state, the home and the resource guard |
| Diagnostic report | `←` `→` change, `Enter` write | `include prompts` is off by default. Saves in the current folder (like `hashi report`) or in `~/.hashi/reports/`, owner-only, and shows the path with `c` to copy it |
| Install the update | `y` / `n` | Steps aside for `hashi update`, then says to quit and start Hashi again |
| Port and bind address | `↑` `↓` field, `←` `→` change, `Enter` move | See below |

The invite popup also shows the link as a QR code when the terminal has room, and asks for a larger one otherwise.

### Moving the server

The port and bind address row moves a running server without stopping it. Choose a port (1 to 65535) and a bind address: `127.0.0.1` (this machine), `0.0.0.0` or `::` (all interfaces), or a typed IP such as a Tailscale address.

1. The new address is bound first. If that fails (the port is in use, the address is not on this machine, permission denied), the form says so next to the field and the server keeps serving where it was.
2. The server serves on the new address with the same TLS settings, connection cap and limits, and the URL in the header changes.
3. The old address stops taking connections, finishes its open requests within the usual 10 second grace period, and closes.

Queued and running prompts carry on: they live in the queue, not the listener. A Cloudflare quick tunnel or a Tailscale route that is up is restarted on the new port. A named tunnel cannot be retargeted by Hashi: change the port of its hostname in the Cloudflare dashboard too. A bind outside loopback without TLS asks first, with the warning `hashi serve` prints.

### settings.json

With "keep for next start" on (the default), the move is saved in `~/.hashi/settings.json`, readable only by you:

```json
{
  "port": 9000,
  "bind": "127.0.0.1"
}
```

At startup each value comes from, in order: its flag (`--port`, `--bind`), its environment variable (`HASHI_PORT`, `HASHI_BIND`), `settings.json`, then the default (`7878`, `127.0.0.1`). When a flag or variable is set, the form says it wins over the saved value at the next start. A file that cannot be read is ignored with a warning in the log. `hashi uninstall` counts `settings.json` and `reports/` as Hashi's own.

Running requests count up their elapsed time, and every status pairs a symbol with its colour (`✓` done, `✗` failed, `◌` queued, a spinner for running). Body text uses your terminal's own foreground colour, so light and dark themes both stay readable, and `HASHI_REDUCED_MOTION=1` freezes the animation.

## Installer options

| Variable | Effect |
|---|---|
| `HASHI_VERSION=v0.2.0` | Install a specific release instead of the latest |
| `HASHI_INSTALL_DIR=/some/dir` | Install somewhere else |
| `HASHI_NO_DOCTOR=1` | Skip the `hashi doctor` check at the end |
| `HASHI_CONNECT='<invite link>'` | After installing, run `hashi connect` with this link instead of `hashi doctor`. The installer fails with a clear message if the link is used up or expired |

```bash
curl -fsSL https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.sh | HASHI_VERSION=v0.2.0 sh
```

```powershell
$env:HASHI_VERSION = "v0.2.0"; irm https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.ps1 | iex
```

The claim page builds the one-command setup for an invited person from its own link:

```bash
curl -fsSL https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.sh | HASHI_CONNECT='https://host/claim/hashi_inv_...' sh
```

```powershell
$env:HASHI_CONNECT='https://host/claim/hashi_inv_...'; irm https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.ps1 | iex
```

## Reporting a bug

```bash
hashi report                     # writes hashi-report-<time>.md in this directory
hashi report --stdout            # print it instead
hashi report --include-prompts   # add the first 500 characters of failed prompts
```

The report holds the version, OS, agent and cloudflared detection, `HASHI_*` settings, database counts, the last 20 failed requests, the last 200 log lines and any crash file. Tokens, invite codes, provider keys and your home directory are redacted, and prompts are left out unless you ask for them. Nothing is uploaded: the command prints a link to a prefilled GitHub issue, and you attach the file yourself after reading it.

If Hashi crashes, it saves `~/.hashi/logs/crash-<time>.txt` and the next report includes it. API errors with status 500 carry an `errorId` that matches a line in the log; quote it in the issue.
