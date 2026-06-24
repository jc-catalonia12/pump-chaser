/**
 * Master label map for audit / notification event types.
 * Maps internal event_type codes to display label, description, tone, and icon.
 */

const ICONS = {
  kill_switch: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><line x1="9" y1="9" x2="15" y2="15"/><line x1="15" y1="9" x2="9" y2="15"/></svg>`,
  trade_blocked: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="10"/><line x1="4.93" y1="4.93" x2="19.07" y2="19.07"/></svg>`,
  position_opened: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><polyline points="22 7 13.5 15.5 8.5 10.5 2 17"/><polyline points="16 7 22 7 22 13"/></svg>`,
  position_closed: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><polyline points="22 17 13.5 8.5 8.5 13.5 2 7"/><polyline points="16 17 22 17 22 11"/></svg>`,
  tp_hit: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M12 2v20M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6"/></svg>`,
  breakeven: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><line x1="5" y1="12" x2="19" y2="12"/><polyline points="12 5 19 12 12 19"/></svg>`,
  cut_loss: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>`,
  live_order: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="2" y="7" width="20" height="14" rx="2"/><path d="M16 7V5a2 2 0 0 0-2-2h-4a2 2 0 0 0-2 2v2"/></svg>`,
  live_order_error: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="2" y="7" width="20" height="14" rx="2"/><path d="M12 11v4M12 17h.01"/></svg>`,
  live_order_dry_run: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/></svg>`,
  position_rollback: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><polyline points="1 4 1 10 7 10"/><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10"/></svg>`,
  exchange: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><polyline points="23 4 23 10 17 10"/><polyline points="1 20 1 14 7 14"/><path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"/></svg>`,
  scanner: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>`,
  signal: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>`,
  scan: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7z"/><circle cx="12" cy="12" r="3"/></svg>`,
  strategy: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>`,
  model_learn: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z"/><path d="M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z"/></svg>`,
  shadow: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="10" stroke-dasharray="4 2"/><path d="M8 12h8"/></svg>`,
  default: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>`,
};

/** @type {Record<string, { label: string, description: string, tone: string, icon: keyof typeof ICONS }>} */
export const EVENT_LABELS = {
  kill_switch: {
    label: "Kill Switch",
    description: "Emergency trading halt or flatten",
    tone: "danger",
    icon: "kill_switch",
  },
  trade_blocked: {
    label: "Trade Blocked",
    description: "Signal rejected by risk rules",
    tone: "warn",
    icon: "trade_blocked",
  },
  position_opened: {
    label: "Position Opened",
    description: "New trade entered",
    tone: "success",
    icon: "position_opened",
  },
  position_closed: {
    label: "Position Closed",
    description: "Trade exited or flattened",
    tone: "neutral",
    icon: "position_closed",
  },
  tp_hit: {
    label: "Take Profit Hit",
    description: "Partial or full profit target reached",
    tone: "success",
    icon: "tp_hit",
  },
  breakeven: {
    label: "Breakeven Stop",
    description: "Stop moved to entry after profit",
    tone: "info",
    icon: "breakeven",
  },
  cut_loss: {
    label: "Cut Loss",
    description: "Stop-loss or forced loss exit",
    tone: "danger",
    icon: "cut_loss",
  },
  live_order: {
    label: "Live Order Filled",
    description: "Order executed on MEXC",
    tone: "success",
    icon: "live_order",
  },
  live_order_error: {
    label: "Live Order Error",
    description: "Exchange order rejected or failed",
    tone: "danger",
    icon: "live_order_error",
  },
  live_order_dry_run: {
    label: "Dry Run Order",
    description: "Simulated order (not sent to exchange)",
    tone: "info",
    icon: "live_order_dry_run",
  },
  position_rollback: {
    label: "Position Rollback",
    description: "Phantom position removed after failed fill",
    tone: "warn",
    icon: "position_rollback",
  },
  exchange_position_imported: {
    label: "Exchange Import",
    description: "Position synced from MEXC",
    tone: "info",
    icon: "exchange",
  },
  exchange_position_linked: {
    label: "Exchange Linked",
    description: "Bot position linked to MEXC id",
    tone: "info",
    icon: "exchange",
  },
  exchange_position_closed: {
    label: "Exchange Closed",
    description: "Position closed on MEXC (sync)",
    tone: "neutral",
    icon: "exchange",
  },
  scanner: {
    label: "Scanner",
    description: "Market scanner lifecycle",
    tone: "info",
    icon: "scanner",
  },
  signal: {
    label: "Signal",
    description: "New confluence setup detected",
    tone: "accent",
    icon: "signal",
  },
  scan: {
    label: "Scan",
    description: "Symbol scan activity",
    tone: "neutral",
    icon: "scan",
  },
  strategy_tune: {
    label: "Strategy Tune",
    description: "Parameters adjusted from performance",
    tone: "info",
    icon: "strategy",
  },
  strategy_optimize: {
    label: "Strategy Optimize",
    description: "Optimization run completed",
    tone: "info",
    icon: "strategy",
  },
  model_learn: {
    label: "Model Learned",
    description: "Online model updated from outcomes",
    tone: "accent",
    icon: "model_learn",
  },
  shadow_signal_saved: {
    label: "Shadow Saved",
    description: "Setup saved for training-only learning",
    tone: "info",
    icon: "shadow",
  },
  shadow_signal_resolved: {
    label: "Shadow Resolved",
    description: "Shadow setup labeled and fed to model",
    tone: "accent",
    icon: "shadow",
  },
};

const DEFAULT_META = {
  label: "Event",
  description: "Bot activity",
  tone: "neutral",
  icon: "default",
};

export function eventMeta(eventType) {
  const key = (eventType || "").toLowerCase();
  return EVENT_LABELS[key] || DEFAULT_META;
}

export function eventIconHtml(eventType) {
  const meta = eventMeta(eventType);
  return ICONS[meta.icon] || ICONS.default;
}

export function formatEventLabel(eventType) {
  return eventMeta(eventType).label;
}

export function formatEventDescription(eventType) {
  return eventMeta(eventType).description;
}
