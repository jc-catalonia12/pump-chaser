import {
  initChartTabs,
  loadPositionChart,
  loadSignalChart,
  renderLightweightChart,
  updateLightweightChartBars,
  renderSignalSetupLegend,
  renderTradingViewWidget,
  activateChartTab,
} from "./charts.js";
import { eventIconHtml, eventMeta, formatEventLabel } from "./event-labels.js";
import { fmtManilaDate, fmtManilaDateTime, fmtManilaTime } from "./time.js";

const API = window.location.origin;

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => document.querySelectorAll(sel);

let ws = null;
let wsReconnectTimer = null;
let pollTimer = null;
let pnlDailyChart = null;
let pnlCumChart = null;
let balanceHistoryChart = null;
let latestEquity = null;
let trainingQualityChart = null;
let trainingRollingChart = null;
let trainingBacktestChart = null;

const CHART_DEFAULTS = {
  color: "#94a3b8",
  borderColor: "rgba(148,163,184,0.1)",
  gridColor: "rgba(148,163,184,0.06)",
};

function pnlClass(n) {
  const v = Number(n);
  if (Number.isNaN(v) || v === 0) return "";
  return v > 0 ? " positive" : " negative";
}

function fmtBuildLabel(health = {}) {
  const parts = [`v${health.version || "?"}`];
  const os = health.build_os || "";
  const arch = health.build_arch || "";
  if (os) parts.push(`${os}${arch ? `-${arch}` : ""}`);
  const unix = Number(health.build_unix || 0);
  if (unix > 0) {
    const d = new Date(unix * 1000);
    const stamp = d.toISOString().slice(0, 16).replace("T", " ") + " UTC";
    parts.push(stamp);
  }
  if (health.git_sha) parts.push(String(health.git_sha));
  return parts.join(" · ");
}

function scoreBar(score, max = 100) {
  const pct = Math.min(100, Math.max(0, (Number(score) / max) * 100));
  return `<div class="score-bar"><span class="mono">${Number(score).toFixed(1)}</span><div class="score-bar-track"><div class="score-bar-fill" style="width:${pct}%"></div></div></div>`;
}

function outcomeClass(o) {
  const v = (o || "pending").toLowerCase();
  if (v === "win") return "outcome-win";
  if (v === "loss" || v === "expired") return "outcome-loss";
  return "outcome-pending";
}

function fmtUsd(n, digits = 2) {
  if (n == null || Number.isNaN(Number(n))) return "—";
  return `$${Number(n).toLocaleString(undefined, { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

function fmtPct(n) {
  if (n == null || Number.isNaN(Number(n))) return "—";
  const v = Number(n);
  const sign = v >= 0 ? "+" : "";
  return `${sign}${v.toFixed(2)}%`;
}

// Auto-dismiss timers keyed by element — cleared when a new message arrives.
const _feedbackTimers = new WeakMap();

function showFeedback(el, message, ok = true, durationMs = null) {
  if (!el) return;
  // Cancel any pending auto-dismiss for this element.
  if (_feedbackTimers.has(el)) {
    const { dismiss, fade } = _feedbackTimers.get(el);
    clearTimeout(dismiss);
    clearTimeout(fade);
    _feedbackTimers.delete(el);
  }
  el.textContent = message;
  el.classList.remove("hidden", "ok", "err", "timed", "dismissing");
  el.classList.add(ok ? "ok" : "err");
  // Default: success 4 s, error 8 s.
  const delay = durationMs ?? (ok ? 4000 : 8000);
  el.style.setProperty("--fb-duration", `${delay / 1000}s`);
  // Force reflow so the CSS animation restarts cleanly.
  void el.offsetWidth;
  el.classList.add("timed");
  // Start fade 400 ms before dismissal.
  const fadeDelay = Math.max(delay - 400, delay * 0.8);
  const fadeTimer = setTimeout(() => el.classList.add("dismissing"), fadeDelay);
  const dismissTimer = setTimeout(() => {
    el.classList.add("hidden");
    el.classList.remove("dismissing", "timed");
    _feedbackTimers.delete(el);
  }, delay);
  _feedbackTimers.set(el, { dismiss: dismissTimer, fade: fadeTimer });
}

export async function apiGet(path) {
  const r = await fetch(`${API}${path}`);
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return r.json();
}

async function apiPost(path, body = null) {
  const r = await fetch(`${API}${path}`, {
    method: "POST",
    headers: body ? { "Content-Type": "application/json" } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return r.json();
}

async function apiPut(path, body) {
  const r = await fetch(`${API}${path}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return r.json();
}

async function apiDelete(path) {
  const r = await fetch(`${API}${path}`, { method: "DELETE" });
  if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
  return r.json();
}

let liveTradingEnabled = false;
let credReplaceMode = false;
let tradingModeSaving = false;
let savedTradingMode = null;
let tradingModeLockUntil = 0; // ms timestamp — ignore WS overrides until this time
let settingsValues = {};
let settingsSections = [];
let activityUnreadCount = 0;
let notificationsOpen = false;
let markingNotificationsSeen = false;

function isEventUnread(e) {
  return e && e.seen === false;
}

function activityItemsHtml(events, limit = 14) {
  if (!events?.length) return "";
  return events
    .slice(0, limit)
    .map((e) => {
      const type = e.event_type || e.type || "event";
      const meta = eventMeta(type);
      const unread = isEventUnread(e);
      return `
    <div class="activity-item${unread ? " notif-unread" : ""}" data-event-id="${e.id ?? ""}">
      <div class="activity-icon activity-icon-${meta.tone}">${eventIconHtml(type)}</div>
      <div class="activity-body">
        <time>${fmtManilaDateTime(e.created_at || e.timestamp)}</time>
        <strong>${formatEventLabel(type)}</strong>
        <p class="activity-desc">${e.message || meta.description}</p>
      </div>
    </div>`;
    })
    .join("");
}

async function markNotificationsSeen({ all = true, ids = [] } = {}) {
  if (markingNotificationsSeen) return;
  markingNotificationsSeen = true;
  try {
    const body = all ? { all: true } : { ids };
    const res = await apiPost("/activity/seen", body);
    activityUnreadCount = res.unread_count ?? 0;
    const events = window.__lastActivityEvents || [];
    if (all) {
      events.forEach((e) => {
        e.seen = true;
      });
    } else {
      const idSet = new Set(ids);
      events.forEach((e) => {
        if (idSet.has(e.id)) e.seen = true;
      });
    }
    window.__lastActivityEvents = events;
    updateNotificationBadge(activityUnreadCount);
    renderNotificationPanel(events);
  } catch {
    /* keep badge until next successful sync */
  } finally {
    markingNotificationsSeen = false;
  }
}

function updateNotificationBadge(unreadCount) {
  const badge = $("#notification-badge");
  if (!badge) return;
  const unread = notificationsOpen ? 0 : Math.max(0, Number(unreadCount) || 0);
  activityUnreadCount = unread;
  badge.textContent = unread > 99 ? "99+" : String(unread);
  badge.classList.toggle("hidden", unread === 0);
}

const NOTIF_PANEL_LIMIT = 10;

function renderNotificationPanel(events) {
  const list = $("#notification-list");
  if (!list) return;
  if (!events?.length) {
    list.innerHTML = '<span class="empty">No notifications yet.</span>';
    list.classList.add("empty");
  } else {
    list.classList.remove("empty");
    list.innerHTML = activityItemsHtml(events, NOTIF_PANEL_LIMIT);
  }
  const showAll = $("#btn-notif-show-all");
  if (showAll) {
    const total = events?.length || 0;
    showAll.classList.toggle("hidden", total <= NOTIF_PANEL_LIMIT);
    showAll.textContent = `Show all (${total})`;
  }
  updateNotificationBadge(activityUnreadCount);
  if (notifModalOpen) renderNotificationsModal();
}

const NOTIF_MODAL_PAGE_SIZE = 15;
let notifModalOpen = false;
let notifModalPage = 1;

function renderNotificationsModal() {
  const list = $("#notifications-modal-list");
  if (!list) return;
  const events = window.__lastActivityEvents || [];
  const total = events.length;
  const totalPages = Math.max(1, Math.ceil(total / NOTIF_MODAL_PAGE_SIZE));
  if (notifModalPage > totalPages) notifModalPage = totalPages;
  if (notifModalPage < 1) notifModalPage = 1;

  if (!total) {
    list.innerHTML = '<span class="empty">No notifications yet.</span>';
    list.classList.add("empty");
  } else {
    list.classList.remove("empty");
    const start = (notifModalPage - 1) * NOTIF_MODAL_PAGE_SIZE;
    const pageItems = events.slice(start, start + NOTIF_MODAL_PAGE_SIZE);
    list.innerHTML = activityItemsHtml(pageItems, NOTIF_MODAL_PAGE_SIZE);
  }

  const bar = $("#notifications-modal-pagination");
  const info = $("#notifications-modal-info");
  const prev = $("#btn-notif-modal-prev");
  const next = $("#btn-notif-modal-next");
  if (bar) bar.classList.toggle("hidden", total === 0);
  if (info) {
    const start = total ? (notifModalPage - 1) * NOTIF_MODAL_PAGE_SIZE + 1 : 0;
    const end = Math.min(notifModalPage * NOTIF_MODAL_PAGE_SIZE, total);
    info.textContent = `Showing ${start}–${end} of ${total} · page ${notifModalPage} of ${totalPages}`;
  }
  if (prev) prev.disabled = notifModalPage <= 1;
  if (next) next.disabled = notifModalPage >= totalPages;
}

function setNotificationsModalOpen(open) {
  notifModalOpen = open;
  const modal = $("#notifications-modal");
  modal?.classList.toggle("hidden", !open);
  document.body.classList.toggle("modal-open", open);
  if (open) {
    notifModalPage = 1;
    setNotificationsOpen(false);
    markNotificationsSeen({ all: true });
    renderNotificationsModal();
  }
}

function setNotificationsOpen(open) {
  notificationsOpen = open;
  const panel = $("#notification-panel");
  const btn = $("#btn-notifications");
  panel?.classList.toggle("hidden", !open);
  btn?.setAttribute("aria-expanded", open ? "true" : "false");
  if (open) {
    markNotificationsSeen({ all: true });
    updateNotificationBadge(0);
  } else {
    updateNotificationBadge(activityUnreadCount);
  }
}

function getNestedValue(obj, key) {
  return key.split(".").reduce((acc, part) => (acc == null ? undefined : acc[part]), obj);
}

function setNestedValue(obj, key, value) {
  const parts = key.split(".");
  let cur = obj;
  for (let i = 0; i < parts.length - 1; i += 1) {
    const p = parts[i];
    if (cur[p] == null || typeof cur[p] !== "object") cur[p] = {};
    cur = cur[p];
  }
  cur[parts[parts.length - 1]] = value;
}

function buildSettingsPatch() {
  const patch = {};
  $$("[data-setting-key]").forEach((el) => {
    const key = el.dataset.settingKey;
    const type = el.dataset.settingType;
    let value;
    if (type === "bool") {
      value = el.checked;
    } else if (type === "select") {
      value = el.value;
    } else if (type === "integer") {
      value = parseInt(el.value, 10);
    } else {
      value = parseFloat(el.value);
    }
    if (Number.isNaN(value)) return;
    setNestedValue(patch, key, value);
  });
  return patch;
}

function renderSettingsField(field, value) {
  const id = `setting-${field.key.replace(/\./g, "-")}`;
  const hint = field.hint ? `<span class="field-hint">${field.hint}</span>` : "";

  if (field.type === "bool") {
    const checked = value ? "checked" : "";
    return `<div class="settings-field">
      <label class="settings-toggle" for="${id}">
        <input type="checkbox" id="${id}" data-setting-key="${field.key}" data-setting-type="bool" ${checked} />
        <span>${field.label}</span>
      </label>
      ${hint}
    </div>`;
  }

  if (field.type === "select") {
    const options = (field.options || [])
      .map((opt) => `<option value="${opt}"${opt === value ? " selected" : ""}>${opt}</option>`)
      .join("");
    return `<div class="settings-field">
      <label for="${id}">${field.label}</label>
      <select id="${id}" data-setting-key="${field.key}" data-setting-type="select">${options}</select>
      ${hint}
    </div>`;
  }

  const step = field.step ?? (field.type === "integer" ? 1 : 0.001);
  const min = field.min != null ? ` min="${field.min}"` : "";
  const max = field.max != null ? ` max="${field.max}"` : "";
  const display = value != null && !Number.isNaN(Number(value)) ? Number(value) : "";
  const inputType = field.type === "integer" ? "integer" : "number";

  return `<div class="settings-field">
    <label for="${id}">${field.label}</label>
    <input type="number" id="${id}" data-setting-key="${field.key}" data-setting-type="${inputType}"
      value="${display}" step="${step}"${min}${max} />
    ${hint}
  </div>`;
}

