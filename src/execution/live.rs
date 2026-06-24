//! Live order execution — port from `execution/live_trader.py`.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use crate::config::AppConfig;
use crate::db::Database;
use crate::exchange::{ContractInfo, MexcPrivateClient, AssetBalance};
use crate::models::PositionSide;
use crate::risk::RiskManager;
use crate::signals::PumpSignal;
use crate::utils::UserSecrets;

fn round_vol(vol: f64, contract: &ContractInfo) -> f64 {
    let unit = contract.vol_unit.max(1e-12);
    let steps = (vol / unit).floor();
    (steps * unit).max(contract.min_vol)
}

fn round_vol_safe(contracts: &HashMap<String, ContractInfo>, symbol: &str, vol: f64) -> f64 {
    if let Some(c) = contracts.get(symbol) {
        round_vol(vol, c)
    } else {
        vol.max(1.0)
    }
}

fn parse_json_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

/// MEXC triggerType: 1 = price >= trigger, 2 = price <= trigger.
fn sl_tp_trigger_types(side: PositionSide) -> (i64, i64) {
    match side {
        PositionSide::Long => (2, 1),
        PositionSide::Short => (1, 2),
    }
}

fn round_price(price: f64, contract: &ContractInfo) -> f64 {
    let unit = contract.price_unit.max(1e-12);
    let steps = (price / unit).round();
    steps * unit
}

pub struct LiveTrader {
    config: Arc<AppConfig>,
    db: Arc<Database>,
    client: MexcPrivateClient,
    secrets: UserSecrets,
    contracts: HashMap<String, ContractInfo>,
}

impl LiveTrader {
    pub fn new(
        config: Arc<AppConfig>,
        db: Arc<Database>,
        secrets: UserSecrets,
    ) -> Self {
        let client = MexcPrivateClient::from_secrets(&config.mexc, &secrets);
        Self {
            config,
            db,
            client,
            secrets,
            contracts: HashMap::new(),
        }
    }

    pub fn update_secrets(&mut self, secrets: UserSecrets) {
        self.secrets = secrets.clone();
        self.client = MexcPrivateClient::from_secrets(&self.config.mexc, &secrets);
    }

    /// Expose the private client for boot reconciliation.
    pub fn private_client(&self) -> &MexcPrivateClient {
        &self.client
    }

    pub fn update_contracts(&mut self, contracts: Vec<ContractInfo>) {
        self.contracts = contracts.into_iter().map(|c| (c.symbol.clone(), c)).collect();
    }

    /// Contracts on MEXC are denominated in *contracts*, where one contract is
    /// `contractSize` units of the base coin (e.g. TAO_USDT = 0.01 TAO/contract).
    /// Returns 1.0 when the contract is unknown so callers degrade gracefully.
    pub fn contract_size(&self, symbol: &str) -> f64 {
        self.contracts
            .get(symbol)
            .map(|c| c.contract_size)
            .filter(|&s| s > 0.0)
            .unwrap_or(1.0)
    }

    /// Taker fee rate for the symbol (defaults to MEXC standard 0.06% when unknown).
    pub fn fee_rate(&self, symbol: &str) -> f64 {
        self.contracts
            .get(symbol)
            .map(|c| c.taker_fee_rate)
            .filter(|&r| r > 0.0)
            .unwrap_or(0.0006)
    }

    pub fn is_live(&self) -> bool {
        self.secrets.live_trading
            && self.config.execution.live_trading_enabled
            && self.client.has_credentials()
    }

    pub fn has_credentials(&self) -> bool {
        self.client.has_credentials()
    }

    /// Borrow the underlying private client (e.g. to cancel plan orders).
    pub fn client(&self) -> &MexcPrivateClient {
        &self.client
    }

    pub async fn get_wallet_balance(&self) -> crate::error::Result<AssetBalance> {
        self.client.get_usdt_balance().await
    }

    /// Returns the per-symbol max leverage from contract discovery (no API call required).
    /// Falls back to config max when the symbol is not cached.
    pub fn max_leverage_for_symbol(&self, symbol: &str) -> i32 {
        self.contracts
            .get(symbol)
            .map(|c| c.max_leverage as i32)
            .filter(|&l| l > 0)
            .unwrap_or(self.config.risk.max_leverage as i32)
    }

