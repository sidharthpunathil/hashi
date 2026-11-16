# Kiro CLI (`kiro`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **docs**. The spec Hashi runs is `KIRO` in `src/agent/specs.rs`.

- **Source:** closed
- **License:** Proprietary (Kiro CLI is closed source; its predecessor github:aws/amazon-q-developer-cli is Apache-2.0/MIT and was read as the reference implementation)
- **Version checked:** 3.3.0 (docs and changelog, 2026-11-12); source read: amazon-q-developer-cli 1.19.7 (main)
- **Install:** curl -fsSL https://cli.kiro.dev/install | bash
- **Headless call:** `kiro-cli chat --no-interactive`, prompt dashdash
- **Runs tools without asking when headless:** no

## Notes

## What it is
Kiro CLI (`kiro-cli`, AWS) is the closed-source successor of Amazon Q Developer CLI (`q chat`). The Q repo README says: "Amazon Q Developer CLI is now available as Kiro CLI, a closed-source product" (amazon-q-developer-cli README.md:3-4). npm `kiro-cli` is an unrelated 0.0.1 placeholder (bin `kirox`). Nothing below was run; fixtures are derived from the Q source plus kiro.dev docs.

## Sources
- Docs: https://kiro.dev/docs/reference/cli-commands/ (chat flags, env vars, login options), https://kiro.dev/docs/cli/headless/ (KIRO_API_KEY, trust flags, stream-json mention, exit codes), https://kiro.dev/changelog/cli/ (3.3.0, 2026-11-12).
- Reference source: ~/.opensrc/repos/github.com/aws/amazon-q-developer-cli/main (paths below are under crates/chat-cli/src).

## Headless
- `kiro-cli chat --no-interactive [INPUT]`. Docs: "Print first response to STDOUT without interactive mode". Q source: cli/chat/mod.rs:230-255 (`ChatArgs`: `-r/--resume` bool, `--agent`, `--model`, `-a/--trust-all-tools`, `--trust-tools` comma list, `--no-interactive` alias `--non-interactive`, positional `input`, `-w/--wrap`). With no INPUT it reads non-TTY stdin and bails "Input must be supplied when running in non-interactive mode" if empty (mod.rs:261-279); /dev/null plus a prompt is fine.
- Prompt: clap positional `input: Option<String>`, so `-- <prompt>` is honoured by clap's default parsing (Q source). Kiro 3.x is closed; that it is still clap is UNVERIFIED but the flag surface is identical.
- 3.x adds `--v3`/`--v2`/`--agent-engine`, `--effort`, `--sandbox`, `--require-mcp-startup`, `--resume-id <ID>`, `--list-sessions`, `--output-format stream-json` (V2/V3 only). Changelog 3.0.0: scripts using `--trust-all-tools` default to V3, others stay on V2.

## Output
- `--output-format stream-json` exists (headless doc) but its event schema is NOT documented anywhere I could find (docs, changelog, web search), so the safe choice here is plain text. Revisit once the schema is published or observed from a real run.
- Text mode (Q source): the answer is streamed to stdout with a `> ` prefix before the first assistant text (mod.rs:2879-2887); tool use banners ("Using tool: ..." with an emoji and bullets) also go to stdout (mod.rs:3519-3548); MCP/tool loading chatter goes to stderr (mod.rs:437). So stdout text needs the leading `> ` stripped and may contain tool banners. Kiro additionally prints a per-turn credits/time line (UNVERIFIED wording and stream; `/usage` reports session credits since 3.1.0). Whether NO_COLOR suppresses ANSI on stdout is UNVERIFIED; `KIRO_LOG_NO_COLOR` only affects logs.
- No session id and no token usage in stdout: kirodotdev/Kiro issue #9066 (2.5.1) says `--no-interactive` "prints neither the session ID nor a resume hint"; the request for `--output-format json` with sessionId/credits was closed not planned (2026-09-15). Usage is credits, not tokens.

## Resume
- `--resume-id <ID>` resumes a local or cloud session by id; `-r/--resume` (bool) resumes the most recent conversation in the cwd, which works naturally with Hashi's per-token workspace. Hashi cannot capture the id from output; `kiro-cli chat --list-sessions` in the workspace could recover it (format UNVERIFIED). Consider mapping resume to bare `--resume` instead.

## Permissions
- Q source: in non-interactive mode, if the model requests a tool that is not trusted the session aborts with "Tool approval required but --no-interactive was specified. Use --trust-all-tools to automatically approve tools." (mod.rs:573-576, 830-838). The default agent trusts read-only tools (fs_read) so reads work; writes and shell fail the run rather than silently running. Kiro V3 docs: sub-agent calls without approval are denied with "[denied] tool permission approval is not supported in non-interactive mode"; hooks needing confirmation are denied without --trust-all-tools. Top-level V3 behaviour for untrusted tools is UNVERIFIED (deny vs abort).
- Safe default: no trust flags. `--trust-tools=read_file,grep_search` (V3 names) is the documented read-only CI pattern. `--sandbox` (V3) forces the local command sandbox. Strip `--trust-all-tools`, `-a`, `--trust-tools` (and `--trust-tools=...`), `--cloud`, `--repo` from operator args.
- Slash commands: a prompt starting with `/` runs a built-in command in non-interactive mode (third-party sehercode runs `kiro-cli chat --no-interactive "/usage"`; Q source routes `/` input through its command parser). Neutralise a leading `/`.

## Auth and config
- Headless docs: "Headless mode requires an API key set as the KIRO_API_KEY environment variable." Keys are Pro/Pro+/Pro Max/Power only. Interactive `kiro-cli login` stores tokens locally (Q stored them in `<data_local_dir>/amazon-q/data.sqlite3`, util/paths.rs:327-331; the Kiro path is UNVERIFIED, likely a kiro-cli data dir). `KIRO_HOME` "overrides the global Kiro directory" (agents, settings, MCP) but whether it also moves the auth database is UNVERIFIED, so config_dir_env is null; use KIRO_API_KEY per account instead. Q also honoured `AMAZON_Q_SIGV4` with AWS credentials (util/consts.rs:100); Kiro support UNVERIFIED, listed in credential_envs defensively.
- Telemetry: Q honoured `Q_DISABLE_TELEMETRY` (util/consts.rs:79); Kiro equivalent UNVERIFIED (`kiro-cli settings telemetry.enabled false` likely).

## Errors and limits
- Q classifies 429s (api_client/mod.rs:540-560): throttling prints " Amazon Q rate limit reached:" (constants.rs:17, Kiro presumably rebranded), monthly cap matches body `MONTHLY_REQUEST_COUNT` ("Maximum Request reached for this month.", api_client/retry_classifier.rs:110) and prints "Monthly request limit reached" (mod.rs:1111). Errors go to stderr with a non-zero exit; stdout may be empty, hence fixture_error null. Credit-exhaustion wording for Kiro is UNVERIFIED.
- Exit codes (docs): 3 MCP startup failure with --require-mcp-startup, 4 unknown --agent; nonzero if stdout closes early. Model overload in non-interactive mode errors with "Please relaunch with '--model <model_id>'" (mod.rs:1072-1080).

## Other
- `--list-models --format json` lists models. `KIRO_HEADLESS_WORKFLOW_TIMEOUT_SECS` caps V3 workflow waits (default 21600 s); set it below Hashi's timeout if V3 is used.
- An alternative integration is `kiro-cli acp` (Agent Client Protocol, JSON-RPC over stdio), which would expose session ids and structured events but is not a one-shot argv run.

## Success output (derived from source or docs, not recorded)

```
> `add` returns the sum of its two arguments.
```
