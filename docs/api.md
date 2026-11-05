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
| `GET` | `/v1/accounts` | admin | Accounts per agent, their state and use |
| `GET` `POST` | `/v1/projects` | admin | List projects with their use, or add one |
| `PATCH` `DELETE` | `/v1/projects/{name}` | admin | Change or remove a project (never its folder) |
| `GET` `POST` `DELETE` | `/v1/request-link` | admin | The ask-access link: its state, open it, close it |
| `GET` | `/v1/access-requests` | admin | Requests for access and for more |
| `POST` | `/v1/access-requests/{id}/accept` | admin | Accept, with your values over the prefill |
| `POST` | `/v1/access-requests/{id}/decline` | admin | Decline, with an optional reason |
| `POST` | `/v1/ask-access/preview`, `/v1/ask-access` | link code | What the ask-access page shows; ask for access |
| `POST` | `/v1/ask-access/status` | pickup secret | A request's state, and its claim link once accepted |
| `GET` `POST` | `/v1/me/requests` | any token | The token's own requests for more; ask for more |
| `POST` | `/mcp` | token | MCP server (JSON-RPC over streamable HTTP). `GET /mcp` answers 405 |
| `GET` | `/claim/{code}`, `/ask-access/{code}`, `/me` | | Pages for the people you share with |

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

A `stateful` token resumes its own conversation with each agent on every prompt. `DELETE /v1/session/{agent}` starts it over, on every account.

For an admin token, the result also has `account`: the account that ran the prompt (`default` for your own login). Other tokens never see it.

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
| `accounts` | object, agent to an ordered list of account names | every agent uses `default` |
| `project` | a project name | none: the token's own workspace |

The response contains the raw `token` exactly once. Only its SHA-256 hash is stored.

A token with a `project` runs in the project's folder. It may use the agents both it and the project allow, and its `accounts` for an agent must be a subset of the project's list for that agent (`default` when the project has none), or the request is refused with `400`. A token without its own list uses the project's. Token listings show `project` to admins, and `/v1/me` shows the token holder only the name.

`accounts` looks like `{"claude": ["work", "personal"], "codex": ["ci"]}`. Each name must be an account added with `hashi account add`, or `default` for your own login; an unknown name is refused with `400`. A run uses the first listed account that is enabled and not at its limit, and fails over to the next one when it hits the agent's limit. Token listings and `PATCH` responses show `accounts` to admins; `/v1/me`, MCP and the claim page never do.

`limits` takes `dailyRequests`, `monthlyRequests`, `dailyBudgetUsd`, `monthlyBudgetUsd` and `maxPending`. Days and months are UTC. Spend uses the cost the agent reports, which only Claude Code and opencode do, so budgets bound only those two.

`PATCH /v1/tokens/{id}` accepts `name`, `note`, `paused`, `expiresInDays` (from now; `0` removes the expiry), `limits`, `accounts` and `project`. A `limits` object replaces the whole set, except `maxPending`, which keeps its value unless you send it (`null` clears it). An `accounts` object replaces the whole map; `{}` puts every agent back on `default`. `project` binds the token to a project, and `null` puts it back on its own workspace; the result is checked as on create.

## `POST /v1/invites`

Accepts `label`, `sessionMode`, `agents`, `expiresInDays` (counted from the claim), `limits`, `linkHours` (`1`, `24` or `168`, default `24`), `accounts` and `project` (as for tokens). It answers with the one-time `url`, and `localOnly: true` when no public URL is configured. At most 100 invites can be pending at once. `POST /v1/claim/preview` shows the project's name, never its folder or accounts.

## Access requests

Off by default. `POST /v1/request-link` (admin) opens the ask-access link and closes any open one. It accepts `project` (new requests are for it, and people pick from its agents), `note` (up to 200 characters, shown on the page) and `hours` (1 to 720, default 168, after which it closes by itself). It answers `201` with the `url`, `<base>/ask-access/hashi_ask_<32 hex>`, once: only a hash of the code is kept. `GET /v1/request-link` shows whether it is open, and `DELETE /v1/request-link` closes it.

The page, and `hashi request`, use three public endpoints. Codes go in the body, never the path:

```bash
curl -X POST https://host/v1/ask-access/preview -d '{"code": "hashi_ask_…"}'
# {"server": "Sid's Hashi", "note": "...", "project": null, "agents": ["claude", "codex"], "closesAt": "...", ...}
curl -X POST https://host/v1/ask-access -d '{"code": "hashi_ask_…", "name": "Alice", "message": "for the docs", "agents": ["claude"]}'
# 201 {"id": "...", "status": "pending", "pickup": "hashi_pick_<64 hex>", "expiresAt": "..."}
curl -X POST https://host/v1/ask-access/status -d '{"pickup": "hashi_pick_…"}'
# {"status": "accepted", "claimCode": "hashi_inv_…", "claimUrl": "https://host/claim/hashi_inv_…", ...}
```

`name` is 1 to 60 characters, `message` up to 500, and `agents` must be among the link's (left out, all of them). Control, bidi and zero-width characters are removed before the lengths are checked, and again whenever a request is shown. The pickup secret is shown once and stored as a hash. `status` is `pending`, `accepted` (with the claim code and URL), `claimed`, `declined` (with the owner's `reason`) or `expired`. A closed or expired link answers `410 link_closed`, an unknown one `404 link_not_found`, a full link `409 link_full` and a full server `409 requests_full`.

