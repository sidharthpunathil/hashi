# Agent coverage

Which Hashi features each agent supports. Generated from the agent specs in `src/agent/specs.rs` (and, for the first six, their own modules) on 2026-11-16; each agent's source was read with [`opensrc`](https://github.com/vercel-labs/opensrc) where public. Per-agent evidence, with file and line references, is in [`research/agents/`](research/agents/).

| Agent | `agent` value | Runs prompts | Reports | Login accounts | API-key accounts | Sign-in | Status check | `hashi switch` | Shows who is signed in | Live quota |
|---|---|---|---|---|---|---|---|---|---|---|
| Claude Code | claude | yes | tokens and cost | yes | yes | `auth login` | `auth status` | yes | email | yes |
| Codex | codex | yes | tokens | yes | yes | `login` | - | yes | email | yes |
| Gemini CLI | gemini | yes | tokens | yes | yes | built in | - | yes | email | - |
| opencode | opencode | yes | tokens and cost | yes | - | `auth login` | - | yes | providers | - |
| Qwen Code | qwen | yes | tokens | yes | - | built in | - | - | - | - |
| Cursor CLI | cursor | yes | text only | - | yes | - | - | - | - | - |
| Copilot CLI | copilot | yes | tokens | - | yes | - | - | - | - | upstream only |
| Grok Build | grok | yes | text only | yes | yes | `login` | - | yes | - | - |
| Antigravity | antigravity | yes | tokens | - | - | - | - | - | - | upstream only |
| Goose | goose | yes | tokens and cost | yes | - | `configure` | `info --check` | - | - | - |
| Amp | amp | opt-in | tokens | - | yes | - | - | - | - | upstream only |
| Factory Droid | droid | opt-in | tokens | yes | yes | built in | - | - | - | upstream only |
| Kimi Code | kimi | opt-in | text only | yes | - | `login` | - | yes | - | upstream only |
| Kilo Code CLI | kilo | yes | tokens and cost | yes | - | `auth login` | - | yes | - | upstream only |
| Cline CLI | cline | yes | tokens and cost | yes | yes | `auth` | - | yes | yes | upstream only |
| Crush | crush | opt-in | text only | yes | - | `login` | - | yes | yes | upstream only |
| Aider | aider | yes | text only | - | - | - | - | - | - | - |
| Continue CLI | continue | yes | text only | yes | yes | - | - | - | - | - |
| Pi | pi | yes | tokens and cost | yes | yes | `auth login anthropic` | - | yes | - | - |
| oh-my-pi | omp | yes | tokens and cost | yes | yes | `login` | - | - | - | upstream only |
| OpenClaude | openclaude | yes | tokens | yes | - | `auth login` | `auth status --json` | - | - | upstream only |
| CodeBuddy Code | codebuddy | yes | tokens | - | yes | - | - | - | - | - |
| Qoder CLI | qoder | yes | tokens | yes | yes | `login` | `--list-models` | - | - | - |
| Qoder CN | qoder-cn | yes | tokens | yes | yes | `login` | `--list-models` | - | - | - |
| Kiro CLI | kiro | yes | text only | - | yes | - | - | - | - | upstream only |
| Auggie | auggie | opt-in | tokens | - | yes | - | - | - | - | upstream only |
| Devin CLI | devin | yes | text only | yes | yes | `auth login` | `auth status` | yes | - | - |
| Rovo Dev | rovodev | opt-in | text only | - | - | - | `rovodev auth status` | - | - | - |
| Mistral Vibe | vibe | yes | text only | yes | yes | `--setup` | - | - | - | upstream only |
| Hermes Agent | hermes | yes | tokens | yes | yes | `setup` | - | yes | - | upstream only |
| OpenClaw | openclaw | opt-in | tokens and cost | yes | yes | `models auth login` | `models status --check --json` | - | - | upstream only |
| Jcode | jcode | opt-in | tokens | yes | yes | `login` | - | - | - | upstream only |
| MiMo Code | mimo | opt-in | tokens and cost | yes | yes | `providers login` | - | yes | yes | - |
| Ante | ante | yes | tokens | yes | yes | `auth login` | - | - | - | upstream only |
| TRAE CLI | trae | yes | tokens and cost | - | - | - | - | - | - | - |
| Muse Code | muse | opt-in | tokens | - | yes | - | - | - | - | upstream only |
| DeepSeek | deepseek | opt-in | tokens | - | yes | - | - | - | - | - |
| ZCode | zcode | yes | tokens | yes | - | `login` | - | - | - | - |
| Prime Agent | prime | opt-in | tokens and cost | yes | yes | built in | - | yes | - | - |
| Autohand Code | autohand | yes | text only | yes | - | `login` | - | - | - | upstream only |
| Command Code | command-code | yes | tokens | - | yes | - | `whoami` | - | - | upstream only |


