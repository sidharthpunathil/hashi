# Devin CLI (`devin`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **docs**. The spec Hashi runs is `DEVIN` in `src/agent/specs.rs`.

- **Source:** closed
- **License:** proprietary
- **Version checked:** 3000.12.57
- **Install:** curl -fsSL https://cli.devin.ai/install.sh | bash | brew install --cask devin-cli | irm https://static.devin.ai/cli/setup.ps1 | iex (Windows PowerShell)
- **Headless call:** `devin`, prompt flag eq `--print=`
- **Runs tools without asking when headless:** no

## Notes

Exists: yes. Cognition's Devin CLI (formerly Windsurf/Codeium terminal agent, internal crate name `chisel`). Closed source; no public repo, no npm package. I downloaded the official Linux bundle (static.devin.ai/cli/3000.12.57/devin-3000.12.57-x86_64-unknown-linux.tar.gz, sha256 0946784e...07d4a, matches manifest.json) and read its man pages (share/man/man1/devin.1) and `strings` of bin/devin. Nothing was executed.

**Headless:** `-p/--print [PROMPT]` "Runs in non-interactive mode: processes the prompt and exits" (devin.1; docs.devin.ai/cli/reference/commands.md). Since v3000.x `-p` takes an optional inline value (changelog: "`devin -p \"fix the bug\"` works without needing the `--` separator. The old `devin -p -- fix the bug` syntax continues to work"). Empty prompt fails with "Print mode runs a single turn non-interactively, so it needs a prompt." (binary string).

**Prompt passing:** clap parser. The top-level usage has TWO positionals, `[PATH]` (opens Devin Desktop) and `[PROMPT]` (devin.1), and `-p` takes an optional value, so `-p -- <prompt>` relies on custom disambiguation. `--print=<prompt>` (flag_eq) is unambiguous in clap for an optional-value flag, even when the prompt starts with `-`. UNVERIFIED by execution.

**Output:** plain text on stdout only. No `--output-format`/JSON/stream mode exists in v3000.12.57 (no such flag in devin.1 or the docs; `--json`/`--format json` exist only on `doctor`, `list`, `models list`, `mcp list`). So: no session id, no token usage, no cost on stdout. `--verbosity 0` ("quiet") is pinned to keep tool cards out of the answer; whether print mode honours it is UNVERIFIED. `--export [PATH]` writes an ATIF trajectory JSON after each turn (would carry session id and metrics), but that is a side file, not stdout. Usage is only visible via `/usage` and `/session-stats` in the REPL.

**Resume:** `-r/--resume <SESSION_ID>`, `-c/--continue` (most recent session in the cwd, not already open). Session ids are never printed in print mode (UNVERIFIED), so Hashi cannot learn the id from stdout; `-c` in the per-token workspace is the practical resume. Sessions live in a SQLite db; `DEVIN_SESSIONS_DIR` relocates it, `DEVIN_SESSION_STORE=files` switches to files (changelog).

**Permissions:** `--permission-mode auto|normal` (default) auto-approves read-only tools; exec, fetch and edits need approval (docs.devin.ai/cli/reference/permissions.md table). In print mode a tool needing confirmation is rejected, not hung: binary strings "Print mode: rejecting tool call" and "_warning: rejected a tool call that requires confirmation. Running in non-interactive mode. Use ...". So pinning `--permission-mode auto` gives a read-only headless run. `accept-edits`, `smart`, `dangerous` (aliases `yolo`, `bypass`) and `autonomous` (needs `--sandbox`) widen it; `DEVIN_PERMISSION_MODE` env and the user config `permission_mode` key also set it, so the pinned flag must win (CLI flag beats config per devin.1). Org-level deny/ask rules still apply.

**Workspace trust (blocks Hashi unless handled):** "Non-interactive (print) mode cannot show the trust prompt and fails in an untrusted directory; pass --respect-workspace-trust false" (devin.1). A fresh per-token workspace is untrusted, so the flag is pinned. Hazard: with trust skipped, project files in the workspace (`.devin/config.json` permissions and hooks, `.devin/mcp_config.json`, plus imported `.claude/`, `.cursor/`, `.windsurf/` rules and MCP servers via `read_config_from`) are loaded; a workspace hook runs shell commands. Acceptable only because auto mode cannot write those files itself; UNVERIFIED whether project MCP servers start in print mode.

**Model:** `--model <MODEL>` (fuzzy names like `opus`, `sonnet`, `codex`, `swe-1-6`), env `DEVIN_MODEL`.

