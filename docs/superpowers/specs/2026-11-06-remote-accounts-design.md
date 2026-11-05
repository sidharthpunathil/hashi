# Pooling what friends share: design

## Intent

Sharing goes both ways. A friend can already invite the owner to their
bridge (`hashi share` on their side, `hashi connect` on the owner's). This
design lets the owner use that shared access as one of their own accounts,
so it joins the owner's account lists, projects, failover and strategies.
Friends can also choose to let the people they share with see how much
headroom the shared access has left, so the owner's strategies can use it.

## 1. Remote accounts

- A new account kind, `remote`, for any agent. It points at a saved remote
  (from `remotes.json`) and the agent on that bridge:
  `hashi account add claude bob-claude --remote bob [--remote-agent claude]`.
- A run on a remote account sends the prompt to the remote bridge with
  `POST /v1/ask` and `wait` (using the remote's token). It polls until the
  request is done or fails, and returns the answer as this run's result.
  Usage and cost come from the remote response.
- **Timeouts and cancel:** the local run timeout applies. Cancelling locally
  sends `DELETE /v1/ask/{id}` to the remote, best effort.
- **Mapping remote failures:**
  - `limit_reached`, `too_many_pending` and `rate_limited` mean the account
    is limited, until `resetAt` when given, otherwise for the cooldown.
  - `token_paused`, `unauthorized` (revoked or expired) and `forbidden` mean
    the account is disabled, with the reason shown to the owner.
  - Network errors fail the run as a retryable failure: failover moves to the
    next account.
- **Sessions:** a stateful conversation on a remote account uses the
  remote's own stateful session (the remote token's session mode decides).
  Hashi does not resume conversations across accounts.
- **Privacy:** the owner is told when adding the account that the friend can
  see every prompt sent through it, as the friend's claim page also says.
  People the owner shares with never see that a remote account exists, just
  as they never see account names.
- **Loops:** a remote account may not point at this same bridge (compare the
  remote's `/v1/status` instance id with our own). Hashi also adds an
  `X-Hashi-Hops` header, refusing forwarded requests past 2 hops, so a chain
  of bridges cannot loop forever.

## 2. Shared headroom (opt-in, on the friend's side)

- A new token and invite flag, `shareUsage` (default off), set by the owner of
  the bridge that issues the token. The CLI flag is `--share-usage`, and the
  dashboard forms have a toggle.
- When it is on, `GET /v1/me` for that token adds `usage`: for each agent the
  token may use, the headroom summary of the accounts serving it. That is
  the percent left on the binding window, the next reset, and the state.
  Account names, the number of accounts and the raw windows are never
  included.
- The receiving Hashi reads this from `/v1/me` on its usage polling cadence.
  It records it as the remote account's usage (source `remote`), so
  `most-left`, `soonest-reset` and `--rotate-at` treat the remote account
  like a local one.
- Without `shareUsage`, a remote account shows only the token's own limits
  from `/v1/me` (daily and monthly requests and budget left). When those run
  out, the account counts as limited until their reset.

## 3. Visibility

- `hashi account list` and `usage` show remote accounts with the bridge name
  and their source.
- The dashboard shows them like other accounts, marked "via bob".
- The claim page on the friend's side says when usage sharing is on: "the
  bridge owner lets you see how much quota is left".

## Anti-goals

- Forwarding to a remote without the owner adding it as an account.
- Exposing a remote's token in logs, reports or API responses.
- Tests that use the network. Use two local Hashi test servers.
