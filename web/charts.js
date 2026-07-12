/** TradingView widget + Lightweight Charts with trade setup overlay. */

import { fmtManilaDateTime } from "./time.js";

const TV_INTERVAL = {
  Min1: "1",
  Min5: "5",
  Min15: "15",
  Min30: "30",
  Min60: "60",
  Hour4: "240",
  Day1: "D",
};

const CHART_THEME = {
  bg: "#111620",
  text: "#94a3b8",
  grid: "rgba(148,163,184,0.06)",
  up: "#34d399",
  down: "#f87171",
};

const LINE_STYLES = {
  0: 0,
  2: 2,
  3: 3,
};

const chartInstances = new Map();

function num(v) {
  const n = Number(v);
  return Number.isFinite(n) ? n : 0;
}

// MEXC kline timestamps are UNIX seconds; tolerate millisecond payloads too.
function barTime(ts) {
  const n = num(ts);
  return n > 1e12 ? Math.floor(n / 1000) : Math.floor(n);
}

function resolveTrade(chartData) {
  if (chartData.trade) {
    const trade = chartData.trade;
    const pos = trade.position || chartData.position || {};
    return {
      ...trade,
      take_profits: trade.take_profits || [],
      signal_time: trade.signal_time || pos.opened_at,
      has_position: trade.has_position ?? !!pos.id,
      position: pos.id ? pos : trade.position,
    };
  }
  const signal = chartData.signal || {};
  const pos = chartData.position || {};
  const storedTps = (pos.take_profit_levels || [])
    .filter((tp) => num(tp.price) > 0)
    .map((tp, i) => ({
      price: num(tp.price),
      close_fraction: num(tp.close_fraction),
      close_pct: Math.round(num(tp.close_fraction) * 1000) / 10,
      label: tp.label || `TP${i + 1}`,
    }));
  return {
    side: pos.side || signal.side || (num(signal.price_change_pct) < 0 ? "short" : "long"),
    entry_price: num(pos.entry_price || signal.last_price),
    stop_loss: num(pos.stop_loss || signal.projected_stop_loss),
    take_profits:
      storedTps.length > 0
        ? storedTps
        : (signal.projected_take_profits || []).map((price, i) => ({
            price: num(price),
            close_fraction: num((signal.tp_close_fractions || [])[i]),
            close_pct: Math.round(num((signal.tp_close_fractions || [])[i]) * 1000) / 10,
            label: `TP${i + 1}`,
          })),
    leverage: pos.leverage ?? signal.suggested_leverage,
    strategy: signal.strategy || pos.strategy,
    composite_score: signal.composite_score,
    setup_probability_pct: signal.setup_probability_pct,
    zone_message: signal.zone_message,
    confluences: signal.confluences,
    mark_price: num(pos.mark_price),
    unrealized_pnl: num(pos.unrealized_pnl),
    unrealized_pnl_pct: num(pos.unrealized_pnl_pct),
    unrealized_roi_pct: num(pos.unrealized_roi_pct),
    signal_time: pos.opened_at || signal.generated_at || signal.created_at,
    has_position: !!pos.id,
    is_closed: (pos.status || "").toLowerCase() === "closed",
    position: pos.id ? pos : null,
  };
}

