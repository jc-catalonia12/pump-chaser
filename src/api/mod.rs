pub mod handlers;
pub mod ws;

use std::sync::Arc;

use axum::http::{header, HeaderValue};
use axum::routing::{delete, get, post, put};
use axum::Router;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::AppState;
use crate::utils::web_assets_dir;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        // Health
        .route("/health", get(handlers::health))
        // Scanner
        .route("/scanner/status", get(handlers::scanner_status))
        .route("/scanner/tracked", get(handlers::scanner_tracked))
        .route("/scanner/start", post(handlers::scanner_start))
        .route("/scanner/stop", post(handlers::scanner_stop))
        .route("/scanner/watchlist/settings", get(handlers::watchlist_settings_get))
        .route("/scanner/watchlist/settings", put(handlers::watchlist_settings_put))
        // Signals
        .route("/signals", get(handlers::signals))
        .route("/signals/chart", get(handlers::signals_chart))
        .route("/signals/history", get(handlers::signals_history))
        .route("/signals/stats", get(handlers::signals_stats))
        // Risk & PnL
        .route("/risk", get(handlers::risk))
        .route("/risk/reanchor-wallet", post(handlers::reanchor_wallet))
        .route("/pnl/daily", get(handlers::pnl_daily))
        .route("/wallet", get(handlers::wallet))
        // Positions
        .route("/positions", get(handlers::positions))
        .route("/positions/history", get(handlers::positions_history))
        .route("/positions/sync", post(handlers::positions_sync))
        .route("/positions/:position_id/close", post(handlers::positions_close))
        .route("/positions/:position_id/chart", get(handlers::position_chart))
        // Activity & learning
        .route("/activity", get(handlers::activity))
        .route("/activity/seen", post(handlers::activity_seen))
        .route("/learning/status", get(handlers::learning_status))
        .route("/learning/reload", post(handlers::learning_reload))
        .route("/sentiment/status", get(handlers::sentiment_status))
        .route("/sentiment/news", get(handlers::sentiment_news))
        .route("/llm/status", get(handlers::llm_regime_status))
        .route("/assistant/chat", post(handlers::assistant_chat))
        .route("/tuner/history", get(handlers::tuner_history))
        .route("/promotion/status", get(handlers::promotion_status))
        .route("/audit", get(handlers::audit))
        .route("/optimization", get(handlers::optimization))
        // User config (settings.yaml)
        .route("/config/settings", get(handlers::user_settings_get))
        .route("/config/settings", put(handlers::user_settings_put))
        // Trading control
        .route("/trading/start", post(handlers::trading_start))
        .route("/trading/stop", post(handlers::trading_stop))
        .route("/trading/settings", get(handlers::trading_settings_get))
        .route("/trading/settings", put(handlers::trading_settings_put))
        .route("/kill-switch/activate", post(handlers::kill_switch_activate))
        .route("/kill-switch/deactivate", post(handlers::kill_switch_deactivate))
        .route("/risk/circuit-breaker/reset", post(handlers::circuit_breaker_reset))
        // ML
        .route("/ml/stack", get(handlers::ml_stack))
        .route("/ml/status", get(handlers::ml_status))
        .route("/ml/history", get(handlers::ml_history))
        .route("/ml/shadow-stats", get(handlers::ml_shadow_stats))
        .route("/ml/outcome-trends", get(handlers::ml_outcome_trends))
        .route("/ml/train", post(handlers::ml_train))
        .route("/ml/resolve-signals", post(handlers::ml_resolve_signals))
        // User
        .route("/user/profile", get(handlers::user_profile))
        .route("/user/credentials", put(handlers::user_credentials_put))
        .route("/user/credentials", delete(handlers::user_credentials_delete))
        // Telegram notifications
        .route("/user/telegram", get(handlers::user_telegram_get))
        .route("/user/telegram", put(handlers::user_telegram_put))
        .route("/user/telegram/test", post(handlers::user_telegram_test))
        // Build installer (stubs)
        .route("/build/installer", get(handlers::build_installer_info))
        .route("/build/installer", post(handlers::build_installer_post))
        .route("/build/installer/status", get(handlers::build_installer_status))
        .route("/build/installer/download/:filename", get(handlers::build_installer_download))
        .route(
            "/build/installer/artifacts/:filename",
            delete(handlers::build_installer_delete),
        )
        // Training & backtest (stubs)
        .route("/training/info", get(handlers::training_info))
        .route("/training/status", get(handlers::training_status))
        .route("/training/run", post(handlers::training_run))
        .route("/training/run/sync", post(handlers::training_run_sync))
        .route("/backtest", post(handlers::backtest))
        .route("/backtest/acceptance", post(handlers::backtest_acceptance))
        .route("/walk-forward", post(handlers::walk_forward))
        .route("/historical/fetch", post(handlers::historical_fetch))
        // Exchanges & live
        .route("/exchanges/health", get(handlers::exchanges_health))
        .route("/live/snapshot", get(handlers::live_snapshot))
        .route("/icon.png", get(handlers::app_logo))
        .route("/ws", get(ws::ws_handler))
        .with_state(Arc::new(state));

    // Dashboard HTML/JS/CSS must never be cached by WebView2 — otherwise upgrades
    // keep showing an old index.html (missing Virtual Assistant / stale UI) while
    // /health already reports the new binary version.
    let web_dir = web_assets_dir();
    info!(path = %web_dir.display(), "Serving dashboard UI from");
    let static_files = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0, must-revalidate"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::PRAGMA,
            HeaderValue::from_static("no-cache"),
        ))
        .service(ServeDir::new(web_dir).append_index_html_on_directories(true));

    api.fallback_service(static_files)
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .layer(TraceLayer::new_for_http())
}
