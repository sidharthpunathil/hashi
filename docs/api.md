# API reference

Hashi listens on `http://127.0.0.1:7878` by default. Every endpoint except `/`, `/healthz`, `/v1/status` and the share pages needs `Authorization: Bearer <token>`. Request and response bodies are JSON with camelCase keys.

## Endpoints

| Method | Path | Auth | Purpose |
|---|---|---|---|
| `GET` | `/` | | Name, version and a pointer to the docs |
| `GET` | `/healthz` | | Liveness probe |
| `GET` | `/v1/status` | | Version, uptime, which agents are installed |
| `POST` | `/v1/ask` | token | Queue a prompt |
| `GET` | `/v1/ask/{id}` | token | Read a result. Add `?wait=30` to long-poll |
| `DELETE` | `/v1/ask/{id}` | token | Cancel a queued or running request (admins: any request) |
| `POST` | `/v1/chat/completions` | token | OpenAI-compatible chat, with `stream: true` support |
| `GET` | `/v1/models` | token | OpenAI-compatible list of the agents this token may use |
| `DELETE` | `/v1/session/{agent}` | token | Forget this token's conversation with an agent |
| `GET` | `/v1/me` | any token | The token's own status, limits and usage (works when paused or expired) |
| `GET` | `/v1/stats` | admin | Request counters, plus memory, disk and run slots under `resources` |
| `GET` | `/v1/tokens` | admin | List tokens |
| `POST` | `/v1/tokens` | admin | Create a token |
| `PATCH` | `/v1/tokens/{id}` | admin | Pause, resume, extend, rename or set limits |
| `DELETE` | `/v1/tokens/{id}` | admin | Revoke a token |
| `GET` `POST` | `/v1/invites` | admin | List or create invite links |
| `DELETE` | `/v1/invites/{id}` | admin | Cancel a pending invite |
| `POST` | `/v1/claim/preview`, `/v1/claim/redeem` | invite code | What an invite grants; use it once |
| `GET` | `/v1/audit` | admin | Recent sharing and access changes |
| `POST` | `/mcp` | token | MCP server (JSON-RPC over streamable HTTP). `GET /mcp` answers 405 |
| `GET` | `/claim/{code}`, `/me` | | Pages for the people you share with |

## `POST /v1/ask`

| Field | Type | Default | Notes |
|---|---|---|---|
| `message` | string | required | Up to 32 KB |
| `agent` | `claude`, `codex`, `gemini`, `opencode`, `qwen` or `cursor` | server default (`claude`) | Must be installed and allowed for the token |
| `model` | string | agent default | Passed to the agent's `--model` / `-m` |
| `wait` | seconds | none | Hold the request open up to this long (max 120) |

It returns `200` with the result when the run finished within `wait`, or `202` with a `poll` path otherwise. `status` is one of `queued`, `running`, `completed` or `failed`. A queued request also reports its `position`.

```json
{
  "id": "c761eb2f-27b8-41c6-a485-8a475d7cec21",
  "agent": "claude",
  "status": "completed",
  "response": "src/main.rs parses the CLI and starts the server...",
  "usage": { "inputTokens": 36371, "outputTokens": 412, "costUsd": 0.1985 },
  "durationMs": 5132
}
```

A `stateful` token resumes its own conversation with each agent on every prompt. `DELETE /v1/session/{agent}` starts it over.

## `DELETE /v1/ask/{id}`

Cancels a request. A queued request fails at once with the error `cancelled`. A running one is stopped the way revoking a token stops it: its whole process tree is killed, and the call answers once it is gone. The answer is the request, now `failed` with `error: "cancelled"`.

A token can cancel its own requests; an admin token can cancel any. Another token's request answers `404`, the same as an unknown id, and a request that has already finished answers `409`. A cancel by anyone other than the token that sent the request is recorded in the audit log as `request.cancel`.

## OpenAI-compatible API

`POST /v1/chat/completions` and `GET /v1/models` speak enough of the OpenAI API for apps and libraries built for it. They take the same Hashi tokens, and every prompt goes through the same path as `POST /v1/ask`, so pause, agent scope, limits and the queue all apply.

