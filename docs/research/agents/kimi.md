# Kimi Code (`kimi`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `KIMI` in `src/agent/specs.rs`.

- **Source:** github:MoonshotAI/kimi-code
- **License:** MIT
- **Version checked:** 2.6.2
- **Install:** curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash | irm https://code.kimi.com/kimi-code/install.ps1 | iex | npm i -g @moonshot-ai/kimi-code
- **Headless call:** `kimi --output-format stream-json`, prompt flag eq `--prompt=`
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

Read from a fresh shallow clone of `MoonshotAI/kimi-code` at commit 334d798 (2026-11-16, `apps/kimi-code/package.json` 2.6.2, matches the npm tarball @moonshot-ai/kimi-code 2.6.2). The opensrc cache at `~/.opensrc/repos/github.com/MoonshotAI/kimi-code/main` is stale (2.5.0) and `opensrc fetch` did not refresh it. Paths below are relative to `apps/kimi-code/src/cli/` unless they start with `packages/`. Not executed; fixtures derived from source.

**Headless.** `-p/--prompt <prompt>` switches to print mode (`options.ts:76-142`, uiMode `print`). The prompt must be the option value: the program's only positional is `[args...]` and any value there fails with `unknown command '<x>'` (`commands.ts:209-212`), so text after `--` is an error, not a prompt. Use `--prompt=<text>` (commander takes everything after the first `=`). An empty or whitespace-only prompt is rejected (`options.ts:78-80`). Stdin is not read in print mode.

**Output.** `--output-format text|stream-json` (`commands.ts:80-85`); env `KIMI_MODEL_OUTPUT_FORMAT` sets the default, flag wins (`options.ts:4-35`). stream-json is JSONL written by `PromptJsonWriter` (`prompt-render.ts:149-267`):
- first line, always: `{"role":"meta","type":"system.version","version"}` (`prompt-render.ts:366-388`, called unconditionally at `v2/run-v2-print.ts:155`). New since the 2.5.0 notes.
- `{"role":"assistant","content"?,"tool_calls"?:[{type:"function",id,function{name,arguments}}]}`: assistant text is buffered and flushed as ONE line per step (before each tool result and at turn end), not as deltas. `content` is omitted when empty, so a tool-call-only step has no `content`.
- `{"role":"tool","tool_call_id","content"}` per tool result.
- `{"role":"meta","type":"turn.step.retrying",failed_attempt,next_attempt,max_attempts,delay_ms,error_name,error_message,status_code?}` on provider retries (`prompt-render.ts:209-225`). Not terminal.
- Last line on success only: `{"role":"meta","type":"session.resume_hint","session_id","command","content"}` (`prompt-render.ts:390-410`, `v2/run-v2-print.ts:342`).
- Hook results are emitted as an extra assistant line (`prompt-render.ts:159-165`). Thinking is never written in stream-json.

