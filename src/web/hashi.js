// Claim, "Ask for access" and "My access" pages. All dynamic text goes through textContent and
// is never parsed as HTML, so nothing from the server can inject markup.
"use strict";

const $ = (id) => document.getElementById(id);
const show = (id, on = true) => { $(id).hidden = !on; };

async function api(path, { method = "GET", body, token } = {}) {
  const headers = { "Content-Type": "application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;
  const res = await fetch(path, { method, headers, body: body && JSON.stringify(body), cache: "no-store" });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    const err = new Error(data?.error?.message || `Request failed (${res.status})`);
    err.status = res.status;
    err.code = data?.error?.code;
    throw err;
  }
  return data;
}

// What one pickup-status check got back: either the request's `state`, or
// an `error` with the HTTP `status` and `code` when the server sent one.
// A network failure has an error and no status.
const FINAL_STATES = ["declined", "claimed", "expired"];

// Whether a pickup-status check is a definite answer, so the pickup secret
// can be forgotten: the server does not know the secret (404
// request_not_found), or the request was declined, collected or has
// expired. A network error, a 5xx or a 429 is not: the secret is kept and
// the check is tried again (see retryDelay). The ask page forgets the
// secret on its own only through this function.
function isFinalAnswer(answer) {
  if (answer.error) return answer.status === 404 && answer.code === "request_not_found";
  return FINAL_STATES.includes(answer.state);
}

// Whether a failed pickup-status check is worth trying again: no answer at
// all, a server error or a rate limit.
function isRetryable(answer) {
  return Boolean(answer.error) && (answer.status == null || answer.status >= 500 || answer.status === 429);
}

// How long to wait before the next check after `failures` failed ones in a
// row: 10 s, 20 s, 40 s, then 60 s.
function retryDelay(failures) {
  return Math.min(10000 * 2 ** Math.max(0, failures - 1), 60000);
}

const AGENT_NAMES = {
  claude: "Claude Code",
  codex: "Codex",
  gemini: "Gemini CLI",
  opencode: "opencode",
  qwen: "Qwen Code",
  cursor: "Cursor CLI",
  crush: "Crush",
};
const agentName = (a) => AGENT_NAMES[a] || a;

// "Claude Code, Codex and Gemini CLI", in the reader's language.
function agentsText(agents) {
  const names = (agents || []).map(agentName);
  try {
    return new Intl.ListFormat(undefined, { style: "long", type: "conjunction" }).format(names);
  } catch {
    return names.join(", ");
  }
}

// Fills a list with one chip per agent, or says none is available.
function renderAgents(list, agents) {
  list.replaceChildren();
  for (const a of agents || []) {
    const li = document.createElement("li");
    li.className = "chip";
    li.textContent = agentName(a);
    list.append(li);
  }
  if (!list.children.length) {
    const li = document.createElement("li");
    li.className = "muted";
    li.textContent = "None installed right now";
    list.append(li);
  }
}

const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function limitsText(l) {
  const parts = [];
  if (l?.dailyRequests != null) parts.push(`${l.dailyRequests} prompts a day`);
  if (l?.monthlyRequests != null) parts.push(`${l.monthlyRequests} a month`);
  if (l?.dailyBudgetUsd != null) parts.push(`$${l.dailyBudgetUsd} a day`);
  if (l?.monthlyBudgetUsd != null) parts.push(`$${l.monthlyBudgetUsd} a month`);
  if (l?.maxPending != null) parts.push(`${l.maxPending} at a time`);
  return parts.length ? parts.join(", ") : "No limits";
}

// "in 23 hours (Sat, Nov 7, 12:17 PM)" or "3 minutes ago (...)": the
// relative part answers "how long", the date part "exactly when".
function when(iso) {
  if (!iso) return "Never";
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "Unknown";
  const exact = at.toLocaleString(undefined, {
    weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit",
  });
  try {
    const secs = (at.getTime() - Date.now()) / 1000;
    const units = [["year", 31536000], ["month", 2592000], ["week", 604800], ["day", 86400], ["hour", 3600], ["minute", 60]];
    const [unit, size] = units.find(([, n]) => Math.abs(secs) >= n) || ["minute", 60];
    const rel = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" }).format(Math.round(secs / size), unit);
    return `${rel} (${exact})`;
  } catch {
    return exact;
  }
}

