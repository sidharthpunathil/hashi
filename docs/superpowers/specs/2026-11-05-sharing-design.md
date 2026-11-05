# Sharing Hashi safely: design

## Intent

Sharing Hashi means lending two things: your Claude Code / Codex subscription and
your computer. Today the only way to share is to mint a token and paste the raw
secret into a chat, with no way to cap use, pause access or see who did what.
This design makes sharing with a person, an app or another AI agent a deliberate,
bounded and visible act.

## Goals

1. Share with a person **without sending a secret**: a one-time invite link.
2. **Bound** every token: request caps and estimated spend per UTC day and month,
   and a cap on in-flight requests so one sharer cannot hog the queue.
3. **Pause, resume and extend** a token without recreating it.
4. Let a sharer **see their own access**: `GET /v1/me` and a `/me` page.
5. Let a sharer plug Hashi **into their own agent** over MCP.
6. Keep an **audit log** of every sharing action.
7. Invite links that **survive restarts** when a stable URL is configured.

## Non-goals

- Webhooks, streaming, server-to-server pairing, email delivery, user accounts.
- Per-token sandbox or permission overrides (shared tokens always get the
  server's settings, which default to read-only).

## Anti-goals (a failure even if it works)

- Any path that reveals the agent's own credentials or the raw secret twice.
- A shared token that can loosen the sandbox or reach admin endpoints.
- Weakening the loopback-first default or the existing token hashing.

## Design

### Tokens gain policy and status

New columns on `tokens`: `paused`, `note` (who it is for), `daily_requests`,
`monthly_requests`, `daily_budget_usd`, `monthly_budget_usd`, `max_pending`.
`NULL` means unlimited. Status is derived: `expired` (past `expires_at`),
`paused`, or `active`.

Schema changes are applied by a versioned migration (`PRAGMA user_version`):
version 1 is the 0.1.x schema, version 2 adds the columns above plus the
`invites` and `audit` tables. Existing databases upgrade in place.

### Enforcement, in `POST /v1/ask` and MCP `tools/call`

Both go through one `submit()` function, checked in this order:

1. Paused token: 403 `token_paused`.
2. Agent not allowed, bad input, agent missing: unchanged.
3. `max_pending` reached (queued + running for this token): 429 `too_many_pending`.
4. Request caps (count of this token's requests since the start of the UTC
   day / month): 429 `limit_reached` with `limit` and `resetAt`.
5. Budget caps (sum of reported `cost_usd` in the period): same error.
   Only Claude reports a cost, so budgets bound Claude spend; the docs say so.
6. Global queue limit: unchanged.

Paused tokens may still read their own results and `GET /v1/me`. Expired
tokens may call only `GET /v1/me` (to see why they stopped).

### Admin API additions

- `PATCH /v1/tokens/{id}`: `paused`, `note`, `expiresInDays` (from now, or `0`
  to clear), and the five limits (`null` clears one).
- `POST /v1/invites` `{label, sessionMode, agents, expiresInDays, limits,
  linkHours: 1|24|168}` returns `{id, url, expiresAt}` once. Only the SHA-256 of
  the code is stored. `GET /v1/invites` lists pending, claimed and expired
  invites; `DELETE /v1/invites/{id}` cancels a pending one.
- `GET /v1/audit?limit=N`.

Invited tokens are always role `user`.

### Claiming an invite (no auth; the code is the credential)

- `GET /claim/{code}` serves a static page. It reads the code from the URL,
  calls `POST /v1/claim/preview {code}` (what is shared, by whom, until when,
  limits, and a plain notice that the owner can see prompts and answers), then
  on "Get my access" calls `POST /v1/claim/redeem {code}`.
- Redeem burns the invite and creates the token in one transaction. A second
  redeem gets 409 `already_claimed`, an expired link 410 `invite_expired`,
  an unknown one 404.
- The redeem answer carries the token once, the base URL, ready snippets
  (curl, Python, TypeScript) and MCP setup (Claude Code command and JSON config).
- `GET /me` serves a page where a person pastes a token to see `/v1/me`. The
  token is kept in `sessionStorage` only when they tick "remember".
- Pages are static files compiled into the binary, served with a strict CSP
  (`default-src 'none'`, scripts and styles from `'self'` only, `connect-src
  'self'`), `no-referrer` and `no-store`, so the code never leaks via Referer
  or cache.

### `GET /v1/me`

Any token, including paused and expired ones:
`{name, note, role, status, sessionMode, agents, expiresAt, limits, usage:
{todayRequests, monthRequests, todayCostUsd, monthCostUsd, pending}, server}`.

### MCP at `POST /mcp`

Streamable HTTP with plain JSON responses, bearer auth, JSON-RPC 2.0.
`initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`.
Tools, filtered by the token's allowed agents: `ask_claude`, `ask_codex`
(`{prompt, model?}`), `get_result` (`{id}`) and `my_usage`. `ask_*` submits
through `submit()` and waits up to the server timeout; if the run is still
going it returns the request id so the client can call `get_result`.
`GET /mcp` answers 405. Limits and pause apply exactly as for HTTP.

### Links and identity

`--public-url` / `HASHI_PUBLIC_URL` sets the base for invite links; otherwise
the tunnel URL, otherwise the local URL (and the invite response says
`localOnly: true`). `--name` / `HASHI_NAME` labels the server on the claim page.

### Audit

`audit(id, at, actor, action, target, detail)`. Actions: `token.create`,
`token.update`, `token.revoke`, `invite.create`, `invite.cancel`,
`invite.claim`, `session.reset`. `actor` is the token name, `cli` or `dashboard`.
Also shown in the dashboard log. `hashi audit` prints it.

### Dashboard

Tokens view: status and today's usage per token, `s` creates an invite (label,
agents, session, request cap, budget, lifetime, link validity) and shows the
link with copy, `p` pauses or resumes, and pending invites are listed with `x`
to cancel.

### CLI

`hashi share <label> [--agents ..] [--daily-requests N] [--daily-budget USD]
[--expires-in-days N] [--link-hours 24]` prints the link.
`hashi token update <id> [--pause|--resume] [limits]`. `hashi audit`.

## Calls made by the builder (not discussed with the owner)

- Day and month windows are UTC.
- Requests count toward caps whether they succeed or fail.
- Invite links default to 24 hours; invited tokens default to stateless,
  both agents, no expiry, `max_pending` 3.
- MCP `ask_*` waits at most the server's per-request timeout.
