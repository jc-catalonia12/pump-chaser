//! Reconcile local open positions with MEXC Futures — port of `position_sync.py`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{json, Value};
use tracing::info;

use crate::config::AppConfig;
use crate::db::Database;
use crate::execution::cleanup_after_position_closed;
use crate::exchange::rest::MexcRestClient;
use crate::exchange::types::ContractInfo;
use crate::exchange::MexcPrivateClient;
use crate::models::PositionSide;

fn side_to_str(side: PositionSide) -> &'static str {
    match side {
        PositionSide::Long => "long",
        PositionSide::Short => "short",
    }
}

fn default_stop_loss(entry: f64, side: PositionSide, sl_pct: f64) -> f64 {
    match side {
        PositionSide::Long => entry * (1.0 - sl_pct),
        PositionSide::Short => entry * (1.0 + sl_pct),
    }
}

/// Parsed open position row from the MEXC API.
struct ExchangeOpenPosition {
    exchange_id: i64,
    symbol: String,
    side: PositionSide,
    hold_vol: f64,
    entry: f64,
    leverage: i32,
    /// `stopLossPrice` from MEXC (0.0 = not set).
    stop_loss_price: f64,
    /// `takeProfitPrice` from MEXC (0.0 = not set).
    take_profit_price: f64,
}

fn parse_f64_field(raw: &Value, key: &str) -> f64 {
    raw.get(key)
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0.0)
}

fn parse_exchange_position(raw: &Value) -> Option<ExchangeOpenPosition> {
    if raw.get("state").and_then(|v| v.as_i64()) != Some(1) {
        return None;
    }
    let hold_vol = raw.get("holdVol").and_then(|v| v.as_f64()).unwrap_or(0.0);
    if hold_vol <= 0.0 {
        return None;
    }
    let exchange_id = raw.get("positionId").and_then(|v| v.as_i64())?;
    let symbol = raw.get("symbol").and_then(|v| v.as_str())?.to_string();
    let side = if raw.get("positionType").and_then(|v| v.as_i64()) == Some(1) {
        PositionSide::Long
    } else {
        PositionSide::Short
    };
    let entry = raw
        .get("holdAvgPrice")
        .or_else(|| raw.get("openAvgPrice"))
        .and_then(|v| v.as_f64())
        .filter(|&e| e > 0.0)?;
    let leverage = raw
        .get("leverage")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0) as i32;
    // MEXC returns these as strings or numbers; 0 / "0" means not set.
    let stop_loss_price = parse_f64_field(raw, "stopLossPrice");
    let take_profit_price = parse_f64_field(raw, "takeProfitPrice");
    Some(ExchangeOpenPosition {
        exchange_id,
        symbol,
        side,
        hold_vol,
        entry,
        leverage,
        stop_loss_price,
        take_profit_price,
    })
}

fn pricing_for_symbol(
    symbol: &str,
    contracts: Option<&HashMap<String, ContractInfo>>,
    marks: &HashMap<String, f64>,
) -> (f64, f64, f64) {
    let (cs, fee) = if let Some(map) = contracts {
        let cs = map
            .get(symbol)
            .map(|c| c.contract_size)
            .filter(|&s| s > 0.0)
            .unwrap_or(1.0);
        let fee = map
            .get(symbol)
            .map(|c| c.taker_fee_rate)
            .filter(|&r| r > 0.0)
            .unwrap_or(0.0006);
        (cs, fee)
    } else {
        (1.0, 0.0006)
    };
    let mark = marks.get(symbol).copied().unwrap_or(0.0);
    (cs, fee, mark)
}

