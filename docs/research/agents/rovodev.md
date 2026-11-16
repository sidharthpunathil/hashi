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