function buildTradeLines(chartData) {
  const trade = resolveTrade(chartData);
  const lines = [];
  const entry = num(trade.entry_price);
  const sl = num(trade.stop_loss);
  const side = (trade.side || "long").toLowerCase();
  const pos = trade.position || {};
  const hideZones = chartData.overlay_options?.hide_zones === true;

  if (entry > 0) {
    const placed = trade.has_position;
    lines.push({
      price: entry,
      color: "#0ea5e9",
      title: placed ? `Entry · ${side}` : `Setup entry · ${side}`,
      lineWidth: 2,
      lineStyle: 0,
    });
  }

  const mark = num(pos.mark_price ?? trade.mark_price);
  if (mark > 0 && Math.abs(mark - entry) > 1e-12) {
    lines.push({
      price: mark,
      color: "#a78bfa",
      title: "Mark",
      lineWidth: 1,
      lineStyle: 2,
    });
  }

  if (sl > 0) {
    lines.push({
      price: sl,
      color: "#f87171",
      title: "Stop loss",
      lineWidth: 2,
      lineStyle: 2,
    });
  }

  (trade.take_profits || []).forEach((tp) => {
    const price = num(tp.price);
    if (price <= 0) return;
    const pct = tp.close_pct != null ? Math.round(tp.close_pct) : Math.round(num(tp.close_fraction) * 100);
    lines.push({
      price,
      color: "#34d399",
      title: `${tp.label || "TP"} ${pct}%`,
      lineWidth: 1,
      lineStyle: 2,
    });
  });

  const exit = num(trade.exit_price);
  const showExitLine = (trade.is_closed || trade.is_resolved) && exit > 0;
  if (showExitLine) {
    const win =
      trade.is_closed
        ? num(trade.realized_pnl) >= 0
        : trade.outcome === "win";
    const reason = (trade.exit_reason || trade.outcome || "exit").replace(/_/g, " ");
    lines.push({
      price: exit,
      color: win ? "#22c55e" : "#ef4444",
      title: `Exit · ${reason}`,
      lineWidth: 2,
      lineStyle: 0,
    });
  }

  if (!hideZones) {
    (chartData.zones || []).forEach((zone) => {
      const kind = zone.kind || "demand";
      const color = kind === "demand" ? "rgba(52, 211, 153, 0.7)" : "rgba(248, 113, 113, 0.7)";
      const label = kind === "demand" ? "Demand" : "Supply";
      const lo = num(zone.low);
      const hi = num(zone.high);
      if (lo > 0) {
        lines.push({ price: lo, color, title: `${label} low`, lineWidth: 1, lineStyle: 3 });
      }
      if (hi > 0) {
        lines.push({ price: hi, color, title: `${label} high`, lineWidth: 1, lineStyle: 3 });
      }
    });
  }

  return { lines, trade, side };
}

function exitBarTime(bars, trade) {
  if (trade.closed_at) return nearestBarTime(bars, trade.closed_at);
  if (trade.exit_timestamp) return barTime(trade.exit_timestamp);
  return null;
}

function hasExitMarker(trade) {
  return (trade.is_closed || trade.is_resolved) && num(trade.exit_price) > 0;
}

function buildTradeMarkers(bars, trade, side) {
  const markers = [];
  const entryPrice = num(trade.entry_price);
  const entryTime = nearestBarTime(bars, trade.signal_time);

  if (entryPrice > 0) {
    markers.push({
      time: entryTime,
      position: side === "short" ? "aboveBar" : "belowBar",
      color: trade.has_position ? "#0ea5e9" : "#fbbf24",
      shape: trade.has_position ? "arrowUp" : "circle",
      text: trade.has_position ? "ENTRY" : "SETUP",
    });
  }

  if (hasExitMarker(trade)) {
    const exitTime = exitBarTime(bars, trade);
    if (exitTime) {
      const win =
        trade.is_closed
          ? num(trade.realized_pnl) >= 0
          : trade.outcome === "win";
      let label = "EXIT";
      if (trade.is_closed) {
        const pnlPct = trade.roi_pct != null ? num(trade.roi_pct) : num(trade.realized_pnl_pct);
        const sign = pnlPct >= 0 ? "+" : "";
        label = `EXIT ${sign}${pnlPct.toFixed(1)}%`;
      } else {
        label = `EXIT · ${(trade.outcome || trade.exit_reason || "closed").toUpperCase()}`;
      }
      markers.push({
        time: exitTime,
        position: side === "short" ? "belowBar" : "aboveBar",
        color: win ? "#22c55e" : "#ef4444",
        shape: side === "short" ? "arrowUp" : "arrowDown",
        text: label,
      });
    }
  }

  markers.sort((a, b) => a.time - b.time);
  return markers;
}

