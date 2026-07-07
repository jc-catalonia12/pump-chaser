pub mod fetcher;
pub mod gate;
pub mod scorer;
pub mod state;

pub use gate::{sentiment_allows, SentimentGateResult, symbol_base};
pub use state::{SentimentService, SentimentState};