function renderSettingsSections() {
  const root = $("#settings-sections");
  if (!root) return;
  if (!settingsSections.length) {
    root.innerHTML = '<p class="empty">No editable settings.</p>';
    return;
  }

  root.innerHTML = settingsSections
    .map((section) => {
      const fields = (section.fields || [])
        .map((field) => renderSettingsField(field, getNestedValue(settingsValues, field.key)))
        .join("");
      return `<div class="card settings-section" data-section="${section.id}">
        <div class="card-head">
          <h2>${section.title}</h2>
        </div>
        <p class="hint" style="margin-bottom:0.75rem">${section.description || ""}</p>
        <div class="settings-grid">${fields}</div>
      </div>`;
    })
    .join("");
}

async function loadSettingsTab() {
  const fb = $("#settings-feedback");
  try {
    const data = await apiGet("/config/settings");
    settingsValues = data.values || {};
    settingsSections = data.sections || [];
    $("#settings-path").textContent = data.config_path || "config/settings.yaml";
    $("#settings-note").textContent = data.note || "";
    renderSettingsSections();
    fb?.classList.add("hidden");
  } catch (e) {
    $("#settings-sections").innerHTML = `<p class="empty">Failed to load settings: ${e.message}</p>`;
  }
}

async function saveSettings() {
  const fb = $("#settings-feedback");
  try {
    const patch = buildSettingsPatch();
    const res = await apiPut("/config/settings", { values: patch });
    if (res.error) throw new Error(res.error);
    settingsValues = res.values || settingsValues;
    showFeedback(
      fb,
      res.scanner_restart_recommended
        ? "Saved. Stop and start the scanner for strategy changes to fully apply."
        : "Settings saved to settings.yaml",
      true,
    );
    if (res.live_trading_enabled != null) {
      liveTradingEnabled = !!res.live_trading_enabled;
    }
    await refreshSnapshotHttp();
    await loadUserProfile();
  } catch (e) {
    showFeedback(fb, e.message, false);
  }
}

function setStatusPill(health, risk) {
  const el = $("#status-pill");
  if (!el) return;
  if (!health || health.error) {
    el.textContent = "Offline";
    el.className = "status-chip chip-danger";
    return;
  }
  const scannerOn = !!health.scanner_running;
  const kill = risk?.kill_switch;
  if (kill) {
    el.textContent = "Kill switch";
    el.className = "status-chip chip-danger";
  } else if (scannerOn) {
    el.textContent = "Active";
    el.className = "status-chip chip-success";
  } else {
    el.textContent = "Idle";
    el.className = "status-chip chip-neutral";
  }
}

function renderMetrics(snapshot) {
  const risk = snapshot.risk || {};
  const health = snapshot.health || {};
  const wallet = snapshot.wallet || {};

  const equity = wallet.equity ?? risk.equity;
  if (equity != null && !Number.isNaN(Number(equity))) {
    latestEquity = Number(equity);
  }
  const daily = risk.daily_pnl;
  const dailyPct = risk.daily_pnl_pct;
  const liveMode = !!health.live_trading;
  const dryRun = !!health.dry_run;
  const ordersLive = !!health.exchange_orders_enabled;

  $("#hdr-equity").textContent = fmtUsd(equity);

  const hdrDaily = $("#hdr-daily-pnl");
  hdrDaily.textContent = `${fmtUsd(daily)} (${fmtPct(dailyPct)})`;
  hdrDaily.className = "hmetric-value mono" + pnlClass(daily);

  const maxPos = risk.max_positions ?? 5;
  const posText = `${risk.open_positions ?? 0} / ${maxPos}`;
  $("#m-positions").textContent = posText;
  $("#hdr-positions").textContent = posText;

  // Aggregate unrealized PnL across the open positions in the snapshot, plus a
  // blended ROI% (total PnL relative to total margin used).
  const openPositions = snapshot.positions || [];
  const pnlEl = $("#m-positions-pnl");
  if (pnlEl) {
    if (openPositions.length) {
      let totalPnl = 0;
      let totalMargin = 0;
      for (const p of openPositions) {
        totalPnl += Number(p.unrealized_pnl || 0);
        const csize = Number(p.contract_size || 1) || 1;
        const sz = Number(p.remaining_size ?? p.size ?? 0) * csize;
        const entry = Number(p.entry_price || 0);
        const lev = Number(p.leverage || 1) || 1;
        totalMargin += (entry * sz) / lev;
      }
      const totalPct = totalMargin > 0 ? (totalPnl / totalMargin) * 100 : 0;
      const sign = totalPnl > 0 ? "+" : totalPnl < 0 ? "-" : "";
      pnlEl.textContent = `${sign}${fmtUsd(Math.abs(totalPnl))} (${totalPct > 0 ? "+" : ""}${totalPct.toFixed(2)}%)`;
      pnlEl.className = "kpi-sub mono" + pnlClass(totalPnl);
    } else {
      pnlEl.textContent = "—";
      pnlEl.className = "kpi-sub mono";
    }
  }

  let riskLabel = "Healthy";
  if (risk.kill_switch) riskLabel = "Kill Switch";
  else if (risk.circuit_breaker_active) {
    const rem = risk.circuit_breaker_remaining_sec || 0;
    const mins = Math.ceil(rem / 60);
    riskLabel = `Circuit Breaker (${mins}m)`;
  } else if (risk.trading_paused) riskLabel = "Paused";
  else if (risk.ws_stale) riskLabel = "WS Stale";
  $("#m-risk").textContent = riskLabel;
  // Colour: red for kill/circuit/stale, amber for paused, green for healthy.
  const riskEl = $("#m-risk");
  if (riskEl) {
    riskEl.style.color =
      risk.kill_switch ? "var(--danger, #f55)"
      : risk.circuit_breaker_active || risk.ws_stale ? "#f90"
      : risk.trading_paused ? "#fa0"
      : "var(--profit, #4c9)";
  }
  // Show consecutive losses beside drawdown if non-zero.
  const ddBase = risk.drawdown_pct != null ? `DD ${Number(risk.drawdown_pct).toFixed(1)}%` : "—";
  const consec = risk.consecutive_losses || 0;
  $("#m-drawdown").textContent = consec > 0 ? `${ddBase} · ${consec}L streak` : ddBase;

  const strategy = health.trading_mode || "confluence";
  let modeLabel = liveMode ? "Live" : "Paper";
  if (liveMode && dryRun) modeLabel = "Live · Dry run";
  else if (liveMode && ordersLive) modeLabel = "Live";
  $("#m-mode").textContent = modeLabel;
  $("#m-scanner").textContent = health.scanner_running ? "Scanner running" : "Scanner stopped";
  $("#m-tracked").textContent = String(health.tracked_symbols ?? 0);
  const wsEl = $("#m-ws");
  if (wsEl) {
    if (!health.ws_connected) {
      wsEl.textContent = "WS disconnected";
      wsEl.style.color = "var(--danger, #f55)";
    } else if (risk.ws_stale) {
      wsEl.textContent = "WS stale";
      wsEl.style.color = "#f90";
    } else {
      wsEl.textContent = "WebSocket connected";
      wsEl.style.color = "";
    }
  }

  $("#hdr-strategy").textContent = strategy;
  const hdrMode = $("#hdr-mode");
  if (hdrMode) {
    hdrMode.textContent = modeLabel;
    hdrMode.className = `exec-badge ${liveMode ? "badge-live" : "badge-paper"}`;
  }

  $("#sidebar-meta").textContent = `${health.tracked_symbols ?? 0} symbols · ${modeLabel} · ${strategy}`;
  const buildEl = $("#sidebar-build");
  if (buildEl) {
    const label = fmtBuildLabel(health);
    buildEl.textContent = label;
    buildEl.title = `Build: ${label}`;
  }

  $("#btn-start").disabled = !!health.scanner_running && !risk.kill_switch;
  $("#btn-stop").disabled = !health.scanner_running;
}

function renderActivity(events, unreadCount) {
  window.__lastActivityEvents = events || [];
  if (unreadCount != null) activityUnreadCount = unreadCount;
  renderNotificationPanel(events);
}

function sideFromSignal(s) {
  if (s.side) return String(s.side).toLowerCase();
  return Number(s.price_change_pct) < 0 ? "short" : "long";
}

const SCORE_PASS_THRESHOLD = 64;

function scanScoreBadgeClass(score, action) {
  if ((action || "").toLowerCase() === "signal") return "scan-score-signal";
  const n = Number(score);
  if (!Number.isFinite(n)) return "";
  if (n >= SCORE_PASS_THRESHOLD) return "scan-score-pass";
  if (n >= SCORE_PASS_THRESHOLD - 12) return "scan-score-mid";
  return "scan-score-low";
}

function scanActionClass(action) {
  const a = (action || "").toLowerCase();
  if (a === "signal") return "scan-action-signal";
  if (a === "rejected") return "scan-action-rejected";
  if (a === "warming") return "scan-action-warming";
  if (a === "cooldown") return "scan-action-cooldown";
  return "scan-action-skipped";
}

function scanActionLabel(action) {
  const a = (action || "").toLowerCase();
  if (a === "signal") return "SIGNAL";
  if (a === "rejected") return "PASS";
  if (a === "warming") return "WARMING";
  if (a === "cooldown") return "COOLDOWN";
  return (action || "SCAN").toUpperCase();
}

let lastScanFeedKey = "";

function renderLiveScan(scanEvents, health, fallbackSignals) {
  const el = $("#live-scan-list");
  const statusEl = $("#live-scan-status");
  if (!el) return;

  const running = !!health?.scanner_running;
  const ws = !!health?.ws_connected;
  const tracked = health?.tracked_symbols ?? 0;
  const buffered = health?.scans_buffered ?? 0;
  if (statusEl) {
    if (!running) {
      statusEl.textContent = "Scanner off";
    } else if (!ws) {
      statusEl.textContent = `${tracked} tokens · WS connecting`;
    } else {
      statusEl.textContent = `${tracked} tokens · live · ${buffered} scans`;
    }
  }

  const rows = (scanEvents?.length ? scanEvents : (fallbackSignals || []).map((s) => ({
    symbol: s.symbol,
    action: "signal",
    message: s.message || "Historical signal",
    composite_score: s.composite_score,
    confluence_count: s.confluence_count,
    side: sideFromSignal(s),
    last_price: s.last_price,
    scanned_at: s.generated_at || s.created_at,
    _historical: true,
  }))).slice(0, 20);

  if (!rows.length) {
    el.innerHTML = running
      ? '<span class="empty">Scanner running — waiting for first token analysis…</span>'
      : '<span class="empty">No scans yet — start the scanner.</span>';
    el.classList.add("empty");
    lastScanFeedKey = "";
    return;
  }

  const feedKey = rows.map((r) => `${r.scanned_at}|${r.symbol}|${r.action}|${r.message}`).join(";");
  const isNewBatch = feedKey !== lastScanFeedKey;
  lastScanFeedKey = feedKey;

  el.classList.remove("empty");
  el.innerHTML = rows
    .map((row, idx) => {
      const side = (row.side || "long").toLowerCase();
      const fullAt = fmtManilaDateTime(row.scanned_at);
      const at = fmtManilaTime(row.scanned_at);
      const action = row.action || "scan";
      const scoreNum = row.composite_score != null ? Number(row.composite_score) : null;
      const scoreBadge =
        scoreNum != null && Number.isFinite(scoreNum)
          ? `<span class="scan-score-badge ${scanScoreBadgeClass(scoreNum, action)}" title="Composite score">${scoreNum.toFixed(1)}</span>`
          : "";
      const conf = row.confluence_count != null ? `${row.confluence_count} conf` : "";
      const price = row.last_price != null ? Number(row.last_price).toPrecision(6) : "—";
      const chg = row.change_24h_pct != null ? `${Number(row.change_24h_pct).toFixed(2)}%` : "";
      const fresh = isNewBatch && idx === 0 && !row._historical ? " scan-item-new" : "";
      return `
      <div class="scan-item${fresh}">
        <div class="scan-head">
          <div class="scan-head-main">
            <span class="scan-action ${scanActionClass(action)}">${scanActionLabel(action)}</span>
            ${side === "short" || side === "long" ? `<span class="badge badge-${side}">${side}</span>` : ""}
            <strong>${row.symbol || "—"}</strong>
            ${scoreBadge}
          </div>
          <time class="scan-time" title="${fullAt}">${at || "—"}</time>
        </div>
        <p class="scan-msg">${row.message || "Analyzed"}</p>
        <div class="scan-meta">
          <span>Price <b class="mono">${price}</b>${chg ? ` <em>(${chg})</em>` : ""}</span>
          ${conf ? `<span>${conf}</span>` : ""}
        </div>
      </div>`;
    })
    .join("");
}

function signalScore(s) {
  return s.composite_score ?? s.score ?? 0;
}

