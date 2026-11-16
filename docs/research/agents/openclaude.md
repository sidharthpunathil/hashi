# OpenClaude (`openclaude`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `OPENCLAUDE` in `src/agent/specs.rs`.

- **Source:** github:Gitlawb/openclaude
- **License:** Claude Code derived (Anthropic proprietary) with contributor modifications under MIT
- **Version checked:** 0.36.0
- **Install:** npm install -g @twigpine/openclaude@latest
- **Headless call:** `openclaude -p --output-format json`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

Source read: `github:Gitlawb/openclaude` (repo now branded Twigpine; package.json name `@twigpine/openclaude` 0.36.0, bin `openclaude`). It is a fork of the leaked Claude Code source (LICENSE: "derived from Anthropic's Claude Code CLI", Anthropic copyright retained, contributor changes MIT "where legally permissible"). Not installed here, nothing executed: every claim below is from source.

**Headless:** identical to Claude Code. `-p/--print` (src/main.tsx:901), positional `[prompt]` (main.tsx:898), `--output-format text|json|stream-json` (main.tsx:912). `json` writes the last `result` message as one line (src/cli/print.ts:1021-1030); `stream-json` requires `--verbose` (print.ts:886-888). Exit code is 1 when the result has `is_error`, else 0 (print.ts:1077-1079). So Hashi's `claude_result` parser applies unchanged.

**Result shape** (src/QueryEngine.ts:1254-1274 success, 1201-1234 error_during_execution): `{type:"result", subtype:"success"|"error_during_execution"|"error_max_turns"|"error_max_budget_usd"|..., is_error, duration_ms, duration_api_ms, num_turns, result, stop_reason, session_id, total_cost_usd, usage, modelUsage, permission_denials, structured_output?, fast_mode_state, uuid, errors?}`. Note `is_error` can be true with subtype `success` when the last assistant message is an API error message (QueryEngine.ts:1248-1252), as in Claude Code. `error_during_execution` carries no `result`, only `errors[]`. Load errors in `json`/`text` mode go to **stderr** as plain text; only `stream-json` gets a JSON error result (print.ts:5068-5097). `total_cost_usd` for non-Anthropic providers depends on OpenClaude's pricing tables (custom pricing exists, cost-tracker.ts); UNVERIFIED whether it is 0 for unknown models.

**Prompt passing:** commander with `enablePositionalOptions()` (main.tsx:839); `--` is standard commander behaviour and the pre-commander argv scanners stop at `--` (src/utils/envFile.ts:306-308 for `--provider-env-file`). `src/entrypoints/cli.tsx:827` does `args.includes('--bare')` over the whole argv, so a prompt that is literally `--bare` after `--` would switch on bare mode (harmless but noted). A prompt starting with `/` runs a slash command/skill, as in Claude Code; `--disable-slash-commands` exists (main.tsx:948).

**Model / resume:** `--model <m>` (main.tsx:935), plus `--provider anthropic|openai|gemini|github|bedrock|vertex|ollama`. `-r/--resume [id]` (main.tsx:928; bare `-r` opens a picker, so always pass an id), `--session-id <uuid>` pre-assigns an id (main.tsx:942), `-c/--continue`, `--fork-session`, `--no-session-persistence`.

**Permissions:** modes `acceptEdits, bypassPermissions, default, dontAsk, fullAccess, plan` plus `auto` behind a feature flag (src/types/permissions.ts:16-37). `fullAccess` is an OpenClaude addition and must be treated as unsafe. `--dangerously-skip-permissions` gained the alias `--yolo` (main.tsx:912). In `-p` without a permission mode, asks are denied and reported in `permission_denials` (inherited Claude Code behaviour; the deny path itself was not re-read here, UNVERIFIED). `-p` skips the workspace trust dialog (main.tsx:901 help text). Pinned args mirror Hashi's Claude pins: `--setting-sources user`, `--strict-mcp-config`, `--disallowedTools WebFetch` (variadic flag: must be followed by another flag or `--`).

**Config dir:** `OPENCLAUDE_CONFIG_DIR` relocates the whole config home (default `~/.openclaude`) and the global config file; `CLAUDE_CONFIG_DIR` is deliberately ignored (src/utils/envUtils.ts:5-20, 36-41, 70-82; src/utils/env.ts:31-51). So OpenClaude never shares Claude Code's login.

