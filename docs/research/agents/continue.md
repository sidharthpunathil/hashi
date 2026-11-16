# Continue CLI (`continue`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `CONTINUE` in `src/agent/specs.rs`.

- **Source:** github:continuedev/continue
- **License:** Apache-2.0
- **Version checked:** 1.5.47
- **Install:** npm i -g @continuedev/cli
- **Headless call:** `cn -p`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

Source: `github:continuedev/continue` main, `extensions/cli` (`@continuedev/cli`, TypeScript, commander ^14). Cached at `~/.opensrc/repos/github.com/continuedev/continue/main/extensions/cli`. Latest npm release is 1.5.47 (2026-06-18); I downloaded that tarball (`npm pack`, not installed, not run) and grepped the bundle: it contains the same headless default policies, the same `processJsonOutput` wrapper string and the "Hub authentication has been removed" stub as main, so line numbers below (main) describe 1.5.47. Not installed here; nothing was executed.

**Headless:** `cn -p [prompt]` (src/index.ts:181-297). `-p/--print` maps to `options.headless` (index.ts:294). Detection of headless for stdout suppression is `process.argv.includes("-p") || includes("--print")` (util/cli.ts:10-13); in that mode init.ts:41-60 turns every console method into a no-op and blocks process.stdout/stderr writes, so only `safeStdout`/`safeStderr` output reaches the pipes. Stdin is read when piped (index.ts:264-277); `/dev/null` yields nothing, harmless. Headless with no prompt exits 1 (index.ts:279-292). Headless processes exactly one turn then `gracefulExit(0)` (commands/chat.ts:510-544).

**Prompt passing:** positional `[prompt]`. Commander honours `--` (bundle: `if(l==="--"){o===r&&o.push(l),o.push(...i);break}`), so `-p -- <prompt>` is safe for prompts starting with `-`. **Gotcha:** commander still dispatches subcommands from operands after `--` (`_parseCommand`: `if(t&&this._findCommand(t[0]))return this._dispatchSubcommand(...)`), so a prompt that is exactly `ls`, `serve` (hidden HTTP server), `checks`, `review` or `help` runs that subcommand instead (index.ts:301, 312, 342, 350). Hashi should refuse or pad such prompts (UNVERIFIED end to end, derived from commander source in the bundle). Do NOT use `--prompt <x>`: it is "file path, hub slug, or string content" (shared-options.ts:57-66) and would read files.

**Slash commands:** in headless only an input of exactly `/compact` is special (chat.ts:358-360); other `/...` text goes to the model. So slash_commands_in_prompt is true only for `/compact`.

**Output:** default is plain text: the final assistant response plus `\n` via `safeStdout` (chat.ts:202-219). `--format json` (only value allowed, flags/flagValidator.ts:78-89) is weak: if the reply parses as JSON it is printed raw, otherwise it is wrapped as `{"response":...,"status":"success","note":"Response was not valid JSON, so it was wrapped in a JSON object"}` (chat.ts:43-60). No session id, no usage, no cost in either mode, and a reply like `42` or `{...}` breaks a `/response` pointer. Recommendation: text mode. `--silent` strips `<think>` blocks and collapses 3+ newlines (chat.ts:67-79); optional. Auto-compaction and `/compact` can print extra JSON status lines to stdout in json mode (chat.ts:136-158, 242-281). If the reply is empty nothing is printed (chat.ts:212).

**Errors:** headless failures print `{"status":"error","message":"..."}` to **stderr** (not stdout, despite the comment) and exit 1 (chat.ts:620-637). Stdout is empty, so Hashi's stderr fallback on non-zero exit covers it. Unhandled rejections flip a 0 exit to 1 (util/exit.ts:214-216). 429/502/503/504, `rate_limit_exceeded`, `server_error`, and messages containing `overloaded` are retried with backoff (util/exponentialBackoff.ts:51-52, 58-77, 117+); exact final rate-limit wording comes from the provider SDK (UNVERIFIED).

**Usage/cost:** tracked in-process (session.ts:149-156, 587-589) and persisted in the session JSON under `$CONTINUE_GLOBAL_DIR/sessions/<uuid>.json`, but never printed to stdout. `--verbose` logs a usage summary through the logger at exit (util/exit.ts:142-183), which headless suppresses (UNVERIFIED whether it reaches a log file only). So Hashi gets no tokens.