const SIGNALS_PAGE_SIZE = 25;
let signalsPage = 1;
let signalsTotal = 0;
let signalsTotalPages = 1;

function signalTime(s) {
  const t = Date.parse(s.generated_at || s.created_at || "");
  return Number.isFinite(t) ? t : 0;
}

function renderSignalsTable(container, signals, { limit, page, pageSize, clickable = false, sortBy = "score", timeCompact = false, serverPaged = false } = {}) {
  if (!container) return { total: 0, page: 1, pageSize: SIGNALS_PAGE_SIZE, totalPages: 1 };

  let rows = signals || [];
  let total = rows.length;
  let currentPage = 1;
  let size = limit ?? pageSize ?? SIGNALS_PAGE_SIZE;

  if (!serverPaged) {
    const sorted = [...rows].sort((a, b) =>
      sortBy === "time" ? signalTime(b) - signalTime(a) : signalScore(b) - signalScore(a)
    );
    total = sorted.length;
    rows = sorted;

    if (page != null && pageSize != null) {
      currentPage = Math.max(1, page);
      size = pageSize;
      const totalPages = Math.max(1, Math.ceil(total / size));
      if (currentPage > totalPages) currentPage = totalPages;
      const start = (currentPage - 1) * size;
      rows = sorted.slice(start, start + size);
    } else if (limit != null) {
      rows = sorted.slice(0, limit);
    }
  }

  if (!rows.length) {
    container.innerHTML = '<span class="empty">No signals yet.</span>';
    container.classList.add("empty");
    return { total, page: currentPage, pageSize: size, totalPages: Math.max(1, Math.ceil(total / size)) };
  }

  container.classList.remove("empty");
  const tableClass = timeCompact ? "data signals-preview-table" : "data";
  container.innerHTML = `
    <table class="${tableClass}">
      <thead>
        <tr>
          <th>Symbol</th>
          <th>Score</th>
          <th>Strategy</th>
          <th>ML %</th>
          <th>Outcome</th>
          <th class="col-time">Time</th>
          ${clickable ? "<th></th>" : ""}
        </tr>
      </thead>
      <tbody>
        ${rows
          .map((s) => {
            const at = s.generated_at || s.created_at || "";
            const timeLabel = timeCompact ? fmtManilaTime(at) : fmtManilaDateTime(at);
            const timeTitle = timeCompact ? fmtManilaDateTime(at) : "";
            const rowClass = clickable ? 'class="clickable"' : "";
            const data = clickable
              ? `data-symbol="${s.symbol}" data-generated-at="${at}"`
              : "";
            return `
          <tr ${rowClass} ${data}>
            <td><strong>${s.symbol || "—"}</strong></td>
            <td>${scoreBar(signalScore(s))}</td>
            <td><span class="tag">${s.strategy || "—"}</span></td>
            <td>${s.setup_probability_pct != null ? `<span class="mono">${s.setup_probability_pct}%</span>` : "—"}</td>
            <td class="${outcomeClass(s.outcome)}">${s.outcome || "pending"}</td>
            <td class="mono col-time"${timeTitle ? ` title="${timeTitle}"` : ""}>${timeLabel}</td>
            ${clickable ? '<td><button type="button" class="btn btn-sm btn-ghost">Chart</button></td>' : ""}
          </tr>`;
          })
          .join("")}
      </tbody>
    </table>`;

  return {
    total,
    page: currentPage,
    pageSize: size,
    totalPages: Math.max(1, Math.ceil(total / size)),
  };
}

function renderSignalsPagination(meta) {
  const bar = $("#signals-pagination");
  if (!bar || !meta) return;
  const { total, page, pageSize, totalPages } = meta;
  if (!total) {
    bar.classList.add("hidden");
    return;
  }
  bar.classList.remove("hidden");
  const start = (page - 1) * pageSize + 1;
  const end = Math.min(page * pageSize, total);
  const info = $("#signals-page-info");
  if (info) {
    info.textContent = `Showing ${start}–${end} of ${total} · page ${page} of ${totalPages}`;
  }
  const prev = $("#btn-signals-prev");
  const next = $("#btn-signals-next");
  if (prev) prev.disabled = page <= 1;
  if (next) next.disabled = page >= totalPages;
}


function renderPositionsTable(container, positions, { clickable = false } = {}) {
  if (!container) return;
  const rows = positions || [];
  if (!rows.length) {
    container.innerHTML = '<span class="empty">No open positions.</span>';
    container.classList.add("empty");
    return;
  }
  container.classList.remove("empty");
  container.innerHTML = `
    <table class="data">
      <thead>
        <tr>
          <th>Symbol</th>
          <th>Side</th>
          <th>Entry</th>
          <th>Mark</th>
          <th>PnL</th>
          <th>PnL %</th>
          <th>Size</th>
          <th>SL</th>
          <th>Lev</th>
          <th>Paper</th>
          <th>Strategy</th>
          ${clickable ? '<th class="col-actions">Actions</th>' : ""}
        </tr>
      </thead>
      <tbody>
        ${rows
          .map((p) => {
            const side = (p.side || "").toLowerCase();
            const badge = side === "long" ? "badge-long" : "badge-short";
            const data = clickable ? `data-position-id="${p.id}" class="clickable"` : "";
            const pnl = Number(p.unrealized_pnl || 0);
            const roiPct = Number(p.unrealized_roi_pct ?? p.unrealized_pnl_pct ?? 0);
            const movePct = Number(p.unrealized_pnl_pct || 0);
            const roiSign = roiPct > 0 ? "+" : "";
            const pnlText = `${pnl < 0 ? "-" : pnl > 0 ? "+" : ""}${fmtUsd(Math.abs(pnl))}`;
            const mark = Number(p.mark_price || p.entry_price || 0);
            return `
          <tr ${data}>
            <td><strong>${p.symbol}</strong></td>
            <td><span class="badge ${badge}">${p.side}</span></td>
            <td>${Number(p.entry_price || 0).toPrecision(6)}</td>
            <td class="mono">${mark.toPrecision(6)}</td>
            <td class="mono ${pnlClass(pnl)}">${pnlText}</td>
            <td class="mono ${pnlClass(roiPct)}" title="Price move ${movePct.toFixed(2)}%">${roiSign}${roiPct.toFixed(2)}%</td>
            <td class="mono">${Number(p.remaining_size ?? p.size ?? 0).toFixed(4)}</td>
            <td class="mono">${Number(p.stop_loss || 0).toPrecision(6)}</td>
            <td class="mono">${p.leverage ?? "—"}×</td>
            <td>${p.paper ? '<span class="tag">paper</span>' : '<span class="badge badge-live">live</span>'}</td>
            <td>${p.strategy || "—"}</td>
            ${
              clickable
                ? `<td class="actions-cell">
              <button type="button" class="icon-btn" data-position-id="${p.id}" title="View chart" aria-label="View chart">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M3 3v18h18"/><path d="M7 16l4-8 4 5 5-9"/></svg>
              </button>
              <button type="button" class="icon-btn icon-btn-danger" data-close-position-id="${p.id}" title="Close at market" aria-label="Close position">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>
              </button>
            </td>`
                : ""
            }
          </tr>`;
          })
          .join("")}
      </tbody>
    </table>`;
}

function getActivePositionsView() {
  return document.querySelector('input[name="positions-view"]:checked')?.value || "open";
}

function getPositionsHistoryFilter() {
  return document.querySelector('input[name="positions-history-mode"]:checked')?.value || "all";
}

function renderPositionsHistoryTable(container, positions) {
  if (!container) return;
  const rows = positions || [];
  if (!rows.length) {
    container.innerHTML = '<span class="empty">No closed positions yet.</span>';
    container.classList.add("empty");
    return;
  }
  container.classList.remove("empty");
  container.innerHTML = `
    <table class="data">
      <thead>
        <tr>
          <th>Symbol</th>
          <th>Side</th>
          <th>Entry</th>
          <th>Exit</th>
          <th>PnL</th>
          <th>PnL %</th>
          <th>Size</th>
          <th>Lev</th>
          <th>Mode</th>
          <th>Reason</th>
          <th>Closed</th>
          <th class="col-actions">Chart</th>
        </tr>
      </thead>
      <tbody>
        ${rows
          .map((p) => {
            const side = (p.side || "").toLowerCase();
            const badge = side === "long" ? "badge-long" : "badge-short";
            const pnl = Number(p.realized_pnl || 0);
            const roiPct = Number(p.realized_pnl_pct ?? 0);
            const roiSign = roiPct > 0 ? "+" : "";
            const pnlText = `${pnl < 0 ? "-" : pnl > 0 ? "+" : ""}${fmtUsd(Math.abs(pnl))}`;
            const entry = Number(p.entry_price || 0);
            const exit = Number(p.exit_price || 0);
            const closedAt = p.closed_at ? fmtManilaDateTime(p.closed_at) : "—";
            const reason = p.exit_reason || "—";
            return `
          <tr data-position-id="${p.id}" class="clickable">
            <td><strong>${p.symbol}</strong></td>
            <td><span class="badge ${badge}">${p.side}</span></td>
            <td class="mono">${entry ? entry.toPrecision(6) : "—"}</td>
            <td class="mono">${exit ? exit.toPrecision(6) : "—"}</td>
            <td class="mono ${pnlClass(pnl)}">${pnlText}</td>
            <td class="mono ${pnlClass(roiPct)}">${roiSign}${roiPct.toFixed(2)}%</td>
            <td class="mono">${Number(p.size ?? 0).toFixed(4)}</td>
            <td class="mono">${p.leverage ?? "—"}×</td>
            <td>${p.paper ? '<span class="tag">paper</span>' : '<span class="badge badge-live">live</span>'}</td>
            <td>${reason}</td>
            <td><time>${closedAt}</time></td>
            <td class="actions-cell">
              <button type="button" class="icon-btn" data-position-id="${p.id}" title="View chart" aria-label="View chart">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M3 3v18h18"/><path d="M7 16l4-8 4 5 5-9"/></svg>
              </button>
            </td>
          </tr>`;
          })
          .join("")}
      </tbody>
    </table>`;
}

function applySnapshot(snapshot) {
  if (!snapshot || snapshot.error) return;
  renderMetrics(snapshot);
  setStatusPill(snapshot.health, snapshot.risk);
  syncTradingModeUI(snapshot.health || {});
  renderActivity(snapshot.activity, snapshot.activity_unread);
  renderLiveScan(snapshot.scan_events, snapshot.health, snapshot.signals);
  renderSignalsTable($("#trading-signals-preview"), snapshot.signals, { limit: 8, timeCompact: true });
  syncOpenPositions(snapshot.positions);
  $("#last-updated").textContent = fmtManilaTime(new Date());
}

let lastPositionsSig = null;

// Keep the Positions tab table live from snapshot data (open/close + PnL updates)
// without a manual refresh. Re-render only when the set of positions or their
// live PnL actually changes to avoid clobbering the table on every tick.
function syncOpenPositions(positions) {
  if (!Array.isArray(positions)) return;
  const panelActive = $("#panel-positions")?.classList.contains("active");
  if (!panelActive || getActivePositionsView() !== "open") {
    // Remember count so the next activation/refresh is correct; cheap no-op.
    lastPositionsSig = positions.map((p) => p.id).join(",");
    return;
  }
  const sig = positions
    .map((p) => `${p.id}:${Number(p.unrealized_pnl ?? 0).toFixed(4)}:${p.status || ""}`)
    .join("|");
  if (sig === lastPositionsSig) return;
  lastPositionsSig = sig;
  renderPositionsTable($("#positions-table"), positions, { clickable: true });
}

function syncTradingModeUI(health = {}) {
  if (health.live_trading_enabled != null) {
    liveTradingEnabled = !!health.live_trading_enabled;
  }
  const liveActive = !!health.live_trading;
  const paperRadio = $('input[name="trading-mode"][value="paper"]');
  const liveRadio = $('input[name="trading-mode"][value="live"]');
  const hint = $("#trading-mode-hint");
  const liveOption = liveRadio?.closest(".mode-option");

  const modeLocked = tradingModeSaving || Date.now() < tradingModeLockUntil;
  if (paperRadio && liveRadio && !modeLocked) {
    paperRadio.checked = !liveActive;
    liveRadio.checked = liveActive;
    savedTradingMode = liveActive ? "live" : "paper";
  }

  if (liveOption) {
    const hasCreds = !!health.has_api_credentials;
    const disabled = !liveTradingEnabled || !hasCreds;
    liveOption.classList.toggle("disabled", disabled);
    liveRadio.disabled = disabled;
  }

  if (hint) {
    if (!liveTradingEnabled) {
      hint.textContent = "Live orders disabled in config — paper only";
    } else if (!health.has_api_credentials) {
      hint.textContent = "Add API keys below to enable live mode";
    } else if (liveActive && health.dry_run) {
      hint.textContent = "Dry run ON — orders are logged but not sent to MEXC. Disable in Settings → Execution.";
    } else if (liveActive && health.exchange_orders_enabled) {
      hint.textContent = "Real MEXC orders — use with caution";
    } else if (liveActive) {
      hint.textContent = "Live mode selected — check API keys and execution settings";
    } else {
      hint.textContent = "Simulated fills — no real orders";
    }
  }
}