What the columns mean:

- **Runs prompts**: through the HTTP API, `hashi ask`, tokens, projects and the queue. `opt-in` means the agent can run shell or write tools without asking in a headless run, so it is offered only when named in `--enable-agents`.
- **Reports**: what a run records in usage stats. `text only` agents print no token counts Hashi can read.
- **Login accounts**: `hashi account add <agent> <name>` keeps a second sign-in in its own folder, which one variable moves. A `-` means the agent keeps its login somewhere that variable does not reach (usually the OS keyring), so a second login would share or overwrite yours.
- **API-key accounts**: `hashi account add <agent> <name> --key-stdin` passes a key the agent reads before any stored login.
- **Sign-in**: the command `hashi account login` runs with the account's folder set; `built in` means the agent signs in from its own screen.
- **Status check**: the command `hashi account list` runs to say whether a login account still works. Without one it says whether the login files are there.
- **`hashi switch`**: the plain command can be pointed at a login account, because its login is plain files in one folder. A `-` means it is kept in an OS keyring, encrypted per path, or mixed into a database or settings file, where copying it would be unsafe.
- **Shows who is signed in**: `hashi switch <agent>` and `hashi account list` name the user of the plain command.
- **Live quota**: `yes` is read in the background today (Claude Code and Codex). `upstream only` means the agent has a quota endpoint or command that Hashi does not read yet; for every agent, a run that hits a limit still marks the account limited and moves the prompt to the next one.

## Checked against the real CLIs (2026-11-16)

Twenty-three of the new agents were installed (npm packages into a scratch folder, plus Grok, Antigravity, Goose and Crush already on the test machine) and checked three ways:

- **Flags:** every flag Hashi passes appears in the CLI's own `--help`, for all 23. Grok's `--no-auto-update` is hidden from help but accepted (Grok rejects unknown flags). Auggie accepts any flag, so `--disable-process-tools` could not be confirmed.
- **A headless run through `hashi ask`** with an empty home folder: 21 of 23 stop at once with the agent's own "not signed in" message, which shows the arguments were accepted. **Grok and Amp start an interactive sign-in instead and wait until Hashi's timeout**, so give them a working login or key; an expired login does the same. Antigravity also answered a real prompt with the owner's login.
- **`hashi switch`** with the real binary in a sandboxed home: switch to an account, show it, switch back and `hashi account reset` all work for the 10 agents in the switch column.
- **Status checks** with no login: Goose, OpenClaude, Qoder, Qoder CN, OpenClaw and Command Code report "not signed in". Droid's `doctor --checks auth` exits 0 even when signed out, so it is not used.

Not installed or not runnable here: Aider, Hermes (Python), Kiro, Devin, Rovo Dev, TRAE, Muse, Ante, ZCode, Prime Agent, Vibe and Jcode.

Not covered at all: Codebuff and Freebuff (no one-shot mode), and Orca's claude-teams (an interactive launcher over `claude`). `opencode2` is a second binary name of opencode.