**Session/resume:** sessions are uuidv4 (session.ts:342, 565) stored in `$CONTINUE_GLOBAL_DIR/sessions` (session.ts:35-59). The id is never printed. `--resume` is a boolean that loads the most recently modified session file in that directory, across all workspaces (session.ts:301-333): unusable for concurrent tokens. `--fork <sessionId>` copies an existing session's history into a new session (chat.ts:91-104), effectively resume by id, but Hashi cannot learn ids from output (`cn ls --json`, index.ts:300-310, lists them). resume_flag is therefore null. `--resume` and `--fork` are mutually exclusive (flagValidator.ts:114-119).

**Model:** no plain model-name flag. `--model <slug>` adds a Continue Hub model block `owner/package` (shared-options.ts:46-55, services/ConfigService.ts:114-131), not a provider model id, and Hub auth is now removed (auth/workos.ts:1-7, 125-128), so model_flag is null. The model comes from `config.yaml` (`--config <path>` or `$CONTINUE_GLOBAL_DIR/config.yaml`, configLoader.ts:84-100). With neither, it fetches `continuedev/default-cli-config` from `CONTINUE_API_BASE` (default https://api.continue.dev/, env.ts:9) unauthenticated (configLoader.ts:326-341; result UNVERIFIED).

**Credentials:** if `ANTHROPIC_API_KEY` is set and `--config` is not given, headless **rewrites `$CONTINUE_GLOBAL_DIR/config.yaml`** with an Anthropic model using that key (services/index.ts:79-93, onboarding.ts:35-49). So each Hashi account must have its own `CONTINUE_GLOBAL_DIR`. `CONTINUE_USE_BEDROCK=1` selects Bedrock (onboarding.ts:59-65; AWS env vars UNVERIFIED). Other providers are configured only through config.yaml (keys may be inline or `${{ secrets.X }}`, resolution UNVERIFIED). `CONTINUE_API_KEY` is referenced only by smoke tests (smoke-api/headless-continue-proxy.test.ts:11), not runtime. No login command: `login()` throws "Login is not available. Hub authentication has been removed." (auth/workos.ts:125-129).

**Permissions (critical):** default headless policies allow everything except Edit/MultiEdit/Write: `Bash` allow and `*` (MCP and any other tool) allow when headless (permissions/defaultPolicies.ts:30-37); "ask" tools (Edit, MultiEdit, Write) are dropped from the request tool list and denied if called (stream/handleToolCalls.ts:172-196, stream/streamChatResponse.helpers.ts:120-131). Fetch is allowed by default (defaultPolicies.ts:21). So out of the box headless can run shell commands. CLI flags have highest precedence, first match wins (permissions/precedenceResolver.ts:33-60), and within CLI flags all `--exclude` entries come before `--ask`/`--allow` (precedenceResolver.ts:65-95), so `--exclude * --allow Read` still excludes Read: a read-only allowlist is not expressible by flags. Hashi pins `--exclude *` (pattern `*` matches every tool, permissions/permissionChecker.ts:17-22), giving a no-tools chat run. Read-only alternative (UNVERIFIED list completeness): `--exclude Bash --exclude Fetch --exclude Write --exclude Edit --exclude MultiEdit --exclude UploadArtifact --exclude Skills`, but MCP tools from config would still be allowed. `--readonly` (plan mode) and `--auto` replace ALL policies, ignoring CLI `--exclude` (services/ToolPermissionService.ts:205-218); plan mode allows Bash and `*` (defaultPolicies.ts:42-66), so both are unsafe. `--agent <slug>` agent files also take full precedence (ToolPermissionService.ts:207-212). `~/.continue/permissions.yaml` sits below CLI flags (precedenceResolver.ts:44-51).

**Workspace and env hazards:** `dotenv.config()` loads `<cwd>/.env` into process.env at startup (env.ts:4-6), so a workspace `.env` can inject `ANTHROPIC_API_KEY` (which then rewrites config.yaml) or `CONTINUE_GLOBAL_DIR`/`CONTINUE_API_BASE` when Hashi does not set them (dotenv does not override existing vars, so Hashi should always set `CONTINUE_GLOBAL_DIR` and `CONTINUE_API_BASE`). Rules load from `<cwd>/.continue/rules` and `AGENTS.md`/`AGENT.md`/`CLAUDE.md`/`CODEX.md` (systemMessage.ts:87-93, 167). Hooks config is loaded from `~/.claude/settings.json`, `$CONTINUE_GLOBAL_DIR/settings.json` and `<cwd>/.claude|.continue/settings(.local).json` (hooks/hookConfig.ts:73-91), but in this source no code outside `hooks/` calls the fire* functions, so hooks appear not to run yet (UNVERIFIED; recheck on upgrade). Auto-update is skipped in headless (services/UpdateService.ts:35-37). OTEL metrics only when OTEL endpoint vars are set; `CONTINUE_METRICS_ENABLED=0` disables them (telemetry/telemetryService.ts:66-84).

**Git repo:** not required (UNVERIFIED). **Windows:** npm package, Node; UNVERIFIED.

**argv, new run:** `["-p", "--exclude", "*", "--config", "<account>/config.yaml", "--", "<prompt>"]` (the `--config` is optional when `CONTINUE_GLOBAL_DIR/config.yaml` exists).

fixture_success is derived from source (chat.ts:202-219, text mode), not run. fixture_error is null because the failure JSON goes to stderr; the stderr line would be `{"status":"error","message":"<error text>"}`.

## Success output (derived from source or docs, not recorded)

```
OK
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** no. There is no login any more: `login()` throws `Login is not available. Hub authentication has been removed.`, `isAuthenticated()` is always false and AuthConfig is always null (extensions/cli/src/auth/workos.ts:1-7, 125-133). Credentials are only provider keys in config.yaml or ANTHROPIC_API_KEY / CONTINUE_USE_BEDROCK. CONTINUE_GLOBAL_DIR does move everything the CLI keeps (config.yaml, sessions, globalContext.json, permissions.yaml; extensions/cli/src/env.ts:8-12; core/util/paths.ts:27-28, 94-95), so a separate folder still isolates a config-file account, but there is nothing to sign in to.
- **API-key accounts:** no. In headless mode with no --config, ANTHROPIC_API_KEY makes the CLI rewrite $CONTINUE_GLOBAL_DIR/config.yaml: managed Anthropic model blocks are dropped and re-added with the env key at the END of models (extensions/cli/src/services/index.ts:79-93; onboarding.ts:18, 35-49; util/yamlConfigUpdater.ts:76-121). Any other models the owner configured stay first, and a persisted `cliSelectedModel` in globalContext.json (util/modelPersistence.ts:7-19; auth/workos.ts:75) can still pick one of them, so in the owner's folder the key does not fully select the account, and the run also overwrites the owner's config.yaml. It works when each key account has its own empty CONTINUE_GLOBAL_DIR.
- **Status check:** {"args": null, "signed_in_when": null}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** no. No login to switch: Hub auth is removed (auth/workos.ts:125-133). The only credential is an inline apiKey inside config.yaml, which also carries models, rules, MCP servers and every other setting, so swapping it would change the owner's whole configuration. Leave switch None.
- **Live quota:** no. No account and no Continue-side billing in the CLI; Hub auth removed (auth/workos.ts:1-7). Quota is whatever the underlying provider key has.
- **Usage in output:** none
- **Limit messages:** Errors go to stderr as {"status":"error","message":...} with exit 1 (commands/chat.ts:620-637); wording is the provider SDK's. 429/502/503/504, error.type `rate_limit_exceeded`/`server_error`, and `overloaded` are retried first (util/exponentialBackoff.ts:51-77). `rate limit` and `too many requests` are reasonable; worth adding `rate_limit_exceeded`, `rate_limit_error` (Anthropic error type) and `insufficient_quota` (OpenAI). No Continue-specific quota strings exist.

Source: github:continuedev/continue main, extensions/cli (@continuedev/cli, bundle 1.5.47 matches). Not installed; nothing run.

- `env.continueHome = CONTINUE_GLOBAL_DIR || ~/.continue` (extensions/cli/src/env.ts:8-12); `dotenv.config()` loads <cwd>/.env first (env.ts:4-6), so Hashi must always set CONTINUE_GLOBAL_DIR itself.
- No keyring, no tokens, no identity, no status command (subcommands: ls, serve, checks, review; index.ts:301-350).
- Output is plain text; usage is tracked in the session file under $CONTINUE_GLOBAL_DIR/sessions but never printed (session.ts:149-156, 587-589), so usage_in_output is none.
