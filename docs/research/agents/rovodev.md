# Atlassian Rovo Dev CLI (`rovodev`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **docs**. The spec Hashi runs is `ROVODEV` in `src/agent/specs.rs`.

- **Source:** closed
- **License:** proprietary
- **Version checked:** acli 1.3.40 (Rovo Dev component version not checked)
- **Install:** brew tap atlassian/homebrew-acli && brew install acli | curl -LO https://acli.atlassian.com/linux/latest/acli_linux_amd64.tar.gz (see developer.atlassian.com/cloud/acli/guides/install-acli/), then acli rovodev auth login
- **Headless call:** `acli rovodev run`, prompt positional
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

Exists: yes. Atlassian Rovo Dev CLI is an extension of the Atlassian CLI (`acli`, Go/cobra). `acli rovodev run` launches a separately downloaded Rovo Dev agent component (Python, judging by its Textual/Pygments config keys); closed source. I downloaded acli 1.3.40 (acli.atlassian.com/linux/latest/acli_linux_amd64.tar.gz) and grepped its strings; the Rovo Dev component itself is fetched at runtime and was not obtained, so flags below come from Atlassian docs and two third-party wrappers. Nothing was executed.

**Headless:** `acli rovodev run [instruction]` "Execute a single instruction (non-interactive mode)" (support.atlassian.com/rovo/docs/rovo-dev-cli-commands/). Community usage guide (community.atlassian.com ... /ba-p/3047073): "In non-interactive mode, Rovo Dev updates files in your codebase directly without interaction."

**Prompt passing:** positional instruction, one argv element (github.com/possibly/llm-rovodev llm_rovodev_cli.py builds `[acli, rovodev, run, *extra_args, prompt]`). That plugin says non-interactive prompts are limited to 256 characters and writes longer prompts to a file in the cwd (llm_rovodev_prompt.py MAX_INLINE_CHARS = 256); the limit is UNVERIFIED for current versions and Hashi would need to handle it. The yanggf8/rovodev-cli-mcp wrapper inserts `--` before prompts that start with `-`, which suggests `--` is honoured, but that is UNVERIFIED, so style is positional (Hashi refuses dash-leading prompts).

**Output:** NO machine-readable mode found. stdout is rich console text: a `Using model: <id>` line, then box-drawn panels, the answer inside a `╭─ Response ─...╮ / │ ... │ / ╰─...╯` box (llm-rovodev tests/test_llm_rovodev.py fixtures, parser llm_rovodev_parser.py parse_all_response_blocks). Policy refusals come as an `AI Policy Filter` box (tests/test_ai_policy_filter.py). Tool result panels also print unless `console.showToolResults: false`. `console.outputFormat` (markdown|simple|raw) and `console.maxOutputWidth` (default 120) in ~/.rovodev/config.yml change rendering. Hashi would need a custom box-extracting parser (strip U+2502 borders, take the last Response box); "text" here means stdout is NOT the bare answer. No session id, tokens or cost on stdout. `acli rovodev serve <port>` exposes an HTTP+SSE JSON API (/v3/set_chat_message, /v3/stream_chat, /v3/sessions/...) which is the only structured interface, but it is a server, not a one-shot command.

**Resume:** `--restore [session UUID]`; without a value restores the most recent session in the current workspace; sessions are per workspace under ~/.rovodev/sessions/ (manage-sessions-in-rovo-dev-cli). The UUID is shown only by `/status` in interactive mode, so Hashi cannot learn it from run output. `--restore` takes an optional value, so `--restore <uuid>` followed by a positional instruction may be ambiguous; UNVERIFIED.

**Model:** no CLI flag documented; `agent.modelId` in config (default `auto`), `/models` interactively.

**Permissions:** config `toolPermissions.default: ask`, file ops ask, bash default ask (manage-rovo-dev-cli-settings, use-tools-in-rovo-dev-cli). What an `ask` does in non-interactive run is undocumented; the community guide implies edits are applied without interaction, so auto_approves_tools_headless is set true as the conservative assumption (UNVERIFIED; bash may still be refused). `--yolo` approves all file, bash and git tools for the session (MCP tools keep their configured permissions). The only way to force read-only is a config file with `toolPermissions.default: deny`, `bash.default: deny`, passed via `--config-file <path>` ("Rovo Dev will create the file if it doesn't exist"); Hashi would have to write that file, so nothing is pinned. `--shadow` (run on a temporary copy of the workspace) is used by third-party wrappers (yanggf8/rovodev-cli-mcp, atlassian-labs/rovo-dev.nvim) and the server-mode docs (`rovodev serve 8123 --shadow`); it would be a good pin but is UNVERIFIED for `run`, so it is not pinned. `--worktree` needs a git repo. `--web` starts a web UI (not headless).

**Auth:** `acli rovodev auth login` (interactive), or non-interactively `acli rovodev auth login --email <email> --token < token.txt` with a scoped API token from https://go.atlassian.com/rovo-dev-api-token (acli strings). The site must have Rovo Dev activated and Rovo Dev credits. acli honours `ACLI_CONFIG_DIR` (strings) for its own config including `rovodev_config.yaml`; Rovo Dev's own state (~/.rovodev/config.yml, sessions, logs, mcp_config.json) is separate, so no single env var relocates everything (config_dir_env null; UNVERIFIED whether ACLI_CONFIG_DIR alone isolates the login). No API-key env var is documented for rovodev.

