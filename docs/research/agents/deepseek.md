# DeepSeek Harness (`deepseek`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `DEEPSEEK` in `src/agent/specs.rs`.

- **Source:** github:deepseek-ai/deepseek-harness (master, apps/cli @deepseek-ai/dsh 0.2.2-rc.3; headless profile packages/bundle/headless). Also read npm:dsh-tui 0.2.19 (github MashedPotato817/dsh-tui), a third-party TUI client.
- **License:** MIT
- **Version checked:** 0.2.2-rc.3
- **Install:** npm i -g @deepseek-ai/dsh (or npx @deepseek-ai/dsh ...)
- **Headless call:** `dsh headless --json`, prompt dashdash
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

**What it is.** DeepSeek's official open-source **DeepSeek Harness** (`dsh`, npm `@deepseek-ai/dsh`, developer preview with breaking changes expected). The requested binary name `dsh-tui` is a **third-party** npm package (`dsh-tui` 0.2.19 by MashedPotato817, "Terminal client for DeepSeek Harness") that only talks to an already running `dsh web` host over HTTP (`DSH_URL`, default http://127.0.0.1:3080; `bin/tui.js:22`). It has `dsh-tui run <prompt> [--session <id>|--resume] [--json]` printing `{sessionId, agentPreset, reply, turn, toolCalls, usage{input,output}, turnEnd}` (bin/tui.js:138-167), but its hand-rolled parser takes the **first argument not starting with `--` as the prompt** (tui.js:44), so `run --session <id> <prompt>` makes the id the prompt, `--` is rejected (tui.js:66-68), and without `--cwd` the session uses the host's directory, not the caller's (`lib/session.js:44-47`). It is unsuitable for Hashi; this spec targets the official `dsh headless` instead and lists `dsh-tui` only as an alias. Not a Claude Code or Gemini CLI fork; JSON is its own NDJSON. Fixtures derived from source, not run.

**Headless.** `dsh headless [options] [task...]` (= `dsh --profile headless`; `docs/cli-help.md:100-135`). Parser: commander, `packages/bundle/headless/src/startup.ts:53-75`; variadic `[task...]` joined by spaces, a lone `-` reads stdin, a whitespace-only task is a usage error (startup.ts:117-128). The launcher (`apps/cli/src/args.ts`, commander with `passThroughOptions()` + `allowUnknownOption()`) forwards everything after the profile name to the app, so `dsh headless --json --approval-policy never -- <prompt>` reaches commander, which honours `--` (the JSON pre-scan also stops at `--`, startup.ts:84-92). End-to-end `--` through the launcher is UNVERIFIED. A positional task means stdin is not read. Must run through the `dsh` launcher (package README "Known Limitations").

**Output `--json`** (`packages/bundle/headless/src/json-stream.ts`): `session {sessionId, cwd}` first (line 320), then `status {phase: turn_start|step_start|step_end|turn_end, turn, step?, usage?, reason?}` (261-291), `text {text}` and `thinking {text}` from committed assistant messages (272-277), `tool_call {callId, tool, input}`, `tool_result {callId, status: completed|error, result, error?}` (293-313), and terminal `final {sessionId, reason, text}` (327, never truncated). Step usage is `{inputTokens, outputTokens, totalTokens?, cacheReadTokens?, cacheWriteTokens?, reasoningTokens?}` on `step_end`, summed across retried attempts and omitted when any attempt lacked a sample (180-227, 279-288). Errors raised outside a turn write `{type:"error", message}` and end without `final` (startup.ts:105-115, index.ts:310). **An in-turn failure still ends with `final` (often empty text) and no `error` event**, with `final.reason.kind == "error"` and `reason.error{code,message}`; exit code is 0 only when `reason.kind == "completed"`, else 1 (index.ts:439-443), and stderr gets `dsh: <code>: <message>`. Hashi must therefore treat a non-zero exit (or `final.reason.kind != "completed"`) as failure. Every string except `final.text` is capped at 8 KiB and each event line at 32 KiB (json-stream.ts:15-19). No cost field.

