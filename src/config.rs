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
    pub trading: TradingConfig,
    pub risk: RiskConfig,
    pub execution: ExecutionConfig,
    pub storage: StorageConfig,
    pub ml: MlConfig,
    pub server: ServerConfig,
    #[serde(default)]
    pub learning: LearningConfig,
    #[serde(default)]
    pub watchlist: WatchlistConfig,
    #[serde(default)]
    pub backtest: BacktestConfig,
    #[serde(default)]
    pub exchanges: ExchangesConfig,
    #[serde(default)]
    pub alerts: AlertsConfig,
    #[serde(default)]
    pub sentiment: SentimentConfig,
    #[serde(default)]
    pub ai: AiConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub assistant: AssistantConfig,
    #[serde(default)]
    pub decision: DecisionConfig,
}

/// Unified decision & sizing authority — Phase 5.
/// The decision engine is the single go/no-go authority: it combines the ML
/// win probability, expected value (in R), the LLM regime alignment, and
/// sentiment into an approve/reject + size/leverage multipliers, *before* the
/// preserved `RiskManager` safety net. When the LLM regime is neutral (Ollama
/// offline) the regime terms vanish and the decision reduces to a pure
/// EV / reward-risk gate on the ML edge — the graceful-degradation path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minimum expected value (in R multiples) to approve a trade.
    /// EV = p*reward_risk - (1-p). 0.0 = require non-negative expectancy.
    #[serde(default = "default_decision_min_ev")]
    pub min_expected_value: f64,
    /// Minimum reward:risk ratio (first TP distance / SL distance).
    #[serde(default = "default_decision_min_rr")]
    pub min_reward_risk: f64,
    /// LLM confidence at/above which a strongly-opposing regime can veto.
    #[serde(default = "default_decision_veto_conf")]
    pub regime_veto_confidence: f64,
    /// Confidence-weighted alignment (0..1) below -this triggers a veto.
    #[serde(default = "default_decision_veto_align")]
    pub regime_veto_alignment: f64,
    /// Max fractional size boost/cut from confidence-weighted regime alignment.
    #[serde(default = "default_decision_regime_boost")]
    pub regime_size_boost: f64,
    /// Fractional size (and leverage) haircut in a high-volatility regime.
    #[serde(default = "default_decision_highvol_haircut")]
    pub high_vol_size_haircut: f64,
    /// How strongly directional sentiment nudges size (fraction per unit).
    #[serde(default = "default_decision_sentiment_weight")]
    pub sentiment_size_weight: f64,
    /// Lower/upper clamp on the final size multiplier.
    #[serde(default = "default_decision_min_size_scale")]
    pub min_size_scale: f64,
    #[serde(default = "default_decision_max_size_scale")]
    pub max_size_scale: f64,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_expected_value: default_decision_min_ev(),
            min_reward_risk: default_decision_min_rr(),
            regime_veto_confidence: default_decision_veto_conf(),
            regime_veto_alignment: default_decision_veto_align(),
            regime_size_boost: default_decision_regime_boost(),
            high_vol_size_haircut: default_decision_highvol_haircut(),
            sentiment_size_weight: default_decision_sentiment_weight(),
            min_size_scale: default_decision_min_size_scale(),
            max_size_scale: default_decision_max_size_scale(),
        }
    }
}

fn default_decision_min_ev() -> f64 {
    0.0
}

fn default_decision_min_rr() -> f64 {
    0.8
}

fn default_decision_veto_conf() -> f64 {
    0.6
}

fn default_decision_veto_align() -> f64 {
    0.5
}

fn default_decision_regime_boost() -> f64 {
    0.25
}

fn default_decision_highvol_haircut() -> f64 {
    0.25
}

fn default_decision_sentiment_weight() -> f64 {
    0.15
}

fn default_decision_min_size_scale() -> f64 {
    0.4
}

fn default_decision_max_size_scale() -> f64 {
    1.5
}

