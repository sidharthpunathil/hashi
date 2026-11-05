# Configuration

Pass flags to `hashi serve` (for example `hashi serve --port 9000 --tunnel`). Running bare `hashi` starts the server with defaults and environment variables. Every flag also reads an environment variable.

| Flag | Env | Default |
|---|---|---|
| `--home` | `HASHI_HOME` | `~/.hashi` |
| `--bind` | `HASHI_BIND` | `127.0.0.1` |
| `--port` | `HASHI_PORT` | `7878` |
| `--tunnel` | `HASHI_TUNNEL` | off |
| `--tunnel-token` | `HASHI_TUNNEL_TOKEN` | none. Runs a named tunnel; needs `--public-url`. Prefer the variable: the flag shows in the process list, and Hashi warns |
| `--trust-proxy` | `HASHI_TRUST_PROXY` | off. Read client addresses from `X-Forwarded-For` on loopback connections |
| `--cors-origin` | `HASHI_CORS_ORIGINS` (comma separated) | none. Origins allowed to call Hashi from a browser |
| `--allowed-host` | `HASHI_ALLOWED_HOSTS` (comma separated) | none. Extra host names accepted over plain HTTP |
| `--public-url` | `HASHI_PUBLIC_URL` | none. Base for invite links, for example `https://hashi.example.com` |
| `--tls-cert`, `--tls-key` | `HASHI_TLS_CERT`, `HASHI_TLS_KEY` | none. Serve HTTPS with these PEM files |
| `--name` | `HASHI_NAME` | `Hashi`. Shown on invite and My access pages |
| `--headless` | | off (on when stdout is not a terminal) |
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
hashi doctor                               # are the agents and cloudflared installed?
hashi token create ci --agents codex --expires-in-days 30
hashi token list                           # status and today's use per token
hashi token update hashi_1a2b3c4d --pause  # or --resume, --daily-requests 100, --clear-limits
hashi token revoke hashi_1a2b3c4d
hashi share alice --agents claude          # one-time invite link
hashi invite list                          # pending, claimed and expired invites
hashi audit                                # who changed what
hashi ask "What does this repo do?" -a claude   # one prompt, no server
hashi report                               # redacted diagnostics for a bug report
```

## Dashboard keys

Run `hashi` in a terminal and the dashboard takes over the screen. When stdout is not a terminal, or with `--headless`, Hashi prints plain log lines instead.

| Key | Action |
|---|---|
| `1` `2` `3` `4`, `Tab`, `h` `l` | Overview, Requests, Tokens, Logs |
| `↑` `↓` / `j` `k` | Move the selection |
| `PgUp` `PgDn` / `J` `K` | Scroll the request detail |
| `n` / `d` | Create or revoke a token (Tokens view) |
| `s` | Share: create an invite link (Tokens view) |
| `p` | Pause or resume the selected token |
| `i` / `x` | Switch to pending invites / cancel one |
| `c` | Copy the API URL, or the token being shown |
| `r` | Refresh |
| `?` | All key bindings |
| `Esc` | Close a popup |
| `q` | Quit and stop the bridge. Asks first while requests are in flight |
| `Ctrl-C` | Quit immediately |

Running requests count up their elapsed time, and every status pairs a symbol with its colour (`✓` done, `✗` failed, `◌` queued, a spinner for running). Body text uses your terminal's own foreground colour, so light and dark themes both stay readable.

## Installer options

| Variable | Effect |
|---|---|
| `HASHI_VERSION=v0.2.0` | Install a specific release instead of the latest |
| `HASHI_INSTALL_DIR=/some/dir` | Install somewhere else |
| `HASHI_NO_DOCTOR=1` | Skip the `hashi doctor` check at the end |

```bash
curl -fsSL https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.sh | HASHI_VERSION=v0.2.0 sh
```

```powershell
$env:HASHI_VERSION = "v0.2.0"; irm https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.ps1 | iex
```

## Reporting a bug

```bash
hashi report                     # writes hashi-report-<time>.md in this directory
hashi report --stdout            # print it instead
hashi report --include-prompts   # add the first 500 characters of failed prompts
```

The report holds the version, OS, agent and cloudflared detection, `HASHI_*` settings, database counts, the last 20 failed requests, the last 200 log lines and any crash file. Tokens, invite codes, provider keys and your home directory are redacted, and prompts are left out unless you ask for them. Nothing is uploaded: the command prints a link to a prefilled GitHub issue, and you attach the file yourself after reading it.

If Hashi crashes, it saves `~/.hashi/logs/crash-<time>.txt` and the next report includes it. API errors with status 500 carry an `errorId` that matches a line in the log; quote it in the issue.
