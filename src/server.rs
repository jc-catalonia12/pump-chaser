//! Axum server bootstrap — shared by CLI and Tauri desktop.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use tokio::sync::RwLock as AsyncRwLock;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::api::router;
use crate::config::AppConfig;
use crate::db::Database;
use crate::error::Result;
use crate::risk::RiskManager;
use crate::scanner::ScannerService;
use crate::utils::{init_runtime_paths, load_secrets, normalize_config_paths, spawn_command_poller};
use crate::AppState;

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("mexc_trading_bot=info".parse().expect("valid log directive")),
        )
        .try_init();
}

pub async fn run() -> Result<()> {
    init_runtime_paths();
    let mut loaded = AppConfig::load()?;
    normalize_config_paths(&mut loaded);
    let config: crate::config::SharedAppConfig = Arc::new(RwLock::new(loaded));
    let config_snap = config.read().unwrap().clone();
    let db = Arc::new(Database::connect(&config_snap.storage.sqlite_path).await?);
    db.migrate(config_snap.execution.paper_initial_equity).await?;
    db.sync_paper_equity_if_unused(config_snap.execution.paper_initial_equity)
        .await?;

    let risk = Arc::new(AsyncRwLock::new(
        RiskManager::new(config.clone(), db.clone()).await?,
    ));
    let secrets = Arc::new(AsyncRwLock::new(load_secrets()));

    let scanner = Arc::new(AsyncRwLock::new(ScannerService::new(
        config.clone(),
        db.clone(),
        risk.clone(),
        secrets.clone(),
    )?));

    let state = AppState {
        config: config.clone(),
        db,
        risk,
        scanner,
        secrets,
        snapshot_cache: Arc::new(AsyncRwLock::new(serde_json::json!({}))),
    };

    // Refresh the UI snapshot cache in the background so WS/HTTP handlers
    // never block on risk locks or SQLite contention during signal bursts.
    {
        let cache_state = Arc::new(state.clone());
        tokio::spawn(async move {
            loop {
                let snap = crate::api::handlers::build_live_snapshot(cache_state.clone()).await;
                *cache_state.snapshot_cache.write().await = snap;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }

    spawn_command_poller(Arc::new(state.clone()));

    let app = router(state);
    let addr: SocketAddr = format!("{}:{}", config_snap.server.host, config_snap.server.port)
        .parse()
        .expect("valid listen address");

    info!(
        "MEXC Trading Bot API listening on http://{} (UI: http://{}/ , trading.mode={})",
        addr, addr, config_snap.trading.mode
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Default API URL for desktop shell.
pub fn default_api_url() -> String {
    "http://127.0.0.1:8001".into()
}

/// Package version of this binary (used to detect stale servers on :8001).
pub fn package_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Fetch `/health` JSON from the local API, if it responds.
pub fn fetch_api_health() -> Option<serde_json::Value> {
    let url = format!("{}/health", default_api_url());
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?
        .get(&url)
        .send()
        .ok()
        .filter(|r| r.status().is_success())?
        .json()
        .ok()
}

/// True when something is already serving the bot API (e.g. `cargo run`).
pub fn is_api_reachable() -> bool {
    fetch_api_health().is_some()
}

/// True when the process on :8001 reports the same package version as this binary.
pub fn is_same_version_api_reachable() -> bool {
    let Some(health) = fetch_api_health() else {
        return false;
    };
    let remote = health
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    !remote.is_empty() && remote == package_version()
}

/// Version string reported by whatever is currently on :8001 (for diagnostics).
pub fn remote_api_version() -> Option<String> {
    fetch_api_health()?
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Block until `/health` responds or attempts are exhausted.
pub fn wait_for_api(timeout_attempts: u32, interval_ms: u64) -> bool {
    for _ in 0..timeout_attempts {
        if is_api_reachable() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
    }
    is_api_reachable()
}