function barIntervalSec(bars) {
  if (bars.length < 2) return 60;
  const d = barTime(bars[1].timestamp) - barTime(bars[0].timestamp);
  return d > 0 ? d : 60;
}

function focusTradeViewport(chart, bars, trade) {
  const entryTime = nearestBarTime(bars, trade.signal_time);
  const exitTime = exitBarTime(bars, trade);
  if (!exitTime) {
    chart.timeScale().fitContent();
    return null;
  }
  const lastTime = barTime(bars[bars.length - 1]?.timestamp);
  const pad = barIntervalSec(bars) * 10;
  const from = Math.min(entryTime, exitTime) - pad;
  const to = Math.max(exitTime, lastTime) + pad;
  try {
    chart.timeScale().setVisibleRange({ from, to });
    return { from, to };
  } catch {
    chart.timeScale().fitContent();
    return null;
  }
}

function applyTradeMarkers(candleSeries, bars, trade, side) {
  const markers = buildTradeMarkers(bars, trade, side);
  if (!markers.length) return;
  try {
    candleSeries.setMarkers(markers);
  } catch {
    /* markers optional if chart library API differs */
  }
}

function nearestBarTime(bars, isoTime) {
  if (!bars?.length || !isoTime) {
    return barTime(bars?.[bars.length - 1]?.timestamp);
  }
  const target = Math.floor(new Date(isoTime).getTime() / 1000);
  if (!Number.isFinite(target)) {
    return barTime(bars[bars.length - 1].timestamp);
  }
  let best = barTime(bars[0].timestamp);
  let bestDiff = Math.abs(best - target);
  for (const b of bars) {
    const t = barTime(b.timestamp);
    const diff = Math.abs(t - target);
    if (diff < bestDiff) {
      best = t;
      bestDiff = diff;
    }
  }
  return best;
}

function seriesToLineData(points) {
  if (!Array.isArray(points)) return [];
  const dedup = new Map();
  points.forEach((p) => {
    const t = barTime(p.timestamp);
    const v = num(p.value);
    if (Number.isFinite(t) && Number.isFinite(v)) dedup.set(t, v);
  });
  return [...dedup.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([time, value]) => ({ time, value }));
}

function applyTaOverlays(chart, chartData) {
  const ta = chartData.ta || {};
  const series = ta.series || {};
  const overlays = {};

  chart.priceScale("right").applyOptions({
    scaleMargins: { top: 0.08, bottom: 0.28 },
  });

  const emaDefs = [
    { key: "ema20", color: "#fbbf24", title: "EMA20" },
    { key: "ema50", color: "#38bdf8", title: "EMA50" },
    { key: "ema200", color: "#c084fc", title: "EMA200" },
  ];
  emaDefs.forEach(({ key, color, title }) => {
    const data = seriesToLineData(series[key]);
    if (!data.length) return;
    const line = chart.addLineSeries({
      color,
      lineWidth: 1,
      title,
      priceLineVisible: false,
      lastValueVisible: true,
      crosshairMarkerVisible: false,
    });
    line.setData(data);
    overlays[key] = line;
  });

  const rsiData = seriesToLineData(series.rsi);
  if (rsiData.length) {
    const rsi = chart.addLineSeries({
      color: "#fb7185",
      lineWidth: 1,
      title: "RSI",
      priceScaleId: "rsi",
      priceLineVisible: false,
      lastValueVisible: true,
    });
    chart.priceScale("rsi").applyOptions({
      scaleMargins: { top: 0.78, bottom: 0.02 },
      borderVisible: false,
    });
    rsi.setData(rsiData);
    rsi.createPriceLine({
      price: 70,
      color: "rgba(248,113,113,0.45)",
      lineWidth: 1,
      lineStyle: 2,
      axisLabelVisible: false,
      title: "",
    });
    rsi.createPriceLine({
      price: 30,
      color: "rgba(52,211,153,0.45)",
      lineWidth: 1,
      lineStyle: 2,
      axisLabelVisible: false,
      title: "",
    });
    overlays.rsi = rsi;
  }

  const macdHist = seriesToLineData(series.macd_hist);
  if (macdHist.length) {
    const hist = chart.addHistogramSeries({
      priceScaleId: "macd",
      priceFormat: { type: "price", precision: 6, minMove: 0.000001 },
      lastValueVisible: false,
    });
    chart.priceScale("macd").applyOptions({
      scaleMargins: { top: 0.62, bottom: 0.22 },
      borderVisible: false,
    });
    hist.setData(
      macdHist.map((p) => ({
        time: p.time,
        value: p.value,
        color: p.value >= 0 ? "rgba(52,211,153,0.55)" : "rgba(248,113,113,0.55)",
      }))
    );
    overlays.macd_hist = hist;
  }

  // Keep volume tucked under candles, above RSI.
  chart.priceScale("vol").applyOptions({
    scaleMargins: { top: 0.72, bottom: 0.14 },
  });

  return overlays;
}

