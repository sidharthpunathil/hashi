# Ante (`ante`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **docs**. The spec Hashi runs is `ANTE` in `src/agent/specs.rs`.

- **Source:** github:AntigmaLabs/ante (docs-site + public crates only; the `ante` binary itself is closed, distributed under BINARY-TERMS.md)
- **License:** Apache-2.0 (repo: docs and ante-sdk/llm/exec/protocol-shape crates); prebuilt binary under BINARY-TERMS.md
- **Version checked:** 0.3.5
- **Install:** curl -fsSL https://ante.run/install.sh | bash
- **Headless call:** `ante --output-format json`, prompt flag eq `--prompt=`
- **Runs tools without asking when headless:** no

## Notes

**What it is.** Antigma Labs' self-contained Rust coding agent (`curl -fsSL https://ante.run/install.sh | bash`, README.md:31-34). Not a Claude Code or Gemini CLI fork; its JSON output is its own daemon protocol, not Claude Code's result object. The repo only publishes docs and SDK crates; the CLI binary is closed, so everything below is from `docs-site/docs/**` (cached at ~/.opensrc/repos/github.com/AntigmaLabs/ante/main, CHANGELOG v0.3.5 2026-11-11). Fixtures derived from the protocol reference, not run.

**Headless.** `ante -p/--prompt <PROMPT>` runs one task and exits (`usage/headless.mdx:8-20`, `reference/cli-reference.mdx:44,70`). Stdin, when piped, is concatenated before the prompt (headless.mdx:36); /dev/null is harmless (UNVERIFIED). Headless appends the cwd folder structure to the prompt (headless.mdx:113-115). Auth is checked eagerly and fails fast (headless.mdx:122). The default prompt tells the model the run is a single unattended turn (headless.mdx:121).

**Prompt passing.** Use `--prompt=<text>`: the CLI is a Rust binary with clap-style help (`-h/--help`, `-V/--version`), where an attached `=` value is taken literally even when it starts with `-` (UNVERIFIED: binary not inspectable). Whether `--` plus a positional works is undocumented; permission.mdx:32-33 shows `ante --yolo "..."` with a bare positional, so a positional prompt may also exist (UNVERIFIED).

**Permissions (important).** Modes `strict` (default interactive; rules first, then known-safe read-only Bash allowed, everything else asks), `auto`, `yolo` (`configuration/permission.mdx:10-14`). **Headless defaults to `yolo`** (permission.mdx:41, headless.mdx:54,120), so tools run unasked unless Hashi pins `--permission-mode strict`; with an explicit `strict`/`auto` any call needing approval is denied immediately (no hang). `--tools Read,Glob,Grep` restricts the tool set (headless.mdx:164-170). No OS sandbox documented.

**Output `--output-format json`** (headless.mdx:92-98, `reference/protocol-reference.mdx`): JSONL `EventMsg` envelopes `{timestamp, id:"evt_...", event, parent}`. `event` is a Rust **externally tagged enum**: unit variants are bare strings (`"Goodbye"`, `"CompactStart"`), data variants are single-key objects (`{"AgentMessage":"..."}`, `{"UsageUpdate":{...}}`). **Hashi gotcha:** `kind_ptr` cannot select a value here; the kind is the single key of `/event` (or the string itself). Either add a "tag-key" kind mode or match on pointer presence: text = `/event/AgentMessage` (complete message, deltas are suppressed headless, headless.mdx:119; keep the last), session = `/event/SessionStart/session_id` (`ses_<ULID>`, protocol-reference.mdx:402-435), usage = sum of `/event/UsageUpdate/usage/{input_tokens,output_tokens}` (protocol-reference.mdx:767-781; may also carry `cache_read_tokens`, `cache_creation_tokens`; sub-agent usage is forwarded too), or take `/event/SessionEnd/usage` once (protocol-reference.mdx:497-512). Errors: `{"Error":"<message>"}` (protocol-reference.mdx:855-861) and `TurnEnd.status` = `{"Error":{"kind","headline","details"}}` (protocol-reference.mdx:578-592; `kind:"oauth"` means sign-in missing/expired). Whether headless JSON prints `SessionStart`/`SessionEnd` and the order shown in the fixture are UNVERIFIED. No cost field.

**Resume.** `-r/--resume <SESSION_ID>` replays the saved session then runs the new prompt (headless.mdx:125-147); bare `--resume` errors headless. `--no-session-save` conflicts with resume. Sessions live in `$ANTE_HOME/sessions/<id>/`.

**Model.** `-m/--model <MODEL>`, `--provider <PROVIDER>`, `--effort` (headless.mdx:49-52).

**Env/config.** `ANTE_HOME` overrides `~/.ante` (settings, catalog, `auth/credentials/`, sessions; `reference/storage-reference.mdx:12-43`). `ANTE_PROFILE`/`--profile <name>` picks `~/.ante/<name>.settings.json`; the built-in `bare` profile disables skills, MCP, session saving and auto-memory (cli-reference.mdx:30). Telemetry is exported by default from headless runs to telemetry.ante.run; `ANTE_TELEMETRY=off` disables it (`configuration/preference.mdx:351,359-361`). Update checks run in the background at most hourly (`start/update.mdx:112`); no documented disable switch. Provider keys: preference.mdx:323-332.