const sentence = (text) => (text ? text.charAt(0).toUpperCase() + text.slice(1) : text);

async function copy(text, button) {
  try {
    await navigator.clipboard.writeText(text);
    button.textContent = "Copied";
  } catch {
    // Clipboard needs a secure context; fall back to selecting the text.
    const range = document.createRange();
    range.selectNodeContents(button.previousElementSibling);
    getSelection().removeAllRanges();
    getSelection().addRange(range);
    button.textContent = "Press Ctrl+C";
  }
  setTimeout(() => { button.textContent = "Copy"; }, 1800);
}

// The terminal setup commands put the invite link inside single quotes. Only
// an origin and a code of exactly these shapes are used, so nothing in the
// link can close the quotes early in sh or PowerShell. Plain http is only
// offered for loopback, matching what `hashi connect` accepts.
const SAFE_ORIGIN = /^(?:https:\/\/(?:[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?|\[[0-9A-Fa-f:.]+\])|http:\/\/(?:localhost|127(?:\.[0-9]{1,3}){3}|\[::1\]))(?::[0-9]{1,5})?$/;
const SAFE_CODE = /^hashi_inv_[0-9a-f]{16,118}$/;
const INSTALLERS = "https://raw.githubusercontent.com/sidharthpunathil/hashi/main";

function setupCommands(origin, code) {
  if (!SAFE_ORIGIN.test(origin) || !SAFE_CODE.test(code)) return null;
  const link = `${origin}/claim/${code}`;
  return {
    sh: `curl -fsSL ${INSTALLERS}/install.sh | HASHI_CONNECT='${link}' sh`,
    ps: `$env:HASHI_CONNECT='${link}'; irm ${INSTALLERS}/install.ps1 | iex`,
  };
}

// Only the project's name is ever sent to the people you share with.
function showProject(name) {
  if (typeof name === "string" && name) $("project").textContent = `Works in the project ${name}`;
  const on = typeof name === "string" && name !== "";
  show("project-label", on);
  show("project", on);
}

function claimPage() {
  const code = decodeURIComponent(location.pathname.split("/claim/")[1] || "");
  const fail = (msg) => {
    show("loading", false);
    show("preview", false);
    show("done", false);
    $("title").textContent = "This link doesn't work";
    show("server", false);
    $("error-text").textContent = sentence(msg);
    show("error");
  };

  api("/v1/claim/preview", { method: "POST", body: { code } })
    .then((p) => {
      $("server").textContent = `to ${p.server}`;
      $("label").textContent = p.label;
      renderAgents($("agents"), p.agents);
      showProject(p.project);
      $("session").textContent = p.sessionMode === "stateful" ? "Remembers your conversation" : "Each prompt starts fresh";
      $("limits").textContent = limitsText(p.limits);
      $("lifetime").textContent = p.tokenExpiresInDays ? `${plural(p.tokenExpiresInDays, "day")} after you claim it` : "Until the owner revokes it";
      $("link-expires").textContent = when(p.linkExpiresAt);
      $("notice").textContent = p.notice;
      show("share-usage", p.shareUsage === true);
      const setup = setupCommands(location.origin, code);
      if (setup) {
        $("setup-sh").textContent = setup.sh;
        $("setup-ps").textContent = setup.ps;
        show("terminal");
      }
      show("loading", false);
      show("preview");
    })
    .catch((e) => fail(e.message));

  $("claim").addEventListener("click", async () => {
    $("claim").disabled = true;
    try {
      const r = await api("/v1/claim/redeem", { method: "POST", body: { code } });
      // The code is spent; keep it out of the address bar and history.
      history.replaceState(null, "", "/claim/used");
      $("token").textContent = r.token;
      $("me-link").href = r.meUrl;
      const links = r.installLinks || {};
      const cursorOk = typeof links.cursor === "string" && links.cursor.startsWith("cursor://anysphere.cursor-deeplink/mcp/install?");
      const vscodeOk = typeof links.vscode === "string" && links.vscode.startsWith("vscode:mcp/install?");
      if (cursorOk) $("cursor-link").href = links.cursor;
      if (vscodeOk) $("vscode-link").href = links.vscode;
      show("cursor-link", cursorOk);
      show("vscode-link", vscodeOk);
      show("install-links", cursorOk || vscodeOk);
      const select = (name) => {
        document.querySelectorAll("[data-tab]").forEach((b) => b.setAttribute("aria-selected", String(b.dataset.tab === name)));
        $("snippet").textContent = r.snippets[name];
      };
      document.querySelectorAll("[data-tab]").forEach((b) => b.addEventListener("click", () => select(b.dataset.tab)));
      select("curl");
      $("title").textContent = "You're in";
      show("preview", false);
      show("done");
      $("title").focus?.();
    } catch (e) {
      fail(e.message);
    }
  });
}

// The ask-access page. The pickup secret is shown by the server once and is
// kept in sessionStorage, like the token on "My access", so only this tab
// can collect the answer.
function askPage() {
  const KEY = "hashi.pickup";
  const code = decodeURIComponent(location.pathname.split("/ask-access/")[1] || "");
  const fail = (msg) => { show("loading", false); show("ask", false); $("error-text").textContent = sentence(msg); show("error"); };
  const forget = () => { try { sessionStorage.removeItem(KEY); } catch { /* ignore */ } };
  let timer = null;

  const render = (s) => {
    const text = $("status-text");
    const detail = $("status-detail");
    detail.textContent = "";
    show("claim-link", false);
    show("status-hint", s.status === "pending");
    if (typeof s.server === "string") $("server").textContent = `to ${s.server}`;
    if (s.status === "pending") {
      text.className = "";
      text.textContent = "Your request is waiting for the owner.";
      detail.textContent = `It expires ${when(s.expiresAt)} if nobody answers.`;
    } else if (s.status === "accepted" && typeof s.claimCode === "string" && SAFE_CODE.test(s.claimCode)) {
      text.className = "good";
      text.textContent = "✓ The owner said yes.";
      detail.textContent = "Your access is ready. The link below works once.";
      $("claim-link").href = `/claim/${s.claimCode}`;
      show("claim-link");
    } else if (s.status === "declined") {
      text.className = "bad";
      text.textContent = "The owner declined this request.";
      if (typeof s.reason === "string" && s.reason) detail.textContent = `Their reason: ${s.reason}`;
    } else if (s.status === "claimed") {
      text.className = "good";
      text.textContent = "This access was already collected.";
    } else {
      text.className = "bad";
      text.textContent = "This request has expired. Ask the owner for a new link.";
    }
    show("loading", false);
    show("ask", false);
    show("status");
  };

  // Shown while a check failed and the next one is waiting. The secret is
  // kept, so a dropped connection or a restart of the server loses nothing.
  const retrying = () => {
    show("loading", false);
    show("ask", false);
    if ($("status").hidden) {
      $("status-text").className = "";
      $("status-text").textContent = "Checking your request…";
      show("claim-link", false);
      show("status");
    }
    $("status-detail").textContent = "Could not reach this Hashi; retrying…";
  };

  let failures = 0;
  const poll = async (pickup) => {
    clearTimeout(timer);
    let answer;
    try {
      const s = await api("/v1/ask-access/status", { method: "POST", body: { pickup } });
      answer = { state: s.status, body: s };
    } catch (e) {
      answer = { error: e.message, status: e.status, code: e.code };
    }
    if (isFinalAnswer(answer)) forget();
    if (!answer.error) {
      failures = 0;
      render(answer.body);
      if (answer.state === "pending") timer = setTimeout(() => poll(pickup), 10000);
    } else if (isRetryable(answer)) {
      failures += 1;
      retrying();
      timer = setTimeout(() => poll(pickup), retryDelay(failures));
    } else {
      fail(answer.error);
    }
  };

  $("claim-link").addEventListener("click", forget);
  $("forget").addEventListener("click", () => { forget(); location.reload(); });

  let saved = null;
  try { saved = sessionStorage.getItem(KEY); } catch { /* ignore */ }
  if (saved) { poll(saved); return; }

  api("/v1/ask-access/preview", { method: "POST", body: { code } })
    .then((p) => {
      $("server").textContent = `to ${p.server}`;
      if (typeof p.note === "string" && p.note) { $("note").textContent = p.note; show("note"); }
      showProject(p.project);
      $("closes").textContent = when(p.closesAt);
      $("name").maxLength = p.maxName || 60;
      $("message").maxLength = p.maxMessage || 500;
      $("ask-notice").textContent = p.notice;
      const box = $("agents");
      box.replaceChildren();
      for (const a of p.agents || []) {
        const label = document.createElement("label");
        label.className = "check";
        const input = document.createElement("input");
        input.type = "checkbox";
        input.value = a;
        input.checked = true;
        label.append(input, document.createTextNode(agentsText([a])));
        box.append(label);
      }
      show("loading", false);
      show("ask");
    })
    .catch((e) => fail(e.message));

  $("ask").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    show("ask-error", false);
    $("send").disabled = true;
    const agents = [...document.querySelectorAll("#agents input:checked")].map((i) => i.value);
    try {
      const r = await api("/v1/ask-access", {
        method: "POST",
        body: { code, name: $("name").value, message: $("message").value, agents },
      });
      try { sessionStorage.setItem(KEY, r.pickup); } catch { /* storage may be blocked */ }
      poll(r.pickup);
    } catch (e) {
      $("ask-error").textContent = e.message;
      show("ask-error");
    } finally {
      $("send").disabled = false;
    }
  });
}