pub async fn fetch_mark_prices(config: &AppConfig) -> HashMap<String, f64> {
    let mexc = Arc::new(config.mexc.clone());
    let Ok(rest) = MexcRestClient::new(mexc) else {
        return HashMap::new();
    };
    match rest.get_tickers().await {
        Ok(tickers) => tickers
            .into_iter()
            .filter_map(|t| {
                let price = if t.fair_price > 0.0 {
                    t.fair_price
                } else {
                    t.last_price
                };
                if price > 0.0 {
                    Some((t.symbol, price))
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => HashMap::new(),
    }
}

async fn close_missing_position(
    client: &MexcPrivateClient,
    db: &Database,
    pos: &Value,
    reason: &str,
    contract_size: f64,
    fee_rate: f64,
    mark_price: f64,
    audit_message: &str,
    audit_extra: Value,
) -> Option<String> {
    let id = pos.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    if id == 0 {
        return None;
    }
    let symbol = pos
        .get("symbol")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let side = pos
        .get("side")
        .and_then(|v| v.as_str())
        .unwrap_or("long")
        .to_string();
    let exchange_pos_id = pos.get("exchange_position_id").and_then(|v| v.as_i64());

    match db
        .close_position_synced(id, reason, contract_size, fee_rate, mark_price)
        .await
    {
        Ok((pnl, exit_price)) => {
            let mut payload = audit_extra;
            if let Value::Object(ref mut m) = payload {
                m.insert("position_id".into(), json!(id));
                m.insert("symbol".into(), json!(symbol));
                m.insert("side".into(), json!(side));
                m.insert("pnl".into(), json!(pnl));
                m.insert("exit_price".into(), json!(exit_price));
                m.insert("reason".into(), json!(reason));
                if let Some(strategy) = pos.get("strategy") {
                    m.insert("strategy".into(), strategy.clone());
                }
            }
            let _ = db
                .log_event("exchange_position_closed", audit_message, Some(payload.clone()))
                .await;
            let _ = db
                .log_event(
                    "position_closed",
                    &format!("Closed {symbol} ({reason})"),
                    Some(payload),
                )
                .await;
            cleanup_after_position_closed(client, &symbol, exchange_pos_id).await;
            Some(symbol)
        }
        Err(exc) => {
            tracing::warn!("Failed to close missing position {id} ({symbol}): {exc}");
            None
        }
    }
}

pub async fn sync_exchange_positions(
    client: &MexcPrivateClient,
    db: &Database,
    config: &AppConfig,
    contracts: Option<&HashMap<String, ContractInfo>>,
) -> Value {
    if !client.has_credentials() {
        return json!({
            "error": "MEXC API credentials not configured",
            "synced": 0,
            "exchange_count": 0,
        });
    }

    let raw_positions = match client.get_open_positions().await {
        Ok(p) => p,
        Err(exc) => {
            return json!({
                "error": exc.to_string(),
                "synced": 0,
                "exchange_count": 0,
            });
        }
    };

    let mut seen_ids: HashSet<i64> = HashSet::new();
    let mut seen_symbol_side: HashSet<(String, String)> = HashSet::new();
    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut linked = 0usize;
    let mut deduped = 0usize;

    let sl_pct = config.risk.default_sl_pct;
    let default_leverage = config.risk.max_leverage as i32;
    let marks = fetch_mark_prices(config).await;

    for raw in &raw_positions {
        let Some(ep) = parse_exchange_position(raw) else {
            continue;
        };
        seen_ids.insert(ep.exchange_id);
        seen_symbol_side.insert((ep.symbol.clone(), side_to_str(ep.side).into()));

        if let Ok(Some(existing)) = db.get_open_position_by_exchange_id(ep.exchange_id).await {
            let id = existing.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            let lev = if ep.leverage > 0 {
                ep.leverage
            } else {
                existing
                    .get("leverage")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(default_leverage as i64) as i32
            };
            let _ = db
                .update_position_after_exchange(id, ep.hold_vol, ep.entry, Some(ep.exchange_id), Some(lev))
                .await;
            // Update SL/TP from exchange values when they are set on MEXC.
            let sl_opt = if ep.stop_loss_price > 0.0 { Some(ep.stop_loss_price) } else { None };
            let tp_opt = if ep.take_profit_price > 0.0 {
                serde_json::to_string(&serde_json::json!([{
                    "level": 1,
                    "price": ep.take_profit_price,
                    "close_fraction": 1.0,
                }]))
                .ok()
            } else {
                None
            };
            if sl_opt.is_some() || tp_opt.is_some() {
                let _ = db.update_position_sl_tp(id, sl_opt, tp_opt.as_deref()).await;
            }
            updated += 1;
            continue;
        }

        if let Ok(Some(bot_pos)) = db
            .get_open_position_by_symbol_side(&ep.symbol, side_to_str(ep.side), Some("bot"))
            .await
        {
            let bot_id = bot_pos.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            let has_exchange_id = bot_pos
                .get("exchange_position_id")
                .and_then(|v| v.as_i64())
                .is_some();
            if !has_exchange_id {
                let lev = if ep.leverage > 0 {
                    ep.leverage
                } else {
                    bot_pos
                        .get("leverage")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(default_leverage as i64) as i32
                };
                let _ = db
                    .update_position_after_exchange(bot_id, ep.hold_vol, ep.entry, Some(ep.exchange_id), Some(lev))
                    .await;
                let sl_opt = if ep.stop_loss_price > 0.0 { Some(ep.stop_loss_price) } else { None };
                let tp_opt = if ep.take_profit_price > 0.0 {
                    serde_json::to_string(&serde_json::json!([{
                        "level": 1,
                        "price": ep.take_profit_price,
                        "close_fraction": 1.0,
                    }]))
                    .ok()
                } else {
                    None
                };
                if sl_opt.is_some() || tp_opt.is_some() {
                    let _ = db.update_position_sl_tp(bot_id, sl_opt, tp_opt.as_deref()).await;
                }
                deduped += close_duplicate_exchange_rows(
                    db,
                    bot_id,
                    &ep.symbol,
                    side_to_str(ep.side),
                    ep.exchange_id,
                    contracts,
                    &marks,
                )
                .await;
                let _ = db
                    .log_event(
                        "exchange_position_linked",
                        &format!("Linked bot position {} {} to exchange id {}", ep.symbol, side_to_str(ep.side), ep.exchange_id),
                        Some(json!({ "position_id": bot_id, "exchange_position_id": ep.exchange_id })),
                    )
                    .await;
                info!(
                    "Linked bot position {} {} to exchange id {}",
                    ep.symbol,
                    side_to_str(ep.side),
                    ep.exchange_id
                );
                linked += 1;
                continue;
            }
        }

        // Prefer actual stop-loss from MEXC; fall back to calculated default.
        let stop_loss = if ep.stop_loss_price > 0.0 {
            ep.stop_loss_price
        } else {
            default_stop_loss(ep.entry, ep.side, sl_pct)
        };
        let leverage = if ep.leverage > 0 {
            ep.leverage
        } else {
            default_leverage
        };
        match db
            .insert_exchange_position(
                &ep.symbol,
                side_to_str(ep.side),
                ep.entry,
                ep.hold_vol,
                stop_loss,
                leverage as i64,
                ep.exchange_id,
            )
            .await
        {
            Ok(id) => {
                // Persist TP if MEXC reported one.
                if ep.take_profit_price > 0.0 {
                    let tp_json = serde_json::to_string(&serde_json::json!([{
                        "level": 1,
                        "price": ep.take_profit_price,
                        "close_fraction": 1.0,
                    }]))
                    .unwrap_or_else(|_| "[]".into());
                    let _ = db.update_position_sl_tp(id, None, Some(&tp_json)).await;
                }
                let _ = db
                    .log_event(
                        "exchange_position_imported",
                        &format!("Imported MEXC position {} {}", ep.symbol, side_to_str(ep.side)),
                        Some(json!({
                            "position_id": id,
                            "exchange_position_id": ep.exchange_id,
                            "size": ep.hold_vol,
                            "entry": ep.entry,
                            "stop_loss": stop_loss,
                            "take_profit_price": ep.take_profit_price,
                        })),
                    )
                    .await;
                info!(
                    "Imported exchange position {} {} size={}",
                    ep.symbol,
                    side_to_str(ep.side),
                    ep.hold_vol
                );
                imported += 1;
            }
            Err(e) => {
                tracing::warn!("Failed to import exchange position {}: {e}", ep.symbol);
            }
        }
    }

    deduped += dedupe_symbol_side_rows(db, &seen_symbol_side, contracts, &marks).await;

    let mut closed = 0usize;
    let mut closed_symbols: Vec<String> = Vec::new();
    let open = db.get_open_positions().await.unwrap_or_default();
    for pos in &open {
        if pos.get("paper").and_then(|v| v.as_bool()).unwrap_or(true) {
            continue;
        }
        let symbol = pos.get("symbol").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let side = pos.get("side").and_then(|v| v.as_str()).unwrap_or("long").to_string();

        if let Some(exchange_id) = pos.get("exchange_position_id").and_then(|v| v.as_i64()) {
            if !seen_ids.contains(&exchange_id) {
                let (cs, fee, mark) = pricing_for_symbol(&symbol, contracts, &marks);
                if let Some(sym) = close_missing_position(
                    client,
                    db,
                    pos,
                    "exchange_closed",
                    cs,
                    fee,
                    mark,
                    &format!("Position gone on MEXC: {symbol}"),
                    json!({ "exchange_position_id": exchange_id }),
                )
                .await
                {
                    closed += 1;
                    closed_symbols.push(sym);
                }
            }
            continue;
        }

        if !seen_symbol_side.contains(&(symbol.clone(), side.clone())) {
            let (cs, fee, mark) = pricing_for_symbol(&symbol, contracts, &marks);
            if let Some(sym) = close_missing_position(
                client,
                db,
                pos,
                "exchange_closed",
                cs,
                fee,
                mark,
                &format!("Bot position closed on MEXC: {symbol}"),
                json!({}),
            )
            .await
            {
                closed += 1;
                closed_symbols.push(sym);
            }
        }
    }

    let exchange_count = seen_ids.len();
    let (healed, healed_symbols) = heal_stuck_live_positions(client, db, contracts, &marks).await;
    closed_symbols.extend(healed_symbols);
    json!({
        "exchange_count": exchange_count,
        "imported": imported,
        "updated": updated,
        "linked": linked,
        "deduped": deduped,
        "closed": closed,
        "healed": healed,
        "closed_symbols": closed_symbols,
        "synced": exchange_count,
        "message": format!(
            "Synced with MEXC — {exchange_count} on exchange, {imported} imported, {updated} updated, {closed} closed locally, {healed} healed"
        ),
    })
}

/// Close live DB rows that are still `open` but absent from MEXC (failed rollbacks,
/// dry-run ghosts, manual exchange closes the sync pass missed).
pub async fn heal_stuck_live_positions(
    client: &MexcPrivateClient,
    db: &Database,
    contracts: Option<&HashMap<String, ContractInfo>>,
    marks: &HashMap<String, f64>,
) -> (usize, Vec<String>) {
    if !client.has_credentials() {
        return (0, Vec::new());
    }
    let raw_positions = match client.get_open_positions().await {
        Ok(p) => p,
        Err(_) => return (0, Vec::new()),
    };

    let mut seen_symbol_side: HashSet<(String, String)> = HashSet::new();
    let mut seen_ids: HashSet<i64> = HashSet::new();
    for raw in &raw_positions {
        let Some(ep) = parse_exchange_position(raw) else {
            continue;
        };
        seen_ids.insert(ep.exchange_id);
        seen_symbol_side.insert((ep.symbol.clone(), side_to_str(ep.side).into()));
    }

    let mut healed = 0usize;
    let mut healed_symbols: Vec<String> = Vec::new();
    let open = db.get_open_positions().await.unwrap_or_default();
    for pos in &open {
        if pos.get("paper").and_then(|v| v.as_bool()).unwrap_or(true) {
            continue;
        }
        let symbol = pos.get("symbol").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let side = pos.get("side").and_then(|v| v.as_str()).unwrap_or("long").to_string();
        let id = pos.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        if id == 0 {
            continue;
        }

        let on_exchange = if let Some(exchange_id) = pos.get("exchange_position_id").and_then(|v| v.as_i64()) {
            seen_ids.contains(&exchange_id)
        } else {
            seen_symbol_side.contains(&(symbol.clone(), side.clone()))
        };

        if on_exchange {
            continue;
        }

        let entry_mode = pos
            .get("entry_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("market");
        // Resting limit orders are not exchange positions until filled — do not heal them away.
        if matches!(entry_mode, "limit" | "sniper") {
            continue;
        }

        let reason = "phantom_heal";
        let (cs, fee, mark) = pricing_for_symbol(&symbol, contracts, marks);
        if let Some(sym) = close_missing_position(
            client,
            db,
            pos,
            reason,
            cs,
            fee,
            mark,
            &format!("Healed stuck position not on MEXC: {symbol}"),
            json!({}),
        )
        .await
        {
            healed += 1;
            healed_symbols.push(sym);
        }
    }
    (healed, healed_symbols)
}

/// Run once at startup to reconcile the local position DB against MEXC.
///
/// This heals divergence that can occur if the bot was restarted while a
/// position was live on the exchange:
///   - Positions open in DB but gone on exchange → closed locally.
///   - Positions open on exchange but missing from DB → imported.
///   - Duplicate rows for the same symbol/side → deduplicated.
///
/// Only runs when live credentials are present; silently skips for paper-only.
pub async fn reconcile_on_boot(
    client: &MexcPrivateClient,
    db: &Database,
    config: &AppConfig,
) {
    if !client.has_credentials() {
        tracing::info!("Boot reconciliation skipped — no live API credentials");
        return;
    }
    tracing::info!("Boot reconciliation: comparing DB positions with MEXC exchange…");
    let result = sync_exchange_positions(client, db, config, None).await;
    tracing::info!(
        "Boot reconciliation complete: {}",
        result.get("message").and_then(|v| v.as_str()).unwrap_or("done")
    );
    let _ = db
        .log_event(
            "boot_reconciliation",
            "Startup position reconciliation complete",
            Some(result),
        )
        .await;
}

async fn close_duplicate_exchange_rows(
    db: &Database,
    keep_id: i64,
    symbol: &str,
    side: &str,
    exchange_id: i64,
    contracts: Option<&HashMap<String, ContractInfo>>,
    marks: &HashMap<String, f64>,
) -> usize {
    let mut removed = 0usize;
    let open = db.get_open_positions().await.unwrap_or_default();
    for pos in open {
        let id = pos.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        if id == keep_id || pos.get("paper").and_then(|v| v.as_bool()).unwrap_or(true) {
            continue;
        }
        if pos.get("symbol").and_then(|v| v.as_str()) != Some(symbol) {
            continue;
        }
        if pos.get("side").and_then(|v| v.as_str()) != Some(side) {
            continue;
        }
        if pos.get("source").and_then(|v| v.as_str()) == Some("exchange")
            && pos.get("exchange_position_id").and_then(|v| v.as_i64()) == Some(exchange_id)
        {
            let (cs, fee, mark) = pricing_for_symbol(symbol, contracts, marks);
            if db
                .close_position_synced(id, "deduped", cs, fee, mark)
                .await
                .is_ok()
            {
                removed += 1;
            }
        }
    }
    removed
}

async fn dedupe_symbol_side_rows(
    db: &Database,
    seen: &HashSet<(String, String)>,
    contracts: Option<&HashMap<String, ContractInfo>>,
    marks: &HashMap<String, f64>,
) -> usize {
    let mut removed = 0usize;
    for (symbol, side) in seen {
        let rows: Vec<Value> = db
            .get_open_positions()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|p| {
                !p.get("paper").and_then(|v| v.as_bool()).unwrap_or(true)
                    && p.get("symbol").and_then(|v| v.as_str()) == Some(symbol.as_str())
                    && p.get("side").and_then(|v| v.as_str()) == Some(side.as_str())
            })
            .collect();
        if rows.len() <= 1 {
            continue;
        }
        let mut sorted = rows;
        sorted.sort_by_key(|p| {
            let source_rank = if p.get("source").and_then(|v| v.as_str()) == Some("bot") {
                0
            } else {
                1
            };
            (source_rank, p.get("id").and_then(|v| v.as_i64()).unwrap_or(0))
        });
        let keep = &sorted[0];
        let keep_id = keep.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let keep_has_exchange = keep.get("exchange_position_id").and_then(|v| v.as_i64()).is_some();
        for dup in sorted.iter().skip(1) {
            let dup_id = dup.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            if !keep_has_exchange {
                if let Some(eid) = dup.get("exchange_position_id").and_then(|v| v.as_i64()) {
                    let _ = db.set_exchange_position_id(keep_id, eid).await;
                }
            }
            let (cs, fee, mark) = pricing_for_symbol(symbol, contracts, marks);
            if db
                .close_position_synced(dup_id, "deduped", cs, fee, mark)
                .await
                .is_ok()
            {
                removed += 1;
            }
        }
    }
    removed
}
