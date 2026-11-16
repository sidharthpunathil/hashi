# Aider (`aider`)

Research date: 2026-11-16. Read with `opensrc` where the source is public; otherwise from the official docs and the shipped bundle. Confidence: **source**. The spec Hashi runs is `AIDER` in `src/agent/specs.rs`.

- **Source:** github:Aider-AI/aider
- **License:** Apache-2.0
- **Version checked:** 0.86.3.dev (source only, not installed)
- **Install:** python -m pip install aider-install && aider-install | pipx install aider-chat | uv tool install aider-chat
- **Headless call:** `aider`, prompt flag eq `--message=`
- **Runs tools without asking when headless:** no

## Notes

Source: `~/.opensrc/repos/github.com/Aider-AI/aider/main` (`aider/__init__.py`: 0.86.3.dev). Not installed here; nothing was run, both fixtures are DERIVED from source (the error fixture's litellm line is illustrative). Complements docs/research/agent-clis.md section 9.

**Headless**: `--message=<text>` (aliases `--msg`, `-m`; args.py:639) runs one turn and returns (main.py:1124-1133). Positionals, including after `--`, are files to add, never the prompt, so `flag_eq` is the only safe style. `-m` is the message, not the model; the model flag is `--model` (args.py:60).

**Output**: no JSON. Plain text on stdout: announcement lines, the reply, then `Tokens: <n> sent, <n> received. Cost: $x message, $y session.` (base_coder.py:2023-2068). Counts are abbreviated (`2.1k`), so usage can only be scraped approximately. Hashi's `text` format will include the banner and the Tokens line in the answer unless it strips them. API errors print to stdout/stderr and still **exit 0** (main.py:1130-1133 returns normally after `coder.run`).

**Safety**: `--chat-mode ask` (args.py:164-170, alias of `--edit-format`) uses the ask coder, which answers without editing files. Confirmations: `--yes-always` answers yes except where `explicit_yes_required` is set, which answers no (io.py:866-867); shell commands require explicit yes (base_coder.py:2450-2460), so they never run. Without `--yes-always`, EOF on /dev/null takes each prompt's default, which is usually `y` (io.py:884-886). `offer_url` opens a browser on yes (io.py:796-803); `BROWSER=true` makes Python's `webbrowser` run `/usr/bin/true` instead (UNVERIFIED end to end). **Always pass a model** (`--model` or `AIDER_MODEL`): with none and no key in the env, `select_default_model` offers an OpenRouter OAuth login that opens a browser and waits on a local callback (onboarding.py:116-146, 92-93).

**Slash and bang**: `is_command` is `inp[0] in "/!"` (commands.py:255-256), checked on the raw message (base_coder.py:912-917). `/run` and `!cmd` run shell commands, `/web` fetches, `/add` reads files. Hashi must prefix a space to prompts starting with `/` or `!` (a leading space defeats the check).

**Config hazards**: `.aider.conf.yml` is searched in the git root, cwd and home (main.py:464) and `.env` files are loaded (`--env-file`, args.py:801); a workspace `.aider.conf.yml` could re-enable shell or set `--test-cmd`. Every option can also be set by an `AIDER_<OPTION>` env var (configargparse `auto_env_var_prefix="AIDER_"`, args.py:41), so Hashi should strip inherited `AIDER_*` variables. Aider writes `.aider.chat.history.md`, `.aider.input.history` and `.aider.tags.cache.v4/` into the cwd.

**Resume**: none by id. `--restore-chat-history` replays `.aider.chat.history.md` from the cwd, which works only because Hashi reuses the per-token workspace (pinned off here).

**Config dir**: no single variable; user state lives under `~/.aider` and `~/.aider.conf.yml`, so only `HOME` relocates it. No sign-in command (keys come from env, `--api-key provider=key`, or `.env`).

**Limit messages**: `RateLimitError` maps to "The API provider has rate limited you. Try again later or check your quotas." (exceptions.py:44-47); the litellm exception text precedes it.

## Success output (derived from source or docs, not recorded)

```
Aider v0.86.3
Model: anthropic/claude-sonnet-4-5 with ask edit format
Git repo: none
Repo-map: disabled

OK

Tokens: 2.1k sent, 2 received. Cost: $0.0063 message, $0.0063 session.
```

## Error output

```
Aider v0.86.3
Model: anthropic/claude-sonnet-4-5 with ask edit format
Git repo: none
Repo-map: disabled

litellm.RateLimitError: AnthropicException - {"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed the rate limit for your organization"}}
The API provider has rate limited you. Try again later or check your quotas.
```

## Hashi features (coverage pass, 2026-11-16)

- **Login accounts:** no. Aider has no login of its own. Its only sign-in is an OpenRouter OAuth that runs during onboarding when no model and no key are set (onboarding.py:79-146); it appends OPENROUTER_API_KEY="..." to ~/.aider/oauth-keys.env (onboarding.py:361-367), a path built from Path.home() / expanduser (main.py:370) that no env var moves except HOME. So there is no dir_var and no separate login folder; a 'login account' would just be an OpenRouter API key, which belongs in api_key_accounts.
- **API-key accounts:** yes. Plain ANTHROPIC_API_KEY does NOT reliably select the account: main.py:501 loads ~/.aider/oauth-keys.env, ~/.env, <git root>/.env, ./.env and --env-file with load_dotenv(override=True) (main.py:361-387, search list main.py:305-330), so an owner's ~/.env that sets ANTHROPIC_API_KEY replaces the token's key, and ~/.aider.conf.yml can set anthropic-api-key (args.py:73) which main.py:612-613 writes over os.environ. AIDER_ANTHROPIC_API_KEY is the configargparse env form of --anthropic-api-key (auto_env_var_prefix AIDER_, args.py:41); env beats config files in configargparse, and main.py:612-613 copies it into ANTHROPIC_API_KEY AFTER the dotenv load, so it wins over ~/.env and ~/.aider.conf.yml. Remaining holes: an owner ~/.env that itself sets AIDER_ANTHROPIC_API_KEY (dotenv override=True runs before the re-parse at main.py:504), and the key only fits Anthropic models, so Hashi must also pass --model (an owner config `model:` pointing at another provider would otherwise use the owner's key for that provider). For other providers use AIDER_OPENAI_API_KEY (args.py, --openai-api-key, main.py:615-616) or --api-key provider=key (main.py:601-610, argv only).
- **Status check:** {"args": null, "signed_in_when": null}
- **Identity:** {"file": null, "pointer": null, "jwt_claim": null}
- **Switch:** no. Nothing to switch: keys come from env, ~/.env, ~/.aider.conf.yml and ~/.aider/oauth-keys.env (an append-only list of OPENROUTER_API_KEY lines, onboarding.py:366-367). There is no dir_var, so Hashi cannot hold a second login folder to switch in, and ~/.env (override=True) outranks oauth-keys.env anyway because oauth-keys.env is loaded first (main.py:369-373).
- **Live quota:** no. Aider reports no remaining quota. It only asks OpenRouter GET https://openrouter.ai/api/v1/auth/key (Bearer OPENROUTER_API_KEY) for is_free_tier to pick a default model (onboarding.py:18-41); that same endpoint also returns limit/usage for the key, but it is provider-specific and not an aider feature. Anthropic/OpenAI keys have no balance endpoint.
- **Usage in output:** none
- **Limit messages:** Spec's list matches exceptions.py:44-47 (RateLimitError: 'The API provider has rate limited you. Try again later or check your quotas.') and litellm's 'RateLimitError'. Missing: 'insufficient credits' (exceptions.py:102-110, OpenRouter 402 maps to 'Insufficient credits with the API provider. Please add credits.', non-retryable). The OpenRouter 'rate limiting your requests' text (exceptions.py:91-99) is already covered by 'rate limit'. Rate limits are retried with doubling backoff until the delay passes RETRY_TIMEOUT=60s (models.py:26, base_coder.py:1449-1485), each retry printing the error, so a limit costs about two minutes before the run gives up. Exit code stays 0 on API errors, so the text matching is what flags failure.

Source: ~/.opensrc/repos/github.com/Aider-AI/aider/main (aider/__init__.py 0.86.3.dev); not installed, nothing run.

- **Login**: none. OpenRouter OAuth only in onboarding (onboarding.py:79-146, 355-375), key appended to ~/.aider/oauth-keys.env and loaded first at startup (main.py:369-373). HOME is the only relocator.
- **Keys**: dotenv files load with override=True in order oauth-keys.env, ~/.env, git-root .env, ./.env, --env-file (main.py:305-330, 361-387); then args are re-parsed (main.py:504) and --set-env, --api-key, --anthropic-api-key, --openai-api-key are written into os.environ (main.py:590-616). So AIDER_ANTHROPIC_API_KEY / AIDER_OPENAI_API_KEY are the variables that win.
- **Status**: no auth status command. `--exit` (args.py:681-684) runs startup and exits but does not validate the key.
- **Identity**: none stored.
- **Quota**: none; OpenRouter /api/v1/auth/key is used only for the free-tier check (onboarding.py:18-41).
- **Usage**: Spec reads Text, so Hashi records none; the Tokens/Cost line is printed (base_coder.py:2023-2068).
- **Limits**: exceptions.py:44-47, 102-110; retries up to RETRY_TIMEOUT 60s (models.py:26, base_coder.py:1449-1485).
