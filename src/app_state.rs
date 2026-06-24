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
}
