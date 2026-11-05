# Practical additions: design

These features were chosen for value per effort. They make Hashi usable from
existing tools and with real projects, and give people a way to stop what
they started.

## 1. OpenAI-compatible API

- `POST /v1/chat/completions` and `GET /v1/models` take the same bearer
  tokens. All limits, pause, scopes, queue and account rules apply, because
  the endpoint calls the same submit path as `/v1/ask`.
- **Models.** `model` selects the agent: `claude`, `codex`, `gemini`,
  `opencode`, `qwen` or `cursor`. `agent/model`, such as `claude/opus`,
  passes the part after the slash as the agent's model, with the same
  `is_safe_arg` check. `/v1/models` lists only the agents this token may use
  that are installed.
- **Messages.** System and developer messages are put before the
  conversation as an instruction block. User and assistant turns become a
  plain transcript, and the last user turn ends the prompt. Only text
  content is accepted (strings, or parts with `type: "text"`). Anything else
  is a 400 with an OpenAI-style error: `{"error": {"message", "type",
  "code"}}`.
- **Response.** Standard `chat.completion` with `id`, `created`, `model`,
  `choices[0].message`, `finish_reason: "stop"`, and `usage` mapped from the
  agent's reported tokens where available.
- **`stream: true`.** Server-sent events in the OpenAI chunk format: a role
  chunk, then `: queued` / `: running` comment lines as keep-alives while
  the request waits or runs, then one content chunk with the whole answer, a
  final chunk with `finish_reason`, and `data: [DONE]`. A failure sends an
  error event and closes the stream.
- **Time limit.** Without streaming, the request is held up to the existing
  `wait` maximum (120 s). After that, the response is a 202 with the Hashi
  request id and a hint to use `stream: true` or `/v1/ask/{id}`. The
  long-poll cap per token applies.
- Unknown fields such as `temperature` and `tools` are ignored, and `tools`
  is documented as not supported.
- **Docs.** `docs/api.md` gets a section with examples for the `openai`
  Python package, `curl` and the Open WebUI connection settings.

## 2. Share a project folder per token

Superseded by 2026-11-06-projects-design.md: projects carry the folder, the accounts and the defaults, and tokens bind to a project.

- An admin-only token and invite field, `workdir`: an absolute path to an
  existing directory. Symlinks are resolved and the result is stored.
- Runs for that token start in this directory instead of the per-token
  workspace. The agent sandboxes stay as they are (Codex read-only, opencode
  read-only tools, and so on).
- Several tokens may share one folder. Revoking a token never deletes a
  workdir, only Hashi's own workspace.
- **Setting it:** CLI `--workdir` on `token create|update` and `share`, and
  `--clear-workdir`. API field `workdir`. The dashboard token and share
  forms.
- **Visibility.** The token holder sees only the folder name, never the
  full path: `/v1/me` shows `project: "api"`, and the claim page says "works
  in the project api".
- **Security model, documented.** Agents already run as the owner. A
  workdir changes where a run starts, not what it can read. Project config
  in that folder (for example `.gemini/`) may load for agents that read
  project config; Claude Code is pinned to user settings. Share only
  folders you trust.

## 3. Cancel a request

- `DELETE /v1/ask/{id}` lets a token cancel its own requests. An admin may
  cancel any. MCP gets a `cancel` tool.
- A queued request becomes `failed` with "cancelled". A running one is
  stopped through the existing cancellation registry (the same path revoke
  uses), so its whole process tree is killed.
- An already finished request answers 409.
- The dashboard Requests view gets a key (choose a free one, for example
  `x` if it is unused there) that cancels the selected queued or running
  request after confirmation.
- Requests cancelled by someone other than the token itself are audited.

## 4. QR code for invite links

The invite popup shows the link as a QR code drawn with Unicode half blocks
when the terminal is big enough. Otherwise it shows a hint to enlarge it.
The `qrcode` crate is used if its licence passes `deny.toml`. Test the
rendering at a fixed size.

## Non-goals

- Tool calling, images, embeddings or audio on the OpenAI endpoint.
- Token-by-token streaming of agent output.
- Uploading files, webhooks, and running as a background service.