**Auth:** `devin auth login` (browser, or `--force-manual-token-flow`) stores a non-expiring token in `$XDG_DATA_HOME/devin/credentials.toml` or `~/.local/share/devin/credentials.toml` (docs.devin.ai/cli/enterprise/devin-auth.md). `WINDSURF_API_KEY` is documented for `devin acp` and appears in the binary; that it also works for `-p` is UNVERIFIED. `WINDSURF_API_SERVER_URL`, `DEVIN_API_URL` select the backend. Config is `~/.config/devin/config.json` (`--config <PATH>` overrides the user config file only). No single env var relocates config, credentials and sessions together; `HOME` (or `XDG_CONFIG_HOME` + `XDG_DATA_HOME`, the former UNVERIFIED) would.

**Other:** background auto-update on macOS/Linux, opt out only via `"auto_update": false` in config.json (no env var found). At session start Devin runs `$SHELL` as an interactive login shell to snapshot env (troubleshooting.md), so the user's shell profile runs. Install: `curl -fsSL https://cli.devin.ai/install.sh | bash`, `brew install --cask devin-cli`, Windows `irm https://static.devin.ai/cli/setup.ps1 | iex`. Requires a Devin/Windsurf account with the "Use Devin CLI" permission; billed in credits/ACUs.

**Limits:** binary strings "Quota exhausted", "Purchase on-demand usage or turn on auto-reload, or wait for your quota to reset.", "Rate limited: ", ACP error kind `resource_exhausted`; changelog "Usage limit reached". Whether these go to stdout or stderr in print mode, and the exit code, are UNVERIFIED.

**Fixtures:** derived, not recorded. Success is the plain answer text. Failure output is UNVERIFIED (assumed stderr plus non-zero exit), so fixture_error is null. Slash commands: skills are invocable as `/name`; whether a leading `/` in a `-p` prompt runs one is UNVERIFIED (assumed yes).

## Success output (derived from source or docs, not recorded)

```
`add` returns the sum of its two arguments.
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. The login is the plain file credentials.toml at $XDG_DATA_HOME/devin/credentials.toml when XDG_DATA_HOME is set, else ~/.local/share/devin/credentials.toml, on macOS and Linux (docs.devin.ai/cli/enterprise/devin-auth.md 'Credentials file location'; Windows uses %APPDATA%\devin). The binary has no keychain code for it (bin/devin strings: serde struct CredentialsFile with api_key/api_server_url/devin_webapp_host/devin_api_url/org_id, read via the toml crate). XDG_DATA_HOME also moves Devin's session database and summaries ($XDG_DATA_HOME/devin/summaries), which is harmless for an account folder, but it is inherited by every tool and shell command the agent runs. User config (~/.config/devin/config.json) is not moved, so org selection saved there may be shared (UNVERIFIED).
- **API-key accounts:** yes. Binary string: '`WINDSURF_API_KEY` controls this session's identity, so `/login` is not available. Unset it and restart to log in with an account.' So the env key outranks the stored credentials.toml. Docs mention it for ACP clients (docs 'Devin uses the credentials from `devin auth login` (or `WINDSURF_API_KEY` if set)'); use with -p is UNVERIFIED by running. WINDSURF_API_SERVER_URL / DEVIN_API_URL pick the backend for the key.
- **Status check:** {"args": ["auth", "status"], "signed_in_when": "exit 0"}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** yes. One plain TOML file in a folder that XDG_DATA_HOME moves (account folder <dir>/devin). The token 'is persistent and does not expire' and the docs explicitly allow copying the file between machines (devin-auth.md), so there is no refresh race and no expiry pointer is needed. Caveats: an owner who already sets XDG_DATA_HOME has a different live dir; Windows uses %APPDATA%\devin; a running devin keeps the identity it started with; an owner WINDSURF_API_KEY in the shell overrides any switched file.
- **Live quota:** no. Usage is shown only by the REPL commands /usage and /session-stats; no non-interactive usage or quota subcommand exists in v3000.12.57 (devin.1 and devin-*.1 man pages; `--json` exists only on doctor, list, models list, mcp list). Billing is in credits/ACUs on the Devin dashboard.
- **Usage in output:** none
- **Limit messages:** Spec strings match binary strings 'Quota exhausted', 'Purchase on-demand usage or turn on auto-reload, or wait for your quota to reset.', 'Rate limited: ', ACP kind 'resource_exhausted' and changelog 'Usage limit reached'. Whether they reach stdout in print mode is UNVERIFIED; since output is Text, add them to text_errors too if they print with exit 0.

Closed source; read from the official Linux bundle 3000.12.57 (bin/devin strings, share/man/man1/devin-auth-*.1) and docs.devin.ai. `devin auth status` takes no --json (devin-auth-status.1); per changelog it prints login method, user name and email, user ID, team ID, plan and tier, and 'Not logged in.' when signed out; its exit code is UNVERIFIED, so 'exit 0' may need to become a text check for 'Not logged in'. credentials.toml holds no email, so identity from a file is not possible; the email is only in `auth status` text.
