# Amp (`amp`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **docs**. The spec Hashi runs is `AMP` in `src/agent/specs.rs`.

- **Source:** npm:@ampcode/cli-darwin-arm64@0.0.1794816357-gc47859 (closed; Bun-compiled binary, embedded JS and help strings grepped, not executed) plus https://ampcode.com/docs/cli/execute-mode, /docs/cli/streaming-json, /docs/cli/settings, /docs/tools
- **License:** Proprietary (SEE LICENSE IN LICENSE.md)
- **Version checked:** 0.0.1794816357-gc47859
- **Install:** curl -fsSL https://ampcode.com/install.sh | bash | npm install -g @ampcode/cli (formerly @sourcegraph/amp, now a shim)
- **Headless call:** `amp --stream-json`, prompt flag eq `--execute=`
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

## How this was derived
Nothing was executed. `@sourcegraph/amp` is now a shim ("Renamed to @ampcode/cli"); `@ampcode/cli` ships per-platform Bun-compiled binaries. I grepped `@ampcode/cli-darwin-arm64@0.0.1794816357-gc47859` (`strings` plus raw-byte search) for flag help and error text, and read the official docs pages execute-mode, streaming-json, settings and tools. The parser is commander (strings `exitOverride`, `configureOutput`, `optional-option`). The stream-json event schema is documented verbatim at https://ampcode.com/docs/cli/streaming-json; fixtures are adapted from its examples, not recorded.

## Headless
- `-x, --execute [message]`: "In execute mode, agent will execute provided prompt (either as argument, or via stdin). By default, only the last assistant message is printed. Enabled automatically when redirecting stdout." Piped stdin is combined with the `-x` text; `/dev/null` should add nothing (UNVERIFIED).
- Prompt: `--execute=<prompt>` (commander splits a long option at the first `=`, so a leading `-` stays in the value; UNVERIFIED on this binary). `--` is not documented for the root command; avoid it.
- `--stream-json`: "output in Claude Code-compatible stream JSON format"; requires `--execute` ("The --stream-json flag requires --execute mode"). `--stats` ("output JSON with both result and token usage data (for /evals)") is mutually exclusive with it.

## Output (docs streaming-json)
`system/init {cwd, session_id:"T-...", tools, mcp_servers, agent_mode?, subagents}`, `user`, `assistant {message{content[text|tool_use|thinking], stop_reason, usage?{input_tokens, cache_creation_input_tokens?, cache_read_input_tokens?, output_tokens, max_tokens?, service_tier?}}, parent_tool_use_id, session_id}`, `system/subagent_finished`, `system {subtype:error_max_turns|error_during_execution, error}`, and the terminal `result`. Success: `{type:"result", subtype:"success", duration_ms, duration_api_ms?, is_error:false, num_turns, result, session_id, usage?, permission_denials?}`. Failure: `{..., subtype:"error_during_execution"|"error_max_turns", is_error:true, error, ...}` with **`error`, not `result`**. That is why format is `json` (last JSON line, `error_ptr=/error`) rather than `claude_result`: Hashi's Claude parser would read the empty `result` and report only the subtype. `result` is "the only signal that the run is over" and Amp waits for async subagents before exiting.
- Usage: `result.usage` is optional in the schema (and absent in the docs examples); per-call usage is on each `assistant` event. With the `json` format Hashi only sees `result.usage`, so tokens may be missing; switching to jsonl with `usage_kind=assistant` would sum `message.usage` but then failures need `error_kind=result`, which the current jsonl parser would also fire on success. No cost field. `input_tokens` excludes cache creation/read tokens (docs example: 10 input, 16256 cache_creation).

## Session and model
- Session id is the thread id `T-<uuid>` (`session_id` on every line). Resume is a subcommand, `amp threads continue <T-id> --execute=<prompt> --stream-json` (docs and help "continue [threads...]"), so it cannot be expressed as a resume_flag; resume_flag is null. Threads live on ampcode.com; execute mode archives a new thread when done unless `--no-archive-after-execute`.
- Model: `--model provider/name` exists but "The --model flag is not enabled for your account." is a gated error and "Use either --model or --mode, not both." `--mode` selects the agent mode (values UNVERIFIED; docs say new threads start in Standard mode; `--fast` toggles Fast). model_flag left null.

