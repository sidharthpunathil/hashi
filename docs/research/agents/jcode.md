# Jcode (`jcode`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `JCODE` in `src/agent/specs.rs`.

- **Source:** github:1jehuang/jcode
- **License:** MIT
- **Version checked:** 0.99.1 (source, master)
- **Install:** curl -fsSL https://jcode.sh/install | bash
- **Headless call:** `jcode run --ndjson`, prompt dashdash
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

Source: `github:1jehuang/jcode` (MIT, workspace version 0.99.1, Rust/clap), cached at `~/.opensrc/repos/github.com/1jehuang/jcode/master`. Not installed here, nothing was run; every fixture is derived from source.

**Headless entry point.** `jcode run [--json|--ndjson] <message>` (src/cli/args.rs:199-210, dispatched at src/cli/dispatch.rs:302-320). `message` is a plain required clap positional (no `allow_hyphen_values`), so `run ... -- <prompt>` is standard clap behaviour (UNVERIFIED by a test; tests only cover `run --json hello`, src/cli/args/tests.rs:941-972). `run` needs no daemon: it builds an `Agent` in-process (docs/SERVER_ARCHITECTURE.md:105, socket comment at src/cli/args.rs:95-103); a temporary server is started only if the `swarm` tool is used.

**Output.**
- `--json` (src/cli/commands.rs:2715-2725, struct at :2132-2138): ONE pretty-printed (multi-line) object `{session_id, provider, model, text, usage{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens}}`. On failure nothing is printed to stdout (the error propagates to main, exit non-zero, message on stderr). Hashi's `json` parser would have to parse the whole stdout, not the last line.
- `--ndjson` (recommended; :3106-3360): compact JSONL. First line `start {session_id, provider, model}` (:3121-3128); streaming events `text_delta {text}`, `text_replace {text}`, `text_suffix_replace`, `tool_start {id,name}`, `tool_input`, `tool_exec`, `tool_done {id,name,output,error}`, `tokens {input,output,cache_read_input,cache_creation_input}`, `message_end {stop_reason}`, `connection_type`, `status_detail`, `service_tier`, plus `auto_poke*` events (:3373-3584). Terminal line is `done {session_id, provider, model, text, usage{...}, ...}` (:3322-3343) on success or `error {session_id, provider, model, message}` (:3346-3357) on failure, followed by a non-zero exit. A mid-stream `ServerEvent::Error` is also written as `{"type":"error","message","retry_after_secs"}` (:3570-3581); whether that is always fatal is UNVERIFIED, Hashi will treat it as failure.
- **Usage caveat:** `tokens` events are re-emitted progressively within one API request (crates/jcode-app-core/src/agent/turn_streaming_mpsc.rs:801-830 and again at :1183), so they must NOT be summed. `done.usage` (and `--json` `usage`) is `agent.last_usage()`, i.e. the LAST API request only (crates/jcode-app-core/src/agent.rs:250-256, :1363), not the run total when tools caused several requests. Spec uses `usage_kind: "done"` (one line, so summing is harmless). No cost field.
- `text` in `done` is the concatenation of all text deltas across the run (state.text), including auto-poke follow-up turns.

**Auto-poke.** By default (`features.auto_poke`) `run` keeps sending follow-up turns while the session has unfinished todos (:2745-2755, loop :2985-3090). `JCODE_RUN_AUTO_POKE=0` disables it; pinned in env for a single-turn contract.

**Resume.** Global `--resume <id>` (src/cli/args.rs:81-83, `num_args = 0..=1`): restores via `agent.restore_session` before the turn (:3096-3104). Because the value is optional, put `--resume <id>` AFTER `run` and before `--`, never immediately before the `run` token (clap could take `run` as the value). Session ids look like `session_<word>_<unix_ms>_<16 hex>` (crates/jcode-core/src/id.rs:262-270) and pass `is_safe_arg`. There is no way to pre-assign an id. Resume error behaviour when the id is unknown: UNVERIFIED (restore returns Err, so exit non-zero, nothing on stdout in --json).

**Model/provider.** Global `-m/--model <id>` (args.rs:117-119) and `-p/--provider <choice>` (default `auto`, args.rs:40-42). Note `-p` is PROVIDER, not print. `--provider-profile` for named OpenAI-compatible profiles.

