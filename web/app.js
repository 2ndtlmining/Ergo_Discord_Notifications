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
const ALERTS_SHOWN = 20;

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const fmt = (n) => (n == null ? "n/a" : Number(n).toLocaleString("en-US"));
const erg = (n) => (n == null ? "n/a" : Number(n).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 4 }));
const absTime = (iso) => (iso ? new Date(iso).toLocaleString() : "");

function ago(iso) {
  if (!iso) return "never";
  const s = Math.max(0, Math.round((Date.now() - new Date(iso)) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m ago`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h ago`;
}
const since = (iso) => (iso ? ago(iso).replace(" ago", "") : "n/a");
// Relative times are filled in by a 1s ticker, so the surrounding HTML stays
// the same between refreshes and doesn't need rebuilding (#32).
const agoSpan = (iso) => `<span data-ago="${esc(iso || "")}" title="${esc(absTime(iso))}">${ago(iso)}</span>`;
const sinceSpan = (iso) => `<span data-since="${esc(iso || "")}">${since(iso)}</span>`;

const shortAddr = (a) => (a && a.length > 16 ? `${a.slice(0, 8)}…${a.slice(-6)}` : a || "");
const hostOf = (url) => url.replace(/^https?:\/\//, "");

// ---------------------------------------------------------------- state
let status = null;
let alerts = [];
let filter = "all";
let query = "";
let lastTip = null;
let lastFetchOk = 0;
let builtAt = null;
let showAllAlerts = false;
const expanded = new Set();
const openAlerts = new Set();
const history = new Map(); // node id -> [{t, lag}]

const threshold = () => status?.settings?.lag_threshold_blocks ?? 5;
const pollSeconds = () => status?.settings?.node_poll_seconds ?? 30;

// Only touch the DOM when something changed, so focus, hover tooltips and
// CSS transitions survive the 5s refresh (#32).
function setHTML(el, html) {
  if (el._html !== html) { el.innerHTML = html; el._html = html; }
}
function setText(el, text) {
  if (el.textContent !== text) el.textContent = text;
}

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
    // One point per poll, not per dashboard refresh.
    if (h.length && h[h.length - 1].gen === s.generated_at) continue;
    h.push({ t, gen: s.generated_at, lag: worstLag(n) });
    while (h.length > 240) h.shift(); // ~2h at 30s
    history.set(n.id, h);
  }
}

// ---------------------------------------------------------------- render
function selecting() {
  const sel = window.getSelection();
  return !!sel && !sel.isCollapsed && document.querySelector("main").contains(sel.anchorNode);
}

function render() {
  if (!status) return;
  renderLive();
  renderHealth();
  renderDiscord();
  renderTip();
  renderRail();
  // Don't rewrite text while it's being selected for copying; the next
  // refresh after the selection is cleared catches up.
  if (!selecting()) {
    renderNodes();
    renderWallets();
    renderAlerts();
  }
  setText($("foot"), `Build ${status.commit}${builtAt ? `, built ${absTime(builtAt)}` : ""}. Alert history and lag graphs are kept in memory and reset when the monitor restarts.`);
}

// null, "unreachable" (the dashboard can't fetch) or "frozen" (it can, but
// the monitor has stopped checking nodes) (#33).
function staleness() {
  if (!lastFetchOk) return null;
  if (Date.now() - lastFetchOk > 20000) return "unreachable";
  const last = Date.parse(status?.generated_at || status?.started_at);
  if (last && Date.now() - last > (3 * pollSeconds() + 30) * 1000) return "frozen";
  return null;
}

function renderLive() {
  const live = $("live");
  const gen = status?.generated_at;
  const stale = staleness();
  live.toggleAttribute("data-on", !stale && !!gen);
  live.toggleAttribute("data-stale", !!stale);
  setText($("updated"), stale === "unreachable" ? "Can't reach the monitor"
    : stale === "frozen" ? "Not refreshing"
    : gen ? `Checked ${ago(gen)}` : "Waiting for the first check");

  document.querySelector("main").toggleAttribute("data-stale", !!stale);
  const banner = $("stale");
  banner.hidden = !stale;
  if (stale) {
    setHTML(banner, stale === "unreachable"
      ? `<strong>Can't reach the monitor.</strong> Showing data from ${agoSpan(gen)}; retrying every 5 seconds.`
      : `<strong>The monitor has stopped checking nodes</strong> (last check ${agoSpan(gen || status.started_at)}). This data is out of date; check the monitor is running with <code>docker compose ps</code>.`);
  }
  document.querySelectorAll("[data-ago]").forEach((el) => setText(el, ago(el.dataset.ago || null)));
  document.querySelectorAll("[data-since]").forEach((el) => setText(el, since(el.dataset.since || null)));
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
  // role="status": rewriting it every refresh would make screen readers re-announce it.
  el.dataset.state = state;
  setHTML(el, `${svg(icon)}${esc(text)}`);
  document.title = bad && status.generated_at ? `${bad} need attention · Ergo Monitor` : "Ergo Monitor";
  const color = { ok: "#30a46c", warn: "#f5a524", bad: "#e5484d" }[state] || "#8b8d98";
  const href = "data:image/svg+xml," + encodeURIComponent(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="11" fill="${color}"/><g fill="none" stroke="white" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" transform="translate(6 6) scale(.5)">${ICON[icon]}</g></svg>`);
  if ($("favicon").getAttribute("href") !== href) $("favicon").setAttribute("href", href);
}

// Shown only while Discord delivery is failing (#29).
function renderDiscord() {
  const d = status.discord;
  const el = $("discord");
  const failing = !!(d && d.enabled && d.last_error);
  el.hidden = !failing;
  if (!failing) return;
  setHTML(el, `${svg("down")}Discord alerts failing`);
  el.title = `${d.last_error} (${ago(d.last_error_at)})${d.queued ? `. ${d.queued} waiting to send.` : ""}`;
}

function renderTip() {
  const { height, sources } = status.reference;
  const el = $("tip-height");
  setText(el, height == null ? "Unknown" : fmt(height));
  if (height != null && lastTip != null && height !== lastTip) {
    el.classList.remove("bump"); void el.offsetWidth; el.classList.add("bump");
  }
  lastTip = height;
  const names = { mainnet: "Mainnet explorer", p2p: "P2P explorer" };
  const heights = sources.filter((s) => s.ok && s.height != null).map((s) => s.height);
  const drift = heights.length === 2 ? Math.abs(heights[0] - heights[1]) : 0;
  setHTML($("sources"), sources.map((s) => `
    <div><dt>${esc(names[s.name] || s.name)}</dt>
      <dd class="${s.ok ? "" : "off"}" title="${esc(s.error || s.url)}"><i class="dot"></i>${s.ok ? fmt(s.height) : "Not responding"}</dd></div>`).join("")
    + (drift > 2 ? `<div><dt>Explorers disagree</dt><dd class="drift">by ${drift} blocks</dd></div>` : ""));
}

const railX = (lag) => 100 * (1 - Math.log10(Math.min(lag, RAIL_MAX) + 1) / Math.log10(RAIL_MAX + 1));

function renderRail() {
  const limit = threshold();
  const rail = $("rail");
  rail.style.setProperty("--zone", `${100 - railX(limit)}%`);

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
  const markers = clusters.map((c) => {
    const worst = c.items.reduce((a, b) => (SEVERITY[b.condition] < SEVERITY[a.condition] ? b : a));
    const lane = c.x - lastLane0 < 22 ? 1 : 0;
    if (lane === 0) lastLane0 = c.x;
    const names = c.items.length > 2 ? `${c.items.length} nodes` : c.items.map((n) => n.name).join(", ");
    const lag = worstLag(worst);
    const note = worst.condition === "syncing" ? `${(worst.sync_progress * 100).toFixed(1)}%`
      : lag <= limit ? "in sync" : `${fmt(lag)} behind`;
    return {
      key: c.items.map((n) => n.id).join(","),
      x: Math.min(c.x, 99), lane, worst,
      edge: c.x > 88 ? "at-end" : c.x < 8 ? "at-start" : "",
      title: c.items.map((n) => `${n.name}: ${n.detail}`).join("\n"),
      tag: `${esc(names)}<b>${esc(note)}</b>`,
    };
  });
  // Same nodes in the same order: move the existing markers so the CSS
  // transition animates them along the rail.
  const els = [...rail.children];
  if (els.length !== markers.length || els.some((el, i) => el.dataset.key !== markers[i].key)) {
    rail.innerHTML = markers.map((m) =>
      `<button type="button" class="marker" data-key="${esc(m.key)}"><span class="pin"></span><span class="tag"></span></button>`).join("");
  }
  [...rail.children].forEach((el, i) => {
    const m = markers[i];
    el.className = `marker ${m.edge}`;
    el.dataset.c = m.worst.condition;
    el.dataset.id = m.worst.id;
    el.style.left = `${m.x}%`;
    el.style.setProperty("--lane", m.lane);
    el.title = m.title;
    setHTML(el.querySelector(".tag"), m.tag);
  });

  const ticks = [[1e6, "1M blocks"], [1e5, "100k"], [1e4, "10k"], [1e3, "1,000"], [100, "100"], [0, "Tip"]];
  setHTML($("rail-axis"), ticks.map(([v, l]) => `<span style="left:${railX(v)}%">${l}</span>`).join(""));

  const down = status.nodes.filter((n) => n.status === "down");
  const off = $("rail-offline");
  off.hidden = !down.length;
  setHTML(off, down.length ? `<strong>Not responding:</strong> ${down.map((n) => esc(n.name)).join(", ")}` : "");
}

function spark(id) {
  const h = history.get(id) || [];
  if (h.length < 3) return `<p class="spark-note" style="color:var(--faint);font-size:13px;margin:8px 0 0">Lag history appears after a few checks.</p>`;
  const max = Math.max(...h.map((p) => p.lag), threshold());
  const w = 300, ht = 44;
  const d = h.map((p, i) => `${i ? "L" : "M"}${((i / (h.length - 1)) * w).toFixed(1)},${(ht - 2 - (p.lag / max) * (ht - 4)).toFixed(1)}`).join("");
  return `<svg class="spark" viewBox="0 0 ${w} ${ht}" preserveAspectRatio="none" role="img" aria-label="Lag over the last ${h.length} checks, peak ${fmt(max)} blocks"><path d="${d}"/></svg>`;
}

function heightCell(value, lag, cls, label, n) {
  if (n.status === "down") return `<div class="h ${cls}"><span class="v" data-label="${label}">n/a</span></div>`;
  const bad = lag != null && lag > threshold();
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

// The parts of a row that only change with its condition or configuration.
// Values that change every poll go in [data-f] slots filled by rowFields.
function rowShell(n, w) {
  const roles = [n.is_mining && '<span class="role mining">Mining</span>', n.is_explorer && '<span class="role">Indexer</span>',
    n.version_outdated && `<span class="role update" title="Running ${esc(n.version)}, latest is ${esc(n.latest_version)}">Update available</span>`]
    .filter(Boolean).join("");
  return `<div class="row" role="rowgroup" data-c="${n.condition}" ${n.condition !== "ok" ? "data-problem" : ""} data-id="${esc(n.id)}">
      <div class="row-main" role="row" tabindex="0" data-fk="main" aria-label="${esc(n.name)}: ${esc(LABEL[n.condition])}. Show details">
        <div class="name" role="cell"><strong>${esc(n.name)}</strong><a class="panel" data-fk="panel" href="${esc(n.url)}/panel" target="_blank" rel="noopener" title="Open the node panel">${esc(hostOf(n.url))}</a><span class="roles">${roles}</span></div>
        <div class="status" role="cell">${svg(n.condition)}<span>${esc(LABEL[n.condition])}</span></div>
        <div role="cell" class="c-full" data-f="full"></div>
        <div role="cell" class="c-idx" data-f="idx"></div>
        <div class="num peers" role="cell" data-f="peers"></div>
        <div class="num lat" role="cell" data-f="lat"></div>
        ${svg("chevron", "chev")}
      </div>
      <div class="detail">
        <div>
          <p class="lead" data-f="lead"></p>
          <div data-f="err"></div>
          ${n.runbook ? `<span class="runbook">Runbook <code>${esc(n.runbook)}</code></span>` : ""}
          <div data-f="spark"></div>
        </div>
        <dl class="kv">
          <dt>Panel</dt><dd><a href="${esc(n.url)}/panel" target="_blank" rel="noopener">${esc(hostOf(n.url))}/panel</a></dd>
          <dt>Endpoint</dt><dd><button type="button" class="copy" data-fk="copy" data-copy="${esc(n.url)}">${esc(n.url)}${svg("copy")}</button></dd>
          <dt>In this state</dt><dd data-f="since"></dd>
          <dt>Last response</dt><dd data-f="lastok"></dd>
          <dt class="m-only">Peers</dt><dd class="m-only" data-f="peers"></dd>
          <dt class="m-only">Response</dt><dd class="m-only" data-f="lat"></dd>
          <dt>Headers</dt><dd data-f="headers"></dd>
          <dt>Version</dt><dd data-f="version"></dd>
          ${w ? `<dt>Wallet</dt><dd data-f="wallet"></dd>` : ""}
        </dl>
      </div>
    </div>`;
}

function rowFields(n, w) {
  const syncing = n.condition === "syncing"
    ? `<div class="progress" aria-hidden="true"><i style="width:${(n.sync_progress * 100).toFixed(1)}%"></i></div>` : "";
  return {
    full: heightCell(n.full_height, n.full_lag, "full", "Height", n) + syncing,
    idx: n.indexed_height == null && n.status !== "down"
      ? '<div class="h idx"><span class="v" data-label="Indexed">n/a</span><span class="lag">no index</span></div>'
      : heightCell(n.indexed_height, n.indexed_lag, "idx", "Indexed", n),
    peers: esc(n.peers ?? "n/a"),
    lat: n.latency_ms != null ? `${n.latency_ms} ms` : "n/a",
    lead: esc(n.detail),
    err: n.last_error ? `<p style="color:var(--down)">${esc(n.last_error)}</p>` : "",
    spark: expanded.has(n.id) ? spark(n.id) : "",
    since: sinceSpan(n.status_since),
    lastok: agoSpan(n.last_ok),
    headers: fmt(n.headers_height),
    version: versionText(n),
    wallet: w ? `${erg(w.balance_erg)} ERG · ${esc(shortAddr(w.address))}` : "",
  };
}

function patchRow(el, n, w) {
  el.setAttribute("aria-expanded", expanded.has(n.id));
  const f = rowFields(n, w);
  el.querySelectorAll("[data-f]").forEach((slot) => setHTML(slot, f[slot.dataset.f] ?? ""));
  const st = el.querySelector(".status");
  if (st.title !== n.detail) st.title = n.detail;
}

function renderNodes() {
  const q = query.trim().toLowerCase();
  const problems = status.nodes.filter((n) => n.condition !== "ok").length;
  setHTML(document.querySelector('[data-filter="problems"]'),
    `Needs attention${problems ? `<span class="count">${problems}</span>` : ""}`);

  const rows = status.nodes
    .filter((n) => filter === "all" || n.condition !== "ok")
    .filter((n) => !q || n.name.toLowerCase().includes(q) || n.url.includes(q))
    .sort((a, b) => SEVERITY[a.condition] - SEVERITY[b.condition] || a.name.localeCompare(b.name));

  const wallets = new Map(status.wallets.filter((w) => w.node_id).map((w) => [w.node_id, w]));
  const box = $("node-rows");
  const active = document.activeElement;
  const focus = active && box.contains(active) && active.dataset.fk
    ? { id: active.closest(".row").dataset.id, fk: active.dataset.fk } : null;

  // Keep each row's element; rebuild a row only when its shell changes
  // (condition, roles, wallet), otherwise just patch its values.
  const existing = new Map([...box.children].map((el) => [el.dataset.id, el]));
  const els = rows.map((n) => {
    const w = wallets.get(n.id);
    const shell = rowShell(n, w);
    let el = existing.get(n.id);
    if (!el || el._shell !== shell) {
      const t = document.createElement("template");
      t.innerHTML = shell.trim();
      const fresh = t.content.firstElementChild;
      fresh._shell = shell;
      if (el) el.replaceWith(fresh);
      el = fresh;
    }
    patchRow(el, n, w);
    return el;
  });
  els.forEach((el, i) => { if (box.children[i] !== el) box.insertBefore(el, box.children[i] || null); });
  while (box.children.length > els.length) box.lastElementChild.remove();

  if (focus && !box.contains(document.activeElement)) {
    box.querySelector(`.row[data-id="${CSS.escape(focus.id)}"] [data-fk="${focus.fk}"]`)?.focus({ preventScroll: true });
  }

  const empty = $("nodes-empty");
  empty.hidden = rows.length > 0;
  setText(empty, status.nodes.length === 0
    ? "No nodes configured yet. Add NODE_1_NAME and NODE_1_URL to .env, then redeploy."
    : filter === "problems" && !q ? "Every node is in sync." : "No nodes match that search.");
}

function renderWallets() {
  const el = $("wallet-rows");
  if (!status.wallets.length) {
    setHTML(el, '<p class="empty">No wallets configured. Add WALLET_1_NAME and WALLET_1_ADDRESS, or NODE_1_WALLET_ADDRESS, to .env.</p>');
    return;
  }
  setHTML(el, status.wallets.map((w) => {
    const t = w.last_tx;
    const last = t
      ? `Last transaction ${agoSpan(t.timestamp)}: <span class="${t.value_erg > 0 ? "in" : ""}">${t.value_erg > 0 ? "+" : ""}${erg(t.value_erg)} ERG</span>`
      : w.last_ok ? "No transactions yet" : "Loading";
    return `<div class="wallet">
      <div class="who">${esc(w.name)}${w.node_id ? "<small>node wallet</small>" : ""}</div>
      <div class="bal">${erg(w.balance_erg)}<small>ERG</small></div>
      <div class="meta">${w.last_error ? `<span class="err">Balance unavailable: ${esc(w.last_error)}</span>` : last}</div>
      <div class="meta"><a href="https://explorer.ergoplatform.com/en/addresses/${esc(w.address)}" target="_blank" rel="noopener">${esc(shortAddr(w.address))}</a> · ${fmt(w.total_txs)} txs</div>
    </div>`;
  }).join(""));
}

function renderAlerts() {
  const list = $("alert-list");
  const more = $("alerts-more");
  if (!alerts.length) {
    setHTML(list, '<li><span></span><span class="none">No alerts since the monitor started.</span></li>');
    more.hidden = true;
    return;
  }
  const shown = showAllAlerts ? alerts : alerts.slice(0, ALERTS_SHOWN);
  setHTML(list, shown.map((a) => `
    <li data-c="${esc(a.kind)}" data-alert="${a.id}" class="${openAlerts.has(a.id) ? "open" : ""}" tabindex="0" aria-expanded="${openAlerts.has(a.id)}">${svg(a.kind)}
      <div><div class="t"><b>${esc(a.subject)}</b> <span>${esc(a.headline)}</span>${a.delivery === "failed" ? ' <em class="undelivered">not delivered to Discord</em>' : ""}</div><div class="d">${esc(a.detail)}</div></div>
      <time datetime="${esc(a.at)}">${agoSpan(a.at)}</time></li>`).join(""));
  more.hidden = alerts.length <= ALERTS_SHOWN;
  setText(more, showAllAlerts ? "Show fewer" : `Show all ${alerts.length}`);
}

// ---------------------------------------------------------------- interaction
function toggleRow(row) {
  const id = row.dataset.id;
  expanded.has(id) ? expanded.delete(id) : expanded.add(id);
  row.setAttribute("aria-expanded", expanded.has(id));
  if (status) renderNodes(); // fills in the lag graph
}

function toggleAlert(li) {
  const id = Number(li.dataset.alert);
  openAlerts.has(id) ? openAlerts.delete(id) : openAlerts.add(id);
  renderAlerts();
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
  const li = e.target.closest("[data-alert]");
  if (li) return toggleAlert(li);
  if (e.target.closest("#alerts-more")) {
    showAllAlerts = !showAllAlerts;
    return renderAlerts();
  }
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
  if (e.key !== "Enter" && e.key !== " ") return;
  const main = e.target.closest?.(".row-main");
  if (main && e.target === main) { e.preventDefault(); toggleRow(main.parentElement); }
  const li = e.target.closest?.("[data-alert]");
  if (li && e.target === li) { e.preventDefault(); toggleAlert(li); }
});
$("search").addEventListener("input", (e) => { query = e.target.value; if (status) renderNodes(); });

// Build time for the footer; it never changes while the page is open.
fetch("healthz").then((r) => r.json()).then((h) => { builtAt = h.built_at !== "unknown" ? h.built_at : null; }).catch(() => {});

refresh();
setInterval(refresh, 5000);
setInterval(renderLive, 1000);