async function saveTradingMode(mode) {
  const hint = $("#trading-mode-hint");
  if (mode === savedTradingMode) return;

  if (mode === "live" && !liveTradingEnabled) {
    showFeedback(hint, "Live trading is disabled in server config", false);
    syncTradingModeUI({ live_trading: false, has_api_credentials: true });
    return;
  }

  if (mode === "live") {
    try {
      const profile = await apiGet("/user/profile");
      if (!profile.credentials?.has_credentials) {
        showFeedback(hint, "Add API keys in Account before enabling live mode", false);
        syncTradingModeUI({ live_trading: false, has_api_credentials: false });
        return;
      }
    } catch (e) {
      showFeedback(hint, e.message, false);
      return;
    }
  }

  tradingModeSaving = true;
  try {
    const res = await apiPut("/user/credentials", { execution_mode: mode });
    if (res.error) throw new Error(res.error);
    savedTradingMode = mode;
    tradingModeLockUntil = Date.now() + 4000; // hold off stale WS frames for 4 s
    if (res.dry_run_disabled) {
      showFeedback(hint, "Dry run disabled — real MEXC orders are now enabled", true);
    }
    await refreshSnapshotHttp();
    if ($("#panel-account")?.classList.contains("active")) await loadAccountTab();
  } catch (e) {
    showFeedback(hint, e.message, false);
    syncTradingModeUI({
      live_trading: savedTradingMode === "live",
      has_api_credentials: true,
    });
  } finally {
    tradingModeSaving = false;
  }
}

function renderCredentialsState(creds = {}, hasCreds = false) {
  const connected = $("#cred-connected");
  const form = $("#credentials-form");
  const keyMasked = $("#cred-key-masked");
  const secretMasked = $("#cred-secret-masked");

  if (hasCreds && !credReplaceMode) {
    connected?.classList.remove("hidden");
    form?.classList.add("hidden");
    if (keyMasked) keyMasked.textContent = creds.mexc_api_key_masked || "********";
    if (secretMasked) secretMasked.textContent = creds.mexc_api_secret_masked || "********";
  } else {
    connected?.classList.add("hidden");
    form?.classList.remove("hidden");
    if (hasCreds && credReplaceMode) {
      $("#cred-key").placeholder = creds.mexc_api_key_masked || "Enter new API key";
      $("#cred-secret").placeholder = "Enter new API secret";
    } else {
      $("#cred-key").placeholder = "MEXC API key";
      $("#cred-secret").placeholder = "MEXC API secret";
    }
  }
}

async function refreshSnapshotHttp() {
  try {
    const data = await apiGet("/live/snapshot");
    applySnapshot(data);
    setWsUi(true);
  } catch {
    setWsUi(false);
    setStatusPill({ error: true });
  }
}

function setWsUi(connected) {
  const dot = $("#ws-dot");
  const label = $("#ws-label");
  if (connected) {
    dot?.classList.add("on");
    if (label) label.textContent = "WebSocket live";
  } else {
    dot?.classList.remove("on");
    if (label) label.textContent = "HTTP fallback";
  }
}

function connectWebSocket() {
  if (ws) {
    ws.close();
    ws = null;
  }
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  ws = new WebSocket(`${proto}//${window.location.host}/ws`);
  ws.onopen = () => setWsUi(true);
  ws.onmessage = (ev) => {
    try {
      applySnapshot(JSON.parse(ev.data));
      setWsUi(true);
    } catch {
      /* ignore */
    }
  };
  ws.onclose = () => {
    setWsUi(false);
    if (wsReconnectTimer) clearTimeout(wsReconnectTimer);
    wsReconnectTimer = setTimeout(connectWebSocket, 3000);
  };
  ws.onerror = () => setWsUi(false);
}

function startPolling() {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = setInterval(() => {
    if ($("#auto-refresh")?.checked) refreshSnapshotHttp();
  }, 2000);
}

// Live candle refresh for the open Overlay chart (TradingView streams on its own).
const CHART_REFRESH_MS = 5000;
let signalChartTimer = null;
let positionChartTimer = null;

function stopChartTimer(which) {
  if (which === "signal" && signalChartTimer) {
    clearInterval(signalChartTimer);
    signalChartTimer = null;
  }
  if (which === "position" && positionChartTimer) {
    clearInterval(positionChartTimer);
    positionChartTimer = null;
  }
}

async function showSignalChart(symbol, generatedAt) {
  const modal = $("#signal-chart-modal");
  modal?.classList.remove("hidden");
  document.body.classList.add("modal-open");
  activateChartTab("signal-chart-tabs", "lwc");
  $("#signal-chart-title").textContent = `Signal · ${symbol}`;
  stopChartTimer("signal");
  try {
    const data = await loadSignalChart(apiGet, symbol, generatedAt);
    if (data.error) throw new Error(data.error);
    renderSignalSetupLegend("signal-chart-legend", data);
    renderTradingViewWidget("signal-tv-chart", data.tv_symbol, data.interval);
    renderLightweightChart("signal-lwc-chart", data);

    signalChartTimer = setInterval(async () => {
      if ($("#signal-chart-modal")?.classList.contains("hidden")) {
        stopChartTimer("signal");
        return;
      }
      try {
        const fresh = await loadSignalChart(apiGet, symbol, generatedAt);
        if (!fresh.error) {
          renderSignalSetupLegend("signal-chart-legend", fresh);
          updateLightweightChartBars("signal-lwc-chart", fresh);
        }
      } catch {
        /* transient — keep last candles */
      }
    }, CHART_REFRESH_MS);
  } catch (e) {
    $("#signal-lwc-chart").innerHTML = `<span class="empty">${e.message}</span>`;
    $("#signal-chart-legend").innerHTML = `<p class="empty">${e.message}</p>`;
  }
}

function closeSignalChartModal() {
  $("#signal-chart-modal")?.classList.add("hidden");
  document.body.classList.remove("modal-open");
  stopChartTimer("signal");
}

async function showPositionChart(positionId, symbol) {
  const modal = $("#position-chart-modal");
  modal?.classList.remove("hidden");
  document.body.classList.add("modal-open");
  activateChartTab("position-chart-tabs", "lwc");
  $("#position-chart-title").textContent = `Position · ${symbol} (#${positionId})`;
  stopChartTimer("position");
  try {
    const data = await loadPositionChart(apiGet, positionId);
    if (data.error) throw new Error(data.error);
    renderSignalSetupLegend("position-chart-legend", data);
    renderTradingViewWidget("position-tv-chart", data.tv_symbol, data.interval);
    renderLightweightChart("position-lwc-chart", data);

    positionChartTimer = setInterval(async () => {
      if ($("#position-chart-modal")?.classList.contains("hidden")) {
        stopChartTimer("position");
        return;
      }
      try {
        const fresh = await loadPositionChart(apiGet, positionId);
        if (!fresh.error) {
          renderSignalSetupLegend("position-chart-legend", fresh);
          updateLightweightChartBars("position-lwc-chart", fresh);
        }
      } catch {
        /* transient — keep last candles */
      }
    }, CHART_REFRESH_MS);
  } catch (e) {
    $("#position-lwc-chart").innerHTML = `<span class="empty">${e.message}</span>`;
    $("#position-chart-legend").innerHTML = `<p class="empty">${e.message}</p>`;
  }
}

function closePositionChartModal() {
  $("#position-chart-modal")?.classList.add("hidden");
  document.body.classList.remove("modal-open");
  stopChartTimer("position");
}

async function loadSignalsPage(page = 1) {
  const offset = (Math.max(1, page) - 1) * SIGNALS_PAGE_SIZE;
  const data = await apiGet(`/signals?limit=${SIGNALS_PAGE_SIZE}&offset=${offset}`);
  signalsPage = data.page ?? page;
  signalsTotal = data.total ?? 0;
  signalsTotalPages = data.total_pages ?? 1;
  renderSignalsTable($("#signals-table"), data.signals || [], {
    clickable: true,
    sortBy: "time",
    serverPaged: true,
  });
  renderSignalsPagination({
    total: signalsTotal,
    page: signalsPage,
    pageSize: SIGNALS_PAGE_SIZE,
    totalPages: signalsTotalPages,
  });
}

async function loadSignalsTab() {
  try {
    await loadSignalsPage(1);
  } catch (e) {
    $("#signals-table").innerHTML = `<span class="empty">Error: ${e.message}</span>`;
    $("#signals-pagination")?.classList.add("hidden");
  }
}

async function loadPositionsOpenTab() {
  const data = await apiGet("/positions");
  const positions = data.positions || [];
  renderPositionsTable($("#positions-table"), positions, { clickable: true });
  lastPositionsSig = positions
    .map((p) => `${p.id}:${Number(p.unrealized_pnl ?? 0).toFixed(4)}:${p.status || ""}`)
    .join("|");
}

async function loadPositionsHistoryTab() {
  const filter = getPositionsHistoryFilter();
  const qs = filter === "all" ? "" : `?paper=${encodeURIComponent(filter)}`;
  const data = await apiGet(`/positions/history${qs}`);
  const positions = data.positions || [];
  renderPositionsHistoryTable($("#positions-history-table"), positions);
  const meta = $("#positions-history-meta");
  if (meta) {
    const total = Number(data.total ?? positions.length);
    const shown = positions.length;
    const label = filter === "all" ? "all trades" : `${filter} trades`;
    if (total) {
      meta.textContent = `Showing ${shown} of ${total} closed ${label}`;
      meta.classList.remove("hidden");
    } else {
      meta.textContent = `No closed ${label}`;
      meta.classList.remove("hidden");
    }
  }
}

async function loadPositionsTab() {
  try {
    if (getActivePositionsView() === "history") {
      await loadPositionsHistoryTab();
    } else {
      await loadPositionsOpenTab();
    }
  } catch (e) {
    const target =
      getActivePositionsView() === "history" ? "#positions-history-table" : "#positions-table";
    const el = $(target);
    if (el) el.innerHTML = `<span class="empty">Error: ${e.message}</span>`;
  }
}

function setPositionsView(view) {
  const openView = $("#positions-open-view");
  const historyView = $("#positions-history-view");
  if (openView) openView.classList.toggle("hidden", view !== "open");
  if (historyView) historyView.classList.toggle("hidden", view !== "history");
  $$(".positions-view-nav .seg").forEach((seg) => {
    const input = seg.querySelector('input[name="positions-view"]');
    seg.classList.toggle("active", input?.value === view);
  });
}

// Native window.confirm() is unreliable inside the Tauri/WKWebView shell (the
// dialog often never renders and returns false), which made action buttons
// appear to do nothing. This is a self-contained promise-based replacement.
function confirmDialog(message, { confirmLabel = "Confirm", danger = false } = {}) {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "chart-modal";
    overlay.setAttribute("role", "dialog");
    overlay.setAttribute("aria-modal", "true");
    overlay.innerHTML = `
      <div class="chart-modal-backdrop" data-confirm-cancel></div>
      <div class="chart-modal-dialog card" style="max-width: 420px; margin-top: 0;">
        <p style="margin: 0 0 1rem; line-height: 1.4;">${message}</p>
        <div class="btn-row" style="justify-content: flex-end;">
          <button type="button" class="btn btn-ghost" data-confirm-cancel>Cancel</button>
          <button type="button" class="btn ${danger ? "btn-danger" : "btn-primary"}" data-confirm-ok>${confirmLabel}</button>
        </div>
      </div>`;
    const cleanup = (result) => {
      document.removeEventListener("keydown", onKey);
      overlay.remove();
      resolve(result);
    };
    const onKey = (e) => {
      if (e.key === "Escape") cleanup(false);
      else if (e.key === "Enter") cleanup(true);
    };
    overlay.addEventListener("click", (e) => {
      if (e.target.closest("[data-confirm-ok]")) cleanup(true);
      else if (e.target.closest("[data-confirm-cancel]")) cleanup(false);
    });
    document.addEventListener("keydown", onKey);
    document.body.appendChild(overlay);
    overlay.querySelector("[data-confirm-ok]")?.focus();
  });
}