**Parser.** text = `content` of the last `role==assistant` line that has a `content` (Hashi's `last` mode must skip assistant lines with no `content`, i.e. pure tool-call steps). Session id from the `session.resume_hint` line only. **No usage or cost anywhere in print-mode stdout** (re-checked in 2.6.2: neither `prompt-render.ts` nor `v2/run-v2-print.ts` writes token counts; the CHANGELOG `usage` frames are server WebSocket only). There is no error event: a failed run writes nothing more to stdout (possibly a `turn.step.retrying` meta line, and the `system.version` line) and prints `error: failed to run prompt: <code>: <message>` plus `See log: ...` on stderr, exit 1 (`main.ts:258-277`, `startup-error.ts:15-35`, failure text from `formatNativeTurnFailure` `v2/run-v2-print.ts:1219-1227`). So Hashi must treat "no resume_hint line" + non-zero exit as failure and fall back to stderr. Session busy: exit 4, and in stream-json a `{"type":"error","code","message","details"}` line on stdout (`v2/run-v2-print.ts:678-694`).

**Resume.** `-S/--session <id>` (hidden alias `-r/--resume`, `commands.ts:52-63`). Ids are `session_<uuid>` (`packages/agent-core-v2/src/workspace/sessionLifecycle/sessionLifecycleService.ts:1087-1089`), which pass `is_safe_arg`. A bare `-S` is rejected in prompt mode (`options.ts:130-132`). Resume fails with `Session "<id>" was created under a different directory.` if cwd differs (`v2/run-v2-print.ts:441-450`), so Hashi's per-token workspace must be stable. Ids cannot be pre-assigned. `--agent`/`--agent-file` cannot be combined with `-S` (`options.ts:122-129`).

**Model.** `-m/--model <alias>` is a config alias (for example `kimi-code/kimi-for-coding`), not a raw provider id; it also applies on resume (`v2/run-v2-print.ts:405-410, 455`). With no `default_model` and no `-m`: `No model configured. Run kimi and use /login ...` (`run-prompt.ts:49-56`). `KIMI_MODEL_NAME` defines an env-only model alias (`packages/agent-core-v2/src/app/kosongConfig/envOverlay.ts:87-140`).

**Permissions (the main hazard).** Print mode forces permission mode `auto` ("Never Ask") on new, resumed and continued sessions (`v2/run-v2-print.ts:433-438, 459, 480, 514`), and `--yolo`, `--auto`, `--plan` are rejected with `-p` (`options.ts:87-95`). In non-interactive runs the dangerous-command guard is not even installed (`packages/agent-core-v2/src/agent/permissionPolicy/permissionPolicyService.ts:39-55`), so Bash, Write, Edit, FetchURL etc. all run. The only thing evaluated before auto-approve is `user-configured-deny`: `[[permission.rules]]` with `decision = "deny"` from the user `config.toml` (docs/en/configuration/config-files.md:537-575, docs/en/reference/kimi-command.md:122). There is NO flag or env var for deny rules, so the only safe headless configuration is: point `KIMI_CODE_HOME` at a Hashi-managed dir whose `config.toml` has deny rules for `Bash`, `Write`, `Edit`, `FetchURL`, `Agent`, `AgentSwarm`, `CronCreate`, `TaskStop` (tool names: docs/en/reference/tools.md). That cannot be expressed in base_args/env alone, hence `auto_approves_tools_headless: true` and empty `pinned_args`. Project `.kimi-code/local.toml` rules and project MCP load only in a trusted workspace; leave `KIMI_CODE_TRUST_WORKSPACE` unset (`packages/agent-core-v2/src/workspace/workspaceTrust/trustEnv.ts`). The secret-file hard block for Read/Write/Edit stays on.

**Slash.** A prompt starting with `/goal` creates a goal and runs a goal loop whose status sets the exit code (`goal-prompt.ts:53-63`, `v2/run-v2-print.ts:302-313, 744-747`). Other slash text is sent as plain user text (UNVERIFIED whether skills expand `/skill:<name>`), so `slash_commands_in_prompt` is set true to be safe.

**Env.** `KIMI_CODE_HOME` relocates config, OAuth tokens, sessions and logs (default `~/.kimi-code`, `packages/agent-core-v2/src/app/bootstrap/bootstrap.ts:173`, `packages/oauth/src/toolkit.ts:401`). `KIMI_CODE_NO_AUTO_UPDATE=1` disables update check/background install (`update/preflight.ts:540-551`). `KIMI_DISABLE_TELEMETRY=1` (`packages/telemetry/src/bootstrap.ts:7`). `KIMI_DISABLE_CRON=1` disables the cron tool (`packages/agent-core-v2/src/features/cron/configSection.ts:41`). Credentials: OAuth via `kimi login`, or env-only `KIMI_MODEL_NAME` + `KIMI_MODEL_API_KEY` + `KIMI_MODEL_BASE_URL` + `KIMI_MODEL_PROVIDER_TYPE` (`packages/agent-core-v2/src/app/kosongConfig/configSection.ts:74-80`). The old notes' `KIMI_MODEL_*` names are confirmed. `KIMI_API_KEY`/`KIMI_BASE_URL` belong to the built-in Kimi provider endpoint (`packages/core/src/llm/builtin/provider/endpoints.ts:33-40`); whether v2 reads them is UNVERIFIED.

**Rate limits.** HTTP 429 maps to error code `provider.rate_limit` (`packages/agent-core-v2/src/llm-adapter/contract/errors.ts:28, 58-63, 368-370`); message patterns it recognises include `too many requests`, `rate limit`, `rate-limited`, `reached ... max rpm` (errors.ts:346-354). `APIProviderQuotaExhaustedError` also uses 429 (errors.ts:225-244). Retries are visible as `turn.step.retrying` meta lines; `KIMI_CODE_INFINITE_RETRY` exists, do not set it.

**Exit codes.** 0 ok, 1 any error, 4 session occupied, goal runs map goal status to a code. Print mode does not call `process.exit` on success; a fallback timer force-exits a wedged loop (`headless-exit.ts`). Windows needs Git Bash (README).

## Success output (derived from source or docs, not recorded)

```
{"role":"meta","type":"system.version","version":"2.6.2"}
{"role":"assistant","tool_calls":[{"type":"function","id":"tool_0a1b2c","function":{"name":"Read","arguments":"{\"path\":\"src/lib.rs\"}"}}]}
{"role":"tool","tool_call_id":"tool_0a1b2c","content":"pub fn add(a: i32, b: i32) -> i32 { a + b }"}
{"role":"assistant","content":"`add` returns the sum of its two arguments."}
{"role":"meta","type":"session.resume_hint","session_id":"session_0b8e6c2a-3f41-4d9a-a7c5-e21f9b04d6c3","command":"kimi -r session_0b8e6c2a-3f41-4d9a-a7c5-e21f9b04d6c3","content":"To resume this session: kimi -r session_0b8e6c2a-3f41-4d9a-a7c5-e21f9b04d6c3"}
```

## Error output

```
{"role":"meta","type":"system.version","version":"2.6.2"}
{"role":"meta","type":"turn.step.retrying","failed_attempt":1,"next_attempt":2,"max_attempts":3,"delay_ms":2000,"error_name":"APIProviderRateLimitError","error_message":"429 Too Many Requests","status_code":429}
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. KIMI_CODE_HOME (default ~/.kimi-code) is the home for tokens, config, region marker and device_id (packages/oauth/src/toolkit.ts:400-404, region.ts:172-177, identity.ts:42-62). Tokens are files: FileTokenStorage under <home>/credentials/<name>.json, mode 0600 (storage.ts:1-13, toolkit.ts:112-113); managed refs default to storage 'file' (managed-kimi-code.ts:288-292). Login exits 0 with `Logged in to <provider>.` on stderr (apps/kimi-code/src/cli/sub/login-flow.ts:69-81) and provisions config.toml (packages/node-sdk/src/auth.ts:102-119).
- **API-key accounts:** no. KIMI_MODEL_API_KEY only fills the env provider (configSection.ts:74-80); the env model becomes the default model only when KIMI_MODEL_NAME is also set (kosongConfig/envOverlay.ts:89-172 sets defaultModel to the env alias), and node-sdk fails if KIMI_MODEL_NAME is set without the key (node-sdk/src/config/env-model.ts:79-81). With the key alone the stored OAuth login's default_model still wins. Works when KIMI_MODEL_NAME (and KIMI_MODEL_BASE_URL / KIMI_MODEL_PROVIDER_TYPE for non-default endpoints) is set too.
- **Status check:** {"args": null, "signed_in_when": null}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** yes. The login is one plain JSON file {access_token, refresh_token, expires_at (number), scope, token_type, expires_in} (packages/oauth/src/types.ts:63-90) at credentials/kimi-code.json for the default slot (KIMI_CODE_OAUTH_KEY 'oauth/kimi-code' maps to file name 'kimi-code', managed-kimi-code.ts:13, toolkit.ts:384-398). Caveats: the owner's ~/.kimi-code/config.toml must already hold the managed kimi-code provider and default model (from their own `kimi login`), both accounts must be in the same region (global or scoped logins use a different key 'oauth/kimi-code-env-...', managed-kimi-code.ts:14, region.ts:252-291), and whether the server binds refresh tokens to <home>/device_id is unverified.
- **Live quota:** yes. packages/oauth/src/managed-usage.ts:5-23 (URL), 60-130 (schema and parse), 218-252 (fetch with Bearer); toolkit.ts:279, 405-409. A /me endpoint on the same base returns the profile (managed-userinfo.ts:1-47).
- **Usage in output:** none
- **Limit messages:** Matches: 429 maps to code provider.rate_limit and the recognised message patterns are too many requests, provider.rate_limit, rate limit(ed), rate-limited, reached ... max rpm (packages/agent-core-v2/src/llm-adapter/contract/errors.ts:28, 346-370). Missing: plan quota exhaustion is APIProviderQuotaExhaustedError (errors.ts:225-244) raised for server codes exceeded_current_quota_error / insufficient_quota (packages/core/src/llm/builtin/provider/kimi/errors.ts:7, protocol/openai/decode.ts:374, 434); add "exceeded_current_quota" and "insufficient_quota". The failure text reaches stderr as `error: failed to run prompt: <code>: <message>`.

Source: fresh clone of MoonshotAI/kimi-code at 334d798 (2.6.2) in the scratchpad; paths relative to it.

- No auth status subcommand exists: the subcommands are login, provider, doctor (validates config.toml / tui.toml only), session, export, fork, acp, app, vis, upgrade (apps/kimi-code/src/cli/sub/*.ts). A check would have to call GET <base>/me or /usages with the stored access token.
- Identity is not stored locally; GET <base>/me returns user_id, nickname, email (managed-userinfo.ts:4-47). Whether the access token is a JWT with an email claim is unverified (the only JWT decode in the tree reads sub or user_id, agent-core-v2/src/agent/media/mediaResolverService.ts:634-648).
- Refresh rotates refresh_token under a lock and re-reads storage first (oauth-manager.ts:193, 311-371), so a switched-in file stays valid as long as the copy is the freshest; expires_at is a number, so the expiry pointer compares correctly.
- No usage or cost is printed in stream-json (spec has none), confirmed again in prompt-render.ts and v2/run-v2-print.ts.
