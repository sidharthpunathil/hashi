# Hermes Agent (`hermes`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `HERMES` in `src/agent/specs.rs`.

- **Source:** github:NousResearch/hermes-agent
- **License:** MIT
- **Version checked:** main branch source (2026-11-16); installed copy broken (venv missing)
- **Install:** curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash
- **Headless call:** `hermes chat --format stream-json`, prompt flag eq `--query=`
- **Runs tools without asking when headless:** no

## Notes

**Which project.** `NousResearch/hermes-agent` (MIT, Python, entry point `hermes = "hermes_cli.main:main"`, pyproject.toml:593-596). Source read at `~/.opensrc/repos/github.com/NousResearch/hermes-agent/main` (pyproject version is a `0.0.0` placeholder; main branch as fetched 2026-11-16). Paths below are relative to that checkout.

**Installed copy is broken, so nothing was run.** `~/.local/bin/hermes` is a bash shim that execs `~/.hermes/hermes-agent/venv/bin/hermes`, but `~/.hermes` only holds `config.yaml` and `plugins/`; the checkout and venv are gone, so `hermes --version` fails with "No such file or directory". `fixture_success` and `fixture_error` are DERIVED from `hermes_cli/stream_json.py`.

**Two headless entry points.**
1. `hermes -z/--oneshot <PROMPT>` (hermes_cli/_parser.py:150-155): prints only the final text, and its own help says **"approvals are auto-bypassed"**. No JSON, no session id. Not recommended.
2. `hermes chat -q/--query <PROMPT> --format stream-json` (hermes_cli/_parser.py:239-268): JSONL protocol, implies `--quiet`, requires `-q` (else exit 2) and refuses `--tui` (hermes_cli/stream_json.py:18-30). Off a TTY `-q` answers and exits (`oneshot` is implied when stdin/stdout are not TTYs, hermes_cli/gateway_chat.py:166-170). **Recommended.**

**Prompt passing.** argparse; there is no positional prompt, so use `--query=<prompt>` (everything after the first `=` is the value even if it starts with `-`). `--query-file <path>` (`-` = stdin) is the documented "safe for arbitrary text" alternative (_parser.py:243-247). `--` handling is irrelevant.

**Gateway daemon (important).** `chat --format stream-json` always goes through `launch_gateway_chat` (hermes_cli/main.py:1521-1528), which calls `connect_gateway()`; that **auto-starts a background gateway daemon for the active `HERMES_HOME`** if none is running, with `idle_exit=True` so it exits once idle (hermes_cli/gateway_client.py:356-370). The agent turn runs inside that daemon, not in the spawned process, using the caller's cwd as the session folder (`_caller_cwd`, gateway_chat.py:112-114). Consequences for Hashi: (a) `kill_on_drop` kills the viewer, not the turn; the daemon may finish or keep the turn (UNVERIFIED how a killed viewer is handled; the code talks of "detached without cancelling", gateway_chat_view.py:465-485); (b) all runs of one `HERMES_HOME` share one daemon; (c) `HERMES_TUI_GATEWAY_URL` points at a remote gateway instead (gateway_client.py:345-355) and must be stripped.

**Output (stream-json)** (hermes_cli/stream_json.py:37-111; every line also gets `timestamp` in ms):
- `{"type":"system","subtype":"init","model","session_id"}` first; in the gateway path it is emitted before the session exists, so `session_id` is `""` here (gateway_chat.py:339-345).
- `{"type":"text","text"}` deltas (whitespace kept; concatenating them reproduces the answer).
- `{"type":"tool_use","name","tool_call_id?","input?"}` and `{"type":"tool_result","name","tool_call_id?","output","duration_ms","is_error"}` (output capped at 5000 chars).
- terminal `{"type":"result","session_id","exit_code","text","tokens":{"input","output","total","cache_read","cache_write"},"duration_ms","error?","replay_gap?"}` (stream_json.py:87-104). `text` is the whole final answer, so Hashi can read text from `result` only (`text_kind` = `result`). **No cost field** (the separate `--usage-file` report has an estimated cost but only for `-z`, _parser.py:156-160).
- Failures (credentials, agent init, interrupt, needs-human) still emit `init` + a `result` with `error` and a non-zero `exit_code` (cli_single_query.py:515-538, gateway_chat.py:345-352, gateway_chat_view.py:465-472). `session_id: <id>` is also printed on **stderr**.
- Tokens are `/tokens/input` and `/tokens/output`; Hashi's Claude convention would add `cache_read`/`cache_write` to input (UNVERIFIED whether `input` already includes cached tokens).

**Exit codes.** 0 ok; 1 generic/credentials; 2 bad args; 3 one-shot needs a human (gateway_chat_view.py:465-472); 75 when the turn failed on a provider rate limit or billing wall (`KANBAN_RATE_LIMIT_EXIT_CODE`, hermes_cli/kanban_db.py:310, mapped by `_single_query_exit_code`, cli_single_query.py:148-180, reasons `rate_limit`, `upstream_rate_limit`, `billing`, `overloaded`, `server_error`, `timeout` in hermes_cli/turn_exit.py:7-9); 78 on a terminal provider error (auth, model_not_found, kanban_db.py:315); 130 interrupted. Exit 75 is a clean "account at limit" signal Hashi can use in addition to `limit_patterns`.