async function closePositionManually(positionId, symbol = "") {
  const fb = $("#positions-feedback");
  const label = symbol ? `${symbol} (#${positionId})` : `#${positionId}`;
  const ok = await confirmDialog(
    `Close position <strong>${label}</strong> at current market price?`,
    { confirmLabel: "Close position", danger: true },
  );
  if (!ok) return;
  try {
    showFeedback(fb, `Closing ${label}…`, true);
    const res = await apiPost(`/positions/${positionId}/close`);
    if (res.error) throw new Error(res.error);
    const pnl = Number(res.pnl || 0);
    const sign = pnl > 0 ? "+" : "";
    showFeedback(fb, `Closed ${res.symbol || symbol} — PnL ${sign}${fmtUsd(pnl)}`, true);
    closePositionChartModal();
    await loadPositionsTab();
  } catch (e) {
    showFeedback(fb, e.message, false);
  }
}

function gateRow(name, passed, current, target, notes = "") {
  return { gate: name, status: passed ? "✅ Pass" : "⏳ Pending", current, target, notes };
}

async function loadReadinessTab() {
  try {
    const [health, risk, learning, statsResp] = await Promise.all([
      apiGet("/health"),
      apiGet("/risk"),
      apiGet("/learning/status"),
      apiGet("/signals/stats"),
    ]);

    const paperMode = !!health.paper_trading;
    const liveMode = !!health.live_trading && !paperMode;
    const scannerOn = !!health.scanner_running;
    const killOn = !!risk.kill_switch;
    const paused = !!risk.trading_paused;

    const strategies = statsResp.strategies || [];
    const conf = strategies.find((s) => (s.strategy || "").toLowerCase() === "confluence") || {};
    const confSetupResolved = Number(conf.total_resolved || conf.resolved || 0);
    const confSetupWinRate = Number(conf.win_rate || 0);
    const confTrade = learning.confluence_trade_stats || {};
    const confTradeCount = Number(confTrade.total || confTrade.total_trades || 0);
    const confTradeWinRate = Number(confTrade.win_rate || 0);
    const confProfitFactor = Number(confTrade.profit_factor || 0);
    const dailyPnlUsd = Number(risk.daily_pnl || 0);
    const dailyPnlPct = Number(risk.daily_pnl_pct || 0);

    const gates = [
      gateRow(
        "Scanner + risk healthy",
        scannerOn && !killOn && !paused,
        `scanner=${scannerOn ? "on" : "off"}, kill=${killOn ? "on" : "off"}`,
        "scanner on, kill off"
      ),
      gateRow("Confluence setup sample size", confSetupResolved >= 50, String(confSetupResolved), ">= 50"),
      gateRow(
        "Confluence setup win rate",
        confSetupWinRate >= 0.5,
        `${(confSetupWinRate * 100).toFixed(1)}%`,
        ">= 50%"
      ),
      gateRow("Closed confluence trades", confTradeCount >= 30, String(confTradeCount), ">= 30"),
      gateRow(
        "Confluence profit factor",
        confProfitFactor >= 1.2,
        confProfitFactor.toFixed(2),
        ">= 1.20"
      ),
      gateRow(
        "Confluence trade win rate",
        confTradeWinRate >= 0.5,
        `${(confTradeWinRate * 100).toFixed(1)}%`,
        ">= 50%"
      ),
      gateRow(
        "Daily risk containment",
        dailyPnlPct > -4,
        `${fmtUsd(dailyPnlUsd)} (${fmtPct(dailyPnlPct)})`,
        "> -4%"
      ),
    ];

    const passed = gates.filter((g) => g.status.startsWith("✅")).length;
    const total = gates.length;
    const progress = total ? passed / total : 0;
    const liveReady = passed >= 6;

    let phase;
    let phaseMsg;
    if (liveMode && liveReady) {
      phase = "Phase 3 — Live Micro";
      phaseMsg = "Live on and core gates pass. Keep size small.";
    } else if (liveReady) {
      phase = "Phase 2 — Ready for Live Micro";
      phaseMsg = "Paper evidence strong enough for micro-live.";
    } else if (paperMode) {
      phase = "Phase 1 — Paper Validation";
      phaseMsg = "Stay in paper and build sample size.";
    } else {
      phase = "Phase 0 — Setup";
      phaseMsg = "Fix scanner/risk health first.";
    }

    $("#rd-phase").textContent = phase;
    $("#rd-score").textContent = `${passed}/${total}`;
    $("#rd-setup-wr").textContent = `${(confSetupWinRate * 100).toFixed(1)}%`;
    $("#rd-pf").textContent = confProfitFactor.toFixed(2);
    $("#rd-progress").style.width = `${progress * 100}%`;

    const ring = $("#rd-score-ring");
    if (ring) {
      ring.style.borderColor = progress >= 0.75 ? "var(--success)" : progress >= 0.4 ? "var(--accent)" : "var(--warning)";
    }
    $("#rd-phase-msg").textContent = phaseMsg;

    $("#readiness-gates").innerHTML = gates
      .map((g) => {
        const pass = g.status.startsWith("✅");
        return `<div class="gate-card ${pass ? "pass" : "pending"}">
          <div class="gate-name">${g.gate}</div>
          <div class="gate-status">${pass ? "Passed" : "Pending"}</div>
          <div class="gate-detail">${g.current} → ${g.target}</div>
        </div>`;
      })
      .join("");

    $("#readiness-mode").innerHTML = `
      <table class="data"><tbody>
        <tr><td>Trading mode</td><td>${health.trading_mode || "—"}</td></tr>
        <tr><td>Execution</td><td>${liveMode ? "LIVE" : "PAPER"}</td></tr>
        <tr><td>Max risk / trade</td><td>${Number(risk.max_risk_per_trade_pct || 0).toFixed(2)}%</td></tr>
        <tr><td>Open positions</td><td>${risk.open_positions || 0}/${risk.max_positions || 0}</td></tr>
      </tbody></table>`;

    renderModelLearning(learning.model || {});
  } catch (e) {
    $("#readiness-gates").innerHTML = `<span class="empty">Error: ${e.message}</span>`;
  }
}

function renderModelLearning(model) {
  const el = $("#readiness-model");
  if (!el) return;
  const om = model.online_model || {};
  const active = model.active_model || "warming_up";
  const samples = Number(om.samples || 0);
  const wins = Number(om.wins || 0);
  const losses = Number(om.losses || 0);
  const minSamples = Number(om.min_samples || 0);
  const winRate = om.win_rate != null ? `${(Number(om.win_rate) * 100).toFixed(1)}%` : "—";
  const acc = om.recent_accuracy != null ? `${(Number(om.recent_accuracy) * 100).toFixed(1)}%` : "—";
  const ready = !!om.ready;
  const activeLabel =
    active === "online"
      ? "Online (self-trained)"
      : active === "onnx"
      ? "ONNX fallback"
      : "Warming up";
  const statusBadge = ready
    ? '<span style="color: var(--success); font-weight: 600;">live &amp; learning</span>'
    : `<span style="color: var(--warning); font-weight: 600;">collecting (${samples}/${minSamples})</span>`;
  el.innerHTML = `
    <table class="data"><tbody>
      <tr><td>Active model</td><td>${activeLabel}</td></tr>
      <tr><td>Status</td><td>${statusBadge}</td></tr>
      <tr><td>Trades learned</td><td>${samples} (${wins}W / ${losses}L)</td></tr>
      <tr><td>Model win rate</td><td>${winRate}</td></tr>
      <tr><td>Recent accuracy</td><td>${acc} <span class="hint">(last ${Number(om.recent_window || 0)} trades)</span></td></tr>
      <tr><td>Updated</td><td>${om.updated_at ? fmtManilaDateTime(om.updated_at) : "—"}</td></tr>
    </tbody></table>`;
}

// --- Training / Models ---

function pctText(v) {
  return v == null || Number.isNaN(Number(v))
    ? "—"
    : `${(Number(v) * 100).toFixed(1)}%`;
}

async function loadTrainingTab() {
  clearTrainingError();
  setTrainingLoading(true);
  try {
    const data = await apiGet("/ml/history");
    renderTrainingModels(data);
    try {
      await renderTrainingChartsDeferred(data.history || [], data.rolling_7d || []);
    } catch (chartErr) {
      console.error("Training charts failed:", chartErr);
    }
  } catch (e) {
    showTrainingError(e.message || String(e));
  } finally {
    setTrainingLoading(false);
  }
}

function setTrainingLoading(loading) {
  const ids = [
    "to-postgatewinrate",
    "training-gate-accuracy",
    "training-recent-accuracy",
    "training-active",
  ];
  if (loading) {
    ids.forEach((id) => {
      const el = $(`#${id}`);
      if (el) el.textContent = "…";
    });
    const gate = $("#training-gate-stats");
    if (gate) gate.innerHTML = '<span class="empty">Loading…</span>';
    const side = $("#training-side-stats");
    if (side) side.innerHTML = '<span class="empty">Loading…</span>';
  }
}

function showTrainingError(message) {
  const banner = $("#training-error");
  if (banner) {
    banner.textContent = `Could not load training data: ${message}. Check that the bot is running, then click Refresh.`;
    banner.classList.remove("hidden");
  }
  const gate = $("#training-gate-stats");
  if (gate) gate.innerHTML = `<span class="empty negative">${message}</span>`;
}

function clearTrainingError() {
  const banner = $("#training-error");
  if (banner) {
    banner.textContent = "";
    banner.classList.add("hidden");
  }
}

function renderTrainingChartsDeferred(history, rolling7d) {
  return new Promise((resolve) => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        renderTrainingCharts(history, rolling7d);
        resolve();
      });
    });
  });
}

function renderTrainingModels(data) {
  const model = data.model || {};
  const om = model.online_model || {};
  const onnx = data.onnx || {};
  const cfg = data.config || {};
  const active = model.active_model || "warming_up";
  const pg = (data.postgate_stats || {}).post_gate || {};
  const threshold = cfg.supervised_threshold ?? om.gate_threshold;
  const thresholdPct = threshold != null ? (Number(threshold) * 100).toFixed(0) : "—";

  const activeLabel =
    active === "online"
      ? "Online"
      : active === "onnx"
        ? "ONNX fallback"
        : "Warming up";

  const samples = Number(om.samples || 0);
  const minSamples = Number(om.min_samples || cfg.min_training_samples || 0);
  const ready = !!om.ready;

  const activeEl = $("#training-active");
  if (activeEl) activeEl.textContent = activeLabel;

  const onlineBadge = $("#training-online-badge");
  if (onlineBadge) {
    onlineBadge.textContent = ready ? "live & learning" : `collecting ${samples}/${minSamples}`;
    onlineBadge.className = "tag " + (ready ? "tag-ok" : "tag-warn");
  }

  const samplesHint = $("#training-samples-hint");
  if (samplesHint) {
    samplesHint.textContent = `${samples.toLocaleString()} training samples`;
  }

  const pgEl = $("#to-postgatewinrate");
  const pgSub = $("#to-postgate-resolved");
  if (pgEl) {
    pgEl.textContent = pg.win_rate != null ? pctText(pg.win_rate) : "—";
    pgEl.className = "kpi-value mono " + (Number(pg.win_rate) >= 0.5 ? "positive" : "");
  }
  if (pgSub) {
    pgSub.textContent = `${pg.win ?? 0}W / ${pg.loss ?? 0}L at ≥${thresholdPct}%`;
  }

  const gateAccEl = $("#training-gate-accuracy");
  const gateThreshEl = $("#training-gate-threshold");
  const gateAcc = om.gate_accuracy;
  if (gateAccEl) {
    gateAccEl.textContent = gateAcc != null ? pctText(gateAcc) : "—";
    gateAccEl.className = "kpi-value mono " + (gateAcc != null && Number(gateAcc) >= 0.6 ? "positive" : "");
  }
  if (gateThreshEl) gateThreshEl.textContent = `At ${thresholdPct}% gate`;

  const recentAccEl = $("#training-recent-accuracy");
  const recentWinEl = $("#training-recent-window");
  if (recentAccEl) {
    recentAccEl.textContent = om.recent_accuracy != null ? pctText(om.recent_accuracy) : "—";
  }
  if (recentWinEl) {
    recentWinEl.textContent = `Last ${Number(om.recent_window || 0)} hard-label samples`;
  }

  const onnxLoaded = !!onnx.loaded;
  const onnxBadge = $("#training-onnx-badge");
  if (onnxBadge) {
    onnxBadge.textContent = onnxLoaded ? "ONNX backup loaded" : "ONNX not loaded";
    onnxBadge.className = "tag " + (onnxLoaded ? "tag-ok" : "tag-warn");
  }

  const gateStats = $("#training-gate-stats");
  if (gateStats) {
    gateStats.innerHTML = `
      <table class="data"><tbody>
        <tr><td>ML threshold</td><td class="mono">≥ ${thresholdPct}%</td></tr>
        <tr><td>Hard gate</td><td>${cfg.hard_ml_gate ? "On" : "Off"}</td></tr>
        <tr><td>Trade learn weights</td><td class="mono">${cfg.trade_win_weight ?? 2}× win / ${cfg.trade_loss_weight ?? 3.5}× loss</td></tr>
        <tr><td>Shadow ML rejects</td><td class="mono">${data.learning?.shadow_ml_reject_weight ?? "—"}×</td></tr>
        <tr><td>Last updated</td><td>${om.updated_at ? fmtManilaDateTime(om.updated_at) : "—"}</td></tr>
      </tbody></table>`;
  }

  const ss = data.side_stats || {};
  const sides = [ss.long, ss.short].filter(Boolean);
  const sideEl = $("#training-side-stats");
  if (sideEl) {
    if (sides.length === 0) {
      sideEl.innerHTML = '<span class="empty">No side data yet</span>';
    } else {
      sideEl.innerHTML = `<table class="data"><thead><tr>
        <th>Side</th><th>W / L</th><th>Win Rate</th>
      </tr></thead><tbody>` +
        sides.map((s) => {
          const wr = s.win_rate != null ? pctText(s.win_rate) : "—";
          const weak = s.win_rate != null && Number(s.win_rate) < 0.55;
          return `<tr>
            <td>${s.side === "long" ? "↑ Long" : "↓ Short"}</td>
            <td class="mono">${s.win ?? 0} / ${s.loss ?? 0}</td>
            <td class="${weak ? "negative" : "positive"}">${wr}</td>
          </tr>`;
        }).join("") +
        "</tbody></table>";
    }
  }

  const so = data.signal_outcomes || {};
  const sh = data.shadow_stats || {};
  const mlGate = sh.ml_gate_shadow || {};
  const adv = $("#training-advanced-stats");
  if (adv) {
    adv.innerHTML = `
      <table class="data"><tbody>
        <tr><td>All signals (incl. shadow)</td><td class="mono">${so.win ?? 0}W / ${so.loss ?? 0}L / ${so.expired ?? 0} expired · ${so.pending ?? 0} pending</td></tr>
        <tr><td>Training-label win rate</td><td class="mono hint-cell">${pctText(om.win_rate)} <span class="hint">(misleading — includes shadow pool)</span></td></tr>
        <tr><td>Shadow pending</td><td class="mono">${sh.pending ?? 0}</td></tr>
        <tr><td>ML-gate reject precision</td><td class="mono">${mlGate.reject_precision != null ? pctText(mlGate.reject_precision) : "—"}</td></tr>
        <tr><td>ONNX path</td><td class="mono" style="word-break:break-all">${onnx.path || "—"}</td></tr>
      </tbody></table>`;
  }
}