**Resume.** `--session-id <id>` adopts an existing persisted session (`session-<uuid>`); an unknown id is an error, and adoption is refused if the session was recorded in a **different cwd** (index.ts:223-240), so resume must use the same workspace. Ids are chosen by dsh (from the `session` event); Hashi cannot pre-assign.

**Model.** No CLI model flag; the model route comes from profile config (`$DSH_HOME/profiles/headless/cordis.patch.yml` or `--patch` overlays). `model_flag` null.

**Permissions/sandbox.** The headless profile sets `sandbox-policy` mode `process.env.DSH_PERMISSION_MODE ?? 'workspace-write'` rooted at `process.cwd()` (`packages/bundle/headless/cordis.patch.yml`), so tools run unattended but writes are confined to the workspace by the local sandbox. `--approval-policy never` (pinned) rejects any call that needs human approval instead of relying on the default; headless has no live approval channel (README "Experimental Auto review"). `--permission-mode <preset>` and `--add-dir` widen access; `DSH_PERMISSION_MODE` env does too (Hashi should not pass it through).

**Credentials.** The headless profile disables the stored credential store (`credentials`, `authorization`, `deepseek-account` rows disabled in cordis.patch.yml); API keys come from the launch env, then **`<cwd>/.env`**, then `$DSH_HOME/.env` (README "Credentials"). Gotcha: a `.env` in the workspace can inject keys. `DEEPSEEK_API_KEY` (+ `DEEPSEEK_BASE_URL`) drive the default DeepSeek route. No interactive sign-in for headless, so `sign_in` is null.

**Rate limits.** `packages/llm/llm-deepseek/src/transport.ts:33` maps HTTP 429 or `rate_limit_error` to code `RATE_LIMIT`; `llm-pi-ai/src/stream.ts:53` maps `429`/`rate.?limit`. These surface as `reason.error.code` and the stderr line `dsh: RATE_LIMIT: ...`.

**Env.** `DSH_HOME` (profiles, .env, sessions), `DSH_PERMISSION_MODE`, `HTTP(S)_PROXY`. Telemetry: the base profile includes an OTel session-telemetry plugin (`packages/bundle/base/cordis.patch.yml:218`); disable switch UNVERIFIED.

## Success output (derived from source or docs, not recorded)

```
{"type":"session","sessionId":"session-3f1c2a9e-7b4d-4e2a-9c1f-0a8b7d6e5f41","cwd":"/srv/hashi/ws/u1"}
{"type":"status","phase":"turn_start","turn":1}
{"type":"status","phase":"step_start","turn":1,"step":1}
{"type":"tool_call","callId":"call_01","tool":"read_file","input":{"path":"src/lib.rs"}}
{"type":"status","phase":"step_end","turn":1,"step":1,"usage":{"inputTokens":5123,"outputTokens":40,"cacheReadTokens":0}}
{"type":"tool_result","callId":"call_01","status":"completed","result":"pub fn add(a: i32, b: i32) -> i32 { a + b }"}
{"type":"status","phase":"step_start","turn":1,"step":2}
{"type":"text","text":"`add` returns the sum of its two arguments."}
{"type":"status","phase":"step_end","turn":1,"step":2,"usage":{"inputTokens":5210,"outputTokens":12,"cacheReadTokens":4096}}
{"type":"status","phase":"turn_end","turn":1,"reason":{"kind":"completed"}}
{"type":"final","sessionId":"session-3f1c2a9e-7b4d-4e2a-9c1f-0a8b7d6e5f41","reason":{"kind":"completed"},"text":"`add` returns the sum of its two arguments."}
```

## Error output

```
{"type":"session","sessionId":"session-3f1c2a9e-7b4d-4e2a-9c1f-0a8b7d6e5f41","cwd":"/srv/hashi/ws/u1"}
{"type":"status","phase":"turn_start","turn":1}
{"type":"status","phase":"turn_end","turn":1,"reason":{"kind":"error","error":{"code":"RATE_LIMIT","message":"429 rate limit reached"}}}
{"type":"final","sessionId":"session-3f1c2a9e-7b4d-4e2a-9c1f-0a8b7d6e5f41","reason":{"kind":"error","error":{"code":"RATE_LIMIT","message":"429 rate limit reached"}},"text":""}
```
