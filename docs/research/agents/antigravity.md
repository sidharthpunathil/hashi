# Google Antigravity CLI (`antigravity`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **ran**. The spec Hashi runs is `ANTIGRAVITY` in `src/agent/specs.rs`.

- **Source:** closed
- **License:** proprietary (Google)
- **Version checked:** 1.5.9, ran here
- **Install:** UNVERIFIED: see https://antigravity.google/docs/install (self-updates with agy update)
- **Headless call:** `agy --output-format json`, prompt flag eq `--print=`
- **Runs tools without asking when headless:** no

## Notes

**What it is.** `agy` is Google Antigravity's terminal CLI (the CLI sibling of the Antigravity desktop IDE), installed at `~/.local/bin/agy` as a 223 MB Go binary (Mach-O arm64), version 1.5.9. Closed source: no public repo; facts below come from `agy --help`, `agy changelog` (release notes embedded in the binary; saved at `scratchpad/agy_changelog.txt`, line numbers cited as `changelog:N`), strings in the binary, the official docs at https://antigravity.google/docs/cli/reference and https://antigravity.google/docs/cli/headless, and ONE real run.

**Real run (recorded).** In a fresh `mktemp -d` (not a trusted workspace; `~/.gemini/antigravity-cli/settings.json` trusts only `$HOME`), stdin `/dev/null`, `NO_COLOR=1 TERM=dumb`, `timeout 120`:
`agy --print='Reply with exactly: OK' --output-format json --settings='{"agentMode":""}'` exited 0 in about 6 s with empty stderr and the single-line JSON in `fixture_success`. So `--print=<prompt>` (flag_eq) works, an untrusted temp dir does not block print mode, and no TTY is needed. The run used the cached Google sign-in.

**Headless.** `-p/--print/--prompt` "Run a single prompt non-interactively and print the response" (help). The flag takes the prompt as its value; `--print=<prompt>` keeps a prompt starting with `-` safe. Whether `--` is honoured is UNVERIFIED (Go-style parser; not tested). `-p` combined with `-i/--prompt-interactive` exits 2 (changelog:365).