A token asks for more with `POST /v1/me/requests` (any token, even paused or expired; not admin tokens): `dailyRequests`, `monthlyRequests`, `dailyBudgetUsd`, `monthlyBudgetUsd`, `agents` (on top of the current ones), `expiresInDays` and `message`, all optional but at least one. A token may hold 3 open requests (`409 too_many_open_requests`) and open one an hour (`429 request_too_soon`). `GET /v1/me/requests` lists its own requests and their state.

`GET /v1/access-requests` (admin) lists every request, newest first, with `kind` (`new` or `more`), `status`, `name`, `message`, `agents`, `project`, `asked`, a one-line `summary` and, while pending, a `prefill`: the values accepting would use. For `more` the prefill has `current`, the token's limits, agents and expiry now, and `limits` with the asked values over them. Never a secret, an address or an account name of the requester.

`POST /v1/access-requests/{id}/accept` takes an optional body. Every field left out takes the prefilled value:

- For a new person: `label`, `sessionMode`, `agents`, `expiresInDays`, `limits`, `linkHours` (`1`, `24` or `168`, default `168`), `accounts` and `project`, as on `POST /v1/invites`. It creates a single-use invite and answers with it, but never with its link: the code is derived from the requester's pickup secret, so only they can collect it.
- For more: `agents`, `limits` (each field given replaces the prefilled one) and `expiresInDays` (`0` removes the expiry). It saves the token as `PATCH /v1/tokens/{id}` would and answers with the `record` and `changes`. The new-person fields are refused.

The token's project is checked the same way as everywhere else, so accepting never takes access outside a project. `POST /v1/access-requests/{id}/decline` takes an optional `reason` (up to 300 characters) that the requester sees. A request that is not pending answers `409 not_pending`. Decided and expired requests are deleted 30 days later. Every link change, request, acceptance and decline is in `GET /v1/audit` (`access.link.open`, `access.link.close`, `access.request`, `access.accept`, `access.decline`).

## Projects

Admin only. A project is a folder with its own agents, accounts and defaults that tokens and invites can be bound to.

`POST /v1/projects` takes:

| Field | Type | Default |
|---|---|---|
| `name` | 1 to 32 characters from `a-z`, `0-9` and `-`; `default` is reserved | required |
| `path` | absolute path of an existing folder, outside the Hashi home | required |
| `agents` | array of agent names | the installed agents |
| `accounts` | object, agent to an ordered list of account names | every agent uses `default` |
| `defaultAgent` | an agent the project allows | the server's default agent |
| `models` | object, agent to a model name | none |
| `note` | string, up to 200 chars | none |

The path is resolved through symlinks and stored as the real folder. A missing folder, a file, the file system root, a folder inside the Hashi home, an unknown account and a model with unsafe characters are refused with `400`; a taken name with `409`. At most 100 projects can exist.

`GET /v1/projects` lists them:

```json
{
  "projects": [
    {
      "name": "api", "path": "/Users/me/code/api", "agents": ["claude", "codex"],
      "accounts": { "claude": ["work"] }, "defaultAgent": "claude",
      "models": { "claude": "sonnet" }, "note": null,
      "createdAt": "2026-11-06T09:00:00Z", "tokens": 2,
      "stats": {
        "todayRequests": 4, "monthRequests": 31,
        "todayCostUsd": 0.12, "monthCostUsd": 1.9, "tokens": 2
      }
    }
  ]
}
```

`stats` counts the requests sent by tokens bound to the project and their reported spend, today and this month (UTC). `PATCH /v1/projects/{name}` accepts `path`, `agents`, `accounts`, `defaultAgent` (`null` removes it), `models` and `note`, checked as on create. An edit that would leave a bound token or pending invite with accounts outside the project answers `409 project_in_use`. `DELETE /v1/projects/{name}` answers `409 project_in_use` while tokens or pending invites are bound; `?unbind=true` puts them back on their own workspace first. Neither ever touches the folder.

When a request names no `model`, the project's model for that agent is used, and when it names no `agent`, the project's `defaultAgent`. `GET /v1/ask/{id}` shows the `project` a request ran in.

## `GET /v1/accounts`

Admin only. Lists every account with its state and use; keys and login files are never part of it.

```json
{
  "accounts": [
    {
      "agent": "claude", "name": "work", "kind": "login",
      "state": "limited", "limitedUntil": "2026-11-06T15:00:00Z",
      "lastUsed": "2026-11-06T14:02:11Z", "createdAt": "2026-11-01T09:00:00Z",
      "runsToday": 12, "runs": 140, "tokens": 3
    }
  ],
  "perAgent": { "claude": 1, "codex": 0, "gemini": 0, "opencode": 0, "qwen": 0, "cursor": 0 }
}
```

`kind` is `login`, `api-key`, `oauth-token` or `default` (your own login, listed once a token that names it has used it). `state` is `ready`, `limited` (with `limitedUntil`) or `disabled`. `tokens` counts the tokens whose list includes the account, and `perAgent` the accounts each agent has besides `default`.

## MCP

`POST /mcp` is an MCP server. It offers one `ask_<agent>` tool for every agent the token may use (for example `ask_claude`, `ask_gemini`), `get_result` for long runs, `cancel` to stop a queued or running request by id (the token's own, or any for an admin), `my_usage`, and `request_more`, which asks the owner for more with the fields of `POST /v1/me/requests`. Limits and pause apply exactly as they do over HTTP. To add it to Claude Code:

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