**Permissions (unsafe by default).** There is no approval prompt for built-in tools: `bash`, `write`, `edit`, `apply_patch` just run in `run` mode. The only guard is a deterministic destructive-command gate inside the bash tool (crates/jcode-app-core/src/tool/bash_destructive_gate.rs:1-45) that refuses catastrophic targets and asks the model for a justification on risky ones. No `--yolo` style flag exists because nothing asks. The SDK's `auto_approve` (crates/jcode-sdk/src/client/types.rs:14) concerns only the ambient tool's PermissionRequest. Restricting tools: global `--tools <csv>` allow-list, `--tool-profile full|acp|minimal|none`, `--disabled-tools`, `--disable-base-tools` (args.rs:129-143; exported as `JCODE_TOOLS`/`JCODE_TOOL_PROFILE`/... at src/cli/dispatch.rs:123-132; resolution in crates/jcode-base/src/config.rs:930-975). Spec pins `--tools=read,ls,agentgrep` (read-only subset of the `minimal` profile names). `--tools=*` or `--tools=all` re-enables everything, so Hashi must strip operator `--tools`/`--tool-profile`. MCP servers from `~/.jcode/mcp.json` load in `run` unless `JCODE_RUN_MCP=0` (:2758-2767); pinned.

**Login hang hazard.** With no credentials and `--json`/`--ndjson`, `init_provider_for_run` allows login bootstrap and would show an interactive provider picker (src/cli/provider_init.rs:1473-1479, :1862-1893). `JCODE_NON_INTERACTIVE=1` turns that into `No credentials configured. Run 'jcode login' or set ANTHROPIC_API_KEY to authenticate.` (:1878). Pinned.

**Config dir.** `JCODE_HOME` relocates `~/.jcode` and the app config dir, and sandboxes external auth files (e.g. reused Claude/Codex creds) under `$JCODE_HOME/external/`; it also disables macOS Keychain use (crates/jcode-storage/src/lib.rs:163-229).

**Credentials.** Auto-detects many providers: `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_AUTH_TOKEN`, `OPENAI_API_KEY`, `OPENROUTER_API_KEY`, `GEMINI_API_KEY`, `XAI_API_KEY`, `COPILOT_GITHUB_TOKEN`/`GH_TOKEN`/`GITHUB_TOKEN`, `CURSOR_API_KEY`, `DEEPSEEK_API_KEY` and dozens more provider keys (counted from crates/jcode-provider-metadata/src and crates/jcode-base/src/auth). `credential_envs` lists the main ones; the full list is longer (UNVERIFIED completeness). Also reuses Claude Max / ChatGPT subscription logins from other CLIs' files.

**Update and telemetry.** A background update check runs unless `--no-update` or `--quiet` (src/cli/startup.rs:494-511), and `--auto-update` defaults to true with no way to set it false from argv, so an update could be auto-installed: pin `--no-update`. Telemetry: `JCODE_NO_TELEMETRY=1` or `DO_NOT_TRACK=1` (TELEMETRY.md:492-510).

**Limit patterns.** From `error_looks_like_usage_limit` (crates/jcode-base/src/provider/account_failover.rs:164-182) and crates/jcode-base/src/provider/model_provider_availability.rs:137.

**Slash commands.** `run_once_capture` sends the message as a plain user message (crates/jcode-app-core/src/agent/turn_execution.rs:74-99); no slash expansion seen in the run path (UNVERIFIED for skills).

**Install:** `curl -fsSL https://jcode.sh/install | bash` (README.md:51).

**argv, new run:** `["run", "--ndjson", "--no-update", "--quiet", "--tools=read,ls,agentgrep", "-m", "<model>", "--", "<prompt>"]`
**argv, resume:** `["run", "--ndjson", "--no-update", "--quiet", "--tools=read,ls,agentgrep", "--resume", "<session_id>", "--", "<prompt>"]`

## Success output (derived from source or docs, not recorded)

```
{"type":"start","session_id":"session_fox_1763300000000_3f9a1c22b7d04e15","provider":"claude","model":"claude-opus-4-6"}
{"type":"connection_type","connection":"https"}
{"type":"text_delta","text":"OK"}
{"type":"tokens","input":11234,"output":3,"cache_read_input":0,"cache_creation_input":null}
{"type":"message_end","stop_reason":"end_turn"}
{"type":"done","session_id":"session_fox_1763300000000_3f9a1c22b7d04e15","provider":"claude","model":"claude-opus-4-6","text":"OK","usage":{"input_tokens":11234,"output_tokens":3,"cache_read_input_tokens":0,"cache_creation_input_tokens":null},"upstream_provider":null,"connection_type":"https","connection_phase":null,"status_detail":null,"requested_service_tier":null,"reported_service_tier":null,"service_tier_report_status":"not_reported","served_service_tier":null}
```

## Error output

```
{"type":"start","session_id":"session_fox_1763300000000_3f9a1c22b7d04e15","provider":"claude","model":"claude-opus-4-6"}
{"type":"error","session_id":"session_fox_1763300000000_3f9a1c22b7d04e15","provider":"claude","model":"claude-opus-4-6","message":"429 Too Many Requests: rate limit exceeded"}
```