function renderTrainingCharts(history, rolling7d) {
  if (typeof Chart === "undefined") {
    setChartPlaceholder("#training-quality-chart", "Chart library unavailable");
    setChartPlaceholder("#training-rolling-chart", "Chart library unavailable");
    return;
  }
  const labels = history.map((_, i) => i + 1);
  const accuracy = history.map((h) =>
    h.recent_accuracy != null ? Number(h.recent_accuracy) * 100 : null,
  );
  const gateAccuracy = history.map((h) =>
    h.gate_accuracy != null ? Number(h.gate_accuracy) * 100 : null,
  );

  const baseOpts = (yLabel, yMax) => ({
    responsive: true,
    maintainAspectRatio: false,
    plugins: { legend: { labels: { color: CHART_DEFAULTS.color } } },
    scales: {
      x: { grid: { display: false }, ticks: { color: CHART_DEFAULTS.color } },
      y: {
        grid: { color: CHART_DEFAULTS.gridColor },
        ticks: { color: CHART_DEFAULTS.color },
        ...(yMax != null ? { min: 0, max: yMax } : {}),
        title: { display: !!yLabel, text: yLabel, color: CHART_DEFAULTS.color },
      },
    },
    interaction: { intersect: false, mode: "index" },
  });

  const qualityCanvas = $("#training-quality-chart");
  if (qualityCanvas) {
    clearChartPlaceholder(qualityCanvas);
    if (!history.length) {
      setChartPlaceholder("#training-quality-chart", "No learning history yet — resolves after trades close");
    } else {
      if (trainingQualityChart) trainingQualityChart.destroy();
      trainingQualityChart = new Chart(qualityCanvas, {
        type: "line",
        data: {
          labels,
          datasets: [
            {
              label: "Gate accuracy %",
              data: gateAccuracy,
              borderColor: "#f59e0b",
              backgroundColor: "rgba(245,158,11,0.08)",
              fill: false,
              tension: 0.35,
              spanGaps: true,
              pointRadius: 2,
              borderWidth: 2,
            },
            {
              label: "Recent accuracy %",
              data: accuracy,
              borderColor: "#60a5fa",
              backgroundColor: "rgba(96,165,250,0.10)",
              fill: false,
              tension: 0.35,
              spanGaps: true,
              pointRadius: 2,
              borderWidth: 2,
            },
          ],
        },
        options: baseOpts("%", 100),
      });
    }
  }

  const rollingCanvas = $("#training-rolling-chart");
  if (rollingCanvas) {
    clearChartPlaceholder(rollingCanvas);
    if (!rolling7d?.length) {
      setChartPlaceholder("#training-rolling-chart", "No resolved outcomes in the last 7 days");
    } else {
      const rLabels = rolling7d.map((d) => d.day);
      const rWin = rolling7d.map((d) => Number(d.win || 0));
      const rLoss = rolling7d.map((d) => Number(d.loss || 0));
      const rWr = rolling7d.map((d) => (d.win_rate != null ? Number(d.win_rate) * 100 : null));
      if (trainingRollingChart) trainingRollingChart.destroy();
      trainingRollingChart = new Chart(rollingCanvas, {
        type: "bar",
        data: {
          labels: rLabels,
          datasets: [
            {
              label: "Wins",
              data: rWin,
              backgroundColor: "rgba(52,211,153,0.6)",
              yAxisID: "y",
            },
            {
              label: "Losses",
              data: rLoss,
              backgroundColor: "rgba(248,113,113,0.6)",
              yAxisID: "y",
            },
            {
              label: "Win rate %",
              data: rWr,
              type: "line",
              borderColor: "#f59e0b",
              backgroundColor: "rgba(245,158,11,0.1)",
              pointRadius: 3,
              borderWidth: 2,
              tension: 0.3,
              spanGaps: true,
              yAxisID: "y2",
            },
          ],
        },
        options: {
          responsive: true,
          maintainAspectRatio: false,
          plugins: { legend: { labels: { color: CHART_DEFAULTS.color } } },
          scales: {
            x: { grid: { display: false }, ticks: { color: CHART_DEFAULTS.color } },
            y: { grid: { color: CHART_DEFAULTS.gridColor }, ticks: { color: CHART_DEFAULTS.color }, stacked: false },
            y2: { position: "right", min: 0, max: 100, grid: { display: false }, ticks: { color: "#f59e0b", callback: (v) => v + "%" } },
          },
        },
      });
    }
  }
}

function setChartPlaceholder(canvasSelector, message) {
  const canvas = $(canvasSelector);
  if (!canvas) return;
  const wrap = canvas.parentElement;
  if (!wrap) return;
  canvas.style.display = "none";
  let note = wrap.querySelector(".training-chart-empty");
  if (!note) {
    note = document.createElement("div");
    note.className = "training-chart-empty";
    wrap.appendChild(note);
  }
  note.textContent = message;
}

function clearChartPlaceholder(canvas) {
  if (!canvas) return;
  canvas.style.display = "";
  canvas.parentElement?.querySelector(".training-chart-empty")?.remove();
}

function renderBacktestResult(res) {
  const card = $("#training-backtest-card");
  const resultEl = $("#training-backtest-result");
  const badge = $("#training-backtest-badge");
  if (!card || !resultEl) return;
  card.style.display = "";
  if (res.error) {
    resultEl.innerHTML = `<span class="negative">${res.error}</span>`;
    return;
  }
  if (badge) {
    badge.textContent = `${res.traded ?? 0} trades`;
    badge.className = "tag tag-ok";
  }
  const r4 = (v) => v != null ? (Number(v) * 100).toFixed(2) + "%" : "—";
  resultEl.innerHTML = `<table class="data"><tbody>
    <tr><td>Total signals</td><td>${res.total_signals ?? "—"}</td></tr>
    <tr><td>Filtered by ML gate</td><td>${res.filtered_by_ml ?? "—"}</td></tr>
    <tr><td>Traded</td><td>${res.traded ?? "—"}</td></tr>
    <tr><td>Wins / Losses / Expired</td><td>${res.wins ?? 0} / ${res.losses ?? 0} / ${res.expired ?? 0}</td></tr>
    <tr><td>Win rate</td><td>${r4(res.win_rate)}</td></tr>
    <tr><td>Total return</td><td class="${Number(res.total_return_pct) >= 0 ? "positive" : "negative"}">${r4(res.total_return_pct)}</td></tr>
    <tr><td>Max drawdown</td><td class="negative">${r4(res.max_drawdown_pct)}</td></tr>
    <tr><td>Expectancy / trade</td><td>${r4(res.expectancy_per_trade)}</td></tr>
    <tr><td>ML threshold</td><td class="mono">${res.settings?.ml_threshold?.toFixed(1) ?? "—"}%</td></tr>
  </tbody></table>`;

  // Equity curve chart
  const curve = res.equity_curve || [];
  if (curve.length > 1 && typeof Chart !== "undefined") {
    if (trainingBacktestChart) trainingBacktestChart.destroy();
    const btCanvas = $("#training-backtest-chart");
    if (btCanvas) {
      trainingBacktestChart = new Chart(btCanvas, {
        type: "line",
        data: {
          labels: curve.map((_, i) => i),
          datasets: [{
            label: "Equity",
            data: curve,
            borderColor: Number(res.total_return_pct) >= 0 ? "#34d399" : "#f87171",
            backgroundColor: Number(res.total_return_pct) >= 0 ? "rgba(52,211,153,0.1)" : "rgba(248,113,113,0.1)",
            fill: true,
            tension: 0.2,
            pointRadius: 0,
            borderWidth: 2,
          }],
        },
        options: {
          responsive: true,
          maintainAspectRatio: false,
          plugins: { legend: { display: false } },
          scales: {
            x: { display: false },
            y: { grid: { color: CHART_DEFAULTS.gridColor }, ticks: { color: CHART_DEFAULTS.color } },
          },
        },
      });
    }
  }
}

function renderWalkForwardResult(res) {
  const card = $("#training-wf-card");
  const resultEl = $("#training-wf-result");
  if (!card || !resultEl) return;
  card.style.display = "";
  if (res.error) {
    resultEl.innerHTML = `<span class="negative">${res.error}</span>`;
    return;
  }
  const pct = (v) => v != null ? (Number(v) * 100).toFixed(1) + "%" : "—";
  const oos = res.out_of_sample || {};
  resultEl.innerHTML = `<table class="data"><tbody>
    <tr><td>Train samples</td><td>${res.train_samples ?? "—"}</td></tr>
    <tr><td>Test samples (OOS)</td><td>${res.test_samples ?? "—"}</td></tr>
    <tr><td>In-sample win rate</td><td>${pct(res.in_sample_win_rate)}</td></tr>
    <tr><td colspan="2" style="font-weight:600;padding-top:.5rem">Out-of-Sample (unseen data)</td></tr>
    <tr><td>OOS accuracy</td><td class="${Number(oos.accuracy) > 0.5 ? "positive" : "negative"}">${pct(oos.accuracy)}</td></tr>
    <tr><td>OOS win rate</td><td>${pct(oos.win_rate)}</td></tr>
    <tr><td>OOS precision</td><td>${pct(oos.precision)}</td></tr>
    <tr><td>OOS trades evaluated</td><td>${oos.total ?? "—"}</td></tr>
  </tbody></table>`;
}

function pnlModeParam() {
  const checked = document.querySelector('input[name="pnl-mode"]:checked');
  const v = checked?.value;
  if (v === "paper") return "?paper=paper";
  if (v === "live") return "?paper=live";
  return "";
}

