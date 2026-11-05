# Access requests: design

## Intent

Today, sharing starts with the owner: they make an invite and send it. The
reverse should work too. A friend asks for access (or for more of it), the
owner sees the request, and accepts or declines it from the dashboard, the
CLI or the API. Accepting hands the friend their access with no link to copy
by hand.

## 1. Asking for access (new people)

- **Off by default.** The owner turns it on with `hashi requests open`
  (`--project <name>` and `--note "..."` optional, and `--hours N`, default
  168, after which it closes by itself), the dashboard, or `POST
  /v1/request-link`. That gives a public link: `<base>/ask-access/<link
  code>`. The link code is 128 bits, stored hashed, and can be closed with
  `hashi requests close`.
- **The page** (same CSP and style as the claim page) shows the bridge name
  and the owner's note. It asks for a name (1 to 60 characters), a message
  (up to 500 characters) and the agents the friend wants (from the agents
  allowed by the link's project).
- **Submitting** returns a pickup secret once. It is 256 bits, stored hashed,
  and the browser keeps it in sessionStorage, the same way the claim page
  handles its token. The friend's page then polls the request's status with
  that secret.
- **From the terminal:** `hashi request <link>` does the same from another
  Hashi or a friend's terminal. It keeps the pickup secret in an owner-only
  `pending-requests.json`, and `hashi request status` checks it. When the
  request is accepted, it saves the remote exactly as `hashi connect` does.
- **Abuse limits:**
  - per-address rate limit (the existing claim limiter shape);
  - at most 50 pending requests per link, and 200 in total;
  - requests expire after 7 days;
  - every field is length-limited and stripped of control and bidi
    characters, both when it is stored and when it is shown;
  - nothing is ever approved without the owner.

## 2. Asking for more (existing tokens)

- A token holder can ask for more from `/me` ("Ask for more" form), `POST
  /v1/me/requests`, or the MCP tool `request_more`. They can ask for higher
  daily or monthly requests or budget, more agents, a later expiry, or say it
  in free text.
- At most 3 open requests per token, at most one new one per hour, and the
  same field limits.

## 3. Deciding (owner)

- **Where requests show up:**
  - a dashboard Requests-for-access list, which the parity builder places,
    with a badge count in the tab bar and a toast when a new one arrives;
  - `hashi requests list`;
  - `GET /v1/access-requests` (admin).
- **Accepting a new-person request** opens the same form as `hashi share`.
  The friend's name, wanted agents and the link's project are filled in, and
  the owner sets the limits and expiry. Hashi then creates a single-use
  invite from it. The friend's polling page, or `hashi request status`,
  receives the claim URL and redeems it. If the friend never collects it,
  the invite simply expires.
- **Accepting a "more" request** shows the current limits next to what was
  asked. The owner edits the values and applies them, the same as `token
  update`. Hashi never widens access beyond what the owner sets.
- **Declining** takes an optional reason that the friend sees. Declined and
  expired requests are deleted after 30 days.
- Every request created, accepted or declined is written to the audit log.
- **CLI:** `hashi requests list|accept <id> [share flags]|decline <id>
  [--reason]|open|close`.

## 4. Privacy

- The request page never shows other people's requests or any account
  names.
- The owner sees the requester's address only as the existing audit records
  show it.

## Anti-goals

- Auto-accepting, or accepting rules.
- Email or push delivery. Delivery stays inside Hashi: the friend's poll and
  the owner's dashboard.
- Any request page that is open by default.
- Tests that use the network. Use local servers and the existing harness.
