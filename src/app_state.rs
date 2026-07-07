use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::SharedAppConfig;
use crate::db::Database;
use crate::risk::RiskManager;
use crate::scanner::ScannerService;
use crate::utils::UserSecrets;

#[derive(Clone)]
pub struct AppState {
    pub config: SharedAppConfig,
    pub db: Arc<Database>,
    pub risk: Arc<tokio::sync::RwLock<RiskManager>>,
    pub scanner: Arc<tokio::sync::RwLock<ScannerService>>,
    pub secrets: Arc<RwLock<UserSecrets>>,
    /// Cached `/live/snapshot` JSON refreshed by a background task so the UI
    /// WebSocket loop never blocks behind concurrent signal execution / DB writes.
    pub snapshot_cache: Arc<tokio::sync::RwLock<serde_json::Value>>,
}