export function renderSignalSetupLegend(containerId, chartData) {
  const el = document.getElementById(containerId);
  if (!el) return;
  const trade = resolveTrade(chartData);
  const pos = trade.position || {};
  const tps = (trade.take_profits || [])
    .filter((tp) => num(tp.price) > 0)
    .map((tp) => {
      const pct = tp.close_pct != null ? tp.close_pct : Math.round(num(tp.close_fraction) * 100);
      return `<span class="setup-chip setup-tp">${tp.label || "TP"} @ ${num(tp.price).toPrecision(6)} <em>${pct}%</em></span>`;
    })
    .join("");

  const conf = (trade.confluences || []).slice(0, 6).map((c) => `<span class="setup-chip">${c}</span>`).join("");

  let outcomeCells = "";
  if (trade.is_closed || trade.is_resolved) {
    const pnl = num(trade.realized_pnl);
    const win = trade.is_closed ? pnl >= 0 : trade.outcome === "win";
    const sign = pnl >= 0 ? "+" : "";
    const roi = trade.roi_pct != null ? num(trade.roi_pct) : num(trade.realized_pnl_pct);
    const cls = win ? "setup-pnl-win" : "setup-pnl-loss";
    const reason = (trade.exit_reason || trade.outcome || "exit").replace(/_/g, " ");
    const exitLabel = trade.is_closed && pnl !== 0
      ? `<div><span class="hint">Realized PnL</span><strong class="mono ${cls}">${sign}$${Math.abs(pnl).toFixed(2)}</strong></div>
         <div><span class="hint">ROI</span><strong class="mono ${cls}">${sign}${roi.toFixed(2)}%</strong></div>`
      : "";
    outcomeCells = `
      <div><span class="hint">Exit</span><strong class="mono">${num(trade.exit_price).toPrecision(6)}</strong></div>
      ${exitLabel}
      <div><span class="hint">Result</span><strong class="setup-outcome ${cls}">${(trade.outcome || (win ? "win" : "loss")).toUpperCase()} · ${reason}</strong></div>
    `;
  } else if (trade.has_position) {
    const mark = num(trade.mark_price ?? pos.mark_price);
    const upnl = num(trade.unrealized_pnl ?? pos.unrealized_pnl);
    const roi = num(trade.unrealized_roi_pct ?? pos.unrealized_roi_pct);
    const movePct = num(trade.unrealized_pnl_pct ?? pos.unrealized_pnl_pct);
    const cls = upnl >= 0 ? "setup-pnl-win" : "setup-pnl-loss";
    const sign = upnl >= 0 ? "+" : "";
    outcomeCells = `
      <div><span class="hint">Mark</span><strong class="mono">${mark > 0 ? mark.toPrecision(6) : "—"}</strong></div>
      <div><span class="hint">Unrealized PnL</span><strong class="mono ${cls}">${sign}$${Math.abs(upnl).toFixed(2)}</strong></div>
      <div><span class="hint">Move</span><strong class="mono ${cls}">${sign}${movePct.toFixed(2)}%</strong></div>
      <div><span class="hint">ROI</span><strong class="mono ${cls}">${sign}${roi.toFixed(2)}%</strong></div>
    `;
  }

  const snap = chartData.ta?.snapshot || {};
  const reasons = (chartData.ta?.reasons || []).slice(0, 8);
  const reasonList = reasons.length
    ? `<ul class="ta-reason-list">${reasons.map((r) => `<li>${r}</li>`).join("")}</ul>`
    : "";
  const snapRow = snap.rsi != null
    ? `<div class="ta-snap-row">
        <span class="setup-chip">RSI ${num(snap.rsi).toFixed(0)}</span>
        <span class="setup-chip">ADX ${num(snap.adx).toFixed(0)}</span>
        <span class="setup-chip">ATR ${num(snap.atr_pct).toFixed(2)}%</span>
        <span class="setup-chip">Vol ${num(snap.volume_ma_ratio).toFixed(1)}×</span>
        <span class="setup-chip ta-legend-ema20">EMA20</span>
        <span class="setup-chip ta-legend-ema50">EMA50</span>
        <span class="setup-chip ta-legend-ema200">EMA200</span>
      </div>`
    : "";

  el.innerHTML = `
    <div class="setup-legend-grid">
      <div><span class="hint">Side</span><strong class="setup-side-${trade.side}">${(trade.side || "—").toUpperCase()}</strong></div>
      <div><span class="hint">Entry</span><strong class="mono">${num(trade.entry_price).toPrecision(6)}</strong></div>
      <div><span class="hint">Stop</span><strong class="mono setup-sl">${num(trade.stop_loss).toPrecision(6)}</strong></div>
      <div><span class="hint">Leverage</span><strong class="mono">${trade.leverage != null ? `${trade.leverage}×` : "—"}</strong></div>
      <div><span class="hint">Score</span><strong class="mono">${trade.composite_score != null ? Number(trade.composite_score).toFixed(1) : "—"}</strong></div>
      <div><span class="hint">ML</span><strong class="mono">${trade.setup_probability_pct != null ? `${trade.setup_probability_pct}%` : "—"}</strong></div>
      ${outcomeCells}
    </div>
    ${tps ? `<div class="setup-tp-row">${tps}</div>` : ""}
    ${conf ? `<div class="setup-conf-row">${conf}</div>` : ""}
    <div class="ta-analysis">
      <div class="ta-analysis-title">Why this trade (technical analysis)</div>
      ${snapRow}
      ${reasonList || `<p class="hint">No TA summary available for this chart window.</p>`}
    </div>
    ${trade.zone_message ? `<p class="hint setup-zone-msg">${trade.zone_message}</p>` : ""}
    ${
      trade.has_position
        ? `<p class="hint setup-pos-note">Position #${pos.id} · ${pos.status || "open"}${
            trade.is_closed && trade.closed_at ? ` · closed ${fmtManilaDateTime(trade.closed_at)}` : ` · size ${num(pos.remaining_size ?? pos.size).toFixed(4)}`
          }</p>`
        : `<p class="hint setup-pos-note">Projected setup levels — no linked position yet</p>`
    }
    ${
      trade.is_resolved && !trade.has_position
        ? `<p class="hint setup-pos-note">Resolved from price action · ${trade.closed_at ? fmtManilaDateTime(trade.closed_at) : "—"}</p>`
        : ""
    }
  `;
}

