use thiserror::Error;

pub type Result<T> = std::result::Result<T, BotError>;

#[derive(Debug, Error)]
pub enum BotError {
    #[error("configuration: {0}")]
    Config(String),

    #[error("database: {0}")]
    Database(#[from] sqlx::Error),

    #[error("exchange: {0}")]
    Exchange(String),

    #[error("risk blocked: {0}")]
    RiskBlocked(String),

    #[error("execution: {0}")]
    Execution(String),

    #[error("signal: {0}")]
    Signal(String),

    #[error("ml: {0}")]
    Ml(String),

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}