async function loadPnlTab() {
  try {
    const data = await apiGet(`/pnl/daily${pnlModeParam()}`);
    if (data.error) throw new Error(data.error);
    const summary = data.summary || {};
    const days = data.days || [];

    $("#pnl-total").textContent = fmtUsd(summary.total_pnl);
    $("#pnl-days").textContent = String(summary.days_with_trades || days.length);
    $("#pnl-trades").textContent = String(summary.total_trades || 0);
    $("#pnl-avg").textContent = fmtUsd(summary.avg_daily_pnl);

    const started = summary.trading_since || "";
    const first = summary.first_pnl_day || "";
    const last = summary.last_pnl_day || "";
    $("#pnl-caption").textContent = started
      ? `Since ${fmtManilaDate(started)} · P&L days ${first} → ${last}`
      : days.length
        ? `P&L days ${first} → ${last}`
        : "No closed trades yet.";

    if (!days.length) {
      $("#pnl-table").innerHTML = '<span class="empty">No closed trades yet.</span>';
      return;
    }

    if (pnlDailyChart) pnlDailyChart.destroy();
    if (pnlCumChart) pnlCumChart.destroy();

    const labels = days.map((d) => d.day);
    const pnls = days.map((d) => Number(d.pnl));
    const cum = days.map((d) => Number(d.cumulative_pnl));
    const colors = pnls.map((v) => (v >= 0 ? "#22c55e" : "#f87171"));

    pnlDailyChart = new Chart($("#pnl-daily-chart"), {
      type: "bar",
      data: { labels, datasets: [{ label: "Daily P&L", data: pnls, backgroundColor: colors, borderRadius: 4 }] },
      options: {
        responsive: true,
        maintainAspectRatio: true,
        plugins: {
          legend: { display: false },
          title: { display: true, text: "Daily Realized P&L (USDT)", color: "#f1f5f9", font: { size: 13, weight: 600 } },
        },
        scales: {
          x: { ticks: { color: CHART_DEFAULTS.color, maxRotation: 45 }, grid: { color: CHART_DEFAULTS.gridColor } },
          y: { ticks: { color: CHART_DEFAULTS.color }, grid: { color: CHART_DEFAULTS.gridColor } },
        },
      },
    });

    pnlCumChart = new Chart($("#pnl-cum-chart"), {
      type: "line",
      data: {
        labels,
        datasets: [{
          label: "Cumulative",
          data: cum,
          borderColor: "#22d3ee",
          backgroundColor: "rgba(34, 211, 238, 0.08)",
          fill: true,
          tension: 0.35,
          pointRadius: 2,
          pointHoverRadius: 5,
        }],
      },
      options: {
        responsive: true,
        maintainAspectRatio: true,
        plugins: {
          legend: { display: false },
          title: { display: true, text: "Cumulative P&L (USDT)", color: "#f1f5f9", font: { size: 13, weight: 600 } },
        },
        scales: {
          x: { ticks: { color: CHART_DEFAULTS.color }, grid: { color: CHART_DEFAULTS.gridColor } },
          y: { ticks: { color: CHART_DEFAULTS.color }, grid: { color: CHART_DEFAULTS.gridColor } },
        },
      },
    });

    $("#pnl-table").innerHTML = `
      <table class="data"><thead><tr><th>Date</th><th>Trades</th><th>Wins</th><th>Losses</th><th>Win %</th><th>Daily</th><th>Cumulative</th></tr></thead>
      <tbody>${[...days]
        .reverse()
        .map(
          (d) => `<tr>
            <td>${d.day}</td><td>${d.trades}</td><td>${d.wins}</td><td>${d.losses}</td>
            <td>${(Number(d.win_rate) * 100).toFixed(1)}%</td>
            <td>${fmtUsd(d.pnl)}</td><td>${fmtUsd(d.cumulative_pnl)}</td></tr>`
        )
        .join("")}</tbody></table>`;
  } catch (e) {
    $("#pnl-table").innerHTML = `<span class="empty">Error: ${e.message}</span>`;
  }
}

async function loadAccountTab() {
  try {
    const [profile, wallet] = await Promise.all([apiGet("/user/profile"), apiGet("/wallet")]);
    const creds = profile.credentials || {};
    liveTradingEnabled = !!profile.live_trading_enabled;
    $("#w-equity").textContent = fmtUsd(wallet.equity, 4);
    $("#w-available").textContent = fmtUsd(wallet.available, 4);
    $("#w-source").textContent = wallet.source || "—";
    $("#w-paper").textContent = wallet.paper_trading ? "Yes" : "No";
    $("#w-live").textContent = wallet.live_trading ? "Yes" : "No";
    renderCredentialsState(creds, !!creds.has_credentials);
    syncTradingModeUI({
      live_trading: profile.live_trading,
      live_trading_enabled: profile.live_trading_enabled,
      has_api_credentials: creds.has_credentials,
      dry_run: profile.dry_run,
      exchange_orders_enabled: profile.exchange_orders_enabled,
    });
  } catch (e) {
    showFeedback($("#wallet-feedback"), e.message, false);
  }
  await loadTelegramState();
}

async function loadUserProfile() {
  try {
    const profile = await apiGet("/user/profile");
    liveTradingEnabled = !!profile.live_trading_enabled;
    const creds = profile.credentials || {};
    renderCredentialsState(creds, !!creds.has_credentials);
    syncTradingModeUI({
      live_trading: profile.live_trading,
      live_trading_enabled: profile.live_trading_enabled,
      has_api_credentials: creds.has_credentials,
      dry_run: profile.dry_run,
      exchange_orders_enabled: profile.exchange_orders_enabled,
    });
  } catch {
    /* profile optional on startup */
  }
}

function initTabs() {
  $$(".nav-item").forEach((btn) => {
    btn.addEventListener("click", () => {
      $$(".nav-item").forEach((t) => t.classList.remove("active"));
      $$(".panel").forEach((p) => p.classList.remove("active"));
      btn.classList.add("active");
      const id = btn.dataset.tab;
      $(`#panel-${id}`)?.classList.add("active");
      if (id === "trading") loadBalanceHistory();
      if (id === "signals") loadSignalsTab();
      if (id === "positions") loadPositionsTab();
      if (id === "readiness") loadReadinessTab();
      if (id === "training") loadTrainingTab();
      if (id === "pnl") loadPnlTab();
      if (id === "account") loadAccountTab();
      if (id === "settings") loadSettingsTab();
    });
  });
}

async function loadBalanceHistory() {
  const canvas = $("#balance-history-chart");
  if (!canvas || typeof Chart === "undefined") return;
  try {
    const data = await apiGet("/pnl/daily");
    const days = data.days || [];

    // Map calendar day -> cumulative realized PnL as of that day.
    const cumByDay = new Map();
    let finalCum = 0;
    days.forEach((d) => {
      const c = Number(d.cumulative_pnl) || 0;
      cumByDay.set(d.day, c);
      finalCum = c;
    });

    // Anchor the series to the current equity (equity = base + cumulative).
    const current = latestEquity != null ? latestEquity : 10000;
    const baseEquity = current - finalCum;

    // Build the last 7 calendar days, carrying forward the last known cumulative.
    const labels = [];
    const series = [];
    let runningCum = 0;
    // Seed runningCum with cumulative before the 7-day window.
    const today = new Date();
    const windowStart = new Date(today);
    windowStart.setDate(today.getDate() - 6);
    days.forEach((d) => {
      if (new Date(`${d.day}T00:00:00`) < windowStart) {
        runningCum = Number(d.cumulative_pnl) || runningCum;
      }
    });

    for (let i = 6; i >= 0; i -= 1) {
      const dt = new Date(today);
      dt.setDate(today.getDate() - i);
      const key = dt.toISOString().slice(0, 10);
      if (cumByDay.has(key)) runningCum = cumByDay.get(key);
      labels.push(fmtManilaDate(dt));
      series.push(Math.round((baseEquity + runningCum) * 100) / 100);
    }

    if (balanceHistoryChart) balanceHistoryChart.destroy();

    const first = series[0];
    const last = series[series.length - 1];
    const up = last >= first;
    const lineColor = up ? "#34d399" : "#f87171";

    balanceHistoryChart = new Chart(canvas, {
      type: "line",
      data: {
        labels,
        datasets: [{
          label: "Equity",
          data: series,
          borderColor: lineColor,
          backgroundColor: up ? "rgba(52,211,153,0.12)" : "rgba(248,113,113,0.12)",
          fill: true,
          tension: 0.35,
          pointRadius: 3,
          pointBackgroundColor: lineColor,
          borderWidth: 2,
        }],
      },
      options: {
        responsive: true,
        maintainAspectRatio: false,
        plugins: {
          legend: { display: false },
          tooltip: {
            callbacks: {
              label: (ctx) => `Equity: ${fmtUsd(ctx.parsed.y)}`,
            },
          },
        },
        scales: {
          x: { grid: { display: false }, ticks: { color: CHART_DEFAULTS.color } },
          y: {
            grid: { color: "rgba(148,163,184,0.08)" },
            ticks: {
              color: CHART_DEFAULTS.color,
              callback: (v) => fmtUsd(v, 0),
            },
          },
        },
        interaction: { intersect: false, mode: "index" },
      },
    });
  } catch {
    /* balance history optional */
  }
}