export function renderTradingViewWidget(containerId, tvSymbol, interval = "15") {
  const el = document.getElementById(containerId);
  if (!el) return;
  el.innerHTML = "";
  const innerId = `${containerId}-inner`;
  el.innerHTML = `<div id="${innerId}" style="height:100%;width:100%"></div>`;
  if (typeof TradingView === "undefined") {
    el.innerHTML = '<p class="empty">TradingView unavailable — check network</p>';
    return;
  }
  const tvInterval = TV_INTERVAL[interval] || interval || "15";
  // eslint-disable-next-line no-new
  new TradingView.widget({
    autosize: true,
    symbol: tvSymbol,
    interval: tvInterval,
    timezone: "Etc/UTC",
    theme: "dark",
    style: "1",
    locale: "en",
    enable_publishing: false,
    allow_symbol_change: true,
    container_id: innerId,
    studies: ["Volume@tv-basicstudies"],
    backgroundColor: CHART_THEME.bg,
    gridColor: CHART_THEME.grid,
  });
}

export function renderLightweightChart(containerId, chartData) {
  const el = document.getElementById(containerId);
  if (!el) return;
  if (typeof LightweightCharts === "undefined") {
    el.innerHTML = '<p class="empty">Chart library failed to load — check network</p>';
    return;
  }

  const draw = () => {
    if (chartInstances.has(containerId)) {
      try {
        chartInstances.get(containerId).chart.remove();
      } catch {
        /* ignore */
      }
      chartInstances.delete(containerId);
    }
    el.innerHTML = "";

    const bars = chartData.bars || [];
    if (!bars.length) {
      el.innerHTML = '<p class="empty">No kline data for this symbol</p>';
      return;
    }

    const width = el.clientWidth || el.parentElement?.clientWidth || 640;
    const height = el.clientHeight || 300;

    const chart = LightweightCharts.createChart(el, {
      layout: { background: { color: CHART_THEME.bg }, textColor: CHART_THEME.text },
      grid: {
        vertLines: { color: CHART_THEME.grid },
        horzLines: { color: CHART_THEME.grid },
      },
      width,
      height,
      timeScale: { timeVisible: true, secondsVisible: false, borderColor: CHART_THEME.grid },
      rightPriceScale: { borderColor: CHART_THEME.grid },
      crosshair: { mode: LightweightCharts.CrosshairMode.Normal },
    });

    const candleSeries = chart.addCandlestickSeries({
      upColor: CHART_THEME.up,
      downColor: CHART_THEME.down,
      borderVisible: false,
      wickUpColor: CHART_THEME.up,
      wickDownColor: CHART_THEME.down,
    });

    const dedup = new Map();
    bars.forEach((b) => {
      dedup.set(barTime(b.timestamp), b);
    });
    const sortedBars = [...dedup.entries()].sort((a, b) => a[0] - b[0]);

    const candles = sortedBars.map(([time, b]) => ({
      time,
      open: num(b.open),
      high: num(b.high),
      low: num(b.low),
      close: num(b.close),
    }));
    candleSeries.setData(candles);

    const volSeries = chart.addHistogramSeries({
      color: "rgba(34, 211, 238, 0.35)",
      priceFormat: { type: "volume" },
      priceScaleId: "vol",
    });
    chart.priceScale("vol").applyOptions({ scaleMargins: { top: 0.82, bottom: 0 } });

    volSeries.setData(
      sortedBars.map(([time, b]) => ({
        time,
        value: num(b.volume || b.amount),
        color:
          num(b.close) >= num(b.open)
            ? "rgba(52, 211, 153, 0.35)"
            : "rgba(248, 113, 113, 0.35)",
      }))
    );

    const taOverlays = applyTaOverlays(chart, chartData);

    const { lines, trade, side } = buildTradeLines(chartData);
    chartInstances.set(containerId, {
      chart,
      candleSeries,
      volSeries,
      taOverlays,
      trade,
      side,
      visibleRange: null,
    });
    lines.forEach((line) => {
      candleSeries.createPriceLine({
        price: line.price,
        color: line.color,
        lineWidth: line.lineWidth ?? 2,
        lineStyle: LINE_STYLES[line.lineStyle ?? 0] ?? 0,
        axisLabelVisible: true,
        title: line.title,
      });
    });

    applyTradeMarkers(candleSeries, bars, trade, side);

    const visibleRange = hasExitMarker(trade)
      ? focusTradeViewport(chart, bars, trade)
      : (chart.timeScale().fitContent(), null);
    const inst = chartInstances.get(containerId);
    if (inst) inst.visibleRange = visibleRange;

    const ro = new ResizeObserver(() => {
      chart.applyOptions({ width: el.clientWidth || width, height: el.clientHeight || height });
    });
    ro.observe(el);
  };

  // Defer until the modal has layout (avoids 0-width charts).
  requestAnimationFrame(() => requestAnimationFrame(draw));
}

