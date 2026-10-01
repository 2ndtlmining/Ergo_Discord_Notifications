"use strict";

// Stroke icons (24x24, Lucide-style). Status glyphs match assets/discord/.
const ICON = {
  ok: '<path d="M5 12.5l5 5L19 7"/>',
  down: '<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/>',
  behind: '<path d="M12 4.5V18M6.5 12.5 12 18l5.5-5.5"/>',
  "indexer-behind": '<path d="M12 4l8 4-8 4-8-4z"/><path d="m4 12.5 8 4 8-4M4 17l8 4 8-4"/>',
  syncing: '<path d="M20 11a8 8 0 0 0-14.6-4.4L4 8M4 4v4h4M4 13a8 8 0 0 0 14.6 4.4L20 16M20 20v-4h-4"/>',
  unknown: '<circle cx="12" cy="12" r="9"/><path d="M12 7.5v5.5M12 16.5v.01"/>',
  unreachable: '<circle cx="12" cy="12" r="9"/><path d="M12 7.5v5.5M12 16.5v.01"/>',
  received: '<path d="M12 3.5v10M7.5 9.5 12 14l4.5-4.5M4 15v4.5h16V15"/>',
  chevron: '<path d="m6 9 6 6 6-6"/>',
  copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/>',
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
  moon: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>',
};
const svg = (name, cls = "") =>
  `<svg viewBox="0 0 24 24" aria-hidden="true" class="${cls}">${ICON[name] || ICON.unknown}</svg>`;

const LABEL = {
  ok: "In sync", down: "Down", behind: "Behind", "indexer-behind": "Indexer behind",
  syncing: "Syncing", unknown: "Lag unknown",
};
// Worst first.
const SEVERITY = { down: 0, behind: 1, "indexer-behind": 2, syncing: 3, unknown: 4, ok: 5 };
const RAIL_MAX = 1e6; // blocks; the left edge of the rail

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const fmt = (n) => (n == null ? "n/a" : Number(n).toLocaleString("en-US"));
const erg = (n) => (n == null ? "n/a" : Number(n).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 4 }));

