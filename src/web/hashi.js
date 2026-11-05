// Claim and "My access" pages. All dynamic text goes through textContent and
// is never parsed as HTML, so nothing from the server can inject markup.
"use strict";

const $ = (id) => document.getElementById(id);
const show = (id, on = true) => { $(id).hidden = !on; };

async function api(path, { method = "GET", body, token } = {}) {
  const headers = { "Content-Type": "application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;
  const res = await fetch(path, { method, headers, body: body && JSON.stringify(body), cache: "no-store" });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data?.error?.message || `Request failed (${res.status})`);
  return data;
}

function agentsText(agents) {
  const names = { claude: "Claude Code", codex: "Codex" };
  return (agents || []).map((a) => names[a] || a).join(" and ");
}

function limitsText(l) {
  const parts = [];
  if (l?.dailyRequests != null) parts.push(`${l.dailyRequests} prompts a day`);
  if (l?.monthlyRequests != null) parts.push(`${l.monthlyRequests} a month`);
  if (l?.dailyBudgetUsd != null) parts.push(`$${l.dailyBudgetUsd} a day`);
  if (l?.monthlyBudgetUsd != null) parts.push(`$${l.monthlyBudgetUsd} a month`);
  if (l?.maxPending != null) parts.push(`${l.maxPending} at a time`);
  return parts.length ? parts.join(", ") : "No limits";
}

function when(iso) {
  return iso ? new Date(iso).toLocaleString() : "Never";
}

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
  const fail = (msg) => { show("loading", false); show("preview", false); $("error-text").textContent = msg; show("error"); };

  api("/v1/claim/preview", { method: "POST", body: { code } })
    .then((p) => {
      $("server").textContent = `to ${p.server}`;
      $("label").textContent = p.label;
      $("agents").textContent = agentsText(p.agents);
      showProject(p.project);
      $("session").textContent = p.sessionMode === "stateful" ? "Remembers your conversation" : "Each prompt starts fresh";
      $("limits").textContent = limitsText(p.limits);
      $("lifetime").textContent = p.tokenExpiresInDays ? `${p.tokenExpiresInDays} day(s) after you claim it` : "Until the owner revokes it";
      $("link-expires").textContent = when(p.linkExpiresAt);
      $("notice").textContent = p.notice;
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
      show("preview", false);
      show("done");
    } catch (e) {
      fail(e.message);
    }
  });
}

function mePage() {
  const KEY = "hashi.token";
  const render = (m) => {
    $("server").textContent = `on ${m.server.name}`;
    $("name").textContent = m.name;
    $("status").textContent = m.status;
    $("status").className = `pill ${m.status}`;
    $("agents").textContent = agentsText(m.agents);
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
    $("resets").textContent = `Daily limits reset ${when(m.resets.day)}. Spend is the estimate the agent reports (Claude only).`;
    show("login", false);
    show("access");
  };

  const load = async (token) => {
    show("login-error", false);
    try {
      render(await api("/v1/me", { token }));
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
