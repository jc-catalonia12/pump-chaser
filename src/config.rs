use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use figment::{providers::Format, Figment};
use serde::{Deserialize, Serialize};

use crate::error::{BotError, Result};

/// Live config shared across API, scanner, risk, and execution — updated when settings are saved.
pub type SharedAppConfig = Arc<RwLock<AppConfig>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub mexc: MexcConfig,
    pub scanner: ScannerConfig,
    pub zones: ZonesConfig,
    pub confluence: ConfluenceConfig,
    pub trading: TradingConfig,
    pub risk: RiskConfig,
    pub execution: ExecutionConfig,
    pub storage: StorageConfig,
    pub ml: MlConfig,
    pub server: ServerConfig,
    #[serde(default)]
    pub learning: LearningConfig,
    #[serde(default)]
    pub scalp: ScalpConfig,
    #[serde(default)]
    pub watchlist: WatchlistConfig,
    #[serde(default)]
    pub backtest: BacktestConfig,
    #[serde(default)]
    pub exchanges: ExchangesConfig,
    #[serde(default)]
    pub alerts: AlertsConfig,
    #[serde(default)]
    pub sniper: SniperConfig,
    #[serde(default)]
    pub pump: PumpConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_host() -> String {
    "127.0.0.1".into()
}