function ago(iso) {
  if (!iso) return "never";
  const s = Math.max(0, Math.round((Date.now() - new Date(iso)) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m ago`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h ago`;
}
const since = (iso) => (iso ? ago(iso).replace(" ago", "") : "n/a");

const shortAddr = (a) => (a && a.length > 16 ? `${a.slice(0, 8)}…${a.slice(-6)}` : a || "");
const hostOf = (url) => url.replace(/^https?:\/\//, "");

// ---------------------------------------------------------------- state
let status = null;
let alerts = [];
let filter = "all";
let query = "";
let lastTip = null;
let lastFetchOk = 0;
const expanded = new Set();
const history = new Map(); // node id -> [{t, lag}]

// ---------------------------------------------------------------- theme
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  const btn = $("theme");
  btn.innerHTML = svg(theme === "dark" ? "sun" : "moon");
  btn.setAttribute("aria-label", theme === "dark" ? "Switch to light theme" : "Switch to dark theme");
}
function storedTheme() {
  try { return localStorage.getItem("theme"); } catch { return null; }
}
$("theme").addEventListener("click", () => {
  const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  applyTheme(next);
  try { localStorage.setItem("theme", next); } catch { /* private mode */ }
});
applyTheme(storedTheme() || "dark");

// ---------------------------------------------------------------- data
async function refresh() {
  try {
    const [s, a] = await Promise.all([
      fetch("api/status", { cache: "no-store" }).then((r) => { if (!r.ok) throw new Error(r.status); return r.json(); }),
      fetch("api/alerts", { cache: "no-store" }).then((r) => (r.ok ? r.json() : [])),
    ]);
    status = s;
    alerts = a;
    lastFetchOk = Date.now();
    record(s);
    render();
  } catch {
    renderLive();
  }
}

function worstLag(n) {
  return Math.max(n.full_lag ?? 0, n.indexed_lag ?? 0);
}

function record(s) {
  const t = Date.now();
  for (const n of s.nodes) {
    if (n.status === "down" || n.full_lag == null) continue;
    const h = history.get(n.id) || [];
    h.push({ t, lag: worstLag(n) });
    while (h.length > 720) h.shift(); // ~1h at 5s
    history.set(n.id, h);
  }
}

// ---------------------------------------------------------------- render
function render() {
  if (!status) return;
  renderLive();
  renderHealth();
  renderDiscord();
  renderTip();
  renderRail();
  renderNodes();
  renderWallets();
  renderAlerts();
  $("foot").textContent = `Build ${status.commit}. Alert history and lag graphs are kept in memory and reset when the monitor restarts.`;
}

function renderLive() {
  const live = $("live");
  const gen = status?.generated_at;
  const stale = !lastFetchOk || Date.now() - lastFetchOk > 20000;
  live.toggleAttribute("data-on", !stale && !!gen);
  live.toggleAttribute("data-stale", stale && !!lastFetchOk);
  $("updated").textContent = stale && lastFetchOk
    ? "Can't reach the monitor. Retrying"
    : gen ? `Checked ${ago(gen)}` : "Waiting for the first check";
}

function renderHealth() {
  const { summary: s, reference } = status;
  const el = $("health");
  const bad = s.total - s.ok;
  let state = "ok", icon = "ok", text;
  if (!status.generated_at) { state = ""; icon = "syncing"; text = "Running the first check"; }
  else if (s.total === 0) { state = "warn"; icon = "unknown"; text = "No nodes configured"; }
  else if (reference.height == null) { state = "warn"; icon = "unreachable"; text = "Explorers unreachable"; }
  else if (bad === 0) { text = s.total === 1 ? "Node in sync" : `All ${s.total} nodes in sync`; }
  else { state = s.down ? "bad" : "warn"; icon = s.down ? "down" : "behind"; text = `${bad} of ${s.total} need attention`; }
  el.dataset.state = state;
  el.innerHTML = `${svg(icon)}${esc(text)}`;
  document.title = bad && status.generated_at ? `${bad} need attention · Ergo Monitor` : "Ergo Monitor";
  const color = { ok: "#30a46c", warn: "#f5a524", bad: "#e5484d" }[state] || "#8b8d98";
  $("favicon").href = "data:image/svg+xml," + encodeURIComponent(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="11" fill="${color}"/><g fill="none" stroke="white" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" transform="translate(6 6) scale(.5)">${ICON[icon]}</g></svg>`);
}

// Shown only while Discord delivery is failing (#29).
function renderDiscord() {
  const d = status.discord;
  const el = $("discord");
  const failing = !!(d && d.enabled && d.last_error);
  el.hidden = !failing;
  if (!failing) return;
  el.innerHTML = `${svg("down")}Discord alerts failing`;
  el.title = `${d.last_error} (${ago(d.last_error_at)})${d.queued ? `. ${d.queued} waiting to send.` : ""}`;
}

function renderTip() {
  const { height, sources } = status.reference;
  const el = $("tip-height");
  el.textContent = height == null ? "Unknown" : fmt(height);
  if (height != null && lastTip != null && height !== lastTip) {
    el.classList.remove("bump"); void el.offsetWidth; el.classList.add("bump");
  }
  lastTip = height;
  const names = { mainnet: "Mainnet explorer", p2p: "P2P explorer" };
  const heights = sources.filter((s) => s.ok && s.height != null).map((s) => s.height);
  const drift = heights.length === 2 ? Math.abs(heights[0] - heights[1]) : 0;
  $("sources").innerHTML = sources.map((s) => `
    <div><dt>${esc(names[s.name] || s.name)}</dt>
      <dd class="${s.ok ? "" : "off"}" title="${esc(s.error || s.url)}"><i class="dot"></i>${s.ok ? fmt(s.height) : "Not responding"}</dd></div>`).join("")
    + (drift > 2 ? `<div><dt>Explorers disagree</dt><dd class="drift">by ${drift} blocks</dd></div>` : "");
}

const railX = (lag) => 100 * (1 - Math.log10(Math.min(lag, RAIL_MAX) + 1) / Math.log10(RAIL_MAX + 1));

function renderRail() {
  const threshold = 5;
  const rail = $("rail");
  rail.style.setProperty("--zone", `${100 - railX(threshold)}%`);

  const up = status.nodes.filter((n) => n.status !== "down" && n.full_lag != null);
  const pts = up.map((n) => ({ n, x: railX(worstLag(n)) })).sort((a, b) => a.x - b.x);
  // Nodes at (almost) the same spot share one marker.
  const clusters = [];
  for (const p of pts) {
    const c = clusters[clusters.length - 1];
    if (c && p.x - c.x < 2.5) c.items.push(p.n);
    else clusters.push({ x: p.x, items: [p.n] });
  }
  // Alternate lanes when neighbours are close so labels don't collide.
  let lastLane0 = -100;
  const html = clusters.map((c) => {
    const worst = c.items.reduce((a, b) => (SEVERITY[b.condition] < SEVERITY[a.condition] ? b : a));
    const lane = c.x - lastLane0 < 22 ? 1 : 0;
    if (lane === 0) lastLane0 = c.x;
    const names = c.items.length > 2 ? `${c.items.length} nodes` : c.items.map((n) => n.name).join(", ");
    const lag = worstLag(worst);
    const note = worst.condition === "syncing" ? `${(worst.sync_progress * 100).toFixed(1)}%`
      : lag <= threshold ? "in sync" : `${fmt(lag)} behind`;
    const edge = c.x > 88 ? "at-end" : c.x < 8 ? "at-start" : "";
    const title = c.items.map((n) => `${n.name}: ${n.detail}`).join("\n");
    return `<button type="button" class="marker ${edge}" data-c="${worst.condition}" data-id="${esc(worst.id)}"
      style="left:${Math.min(c.x, 99)}%;--lane:${lane}" title="${esc(title)}">
      <span class="pin"></span><span class="tag">${esc(names)}<b>${esc(note)}</b></span></button>`;
  }).join("");
  rail.innerHTML = html;

  const ticks = [[1e6, "1M blocks"], [1e5, "100k"], [1e4, "10k"], [1e3, "1,000"], [100, "100"], [0, "Tip"]];
  $("rail-axis").innerHTML = ticks.map(([v, l]) => `<span style="left:${railX(v)}%">${l}</span>`).join("");

  const down = status.nodes.filter((n) => n.status === "down");
  const off = $("rail-offline");
  off.hidden = !down.length;
  off.innerHTML = down.length ? `<strong>Not responding:</strong> ${down.map((n) => esc(n.name)).join(", ")}` : "";
}

function spark(id) {
  const h = history.get(id) || [];
  if (h.length < 3) return `<p class="spark-note" style="color:var(--faint);font-size:13px;margin:8px 0 0">Lag history appears after a few checks.</p>`;
  const max = Math.max(...h.map((p) => p.lag), 5);
  const w = 300, ht = 44;
  const d = h.map((p, i) => `${i ? "L" : "M"}${((i / (h.length - 1)) * w).toFixed(1)},${(ht - 2 - (p.lag / max) * (ht - 4)).toFixed(1)}`).join("");
  return `<svg class="spark" viewBox="0 0 ${w} ${ht}" preserveAspectRatio="none" role="img" aria-label="Lag over the last ${h.length} checks, peak ${fmt(max)} blocks"><path d="${d}"/></svg>`;
}

function heightCell(value, lag, cls, label, n) {
  if (n.status === "down") return `<div class="h ${cls}"><span class="v" data-label="${label}">n/a</span></div>`;
  const bad = lag != null && lag > 5;
  const lagText = lag == null ? "" : lag === 0 ? "at tip" : `${fmt(lag)} behind`;
  return `<div class="h ${cls}"><span class="v" data-label="${label}">${fmt(value)}</span>
    <span class="lag ${bad ? "bad" : ""}">${lagText}</span></div>`;
}

function versionText(n) {
  if (!n.version) return "n/a";
  if (!n.latest_version) return esc(n.version);
  if (n.version_outdated) {
    return `${esc(n.version)} <a class="outdated" href="${esc(n.latest_version_url)}" target="_blank" rel="noopener">${esc(n.latest_version)} available</a>`;
  }
  return `${esc(n.version)} <span class="latest">latest</span>`;
}

function renderNodes() {
  const q = query.trim().toLowerCase();
  const problems = status.nodes.filter((n) => n.condition !== "ok").length;
  document.querySelector('[data-filter="problems"]').innerHTML =
    `Needs attention${problems ? `<span class="count">${problems}</span>` : ""}`;

  const rows = status.nodes
    .filter((n) => filter === "all" || n.condition !== "ok")
    .filter((n) => !q || n.name.toLowerCase().includes(q) || n.url.includes(q))
    .sort((a, b) => SEVERITY[a.condition] - SEVERITY[b.condition] || a.name.localeCompare(b.name));

  const wallets = new Map(status.wallets.filter((w) => w.node_id).map((w) => [w.node_id, w]));
  $("node-rows").innerHTML = rows.map((n) => {
    const open = expanded.has(n.id);
    const roles = [n.is_mining && '<span class="role mining">Mining</span>', n.is_explorer && '<span class="role">Indexer</span>',
      n.version_outdated && `<span class="role update" title="Running ${esc(n.version)}, latest is ${esc(n.latest_version)}">Update available</span>`]
      .filter(Boolean).join("");
    const w = wallets.get(n.id);
    const syncing = n.condition === "syncing"
      ? `<div class="progress" aria-hidden="true"><i style="width:${(n.sync_progress * 100).toFixed(1)}%"></i></div>` : "";
    return `<div class="row" role="rowgroup" data-c="${n.condition}" ${n.condition !== "ok" ? "data-problem" : ""} aria-expanded="${open}" data-id="${esc(n.id)}">
      <div class="row-main" role="row" tabindex="0" aria-label="${esc(n.name)}: ${esc(LABEL[n.condition])}. Show details">
        <div class="name" role="cell"><strong>${esc(n.name)}</strong><a class="panel" href="${esc(n.url)}/panel" target="_blank" rel="noopener" title="Open the node panel">${esc(hostOf(n.url))}</a><span class="roles">${roles}</span></div>
        <div class="status" role="cell" title="${esc(n.detail)}">${svg(n.condition)}<span>${esc(LABEL[n.condition])}</span></div>
        <div role="cell" class="c-full">${heightCell(n.full_height, n.full_lag, "full", "Height", n)}${syncing}</div>
        <div role="cell" class="c-idx">${n.indexed_height == null && n.status !== "down"
          ? '<div class="h idx"><span class="v" data-label="Indexed">n/a</span><span class="lag">no index</span></div>'
          : heightCell(n.indexed_height, n.indexed_lag, "idx", "Indexed", n)}</div>
        <div class="num peers" role="cell">${n.peers ?? "n/a"}</div>
        <div class="num lat" role="cell">${n.latency_ms != null ? `${n.latency_ms} ms` : "n/a"}</div>
        ${svg("chevron", "chev")}
      </div>
      <div class="detail">
        <div>
          <p class="lead">${esc(n.detail)}</p>
          ${n.last_error ? `<p style="color:var(--down)">${esc(n.last_error)}</p>` : ""}
          ${n.runbook ? `<span class="runbook">Runbook <code>${esc(n.runbook)}</code></span>` : ""}
          ${spark(n.id)}
        </div>
        <dl class="kv">
          <dt>Panel</dt><dd><a href="${esc(n.url)}/panel" target="_blank" rel="noopener">${esc(hostOf(n.url))}/panel</a></dd>
          <dt>Endpoint</dt><dd><button type="button" class="copy" data-copy="${esc(n.url)}">${esc(n.url)}${svg("copy")}</button></dd>
          <dt>In this state</dt><dd>${since(n.status_since)}</dd>
          <dt>Last response</dt><dd>${ago(n.last_ok)}</dd>
          <dt>Headers</dt><dd>${fmt(n.headers_height)}</dd>
          <dt>Version</dt><dd>${versionText(n)}</dd>
          ${w ? `<dt>Wallet</dt><dd>${erg(w.balance_erg)} ERG · ${esc(shortAddr(w.address))}</dd>` : ""}
        </dl>
      </div>
    </div>`;
  }).join("");

  const empty = $("nodes-empty");
  empty.hidden = rows.length > 0;
  empty.textContent = status.nodes.length === 0
    ? "No nodes configured yet. Add NODE_1_NAME and NODE_1_URL to .env, then redeploy."
    : filter === "problems" && !q ? "Every node is in sync." : "No nodes match that search.";
}

function renderWallets() {
  const el = $("wallet-rows");
  if (!status.wallets.length) {
    el.innerHTML = '<p class="empty">No wallets configured. Add WALLET_1_NAME and WALLET_1_ADDRESS, or NODE_1_WALLET_ADDRESS, to .env.</p>';
    return;
  }
  el.innerHTML = status.wallets.map((w) => {
    const t = w.last_tx;
    const last = t
      ? `Last transaction ${ago(t.timestamp)}: <span class="${t.value_erg > 0 ? "in" : ""}">${t.value_erg > 0 ? "+" : ""}${erg(t.value_erg)} ERG</span>`
      : w.last_ok ? "No transactions yet" : "Loading";
    return `<div class="wallet">
      <div class="who">${esc(w.name)}${w.node_id ? "<small>node wallet</small>" : ""}</div>
      <div class="bal">${erg(w.balance_erg)}<small>ERG</small></div>
      <div class="meta">${w.last_error ? `<span class="err">Balance unavailable: ${esc(w.last_error)}</span>` : last}</div>
      <div class="meta"><a href="https://explorer.ergoplatform.com/en/addresses/${esc(w.address)}" target="_blank" rel="noopener">${esc(shortAddr(w.address))}</a> · ${fmt(w.total_txs)} txs</div>
    </div>`;
  }).join("");
}

function renderAlerts() {
  const list = $("alert-list");
  if (!alerts.length) {
    list.innerHTML = '<li><span></span><span class="none">No alerts since the monitor started.</span></li>';
    return;
  }
  list.innerHTML = alerts.slice(0, 20).map((a) => `
    <li data-c="${esc(a.kind)}">${svg(a.kind)}
      <div><div class="t"><b>${esc(a.subject)}</b> <span>${esc(a.headline)}</span>${a.delivery === "failed" ? ' <em class="undelivered">not delivered to Discord</em>' : ""}</div><div class="d">${esc(a.detail)}</div></div>
      <time datetime="${esc(a.at)}">${ago(a.at)}</time></li>`).join("");
}

// ---------------------------------------------------------------- interaction
function toggleRow(row) {
  const id = row.dataset.id;
  expanded.has(id) ? expanded.delete(id) : expanded.add(id);
  row.setAttribute("aria-expanded", expanded.has(id));
}

function toast(text) {
  const t = $("toast");
  t.textContent = text;
  t.setAttribute("data-show", "");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => t.removeAttribute("data-show"), 1600);
}

// navigator.clipboard only exists on HTTPS and localhost; the dashboard is
// usually opened over plain HTTP on the LAN, so fall back to execCommand.
async function copyText(text) {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch { /* fall through */ }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.cssText = "position:fixed;top:0;left:0;opacity:0";
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try { ok = document.execCommand("copy"); } catch { ok = false; }
  ta.remove();
  return ok;
}

document.addEventListener("click", (e) => {
  const copy = e.target.closest("[data-copy]");
  if (copy) {
    e.stopPropagation();
    copyText(copy.dataset.copy).then((ok) => toast(ok ? "Endpoint copied" : "Copy not available here"));
    return;
  }
  // Links inside a row (the panel) open normally instead of toggling it.
  if (e.target.closest("a")) return;
  const main = e.target.closest(".row-main");
  if (main) return toggleRow(main.parentElement);
  const marker = e.target.closest(".marker");
  if (marker) {
    const row = document.querySelector(`.row[data-id="${CSS.escape(marker.dataset.id)}"]`);
    if (row) {
      if (!expanded.has(row.dataset.id)) toggleRow(row);
      row.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  }
  const f = e.target.closest("[data-filter]");
  if (f) {
    filter = f.dataset.filter;
    document.querySelectorAll("[data-filter]").forEach((b) => b.setAttribute("aria-pressed", b === f));
    renderNodes();
  }
});
document.addEventListener("keydown", (e) => {
  const main = e.target.closest?.(".row-main");
  if (main && e.target === main && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); toggleRow(main.parentElement); }
});
$("search").addEventListener("input", (e) => { query = e.target.value; if (status) renderNodes(); });

refresh();
setInterval(refresh, 5000);
setInterval(renderLive, 1000);
