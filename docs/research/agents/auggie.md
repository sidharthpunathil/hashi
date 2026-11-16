# Augment Auggie (`auggie`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `AUGGIE` in `src/agent/specs.rs`.

- **Source:** npm:@augmentcode/auggie (bundled JS; closed source)
- **License:** Proprietary (SEE LICENSE IN LICENSE.md)
- **Version checked:** 0.37.1
- **Install:** npm install -g @augmentcode/auggie
- **Headless call:** `auggie --print --quiet --output-format json`, prompt flag eq `--instruction=`
- **Runs tools without asking when headless:** yes, so it is opt-in

## Notes

Source: npm `@augmentcode/auggie@0.37.1` ships a readable (minified) JS bundle, pkgs/_augmentcode_auggie/package/augment.mjs (single minified 13 MB bundle, cited by identifier); opensrc could not map it to a repo (closed source). Docs: https://docs.augmentcode.com/cli/v0/reference.md, /cli/v0/permissions.md, /cli/v0/automation/overview.md, /cli/v0/setup-auggie/authentication.md (0.x docs; the unversioned pages now describe the 1.x prerelease, e.g. `auggie auth login`, `--mode stream`). Not executed; fixtures derived from the serializer.

**Headless:** `-p, --print` (one-shot, mode `text`). `-q/--quiet` also selects non-TUI mode and sets log level `error`; pinned to keep progress lines (e.g. `Waiting for N MCP server(s)`) off stdout. Help text warns: "the indexing confirmation prompt is skipped in print mode. Only use --print from a workspace root that you want to index": the workspace (git root or cwd) is indexed and uploaded to Augment's context engine. Non-interactive mode "may be disabled if it is not included in your agreement (enterprise)" (automation docs).

**Prompt:** positional `[instruction]` or `-i, --instruction <text>` (`t4e.buildConfig`: `n.instruction ?? args[0]`). Use `--instruction=<prompt>` (flag_eq). Do NOT use `-- <prompt>`: commander dispatches the first operand to a subcommand when it matches one (`login`, `logout`, `session`, `mcp`, `tools`, `model`, `command`, `account`, ...), so a prompt equal to a subcommand name would run that command. stdin (non-TTY) is read with a 100 ms timeout (`X4()`) and appended to the instruction; /dev/null yields null, harmless. Docs: a prompt starting with `@path` inlines that file. Whether a leading `/` runs a custom command in print mode is UNVERIFIED (custom commands are exposed as `auggie command <name>` and as TUI slash commands); marked true to be safe.

**Output `--output-format json`** (only with `--print`): one compact line written by `HUe.onAgentLoopComplete`: `{type:"result", result, is_error, subtype, session_id, num_turns, request_id, billing?, retry_stats?}`. `subtype` is `success`, `error_during_execution` (set by `onError`), `error_max_turns`, or `empty_completion` (is_error false). `result` is the concatenated final assistant text (reset at each response start). **No token usage at all.** `billing {usage_unit:"credits"|"usd", total_cost, parent_cost, sub_agent_cost}` appears only with `--show-cost` AND the tenant feature flag `cliEnableShowCredits`; since the unit may be credits, cost_ptr is null. `num_turns` counts tool calls in the last turn, not model turns. `retry_stats.events[].retry_reason` is `rate_limited|unavailable|deadline_exceeded|unknown`. Whether the result line is still printed after a fatal agent error (vs only stderr) is UNVERIFIED; the error fixture assumes it is. Auth failure prints `\u274c No authentication available. Please run 'auggie login' or set AUGMENT_SESSION_AUTH...` to stderr and exits non-zero (stdout empty).

**Model:** `-m, --model <id>` (`auggie model list` / `models list`). **Resume:** `-r, --resume [sessionId]` takes an id or prefix (without an id it opens an interactive picker: always pass one). `session_id` in the JSON is the conversation id. `-c/--continue` resumes the latest session. Sessions live under `--augment-cache-dir` (default `~/.augment`). Resume from a different cwd: UNVERIFIED (session list is per workspace by default).

**Permissions (weak default):** in print mode there is no approval handler. `sq.callTool` only consults explicit `toolPermissions` rules (`cwe()` returns allow when no rule matches; `ask-user` without a handler becomes a denial with "User approval required"). The interactive "manual mode" gate (`checkToolCallSafe`) is TUI only. So by default `launch-process`, `save-file`, `str-replace-editor`, `remove-files`, `web-fetch` and MCP tools run without asking. Pinned args remove the mutating and shell tools with `--remove-tool` (CLI removals take precedence over settings) and hide the process tools with `--disable-process-tools` (hidden flag). Tool names come from the bundle's tool classes (`super("launch-process",2)`, `save-file`, `str-replace-editor`, `remove-files`, `apply_patch`, `edit`, `write`, `bash`, `setup-script`, `web-fetch`, `write-process`, `kill-process`). `--ask/-a` ("retrieval and non-editing tools only") looks prompt-level (AskModeProcessor), not an enforced gate (UNVERIFIED), so it is not relied on. Settings with `toolPermissions`, MCP servers, hooks, rules, skills and custom commands load from `/etc/augment/settings.json`, `~/.augment/settings.json` and the workspace `.augment/settings*.json` plus `.claude/skills|commands`; a deny anywhere wins (most-restrictive-wins), but a workspace file can add MCP servers. `--mcp-config` "Overwrites default mcp configuration in settings.json"; passing an empty config to disable MCP is UNVERIFIED.

**Env/config:** `AUGMENT_SESSION_AUTH` = session JSON `{accessToken, tenantURL, scopes}` from `auggie token print` (preferred over `~/.augment/session.json`; `--augment-session-json` flag equivalent). The CLI exports `AUGMENT_API_TOKEN`/`AUGMENT_API_URL` to its own children; they are read only by internal capability commands. `GITHUB_API_TOKEN` enables the GitHub tool. No env var relocates state: use the `--augment-cache-dir <path>` flag (default `~/.augment`; whether it also relocates `~/.augment/settings.json` is UNVERIFIED), so config_dir_env is null. `AUGMENT_DISABLE_AUTO_UPDATE=1` disables self-upgrade. Requires Node 20+ (package.json engines; README says 22).

**Limits:** `Rate limit reached.`, `Rate limited: too many requests. Wait a moment and retry, or reduce request concurrency.`, `\u274c Rate limit exceeded: <msg>` (TUI path); `--retry-timeout <sec>` bounds rate-limit retries. Credit exhaustion wording not found in the bundle (UNVERIFIED).

## Success output (derived from source or docs, not recorded)

```
{"type":"result","result":"`add` returns the sum of its two arguments.","is_error":false,"subtype":"success","session_id":"5d0c7f3e-2a91-4b6e-9f12-7c3e8a4b1d20","num_turns":1,"request_id":"0f6a2b9c-3e4d-4c1a-8b7e-2d9f5a6c3b10"}
```

## Error output

```
{"type":"result","result":"","is_error":true,"subtype":"error_during_execution","session_id":"5d0c7f3e-2a91-4b6e-9f12-7c3e8a4b1d20","num_turns":0}
```