/// Local LLM (Ollama) market-regime layer — Phase 4.
/// Purely additive: regime output feeds ML features and (later) the decision
/// layer as soft context. If Ollama is offline the regime stays neutral and
/// nothing is ever hard-blocked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Ollama HTTP endpoint.
    #[serde(default = "default_llm_base_url")]
    pub base_url: String,
    /// Ollama model tag (must already be pulled, e.g. `ollama pull llama3.2`).
    #[serde(default = "default_llm_model")]
    pub model: String,
    /// Seconds between regime classifications (result is cached in between).
    #[serde(default = "default_llm_poll_sec")]
    pub poll_interval_sec: u64,
    /// HTTP timeout per classification request.
    #[serde(default = "default_llm_timeout_sec")]
    pub timeout_sec: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: default_llm_base_url(),
            model: default_llm_model(),
            poll_interval_sec: default_llm_poll_sec(),
            timeout_sec: default_llm_timeout_sec(),
        }
    }
}

fn default_llm_base_url() -> String {
    "http://localhost:11434".into()
}

fn default_llm_model() -> String {
    "llama3.2".into()
}

fn default_llm_poll_sec() -> u64 {
    300
}

fn default_llm_timeout_sec() -> u64 {
    30
}

/// In-app virtual assistant (Ollama chat + optional tools).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantConfig {
    /// Allow `web_fetch` tool (public HTTP GET only).
    #[serde(default = "default_true")]
    pub web_enabled: bool,
    /// Allow `update_settings` tool to write config/settings.yaml.
    #[serde(default = "default_true")]
    pub settings_write_enabled: bool,
    /// Max Ollama tool-call rounds per user message.
    #[serde(default = "default_assistant_max_tool_rounds")]
    pub max_tool_rounds: u32,
    /// Max raw response bytes for web_fetch.
    #[serde(default = "default_assistant_max_fetch_bytes")]
    pub max_fetch_bytes: usize,
    /// Max characters sent back to Ollama per tool result (prevents context blow-up).
    #[serde(default = "default_assistant_max_tool_result_chars")]
    pub max_tool_result_chars: usize,
}

impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            web_enabled: true,
            settings_write_enabled: true,
            max_tool_rounds: default_assistant_max_tool_rounds(),
            max_fetch_bytes: default_assistant_max_fetch_bytes(),
            max_tool_result_chars: default_assistant_max_tool_result_chars(),
        }
    }
}

fn default_assistant_max_tool_rounds() -> u32 {
    4
}

fn default_assistant_max_fetch_bytes() -> usize {
    131_072
}

fn default_assistant_max_tool_result_chars() -> usize {
    4_000
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
    /// Higher-timeframe kline interval for per-symbol and BTC/ETH macro context.
    #[serde(default = "default_htf_interval")]
    pub htf_interval: String,
    /// Number of HTF bars fetched/kept for context features.
    #[serde(default = "default_htf_lookback")]
    pub htf_lookback_bars: u32,
    /// Poll perpetual funding rates for tracked symbols as an ML feature input
    /// (low-frequency — see `FUNDING_REFRESH_EVERY_N_CYCLES`).
    #[serde(default = "default_true")]
    pub fetch_funding_rate: bool,
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

fn default_htf_interval() -> String {
    "Min15".into()
}

fn default_htf_lookback() -> u32 {
    48
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

/// Single AI-driven trading pipeline (the only mode since the strategy engines
/// were retired). `mode` is kept for API/UI compatibility and must be `ai`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    #[serde(default = "default_trading_mode")]
    pub mode: String,
    /// Max seconds a position may stay open before the time exit fires.
    /// 0 = disabled.
    #[serde(default = "default_max_hold")]
    pub max_hold_sec: u64,
}

impl Default for TradingConfig {
    fn default() -> Self {
        Self {
            mode: default_trading_mode(),
            max_hold_sec: default_max_hold(),
        }
    }
}

fn default_trading_mode() -> String {
    "ai".into()
}

fn default_max_hold() -> u64 {
    2400
}

/// AI candidate generator (Phase 1) — sanity filters and candidate shaping only.
/// The ML decision core is the quality gate; nothing here should hard-block a
/// setup on "strategy" grounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    /// Minimum absolute recent move (%) for a symbol to become a candidate.
    /// Purely a dead-symbol filter, not a momentum gate.
    #[serde(default = "default_ai_min_move_pct")]
    pub min_move_pct: f64,
    /// Ticks used to measure the recent directional move.
    #[serde(default = "default_ai_move_lookback_ticks")]
    pub move_lookback_ticks: usize,
    /// Seconds between candidates on the same symbol (rate limiter, not a gate).
    #[serde(default = "default_ai_cooldown_sec")]
    pub signal_cooldown_sec: u64,
    /// Stop distance = ATR% x this multiple (floored by risk.default_sl_pct).
    #[serde(default = "default_ai_atr_sl_mult")]
    pub atr_sl_mult: f64,
    /// Take-profit levels expressed in R multiples of the stop distance.
    #[serde(default = "default_ai_tp_r_multiples")]
    pub tp_r_multiples: Vec<f64>,
    /// Fraction of the position closed at each TP level.
    #[serde(default = "default_ai_tp_close_fractions")]
    pub tp_close_fractions: Vec<f64>,
    /// Default leverage for candidates (ML risk scaling adjusts sizing).
    #[serde(default = "default_ai_leverage")]
    pub base_leverage: u32,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            min_move_pct: default_ai_min_move_pct(),
            move_lookback_ticks: default_ai_move_lookback_ticks(),
            signal_cooldown_sec: default_ai_cooldown_sec(),
            atr_sl_mult: default_ai_atr_sl_mult(),
            tp_r_multiples: default_ai_tp_r_multiples(),
            tp_close_fractions: default_ai_tp_close_fractions(),
            base_leverage: default_ai_leverage(),
        }
    }
}

