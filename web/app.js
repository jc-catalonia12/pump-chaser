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
let marketPollTimer = null;
let dashboardNewsItems = [];
let dashboardNewsPage = 0;
let dashboardNewsAutoTimer = null;
const DASHBOARD_NEWS_PER_PAGE = 1;
const DASHBOARD_NEWS_AUTO_MS = 7000;
const NEWS_TABLE_PER_PAGE = 8;
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

function fmtStrategyLabel(strategy) {
  const s = (strategy || "").toLowerCase();
  if (s === "ai") return "AI";
  if (!s) return "—";
  // Legacy strategy ids from old trade rows fall through to title case.
  return s.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
}

function fmtEntryModeBadge(entryMode, orderStatus) {
  const mode = (entryMode || "market").toLowerCase();
  const status = (orderStatus || "open").toLowerCase();
  let label = "Market";
  let cls = "badge-market";
  if (mode === "limit") {
    label = "Limit";
    cls = "badge-limit";
  } else if (mode === "sniper") {
    label = "Sniper";
    cls = "badge-sniper";
  }
  if (status === "pending" && (mode === "limit" || mode === "sniper")) {
    return `<span class="badge ${cls}" title="Resting limit order">${label}</span> <span class="tag tag-warn">pending</span>`;
  }
  return `<span class="badge ${cls}">${label}</span>`;
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

const TOAST_TRADE_TYPES = new Set([
  "position_opened",
  "position_closed",
  "position_partial_tp",
  "tp_hit",
  "cut_loss",
  "kill_switch",
  "volume_pump_detected",
  "volume_pump_signal",
  "signal",
]);
const DEFAULT_TOAST_EVENT_KEYS = [
  "position_opened",
  "position_closed",
  "tp_hit",
  "cut_loss",
  "kill_switch",
];
const TOAST_MAX_VISIBLE = 5;
const TOAST_DURATION_MS = 6000;
let lastToastEventId = 0;
let toastBaselineSet = false;
let toastNotificationsEnabled = true;
let toastEventKeys = new Set(DEFAULT_TOAST_EVENT_KEYS);

function escapeHtml(text) {
  return String(text)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function symbolFromTradeMessage(message) {
  const match = String(message || "").match(/\b([A-Z0-9]+_USDT)\b/);
  return match ? match[1] : "";
}

function formatToastPnl(pnl) {
  if (pnl == null || Number.isNaN(Number(pnl))) return null;
  const n = Number(pnl);
  return `${n >= 0 ? "+" : ""}${n.toFixed(2)} USDT`;
}

function toastFromEvent(event) {
  const type = (event.event_type || event.type || "").toLowerCase();
  const payload =
    event.payload && typeof event.payload === "object" ? event.payload : {};
  const reason = String(payload.reason || "").toLowerCase();
  const symbol = payload.symbol || symbolFromTradeMessage(event.message);
  const pnlStr = formatToastPnl(payload.pnl);
  const symbolLine = [symbol, pnlStr].filter(Boolean).join(" · ");

  if (type === "kill_switch") {
    return {
      title: "Kill Switch",
      message: event.message || "Trading halted",
      tone: "danger",
      iconType: "kill_switch",
    };
  }

  if (type === "volume_pump_detected" || type === "volume_pump_signal") {
    const score = payload.composite_score != null ? ` score ${Number(payload.composite_score).toFixed(1)}` : "";
    const sym = symbol || symbolFromTradeMessage(event.message);
    return {
      title: "Volume Pump",
      message: [sym, score].filter(Boolean).join(" · ") || event.message || "Volume anomaly",
      tone: "accent",
      iconType: type,
    };
  }

  if (type === "signal") {
    const sym = symbol || symbolFromTradeMessage(event.message);
    return {
      title: "Signal",
      message: sym || event.message || "New setup",
      tone: "accent",
      iconType: "signal",
    };
  }

  if (type === "position_opened") {
    let side = String(payload.side || "").toUpperCase();
    if (!side) {
      const sideMatch = String(event.message || "").match(/Opened\s+(long|short)\s+/i);
      if (sideMatch) side = sideMatch[1].toUpperCase();
    }
    const sym = symbol || symbolFromTradeMessage(event.message);
    return {
      title: "Position Opened",
      message: [side, sym].filter(Boolean).join(" ") || event.message || "New trade entered",
      tone: "success",
      iconType: "position_opened",
    };
  }

  if (type === "position_partial_tp" || type === "tp_hit") {
    const level = payload.level ? `TP${payload.level}` : "Take Profit";
    return {
      title: `Partial ${level}`,
      message: symbolLine || event.message,
      tone: "success",
      iconType: "tp_hit",
    };
  }

  if (
    type === "cut_loss" ||
    (type === "position_closed" &&
      (reason.includes("stop") || reason === "trailing_stop"))
  ) {
    const title = reason === "trailing_stop" ? "Trailing Stop" : "Stop Loss";
    return {
      title,
      message: symbolLine || event.message,
      tone: "danger",
      iconType: "cut_loss",
    };
  }

  if (type === "position_closed") {
    if (reason.includes("take_profit") || reason === "take_profit") {
      return {
        title: "Take Profit",
        message: symbolLine || event.message,
        tone: "success",
        iconType: "tp_hit",
      };
    }
    const pnl = payload.pnl;
    return {
      title: "Position Closed",
      message: symbolLine || event.message,
      tone: pnl != null && Number(pnl) < 0 ? "warn" : "neutral",
      iconType: "position_closed",
    };
  }

  return null;
}

/** Map audit_log rows to the same keys used in Settings → Notify on events. */
function toastPreferenceKey(event) {
  const type = (event.event_type || event.type || "").toLowerCase();
  const payload =
    event.payload && typeof event.payload === "object" ? event.payload : {};
  const reason = String(payload.reason || "").toLowerCase();
  const msg = String(event.message || "").toLowerCase();

  if (type === "position_partial_tp" || type === "tp_hit") return "tp_hit";
  if (type === "cut_loss") return "cut_loss";
  if (type === "position_closed") {
    if (
      reason.includes("stop") ||
      reason === "trailing_stop" ||
      reason.includes("cut") ||
      msg.includes("stop_loss") ||
      msg.includes("trailing_stop") ||
      (msg.includes("stop") && !msg.includes("non-stop"))
    ) {
      return "cut_loss";
    }
    if (reason.includes("take_profit") || reason.includes("tp") || msg.includes(" tp")) {
      return "tp_hit";
    }
    return "position_closed";
  }
  if (type === "kill_switch") return "kill_switch";
  if (type === "position_opened") return "position_opened";
  return type;
}

function applyNotificationPreferences(tg) {
  if (!tg?.connected) {
    toastNotificationsEnabled = true;
    toastEventKeys = new Set(DEFAULT_TOAST_EVENT_KEYS);
    return;
  }
  toastNotificationsEnabled = tg.enabled !== false;
  const events = Array.isArray(tg.events) ? tg.events : DEFAULT_TOAST_EVENT_KEYS;
  toastEventKeys = new Set(events.length ? events : DEFAULT_TOAST_EVENT_KEYS);
}

function isToastEventEnabled(event) {
  if (!toastNotificationsEnabled) return false;
  const type = (event.event_type || event.type || "").toLowerCase();
  if (!TOAST_TRADE_TYPES.has(type)) return false;
  const pref = toastPreferenceKey(event);
  if (!DEFAULT_TOAST_EVENT_KEYS.includes(pref)) {
    // Strategy signals — always toast when enabled globally.
    return true;
  }
  return toastEventKeys.has(pref);
}

function dismissToast(el) {
  if (!el || el.classList.contains("toast-out")) return;
  el.classList.add("toast-out");
  el.addEventListener("animationend", () => el.remove(), { once: true });
}

function showTradeToast({ title, message, tone = "info", iconType = "default", duration = TOAST_DURATION_MS }) {
  const container = $("#toast-container");
  if (!container) return;

  while (container.children.length >= TOAST_MAX_VISIBLE) {
    dismissToast(container.firstElementChild);
  }

  const toast = document.createElement("div");
  toast.className = `toast toast-${tone}`;
  toast.innerHTML = `
    <div class="toast-icon">${eventIconHtml(iconType)}</div>
    <div class="toast-body">
      <strong class="toast-title">${escapeHtml(title)}</strong>
      ${message ? `<p class="toast-msg">${escapeHtml(message)}</p>` : ""}
    </div>
    <button type="button" class="toast-close" aria-label="Dismiss">×</button>
  `;

  const closeBtn = toast.querySelector(".toast-close");
  closeBtn?.addEventListener("click", () => dismissToast(toast));

  container.appendChild(toast);
  if (duration > 0) {
    setTimeout(() => dismissToast(toast), duration);
  }
}

function maybeToastNewEvents(events) {
  const list = events || [];
  const maxId = list.reduce((max, e) => Math.max(max, Number(e.id) || 0), 0);

  if (!toastBaselineSet) {
    // First batch with data becomes the baseline — skip historical rows, toast only newer ids.
    if (maxId > 0) {
      lastToastEventId = maxId;
      toastBaselineSet = true;
    }
    return;
  }

  if (!list.length) return;

  const fresh = list
    .filter((e) => {
      const id = Number(e.id) || 0;
      return id > lastToastEventId && isToastEventEnabled(e);
    })
    .sort((a, b) => (Number(a.id) || 0) - (Number(b.id) || 0));

  // If the window was hidden (minimised) we skip toasts for accumulated events
  // to avoid a flood of notifications on restore. The badge still updates.
  if (!document.hidden) {
    fresh.forEach((e) => {
      const spec = toastFromEvent(e);
      if (spec) showTradeToast(spec);
    });
  }

  if (maxId > lastToastEventId) lastToastEventId = maxId;
}

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

function settingsFieldType(field) {
  return field.type || field.field_type || "number";
}

function isSettingsTextField(field) {
  const t = settingsFieldType(field);
  if (t === "text") return true;
  const key = field.key || "";
  return key.includes("url") || key.includes("_base_url");
}

function buildSettingsPatch() {
  const patch = {};
  $$("[data-setting-key]").forEach((el) => {
    const key = el.dataset.settingKey;
    const type = el.dataset.settingType;
    let value;
    if (type === "bool") {
      value = el.checked;
    } else if (type === "select" || type === "text" || type === "url") {
      value = el.value.trim();
      if ((type === "text" || type === "url") && !value) return;
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
  const fieldType = settingsFieldType(field);

  if (fieldType === "bool") {
    const checked = value ? "checked" : "";
    return `<div class="settings-field">
      <label class="settings-toggle" for="${id}">
        <input type="checkbox" id="${id}" data-setting-key="${field.key}" data-setting-type="bool" ${checked} />
        <span>${field.label}</span>
      </label>
      ${hint}
    </div>`;
  }

  if (fieldType === "select") {
    const options = (field.options || [])
      .map((opt) => `<option value="${opt}"${opt === value ? " selected" : ""}>${opt}</option>`)
      .join("");
    return `<div class="settings-field">
      <label for="${id}">${field.label}</label>
      <select id="${id}" data-setting-key="${field.key}" data-setting-type="select">${options}</select>
      ${hint}
    </div>`;
  }

  if (isSettingsTextField(field)) {
    const display = value != null ? String(value) : "";
    const inputKind = (field.key || "").includes("url") ? "url" : "text";
    return `<div class="settings-field">
      <label for="${id}">${field.label}</label>
      <input type="text" id="${id}" data-setting-key="${field.key}" data-setting-type="${inputKind}"
        class="settings-text-input" value="${display.replace(/"/g, "&quot;")}"
        spellcheck="false" autocomplete="off" inputmode="url" />
      ${hint}
    </div>`;
  }

  const step = field.step ?? (fieldType === "integer" ? 1 : 0.001);
  const min = field.min != null ? ` min="${field.min}"` : "";
  const max = field.max != null ? ` max="${field.max}"` : "";
  const display = value != null && !Number.isNaN(Number(value)) ? Number(value) : "";
  const inputType = fieldType === "integer" ? "integer" : "number";

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
    let msg = res.scanner_restart_recommended
      ? "Saved. Restart the scanner if you changed MEXC API URLs."
      : "Settings saved and applied live.";
    if (res.paper_equity_applied) {
      msg = "Settings saved. Paper equity updated to your new starting value.";
    } else if (res.paper_equity_blocked_open_positions) {
      msg =
        "Settings saved. Close open paper positions before changing starting equity.";
    }
    showFeedback(fb, msg, true);
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

  const baseEquity = wallet.equity ?? risk.equity;
  if (baseEquity != null && !Number.isNaN(Number(baseEquity))) {
    latestEquity = Number(baseEquity);
  }

  // Sum unrealized P&L from all open positions to show live equity & daily P&L.
  const openPositions = snapshot.positions || [];
  let totalUnrealized = 0;
  for (const p of openPositions) {
    totalUnrealized += Number(p.unrealized_pnl || 0);
  }
  const equity = baseEquity != null ? Number(baseEquity) + totalUnrealized : baseEquity;

  const realizedDaily = Number(risk.daily_pnl ?? 0);
  const daily = realizedDaily + totalUnrealized;
  const equityBase = Number(baseEquity ?? risk.equity ?? 0) || 1;
  const dailyPct = (daily / equityBase) * 100;

  const liveMode = !!health.live_trading;
  const dryRun = !!health.dry_run;
  const ordersLive = !!health.exchange_orders_enabled;

  const hdrEquity = $("#hdr-equity");
  hdrEquity.textContent = fmtUsd(equity);
  if (totalUnrealized !== 0) {
    hdrEquity.title = `Base: ${fmtUsd(baseEquity)}  |  Unrealized: ${totalUnrealized > 0 ? "+" : ""}${fmtUsd(totalUnrealized)}`;
  } else {
    hdrEquity.title = "";
  }

  const hdrDaily = $("#hdr-daily-pnl");
  hdrDaily.textContent = `${fmtUsd(daily)} (${fmtPct(dailyPct)})`;
  hdrDaily.className = "hmetric-value mono" + pnlClass(daily);

  const maxPos = risk.max_positions ?? 5;
  const posText = `${risk.open_positions ?? 0} / ${maxPos}`;
  $("#m-positions").textContent = posText;
  $("#hdr-positions").textContent = posText;
  const slotsEl = $("#m-positions-slots");
  if (slotsEl) {
    slotsEl.textContent = "";
  }

  // Aggregate unrealized PnL across the open positions (openPositions already
  // computed above for header equity), plus a blended ROI% for the widget.
  const pnlEl = $("#m-positions-pnl");
  if (pnlEl) {
    if (openPositions.length) {
      let totalMargin = 0;
      for (const p of openPositions) {
        const csize = Number(p.contract_size || 1) || 1;
        const sz = Number(p.remaining_size ?? p.size ?? 0) * csize;
        const entry = Number(p.entry_price || 0);
        const lev = Number(p.leverage || 1) || 1;
        totalMargin += (entry * sz) / lev;
      }
      const totalPct = totalMargin > 0 ? (totalUnrealized / totalMargin) * 100 : 0;
      const sign = totalUnrealized > 0 ? "+" : totalUnrealized < 0 ? "-" : "";
      pnlEl.textContent = `${sign}${fmtUsd(Math.abs(totalUnrealized))} (${totalPct > 0 ? "+" : ""}${totalPct.toFixed(2)}%)`;
      pnlEl.className = "kpi-sub mono" + pnlClass(totalUnrealized);
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

  const strategy = health.trading_mode || "ai";
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

  $("#hdr-strategy").textContent = fmtStrategyLabel(strategy);
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

  // Show "Reset Circuit Breaker" button only while the circuit breaker is active.
  const cbBtn = $("#btn-reset-circuit-breaker");
  if (cbBtn) {
    cbBtn.classList.toggle("hidden", !risk.circuit_breaker_active);
  }
}

function renderActivity(events, unreadCount) {
  window.__lastActivityEvents = events || [];
  if (unreadCount != null) activityUnreadCount = unreadCount;
  maybeToastNewEvents(events);
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
  const fmtEv = (v) => {
    const n = Number(v);
    if (!Number.isFinite(n) || v == null) return "—";
    const cls = n > 0 ? "positive" : n < 0 ? "negative" : "";
    return `<span class="mono ${cls}">${n >= 0 ? "+" : ""}${n.toFixed(2)}R</span>`;
  };
  const fmtRr = (v) => {
    const n = Number(v);
    return Number.isFinite(n) && v != null && n > 0 ? `<span class="mono">${n.toFixed(2)}</span>` : "—";
  };
  container.innerHTML = `
    <table class="${tableClass}">
      <thead>
        <tr>
          <th>Symbol</th>
          <th>Score</th>
          <th>ML %</th>
          <th title="Decision-engine expected value (R multiples)">EV</th>
          <th title="Reward : risk at signal time">R:R</th>
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
            const reason = s.decision_reason ? escapeHtml(s.decision_reason) : "";
            const data = clickable
              ? `data-symbol="${s.symbol}" data-generated-at="${at}"`
              : "";
            return `
          <tr ${rowClass} ${data}${reason ? ` title="${reason}"` : ""}>
            <td><strong>${s.symbol || "—"}</strong></td>
            <td>${scoreBar(signalScore(s))}</td>
            <td>${s.setup_probability_pct != null ? `<span class="mono">${s.setup_probability_pct}%</span>` : "—"}</td>
            <td>${fmtEv(s.expected_value_r)}</td>
            <td>${fmtRr(s.reward_risk)}</td>
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
          <th>Entry</th>
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
            const entryMode = p.entry_mode || "market";
            const orderStatus = p.order_status || "open";
            const entryTitle =
              entryMode !== "market" && p.limit_price
                ? ` title="Limit @ ${Number(p.limit_price).toPrecision(6)}"`
                : "";
            return `
          <tr ${data}>
            <td><strong>${p.symbol}</strong></td>
            <td><span class="badge ${badge}">${p.side}</span></td>
            <td class="mono"${entryTitle}>${Number(p.entry_price || 0).toPrecision(6)}</td>
            <td class="mono">${mark.toPrecision(6)}</td>
            <td class="mono ${pnlClass(pnl)}">${pnlText}</td>
            <td class="mono ${pnlClass(roiPct)}" title="Price move ${movePct.toFixed(2)}%">${roiSign}${roiPct.toFixed(2)}%</td>
            <td class="mono">${Number(p.remaining_size ?? p.size ?? 0).toFixed(4)}</td>
            <td class="mono">${Number(p.stop_loss || 0).toPrecision(6)}</td>
            <td class="mono">${p.leverage ?? "—"}×</td>
            <td>${p.paper ? '<span class="tag">paper</span>' : '<span class="badge badge-live">live</span>'}</td>
            <td>${fmtStrategyLabel(p.strategy)}</td>
            <td>${fmtEntryModeBadge(entryMode, orderStatus)}</td>
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

// Debounce applySnapshot so a burst of WS messages or HTTP poll catch-up
// ticks (e.g. after unminimising) collapses into a single DOM update.
let _applySnapshotTimer = null;
let _pendingSnapshot = null;
function applySnapshot(snapshot) {
  if (!snapshot || snapshot.error) return;
  _pendingSnapshot = snapshot;
  if (_applySnapshotTimer) return; // already scheduled
  _applySnapshotTimer = requestAnimationFrame(() => {
    _applySnapshotTimer = null;
    const snap = _pendingSnapshot;
    _pendingSnapshot = null;
    if (!snap) return;
    renderMetrics(snap);
    setStatusPill(snap.health, snap.risk);
    syncTradingModeUI(snap.health || {});
    renderActivity(snap.activity, snap.activity_unread);
    renderLiveScan(snap.scan_events, snap.health, snap.signals);
    renderSignalsTable($("#trading-signals-preview"), snap.signals, { limit: 8, timeCompact: true });
    syncOpenPositions(snap.positions);
    $("#last-updated").textContent = fmtManilaTime(new Date());
  });
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

let snapshotFetchInFlight = false;
async function refreshSnapshotHttp() {
  if (snapshotFetchInFlight) return;
  snapshotFetchInFlight = true;
  try {
    const data = await apiGet("/live/snapshot");
    applySnapshot(data);
    setWsUi(true);
  } catch {
    setWsUi(false);
    setStatusPill({ error: true });
  } finally {
    snapshotFetchInFlight = false;
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
  // Cancel any pending reconnect before starting a new connection.
  if (wsReconnectTimer) {
    clearTimeout(wsReconnectTimer);
    wsReconnectTimer = null;
  }
  if (ws) {
    ws.onclose = null; // suppress the onclose reconnect for this intentional close
    ws.close();
    ws = null;
  }
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  ws = new WebSocket(`${proto}//${window.location.host}/ws`);
  ws.onopen = () => setWsUi(true);
  ws.onmessage = (ev) => {
    if (document.hidden) return; // don't process WS messages while minimised
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
    if (document.hidden) return;
    if ($("#auto-refresh")?.checked) refreshSnapshotHttp();
  }, 2000);
  if (marketPollTimer) clearInterval(marketPollTimer);
  marketPollTimer = setInterval(() => {
    if (document.hidden) return;
    if ($("#panel-trading")?.classList.contains("active")) loadDashboardMarket();
  }, 60000);
}

// When the window is restored after being minimised/hidden for a while,
// do exactly one refresh instead of a burst of throttled interval catch-up.
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) {
    // Small delay so the Tauri webview has finished painting before we hit the network.
    setTimeout(() => refreshSnapshotHttp(), 200);
  }
});

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

    // Aggregate setup stats across all strategy rows (legacy rows + "ai").
    const strategies = statsResp.strategies || [];
    let setupResolved = 0;
    let setupWins = 0;
    for (const s of strategies) {
      const resolved = Number(s.total_resolved || s.resolved || 0);
      setupResolved += resolved;
      setupWins += Number(s.wins ?? resolved * Number(s.win_rate || 0));
    }
    const setupWinRate = setupResolved > 0 ? setupWins / setupResolved : 0;
    const trade = learning.trade_stats || {};
    const tradeCount = Number(trade.total || trade.total_trades || 0);
    const tradeWinRate = Number(trade.win_rate || 0);
    const profitFactor = Number(trade.profit_factor || 0);
    const dailyPnlUsd = Number(risk.daily_pnl || 0);
    const dailyPnlPct = Number(risk.daily_pnl_pct || 0);

    const gates = [
      gateRow(
        "Scanner + risk healthy",
        scannerOn && !killOn && !paused,
        `scanner=${scannerOn ? "on" : "off"}, kill=${killOn ? "on" : "off"}`,
        "scanner on, kill off"
      ),
      gateRow("Setup sample size", setupResolved >= 50, String(setupResolved), ">= 50"),
      gateRow(
        "Setup win rate",
        setupWinRate >= 0.5,
        `${(setupWinRate * 100).toFixed(1)}%`,
        ">= 50%"
      ),
      gateRow("Closed trades", tradeCount >= 30, String(tradeCount), ">= 30"),
      gateRow(
        "Profit factor",
        profitFactor >= 1.2,
        profitFactor.toFixed(2),
        ">= 1.20"
      ),
      gateRow(
        "Trade win rate",
        tradeWinRate >= 0.5,
        `${(tradeWinRate * 100).toFixed(1)}%`,
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
    $("#rd-setup-wr").textContent = `${(setupWinRate * 100).toFixed(1)}%`;
    $("#rd-pf").textContent = profitFactor.toFixed(2);
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

function clampPct(n) {
  return Math.max(0, Math.min(100, Number(n) || 0));
}

function fearGreedMeta(value) {
  const v = Number(value);
  if (!Number.isFinite(v)) {
    return { label: "No data", className: "fg-neutral", pct: 50, value: null };
  }
  const pct = clampPct(v);
  if (v <= 24) return { label: "Extreme Fear", className: "fg-extreme-fear", pct, value: v };
  if (v <= 44) return { label: "Fear", className: "fg-fear", pct, value: v };
  if (v <= 55) return { label: "Neutral", className: "fg-neutral", pct, value: v };
  if (v <= 75) return { label: "Greed", className: "fg-greed", pct, value: v };
  return { label: "Extreme Greed", className: "fg-extreme-greed", pct, value: v };
}

function fgArcPath(cx, cy, r, startPct, endPct) {
  const toRad = (pct) => Math.PI * (1 - pct / 100);
  const a1 = toRad(startPct);
  const a2 = toRad(endPct);
  const x1 = cx + r * Math.cos(a1);
  const y1 = cy - r * Math.sin(a1);
  const x2 = cx + r * Math.cos(a2);
  const y2 = cy - r * Math.sin(a2);
  const sweep = endPct - startPct > 50 ? 1 : 0;
  return `M ${x1.toFixed(2)} ${y1.toFixed(2)} A ${r} ${r} 0 ${sweep} 1 ${x2.toFixed(2)} ${y2.toFixed(2)}`;
}

function renderGreedMeterHtml(fearGreed) {
  const meta = fearGreedMeta(fearGreed);
  const cx = 110;
  const cy = 96;
  const r = 76;
  const valueText = meta.value == null ? "—" : Math.round(meta.value);
  const segments = [
    [0, 25, "#ef4444"],
    [25, 45, "#f97316"],
    [45, 55, "#64748b"],
    [55, 75, "#34d399"],
    [75, 100, "#22c55e"],
  ];
  const gap = 0.8;
  const arcs = segments
    .map(
      ([start, end, color]) =>
        `<path d="${fgArcPath(cx, cy, r, start + gap, end - gap)}" fill="none" stroke="${color}" stroke-width="13" stroke-linecap="round" class="fg-arc-seg"/>`
    )
    .join("");

  let needle = "";
  if (meta.value != null) {
    const tipR = r - 22;
    const rot = meta.pct * 1.8 - 90;
    needle = `<g class="fg-needle-wrap" transform="rotate(${rot} ${cx} ${cy})">
      <line x1="${cx}" y1="${cy}" x2="${cx}" y2="${cy - tipR}" class="fg-needle"/>
    </g>
    <circle cx="${cx}" cy="${cy}" r="6" class="fg-hub"/>`;
  }

  return `<div class="fg-gauge ${meta.className}">
    <svg viewBox="0 0 220 122" class="fg-gauge-svg" role="img" aria-label="Fear and Greed Index ${valueText}, ${meta.label}">
      <path d="${fgArcPath(cx, cy, r, 0, 100)}" fill="none" stroke="rgba(148,163,184,0.12)" stroke-width="15" stroke-linecap="round"/>
      ${arcs}
      <text x="20" y="${cy + 20}" class="fg-gauge-tick">FEAR</text>
      <text x="200" y="${cy + 20}" text-anchor="end" class="fg-gauge-tick">GREED</text>
      ${needle}
    </svg>
    <div class="fg-readout">
      <span class="fg-value mono">${valueText}</span>
      <span class="fg-status">${meta.label}</span>
    </div>
  </div>`;
}

function sentimentScoreClass(score) {
  const n = Number(score);
  if (!Number.isFinite(n)) return "flat";
  if (n > 0.15) return "bullish";
  if (n < -0.15) return "bearish";
  return "flat";
}

function formatSentimentScore(score) {
  const n = Number(score);
  if (!Number.isNaN(n)) {
    const sign = n > 0 ? "+" : "";
    return `${sign}${n.toFixed(2)}`;
  }
  return "—";
}

function fmtTimeAgo(iso) {
  if (!iso) return "";
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "";
  const diffSec = Math.max(0, (Date.now() - then) / 1000);
  if (diffSec < 90) return "just now";
  const mins = Math.round(diffSec / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.round(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.round(hrs / 24);
  return `${days}d ago`;
}

function renderNewsSlide(item) {
  const score = Number(item.score);
  const scoreCls = sentimentScoreClass(score);
  const scoreLabel = Number.isFinite(score) ? score.toFixed(2) : "—";
  const symbols = Array.isArray(item.symbols) ? item.symbols : [];
  const url = item.url ? escapeHtml(item.url) : "";
  const title = escapeHtml(item.title || "Untitled");
  const titleHtml = url
    ? `<a href="${url}" target="_blank" rel="noopener noreferrer">${title}</a>`
    : title;
  const age = fmtTimeAgo(item.published_at || item.created_at);
  return `<article class="news-slide">
    <div class="news-slide-meta">
      <span class="news-slide-source">${escapeHtml(item.source || "news")}</span>
      ${age ? `<span class="news-slide-age">${age}</span>` : ""}
      <span class="news-slide-score ${scoreCls}">${scoreLabel}</span>
    </div>
    <h3 class="news-slide-title">${titleHtml}</h3>
    ${symbols.length ? `<div class="news-slide-symbols">${symbols.slice(0, 6).map((s) => `<span class="news-symbol-chip">${escapeHtml(s)}</span>`).join("")}</div>` : ""}
  </article>`;
}

function stopDashboardNewsAutoplay() {
  if (dashboardNewsAutoTimer) {
    clearInterval(dashboardNewsAutoTimer);
    dashboardNewsAutoTimer = null;
  }
}

function restartCarouselProgress() {
  const bar = $("#dash-news-progress");
  const carousel = $("#dash-news-carousel");
  if (carousel) carousel.style.setProperty("--news-auto-ms", `${DASHBOARD_NEWS_AUTO_MS}ms`);
  if (!bar) return;
  bar.classList.remove("hidden");
  bar.classList.remove("anim");
  void bar.offsetWidth;
  bar.classList.add("anim");
}

function startDashboardNewsAutoplay() {
  stopDashboardNewsAutoplay();
  const pageCount = Math.ceil(dashboardNewsItems.length / DASHBOARD_NEWS_PER_PAGE);
  if (pageCount <= 1) {
    $("#dash-news-progress")?.classList.add("hidden");
    return;
  }
  restartCarouselProgress();
  dashboardNewsAutoTimer = setInterval(() => {
    dashboardNewsPage = (dashboardNewsPage + 1) % pageCount;
    renderDashboardNewsCarousel({ autoplay: true });
  }, DASHBOARD_NEWS_AUTO_MS);
}

function goDashboardNewsPage(delta) {
  const pageCount = Math.max(1, Math.ceil(dashboardNewsItems.length / NEWS_TABLE_PER_PAGE));
  dashboardNewsPage = (dashboardNewsPage + delta + pageCount) % pageCount;
  renderDashboardNewsFeed();
}

function renderDashboardNewsCarousel({ autoplay = false } = {}) {
  const carousel = $("#dash-news-carousel");
  const track = $("#dash-news-track");
  const dots = $("#dash-news-dots");
  const counter = $("#dash-news-counter");
  const prevBtn = $("#dash-news-prev");
  const nextBtn = $("#dash-news-next");
  const viewport = track?.parentElement;
  if (!track || !viewport) return;

  const items = dashboardNewsItems;
  const pageCount = Math.max(1, Math.ceil(items.length / DASHBOARD_NEWS_PER_PAGE));

  if (!items.length) {
    stopDashboardNewsAutoplay();
    track.innerHTML = "";
    track.style.transform = "";
    if (dots) dots.innerHTML = "";
    if (counter) counter.textContent = "0 items";
    carousel?.classList.remove("news-carousel--live");
    viewport.innerHTML = '<div class="news-carousel-empty">No headlines yet — starts when the scanner runs</div>';
    prevBtn?.setAttribute("disabled", "disabled");
    nextBtn?.setAttribute("disabled", "disabled");
    $("#dash-news-progress")?.classList.add("hidden");
    return;
  }

  if (!viewport.querySelector(".news-carousel-track")) {
    viewport.innerHTML = `<div id="dash-news-track" class="news-carousel-track"></div><div id="dash-news-progress" class="news-carousel-progress anim"></div>`;
  }
  const liveTrack = $("#dash-news-track");
  if (!liveTrack) return;

  dashboardNewsPage = ((dashboardNewsPage % pageCount) + pageCount) % pageCount;
  carousel?.classList.add("news-carousel--live");

  const slides = [];
  for (let p = 0; p < pageCount; p += 1) {
    const chunk = items.slice(p * DASHBOARD_NEWS_PER_PAGE, (p + 1) * DASHBOARD_NEWS_PER_PAGE);
    slides.push(`<div class="news-carousel-page" data-page="${p}">${chunk.map(renderNewsSlide).join("")}</div>`);
  }
  liveTrack.innerHTML = slides.join("");
  liveTrack.style.transform = `translate3d(-${dashboardNewsPage * 100}%, 0, 0)`;

  const MAX_DOTS = 8;
  if (dots) {
    if (pageCount <= MAX_DOTS) {
      dots.classList.remove("news-dots--bar");
      dots.innerHTML = Array.from({ length: pageCount }, (_, i) =>
        `<button type="button" class="news-dot ${i === dashboardNewsPage ? "active" : ""}" data-page="${i}" aria-label="Headline ${i + 1} of ${pageCount}" aria-current="${i === dashboardNewsPage ? "true" : "false"}"></button>`
      ).join("");
    } else {
      dots.classList.add("news-dots--bar");
      const fillPct = clampPct(((dashboardNewsPage + 1) / pageCount) * 100);
      dots.innerHTML = `<div class="news-dots-track" data-seek-track title="Jump to headline"><div class="news-dots-fill" style="width:${fillPct}%"></div></div>`;
    }
  }
  if (counter) counter.textContent = `${dashboardNewsPage + 1} / ${pageCount}`;
  prevBtn?.toggleAttribute("disabled", pageCount <= 1);
  nextBtn?.toggleAttribute("disabled", pageCount <= 1);

  if (!autoplay) startDashboardNewsAutoplay();
  else restartCarouselProgress();
}

const FGI_COLORS = {
  "fg-extreme-fear": "#ef4444",
  "fg-fear": "#f97316",
  "fg-neutral": "#94a3b8",
  "fg-greed": "#34d399",
  "fg-extreme-greed": "#22c55e",
};

function renderGreedWidgetHtml(currentValue, fngHistory = []) {
  const meta = fearGreedMeta(currentValue);
  const color = FGI_COLORS[meta.className] || "var(--text)";
  const valueText = meta.value == null ? "—" : Math.round(meta.value);

  const periods = [
    { label: "Yesterday", idx: 1 },
    { label: "1 Week", idx: 6 },
    { label: "1 Month", idx: 29 },
  ];

  const historyHtml = fngHistory.length
    ? `<div class="greed-history-row">${periods.map(({ label, idx }) => {
        const entry = fngHistory[idx];
        const m = entry ? fearGreedMeta(entry.value) : null;
        const c = m ? (FGI_COLORS[m.className] || "var(--muted)") : "var(--muted)";
        return `<div class="greed-history-item">
          <span class="greed-history-period">${label}</span>
          <span class="greed-history-val" style="color:${c}">${m ? Math.round(m.value) : "—"}</span>
          <span class="greed-history-lbl">${m ? m.label : "—"}</span>
        </div>`;
      }).join("")}</div>`
    : "";

  return `<div class="greed-gauge-num" style="color:${color}">${valueText}</div>
    <div class="greed-gauge-label" style="color:${color}">${meta.label}</div>
    ${historyHtml}`;
}

function renderDashboardGreedMeter(status = {}, fngHistory = []) {
  const widget = document.getElementById("dash-greed-widget");
  if (widget) {
    widget.innerHTML = renderGreedWidgetHtml(status.fear_greed, fngHistory);
  }
  const globalEl = $("#dash-global-sentiment");
  if (globalEl) {
    const g = Number(status.global_score ?? 0);
    globalEl.textContent = formatSentimentScore(g);
    globalEl.className = "sentiment-pill mono" + (g > 0.2 ? " positive" : g < -0.2 ? " negative" : "");
  }
}

/// Human label for the local-LLM market regime (Phase 4/7).
function regimeSummary(llm = {}) {
  const r = llm.regime || {};
  if (llm.enabled === false) return { label: "Disabled", cls: "", title: "LLM regime layer disabled in settings" };
  if (!llm.available) {
    return {
      label: "Neutral (offline)",
      cls: "",
      title: llm.last_error ? `Ollama offline — trading on ML alone. ${llm.last_error}` : "Ollama offline — trading on ML alone",
    };
  }
  const parts = [];
  if (r.trending) parts.push("Trending");
  if (r.chop) parts.push("Chop");
  if (r.high_vol) parts.push("High vol");
  if (r.risk_off) parts.push("Risk-off");
  if (!parts.length) parts.push("Neutral");
  const bias = Number(r.btc_bias ?? 0);
  const conf = Number(r.confidence ?? 0);
  const biasLabel = bias > 0.15 ? "bullish" : bias < -0.15 ? "bearish" : "flat";
  return {
    label: `${parts.join(" · ")} · ${biasLabel} ${(conf * 100).toFixed(0)}%`,
    cls: bias > 0.15 ? " positive" : bias < -0.15 ? " negative" : "",
    title: `BTC bias ${bias >= 0 ? "+" : ""}${bias.toFixed(2)} · confidence ${(conf * 100).toFixed(0)}% · updated ${llm.updated_at || "—"}`,
  };
}

function renderDashboardRegime(llm = {}) {
  const el = $("#dash-llm-regime");
  if (!el) return;
  const { label, cls, title } = regimeSummary(llm);
  el.textContent = label;
  el.className = "sentiment-pill" + cls;
  el.title = title;
}

function renderNewsFeedRow(item) {
  const score = Number(item.score);
  const scoreCls = sentimentScoreClass(score);
  const scoreLabel = Number.isFinite(score) ? (score >= 0 ? `+${score.toFixed(2)}` : score.toFixed(2)) : "—";
  const url = item.url ? escapeHtml(item.url) : "";
  const title = escapeHtml(item.title || "Untitled");
  const titleHtml = url
    ? `<a href="${url}" target="_blank" rel="noopener noreferrer" title="${title}">${title}</a>`
    : `<span title="${title}">${title}</span>`;
  const age = fmtTimeAgo(item.published_at || item.created_at) || "—";
  return `<tr>
    <td class="news-col-source">${escapeHtml(item.source || "—")}</td>
    <td class="news-col-title">${titleHtml}</td>
    <td class="news-col-score"><span class="news-feed-score ${scoreCls}">${scoreLabel}</span></td>
    <td class="news-col-age">${age}</td>
  </tr>`;
}

function renderDashboardNewsFeed() {
  const tbody = document.getElementById("dash-news-tbody");
  const counter = $("#dash-news-counter");
  const info = $("#dash-news-info");
  const prevBtn = $("#dash-news-prev");
  const nextBtn = $("#dash-news-next");
  const pagination = $("#dash-news-pagination");
  if (!tbody) return;

  const items = dashboardNewsItems;
  const pageCount = Math.max(1, Math.ceil(items.length / NEWS_TABLE_PER_PAGE));
  dashboardNewsPage = Math.min(Math.max(0, dashboardNewsPage), pageCount - 1);

  if (counter) counter.textContent = items.length ? `${items.length} items` : "—";

  if (!items.length) {
    tbody.innerHTML = '<tr><td colspan="4" class="news-feed-empty">No headlines yet — starts when the scanner runs</td></tr>';
    if (pagination) pagination.classList.add("hidden");
    return;
  }

  const start = dashboardNewsPage * NEWS_TABLE_PER_PAGE;
  tbody.innerHTML = items.slice(start, start + NEWS_TABLE_PER_PAGE).map(renderNewsFeedRow).join("");
  if (info) info.textContent = `${start + 1}–${Math.min(start + NEWS_TABLE_PER_PAGE, items.length)} of ${items.length}`;
  if (prevBtn) prevBtn.toggleAttribute("disabled", dashboardNewsPage === 0);
  if (nextBtn) nextBtn.toggleAttribute("disabled", dashboardNewsPage >= pageCount - 1);
  if (pagination) pagination.classList.toggle("hidden", pageCount <= 1);
}

async function fetchFngHistory() {
  try {
    const resp = await fetch("https://api.alternative.me/fng/?limit=30&format=json");
    if (!resp.ok) return [];
    const data = await resp.json();
    return (data.data || []).map((d) => ({ value: Number(d.value), label: d.value_classification }));
  } catch {
    return [];
  }
}

async function loadDashboardMarket() {
  if (!$("#panel-trading")?.classList.contains("active")) return;
  try {
    const [status, newsResp, fngHistory, llmStatus] = await Promise.all([
      apiGet("/sentiment/status").catch(() => ({})),
      apiGet("/sentiment/news?limit=20").catch(() => ({ items: [] })),
      fetchFngHistory(),
      apiGet("/llm/status").catch(() => ({})),
    ]);
    renderDashboardGreedMeter(status, fngHistory);
    renderDashboardRegime(llmStatus);
    const fromDb = newsResp.items || [];
    const fromMem = status.headlines || [];
    dashboardNewsItems = (fromDb.length ? fromDb : fromMem).slice(0, 20);
    renderDashboardNewsFeed();
  } catch (e) {
    const feed = $("#dash-news-feed");
    if (feed) feed.innerHTML = `<div class="news-feed-empty">${escapeHtml(e.message || "Failed to load")}</div>`;
  }
}

function initDashboardMarketControls() {
  $("#dash-news-prev")?.addEventListener("click", () => goDashboardNewsPage(-1));
  $("#dash-news-next")?.addEventListener("click", () => goDashboardNewsPage(1));
  $("#dash-news-dots")?.addEventListener("click", (ev) => {
    const btn = ev.target.closest("[data-page]");
    if (btn) {
      dashboardNewsPage = Number(btn.dataset.page) || 0;
      renderDashboardNewsCarousel();
      startDashboardNewsAutoplay();
      return;
    }
    const track = ev.target.closest("[data-seek-track]");
    if (track) {
      const rect = track.getBoundingClientRect();
      const ratio = clampPct(((ev.clientX - rect.left) / Math.max(1, rect.width)) * 100) / 100;
      const pageCount = Math.max(1, Math.ceil(dashboardNewsItems.length / DASHBOARD_NEWS_PER_PAGE));
      dashboardNewsPage = Math.min(pageCount - 1, Math.floor(ratio * pageCount));
      renderDashboardNewsCarousel();
      startDashboardNewsAutoplay();
    }
  });
  const carousel = $("#dash-news-carousel");
  carousel?.addEventListener("mouseenter", stopDashboardNewsAutoplay);
  carousel?.addEventListener("mouseleave", startDashboardNewsAutoplay);
  carousel?.addEventListener("focusin", stopDashboardNewsAutoplay);
  carousel?.addEventListener("focusout", startDashboardNewsAutoplay);

  let touchStartX = 0;
  carousel?.addEventListener("touchstart", (ev) => {
    touchStartX = ev.changedTouches[0]?.clientX ?? 0;
    stopDashboardNewsAutoplay();
  }, { passive: true });
  carousel?.addEventListener("touchend", (ev) => {
    const dx = (ev.changedTouches[0]?.clientX ?? 0) - touchStartX;
    if (Math.abs(dx) > 40) goDashboardNewsPage(dx < 0 ? 1 : -1);
    else startDashboardNewsAutoplay();
  }, { passive: true });
}

async function loadTrainingTab() {
  clearTrainingError();
  setTrainingLoading(true);
  try {
    const [data, sentiment, promotion, tuner, llmStatus] = await Promise.all([
      apiGet("/ml/history"),
      apiGet("/sentiment/status").catch(() => ({})),
      apiGet("/promotion/status").catch(() => ({})),
      apiGet("/tuner/history").catch(() => ({ runs: [] })),
      apiGet("/llm/status").catch(() => ({})),
    ]);
    renderTrainingModels(data);
    renderPromotionPanel(promotion);
    renderSentimentPanel(sentiment, llmStatus);
    renderTunerHistory(tuner);
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

function renderPromotionPanel(payload) {
  const el = $("#training-promotion-stats");
  const badge = $("#training-promotion-badge");
  const card = $("#training-promotion-card");
  if (!el) return;
  const m = payload.metrics || {};
  const c = m.criteria || {};
  const ready = !!m.live_promotion_ready;
  const minTrades = Number(c.min_trades ?? 300);
  const minPf = Number(c.min_profit_factor ?? 1.3);
  const minWr = 0.45;
  const resolved = Number(m.resolved_trades ?? 0);
  const pf = Number(m.profit_factor ?? 0);
  const wr = Number(m.win_rate ?? 0);
  const halts = Number(m.drawdown_halts_30d ?? 0);

  if (badge) {
    badge.textContent = ready ? "Ready" : "Paper";
    badge.className = "tag " + (ready ? "tag-ok" : "");
  }
  card?.classList.toggle("promo-ready", ready);

  const criteria = [
    {
      label: "Resolved trades",
      current: `${resolved}`,
      target: `≥ ${minTrades}`,
      pct: clampPct((resolved / minTrades) * 100),
      pass: resolved >= minTrades,
    },
    {
      label: "Profit factor",
      current: pf.toFixed(2),
      target: `≥ ${minPf}`,
      pct: clampPct((pf / minPf) * 100),
      pass: pf >= minPf,
    },
    {
      label: "Win rate",
      current: pctText(wr),
      target: `≥ ${(minWr * 100).toFixed(0)}%`,
      pct: clampPct((wr / minWr) * 100),
      pass: wr >= minWr,
    },
    {
      label: "Drawdown halts (30d)",
      current: String(halts),
      target: "0",
      pct: halts === 0 ? 100 : 20,
      pass: halts === 0,
    },
  ];

  el.innerHTML = `<div class="promo-panel ${ready ? "promo-ready" : ""}">
    <div class="promo-hero">
      <div class="promo-icon">${ready ? "✓" : "◎"}</div>
      <div>
        <h3 class="promo-title">${ready ? "Ready for live review" : "Keep paper trading"}</h3>
        <p class="promo-sub">${ready ? "Core promotion gates passed — review risk before enabling live." : "Build sample size and stability before live capital."}</p>
      </div>
    </div>
    <div class="promo-criteria">
      ${criteria.map((row) => `<div class="criterion-row ${row.pass ? "pass" : "fail"}">
        <div class="criterion-head"><span>${row.label}</span><span class="mono">${row.current} <span class="hint">/ ${row.target}</span></span></div>
        <div class="criterion-bar"><span style="width:${row.pct}%"></span></div>
      </div>`).join("")}
    </div>
  </div>`;
}

function renderSentimentPanel(s, llmStatus = {}) {
  const stats = $("#training-sentiment-stats");
  const headlines = $("#training-sentiment-headlines");
  const badge = $("#training-sentiment-score");
  const g = Number(s.global_score ?? 0);
  if (badge) {
    badge.textContent = `News ${formatSentimentScore(g)}`;
    badge.className = "tag " + (g > 0.2 ? "tag-ok" : g < -0.2 ? "tag-warn" : "");
  }
  if (stats) {
    const fgi = fearGreedMeta(s.fear_greed);
    const fgiColor = FGI_COLORS[fgi.className] || "var(--text)";
    const markerLeft = clampPct(((g + 1) / 2) * 100);
    stats.innerHTML = `<div class="training-sentiment-compact">
      <div class="training-sentiment-scores">
        <div class="training-sentiment-score-block">
          <span class="training-sentiment-score-label">Fear &amp; Greed</span>
          <span class="training-sentiment-score-val mono" style="color:${fgiColor}">${fgi.value != null ? Math.round(fgi.value) : "—"}</span>
          <span class="hint">${fgi.label}</span>
        </div>
        <div class="training-sentiment-score-block">
          <span class="training-sentiment-score-label">News sentiment</span>
          <span class="training-sentiment-score-val mono ${g > 0.2 ? "positive" : g < -0.2 ? "negative" : ""}">${formatSentimentScore(g)}</span>
        </div>
        <div class="training-sentiment-meta">
          <span>${s.headline_count ?? 0} headlines</span>
          <span>${Object.keys(s.symbol_scores || {}).length} symbols</span>
        </div>
      </div>
      <div class="sentiment-bar-track" aria-hidden="true">
        <div class="sentiment-bar-marker" style="left:${markerLeft}%"></div>
      </div>
      ${(() => {
        const { label, cls, title } = regimeSummary(llmStatus);
        const modelTag = llmStatus.available && llmStatus.model ? ` · ${escapeHtml(llmStatus.model)}` : "";
        return `<div class="sentiment-mini-row">
          <span class="hint">LLM regime${modelTag}</span>
          <span class="sentiment-pill${cls}" title="${escapeHtml(title)}">${escapeHtml(label)}</span>
        </div>`;
      })()}
    </div>`;
  }
  if (headlines) {
    const items = (s.headlines || []).slice(0, 4);
    if (!items.length) {
      headlines.innerHTML = '<span class="empty">No headlines yet</span>';
      return;
    }
    headlines.innerHTML = items.map((h) => {
      const score = Number(h.score);
      const cls = score > 0.15 ? "up" : score < -0.15 ? "down" : "flat";
      const url = h.url ? escapeHtml(h.url) : "";
      const title = escapeHtml(h.title || "");
      const titleHtml = url
        ? `<a href="${url}" target="_blank" rel="noopener noreferrer">${title}</a>`
        : title;
      return `<div class="headline-card">
        <span class="headline-card-score ${cls}">${Number.isFinite(score) ? score.toFixed(2) : "—"}</span>
        <div class="headline-card-body">
          <div class="headline-card-source">${escapeHtml(h.source || "news")}</div>
          <p class="headline-card-title">${titleHtml}</p>
        </div>
      </div>`;
    }).join("");
  }
}

function formatTunerParams(params) {
  if (!params || typeof params !== "object") return "—";
  const parts = [];
  const score = params.min_composite_score;
  const thr = params.supervised_threshold;
  const sl = params.default_sl_pct;
  if (score != null && !Number.isNaN(Number(score))) parts.push(`Score ${Number(score)}`);
  if (thr != null && !Number.isNaN(Number(thr))) parts.push(`Gate ${(Number(thr) * 100).toFixed(0)}%`);
  if (sl != null && !Number.isNaN(Number(sl))) parts.push(`SL ${(Number(sl) * 100).toFixed(1)}%`);
  return parts.length ? parts.join(" · ") : "—";
}

function formatTunerPct(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return "—";
  return `${(n * 100).toFixed(1)}%`;
}

function renderTunerHistory(payload) {
  const el = $("#training-tuner-history");
  if (!el) return;
  const runs = payload.runs || [];
  if (!runs.length) {
    el.innerHTML = '<span class="empty">No tuner runs yet — runs every 6h when enabled</span>';
    return;
  }
  el.innerHTML = `<table class="data tuner-table">
    <thead>
      <tr>
        <th>Date</th>
        <th>Challenger</th>
        <th class="col-num">Test WR</th>
        <th class="col-num">Return</th>
        <th class="col-num">Trades</th>
        <th class="col-num">Score</th>
        <th>Result</th>
      </tr>
    </thead>
    <tbody>${runs.map((r) => {
      const oos = r.oos_metrics || {};
      const promoted = !!r.promoted;
      const testWr = oos.test_win_rate;
      const testRet = oos.test_return_pct;
      const trades = oos.test_n ?? oos.test_trades;
      const score = oos.score;
      return `<tr class="${promoted ? "tuner-row-promoted" : ""}">
        <td class="col-time" title="${escapeHtml(r.created_at || "")}">${fmtManilaDateTime(r.created_at)}</td>
        <td class="tuner-params-cell">${escapeHtml(formatTunerParams(r.challenger))}</td>
        <td class="col-num mono">${testWr != null ? formatTunerPct(testWr) : "—"}</td>
        <td class="col-num mono ${Number(testRet) >= 0 ? "positive" : "negative"}">${testRet != null ? formatTunerPct(testRet) : "—"}</td>
        <td class="col-num mono">${trades ?? "—"}</td>
        <td class="col-num mono">${score != null ? Number(score).toFixed(2) : "—"}</td>
        <td><span class="tag ${promoted ? "tag-ok" : ""}">${promoted ? "Promoted" : "Suggest"}</span></td>
      </tr>`;
    }).join("")}</tbody>
  </table>`;
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
  const threshold = model.effective_threshold ?? cfg.supervised_threshold ?? om.gate_threshold;
  const thresholdPct = threshold != null ? (Number(threshold) * 100).toFixed(0) : "—";

  const activeLabel =
    active === "ensemble"
      ? "Ensemble"
      : active === "online"
        ? "Online"
        : active === "onnx"
          ? "ONNX only"
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
    const blendW = model.ensemble_online_weight;
    const blendLabel =
      blendW != null
        ? `${Math.round(Number(blendW) * 100)}% online / ${Math.round((1 - Number(blendW)) * 100)}% ONNX`
        : "—";
    const kellyLabel = model.kelly_fraction != null ? `${Number(model.kelly_fraction).toFixed(2)}× fractional` : "—";
    const rFmt = (v) => (v != null && Number.isFinite(Number(v)) ? `${Number(v).toFixed(2)}R` : "—");
    gateStats.innerHTML = `
      <table class="data"><tbody>
        <tr><td>ML threshold</td><td class="mono">≥ ${thresholdPct}%</td></tr>
        <tr><td>Hard gate</td><td>${cfg.hard_ml_gate ? "On" : "Off"}</td></tr>
        <tr><td>Ensemble blend</td><td class="mono">${blendLabel}</td></tr>
        <tr><td>Kelly sizing</td><td class="mono">${kellyLabel}</td></tr>
        <tr><td>Realized avg win / loss</td><td class="mono">${rFmt(model.avg_win_r)} / ${rFmt(model.avg_loss_r)}</td></tr>
        <tr><td>Auto ONNX retrain</td><td class="mono">${model.auto_retrain_enabled ? "ON" : "OFF"}</td></tr>
        <tr><td>Auto gate</td><td class="mono">${model.gate_auto_enabled ? "ON" : "OFF"}</td></tr>
        <tr><td>Trade learn weights</td><td class="mono">${cfg.trade_win_weight ?? 2}× win / ${cfg.trade_loss_weight ?? 3.5}× loss</td></tr>
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
  card.classList.remove("hidden");
  if (res.error) {
    resultEl.innerHTML = `<span class="negative">${res.error}</span>`;
    return;
  }
  if (badge) {
    badge.textContent = `${res.traded ?? 0} trades`;
    badge.className = "tag tag-ok";
  }
  const r4 = (v) => v != null ? (Number(v) * 100).toFixed(2) + "%" : "—";
  const gateLabel =
    res.settings?.gate === "decision_engine"
      ? "Decision engine (EV + R:R gates)"
      : res.settings?.ml_threshold != null
        ? `ML threshold ≥ ${Number(res.settings.ml_threshold).toFixed(1)}%`
        : "—";
  const pf = res.profit_factor;
  resultEl.innerHTML = `<table class="data"><tbody>
    <tr><td>Total signals</td><td>${res.total_signals ?? "—"}</td></tr>
    <tr><td>Filtered by gate</td><td>${res.filtered ?? res.filtered_by_ml ?? "—"}</td></tr>
    <tr><td>Traded</td><td>${res.traded ?? "—"}</td></tr>
    <tr><td>Wins / Losses / Expired</td><td>${res.wins ?? 0} / ${res.losses ?? 0} / ${res.expired ?? 0}</td></tr>
    <tr><td>Win rate</td><td>${r4(res.win_rate)}</td></tr>
    <tr><td>Profit factor</td><td class="mono">${pf != null ? Number(pf).toFixed(2) : res.traded > 0 && res.losses === 0 ? "∞ (no losses)" : "—"}</td></tr>
    <tr><td>Total return</td><td class="${Number(res.total_return_pct) >= 0 ? "positive" : "negative"}">${r4(res.total_return_pct)}</td></tr>
    <tr><td>Max drawdown</td><td class="negative">${r4(res.max_drawdown_pct)}</td></tr>
    <tr><td>Expectancy / trade</td><td>${r4(res.expectancy_per_trade)}</td></tr>
    <tr><td>Avg win / avg loss</td><td class="mono">${r4(res.avg_win)} / ${r4(res.avg_loss)}</td></tr>
    <tr><td>Gate</td><td>${gateLabel}</td></tr>
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
  card.classList.remove("hidden");
  if (res.error) {
    resultEl.innerHTML = `<span class="negative">${res.error}</span>`;
    return;
  }
  const pct = (v) => v != null ? (Number(v) * 100).toFixed(1) + "%" : "—";
  const oos = res.out_of_sample || {};
  const pnl = oos.traded_pnl || {};
  resultEl.innerHTML = `<table class="data"><tbody>
    <tr><td>Train samples</td><td>${res.train_samples ?? "—"}</td></tr>
    <tr><td>Test samples (OOS)</td><td>${res.test_samples ?? "—"}</td></tr>
    <tr><td>In-sample win rate</td><td>${pct(res.in_sample_win_rate)}</td></tr>
    <tr><td colspan="2" style="font-weight:600;padding-top:.5rem">Out-of-Sample (unseen data)</td></tr>
    <tr><td>OOS accuracy</td><td class="${Number(oos.accuracy) > 0.5 ? "positive" : "negative"}">${pct(oos.accuracy)}</td></tr>
    <tr><td>OOS win rate</td><td>${pct(oos.win_rate)}</td></tr>
    <tr><td>OOS precision</td><td>${pct(oos.precision)}</td></tr>
    <tr><td>OOS trades evaluated</td><td>${oos.total ?? "—"}</td></tr>
    <tr><td colspan="2" style="font-weight:600;padding-top:.5rem">Model-traded PnL (OOS)</td></tr>
    <tr><td>Trades taken</td><td>${pnl.traded ?? "—"}</td></tr>
    <tr><td>Traded win rate</td><td>${pct(pnl.win_rate)}</td></tr>
    <tr><td>Total return</td><td class="${Number(pnl.total_return_pct) >= 0 ? "positive" : "negative"}">${pct(pnl.total_return_pct)}</td></tr>
    <tr><td>Profit factor</td><td class="mono">${pnl.profit_factor != null ? Number(pnl.profit_factor).toFixed(2) : "—"}</td></tr>
    <tr><td>Max drawdown</td><td class="negative">${pct(pnl.max_drawdown_pct)}</td></tr>
  </tbody></table>`;
}

function renderAcceptanceResult(res) {
  const card = $("#training-acceptance-card");
  const resultEl = $("#training-acceptance-result");
  const badge = $("#training-acceptance-badge");
  if (!card || !resultEl) return;
  card.classList.remove("hidden");
  if (res.error) {
    resultEl.innerHTML = `<span class="negative">${escapeHtml(res.error)}</span>`;
    if (badge) { badge.textContent = "Error"; badge.className = "tag tag-warn"; }
    return;
  }
  const acc = res.acceptance || {};
  const passed = acc.passed === true;
  if (badge) {
    badge.textContent = passed ? "PASSED" : "NOT READY";
    badge.className = "tag " + (passed ? "tag-ok" : "tag-warn");
  }
  const fmtVal = (v) => {
    if (v == null) return "—";
    const n = Number(v);
    if (!Number.isFinite(n)) return escapeHtml(String(v));
    return Number.isInteger(n) ? String(n) : n.toFixed(3);
  };
  const checks = (acc.checks || []).map((c) => `
    <tr>
      <td>${escapeHtml(c.check || "—")}</td>
      <td class="mono">${fmtVal(c.actual)}</td>
      <td class="mono">${fmtVal(c.required)}</td>
      <td class="${c.passed ? "positive" : "negative"}">${c.passed ? "✓ pass" : "✗ fail"}</td>
    </tr>`).join("");
  const liveNote = res.live_trading_enabled
    ? '<span class="negative">Live trading is already enabled.</span>'
    : passed
      ? "Gates cleared — you may enable live trading in settings."
      : "Keep paper trading until every gate passes.";
  resultEl.innerHTML = `
    <table class="data">
      <thead><tr><th>Check</th><th>Actual</th><th>Required</th><th>Result</th></tr></thead>
      <tbody>${checks || '<tr><td colspan="4" class="empty">No checks returned</td></tr>'}</tbody>
    </table>
    <p class="hint" style="margin-top:.5rem">${acc.summary ? escapeHtml(acc.summary) + " · " : ""}${liveNote}</p>`;
  renderBacktestResult(res.metrics || {});
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
      if (id === "trading") {
        loadBalanceHistory();
        loadDashboardMarket();
      }
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

  $("#btn-reset-circuit-breaker")?.addEventListener("click", async () => {
    const fb = $("#control-feedback");
    try {
      const res = await apiPost("/risk/circuit-breaker/reset");
      if (res.error) throw new Error(res.error);
      showFeedback(fb, "Circuit breaker reset — trading resumed.", true);
      await refreshSnapshotHttp();
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

  $("#btn-training-acceptance")?.addEventListener("click", async () => {
    const fb = $("#training-feedback");
    showFeedback(fb, "Replaying history through the decision engine and checking go-live gates…", true);
    try {
      const res = await apiPost("/backtest/acceptance", {});
      renderAcceptanceResult(res);
      const passed = res.acceptance?.passed === true;
      showFeedback(fb, passed ? "Acceptance gate PASSED — strategy cleared all go-live minimums." : "Acceptance gate not passed yet — see the checks below.", passed);
    } catch (e) {
      showFeedback(fb, `Acceptance check failed: ${e.message}`, false);
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
  applyNotificationPreferences(tg);
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
      showFeedback(fb, "Notification preferences saved", true);
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
    toastNotificationsEnabled = ev.target.checked;
    try {
      const res = await apiPut("/user/telegram", { telegram_enabled: ev.target.checked });
      if (res.error) throw new Error(res.error);
      const label = $("#tg-enabled-label");
      if (label) label.textContent = ev.target.checked ? "Yes" : "No";
      if (res.telegram) applyNotificationPreferences(res.telegram);
    } catch (e) {
      showFeedback(fb, e.message, false);
    }
  });
}

// ── Tauri in-app update banner ────────────────────────────────────────────────
// Called by the Tauri desktop backend via win.eval(...) when a newer version is
// available on GitHub Releases.  Works only when running inside the Tauri app;
// silently no-ops in the browser.

let _pendingUpdateInstalling = false;

window.showUpdateBanner = function showUpdateBanner(version, notes) {
  const banner = $("#update-banner");
  if (!banner) return;
  const title = $("#update-banner-title");
  const notesEl = $("#update-banner-notes");
  if (title) title.textContent = `Update available: v${version}`;
  if (notesEl) notesEl.textContent = notes ? notes.slice(0, 120) : "";
  banner.classList.remove("hidden");
};

function initUpdateBanner() {
  const banner = $("#update-banner");
  const installBtn = $("#btn-update-install");
  const dismissBtn = $("#btn-update-dismiss");
  if (!banner) return;

  dismissBtn?.addEventListener("click", () => {
    banner.classList.add("hidden");
  });

  installBtn?.addEventListener("click", async () => {
    if (_pendingUpdateInstalling) return;
    _pendingUpdateInstalling = true;
    installBtn.disabled = true;
    installBtn.textContent = "Installing…";
    try {
      // Use Tauri IPC if available (desktop only).
      if (window.__TAURI__?.core?.invoke) {
        await window.__TAURI__.core.invoke("install_update");
      } else {
        // Fallback: open the GitHub releases page in the default browser.
        window.open("https://github.com/OWNER/mexc-trading-bot-rust/releases/latest", "_blank", "noopener");
        banner.classList.add("hidden");
      }
    } catch (err) {
      installBtn.textContent = "Install & Restart";
      installBtn.disabled = false;
      _pendingUpdateInstalling = false;
      const notesEl = $("#update-banner-notes");
      if (notesEl) notesEl.textContent = `Install failed: ${err}. Download from GitHub Releases.`;
    }
  });
}

async function init() {
  initTabs();
  initControls();
  initTelegramControls();
  initDashboardMarketControls();
  initUpdateBanner();
  await loadUserProfile();
  await refreshSnapshotHttp();
  await loadBalanceHistory();
  await loadDashboardMarket();
  connectWebSocket();
  startPolling();
}

init();