- **`model`** picks the agent: `claude`, `codex`, `gemini`, `opencode`, `qwen` or `cursor`. Add the agent's own model after a slash, for example `claude/opus` or `gemini/gemini-2.5-pro`. Without `model` the server's default agent answers.
- **`messages`.** System and developer messages go first as an instruction block, earlier user and assistant turns follow as a plain transcript, and the last message, which must be from the user, ends the prompt. Only text is accepted: a string, or parts with `type: "text"`. Images, audio and `tool` messages answer `400`.
- **Response.** A standard `chat.completion` with `finish_reason: "stop"`. `usage` maps the tokens the agent reports (`prompt_tokens`, `completion_tokens`, `total_tokens`) and is left out when the agent reports none. The `X-Hashi-Request-Id` header carries the Hashi request id.
- **`stream: true`** answers with server-sent events: a role chunk, then `: queued` or `: running` comment lines every second while the request waits or runs, then one chunk with the whole answer, a final chunk with `finish_reason`, and `data: [DONE]`. Agents do not stream token by token. With `stream_options.include_usage` a usage chunk comes before `[DONE]`. A failed run sends `data: {"error": {...}}` and closes the stream. A stream counts as one of the token's 4 open long polls; a fifth answers `429 too_many_waits`.
- **Time limit.** Without streaming the request is held for up to 120 seconds (set a shorter `wait`, in seconds, if you like). If the answer is not ready by then you get `202` with the Hashi request `id` and a `poll` path: read it with `GET /v1/ask/{id}`, or send `stream: true` next time. A token that already holds 4 long polls gets the `202` at once.
- **Ignored.** `temperature`, `max_tokens`, `tools` and other unknown fields are accepted and ignored. Tool calling is not supported.
- **`GET /v1/models`** lists the agents this token may use that are installed on this machine.
- **Errors** use OpenAI's shape, with the status codes and `code` values listed under [Errors](#errors): `{"error": {"message": "...", "type": "invalid_request_error", "param": null, "code": "model_not_found"}}`. `type` is `invalid_request_error`, `authentication_error`, `permission_error`, `not_found_error`, `rate_limit_error` or `server_error`. A failed run answers `502` with code `agent_failed`.

With `curl`:

```bash
curl http://127.0.0.1:7878/v1/chat/completions \
  -H "Authorization: Bearer hashi_..." \
  -H "Content-Type: application/json" \
  -d '{"model": "claude", "messages": [{"role": "user", "content": "Write a haiku about bridges"}]}'
```

With the `openai` Python package:

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:7878/v1", api_key="hashi_...")

reply = client.chat.completions.create(
    model="claude",
    messages=[{"role": "user", "content": "List three uses for a bridge"}],
)
print(reply.choices[0].message.content)

stream = client.chat.completions.create(
    model="codex",
    messages=[{"role": "user", "content": "Explain src/main.rs"}],
    stream=True,
)
for chunk in stream:
    if chunk.choices:
        print(chunk.choices[0].delta.content or "", end="")
```

With Open WebUI: open **Admin Settings**, then **Connections**, and add an OpenAI API connection with the URL `http://127.0.0.1:7878/v1` (or your public URL followed by `/v1`) and your `hashi_...` token as the key. The agents this token may use show up as models. When Open WebUI runs in Docker on the same machine, use `http://host.docker.internal:7878/v1` and start Hashi with `--allowed-host host.docker.internal` (on Linux also with a `--bind` address the container can reach). Open WebUI sends the whole conversation with every message, and a prompt is capped at 32 KB, so start a new chat when a long one is refused.

## `POST /v1/tokens`

| Field | Type | Default |
|---|---|---|
| `name` | string, 1 to 100 chars | required |
| `role` | `"user"` or `"admin"` | `"user"` |
| `sessionMode` | `"stateless"` or `"stateful"` | `"stateless"` |
| `agents` | array of agent names | all |
| `expiresInDays` | 1 to 3650 | never expires |
| `note` | string, up to 200 chars | none |
| `limits` | object, see below | `maxPending: 3` for `user` tokens, none for admins |

