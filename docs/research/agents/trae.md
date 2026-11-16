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

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** no. The TRAE login (tenant/trae/cli/auth AuthService) is saved through tokenstore, which has only a KeyringStore (zalando/go-keyring: macOS Keychain, Secret Service/gnome-keyring/kwallet on Linux, wincred) and an in-memory fallback; there is no file store and no env var to force one. The embedded FAQ says plainly that without a keyring the SSO token cannot persist across processes and there is no token-file workaround. HOME isolation only separates config/sessions (the FAQ suggests it for shared machines), not the keyring entry, so a second login on the same OS user overwrites the first.
- **API-key accounts:** no. No env var selects a TRAE account. The built-in models go through the logged-in TRAE gateway (/trae-cli/api/v1/llm/proxy). A personal access token exchange exists ("successfully logged in with personal access token", "failed to exchange personal access token, falling back to normal login flow") but how the PAT is supplied (flag, env or config) was not found in strings and is undocumented. Custom models take api_key/base_url from config (`models[].api_key: ${VAR}`), which picks a provider, not a TRAE account, and the quota/commercialisation middlewares still require the TRAE login.
- **Status check:** {"args": null, "signed_in_when": null}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** no. Login state is in the OS keyring, not in a file under ~/.trae or ~/.coco, so there is nothing to copy in and out of a live folder.
- **Live quota:** no. Quota is enforced server side per request: CommercializationPreCheckMiddleware/CheckCustomModelQuotaMiddleware call /api/ide/v1/check_custom_model_quota (custom models only) and usage is reported to /api/ide/v1/report_custom_model_token_usage, both with the keyring OAuth token. No command or documented endpoint reports remaining personal quota; the /model picker's `Quota: 100% used` is company-wide load, not personal quota (embedded FAQ).
- **Usage in output:** tokens and cost
- **Limit messages:** Spec has ["rate limit", "rate limit exceeded"], which catches the generic 429 text but misses the TRAE enterprise quota messages. Add: "quota limit" ("Model usage has reached personal quota limit...", "...enterprise quota limit...", "Usage for this model has reached your personal quota limit"), "quota exceeded" ("personal quota exceeded", "enterprise quota exceeded", "model quota exceeded"), "usage has reached limit" ("AI usage has reached limit. Please contact the administrator to activate pay-as-you-go billing."), "too many requests" (FAQ: "Status: 429 Too Many Requests / Your queue position is N"). Optional: "subscription expired" ("Enterprise subscription expired. Please contact your administrator.", "account subscription expired") is an account state, not a window limit.

Source: closed Go binary trae-cli 0.121.3 linux_amd64 (scratchpad/pkgs/trae/x/trae-cli), ASCII strings in scratchpad/pkgs/trae/strings.txt (line numbers below) plus its embedded Chinese docs decoded as UTF-8 from the binary. Not run.

**Login store.** Auth lives in package code.byted.org/nextcode/coco/tenant/trae/cli/auth: AuthService.{CheckLogin, GetUserInfo, GetAuthInfo, LoginWithBaseURL, LoginWithToken, Logout, loadAuthInfoFromStorage, saveAuthInfoToStorage, refreshAndSave}, OauthToken.{TokenExpireIn, RefreshExpired}, UserInfo.{HasFeature, IsEnterprisePrivateDeployment} (strings.txt:226249-226309). Storage is code.byted.org/nextcode/coco/cli/util/tokenstore with only KeyringStore and MemoryStore (strings.txt:214490-214512) over github.com/zalando/go-keyring (strings.txt:40556, 147893). Messages: "using keyring token store" (88960), "keyring support check timed out, considering keyring unsupported" (89453), "failed to create token store" / "attempting token-based login" / "failed to save auth info" (89034), "login session has expired, please reauthenticate" (89349). Embedded FAQ "keyring 不可用 / 反复 SSO" (strings.txt:145844-145859): with no keyring TraeCLI falls back to memory, the token is lost on exit, and there is no token-file workaround; for multi-user machines it advises a per-user HOME (config/sessions only).

**Endpoints** (enterprise base DefaultEnterpriseBaseURL, e.g. console.enterprise.trae.cn): /cloudide/api/v3/trae/CheckLogin (89115), /cloudide/api/v3/trae/GetUserInfo (89131), /cloudide/api/v3/trae/oauth/ExchangeToken, /api/ide/v1/tenant/get_tenant_user_config, /api/ide/v1/check_custom_model_quota (89170), /api/ide/v1/report_custom_model_token_usage. PAT login exists (89349, 89482) but its input is not documented.

**Status/identity.** No auth status or whoami command. `traecli doctor --json` checks version, os, binary, config, ripgrep, update, model, mcp (strings.txt:96899-96967), no login check; `traecli models --json` (132059-132081) may fail when not logged in but is not a defined login probe (UNVERIFIED). User info is fetched from GetUserInfo at runtime and kept in the keyring auth blob, not in a readable file.

**Output.** Embedded non-interactive doc: stream-json last line `result` with usage{input_tokens,output_tokens} and total_cost_usd, always written last; exit 0 success, 1 error with subtype error_during_execution and `error` (TotalCostUSD json:"total_cost_usd" type tag also present).

**Limits.** Quota/commercialisation messages at strings.txt:88808 (model quota exceeded), 88888 (personal quota exceeded), 88952 (enterprise quota exceeded), 89552-89591 (personal/enterprise quota limit texts), 89559 (AI usage has reached limit ... pay-as-you-go), 89464 (Enterprise subscription expired), 89034 (account subscription expired), 88785/88801 (rate limit exceeded), FAQ 429 section (145905: Status: 429 Too Many Requests, queue position).
