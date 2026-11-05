# Configuration

Pass flags to `hashi serve` (for example `hashi serve --port 9000 --tunnel`). Running bare `hashi` starts the server with defaults and environment variables. Every flag also reads an environment variable.

| Flag | Env | Default |
|---|---|---|
| `--home` | `HASHI_HOME` | `~/.hashi` |
| `--bind` | `HASHI_BIND` | `127.0.0.1` |
| `--port` | `HASHI_PORT` | `7878` |
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
| `--claude-bin` | `HASHI_CLAUDE_BIN` | found on `PATH` |
| `--codex-bin` | `HASHI_CODEX_BIN` | found on `PATH` |
| `--gemini-bin`, `--opencode-bin`, `--qwen-bin`, `--cursor-bin` | `HASHI_GEMINI_BIN`, `HASHI_OPENCODE_BIN`, `HASHI_QWEN_BIN`, `HASHI_CURSOR_BIN` | found on `PATH` |
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
hashi token create ci --agents codex --expires-in-days 30
hashi token list                           # status and today's use per token
hashi token update hashi_1a2b3c4d --pause  # or --resume, --daily-requests 100, --clear-limits
hashi token revoke hashi_1a2b3c4d
hashi share alice --agents claude          # one-time invite link
hashi invite list                          # pending, claimed and expired invites
hashi audit                                # who changed what
hashi ask "What does this repo do?" -a claude   # one prompt, no server
hashi report                               # redacted diagnostics for a bug report
hashi update                               # install the latest release (--check, --version v0.3.0, --yes)
hashi uninstall                            # remove the binary and ~/.hashi (--dry-run, --keep-data, --yes)
```

`hashi install` prints the vendor's documented command for your platform (Homebrew on macOS, Cloudflare's package repository or Tailscale's install script on Linux) and runs it with your terminal attached, so `sudo` can prompt. Where the vendor documents no command for your system or its package manager is not on `PATH` (Windows, for both helpers), it prints the download page instead. Without a terminal it refuses unless you pass `--yes`. The commands and their sources are listed in [remote access](remote-access.md#installing-the-helpers).

The update check sends one request to api.github.com a day, carrying only the Hashi version in its User-Agent (`hashi/0.2.0`), and caches the answer in `~/.hashi/update-check.json`. `hashi update` downloads the archive for your platform from the GitHub release, verifies its SHA-256 checksum, checks that the new binary reports the expected version and swaps it in; restart Hashi afterwards. It refuses a binary installed by Cargo (update that with `cargo install --locked --git https://github.com/sidharthpunathil/hashi --force`) or one in a directory you cannot write to, and asks before replacing unless you pass `--yes`.

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

| Key | Action |
|---|---|
| `1` `2` `3` `4` `5`, `Tab`, `h` `l` | Overview, Requests, Tokens, Logs, Commands |
| `↑` `↓` / `j` `k` | Move the selection |
| `PgUp` `PgDn` / `J` `K` | Scroll the request detail |
| `Enter` | Run the selected row (Commands view) |
| `n` / `d` | Create a token (Tokens and Commands views) / revoke one (Tokens view) |
| `s` | Share: create an invite link (Tokens and Commands views) |
| `p` | Pause or resume the selected token |
| `i` / `x` | Switch to pending invites / cancel one |
| `c` | Copy the API URL, or the token or invite link being shown |
| `t` | Go public: start a Cloudflare quick tunnel on the running server. Without `cloudflared`, opens its install popup; with Tailscale public, asks to switch |
| `r` | Refresh |
| `?` | All key bindings |
| `Esc` | Close a popup |
| `q` | Quit and stop the bridge. Asks first while requests are in flight |
| `Ctrl-C` | Quit immediately |

### Commands view

Tab `5` lists everything the owner can do, grouped, one row each with its key (or `enter`), what it does, its state right now and the command that does the same from a terminal:

| Group | Rows |
|---|---|
| Go public | Cloudflare quick tunnel (`t`), Tailscale serve, Tailscale funnel, stop going public |
| Share | new invite (`s`), new token (`n`) |
| Install | cloudflared, tailscale |
| Maintenance | check for updates (shows `hashi update` when one is out), copy the API URL (`c`) |
| Help | all key bindings (`?`) |

States include "live at <url>", "starting", "approve at <url>", "not installed" and "signed out". In an install popup, `enter` runs the command with the terminal attached and brings the dashboard back afterwards, `c` copies it and `esc` cancels. Ctrl-C while it runs goes to the install, not to Hashi.

Running requests count up their elapsed time, and every status pairs a symbol with its colour (`✓` done, `✗` failed, `◌` queued, a spinner for running). Body text uses your terminal's own foreground colour, so light and dark themes both stay readable.

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
