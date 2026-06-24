//! MEXC Trading Bot — API server entry point.

use mexc_trading_bot::error::Result;
use mexc_trading_bot::server;

#[tokio::main]
async fn main() -> Result<()> {
    server::init_tracing();
    server::run().await
}