// Refresh an already-rendered lightweight chart with fresh klines without
// recreating it — preserves the user's current zoom/scroll. Updates the latest
// (and any new) candles + volume in place.
export function updateLightweightChartBars(containerId, chartData) {
  const inst = chartInstances.get(containerId);
  if (!inst) return false;
  const bars = chartData.bars || [];
  if (!bars.length) return false;

  const dedup = new Map();
  bars.forEach((b) => dedup.set(barTime(b.timestamp), b));
  const sortedBars = [...dedup.entries()].sort((a, b) => a[0] - b[0]);

  inst.candleSeries.setData(
    sortedBars.map(([time, b]) => ({
      time,
      open: num(b.open),
      high: num(b.high),
      low: num(b.low),
      close: num(b.close),
    }))
  );
  inst.volSeries.setData(
    sortedBars.map(([time, b]) => ({
      time,
      value: num(b.volume || b.amount),
      color:
        num(b.close) >= num(b.open)
          ? "rgba(52, 211, 153, 0.35)"
          : "rgba(248, 113, 113, 0.35)",
    }))
  );

  // Refresh TA series in place when live chart polls.
  const series = chartData.ta?.series || {};
  const overlays = inst.taOverlays || {};
  if (overlays.ema20) overlays.ema20.setData(seriesToLineData(series.ema20));
  if (overlays.ema50) overlays.ema50.setData(seriesToLineData(series.ema50));
  if (overlays.ema200) overlays.ema200.setData(seriesToLineData(series.ema200));
  if (overlays.rsi) overlays.rsi.setData(seriesToLineData(series.rsi));
  if (overlays.macd_hist) {
    const macdHist = seriesToLineData(series.macd_hist);
    overlays.macd_hist.setData(
      macdHist.map((p) => ({
        time: p.time,
        value: p.value,
        color: p.value >= 0 ? "rgba(52,211,153,0.55)" : "rgba(248,113,113,0.55)",
      }))
    );
  }

  const trade = resolveTrade(chartData);
  const side = (trade.side || "long").toLowerCase();
  inst.trade = trade;
  inst.side = side;
  applyTradeMarkers(inst.candleSeries, bars, trade, side);

  if (hasExitMarker(trade)) {
    inst.visibleRange = focusTradeViewport(inst.chart, bars, trade);
  } else if (inst.visibleRange) {
    try {
      inst.chart.timeScale().setVisibleRange(inst.visibleRange);
    } catch {
      /* keep current zoom */
    }
  }

  return true;
}