function initControls() {
  $("#btn-start")?.addEventListener("click", async () => {
    const fb = $("#control-feedback");
    try {
      const res = await apiPost("/trading/start");
      if (res.error) throw new Error(res.error);
      showFeedback(fb, (res.steps || ["Started"]).join(" · "), true);
      await refreshSnapshotHttp();
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $("#btn-stop")?.addEventListener("click", async () => {
    const fb = $("#control-feedback");
    try {
      await apiPost("/trading/stop");
      showFeedback(fb, "Scanner stopped", true);
      await refreshSnapshotHttp();
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $("#btn-kill")?.addEventListener("click", async () => {
    const ok = await confirmDialog(
      "Activate kill switch? This closes <strong>ALL</strong> open positions and stops the scanner.",
      { confirmLabel: "Activate kill switch", danger: true },
    );
    if (!ok) return;
    const fb = $("#control-feedback");
    try {
      const res = await apiPost("/kill-switch/activate");
      if (res.error) throw new Error(res.error);
      const closed = Number(res.closed || 0);
      const msg = closed > 0
        ? `Kill switch ON — closed ${closed} position${closed === 1 ? "" : "s"}, scanner stopped`
        : "Kill switch ON — scanner stopped";
      showFeedback(fb, msg, true);
      await refreshSnapshotHttp();
      if ($("#panel-positions")?.classList.contains("active")) await loadPositionsTab();
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $("#btn-refresh-signals")?.addEventListener("click", loadSignalsTab);

  $("#btn-signals-prev")?.addEventListener("click", async () => {
    if (signalsPage <= 1) return;
    try {
      await loadSignalsPage(signalsPage - 1);
    } catch (e) {
      $("#signals-table").innerHTML = `<span class="empty">Error: ${e.message}</span>`;
    }
  });
  $("#btn-signals-next")?.addEventListener("click", async () => {
    if (signalsPage >= signalsTotalPages) return;
    try {
      await loadSignalsPage(signalsPage + 1);
    } catch (e) {
      $("#signals-table").innerHTML = `<span class="empty">Error: ${e.message}</span>`;
    }
  });

  $("#btn-refresh-positions")?.addEventListener("click", loadPositionsTab);
  $("#btn-refresh-positions-history")?.addEventListener("click", loadPositionsHistoryTab);

  $$('input[name="positions-view"]').forEach((el) => {
    el.addEventListener("change", () => {
      const view = el.value || "open";
      $$(".positions-view-nav .seg").forEach((s) => s.classList.remove("active"));
      el.closest(".seg")?.classList.add("active");
      setPositionsView(view);
      loadPositionsTab();
    });
  });

  $$(".positions-view-nav .seg").forEach((seg) => {
    const input = seg.querySelector('input[name="positions-view"]');
    if (input?.checked) seg.classList.add("active");
  });

  $$('input[name="positions-history-mode"]').forEach((el) => {
    el.addEventListener("change", () => {
      $$(".positions-history-filter .seg").forEach((s) => s.classList.remove("active"));
      el.closest(".seg")?.classList.add("active");
      if (getActivePositionsView() === "history") loadPositionsHistoryTab();
    });
  });

  $$(".positions-history-filter .seg").forEach((seg) => {
    const input = seg.querySelector('input[name="positions-history-mode"]');
    if (input?.checked) seg.classList.add("active");
  });

  $("#btn-close-signal-chart")?.addEventListener("click", closeSignalChartModal);
  $("[data-close-signal-chart]")?.addEventListener("click", closeSignalChartModal);
  $("#btn-close-position-chart")?.addEventListener("click", closePositionChartModal);
  $("[data-close-position-chart]")?.addEventListener("click", closePositionChartModal);
  document.addEventListener("keydown", (ev) => {
    if (ev.key !== "Escape") return;
    if (!$("#signal-chart-modal")?.classList.contains("hidden")) closeSignalChartModal();
    if (!$("#position-chart-modal")?.classList.contains("hidden")) closePositionChartModal();
  });

  $("#signals-table")?.addEventListener("click", (ev) => {
    const row = ev.target.closest("tr[data-symbol]");
    if (!row) return;
    showSignalChart(row.dataset.symbol, row.dataset.generatedAt);
  });

  $("#positions-table")?.addEventListener("click", (ev) => {
    const closeBtn = ev.target.closest("[data-close-position-id]");
    if (closeBtn) {
      ev.preventDefault();
      const row = closeBtn.closest("tr[data-position-id]");
      const symbol = row?.querySelector("strong")?.textContent || "";
      closePositionManually(closeBtn.dataset.closePositionId, symbol);
      return;
    }
    const row = ev.target.closest("tr[data-position-id]");
    if (!row) return;
    const symbol = row.querySelector("strong")?.textContent || "";
    showPositionChart(row.dataset.positionId, symbol);
  });

  $("#positions-history-table")?.addEventListener("click", (ev) => {
    const row = ev.target.closest("tr[data-position-id]");
    if (!row) return;
    const symbol = row.querySelector("strong")?.textContent || "";
    showPositionChart(row.dataset.positionId, symbol);
  });

  $$('input[name="pnl-mode"]').forEach((el) => {
    el.addEventListener("change", () => {
      $$(".pnl-mode .seg").forEach((s) => s.classList.remove("active"));
      el.closest(".seg")?.classList.add("active");
      loadPnlTab();
    });
  });

  $$(".pnl-mode .seg").forEach((seg) => {
    const input = seg.querySelector('input[name="pnl-mode"]');
    if (input?.checked) seg.classList.add("active");
  });

  initChartTabs("signal-chart-tabs", "signal-tv-chart", "signal-lwc-chart");
  initChartTabs("position-chart-tabs", "position-tv-chart", "position-lwc-chart");

  $("#btn-sync-positions")?.addEventListener("click", async () => {
    const fb = $("#positions-feedback");
    try {
      const res = await apiPost("/positions/sync");
      if (res.error) throw new Error(res.error);
      showFeedback(fb, res.message || `Synced ${res.synced ?? 0}`, true);
      await loadPositionsTab();
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $("#btn-reanchor")?.addEventListener("click", async () => {
    const fb = $("#wallet-feedback");
    try {
      const res = await apiPost("/risk/reanchor-wallet");
      if (res.error) throw new Error(res.error);
      showFeedback(fb, `Re-anchored · ${fmtUsd(res.wallet_balance)}`, true);
      await loadAccountTab();
      await refreshSnapshotHttp();
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $$('input[name="trading-mode"]').forEach((el) => {
    el.addEventListener("change", () => {
      if (el.checked) saveTradingMode(el.value);
    });
  });

  $("#btn-cred-logout")?.addEventListener("click", async () => {
    const ok = await confirmDialog("Disconnect MEXC API credentials from this bot?", {
      confirmLabel: "Disconnect",
      danger: true,
    });
    if (!ok) return;
    const fb = $("#cred-feedback");
    try {
      const res = await apiDelete("/user/credentials");
      if (res.error) throw new Error(res.error);
      credReplaceMode = false;
      $("#cred-key").value = "";
      $("#cred-secret").value = "";
      showFeedback(fb, "Credentials disconnected", true);
      await loadAccountTab();
      await refreshSnapshotHttp();
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $("#btn-cred-replace")?.addEventListener("click", () => {
    credReplaceMode = true;
    $("#cred-key").value = "";
    $("#cred-secret").value = "";
    const creds = {
      mexc_api_key_masked: $("#cred-key-masked")?.textContent,
      mexc_api_secret_masked: $("#cred-secret-masked")?.textContent,
    };
    renderCredentialsState(creds, true);
    $("#cred-key")?.focus();
  });

  $("#btn-settings-reload")?.addEventListener("click", loadSettingsTab);
  $("#btn-settings-save")?.addEventListener("click", saveSettings);

  $("#btn-training-refresh")?.addEventListener("click", loadTrainingTab);
  $("#btn-training-resolve")?.addEventListener("click", async () => {
    const fb = $("#training-feedback");
    showFeedback(fb, "Resolving pending signals against price action…", true);
    try {
      const res = await apiPost("/ml/resolve-signals");
      const n = res.resolved_samples ?? 0;
      showFeedback(fb, `Resolved ${n} signal(s) from price and trained the model.`, true);
      await loadTrainingTab();
    } catch (e) {
      showFeedback(fb, `Resolve failed: ${e.message}`, false);
    }
  });
  $("#btn-training-replay")?.addEventListener("click", async () => {
    const fb = $("#training-feedback");
    showFeedback(fb, "Replaying resolved trade history into the online model…", true);
    try {
      const res = await apiPost("/ml/train");
      const n = res.trained_samples ?? res.online_model?.samples ?? 0;
      showFeedback(fb, `Done — model trained from ${n} resolved sample(s).`, true);
      await loadTrainingTab();
    } catch (e) {
      showFeedback(fb, `Replay failed: ${e.message}`, false);
    }
  });

  $("#btn-training-backtest")?.addEventListener("click", async () => {
    const fb = $("#training-feedback");
    showFeedback(fb, "Running backtest on resolved signals…", true);
    try {
      const res = await apiPost("/backtest", {});
      renderBacktestResult(res);
      showFeedback(fb, `Backtest done — ${res.traded ?? 0} trades simulated.`, true);
    } catch (e) {
      showFeedback(fb, `Backtest failed: ${e.message}`, false);
    }
  });

  $("#btn-training-walkforward")?.addEventListener("click", async () => {
    const fb = $("#training-feedback");
    showFeedback(fb, "Running walk-forward validation…", true);
    try {
      const res = await apiPost("/walk-forward", {});
      renderWalkForwardResult(res);
      showFeedback(fb, "Walk-forward complete.", true);
    } catch (e) {
      showFeedback(fb, `Walk-forward failed: ${e.message}`, false);
    }
  });

  $("#btn-notifications")?.addEventListener("click", (ev) => {
    ev.stopPropagation();
    setNotificationsOpen($("#notification-panel")?.classList.contains("hidden"));
  });
  $("#btn-notif-close")?.addEventListener("click", () => setNotificationsOpen(false));
  document.addEventListener("click", (ev) => {
    if (!notificationsOpen) return;
    if (ev.target.closest("#notif-wrap")) return;
    setNotificationsOpen(false);
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && notificationsOpen) setNotificationsOpen(false);
  });

  $("#btn-notif-show-all")?.addEventListener("click", () => setNotificationsModalOpen(true));
  $("#btn-notif-modal-close")?.addEventListener("click", () => setNotificationsModalOpen(false));
  $("[data-close-notifications]")?.addEventListener("click", () => setNotificationsModalOpen(false));
  $("#btn-notif-modal-prev")?.addEventListener("click", () => {
    if (notifModalPage <= 1) return;
    notifModalPage -= 1;
    renderNotificationsModal();
  });
  $("#btn-notif-modal-next")?.addEventListener("click", () => {
    notifModalPage += 1;
    renderNotificationsModal();
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && notifModalOpen) setNotificationsModalOpen(false);
  });

  $("#credentials-form")?.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const fb = $("#cred-feedback");
    const key = $("#cred-key").value.trim();
    const secret = $("#cred-secret").value.trim();
    if (!key || !secret) {
      showFeedback(fb, "API key and secret are both required", false);
      return;
    }
    try {
      const res = await apiPut("/user/credentials", {
        mexc_api_key: key,
        mexc_api_secret: secret,
      });
      if (res.error) throw new Error(res.error);
      credReplaceMode = false;
      showFeedback(fb, "Credentials saved", true);
      $("#cred-key").value = "";
      $("#cred-secret").value = "";
      await loadAccountTab();
      await refreshSnapshotHttp();
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });
}

// ---------------------------------------------------------------------------
// Telegram notifications
// ---------------------------------------------------------------------------

const TG_EVENTS = [
  { key: "position_opened", label: "Position Opened" },
  { key: "position_closed", label: "Position Closed" },
  { key: "tp_hit",          label: "Take Profit Hit" },
  { key: "cut_loss",        label: "Stop Loss Triggered" },
  { key: "kill_switch",     label: "Kill Switch" },
];

function renderTelegramState(tg) {
  const connected = !!tg?.connected;
  const badge = $("#tg-status-badge");
  const connDiv = $("#tg-connected");
  const form = $("#telegram-form");
  if (badge) {
    badge.textContent = connected ? "Connected" : "Disconnected";
    badge.className = connected ? "tag tag-green" : "tag";
  }
  if (connDiv) connDiv.classList.toggle("hidden", !connected);
  if (form) form.classList.toggle("hidden", connected);
  if (!connected) {
    $("#tg-start-note")?.classList.add("hidden");
    return;
  }

  const chatMasked = $("#tg-chat-masked");
  if (chatMasked) chatMasked.textContent = tg.chat_id_masked || "—";
  const enabledLabel = $("#tg-enabled-label");
  if (enabledLabel) enabledLabel.textContent = tg.enabled ? "Yes" : "No";
  const toggle = $("#tg-toggle-enabled");
  if (toggle) toggle.checked = !!tg.enabled;

  const container = $("#tg-event-toggles");
  if (container) {
    container.innerHTML = "";
    const activeEvents = Array.isArray(tg.events) ? tg.events : TG_EVENTS.map((e) => e.key);
    TG_EVENTS.forEach(({ key, label }) => {
      const id = `tg-evt-${key}`;
      const wrap = document.createElement("label");
      wrap.className = "tg-event-toggle";
      wrap.innerHTML = `<input type="checkbox" id="${id}" data-event="${key}" ${activeEvents.includes(key) ? "checked" : ""} /><span>${label}</span>`;
      container.appendChild(wrap);
    });
  }

  const startNote = $("#tg-start-note");
  const startNoteText = $("#tg-start-note-text");
  const botUsername = tg.bot_username || "";
  const botName = tg.bot_name || botUsername || "your bot";
  const botLink = tg.bot_link || (botUsername ? `https://t.me/${botUsername}` : "");
  if (startNote && startNoteText) {
    if (botUsername) {
      const handle = `@${botUsername}`;
      const label = botName && botName !== botUsername ? `${botName} (${handle})` : handle;
      startNoteText.innerHTML =
        `Open Telegram and chat with <a href="${botLink}" target="_blank" rel="noopener">${label}</a>, ` +
        "then press <strong>Start</strong>. Bots cannot message you until you've started the chat. " +
        "Send <strong>/info</strong> anytime for live wallet &amp; position stats.";
      startNote.classList.remove("hidden");
    } else {
      startNoteText.textContent =
        "Open Telegram, find your bot, and press Start so it can send you messages.";
      startNote.classList.remove("hidden");
    }
  }
}

async function loadTelegramState() {
  try {
    const tg = await apiGet("/user/telegram");
    renderTelegramState(tg);
  } catch { /* non-critical */ }
}

function initTelegramControls() {
  const fb = $("#tg-feedback");

  $("#telegram-form")?.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const token = $("#tg-token")?.value.trim();
    const chatId = $("#tg-chat-id")?.value.trim();
    if (!token || !chatId) {
      showFeedback(fb, "Bot token and chat ID are required", false);
      return;
    }
    try {
      const res = await apiPut("/user/telegram", {
        telegram_bot_token: token,
        telegram_chat_id: chatId,
        telegram_enabled: true,
        telegram_events: TG_EVENTS.map((e) => e.key),
      });
      if (res.error) throw new Error(res.error);
      $("#tg-token").value = "";
      $("#tg-chat-id").value = "";
      showFeedback(fb, "Telegram connected", true);
      renderTelegramState(res.telegram);
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $("#btn-tg-save-events")?.addEventListener("click", async () => {
    const events = [...$$('#tg-event-toggles input[type="checkbox"]')]
      .filter((el) => el.checked)
      .map((el) => el.dataset.event);
    const enabled = $("#tg-toggle-enabled")?.checked ?? true;
    try {
      const res = await apiPut("/user/telegram", { telegram_events: events, telegram_enabled: enabled });
      if (res.error) throw new Error(res.error);
      showFeedback(fb, "Telegram settings saved", true);
      renderTelegramState(res.telegram);
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $("#btn-tg-test")?.addEventListener("click", async () => {
    showFeedback(fb, "Sending test message…", true);
    try {
      const res = await apiPost("/user/telegram/test");
      if (!res.ok) throw new Error(res.error || "Test failed");
      showFeedback(fb, res.message || "Test message sent!", true);
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $("#btn-tg-disconnect")?.addEventListener("click", async () => {
    const ok = await confirmDialog("Disconnect Telegram from this bot?", { confirmLabel: "Disconnect", danger: true });
    if (!ok) return;
    try {
      const res = await apiPut("/user/telegram", { clear_telegram: true });
      if (res.error) throw new Error(res.error);
      showFeedback(fb, "Telegram disconnected", true);
      renderTelegramState(res.telegram);
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });

  $("#tg-toggle-enabled")?.addEventListener("change", async (ev) => {
    try {
      const res = await apiPut("/user/telegram", { telegram_enabled: ev.target.checked });
      if (res.error) throw new Error(res.error);
      const label = $("#tg-enabled-label");
      if (label) label.textContent = ev.target.checked ? "Yes" : "No";
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });
}

async function init() {
  initTabs();
  initControls();
  initTelegramControls();
  await loadUserProfile();
  await refreshSnapshotHttp();
  await loadBalanceHistory();
  connectWebSocket();
  startPolling();
}

init();