fn default_ai_min_move_pct() -> f64 {
    0.1
}

fn default_ai_move_lookback_ticks() -> usize {
    6
}

fn default_ai_cooldown_sec() -> u64 {
    120
}

fn default_ai_atr_sl_mult() -> f64 {
    1.5
}

fn default_ai_tp_r_multiples() -> Vec<f64> {
    vec![1.5, 2.5, 4.0]
}

fn default_ai_tp_close_fractions() -> Vec<f64> {
    vec![0.5, 0.3, 0.2]
}

fn default_ai_leverage() -> u32 {
    20
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
    #[serde(default = "default_funding_abs")]
    pub max_funding_rate_abs: f64,
    #[serde(default = "default_max_atr")]
    pub max_atr_pct: f64,
    #[serde(default = "default_sl_pct")]
    pub default_sl_pct: f64,

    // ── Trailing stop (adaptive: widens as profit grows) ────────────────────
    #[serde(default = "default_trailing")]
    pub trailing_stop_pct: f64,
    /// Unrealized move required before the trailing stop activates.
    #[serde(default = "default_trail_act")]
    pub trailing_activation_pct: f64,
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

fn default_sl_pct() -> f64 {
    0.018
}

fn default_trailing() -> f64 {
    0.008
}

fn default_trail_act() -> f64 {
    0.012
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    #[serde(default)]
    pub live_trading_enabled: bool,
    #[serde(default = "default_true")]
    pub dry_run: bool,
    #[serde(default = "default_true")]
    pub sync_exchange_positions: bool,
    /// Taker fee rate applied to paper round-trip (per side; close_position doubles it).
    #[serde(default = "default_paper_fee_rate")]
    pub paper_fee_rate: f64,
    /// Adverse slippage on paper fills (fraction, e.g. 0.0005 = 0.05%).
    #[serde(default = "default_paper_slippage")]
    pub paper_slippage_pct: f64,
    /// Starting paper equity when portfolio row is first created or reset.
    #[serde(default = "default_paper_initial_equity")]
    pub paper_initial_equity: f64,
    /// On next startup, reset paper equity to `paper_initial_equity` (no open positions).
    #[serde(default)]
    pub paper_reset_on_start: bool,
    /// In paper mode: ML/sentiment gates score only — do not hard-block entries.
    #[serde(default = "default_true")]
    pub paper_relax_gates: bool,
    /// Limit order offset from mark price (fraction): more favourable side.
    #[serde(default = "default_limit_offset")]
    pub limit_offset_pct: f64,
    /// Seconds to wait for a resting limit order to fill before cancelling.
    #[serde(default = "default_limit_ttl")]
    pub limit_ttl_sec: u64,
}

fn default_paper_fee_rate() -> f64 {
    0.0006
}

fn default_paper_slippage() -> f64 {
    0.0005
}

fn default_paper_initial_equity() -> f64 {
    100.0
}

fn default_limit_offset() -> f64 {
    0.001
}

fn default_limit_ttl() -> u64 {
    30
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
    /// Auto-enable hard gate when rolling accuracy exceeds this (0 = manual only).
    #[serde(default = "default_gate_min_accuracy")]
    pub gate_min_accuracy: f64,
    /// Auto-disable hard gate when accuracy drops below this.
    #[serde(default = "default_gate_disable_accuracy")]
    pub gate_disable_accuracy: f64,
    /// Minimum resolved samples before auto gate toggling.
    #[serde(default = "default_gate_min_samples")]
    pub gate_auto_min_samples: u32,
    /// Minimum risk scale at gate threshold (fraction of max risk).
    #[serde(default = "default_ml_risk_scale_min")]
    pub ml_risk_scale_min: f64,
    /// Maximum risk scale at high confidence.
    #[serde(default = "default_ml_risk_scale_max")]
    pub ml_risk_scale_max: f64,
    /// Fractional-Kelly multiplier applied to the full-Kelly sizing estimate
    /// (e.g. 0.5 = "half-Kelly" — a common safety margin against edge/odds
    /// estimation error). Drives `suggested_risk_pct` / `suggested_leverage`.
    #[serde(default = "default_kelly_fraction")]
    pub kelly_fraction: f64,
    /// Periodically re-train the GradientBoosting ONNX model offline via
    /// `scripts/export_onnx.py` and hot-reload it into the live pipeline.
    /// Requires a local Python env with `requirements.txt` installed — off
    /// by default so the bot never depends on Python unless opted in.
    #[serde(default)]
    pub auto_retrain_enabled: bool,
    /// Hours between retrain attempts.
    #[serde(default = "default_retrain_interval_hours")]
    pub retrain_interval_hours: u64,
    /// Minimum newly-resolved signals (since the last retrain) required
    /// before spending a retrain cycle.
    #[serde(default = "default_retrain_min_new_samples")]
    pub retrain_min_new_samples: u32,
    /// Python interpreter used to run the export script (repo-root relative
    /// paths, e.g. from a local venv, are resolved by the OS/shell as usual).
    #[serde(default = "default_python_bin")]
    pub python_bin: String,
    /// Path to the offline retrain script, relative to the bot's working directory.
    #[serde(default = "default_export_script_path")]
    pub export_script_path: String,
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

fn default_gate_min_accuracy() -> f64 {
    0.60
}

fn default_gate_disable_accuracy() -> f64 {
    0.52
}

fn default_gate_min_samples() -> u32 {
    150
}

fn default_ml_risk_scale_min() -> f64 {
    0.35
}

fn default_ml_risk_scale_max() -> f64 {
    1.0
}

fn default_kelly_fraction() -> f64 {
    0.5
}

fn default_retrain_interval_hours() -> u64 {
    6
}

fn default_retrain_min_new_samples() -> u32 {
    30
}

fn default_python_bin() -> String {
    "python3".into()
}

fn default_export_script_path() -> String {
    "scripts/export_onnx.py".into()
}

/// Free news/sentiment feed — no paid API keys required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentimentConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Poll interval for RSS / Fear&Greed / Reddit.
    #[serde(default = "default_sentiment_poll_sec")]
    pub poll_interval_sec: u64,
    /// Block longs when global sentiment is below this (-1..1 scale).
    #[serde(default = "default_sentiment_block_long")]
    pub block_long_below: f64,
    /// Block shorts when global sentiment is above this.
    #[serde(default = "default_sentiment_block_short")]
    pub block_short_above: f64,
    /// Per-symbol sentiment threshold (absolute) to block.
    #[serde(default = "default_symbol_sentiment_threshold")]
    pub symbol_block_threshold: f64,
    /// Exponential decay half-life for headline scores (seconds).
    #[serde(default = "default_sentiment_half_life")]
    pub decay_half_life_sec: f64,
}

fn default_sentiment_poll_sec() -> u64 {
    300
}

fn default_sentiment_block_long() -> f64 {
    -0.45
}

fn default_sentiment_block_short() -> f64 {
    0.55
}

fn default_symbol_sentiment_threshold() -> f64 {
    0.55
}

fn default_sentiment_half_life() -> f64 {
    7200.0
}

impl Default for SentimentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_sec: default_sentiment_poll_sec(),
            block_long_below: default_sentiment_block_long(),
            block_short_above: default_sentiment_block_short(),
            symbol_block_threshold: default_symbol_sentiment_threshold(),
            decay_half_life_sec: default_sentiment_half_life(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Save signals rejected by the hard ML gate for shadow training.
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
    /// SGD weight for shadow-resolved near-misses (kept for historical rows).
    #[serde(default = "default_shadow_near_miss_weight")]
    pub shadow_near_miss_weight: f64,
    /// SGD weight for shadow-resolved sentiment-gate rejects.
    #[serde(default = "default_shadow_sentiment_weight")]
    pub shadow_sentiment_weight: f64,
    /// Run walk-forward parameter tuning on a schedule.
    #[serde(default = "default_true")]
    pub auto_tune_enabled: bool,
    /// `suggest` logs recommendations; `apply` promotes challengers to the runtime overlay.
    #[serde(default = "default_auto_tune_apply")]
    pub auto_tune_apply: String,
    /// Hours between auto-tune runs.
    #[serde(default = "default_auto_tune_interval")]
    pub auto_tune_interval_hours: u64,
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

fn default_shadow_near_miss_weight() -> f64 {
    0.3
}

fn default_shadow_sentiment_weight() -> f64 {
    0.4
}

fn default_auto_tune_apply() -> String {
    "suggest".into()
}

fn default_auto_tune_interval() -> u64 {
    6
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            shadow_ml_rejects: true,
            shadow_ml_reject_weight: default_shadow_ml_weight(),
            shadow_max_per_symbol_hour: default_shadow_per_symbol_hour(),
            shadow_max_pending: default_shadow_max_pending(),
            shadow_near_miss_weight: default_shadow_near_miss_weight(),
            shadow_sentiment_weight: default_shadow_sentiment_weight(),
            auto_tune_enabled: true,
            auto_tune_apply: default_auto_tune_apply(),
            auto_tune_interval_hours: default_auto_tune_interval(),
        }
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
    /// Phase 6 paper-acceptance gates — the minimum out-of-sample performance
    /// required before enabling live trading. Checked by the `/backtest/acceptance`
    /// endpoint against a decision-pipeline replay of stored resolved signals.
    #[serde(default = "default_acceptance_min_trades")]
    pub acceptance_min_trades: u32,
    #[serde(default = "default_acceptance_min_win_rate")]
    pub acceptance_min_win_rate: f64,
    #[serde(default = "default_acceptance_min_profit_factor")]
    pub acceptance_min_profit_factor: f64,
    #[serde(default = "default_acceptance_min_expectancy")]
    pub acceptance_min_expectancy: f64,
    #[serde(default = "default_acceptance_max_drawdown")]
    pub acceptance_max_drawdown: f64,
}

fn default_backtest_engine() -> String {
    "builtin".into()
}

fn default_acceptance_min_trades() -> u32 {
    30
}

fn default_acceptance_min_win_rate() -> f64 {
    0.50
}

fn default_acceptance_min_profit_factor() -> f64 {
    1.2
}

fn default_acceptance_min_expectancy() -> f64 {
    0.0
}

fn default_acceptance_max_drawdown() -> f64 {
    0.25
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            engine: default_backtest_engine(),
            acceptance_min_trades: default_acceptance_min_trades(),
            acceptance_min_win_rate: default_acceptance_min_win_rate(),
            acceptance_min_profit_factor: default_acceptance_min_profit_factor(),
            acceptance_min_expectancy: default_acceptance_min_expectancy(),
            acceptance_max_drawdown: default_acceptance_max_drawdown(),
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
