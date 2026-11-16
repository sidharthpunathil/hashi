# Qoder CLI CN (`qoder-cn`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `QODER_CN` in `src/agent/specs.rs`.

- **Source:** npm:@qodercn-ai/qoderclicn 1.1.89 (same bundle as npm:@qoder-ai/qodercli 1.1.89, built for the cn site; bundle read from the global package) + docs https://static.qoder.com.cn/qoder-cli-cn/docs/llms.txt
- **License:** Proprietary (Alibaba Qoder); package LICENSE file is Apache-2.0 text but no source is published
- **Version checked:** 1.1.89
- **Install:** curl -fsSL https://static.qoder.com.cn/qoder-cli-cn/install.sh | bash | npm install -g @qodercn-ai/qoderclicn
- **Headless call:** `qoderclicn -p --output-format json`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

**What it is.** **Qoder CLI CN**, the China-region build of Alibaba's Qoder CLI. It is the **same bundle** as `@qoder-ai/qodercli` with a build-time site constant switched to `cn`: the brand table in `bundle/qodercli.js` (offset ~1836) maps `cn` to `CLI_BRAND_CLI_BIN:"qoderclicn"`, `CLI_BRAND_ENV_PREFIX:"QODERCN_"`, user dir `.qoder-cn`, endpoints `gateway.qoder.com.cn` / `openapi.qoder.com.cn`, docs `docs.qoder.cn`, keychain service `qoderclicn-login` (offset ~32097617). The global build hard-codes `"cn"==(fUa="global")` (offset ~19838). npm `@qodercn-ai/qoderclicn` 1.1.89 ships bins `qoderclicn` (bundle) and `qodercn` (dispatcher); the native installer installs `qodercn` (https://docs.qoder.cn/cli/installation.md). Everything in the `qoder` spec applies, with the env prefix `QODERCN_` instead of `QODER_`; the CN tarball itself was not unpacked, so CN-specific details are inferred from the shared bundle (UNVERIFIED in detail). Accounts and PATs are separate from the global site.

**CN specifics.** PAT env `QODERCN_PERSONAL_ACCESS_TOKEN` (https://docs.qoder.cn/cli/authentication.md, lines 35, 79-98); config root `~/.qoder-cn`, overridden by `QODERCN_CONFIG_DIR`, then `QODERCN_CLI_HOME` + dir name, `QODERCN_CONFIG_DIR_NAME` (package README "配置目录环境变量"). `QODERCN_SERVICE_ACCOUNT_KEY` / `QODERCN_JOB_TOKEN` are inferred from the `yi("SERVICE_ACCOUNT_KEY")`/`yi("JOB_TOKEN")` prefixed env helpers (UNVERIFIED). Project resources still use `<cwd>/.qoder`.

**Shared behaviour (see qoder.json for full citations).** Claude Code clone (commander, minified/obfuscated). Headless `-p/--print`, `-o/--output-format text|json|stream-json`; `json` prints one Claude Code-style result object `{type:"result", subtype, is_error, result, session_id, usage{input_tokens,output_tokens,...}, total_cost_usd (always 0), total_credits, modelUsage, permission_denials, ...}`; error results carry `errors[]`, `error_message`, `error_code` and no `result`. `--` separates the prompt. Resume `-r/--resume <id>`, `--session-id`, `-c/--continue`. Model `-m/--model` (auto, lite, performance, BYOK key). Headless denies anything needing confirmation unless `--permission-mode accept_edits|bypass_permissions` or `--yolo`. Leading `/` runs slash commands. Native installs auto-update unless `general.enableAutoUpdate` is false in settings.

## Success output (derived from source or docs, not recorded)

```
{"type":"result","subtype":"success","duration_ms":4210,"duration_api_ms":3900,"is_error":false,"num_turns":1,"result":"Paris.","stop_reason":"end_turn","total_cost_usd":0,"total_credits":0.4,"usage":{"input_tokens":8120,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0},"modelUsage":{"auto":{"inputTokens":8120,"outputTokens":5,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"webSearchRequests":0,"costUSD":0,"contextWindow":0,"maxOutputTokens":0}},"permission_denials":[],"fast_mode_state":"off","uuid":"9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6b","session_id":"4b7e2f9a-1c3d-4e5f-8a9b-0c1d2e3f4a5b"}
```

## Error output

```
{"type":"result","subtype":"error_during_execution","duration_ms":320,"duration_api_ms":0,"is_error":true,"num_turns":0,"stop_reason":null,"total_cost_usd":0,"total_credits":0,"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0},"modelUsage":{},"permission_denials":[],"errors":["Quota exhausted for the current plan"],"error_message":"Quota exhausted for the current plan","error_code":"quota_exhausted","fast_mode_state":"off","uuid":"9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6c","session_id":"4b7e2f9a-1c3d-4e5f-8a9b-0c1d2e3f4a5b"}
```