## Permissions (most important)
- Docs /docs/tools: "By default, Amp does not ask for approval before running tools." The TUI intro says "Amp runs tools and commands without asking for confirmation." So with no extra config Bash and edit_file run unattended: auto_approves_tools_headless=true.
- Some actions are still blocked in execute mode: a permission rule that asks, a guarded file such as `.env`, or a non-allowlisted command produce "... tool requires user approval, but no interactive UI is available. Rerun with --dangerously-allow-all to bypass" and "... tool is not allowed to run in execute mode. Rerun with --dangerously-allow-all to bypass." (binary strings). Built-in ask defaults appear to include `git push*`, `git commit*`, `git branch -D*`, `git checkout HEAD*` (strings near the settings descriptions; UNVERIFIED as defaults).
- There is no read-only CLI flag. Safe headless use needs a Hashi-written settings file passed with `--settings-file <path>` (or `AMP_SETTINGS_FILE`) containing for example `{"amp.tools.disable":["*"], "amp.mcpPermissions":[{"matches":{"command":"*"},"action":"reject"},{"matches":{"url":"*"},"action":"reject"}], "amp.updates.mode":"disabled"}` or an allowlist via `amp.tools.enable`. That needs a file path the static spec cannot pin, so it is not in pinned_args. Hazard: workspace settings `.amp/settings.json` (searched upward from cwd, or cwd outside git) override user settings, so a workspace file could undo the deny; AGENTS.md and `.agents/skills`, `.claude/skills` in the workspace are loaded too.
- Strip `--dangerously-allow-all` (also the `amp.dangerouslyAllowAll` setting: "Disable all command confirmation prompts").

## Auth and config
- `AMP_API_KEY`: access token (`sgamp_...`); execute docs: "The CLI rejects the short-lived session token that amp login stores". Warning string: "AMP_API_KEY is set and takes precedence over stored credentials." `AMP_URL` overrides the server.
- Config: `~/.config/amp/settings.json` (XDG_CONFIG_HOME on Linux). Data and `secrets.json`: `~/.local/share/amp`; the bundle (`Ra` module) honours `XDG_DATA_HOME` only on Linux, using `~/.local/share` on macOS and Windows. `AMP_HOME` is the install directory (`$AMP_HOME/bin`), not config. No single env var relocates all state on macOS, so config_dir_env is null; per-account separation should use `AMP_API_KEY`.
- Updates: `AMP_SKIP_UPDATE_CHECK=1` "disables all update checks" (docs settings `amp.updates.mode`, default "auto" installs updates).

## Limits
Strings: "Rate limit exceeded. Try again shortly.", "Rate Limit Hit", `rate_limit_exceeded`, `rate_limit_error`, "You are out of credits. Add paid credits or choose a paid tier.", "Out of Credits", "You've reached your free usage limit", "Low Credit Balance". Which of these reach `result.error` in execute mode is UNVERIFIED; the error fixture message is one of these strings placed in the documented failure shape.

## Other
- Exit codes undocumented (non-zero on failure assumed, UNVERIFIED). Git repo not required. A leading `/` in an `-x` prompt is not documented as a slash command (UNVERIFIED; the TUI command palette uses `/`).
- `--no-ide`, `--no-notifications`, `--no-remote-control-terminal` exist as root flags (strings); pinned to keep the run from connecting to an IDE, playing sounds or exposing the terminal to ampcode.com.

## Success output (derived from source or docs, not recorded)

```
{"type":"system","subtype":"init","cwd":"/srv/hashi/ws/u1","session_id":"T-f9941a55-3765-421e-972f-05dc1138c3a3","tools":["Read","glob","Grep"],"mcp_servers":[],"agent_mode":"smart","subagents":[]}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"what is 3 + 5?"}]},"parent_tool_use_id":null,"session_id":"T-f9941a55-3765-421e-972f-05dc1138c3a3"}
{"type":"assistant","message":{"type":"message","role":"assistant","content":[{"type":"text","text":"8"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"cache_creation_input_tokens":16256,"cache_read_input_tokens":0,"output_tokens":99,"max_tokens":968000,"service_tier":"standard"}},"parent_tool_use_id":null,"session_id":"T-f9941a55-3765-421e-972f-05dc1138c3a3"}
{"type":"result","subtype":"success","duration_ms":5400,"is_error":false,"num_turns":1,"result":"8","session_id":"T-f9941a55-3765-421e-972f-05dc1138c3a3","usage":{"input_tokens":10,"cache_creation_input_tokens":16256,"cache_read_input_tokens":0,"output_tokens":99}}
```

## Error output

```
{"type":"system","subtype":"init","cwd":"/srv/hashi/ws/u1","session_id":"T-5e2b8c41-0d9f-4a7e-b3c6-91f0e2d4a8b7","tools":["Read","glob","Grep"],"mcp_servers":[],"subagents":[]}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"what is 3 + 5?"}]},"parent_tool_use_id":null,"session_id":"T-5e2b8c41-0d9f-4a7e-b3c6-91f0e2d4a8b7"}
{"type":"result","subtype":"error_during_execution","duration_ms":900,"is_error":true,"num_turns":0,"error":"You are out of credits. Add paid credits or choose a paid tier.","session_id":"T-5e2b8c41-0d9f-4a7e-b3c6-91f0e2d4a8b7"}
```
