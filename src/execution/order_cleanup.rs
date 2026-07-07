//! Cancel dangling MEXC plan/stop orders after a position is closed.

use tracing::info;

use crate::exchange::MexcPrivateClient;

/// Best-effort cancel of all plan + stop/TP-SL orders for a symbol.
pub async fn cleanup_after_position_closed(
    client: &MexcPrivateClient,
    symbol: &str,
    exchange_position_id: Option<i64>,
) {
    if !client.has_credentials() {
        return;
    }
    let result = client
        .cleanup_symbol_orders(symbol, exchange_position_id)
        .await;
    info!("Order cleanup for {symbol}: {result}");
}