    pub async fn open_from_signal(
        &self,
        signal: &PumpSignal,
        risk: &mut RiskManager,
    ) -> crate::error::Result<Option<i64>> {
        if !self.is_live() {
            // Paper path: create the position, then correct leverage using the
            // per-symbol MEXC contract max so the displayed leverage is accurate.
            let pos_id = risk.try_open_from_signal(signal, true).await?;
            if let Some(id) = pos_id {
                let contract_max = self.max_leverage_for_symbol(&signal.symbol);
                let corrected = signal.suggested_leverage.min(contract_max as u32).max(1) as i32;
                let _ = self.db.update_position_leverage(id, corrected).await;
            }
            return Ok(pos_id);
        }

        let side = if signal.price_change_pct > 0.0 {
            PositionSide::Long
        } else {
            PositionSide::Short
        };

        let leverage = self.resolve_leverage(signal, side).await;
        let pos_id = match risk.try_open_from_signal(signal, false).await? {
            Some(id) => id,
            None => return Ok(None),
        };

        let positions = self.db.get_open_positions().await?;
        let pos = positions
            .iter()
            .find(|p| p.get("id").and_then(|v| v.as_i64()) == Some(pos_id))
            .cloned()
            .ok_or_else(|| crate::error::BotError::Execution("position not found".into()))?;

        let size = pos.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let mut payload = json!({
            "symbol": signal.symbol,
            "price": signal.last_price,
            "vol": size,
            "side": if side == PositionSide::Long { 1 } else { 3 },
            "type": 5,
            "openType": 2,
            "leverage": leverage,
        });
        payload = self.apply_precision(&signal.symbol, payload);
        payload = self.enforce_min_margin(&signal.symbol, payload, signal.last_price, leverage);

        let vol = payload.get("vol").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if vol <= 0.0 {
            self.rollback_position(pos_id, "volume below contract minimum")
                .await;
            return Ok(None);
        }

        if self.config.execution.dry_run {
            let _ = self
                .db
                .log_event(
                    "live_order_dry_run",
                    &format!("Would open {}", signal.symbol),
                    Some(json!({ "body": payload, "leverage": leverage })),
                )
                .await;
            return Ok(Some(pos_id));
        }

        if !self.ensure_exchange_leverage(&signal.symbol, leverage, side).await {
            self.rollback_position(pos_id, "failed to set MEXC leverage")
                .await;
            return Ok(None);
        }

        match self.client.submit_order(payload.clone()).await {
            Ok(result) => {
                if result.get("success").and_then(|v| v.as_bool()) != Some(true) {
                    self.rollback_position(pos_id, "order rejected").await;
                    let _ = self
                        .db
                        .log_event(
                            "live_order_error",
                            &format!("Order rejected for {}", signal.symbol),
                            Some(result),
                        )
                        .await;
                    return Ok(None);
                }
                let _ = self
                    .db
                    .log_event(
                        "live_order",
                        &format!("Order filled for {}", signal.symbol),
                        Some(result),
                    )
                    .await;
                // Link position to exchange and then place SL / TP plan orders.
                self.link_exchange_position(pos_id, &signal.symbol, side)
                    .await;
                self.place_sl_tp_for_position(pos_id, signal, side, vol).await;
                Ok(Some(pos_id))
            }
            Err(exc) => {
                error!("Open order failed for {}: {exc}", signal.symbol);
                self.rollback_position(pos_id, &exc.to_string()).await;
                let _ = self
                    .db
                    .log_event(
                        "live_order_error",
                        &format!("Order failed for {}", signal.symbol),
                        Some(json!({ "error": exc.to_string(), "payload": payload })),
                    )
                    .await;
                Ok(None)
            }
        }
    }

