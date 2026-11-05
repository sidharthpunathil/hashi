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

A `stateful` token resumes its own conversation with each agent on every prompt. `DELETE /v1/session/{agent}` starts it over, on every account.

For an admin token, the result also has `account`: the account that ran the prompt (`default` for your own login). Other tokens never see it.

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

The response contains the raw `token` exactly once. Only its SHA-256 hash is stored.

`accounts` looks like `{"claude": ["work", "personal"], "codex": ["ci"]}`. Each name must be an account added with `hashi account add`, or `default` for your own login; an unknown name is refused with `400`. A run uses the first listed account that is enabled and not at its limit, and fails over to the next one when it hits the agent's limit. Token listings and `PATCH` responses show `accounts` to admins; `/v1/me`, MCP and the claim page never do.

`limits` takes `dailyRequests`, `monthlyRequests`, `dailyBudgetUsd`, `monthlyBudgetUsd` and `maxPending`. Days and months are UTC. Spend uses the cost the agent reports, which only Claude Code and opencode do, so budgets bound only those two.

`PATCH /v1/tokens/{id}` accepts `name`, `note`, `paused`, `expiresInDays` (from now; `0` removes the expiry), `limits` and `accounts`. A `limits` object replaces the whole set, except `maxPending`, which keeps its value unless you send it (`null` clears it). An `accounts` object replaces the whole map; `{}` puts every agent back on `default`.

## `POST /v1/invites`

Accepts `label`, `sessionMode`, `agents`, `expiresInDays` (counted from the claim), `limits`, `linkHours` (`1`, `24` or `168`, default `24`) and `accounts` (as for tokens). It answers with the one-time `url`, and `localOnly: true` when no public URL is configured. At most 100 invites can be pending at once.

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

`POST /mcp` is an MCP server. It offers one `ask_<agent>` tool for every agent the token may use (for example `ask_claude`, `ask_gemini`), `get_result` for long runs and `my_usage`. Limits and pause apply exactly as they do over HTTP. To add it to Claude Code:

```bash
claude mcp add --transport http hashi https://your-host/mcp --header "Authorization: Bearer hashi_…"
```

## Errors

Errors share one shape: `{"error": {"code": "rate_limited", "message": "..."}}`.

| Status | Code | When |
|---|---|---|
| 400 | `bad_request` | Missing or invalid fields, unknown agent, unsafe model name |
| 401 | `unauthorized` | Missing, unknown or expired token |
| 403 | `forbidden` | Admin required, or the token may not use that agent |
| 403 | `token_paused` | The token is paused by the owner |
| 404 | `not_found` | Unknown request, or a request owned by another token |
| 404 | `invite_not_found` | Unknown or cancelled invite link |
| 405 | `method_not_allowed` | `GET /mcp` |
| 409 | `conflict` | Revoking the last admin token, pausing your own token, the token limit is reached, or too many invites are pending |
| 409 | `already_claimed` | The invite link was already used |
| 410 | `invite_expired` | The invite link is past its expiry |
| 413 | | Body larger than 64 KB |
| 421 | `unknown_host` | Plain HTTP request with a `Host` Hashi does not serve (see `--allowed-host`) |
| 429 | `limit_reached` | A daily or monthly cap was hit; `limit` and `resetAt` say which and until when |
| 429 | `too_many_pending` | The token already has `maxPending` prompts in flight |
| 429 | `rate_limited` | Per-token rate limit, full queue, or too many failed token or invite attempts from one address |
| 500 | `internal` | A bug in Hashi. The error also carries an `errorId` to quote in a bug report |
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