**Exit codes.** Non-zero when the daemon dies or auth fails (headless.mdx:122-123); exact codes undocumented.

**Platforms.** macOS and Linux only; Windows via WSL (README warning).

## Success output (derived from source or docs, not recorded)

```
{"timestamp":"2026-11-16T10:00:00Z","id":"evt_01JDA0000000000000000000A1","event":{"SessionStart":{"model":{"id":"claude-sonnet-5-5","max_tokens":8192},"provider":{"id":"anthropic","display_name":"Anthropic","base_url":"https://api.anthropic.com/v1"},"session_id":"ses_01JDA0000000000000000000S1","cwd":"/srv/hashi/ws/u1","permission_mode":"strict","skills":[],"subagents":[]}},"parent":null}
{"timestamp":"2026-11-16T10:00:00Z","id":"evt_01JDA0000000000000000000A2","event":{"ExtensionRefreshed":{"session_id":"ses_01JDA0000000000000000000S1","skills":[],"subagents":[],"mcp_servers":[]}},"parent":null}
{"timestamp":"2026-11-16T10:00:00Z","id":"evt_01JDA0000000000000000000A3","event":{"TurnStart":{"turn_id":"op_01JDA0000000000000000000O1"}},"parent":"op_01JDA0000000000000000000O1"}
{"timestamp":"2026-11-16T10:00:01Z","id":"evt_01JDA0000000000000000000A4","event":{"ToolStart":{"id":"tool_use_01","name":"Read","args":{"file_path":"/srv/hashi/ws/u1/src/lib.rs"}}},"parent":"op_01JDA0000000000000000000O1"}
{"timestamp":"2026-11-16T10:00:01Z","id":"evt_01JDA0000000000000000000A5","event":{"ToolEnd":{"tool_use_id":"tool_use_01","tool_name":"Read","status":"Completed","result_json":{"content":"pub fn add(a: i32, b: i32) -> i32 { a + b }"}}},"parent":"op_01JDA0000000000000000000O1"}
{"timestamp":"2026-11-16T10:00:02Z","id":"evt_01JDA0000000000000000000A6","event":{"UsageUpdate":{"usage":{"input_tokens":5200,"output_tokens":40},"context":{"used_tokens":5240,"limit_tokens":200000}}},"parent":"op_01JDA0000000000000000000O1"}
{"timestamp":"2026-11-16T10:00:02Z","id":"evt_01JDA0000000000000000000A7","event":{"AgentMessage":"`add` returns the sum of its two arguments."},"parent":"op_01JDA0000000000000000000O1"}
{"timestamp":"2026-11-16T10:00:02Z","id":"evt_01JDA0000000000000000000A8","event":{"UsageUpdate":{"usage":{"input_tokens":5300,"output_tokens":12},"context":{"used_tokens":5312,"limit_tokens":200000}}},"parent":"op_01JDA0000000000000000000O1"}
{"timestamp":"2026-11-16T10:00:02Z","id":"evt_01JDA0000000000000000000A9","event":{"TurnEnd":{"turn_id":"op_01JDA0000000000000000000O1","status":"Completed","steps":2}},"parent":"op_01JDA0000000000000000000O1"}
{"timestamp":"2026-11-16T10:00:02Z","id":"evt_01JDA0000000000000000000B1","event":{"SessionEnd":{"session_id":"ses_01JDA0000000000000000000S1","reason":"Shutdown","usage":{"input_tokens":10500,"output_tokens":52}}},"parent":null}
{"timestamp":"2026-11-16T10:00:02Z","id":"evt_01JDA0000000000000000000B2","event":"Goodbye","parent":null}
```

## Error output

```
{"timestamp":"2026-11-16T10:00:00Z","id":"evt_01JDA0000000000000000000C1","event":{"SessionStart":{"model":{"id":"claude-sonnet-5-5","max_tokens":8192},"provider":{"id":"anthropic","display_name":"Anthropic","base_url":"https://api.anthropic.com/v1"},"session_id":"ses_01JDA0000000000000000000S2","cwd":"/srv/hashi/ws/u1","permission_mode":"strict","skills":[],"subagents":[]}},"parent":null}
{"timestamp":"2026-11-16T10:00:01Z","id":"evt_01JDA0000000000000000000C2","event":{"Error":"Rate limit exceeded (429): please retry after 20s"},"parent":"op_01JDA0000000000000000000O2"}
{"timestamp":"2026-11-16T10:00:01Z","id":"evt_01JDA0000000000000000000C3","event":{"TurnEnd":{"turn_id":"op_01JDA0000000000000000000O2","status":{"Error":{"kind":"rate_limit","headline":"Rate limit exceeded","details":["429 Too Many Requests"]}},"steps":1}},"parent":"op_01JDA0000000000000000000O2"}
```