    /// Place stop-loss and take-profit trigger (plan) orders on MEXC after an
    /// entry order fills, and persist the levels to the local DB.
    async fn place_sl_tp_for_position(
        &self,
        pos_id: i64,
        signal: &PumpSignal,
        side: PositionSide,
        filled_vol: f64,
    ) {
        let sl = signal.projected_stop_loss;
        let tps: Vec<(f64, f64)> = signal
            .projected_take_profits
            .iter()
            .zip(
                signal
                    .tp_close_fractions
                    .iter()
                    .chain(std::iter::repeat(&0.5_f64)),
            )
            .filter_map(|(&price, &frac)| if price > 0.0 { Some((price, frac)) } else { None })
            .collect();

        // Persist levels in DB so the overlay can show them even without an exchange call.
        let tp_json: Vec<serde_json::Value> = tps
            .iter()
            .enumerate()
            .map(|(i, (price, frac))| {
                json!({ "level": i + 1, "price": price, "close_fraction": frac })
            })
            .collect();
        let tp_str = serde_json::to_string(&tp_json).unwrap_or_else(|_| "[]".into());
        let sl_store = if sl > 0.0 { Some(sl) } else { None };
        let tp_store = if !tp_json.is_empty() { Some(tp_str.as_str()) } else { None };
        let _ = self.db.update_position_sl_tp(pos_id, sl_store, tp_store).await;

        if self.config.execution.dry_run {
            return;
        }

        // Give MEXC a moment to register the new position before attaching plan orders.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        let (exchange_pos_id, hold_vol, leverage) = match self.db.get_position_by_id(pos_id).await {
            Ok(Some(p)) => (
                p.get("exchange_position_id").and_then(|v| v.as_i64()),
                p.get("size")
                    .and_then(|v| v.as_f64())
                    .filter(|&s| s > 0.0)
                    .unwrap_or(filled_vol),
                p.get("leverage")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(signal.suggested_leverage as i64) as i32,
            ),
            _ => (None, filled_vol, signal.suggested_leverage as i32),
        };
        let order_vol = round_vol_safe(&self.contracts, &signal.symbol, hold_vol);

        // close_side: 4 = close_long, 2 = close_short
        let close_side: i64 = if side == PositionSide::Long { 4 } else { 2 };
        // triggerType: 1 = price >= trigger, 2 = price <= trigger
        let (sl_trigger_type, tp_trigger_type) = sl_tp_trigger_types(side);
        const TREND_LAST_PRICE: i64 = 1;
        let pacing_ms = self.config.mexc.rate_limit_delay_ms.max(250);

