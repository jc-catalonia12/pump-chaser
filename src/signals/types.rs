use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SignalStrength {
    Weak,
    Moderate,
    Strong,
}

impl SignalStrength {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Weak => "weak",
            Self::Moderate => "moderate",
            Self::Strong => "strong",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PumpSignal {
    pub symbol: String,
    pub strategy: String,
    pub composite_score: f64,
    pub strength: SignalStrength,
    pub last_price: f64,
    pub price_change_pct: f64,
    pub volume_surge_ratio: f64,
    pub confluence_count: u32,
    pub confluences: Vec<String>,
    pub confluence_details: Vec<Value>,
    pub setup_probability_pct: f64,
    pub suggested_risk_pct: f64,
    pub suggested_leverage: u32,
    pub zone_score: f64,
    pub zone_message: String,
    pub sizing_tier: String,
    pub message: String,
    pub generated_at: DateTime<Utc>,
    pub signal_id: Option<i64>,
    pub projected_stop_loss: f64,
    pub projected_take_profits: Vec<f64>,
    pub tp_close_fractions: Vec<f64>,
    /// Feature vector captured at signal time; used to train the online model
    /// once the trade resolves. Empty until the ML pipeline enriches the signal.
    #[serde(default)]
    pub ml_features: Vec<f64>,
}

impl PumpSignal {
    pub fn to_payload(&self) -> Value {
        json!({
            "symbol": self.symbol,
            "strategy": self.strategy,
            "composite_score": self.composite_score,
            "strength": self.strength.as_str(),
            "last_price": self.last_price,
            "price_change_pct": self.price_change_pct,
            "volume_surge_ratio": self.volume_surge_ratio,
            "confluence_count": self.confluence_count,
            "confluences": self.confluences,
            "confluence_details": self.confluence_details,
            "setup_probability_pct": self.setup_probability_pct,
            "suggested_risk_pct": self.suggested_risk_pct,
            "suggested_leverage": self.suggested_leverage,
            "zone_score": self.zone_score,
            "zone_message": self.zone_message,
            "sizing_tier": self.sizing_tier,
            "message": self.message,
            "generated_at": self.generated_at.to_rfc3339(),
            "signal_id": self.signal_id,
            "projected_stop_loss": self.projected_stop_loss,
            "projected_take_profits": self.projected_take_profits,
            "tp_close_fractions": self.tp_close_fractions,
            "ml_features": self.ml_features,
        })
    }
}