export function initChartTabs(tabsId, tvPaneId, lwcPaneId) {
  const tabs = document.getElementById(tabsId);
  if (!tabs) return;
  const tvPane = document.getElementById(tvPaneId);
  const lwcPane = document.getElementById(lwcPaneId);

  tabs.querySelectorAll(".seg").forEach((btn) => {
    btn.addEventListener("click", () => {
      tabs.querySelectorAll(".seg").forEach((b) => b.classList.remove("active"));
      btn.classList.add("active");
      const mode = btn.dataset.chart;
      tvPane?.classList.toggle("active", mode === "tv");
      lwcPane?.classList.toggle("active", mode === "lwc");
    });
  });
}

export function activateChartTab(tabsId, mode) {
  const tabs = document.getElementById(tabsId);
  if (!tabs) return;
  const btn = tabs.querySelector(`.seg[data-chart="${mode}"]`);
  btn?.click();
}

export async function loadSignalChart(apiGet, symbol, generatedAt) {
  const q = new URLSearchParams({ symbol, bars: "120" });
  if (generatedAt) q.set("generated_at", generatedAt);
  return apiGet(`/signals/chart?${q}`);
}

export async function loadPositionChart(apiGet, positionId) {
  return apiGet(`/positions/${positionId}/chart?bars=120`);
}