**Provider selection hazards (important):**
1. With no saved provider profile, no `CLAUDE_CODE_USE_*` selection and no `CLAUDE_CODE_USE_OPENAI=0` opt-out, startup **defaults to the vendor's own gateway** `https://api.twigpine.com/v1` with model `auto` (src/utils/providerProfile.ts:2869-2906, src/utils/providerAutoDetect.ts:329-330). Hashi should always set an explicit provider (`CLAUDE_CODE_USE_OPENAI=1` + `OPENAI_BASE_URL` + `OPENAI_API_KEY` + `OPENAI_MODEL`, or `CLAUDE_CODE_USE_OPENAI=0` for plain Anthropic). Whether an `ANTHROPIC_API_KEY`-only env counts as a "concrete provider selection" (`hasConcreteProviderSelection`, providerProfile.ts:2854) is UNVERIFIED.
2. The legacy single-profile file `.openclaude-profile.json` is read from the config dir and, **if absent there, from the process cwd** (providerProfile.ts:318-349, loaded at startup via `applyStartupEnvFromProfile`, providerProfile.ts:2952-2966, called from cli.tsx:494). A per-token workspace containing that file can inject provider env such as `OPENAI_BASE_URL`, which could send the operator's key to an attacker endpoint. Mitigation: set an explicit `CLAUDE_CODE_USE_*` provider (explicit selection short-circuits the legacy profile, providerProfile.ts:2852-2864) or create an empty-but-valid profile in `OPENCLAUDE_CONFIG_DIR`.
3. `--provider-env-file <path>` loads env from a file (envFile.ts:297-330); strip it from operator extra args.

**Process model:** `bin/openclaude` re-spawns itself with `spawnSync(process.execPath, [--max-old-space-size=..., --expose-gc, ...])` (bin/openclaude:251-320), so Hashi's `kill_on_drop` would SIGKILL only the parent and orphan the child. Set `OPENCLAUDE_DISABLE_HEAP_RELAUNCH=1` (bin/openclaude:230, 252-255) to keep a single process.

