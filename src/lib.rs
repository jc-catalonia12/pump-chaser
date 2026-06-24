//! MEXC Trading Bot — Rust core library.
//!
//! Hybrid migration from the Python Pump Chaser stack:
//! Rust handles scanner, risk, signals, execution, and API;
//! Python ML training remains optional via PyO3 (`ml-python` feature).

pub mod api;
pub mod app_state;
pub mod backtest;
pub mod charts;
pub mod config;
pub mod db;
pub mod error;
pub mod exchange;
pub mod execution;
pub mod learning;
pub mod ml;
pub mod models;
pub mod risk;
pub mod scanner;
pub mod server;
pub mod signals;
pub mod user_settings;
pub mod utils;
pub mod version;

pub use app_state::AppState;
pub use config::AppConfig;
pub use error::{BotError, Result};