**Resume.** `chat --resume <SESSION_ID>` resumes by exact id, then by title (gateway_chat.py:178-196); `-c [name]` resumes the latest in the workspace. A resume reuses the session's frozen route and policy: repeating the same creation flags (toolsets, model) is fine, a different value is refused (gateway_chat.py:102-104). Ids cannot be pre-assigned (`--create-if-missing` only works with `-c <name>`, titles not ids).

**Model.** `-m/--model` (e.g. `anthropic/claude-sonnet-4.6`) plus `--provider`; env `HERMES_INFERENCE_MODEL` (_parser.py:162-170).

**Permissions headless.** Hermes has no general allow/ask policy: its approval gate only fires for commands matched as **dangerous** by pattern (tools/approval.py:811-866). Ordinary terminal commands and file writes (`write_file`, `patch`) run without asking. In a single-query run `HERMES_SINGLE_QUERY_SESSION=1` is set (cli_single_query.py:488-496) and flagged actions follow `approvals.single_query_mode`, default **deny** (tools/approval_context.py:209-225, 370-386; tools/approval.py:1255-1290). So by default a `-q` run auto-runs non-dangerous shell commands. `--yolo` bypasses even the dangerous-command gate; `--accept-hooks` approves unseen shell hooks. The only clean way to make it read-only is to restrict toolsets: `-t/--toolsets` (toolsets.py). There is no read-only file toolset (`file` = read_file, write_file, patch, search_files, toolsets.py:118-122), so the pinned `--toolsets=todo` gives a tools-free chat. Toolsets named `terminal`, `file`, `code_execution`, `delegation`, `browser`, `computer_use`, `cronjob` grant side effects. Whether an empty or `todo`-only toolset also drops MCP and plugin tools is UNVERIFIED; `--safe-mode` disables plugins and MCP but then requires `--model` and ignores the owner's config (gateway_chat.py:106-109), so it is noted rather than pinned. `--ignore-rules` is pinned so AGENTS.md, SOUL.md, `.cursorrules` and memory from the workspace are not injected (_parser.py:318-319). `--source=tool` keeps Hashi runs out of the owner's session pickers (_parser.py:322-323).

**Config and credentials.** `HERMES_HOME` (default `~/.hermes`) holds config.yaml, `.env` credentials, sessions and the gateway lock (hermes_constants.py:206-215); `-p/--profile <name>` switches to a named profile home and is consumed before argparse (_parser.py:14-17), so Hashi must never let a prompt or extra arg inject `-p`. Credentials come from `HERMES_HOME/.env`, pooled credentials (`hermes auth add`), and provider env vars (OPENROUTER_API_KEY is the common default; ANTHROPIC_API_KEY, OPENAI_API_KEY, GEMINI_API_KEY, XAI_API_KEY, DEEPSEEK_API_KEY, KIMI_API_KEY, MINIMAX_API_KEY and many more, hermes_cli/auth*.py). OAuth logins exist for Nous Portal, Codex, Qwen, xAI and others (hermes_cli/auth_nous.py, auth_codex.py, auth_xai.py).

**Rate limits.** Messages vary per provider; agent/agent_runtime_helpers.py:877 matches "usage limit reached"/"usage limit has been reached", and the failure reason codes above (`rate_limit`, `billing`) drive exit 75. Error text in `result.error` is the provider message.

**Install.** `curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash` (README.md:40); Windows `iex (irm https://hermes-agent.nousresearch.com/install.ps1)` (README.md:50). The local install should be repaired (rerun the installer) before Hashi can use it.

The JSONL `error_kind` is `result` because Hermes has no separate error event: a failed run is a `result` whose `error` is set and `exit_code` is non-zero. `HERMES_ACCEPT_HOOKS=0` in `env` is defensive (the variable only enables auto-accept when truthy, _parser.py:195-199). Both fixtures are DERIVED from source; ids, timestamps and token counts are invented.

## Success output (derived from source or docs, not recorded)

```
{"type": "system", "subtype": "init", "model": "anthropic/claude-sonnet-4.6", "session_id": "", "timestamp": 1763301600120}
{"type": "text", "text": "OK", "timestamp": 1763301603480}
{"type": "result", "session_id": "20261116_153003_a1b2c3", "exit_code": 0, "text": "OK", "tokens": {"input": 9812, "output": 4, "total": 9816, "cache_read": 0, "cache_write": 0}, "duration_ms": 3390, "timestamp": 1763301603510}
```

## Error output

```
{"type": "system", "subtype": "init", "model": "", "session_id": "", "timestamp": 1763301600120}
{"type": "result", "session_id": "", "exit_code": 1, "text": "", "tokens": {"input": 0, "output": 0, "total": 0, "cache_read": 0, "cache_write": 0}, "duration_ms": 41, "error": "credentials or agent init failed", "timestamp": 1763301600161}
```
