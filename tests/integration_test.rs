//! Integration smoke test.

#[tokio::test]
async fn config_loads() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config/settings.yaml");
    std::env::set_var("MEXC_BOT_CONFIG", path.to_str().unwrap());
    let cfg = mexc_trading_bot::AppConfig::load().expect("load config");
    assert_eq!(cfg.trading.mode, "all");
    assert_eq!(cfg.risk.max_confluence_positions, 1);
    assert_eq!(cfg.risk.max_volume_pump_positions, 1);
    assert!(cfg.pump.enabled);
    assert_eq!(cfg.server.port, 8001);
}
