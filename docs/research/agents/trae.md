# TRAE CLI (`trae`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **docs**. The spec Hashi runs is `TRAE` in `src/agent/specs.rs`.

- **Source:** closed: Go binary trae-cli v0.121.3 (linux_amd64 from lf-cdn.trae.com.cn), inspected with strings; it embeds its own Chinese user docs (non-interactive-mode, cli-reference, settings, env-vars). Not github:bytedance/trae-agent.
- **License:** Proprietary (ByteDance; TRAE Enterprise accounts only)
- **Version checked:** 0.121.3
- **Install:** sh -c "$(curl -L https://lf-cdn.trae.com.cn/obj/trae-com-cn/trae-cli/install.sh)" (installs ~/.local/share/trae-cli/trae-cli with symlinks traecli, trae-cli, trae-agent, ta in ~/.local/bin)
- **Headless call:** `traecli -p --output-format stream-json`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

**What it is.** `traecli` is ByteDance's closed-source **TRAE CLI** (internal codename **Coco**: env vars use the `COCO_` prefix, init event carries `coco_version`), a Go/cobra binary. It is NOT `github:bytedance/trae-agent` (that is a separate Apache-2.0 Python project whose CLI is `trae-cli run "<task>"` / `trae-cli interactive`, rich text output, no JSON stream). Install script `https://lf-cdn.trae.com.cn/obj/trae-com-cn/trae-cli/install.sh` downloads `trae-cli_<ver>_<os>_<arch>.tar.gz` (latest v0.121.3) and symlinks `traecli`, `trae-cli`, `trae-agent`, `ta`. I downloaded the linux_amd64 binary (not run) and read the user docs embedded in it; citations below are those embedded doc pages ("非交互模式", "命令参考（CLI Reference）", "配置参考", "环境变量参考"). Third-party summary: https://clawhub.ai/alexjunanjing-2/openclaw-traecli. Only TRAE Enterprise accounts can use it; the embedded docs reference ByteDance-internal endpoints (codebase-api.byted.org), so availability outside ByteDance/Enterprise is limited.

**Headless.** `traecli -p "<prompt>"` (`--print`). In -p mode the reply goes to stdout, permission dialogs are not shown (unauthorised tools are skipped) and the agent does not ask questions (非交互模式 doc). **Stdin is NOT read** in -p mode ("`traecli -p` 当前不读取 stdin"), so /dev/null is harmless. **Only the first positional argument is the prompt**; extra words are ignored by cobra, so pass the prompt as one argv element. cobra/pflag honours `--`, so `-p ... -- <prompt>` is safe (standard pflag behaviour, UNVERIFIED by running).

**Output.** `--output-format text|json|stream-json` (`--json` == `--output-format json`, mutually exclusive). `json`: one object `{session_id, agent_states[], message{role,content}, tools[], error?}` with **no usage**; errors still exit 0 with `error` set. `stream-json` (chosen): NDJSON shaped like Claude Code's stream-json: `system/init {session_id, uuid, cwd, tools, model, permission_mode, coco_version}`, `assistant {message{role,content[]}}`, `user/tool_result {tool_use_id, tool_name, is_error, content}`, `system/status`, `stream_event` (only with `--include-partial-messages`), and a final `result {subtype:"success"|"error_during_execution", session_id, duration_ms, num_turns, result, usage{input_tokens,output_tokens}, total_cost_usd, error?}`. The `result` line is always written and is always the last stdout line; non-JSON stdout from libraries is diverted to stderr with a `[stdout-guard]` prefix. Exit 0 on success, 1 on run error (with `result.subtype=error_during_execution` and `result.error`). It is close to Claude Code's result object but `is_error` is not documented (UNVERIFIED), so Hashi should read the last line as `json` and treat a non-empty `/error` as failure rather than use `claude_result`. Error string wording in the fixture is illustrative.

**Resume.** `--session-id <id>` "specifies the session id (creates it on first use or resumes it)", so Hashi can pre-assign ids. `--resume [id]` has an optional value (resumes the most recent session in the cwd when bare); with pflag's NoOptDefVal a separate `--resume <id>` may not consume the id and would make it the prompt, so use `--session-id` (UNVERIFIED pflag detail).

