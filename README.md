<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.png">
    <img src="assets/logo-light.png" alt="Hashi the Bridge" width="480">
  </picture>
</p>

# Hashi

**Call Claude Code, Codex and other coding agents on your machine over HTTP, and share them with people and apps without sharing your account.**

[![Release](https://img.shields.io/github/v/release/sidharthpunathil/hashi?color=7c4dff)](https://github.com/sidharthpunathil/hashi/releases/latest)
[![CI](https://github.com/sidharthpunathil/hashi/actions/workflows/ci.yml/badge.svg)](https://github.com/sidharthpunathil/hashi/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-a855f7)](#license)

## What is Hashi

Hashi (橋, Japanese for bridge) is a single binary that runs next to the agent CLIs you already installed and signed in to. It puts them behind one authenticated API, so a script, an app, a bot or a friend can send a prompt and get the answer back as JSON:

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

## Why Hashi

To share a coding agent today you either hand out a provider API key or let people SSH in and run `claude -p` themselves. Hashi sits between the two:

- **It runs the agents, not just the models.** Each prompt runs the real CLI in its own workspace, with the agent's tools and its ongoing conversation, under the plan you are already signed in to. No API key per app.
- **Everyone gets their own token.** A token can be limited to some agents, capped per day or month, paused, expired and revoked. Nobody gets your login or a shell.
- **It uses all your accounts.** Add a work plan, a personal plan or an API key per agent. When one runs out, the prompt moves to the next, and Hashi shows how much of each Claude Code and Codex plan is left.
- **Your machine stays usable.** Prompts wait in a SQLite queue that survives restarts, and a machine-wide run cap, a memory reserve and a per-run memory limit keep a busy friend from swapping your laptop to death.
- **It is one file.** A Rust binary with SQLite built in, for Linux, macOS and Windows. No runtime, database server or container.

**When not to use it.** Each answer is a full agent session, so it takes seconds to minutes and depends on one machine staying on. For high-volume or low-latency production traffic, call the provider's API. Agents run as your OS user, so share them only with people you would trust with your files ([what that means](docs/security-model.md#what-a-token-can-and-cannot-do)). Using a subscription this way is subject to each provider's terms; check yours before you share.

## Status

Early and actively developed: v0.3.1, released November 2026. CI builds and tests every commit on Linux, macOS and Windows; Windows has seen the least real use. The HTTP API may still change before 1.0, and every change is in the [changelog](CHANGELOG.md).

## Install

**macOS and Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.sh | sh
```

**Windows** (PowerShell)

```powershell
irm https://raw.githubusercontent.com/sidharthpunathil/hashi/main/install.ps1 | iex
```

The script downloads the build for your platform, checks its SHA-256 checksum, installs it without admin rights (to `~/.local/bin`, or `%LOCALAPPDATA%\Programs\hashi\bin` on Windows, added to your PATH) and runs `hashi doctor` to show which agents it found. Pin a release with `HASHI_VERSION=v0.3.1` ([installer options](docs/configuration.md#installer-options)).

The checksum proves the download arrived intact, not who built it. Every release archive also has a GitHub build attestation tying it to this repository's release workflow. Check one with the [GitHub CLI](https://cli.github.com):

```bash
gh attestation verify hashi-aarch64-apple-darwin.tar.gz --repo sidharthpunathil/hashi
```

You can also download an archive from the [releases page](https://github.com/sidharthpunathil/hashi/releases) (Linux x64 and ARM64, macOS Intel and Apple Silicon, Windows x64) or build with `cargo install --locked --git https://github.com/sidharthpunathil/hashi` (Rust 1.89 or later). `hashi update` installs a new release and `hashi uninstall` removes Hashi and its data.

## Quick start

1. **Have at least one agent installed and signed in**, for example `claude --version` or `codex --version`.

2. **Start the bridge.**

   ```bash
   hashi
   ```

   On the first start Hashi creates an admin token and shows it once. Copy it with `c` in the dashboard, or run `hashi serve --headless` to get it in the terminal.

3. **Make a token for your app and send a prompt.**

   ```bash
   export HASHI_ADMIN=hashi_...   # from step 2

   curl http://127.0.0.1:7878/v1/tokens \
     -H "Authorization: Bearer $HASHI_ADMIN" \
     -d '{"name": "my-app", "sessionMode": "stateful"}'

   curl http://127.0.0.1:7878/v1/ask \
     -H "Authorization: Bearer hashi_<the token you just got>" \
     -d '{"message": "Write a haiku about bridges", "wait": 60}'
   ```

   The first call answers with the new token in its `token` field, shown only this once. A `stateful` token picks up its conversation where it left off. `hashi token create my-app --stateful` does the same from a terminal.

The dashboard that `hashi` opens does nearly everything the CLI does: tokens, invites, accounts, projects, going public, moving the server to another port, the audit log and updates. Tab `5` lists every action with the command that does the same, and `?` shows every key ([dashboard keys](docs/configuration.md#dashboard-keys)).

<p align="center">
  <img src="assets/dashboard.png" alt="Hashi terminal dashboard: agent cards with their state, request counters and a live activity feed" width="900">
</p>

## Sharing with people

```bash
hashi share alice --daily-requests 50 --daily-budget 5 --expires-in-days 30
```

This prints a one-time link, `https://your-host/claim/hashi_inv_…`, that works once and expires after 24 hours (for people on other machines, set up [remote access](#remote-access) first). Alice's page shows who is sharing, which agents she gets, her limits and a notice that you can see her prompts and answers. One click gives her a personal token with curl, Python, TypeScript and MCP snippets, plus Add to Cursor and Add to VS Code buttons. Or she copies one setup command that installs Hashi and runs `hashi connect`, after which `hashi ask --remote "..."` works from her terminal.

Pause, edit or revoke her token from the dashboard, `hashi token update` or `hashi token revoke`. Revoking stops her running prompts at once, and every change is in `hashi audit`.

Instead of making every invite yourself, you can let people ask:

```bash
hashi requests open --note "For the docs team" --hours 48   # prints https://your-host/ask-access/hashi_ask_…
hashi requests list
hashi requests accept 1a2b3c4d --daily-requests 50          # or: hashi requests decline 1a2b3c4d
```

Accepting creates an invite only the requester can collect, so there is no link for you to pass on. People who already have a token can ask for more from their `/me` page. Nothing is granted without you. See [invites and limits](docs/configuration.md#invites-and-limits) and [access requests](docs/configuration.md#access-requests).

## Projects and accounts

```bash
hashi account add claude work                 # a second Claude login, signed in in its own directory
hashi account add codex ci --key-env CI_KEY   # or an API key
hashi project add api ~/code/api --account claude=work,default --model claude=sonnet
hashi share alice --project api
```

Alice's prompts now run in `~/code/api` instead of a private workspace, on your `work` Claude plan first and your own login once `work` hits its limit. She sees only the project's name, never its folder or your account names. Your normal login is never touched, and Hashi never changes, moves or deletes a project folder. A project changes where a run starts, not what an agent running as you can read, so share only folders you trust people with. See [accounts](docs/configuration.md#accounts) and [projects](docs/configuration.md#projects).

## Knowing what's left

While the server runs, Hashi reads how much of each Claude Code and Codex plan is left, every five minutes and after each run, without spending a token or refreshing a login:

```console
$ hashi account usage
claude:work  max  28% left  (poll, 2m ago)
  5 hours        ██████░░░░░░░░░░░░░░  31%  resets in 2h 9m (2026-11-06T23:37:22Z)
  week           ██████████████░░░░░░  72%  resets in 3d 11h (2026-11-09T09:27:22Z)
codex:default  plus  9% left (warning)  (poll, 1m ago)
  5 hours        ██████████████████░░  91%  resets in 49m (2026-11-06T22:17:22Z)
  week           ████████░░░░░░░░░░░░  40%  resets in 4d 15h (2026-11-10T12:34:02Z)
```

Which account a run uses follows a strategy, set per project (`--account-strategy` on `hashi project add`) or for the server (`hashi serve --account-strategy`):

- `in-order` (the default): the first account in the list that is not at its limit.
- `most-left`: the account with the most quota left.
- `soonest-reset`: the account whose weekly quota resets soonest, so quota that would expire unused is spent first.

With any strategy, runs move off an account once it is 90% used (`--rotate-at`, 50 to 99). See [usage and strategies](docs/configuration.md#usage-and-strategies).

## Pooling friends' access

When a friend shares their Hashi with you, connect to it once and add it as one of your own accounts:

```bash
hashi connect https://bob.example/claim/hashi_inv_...   # saved as the remote "bob"
hashi account add claude bob-claude --remote bob
hashi share alice --account claude=work,bob-claude
```

A run on `bob-claude` goes to Bob's bridge and comes back with its answer, usage and cost; if his bridge is down or his limits are reached, the prompt moves to your next account. Bob sees every prompt sent through it, and the people you share with never see that the account is remote. If Bob shares with `--share-usage`, his remaining quota counts for `most-left` and `--rotate-at` too. See [remote accounts](docs/configuration.md#remote-accounts).

## Using it from OpenAI-compatible apps

Hashi also speaks the OpenAI chat API at `/v1/chat/completions` and `/v1/models`, so the `openai` packages, Open WebUI and other tools built for it work with a Hashi token. Point them at `http://127.0.0.1:7878/v1`, use your `hashi_...` token as the API key, and pick an agent as the model: `claude`, `codex`, or `claude/opus` to choose the agent's own model. `stream: true` works, but the answer arrives as one chunk because agents do not stream token by token. See the [API reference](docs/api.md#openai-compatible-api).

## Supported agents

Hashi drives whichever of these you have installed and signed in to. Each runs headless with its safest unattended setting, so a prompt cannot make it approve its own tool use.

| Agent | `agent` value | Headless defaults Hashi uses | Reports | Verified with |
|---|---|---|---|---|
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | `claude` | print mode, 10-turn limit, user settings only, no MCP servers, `WebFetch` denied unless you set a permission mode | tokens and cost | 2.1.318 (2026-11-06) |
| [Codex](https://github.com/openai/codex) | `codex` | `exec` with a read-only sandbox (your global MCP servers still load) | tokens | 0.166.0 (2026-11-06) |
| [Gemini CLI](https://github.com/google-gemini/gemini-cli) | `gemini` | `--approval-mode default`, so tools that need approval are denied; no MCP servers | tokens | 0.67.0 (2026-11-06) |
| [opencode](https://opencode.ai) 2.2+ | `opencode` | every tool denied except read, glob, grep, list and lsp; `.env` files unreadable; project config ignored | tokens and cost | 2.2.5 (2026-11-06) |
| [Qwen Code](https://github.com/QwenLM/qwen-code) | `qwen` | `--approval-mode default`, so shell and edits are denied; no MCP servers | tokens | 0.26.8 (2026-11-06) |
| [Cursor CLI](https://docs.cursor.com/en/cli/overview) | `cursor` | print mode with the sandbox on, never `--force`, so changes are proposed and not applied | text only | 2026.11.05-86047df (2026-11-06) |
| [Crush](https://github.com/charmbracelet/crush) 0.100 (opt-in: `--enable-crush`) | `crush` | every write, shell, network and sub-agent tool removed; project config refused; your own Crush config never loads | tokens and cost | 0.100.0 (2026-11-06) |

"Verified with" is the upstream release whose headless flags, environment variables, account isolation and limit messages were last checked, recorded in [`verified-versions.json`](docs/research/verified-versions.json). A newer release usually works; `hashi doctor` says when yours is newer, and a weekly check opens an issue for each new release.

Crush is off unless you pass `--enable-crush`, because a headless `crush run` approves every tool call itself: Hashi can only take tools away, and the read tools left can still read any file your user can ([details](docs/security-model.md#crush)). To loosen Claude Code or Codex, use `--claude-permission-mode` or `--codex-sandbox`; the other defaults are fixed in this release.

## Remote access

Hashi listens on `127.0.0.1` by default. Each of these gives you HTTPS from elsewhere:

| Option | Command |
|---|---|
| Cloudflare quick tunnel (random URL, new on each start) | press `t` in the dashboard, or `hashi serve --tunnel` |
| Cloudflare named tunnel (your hostname) | `HASHI_TUNNEL_TOKEN=... hashi serve --public-url https://hashi.example.com` |
| Tailscale Serve (your tailnet) or Funnel (the internet) | `hashi serve --tailscale serve` or `--tailscale funnel` |
| Hashi serves TLS itself (LAN, your own certificate) | `hashi serve --tls-cert cert.pem --tls-key key.pem` |
| Behind Caddy or nginx | `hashi serve --public-url https://... --trust-proxy` |

Missing `cloudflared` or `tailscale`? `hashi install cloudflared` or `hashi install tailscale` shows the vendor's own install command and runs it after you confirm. Setup for each option is in [docs/remote-access.md](docs/remote-access.md).

## Documentation and contributing

- [API reference](docs/api.md): every endpoint, request field, MCP tool and error, with Python and TypeScript examples
- [Configuration](docs/configuration.md): every flag and environment variable, CLI commands, dashboard keys
- [Security model](docs/security-model.md): what a token can reach, resource protection, limits, known gaps
- [Remote access and HTTPS](docs/remote-access.md)
- [Performance](docs/performance.md): measurements and how to reproduce them
- [Changelog](CHANGELOG.md) and [security policy](SECURITY.md)

Bug reports and pull requests are welcome: [CONTRIBUTING.md](CONTRIBUTING.md) covers the local setup and the checks CI runs. Found a bug? `hashi report` writes a redacted diagnostic file and prints a prefilled issue link; nothing is uploaded ([details](docs/configuration.md#reporting-a-bug)).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.

Hashi is an independent project. It is not affiliated with Anthropic, OpenAI, Google, Alibaba, Anysphere, Charm or the opencode project. All product names are trademarks of their respective owners.