fn default_port() -> u16 {
    8001
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MexcConfig {
    #[serde(default = "default_mexc_rest")]
    pub rest_base_url: String,
    #[serde(default = "default_mexc_ws")]
    pub ws_url: String,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_sec: f64,
    #[serde(default = "default_rate_limit_ms")]
    pub rate_limit_delay_ms: u64,
}

fn default_request_timeout() -> f64 {
    15.0
}

fn default_rate_limit_ms() -> u64 {
    100
}

fn default_mexc_rest() -> String {
    "https://contract.mexc.co".into()
}

fn default_mexc_ws() -> String {
    "wss://contract.mexc.co/edge".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerConfig {
    #[serde(default = "default_kline_interval")]
    pub kline_interval: String,
    #[serde(default = "default_kline_lookback")]
    pub kline_lookback_bars: u32,
    #[serde(default = "default_max_symbols_poll")]
    pub max_symbols_kline_poll: u32,
    #[serde(default = "default_min_turnover")]
    pub min_24h_turnover_usdt: f64,
    #[serde(default = "default_min_price")]
    pub min_price_usdt: f64,
    #[serde(default = "default_true")]
    pub usdt_m_crypto_only: bool,
    #[serde(default = "default_kline_refresh")]
    pub kline_refresh_sec: u64,
}

fn default_kline_refresh() -> u64 {
    60
}

fn default_max_symbols_poll() -> u32 {
    150
}

fn default_min_turnover() -> f64 {
    500_000.0
}

fn default_min_price() -> f64 {
    0.00001
}

fn default_kline_interval() -> String {
    "Min1".into()
}

fn default_kline_lookback() -> u32 {
    60
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZonesConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_zone_lookback")]
    pub lookback_bars: u32,
    #[serde(default = "default_pivot")]
    pub pivot_left: u32,
    #[serde(default = "default_pivot")]
    pub pivot_right: u32,
    #[serde(default = "default_zone_width")]
    pub zone_width_pct: f64,
    #[serde(default = "default_proximity")]
    pub proximity_pct: f64,
}

fn default_zone_lookback() -> u32 {
    50
}

fn default_pivot() -> u32 {
    2
}

fn default_zone_width() -> f64 {
    0.2
}

fn default_proximity() -> f64 {
    0.25
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfluenceConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_min_score")]
    pub min_composite_score: f64,
    #[serde(default = "default_min_zone_score")]
    pub min_zone_score: f64,
    #[serde(default = "default_min_confluences")]
    pub min_confluences: u32,
    #[serde(default = "default_min_move")]
    pub min_move_pct: f64,
    #[serde(default = "default_max_move")]
    pub max_move_pct: f64,
    #[serde(default = "default_vol_surge")]
    pub volume_surge_multiplier: f64,
    #[serde(default = "default_vol_z")]
    pub volume_zscore_threshold: f64,
    #[serde(default = "default_cooldown")]
    pub alert_cooldown_sec: u64,
    #[serde(default = "default_sl_pct")]
    pub default_sl_pct: f64,
    #[serde(default = "default_trailing")]
    pub trailing_stop_pct: f64,
    #[serde(default = "default_trail_act")]
    pub trailing_activation_pct: f64,
    #[serde(default = "default_tp_levels")]
    pub tp_levels_pct: Vec<f64>,
    #[serde(default = "default_tp_fracs")]
    pub tp_close_fractions: Vec<f64>,
    #[serde(default = "default_max_hold")]
    pub max_hold_sec: u64,
    #[serde(default = "default_risk_pct")]
    pub max_risk_per_trade: f64,
    #[serde(default = "default_max_positions")]
    pub max_concurrent_positions: u32,
    #[serde(default = "default_true")]
    pub require_structure: bool,
    #[serde(default = "default_true")]
    pub require_market_structure_bias: bool,
    #[serde(default = "default_ms_lookback")]
    pub market_structure_lookback_bars: u32,
    #[serde(default)]
    pub require_inside_zone: bool,
    #[serde(default)]
    pub require_ema_trend: bool,
    #[serde(default = "default_ema_span")]
    pub ema_trend_span: u32,
    #[serde(default)]
    pub require_pullback_candle: bool,
    #[serde(default = "default_range_extreme")]
    pub max_range_extreme_pct: f64,
    #[serde(default = "default_extension")]
    pub max_extension_pct: f64,
    #[serde(default = "default_base_lev")]
    pub base_leverage: u32,
    #[serde(default = "default_moderate_lev")]
    pub moderate_leverage: u32,
    #[serde(default = "default_strong_lev")]
    pub strong_leverage: u32,
    /// Require higher-timeframe structural bias to align with signal direction.
    #[serde(default = "default_true")]
    pub htf_enabled: bool,
    /// Kline interval for higher-timeframe bias check (e.g. "Min15", "Min30").
    #[serde(default = "default_htf_interval")]
    pub htf_interval: String,
    /// Number of HTF bars to fetch and analyse.
    #[serde(default = "default_htf_lookback")]
    pub htf_lookback_bars: u32,
    /// Detect 15m liquidity grabs (sweep + reclaim) for entries.
    #[serde(default = "default_true")]
    pub liquidity_grab_enabled: bool,
    /// Block entries unless a recent HTF liquidity grab aligns with direction.
    #[serde(default = "default_true")]
    pub require_liquidity_grab: bool,
    /// HTF bars used to build the high/low liquidity pool.
    #[serde(default = "default_liq_grab_lookback")]
    pub liquidity_grab_lookback_bars: u32,
    /// Max age (bars) of the grab candle on HTF.
    #[serde(default = "default_liq_grab_age")]
    pub liquidity_grab_max_age_bars: u32,
    /// Minimum sweep beyond pool level (%).
    #[serde(default = "default_liq_grab_sweep")]
    pub liquidity_grab_sweep_pct: f64,
    /// Minimum wick rejection ratio (0–1) on the grab candle.
    #[serde(default = "default_liq_grab_rejection")]
    pub liquidity_grab_min_rejection: f64,
    /// Wider trail once move exceeds this fraction (e.g. 0.03 = 3%).
    #[serde(default = "default_trail_ext_act")]
    pub trailing_extended_activation_pct: f64,
    #[serde(default = "default_trail_ext_stop")]
    pub trailing_extended_stop_pct: f64,
    /// Widest trail for large runners (pumps/dumps).
    #[serde(default = "default_trail_run_act")]
    pub trailing_runner_activation_pct: f64,
    #[serde(default = "default_trail_run_stop")]
    pub trailing_runner_stop_pct: f64,
}

fn default_min_zone_score() -> f64 {
    55.0
}

fn default_min_move() -> f64 {
    0.15
}

fn default_max_move() -> f64 {
    2.5
}

fn default_vol_surge() -> f64 {
    1.8
}

fn default_vol_z() -> f64 {
    2.0
}

fn default_cooldown() -> u64 {
    300
}

fn default_trailing() -> f64 {
    0.008
}

fn default_trail_act() -> f64 {
    0.012
}

fn default_tp_levels() -> Vec<f64> {
    vec![0.01, 0.025, 0.045]
}

fn default_tp_fracs() -> Vec<f64> {
    vec![0.4, 0.35, 0.25]
}

fn default_ms_lookback() -> u32 {
    48
}

fn default_ema_span() -> u32 {
    20
}

fn default_range_extreme() -> f64 {
    0.18
}

fn default_extension() -> f64 {
    2.0
}

fn default_base_lev() -> u32 {
    20
}

fn default_moderate_lev() -> u32 {
    50
}

fn default_strong_lev() -> u32 {
    100
}

fn default_htf_interval() -> String {
    "Min15".into()
}

fn default_htf_lookback() -> u32 {
    120
}

fn default_liq_grab_lookback() -> u32 {
    80
}

fn default_liq_grab_age() -> u32 {
    5
}

fn default_liq_grab_sweep() -> f64 {
    0.04
}

fn default_liq_grab_rejection() -> f64 {
    0.4
}

fn default_trail_ext_act() -> f64 {
    0.03
}

fn default_trail_ext_stop() -> f64 {
    0.014
}

fn default_trail_run_act() -> f64 {
    0.05
}

fn default_trail_run_stop() -> f64 {
    0.024
}

fn default_min_score() -> f64 {
    60.0
}

fn default_min_confluences() -> u32 {
    3
}

fn default_sl_pct() -> f64 {
    0.018
}

fn default_max_hold() -> u64 {
    2400
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    #[serde(default = "default_trading_mode")]
    pub mode: String,
}

fn default_trading_mode() -> String {
    "confluence".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    #[serde(default = "default_risk_pct")]
    pub max_risk_per_trade: f64,
    #[serde(default = "default_max_leverage")]
    pub max_leverage: u32,
    #[serde(default = "default_max_positions")]
    pub max_concurrent_positions: u32,
    #[serde(default = "default_daily_loss")]
    pub daily_loss_limit: f64,
    #[serde(default = "default_min_margin")]
    pub min_position_margin_usdt: f64,
    #[serde(default = "default_true")]
    pub use_live_wallet_equity: bool,
    #[serde(default = "default_exposure")]
    pub max_exposure_pct: f64,
    #[serde(default = "default_funding_abs")]
    pub max_funding_rate_abs: f64,
    #[serde(default = "default_max_atr")]
    pub max_atr_pct: f64,
    #[serde(default = "default_sl_pct")]
    pub default_sl_pct: f64,
    #[serde(default = "default_trailing")]
    pub trailing_stop_pct: f64,
    /// When false (default), the bot refuses to open a position on a symbol that
    /// already has an open position in the opposite direction (no hedging).
    #[serde(default)]
    pub allow_hedge: bool,

    // ── Circuit breakers ────────────────────────────────────────────────────
    /// Auto-pause trading after this many consecutive losses.
    /// Set to 0 to disable the streak breaker.
    #[serde(default = "default_max_consec_losses")]
    pub max_consecutive_losses: u32,
    /// Seconds to stay paused after a loss streak trips. Default 30 min.
    #[serde(default = "default_loss_streak_cooldown")]
    pub loss_streak_cooldown_sec: u64,
    /// If current drawdown from peak exceeds this fraction, auto-activate the
    /// kill switch. 0.15 = 15% drawdown. Set to 1.0 to disable.
    #[serde(default = "default_drawdown_halt")]
    pub max_drawdown_halt_pct: f64,
    /// Seconds to block re-entry on a specific symbol after a stop-out.
    /// Prevents consecutive revenge-trades on the same failing setup.
    #[serde(default = "default_symbol_cooldown")]
    pub symbol_loss_cooldown_sec: u64,
    /// Seconds since last WS tick before marking the feed as stale.
    #[serde(default = "default_ws_stale_sec")]
    pub ws_stale_sec: u64,
    /// Minimum expected gross profit in USDT across all TP levels to take a trade.
    /// 0.0 = disabled (no minimum). Default 5.0.
    #[serde(default = "default_min_profit_usdt")]
    pub min_profit_usdt: f64,
    /// Max open positions for confluence strategy (separate slot).
    #[serde(default = "default_max_confluence_positions")]
    pub max_confluence_positions: u32,
    /// Max open positions for volume_pump strategy (separate slot).
    #[serde(default = "default_max_volume_pump_positions")]
    pub max_volume_pump_positions: u32,
}

// ── Execution mode ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntryMode {
    /// Immediate market fill on signal (original behaviour).
    Market,
    /// Place a limit order at `limit_offset_pct` above/below current price.
    Limit,
    /// Wait for a 1m sniper trigger (pullback / pin-bar) after the 15m setup
    /// fires, then submit a limit order at the trigger price.
    Sniper,
}

impl Default for EntryMode {
    fn default() -> Self {
        Self::Sniper
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniperConfig {
    /// How to enter confirmed setups.
    #[serde(default)]
    pub entry_mode: EntryMode,
    /// Limit order offset from mark price (%): positive = more favourable.
    /// e.g. 0.001 = 0.1% below mark for longs, 0.1% above for shorts.
    #[serde(default = "default_limit_offset")]
    pub limit_offset_pct: f64,
    /// Seconds to wait for the limit order to fill before cancelling.
    #[serde(default = "default_limit_ttl")]
    pub limit_ttl_sec: u64,
    /// Lookback bars (1m) to check for sniper trigger after HTF setup.
    #[serde(default = "default_sniper_lookback")]
    pub sniper_lookback_bars: u32,
    /// Minimum wick-rejection ratio (0–1) on the 1m trigger candle (pin-bar).
    #[serde(default = "default_sniper_rejection")]
    pub sniper_min_wick_rejection: f64,
    /// Maximum pullback allowed (fraction of SL distance) before trigger is invalid.
    #[serde(default = "default_sniper_pullback")]
    pub sniper_max_pullback_pct: f64,
    /// Seconds an HTF setup stays valid, waiting for a 1m trigger.
    #[serde(default = "default_sniper_expiry")]
    pub htf_setup_expiry_sec: u64,
}

fn default_limit_offset() -> f64 { 0.001 }
fn default_limit_ttl() -> u64 { 30 }
fn default_sniper_lookback() -> u32 { 5 }
fn default_sniper_rejection() -> f64 { 0.4 }
fn default_sniper_pullback() -> f64 { 0.7 }
fn default_sniper_expiry() -> u64 { 600 }

impl Default for SniperConfig {
    fn default() -> Self {
        serde_json::from_str("{}").expect("SniperConfig default")
    }
}

fn default_funding_abs() -> f64 {
    0.001
}

fn default_max_atr() -> f64 {
    12.0
}

fn default_risk_pct() -> f64 {
    0.01
}

fn default_max_leverage() -> u32 {
    200
}

fn default_max_positions() -> u32 {
    1
}

fn default_daily_loss() -> f64 {
    0.05
}

fn default_min_margin() -> f64 {
    3.0
}

fn default_exposure() -> f64 {
    0.15
}

fn default_max_consec_losses() -> u32 {
    3
}

fn default_loss_streak_cooldown() -> u64 {
    1800
}

fn default_drawdown_halt() -> f64 {
    0.15
}

fn default_symbol_cooldown() -> u64 {
    900
}

fn default_ws_stale_sec() -> u64 {
    30
}

fn default_min_profit_usdt() -> f64 {
    5.0
}

fn default_max_confluence_positions() -> u32 {
    1
}

fn default_max_volume_pump_positions() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    #[serde(default)]
    pub live_trading_enabled: bool,
    #[serde(default = "default_true")]
    pub dry_run: bool,
    #[serde(default = "default_true")]
    pub sync_exchange_positions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_sqlite")]
    pub sqlite_path: String,
}

fn default_sqlite() -> String {
    "data/mexc_trading_bot.db".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub supervised_enabled: bool,
    #[serde(default = "default_ml_threshold")]
    pub supervised_threshold: f64,
    #[serde(default = "default_min_samples")]
    pub min_training_samples: u32,
    #[serde(default = "default_hard_gate")]
    pub hard_ml_gate: bool,
    #[serde(default)]
    pub onnx_model_path: Option<String>,
    /// SGD weight for real trade wins (live/paper).
    #[serde(default = "default_trade_win_weight")]
    pub trade_win_weight: f64,
    /// SGD weight for real trade losses — typically higher than wins.
    #[serde(default = "default_trade_loss_weight")]
    pub trade_loss_weight: f64,
}

impl MlConfig {
    pub fn trade_outcome_weight(&self, won: bool) -> f64 {
        if won {
            self.trade_win_weight
        } else {
            self.trade_loss_weight
        }
    }
}

fn default_ml_threshold() -> f64 {
    0.55
}

fn default_trade_win_weight() -> f64 {
    2.0
}

fn default_trade_loss_weight() -> f64 {
    3.5
}

fn default_min_samples() -> u32 {
    100
}

fn default_hard_gate() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Save confluence signals rejected by the hard ML gate for shadow training.
    #[serde(default = "default_true")]
    pub shadow_ml_rejects: bool,
    /// SGD weight for shadow-resolved ML-gate rejects (weaker than trade-blocked shadows).
    #[serde(default = "default_shadow_ml_weight")]
    pub shadow_ml_reject_weight: f64,
    /// Max shadow signals saved per symbol per hour (rate limit).
    #[serde(default = "default_shadow_per_symbol_hour")]
    pub shadow_max_per_symbol_hour: u32,
    /// Drop new shadow saves when pending shadow queue exceeds this count.
    #[serde(default = "default_shadow_max_pending")]
    pub shadow_max_pending: u32,
    /// Save confluence near-misses (score within margin of threshold) for shadow training.
    #[serde(default)]
    pub shadow_near_miss: bool,
    /// Composite score margin below min_composite_score for near-miss shadow saves.
    #[serde(default = "default_near_miss_margin")]
    pub near_miss_margin: f64,
    /// SGD weight for shadow-resolved confluence near-misses.
    #[serde(default = "default_shadow_near_miss_weight")]
    pub shadow_near_miss_weight: f64,
}

fn default_shadow_ml_weight() -> f64 {
    0.5
}

fn default_shadow_per_symbol_hour() -> u32 {
    2
}

fn default_shadow_max_pending() -> u32 {
    500
}

fn default_near_miss_margin() -> f64 {
    5.0
}

fn default_shadow_near_miss_weight() -> f64 {
    0.3
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            shadow_ml_rejects: true,
            shadow_ml_reject_weight: default_shadow_ml_weight(),
            shadow_max_per_symbol_hour: default_shadow_per_symbol_hour(),
            shadow_max_pending: default_shadow_max_pending(),
            shadow_near_miss: false,
            near_miss_margin: default_near_miss_margin(),
            shadow_near_miss_weight: default_shadow_near_miss_weight(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalpConfig {
    #[serde(default)]
    pub enabled: bool,
}

impl Default for ScalpConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

/// Volume-pump strategy: abnormal 1m volume + universe rank, fast limit entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PumpConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_pump_vol_surge")]
    pub volume_surge_multiplier: f64,
    #[serde(default = "default_pump_vol_z")]
    pub volume_zscore_threshold: f64,
    #[serde(default = "default_pump_price_min")]
    pub price_change_pct_min: f64,
    #[serde(default = "default_pump_price_max")]
    pub price_change_pct_max: f64,
    #[serde(default = "default_pump_ewma_span")]
    pub ewma_span: u32,
    #[serde(default = "default_pump_min_score")]
    pub min_composite_score: f64,
    #[serde(default = "default_pump_universe_rank")]
    pub universe_rank_max: u32,
    #[serde(default = "default_pump_min_turnover")]
    pub min_24h_turnover_usdt: f64,
    #[serde(default = "default_pump_max_turnover")]
    pub max_24h_turnover_usdt: f64,
    #[serde(default = "default_pump_cooldown")]
    pub alert_cooldown_sec: u64,
    #[serde(default = "default_pump_entry_limit")]
    pub entry_mode: EntryMode,
    #[serde(default = "default_limit_offset")]
    pub limit_offset_pct: f64,
    #[serde(default = "default_pump_limit_ttl")]
    pub limit_ttl_sec: u64,
    #[serde(default = "default_pump_sl")]
    pub default_sl_pct: f64,
    #[serde(default = "default_pump_tp_levels")]
    pub tp_levels_pct: Vec<f64>,
    #[serde(default = "default_pump_tp_fracs")]
    pub tp_close_fractions: Vec<f64>,
    #[serde(default = "default_pump_max_hold")]
    pub max_hold_sec: u64,
    #[serde(default = "default_pump_trailing")]
    pub trailing_stop_pct: f64,
    #[serde(default = "default_pump_trail_act")]
    pub trailing_activation_pct: f64,
    #[serde(default = "default_base_lev")]
    pub base_leverage: u32,
    #[serde(default = "default_moderate_lev")]
    pub moderate_leverage: u32,
    #[serde(default = "default_strong_lev")]
    pub strong_leverage: u32,
    #[serde(default = "default_max_volume_pump_positions")]
    pub max_concurrent_positions: u32,
    #[serde(default = "default_pump_min_profit")]
    pub min_profit_usdt: f64,
    #[serde(default = "default_pump_risk_pct")]
    pub max_risk_per_trade: f64,
    /// Two-phase flow: volume surge arms setup, confirmation gates fire entry.
    #[serde(default = "default_true")]
    pub confirmation_enabled: bool,
    #[serde(default = "default_pump_confirm_ttl")]
    pub confirmation_ttl_sec: u64,
    #[serde(default = "default_true")]
    pub require_breakout_or_shift: bool,
    #[serde(default = "default_pump_breakout_lookback")]
    pub breakout_lookback_bars: u32,
    #[serde(default = "default_pump_breakout_min_pct")]
    pub breakout_min_pct: f64,
    #[serde(default = "default_pump_breakout_vol_mult")]
    pub breakout_vol_mult: f64,
    #[serde(default = "default_true")]
    pub require_structure: bool,
    #[serde(default = "default_true")]
    pub require_market_structure_bias: bool,
    #[serde(default = "default_pump_ms_lookback")]
    pub market_structure_lookback_bars: u32,
    #[serde(default = "default_true")]
    pub htf_enabled: bool,
    #[serde(default = "default_htf_interval")]
    pub htf_interval: String,
    #[serde(default = "default_htf_lookback")]
    pub htf_lookback_bars: u32,
    #[serde(default = "default_true")]
    pub macro_filter_enabled: bool,
    #[serde(default = "default_htf_interval")]
    pub macro_htf_interval: String,
    #[serde(default = "default_pump_macro_lookback")]
    pub macro_htf_lookback_bars: u32,
    #[serde(default = "default_pump_macro_min_move")]
    pub macro_min_move_pct: f64,
}

fn default_pump_vol_surge() -> f64 { 4.0 }
fn default_pump_vol_z() -> f64 { 2.5 }
fn default_pump_price_min() -> f64 { 0.8 }
fn default_pump_price_max() -> f64 { 8.0 }
fn default_pump_ewma_span() -> u32 { 20 }
fn default_pump_min_score() -> f64 { 68.0 }
fn default_pump_confirm_ttl() -> u64 { 180 }
fn default_pump_breakout_lookback() -> u32 { 20 }
fn default_pump_breakout_min_pct() -> f64 { 0.0005 }
fn default_pump_breakout_vol_mult() -> f64 { 1.5 }
fn default_pump_ms_lookback() -> u32 { 48 }
fn default_pump_macro_lookback() -> u32 { 48 }
fn default_pump_macro_min_move() -> f64 { 0.5 }
fn default_pump_universe_rank() -> u32 { 5 }
fn default_pump_min_turnover() -> f64 { 500_000.0 }
fn default_pump_max_turnover() -> f64 { 50_000_000.0 }
fn default_pump_cooldown() -> u64 { 180 }
fn default_pump_entry_limit() -> EntryMode { EntryMode::Limit }
fn default_pump_limit_ttl() -> u64 { 15 }
fn default_pump_sl() -> f64 { 0.012 }
fn default_pump_tp_levels() -> Vec<f64> { vec![0.015, 0.03, 0.05] }
fn default_pump_tp_fracs() -> Vec<f64> { vec![0.5, 0.3, 0.2] }
fn default_pump_max_hold() -> u64 { 900 }
fn default_pump_trailing() -> f64 { 0.008 }
fn default_pump_trail_act() -> f64 { 0.01 }
fn default_pump_min_profit() -> f64 { 1.0 }
fn default_pump_risk_pct() -> f64 { 0.01 }

impl Default for PumpConfig {
    fn default() -> Self {
        serde_json::from_str("{}").expect("PumpConfig default")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchlistConfig {
    #[serde(default = "default_watchlist_mode")]
    pub mode: String,
}

fn default_watchlist_mode() -> String {
    "all".into()
}

impl Default for WatchlistConfig {
    fn default() -> Self {
        Self {
            mode: default_watchlist_mode(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    #[serde(default = "default_backtest_engine")]
    pub engine: String,
}

fn default_backtest_engine() -> String {
    "builtin".into()
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            engine: default_backtest_engine(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangesConfig {
    #[serde(default = "default_mexc_client_name")]
    pub mexc_client: String,
}

fn default_mexc_client_name() -> String {
    "native".into()
}

impl Default for ExchangesConfig {
    fn default() -> Self {
        Self {
            mexc_client: default_mexc_client_name(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

/// Webhook-based alert configuration (Telegram bot or Discord).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertsConfig {
    /// Set to false to silence all webhooks even if a URL is configured.
    #[serde(default)]
    pub enabled: bool,
    /// Telegram: `https://api.telegram.org/bot<TOKEN>/sendMessage?chat_id=<ID>&text=`
    /// Discord:  `https://discord.com/api/webhooks/<ID>/<TOKEN>`
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Telegram chat_id (only needed for Telegram bots, ignored for Discord).
    #[serde(default)]
    pub telegram_chat_id: Option<String>,
    /// Minimum seconds between repeated alerts of the same event type to avoid spam.
    #[serde(default = "default_alert_cooldown")]
    pub min_interval_sec: u64,
}

fn default_alert_cooldown() -> u64 {
    300
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            webhook_url: None,
            telegram_chat_id: None,
            min_interval_sec: default_alert_cooldown(),
        }
    }
}

impl AppConfig {
    /// Load from `config/settings.yaml` with optional `MEXC_BOT_CONFIG` override.
    pub fn load() -> Result<Self> {
        let path = std::env::var("MEXC_BOT_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("config/settings.yaml"));

        if !path.exists() {
            return Err(BotError::Config(format!(
                "config file not found: {}",
                path.display()
            )));
        }

        let cfg: AppConfig = Figment::new()
            .merge(figment::providers::Yaml::file(&path))
            .merge(figment::providers::Env::prefixed("MEXC_BOT_").split("__"))
            .extract()
            .map_err(|e| BotError::Config(e.to_string()))?;

        Ok(cfg)
    }
}