**Other env:** acli knows `ATLASSIAN_API_URL`, `ATLASSIAN_AUTH_URL`, `ATLASSIAN_ACCESS_TOKEN_URL`, `ROVODEV_BETA` (strings; semantics UNVERIFIED). Startup animations and terminal title updates are config keys (`console.enableStartupAnimations`, `console.terminalTitle.isEnabled`), not env.

**Limits:** no documented message. Billing is "Rovo Dev credits"; `/usage` shows them. limit_patterns are guesses (UNVERIFIED).

**Fixtures:** derived from llm-rovodev's mock fixtures, not recorded. Failure is reported in stdout as a box (policy filter) or on stderr with non-zero exit (llm_rovodev.py:205 falls back to stderr).

## Success output (derived from source or docs, not recorded)

```
Using model: gpt-5-2025-08-07

╭─ Response ───────────────────────────────────────────────────────────╮
│ 42                                                                  │
│                                                                      │
│ Done.                                                               │
╰──────────────────────────────────────────────────────────────────────╯
```

## Error output

```
Using model: gpt-5-2025-08-07

╭─ AI Policy Filter ────────────────────────────────╮
│                                                   │
│ Your prompt was blocked by the Atlassian AI       │
│ policy filter.                                    │
│                                                   │
│ Harm category: Jailbreak/Prompt Injection         │
│                                                   │
╰───────────────────────────────────────────────────╯
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** no. Closed Go binary; evidence is from strings of acli 1.3.40 linux_amd64 (scratchpad/acli). ACLI_CONFIG_DIR is read by acli (string `ACLI_CONFIG_DIR`) and holds rovodev_config.yaml (config.NewRovodevFileConfig / RovodevProfile with yaml tags email, token,omitempty, cloud_id, site, current_profile, profiles). But the API token is saved through store.KeyringStore on github.com/zalando/go-keyring (macOS Keychain, Linux Secret Service, `secret not found in keyring`), which ACLI_CONFIG_DIR does not move, and no env var that forces file storage was found. Whether keyring entries are keyed per product+email (store.productUser suggests so, which would let two different emails coexist) is UNVERIFIED. Rovo Dev's own state (~/.rovodev/config.yml, sessions, logs, mcp_config.json; web/rovo_manage-rovo-dev-cli-settings.html) is also not moved. Two accounts with distinct emails may work in practice but cannot be confirmed without running it.
- **API-key accounts:** no. No documented API-key env var for rovodev. acli builds child env lines `USER_EMAIL=%s` and `USER_API_TOKEN=%s` (strings) to hand the stored login to the downloaded Rovo Dev component, so those names are what the component reads, but acli sets them itself from its stored profile and nothing shows that an exported value outranks the stored one. UNVERIFIED; not usable as a key account.
- **Status check:** {"args": ["rovodev", "auth", "status"], "signed_in_when": "exit 0 (UNVERIFIED; command exists as atlassian.com/cli/pkg/cmd/rovodev/auth/status and help text `acli rovodev auth status`, output format unknown, no --json seen)"}
- **Identity:** {"file": "rovodev_config.yaml", "pointer": null, "jwt_claim": null}
- **Switch:** no. The secret half of the login is in the OS keyring (zalando/go-keyring), not in plain files, and the default acli config dir and exact rovodev_config.yaml layout are not documented (the file is YAML, not JSON, so Hashi's JSON pointers cannot read it). File-copy switching cannot work.
- **Live quota:** no. Rovo Dev credit usage is only shown by the interactive `/usage` command (and `/usage site` to change the credit site) in the TUI (web/https___support_atlassian_com_rovo_docs_rovo_dev_cli_commands__ lines 127-130); no non-interactive command or documented endpoint was found.
- **Usage in output:** none
- **Limit messages:** No documented limit message; 'rate limit' and 'too many requests' are guesses (acli carries Go's `Too Many Requests` status text). Billing is Rovo Dev credits per site, so an exhausted allocation likely has its own wording (UNVERIFIED); consider adding 'credits', 'out of credits' or 'credit limit' once a real message is recorded. Output is box-drawn text, so matching must run on stdout and stderr.

Source: closed. Evidence from `strings` of acli 1.3.40 linux_amd64 (downloaded in an earlier pass to scratchpad/acli) and saved Atlassian support pages in scratchpad/web; the Rovo Dev component itself is downloaded at runtime and was not obtained. Nothing was run. Commands present: pkg/cmd/rovodev/auth/{login,logout,status}, api/rovodev.(*repository).RovodevAuth and RovodevAuthStatus, cmd/rovodev.executeBinary/tryFallback. Config: config.RovodevFileConfig/RovodevProfile saved to rovodev_config.yaml under ACLI_CONFIG_DIR, yaml tags include email, token,omitempty, cloud_id, site, account_id, current_profile, profiles. Secrets: config/store.KeyringStore (Get/Set/Delete) over github.com/zalando/go-keyring (secret_service on Linux, Keychain on macOS); `token,omitempty` in the YAML suggests a file fallback when no keyring exists, conditions UNVERIFIED. Identity: the email is in rovodev_config.yaml (yaml key `email` under the current profile), but the exact path is unknown and the file is YAML, so pointer is null. Quota: TUI `/usage` only. Usage: stdout is rich text with no tokens or cost (docs/research/agents/rovodev.md).