        if sl > 0.0 {
            let body = self.build_plan_order_body(
                &signal.symbol,
                close_side,
                order_vol,
                sl,
                sl_trigger_type,
                TREND_LAST_PRICE,
                leverage,
                exchange_pos_id,
            );
            self.place_plan_order_with_retry(&signal.symbol, "SL", body, pos_id)
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(pacing_ms)).await;
        }

        let tp_vol_rem = order_vol;
        let mut tp_vol_placed = 0.0_f64;
        for (i, (tp_price, frac)) in tps.iter().enumerate() {
            let is_last = i == tps.len() - 1;
            let vol = if is_last {
                (tp_vol_rem - tp_vol_placed).max(1.0)
            } else {
                (order_vol * frac).max(1.0)
            };
            let vol = round_vol_safe(&self.contracts, &signal.symbol, vol);
            tp_vol_placed += vol;
            let body = self.build_plan_order_body(
                &signal.symbol,
                close_side,
                vol,
                *tp_price,
                tp_trigger_type,
                TREND_LAST_PRICE,
                leverage,
                exchange_pos_id,
            );
            self.place_plan_order_with_retry(&signal.symbol, &format!("TP{}", i + 1), body, pos_id)
                .await;
            if i + 1 < tps.len() {
                tokio::time::sleep(std::time::Duration::from_millis(pacing_ms)).await;
            }
        }
    }

    fn build_plan_order_body(
        &self,
        symbol: &str,
        close_side: i64,
        vol: f64,
        trigger_price: f64,
        trigger_type: i64,
        trend: i64,
        leverage: i32,
        exchange_pos_id: Option<i64>,
    ) -> serde_json::Value {
        let mut body = json!({
            "symbol": symbol,
            "side": close_side,
            "openType": 2,
            "orderType": 5,
            "vol": vol,
            "leverage": leverage.max(1),
            "triggerPrice": trigger_price,
            "triggerType": trigger_type,
            "executeCycle": 1,
            "trend": trend,
            "reduceOnly": true,
        });
        if let Some(pid) = exchange_pos_id {
            body["positionId"] = json!(pid);
        }
        self.apply_plan_precision(symbol, body)
    }

    fn apply_plan_precision(&self, symbol: &str, mut payload: Value) -> Value {
        let Some(contract) = self.contracts.get(symbol) else {
            return payload;
        };
        if let Some(p) = payload.get("triggerPrice").and_then(parse_json_f64) {
            payload["triggerPrice"] = json!(round_price(p, contract));
        }
        if let Some(v) = payload.get("vol").and_then(parse_json_f64) {
            payload["vol"] = json!(round_vol(v, contract));
        }
        payload
    }

    fn plan_order_failed(result: &std::result::Result<Value, crate::error::BotError>) -> bool {
        match result {
            Ok(v) => v.get("success").and_then(|x| x.as_bool()) == Some(false),
            Err(_) => true,
        }
    }

    fn plan_order_response_code(value: &Value) -> i64 {
        value.get("code").and_then(|c| c.as_i64()).unwrap_or(0)
    }

    /// Place a plan order with retries and pacing for MEXC rate limits.
    async fn place_plan_order_with_retry(
        &self,
        symbol: &str,
        label: &str,
        body: serde_json::Value,
        pos_id: i64,
    ) {
        let retry_delays_ms = [0_u64, 600, 1500, 3000];
        let mut last_err = String::new();

        for (attempt, delay_ms) in retry_delays_ms.iter().enumerate() {
            if *delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
            }

            let result = self.client.place_plan_order(body.clone()).await;
            if !Self::plan_order_failed(&result) {
                let trigger = body
                    .get("triggerPrice")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".into());
                info!("{label} plan order placed for {symbol} @ {trigger}");
                return;
            }

            let detail = match &result {
                Ok(v) => v.to_string(),
                Err(e) => e.to_string(),
            };
            last_err = detail.clone();
            let code = result
                .as_ref()
                .ok()
                .map(Self::plan_order_response_code)
                .unwrap_or(-1);

            if attempt + 1 < retry_delays_ms.len() {
                warn!(
                    "{label} plan order failed for {symbol} (attempt {}): {detail} — retrying",
                    attempt + 1
                );
                // MEXC 510 = rate limited; wait a bit longer before the next attempt.
                if code == 510 {
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                }
            } else {
                let msg = format!("{label} plan order permanently failed for {symbol}: {detail}");
                warn!("{msg}");
                let _ = self
                    .db
                    .log_event(
                        "plan_order_error",
                        &msg,
                        Some(json!({
                            "position_id": pos_id,
                            "label": label,
                            "symbol": symbol,
                            "response": result.as_ref().ok(),
                            "error": result.as_ref().err().map(|e| e.to_string()),
                        })),
                    )
                    .await;
            }
        }

        if !last_err.is_empty() {
            debug!("{label} plan order final error for {symbol}: {last_err}");
        }
    }

    pub async fn close_position(
        &self,
        symbol: &str,
        size: f64,
        side: PositionSide,
        exchange_position_id: Option<i64>,
        mark_price: Option<f64>,
    ) -> Value {
        let close_side = if side == PositionSide::Long { 4 } else { 2 };
        let mut payload = json!({
            "symbol": symbol,
            "vol": size,
            "side": close_side,
            "type": 5,
            "openType": 2,
        });
        if let Some(p) = mark_price {
            payload["price"] = json!(p);
        }
        if let Some(pid) = exchange_position_id {
            payload["positionId"] = json!(pid);
        }
        payload = self.apply_precision(symbol, payload);

        if self.config.execution.dry_run {
            let _ = self
                .db
                .log_event("live_order_dry_run", &format!("Would close {symbol}"), Some(payload.clone()))
                .await;
            return json!({ "dry_run": true, "success": true, "payload": payload });
        }
        if !self.client.has_credentials() {
            return json!({ "success": false, "error": "MEXC API credentials not configured" });
        }
        match self.client.submit_order(payload).await {
            Ok(result) => result,
            Err(exc) => {
                let _ = self
                    .db
                    .log_event(
                        "live_order_error",
                        &format!("Close failed for {symbol}"),
                        Some(json!({ "error": exc.to_string() })),
                    )
                    .await;
                json!({ "success": false, "error": exc.to_string() })
            }
        }
    }

    /// Returns true if the exchange currently holds an open position matching
    /// the given symbol and side. Used to detect phantom/dry-run positions that
    /// exist only in the local DB (so a manual close doesn't try — and fail — to
    /// submit a real exchange order for a position that was never opened live).
    ///
    /// On a network/credential error this returns `None` so callers can decide
    /// whether to proceed; a definitive `Some(false)` means no live position.
    pub async fn exchange_has_position(&self, symbol: &str, side: PositionSide) -> Option<bool> {
        if !self.client.has_credentials() {
            return None;
        }
        let raw_positions = self.client.get_open_positions().await.ok()?;
        let found = raw_positions.iter().any(|raw| {
            if raw.get("symbol").and_then(|v| v.as_str()) != Some(symbol) {
                return false;
            }
            let raw_side = if raw.get("positionType").and_then(|v| v.as_i64()) == Some(1) {
                PositionSide::Long
            } else {
                PositionSide::Short
            };
            let hold_vol = raw.get("holdVol").and_then(|v| v.as_f64()).unwrap_or(0.0);
            raw_side == side && hold_vol > 0.0
        });
        Some(found)
    }

    pub async fn sync_exchange_positions(&self) -> Value {
        crate::execution::position_sync::sync_exchange_positions(
            &self.client,
            &self.db,
            &self.config,
        )
        .await
    }

    fn apply_precision(&self, symbol: &str, mut payload: Value) -> Value {
        let Some(contract) = self.contracts.get(symbol) else {
            return payload;
        };
        if let Some(p) = payload.get("price").and_then(|v| v.as_f64()) {
            payload["price"] = json!(round_price(p, contract));
        }
        if let Some(v) = payload.get("vol").and_then(|v| v.as_f64()) {
            payload["vol"] = json!(round_vol(v, contract));
        }
        payload
    }

    fn enforce_min_margin(
        &self,
        symbol: &str,
        mut payload: Value,
        entry: f64,
        leverage: i32,
    ) -> Value {
        let min_margin = self.config.risk.min_position_margin_usdt;
        if min_margin <= 0.0 {
            return payload;
        }
        let Some(contract) = self.contracts.get(symbol) else {
            return payload;
        };
        let vol = payload.get("vol").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let lev = leverage.max(1) as f64;
        let margin = vol * entry / lev;
        if margin >= min_margin {
            return payload;
        }
        let min_vol = (min_margin * lev) / entry.max(1e-12);
        let bumped = round_vol(min_vol, contract);
        payload["vol"] = json!(bumped);
        payload
    }

    async fn resolve_leverage(&self, signal: &PumpSignal, side: PositionSide) -> i32 {
        let mut symbol_max = self
            .contracts
            .get(&signal.symbol)
            .map(|c| c.max_leverage as i32)
            .unwrap_or(self.config.risk.max_leverage as i32);

        if self.client.has_credentials() {
            if let Ok(account_max) = self
                .client
                .get_symbol_max_leverage(&signal.symbol, side)
                .await
            {
                if account_max > 0 {
                    symbol_max = account_max;
                }
            }
        }

        signal.suggested_leverage.min(symbol_max as u32).max(1) as i32
    }

    async fn ensure_exchange_leverage(&self, symbol: &str, leverage: i32, side: PositionSide) -> bool {
        if !self.client.has_credentials() {
            return true;
        }
        match self.client.change_leverage(symbol, leverage, side).await {
            Ok(result) => result.get("success").and_then(|v| v.as_bool()).unwrap_or(false),
            Err(exc) => {
                warn!("Failed to set leverage for {symbol}: {exc}");
                false
            }
        }
    }

    async fn link_exchange_position(&self, pos_id: i64, symbol: &str, side: PositionSide) {
        let Ok(raw_positions) = self.client.get_open_positions().await else {
            return;
        };
        for raw in raw_positions {
            if raw.get("state").and_then(|v| v.as_i64()) != Some(1) {
                continue;
            }
            if raw.get("symbol").and_then(|v| v.as_str()) != Some(symbol) {
                continue;
            }
            let raw_side = if raw.get("positionType").and_then(|v| v.as_i64()) == Some(1) {
                PositionSide::Long
            } else {
                PositionSide::Short
            };
            if raw_side != side {
                continue;
            }
            let hold_vol = raw.get("holdVol").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if hold_vol <= 0.0 {
                continue;
            }
            let exchange_id = raw.get("positionId").and_then(|v| v.as_i64());
            let entry = raw
                .get("holdAvgPrice")
                .or_else(|| raw.get("openAvgPrice"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let leverage = raw
                .get("leverage")
                .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                .map(|l| l as i32);
            let _ = self
                .db
                .update_position_after_exchange(pos_id, hold_vol, entry, exchange_id, leverage)
                .await;
            info!("Linked local position {pos_id} to exchange {symbol}");
            return;
        }
    }

    async fn rollback_position(&self, pos_id: i64, reason: &str) {
        let _ = self.db.close_position(pos_id, 0.0, 1.0, 0.0, "rollback").await;
        let _ = self
            .db
            .log_event(
                "position_rollback",
                &format!("Removed phantom position id={pos_id}"),
                Some(json!({ "reason": reason, "position_id": pos_id })),
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::ContractInfo;

    fn make_contract(vol_unit: f64, min_vol: f64, price_unit: f64, max_lev: u32) -> ContractInfo {
        ContractInfo {
            symbol: "TEST_USDT".into(),
            base_coin: "TEST".into(),
            quote_coin: "USDT".into(),
            contract_size: 1.0,
            state: 0,
            api_allowed: true,
            taker_fee_rate: 0.0006,
            is_hidden: false,
            price_scale: 5,
            vol_scale: 0,
            min_vol,
            vol_unit,
            price_unit,
            max_leverage: max_lev,
        }
    }

    #[test]
    fn round_vol_snaps_to_step() {
        let c = make_contract(0.01, 0.01, 0.0001, 100);
        // 1.234 → 1.23 (floor to 2 dp step)
        let rounded = round_vol(1.234, &c);
        assert!((rounded - 1.23).abs() < 1e-10, "expected 1.23 got {rounded}");
    }

    #[test]
    fn round_vol_respects_min_vol() {
        let c = make_contract(0.01, 0.5, 0.0001, 100);
        // Very small vol should be clamped to min_vol.
        let rounded = round_vol(0.0001, &c);
        assert!((rounded - 0.5).abs() < 1e-10, "expected 0.5 got {rounded}");
    }

    #[test]
    fn round_price_snaps_to_tick() {
        let c = make_contract(0.01, 0.01, 0.05, 100);
        // 1.234 → 1.25 (nearest 0.05 tick)
        let rounded = round_price(1.234, &c);
        assert!((rounded - 1.25).abs() < 1e-10, "expected 1.25 got {rounded}");
    }

    #[test]
    fn round_price_exact_multiple_unchanged() {
        let c = make_contract(0.01, 0.01, 0.1, 100);
        let rounded = round_price(1.3, &c);
        assert!((rounded - 1.3).abs() < 1e-10, "expected 1.3 got {rounded}");
    }

    #[test]
    fn leverage_clamped_to_contract_max() {
        // Simulate what resolve_leverage would enforce at the contract level.
        let c = make_contract(0.01, 0.01, 0.0001, 20);
        let requested: i32 = 50;
        let effective = requested.min(c.max_leverage as i32);
        assert_eq!(effective, 20, "leverage should be clamped to contract max");
    }

    #[test]
    fn sl_tp_trigger_types_match_mexc_semantics() {
        assert_eq!(sl_tp_trigger_types(PositionSide::Long), (2, 1));
        assert_eq!(sl_tp_trigger_types(PositionSide::Short), (1, 2));
    }
}