**Output `--output-format json`** (docs/cli/headless): one object `{conversation_id, status, response, error?, duration_seconds, num_turns, structured_output?, json_schema?, usage{input_tokens, output_tokens, thinking_tokens, cache_read_tokens, total_tokens}}` plus `finished_reason` (`print_timeout`|`interrupted`|`error`, omitted on normal finish, changelog:183) and `denied_actions` (refused tool actions, changelog:658). `status` is one of `SUCCESS`, `ERROR`, `CANCELED`, `INTERRUPTED`, `INVALID`, `WAITING`, `RUNNING`; `error` is "present only on failure". The parser should treat a non-empty `/error` OR `status != "SUCCESS"` as failure (Hashi's `error_ptr` covers the first; checking `status` needs a small extra rule). No cost field. `thinking_tokens` is separate from `output_tokens` (29 output, 28 thinking in the real run), so Hashi may want to add it to output. `stream-json` events: `init`, `step_update` (`step_type` `user_input|agent_response|tool|checkpoint`, `state` `ACTIVE|DONE`, `tool_info{name,parameters,output,error{type,message}}`), terminal `result` with the same shape as `json` (changelog:931-934, docs/cli/headless). `json` is recommended: one line, carries everything Hashi needs.

**Errors and exit codes.** 0 success; a run that produces no response exits non-zero; unknown `--model` exits 1 with an `ERROR` envelope (docs/cli/headless); model/agent API failures exit **3** and print `AGY_ERROR: {...}` on **stderr** with canonical status, HTTP/gRPC code, retryability and error id (changelog:557, 505; binary string `AGY_ERROR: {"short_error":%q}`); `--print-timeout` expiry exits 3 (changelog:167); print-mode usage errors exit 2 (changelog:365). Benign tool errors and permission denials do NOT fail the run (changelog:742). Unauthenticated: "a run that isn't already authenticated exits with an `authentication required` error" (docs/cli/headless), i.e. it fails fast rather than opening a browser (not exercised here).

**Resume.** `--conversation <id>` resumes by the `conversation_id` from a previous run; `-c/--continue` resumes the latest; runs are otherwise stateless (docs/cli/headless). An unknown id reports `conversation <id> not found` (changelog:8 describes this for `/resume`; for `--conversation` UNVERIFIED). Ids cannot be pre-assigned. Whether `--conversation <id>` (space separated) parses like `--conversation=<id>` is UNVERIFIED; Go flag parsers accept both for value flags.

**Model.** `--model <name>` (`agy models --output-format json` lists them, changelog:851). Available models depend on the auth route (Google sign-in, `GEMINI_API_KEY`, ADC, enterprise gateway; changelog:23).

**Permissions headless.** Since 1.5.7 the default Agent Mode is **Auto** wherever available, "including headless `-p` runs" (changelog:52). Auto uses a model-based reviewer that can approve commands leaving the terminal sandbox. To get the conservative Default mode, pass `--settings={"agentMode":""}` (the value `/config` saves for Default, changelog:52); this was used in the real run. In Default mode `toolPermission` defaults to `request-review`, which asks for write, bash, web and MCP tools unless an allow rule covers them (docs/cli/reference); in print mode such asks are **soft-denied**: the run continues, exits 0, prints a notice on stderr and lists them in `denied_actions` (docs/cli/headless, changelog:658). Plan approval proceeds automatically headless (changelog:646) and questions are settled by the agent itself (changelog:859). `--dangerously-skip-permissions` auto-approves everything. `--mode accept-edits|auto|plan` overrides the mode. Headless runs honour the persisted `~/.gemini/antigravity-cli/settings.json` permissions (changelog:983), so an owner's allow rules apply to Hashi runs too. `--sandbox` adds OS terminal restrictions. Reading files is allowed without asking; whether reads outside the workspace ask is UNVERIFIED.

**Slash commands.** Print mode expands slash commands and skills (`-p "/my-skill ..."`, changelog:917) and answers read-only ones like `/usage`, `/quota`, `/model` without a turn (changelog:880, 850). `--disable-slash-commands` turns this off and is pinned; with it, `slash_commands_in_prompt` is false.

**Background tasks.** Headless `-p` waits for background tasks with no limit unless `--print-timeout` is set (changelog:167, 556) and leaves daemon background processes (dev servers) running after exit (changelog:634; but changelog:516 says they terminate). With tools denied this mostly cannot happen; Hashi's own timeout bounds it anyway. Hashi could also pass `--print-timeout=<n>s` slightly below its timeout to get a clean exit 3 with partial JSON.

**Config and credentials.** State lives in `~/.gemini/antigravity-cli/` (settings.json, conversations, logs) and the macOS keychain (sign-in token; file fallback after keychain failure, changelog 1.5.9). No env var relocating it was found in the binary strings (`ANTIGRAVITY_*` vars are for the IDE sidecar), so `config_dir_env` is null; overriding `HOME` might work but the keychain entry would still be shared (UNVERIFIED). Credential routes: Google account sign-in (default), `GEMINI_API_KEY` with `"modelProvider":"gemini"` (docs/cli/reference), Application Default Credentials via `AGY_ADC_AUTH` (+ `GOOGLE_APPLICATION_CREDENTIALS`, `GOOGLE_CLOUD_PROJECT`), and an enterprise LLM gateway (`AGY_LLM_GATEWAY_API_KEY`, `AGY_LLM_GATEWAY_HEADERS`, `AGY_LLM_GATEWAY_MODELS`, `AGY_LLM_GATEWAY_CA_CERT`). Env names come from binary strings; exact semantics UNVERIFIED.

**Rate limits.** Binary strings include `RESOURCE_EXHAUSTED`, `Quota exhausted`, `AI: Out of credits`, `ERROR_FAILED_RATE_LIMIT_SYSTEM`, `X-RateLimit-Reset`; `/quota` and `/credits` exist. Exact headless error text UNVERIFIED; failures arrive in `/error` and in the stderr `AGY_ERROR` line.

**Install.** Official install command not confirmed (docs page /docs/install was not fetched); the CLI self-updates (`agy update`, updater state under `~/.gemini/antigravity-cli/updater`). Disabling auto-update headlessly: UNVERIFIED (no env found).

`fixture_success` is the exact stdout of the real run. `fixture_error` is DERIVED from the documented envelope (status ERROR with `error`, docs/cli/headless) and its exact field set and message text are UNVERIFIED.

## Success output (recorded)

```
{"conversation_id":"7aa3e9f0-50ad-4bfa-a452-fdce3bee698a","status":"SUCCESS","response":"OK\n","duration_seconds":6.100747,"num_turns":1,"usage":{"input_tokens":11568,"output_tokens":29,"thinking_tokens":28,"cache_read_tokens":0,"total_tokens":11597}}
```

## Error output

```
{"conversation_id":"","status":"ERROR","response":"","error":"unknown model \"no-such-model\"","duration_seconds":0.41,"num_turns":0}
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** no. State lives in ~/.gemini/antigravity-cli (verified names here: settings.json, conversations, brain, cache, log, updater, installation_id, ...; no credential file), the path is built from the home dir (binary string "/.gemini/antigravity-cli/settings.json") and no env var relocates it: the AGY_*/ANTIGRAVITY_* names in the binary are UI, ADC, gateway and IDE-sidecar switches. The sign-in token is in the system keychain (changelog:15, 105, 253), shared by any HOME override. AGY_ACCOUNT selects among accounts connected in the Plugins Manager for agent tool calls, not the CLI login.
- **API-key accounts:** no. The auth route is chosen by settings, not by the env var: GEMINI_API_KEY needs "modelProvider": "gemini" in settings.json (changelog:826, 10), and an auth method without its credential falls back to the saved login (changelog:9). Hashi could pass modelProvider through its --settings JSON together with the key, unverified.
- **Status check:** {"args": ["--print=/usage", "--output-format", "json"], "signed_in_when": "json pointer /status SUCCESS"}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** no. The login is a keychain item, not a file in the folder, so there is nothing to copy; and Hashi has no separate login folder for it.
- **Live quota:** yes. Print mode answers read-only slash commands without a turn, quota or a saved conversation (changelog:880, 452). Ran here with the cached sign-in: exit 0, /status SUCCESS, /num_turns 0, /usage/total_tokens 0, /command/name "usage", two groups ("Gemini Models", "Claude and GPT models") with weekly buckets carrying remaining_fraction and reset_time.
- **Usage in output:** tokens
- **Limit messages:** Binary/changelog texts: RESOURCE_EXHAUSTED, Quota exhausted, AI: Out of credits, ERROR_FAILED_RATE_LIMIT_SYSTEM; plan quota used up with credits unable to cover (changelog:456) and Gemini API daily quota or spend cap (changelog:484). The spec covers these; exact headless wording still unverified.

Closed Go binary ~/.local/bin/agy 1.5.9; read `agy --help`, the embedded changelog (scratchpad/agy_changelog.txt, cited changelog:N) and binary strings, and ran `agy -p /usage --output-format json` once in a temp dir (only key names and non-secret status fields printed).

- Subcommands: acp, agent(s), changelog, install, mcp, mic-serve, models, plugin(s), remote-control, update; no login, logout or status (sign-in and /logout are TUI only).
- An unauthenticated print run exits with an `authentication required` error (docs/cli/headless), so /status ERROR (or a non-zero exit) means signed out.
- Identity is read from the stored credential (changelog:633) in the keychain; nothing readable on disk.
- Usage: /usage has input, output, thinking, cache_read and total tokens; no cost.