**Env:** `DISABLE_AUTOUPDATER=1` (utils/config.ts:2170), `DISABLE_TELEMETRY=1` and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` (utils/privacyLevel.ts:14-24). Retries: `OPENCLAUDE_MAX_RETRIES`.

**Auth:** env vars per provider (the OpenAI-compatible shim reads `OPENAI_API_KEY`, `OPENAI_API_KEYS` pool, `OPENAI_BASE_URL`/`OPENAI_API_BASE`, `OPENAI_MODEL`; also `GEMINI_API_KEY`, `GITHUB_TOKEN`/`GH_TOKEN`, `MISTRAL_API_KEY`, `XAI_API_KEY`, `OPENROUTER_API_KEY`, `DEEPSEEK_API_KEY`, `CODEX_API_KEY`, `OPENGATEWAY_API_KEY`, `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN`), saved profiles via the interactive `/provider` command, and `openclaude auth login` for an Anthropic account (main.tsx:3956). Keys saved through `/provider` live in plaintext JSON under the config dir (per upstream README; UNVERIFIED in source).

**Rate limits:** Claude-style prefixes "You've hit your", "You've used", "You're out of extra usage" (src/services/rateLimitMessages.ts:21-27); shim errors "API Error: Provider rate limit reached" and "API Error: Provider quota or usage allotment has run out" (src/services/api/errors.ts:125-131), and "API Error: Request rejected (429)" (errors.ts:948).

**Fixtures:** derived from QueryEngine.ts:1254-1274 and print.ts, not from a real run. Install: `npm install -g @twigpine/openclaude@latest` (README.md:149; the old `@gitlawb/openclaude` package is superseded, README.md:152-159), Node >= 24.22.1.

## Success output (derived from source or docs, not recorded)

```
{"type":"result","subtype":"success","is_error":false,"duration_ms":2310,"duration_api_ms":2150,"num_turns":1,"result":"OK","stop_reason":"end_turn","session_id":"6f2c1d9a-4b7e-4c1a-9e3f-2a8b7c6d5e41","total_cost_usd":0.00042,"usage":{"input_tokens":1840,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":2},"modelUsage":{},"permission_denials":[],"fast_mode_state":"off","uuid":"0d1e2f3a-5b6c-4d7e-8f90-a1b2c3d4e5f6"}
```

## Error output

```
{"type":"result","subtype":"error_during_execution","duration_ms":410,"duration_api_ms":0,"is_error":true,"num_turns":0,"stop_reason":null,"session_id":"6f2c1d9a-4b7e-4c1a-9e3f-2a8b7c6d5e41","total_cost_usd":0,"usage":{"input_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0},"modelUsage":{},"permission_denials":[],"fast_mode_state":"off","uuid":"1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d","errors":["[ede_diagnostic] result_type=assistant last_content_type=text stop_reason=null","API Error: Provider rate limit reached. Retry in a few seconds."]}
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** yes. OPENCLAUDE_CONFIG_DIR moves the config home, the global .openclaude.json and the profile file (src/utils/envUtils.ts:30-41,70-82; src/utils/env.ts:31-52). The OAuth token goes to the OS vault first (macOS Keychain, libsecret via secret-tool on Linux, Windows Credential Locker, falling back to <dir>/.credentials.json: src/utils/secureStorage/index.ts:69-93), but the vault service name gets a sha256 suffix of the config dir whenever it is not the default (src/utils/secureStorage/macOsKeychainHelpers.ts:37-56), so each account dir gets its own vault entry and dir_var alone still isolates logins. No env var forces plain-file storage.
- **API-key accounts:** no. OPENAI_API_KEY alone is not a concrete provider selection. hasConcreteProviderSelection needs CLAUDE_CODE_USE_OPENAI=1 plus OPENAI_BASE_URL/OPENAI_API_BASE or OPENAI_MODEL (src/utils/providerProfile.ts:1530-1545). Without it, a saved profile in the owner's config dir wins (buildLaunchEnv with persisted, providerProfile.ts:2914-2925), or with no profile the run is pointed at the vendor gateway https://api.twigpine.com/v1 with model auto and the key is sent there (providerProfile.ts:2870-2906). A key account needs CLAUDE_CODE_USE_OPENAI=1 and OPENAI_BASE_URL (and usually OPENAI_MODEL) set alongside the key, or ANTHROPIC_API_KEY with CLAUDE_CODE_USE_OPENAI=0.
- **Status check:** {"args": ["auth", "status", "--json"], "signed_in_when": "json pointer /loggedIn true"}
- **Identity:** {"file": ".openclaude.json", "pointer": "/oauthAccount/emailAddress", "jwt_claim": null}
- **Switch:** no. On macOS (Keychain), Linux with secret-tool (libsecret) and Windows (Credential Locker) the Anthropic OAuth token is not in a file: it is in the vault entry 'OpenClaude-credentials' (plus an 8-hex config-dir hash for non-default dirs), and .credentials.json is only the fallback (src/utils/secureStorage/index.ts:69-93, fallbackStorage.ts:13-26, macOsKeychainHelpers.ts:27-56). The identity file also sits in a different place for the plain command (~/.openclaude.json in the home dir) than for an account (<dir>/.openclaude.json): src/utils/env.ts:18-52. A plain-file switch would therefore copy nothing useful on a desktop; it needs Claude-style vault handling. Non-Anthropic provider logins (/provider profiles) are plain JSON in the config dir and would switch fine as files, but which file holds them was not pinned down.
- **Live quota:** yes. Works only for Anthropic OAuth accounts. Per-provider helpers exist for codex, clinepass and minimax (src/services/api/codexUsage.ts, clinepassUsage.ts, minimaxUsage.ts) but were not read.
- **Usage in output:** tokens and cost
- **Limit messages:** Spec strings match src/services/rateLimitMessages.ts:21-27 and src/services/api/errors.ts:125-131,948. Missing: OpenCode Go quota texts 'OpenCode Go free usage exhausted' and 'OpenCode Go subscription limit reached' (errors.ts:462-465); suggest adding 'free usage exhausted' and 'subscription limit reached'. RATE_LIMIT_ERROR_PREFIXES also has "You've used" and "You're close to", but those are warnings, not failures, so leave them out.

Source: github:Gitlawb/openclaude 0.36.0 (opensrc cache). Usage: claude_result has usage.input_tokens/output_tokens and total_cost_usd (src/QueryEngine.ts:1254-1274). Identity: the email is only written for an Anthropic claude.ai login (oauthAccount in the global config, src/cli/handlers/auth.ts:306-311); for third-party providers there is no identity. Global config name gets an OAuth suffix for staging/local builds (src/constants/oauth.ts:101) and a legacy <configdir>/.config.json is preferred when present (src/utils/env.ts:31-39). auth status exits 0 when logged in, 1 otherwise (auth.ts:318).