**Model.** No model flag. Use `-c model.name=<model>` (config override `k=v`, repeatable) or `/model`; `traecli models` lists models. Because `-c` can also set `permission_mode=bypass_permissions`, it is listed as unsafe.

**Permissions.** Global flags `-y/--yolo` (== `--permission-mode bypass_permissions`), `--permission-mode default|plan|bypass_permissions`, `--allowed-tool`, `--disallowed-tool` (repeatable), `--query-timeout`, `--bash-tool-timeout`, `--mcp-init-wait-seconds`, `-w/--worktree`. Default mode is `default`, which in -p mode silently skips unauthorised tools, so the safe default is no extra flag; add `--disallowed-tool Bash --disallowed-tool Edit --disallowed-tool Write` for read-only. No OS sandbox documented.

**Config/env.** User config `~/.trae/traecli.yaml`; project `.trae/traecli.yaml` (legacy `.coco/coco.yaml`, `.agents/coco.yaml` read-only), searched from cwd up to the git root; cache `~/Library/Caches/coco` / `~/.cache/coco`; IDE lock files in `~/.coco/ide/`. No env var relocates all state (only HOME), so `config_dir_env` is null. Env: `COCO_NEXTCODE_URL`, `COCO_NEXTCODE_HEADERS` (API endpoint/headers), `COCO_DISABLE_CLOUD_SESSION_UPLOAD=1` (sessions are uploaded to the cloud by default after login; pinned off), `COCO_IDE_PLUGIN_PORT`, OTEL_* telemetry (off unless configured). `COCO_BYTED_GPT_API_KEY` appears in the binary (purpose UNVERIFIED).

**Auto-update.** The CLI upgrades itself at startup when a newer version exists (clawhub guide); a config key `auto_update` exists in the binary's config structs (`-c auto_update=false` is a plausible but UNVERIFIED way to disable it). Hashi's timeout covers a slow update.

**Slash commands.** `traecli -p "/commit"` and `traecli -p /builtin:review` are documented, so a leading `/` runs a command; custom commands live in `.trae/commands/*.md` (legacy `.traecli/commands`).

**Rate limits.** Binary strings include `rate limit exceeded` and `[Error: rate_limit`; exact user-facing wording UNVERIFIED.

## Success output (derived from source or docs, not recorded)

```
{"type":"system","subtype":"init","session_id":"traecli-hashi-7f3a9c21","uuid":"6c1f0b2e-1a2b-4c3d-8e9f-000000000001","cwd":"/srv/hashi/ws/u1","tools":["Read","Glob","Grep","Bash","Edit","Write"],"model":"doubao-seed-code","permission_mode":"default","coco_version":"0.121.3"}
{"type":"assistant","session_id":"traecli-hashi-7f3a9c21","uuid":"6c1f0b2e-1a2b-4c3d-8e9f-000000000002","message":{"role":"assistant","content":[{"type":"text","text":"Paris."}]}}
{"type":"result","subtype":"success","session_id":"traecli-hashi-7f3a9c21","uuid":"6c1f0b2e-1a2b-4c3d-8e9f-000000000003","duration_ms":4120,"num_turns":1,"result":"Paris.","usage":{"input_tokens":8123,"output_tokens":5},"total_cost_usd":0.0021}
```

## Error output

```
{"type":"system","subtype":"init","session_id":"traecli-hashi-7f3a9c21","uuid":"6c1f0b2e-1a2b-4c3d-8e9f-000000000004","cwd":"/srv/hashi/ws/u1","tools":["Read","Glob","Grep"],"model":"doubao-seed-code","permission_mode":"default","coco_version":"0.121.3"}
{"type":"result","subtype":"error_during_execution","session_id":"traecli-hashi-7f3a9c21","uuid":"6c1f0b2e-1a2b-4c3d-8e9f-000000000005","duration_ms":310,"num_turns":0,"result":"","usage":{"input_tokens":0,"output_tokens":0},"total_cost_usd":0,"error":"rate limit exceeded"}
```