The response contains the raw `token` exactly once. Only its SHA-256 hash is stored.

`limits` takes `dailyRequests`, `monthlyRequests`, `dailyBudgetUsd`, `monthlyBudgetUsd` and `maxPending`. Days and months are UTC. Spend uses the cost the agent reports, which only Claude Code and opencode do, so budgets bound only those two.

`PATCH /v1/tokens/{id}` accepts `name`, `note`, `paused`, `expiresInDays` (from now; `0` removes the expiry) and `limits`. A `limits` object replaces the whole set, except `maxPending`, which keeps its value unless you send it (`null` clears it).

## `POST /v1/invites`

Accepts `label`, `sessionMode`, `agents`, `expiresInDays` (counted from the claim), `limits` and `linkHours` (`1`, `24` or `168`, default `24`). It answers with the one-time `url`, and `localOnly: true` when no public URL is configured. At most 100 invites can be pending at once.

## MCP

`POST /mcp` is an MCP server. It offers one `ask_<agent>` tool for every agent the token may use (for example `ask_claude`, `ask_gemini`), `get_result` for long runs, `cancel` to stop a queued or running request by id (the token's own, or any for an admin), and `my_usage`. Limits and pause apply exactly as they do over HTTP. To add it to Claude Code:

```bash
claude mcp add --transport http hashi https://your-host/mcp --header "Authorization: Bearer hashi_…"
```

## Errors

Errors share one shape: `{"error": {"code": "rate_limited", "message": "..."}}`.

| Status | Code | When |
|---|---|---|
| 400 | `bad_request` | Missing or invalid fields, unknown agent, unsafe model name |
| 400 | `model_not_found`, `unsupported_content`, `unsupported_role` | OpenAI endpoint: `model` names no agent, a message has non-text content, or a `tool` or `function` message was sent |
| 401 | `unauthorized` | Missing, unknown or expired token |
| 403 | `forbidden` | Admin required, or the token may not use that agent |
| 403 | `token_paused` | The token is paused by the owner |
| 404 | `not_found` | Unknown request, or a request owned by another token |
| 404 | `invite_not_found` | Unknown or cancelled invite link |
| 405 | `method_not_allowed` | `GET /mcp` |
| 409 | `conflict` | Revoking the last admin token, pausing your own token, the token limit is reached, too many invites are pending, or cancelling a request that already finished |
| 409 | `already_claimed` | The invite link was already used |
| 410 | `invite_expired` | The invite link is past its expiry |
| 413 | | Body larger than 64 KB |
| 421 | `unknown_host` | Plain HTTP request with a `Host` Hashi does not serve (see `--allowed-host`) |
| 429 | `limit_reached` | A daily or monthly cap was hit; `limit` and `resetAt` say which and until when |
| 429 | `too_many_pending` | The token already has `maxPending` prompts in flight |
| 429 | `too_many_waits` | OpenAI endpoint: the token already holds 4 open long polls or streams |
| 429 | `rate_limited` | Per-token rate limit, full queue, or too many failed token or invite attempts from one address |
| 500 | `internal` | A bug in Hashi. The error also carries an `errorId` to quote in a bug report |
| 502 | `agent_failed` | OpenAI endpoint: the agent run failed or was cancelled; `message` says why |
| 503 | `unavailable` | That agent is not installed on this machine |

## From code

```python
import requests

HASHI = "http://127.0.0.1:7878"
HEADERS = {"Authorization": "Bearer hashi_..."}

r = requests.post(f"{HASHI}/v1/ask", headers=HEADERS,
                  json={"message": "List three uses for a bridge", "agent": "codex", "wait": 90})
print(r.json()["response"])
```

```ts
const res = await fetch("http://127.0.0.1:7878/v1/ask", {
  method: "POST",
  headers: { Authorization: "Bearer hashi_...", "Content-Type": "application/json" },
  body: JSON.stringify({ message: "Summarise README.md", wait: 90 }),
});
const { status, response, error } = await res.json();
```