function askedText(r) {
  const a = r.asked || {};
  const parts = [];
  if (a.dailyRequests != null) parts.push(`${a.dailyRequests} prompts a day`);
  if (a.monthlyRequests != null) parts.push(`${a.monthlyRequests} a month`);
  if (a.dailyBudgetUsd != null) parts.push(`$${a.dailyBudgetUsd} a day`);
  if (a.monthlyBudgetUsd != null) parts.push(`$${a.monthlyBudgetUsd} a month`);
  if ((r.agents || []).length) parts.push(`also ${agentsText(r.agents)}`);
  if (a.expiresInDays != null) parts.push(`${plural(a.expiresInDays, "more day", "more days")}`);
  return parts.length ? parts.join(", ") : "see message";
}

function mePage() {
  const KEY = "hashi.token";
  let current = null;
  const renderRequests = (list) => {
    const ul = $("my-requests");
    ul.replaceChildren();
    for (const r of list || []) {
      const li = document.createElement("li");
      const pill = document.createElement("span");
      pill.className = `pill ${r.status}`;
      pill.textContent = r.status;
      const what = document.createElement("span");
      what.textContent = ` ${askedText(r)} · ${when(r.createdAt)}`;
      li.append(pill, what);
      if (r.status === "declined" && r.reason) {
        const why = document.createElement("div");
        why.className = "muted small wrap";
        why.textContent = `Reason: ${r.reason}`;
        li.append(why);
      }
      ul.append(li);
    }
    show("my-requests", (list || []).length > 0);
  };
  const loadRequests = async () => {
    if (!current) return;
    try {
      renderRequests((await api("/v1/me/requests", { token: current })).requests);
    } catch { /* the access view still works without it */ }
  };
  const number = (id) => {
    const v = $(id).value.trim();
    return v === "" ? undefined : Number(v);
  };
  $("more-form").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    show("more-error", false);
    show("more-sent", false);
    const agents = $("more-agents").value.split(",").map((s) => s.trim().toLowerCase()).filter(Boolean);
    const body = {
      dailyRequests: number("more-daily"),
      monthlyRequests: number("more-monthly"),
      dailyBudgetUsd: number("more-daily-budget"),
      monthlyBudgetUsd: number("more-monthly-budget"),
      expiresInDays: number("more-expiry"),
      agents: agents.length ? agents : undefined,
      message: $("more-message").value.trim() || undefined,
    };
    try {
      await api("/v1/me/requests", { method: "POST", body, token: current });
      $("more-form").reset();
      show("more-sent");
      loadRequests();
    } catch (e) {
      $("more-error").textContent = e.message;
      show("more-error");
    }
  });
  const render = (m) => {
    $("server").textContent = `on ${m.server.name}`;
    $("name").textContent = m.name;
    $("status").textContent = m.status;
    $("status").className = `pill ${m.status}`;
    renderAgents($("agents"), m.agents);
    showProject(m.project);
    $("session").textContent = m.sessionMode === "stateful" ? "Remembers your conversation" : "Each prompt starts fresh";
    $("expires").textContent = when(m.expiresAt);
    $("pending").textContent = `${m.usage.pending}${m.limits.maxPending != null ? ` of ${m.limits.maxPending}` : ""}`;
    const meters = $("meters");
    meters.replaceChildren();
    const rows = [
      ["Prompts today", m.usage.todayRequests, m.limits.dailyRequests, (v) => String(v)],
      ["Prompts this month", m.usage.monthRequests, m.limits.monthlyRequests, (v) => String(v)],
      ["Spend today", m.usage.todayCostUsd, m.limits.dailyBudgetUsd, (v) => `$${v.toFixed(2)}`],
      ["Spend this month", m.usage.monthCostUsd, m.limits.monthlyBudgetUsd, (v) => `$${v.toFixed(2)}`],
    ];
    for (const [label, used, cap, fmt] of rows) {
      const wrap = document.createElement("div");
      wrap.className = "meter";
      const row = document.createElement("div");
      row.className = "row";
      const a = document.createElement("span");
      a.textContent = label;
      const b = document.createElement("span");
      b.textContent = cap != null ? `${fmt(used)} of ${fmt(cap)}` : `${fmt(used)}, no limit`;
      row.append(a, b);
      wrap.append(row);
      if (cap != null) {
        const bar = document.createElement("div");
        bar.className = "bar";
        const fill = document.createElement("div");
        fill.className = "fill";
        fill.style.width = `${Math.min(100, cap > 0 ? (used / cap) * 100 : 100)}%`;
        bar.append(fill);
        wrap.append(bar);
      }
      meters.append(wrap);
    }
    $("resets").textContent = `Daily limits reset ${when(m.resets.day)}. Spend is the agent's own estimate; Claude Code, opencode and Crush report one.`;
    // Admin tokens change their own access, so they have nobody to ask.
    show("more", m.role !== "admin");
    // Shown only when the bridge owner shares how much quota is left.
    const shared = m.agentUsage && typeof m.agentUsage === "object" ? Object.entries(m.agentUsage) : [];
    const parts = shared.map(([agent, u]) => {
      const left = typeof u.headroom === "number" ? `${u.headroom}% left` : "unknown";
      const reset = u.nextReset ? `, resets ${when(u.nextReset)}` : "";
      return `${agentName(agent)}: ${left} (${u.state})${reset}`;
    });
    $("headroom").textContent = parts.length ? `Quota left: ${parts.join("; ")}` : "";
    show("headroom", parts.length > 0);
    show("login", false);
    show("access");
  };

  const load = async (token) => {
    show("login-error", false);
    try {
      current = token;
      render(await api("/v1/me", { token }));
      loadRequests();
      return true;
    } catch (e) {
      $("login-error").textContent = e.message;
      show("login-error");
      return false;
    }
  };

  $("login").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const token = $("token-input").value.trim();
    if (await load(token) && $("remember").checked) {
      try { sessionStorage.setItem(KEY, token); } catch { /* storage may be blocked */ }
    }
    $("token-input").value = "";
  });
  $("forget").addEventListener("click", () => {
    try { sessionStorage.removeItem(KEY); } catch { /* ignore */ }
    current = null;
    show("access", false);
    show("login");
  });
  let saved = null;
  try { saved = sessionStorage.getItem(KEY); } catch { /* ignore */ }
  if (saved) load(saved);
}

document.addEventListener("click", (ev) => {
  const btn = ev.target.closest("[data-copy]");
  if (btn) copy($(btn.dataset.copy).textContent, btn);
});

if (document.body.dataset.page === "claim") claimPage();
if (document.body.dataset.page === "me") mePage();
if (document.body.dataset.page === "ask") askPage();
