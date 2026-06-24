//! Live MEXC API smoke test (requires network).

#[tokio::test]
#[ignore = "hits live MEXC API"]
async fn mexc_ping_live() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config/settings.yaml");
    std::env::set_var("MEXC_BOT_CONFIG", path.to_str().unwrap());
    let cfg = std::sync::Arc::new(std::sync::RwLock::new(
        mexc_trading_bot::AppConfig::load().expect("load config"),
    ));
    let client = mexc_trading_bot::exchange::MexcClient::new(cfg).expect("client");
    assert!(client.ping().await, "MEXC contract ping should succeed");
}

#[tokio::test]
#[ignore = "hits live MEXC API"]
async fn mexc_discover_symbols_live() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config/settings.yaml");
    std::env::set_var("MEXC_BOT_CONFIG", path.to_str().unwrap());
    let cfg = std::sync::Arc::new(std::sync::RwLock::new(
        mexc_trading_bot::AppConfig::load().expect("load config"),
    ));
    let client = mexc_trading_bot::exchange::MexcClient::new(cfg).expect("client");
    let symbols = client.get_symbols().await.expect("symbols");
    assert!(!symbols.is_empty());
    assert!(symbols.iter().any(|s| s == "BTC_USDT"));
}
