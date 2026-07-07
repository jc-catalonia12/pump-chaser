//! AI orchestration layer: local-LLM market regime (Phase 4) and the unified
//! decision core (Phase 5).

pub mod assistant;
pub mod decision;
pub mod llm_regime;

pub use assistant::{AssistantChatRequest, AssistantChatResponse, ChatMessage};
pub use decision::{DecisionEngine, DecisionInputs, TradeDecision};
pub use llm_regime::{LlmRegimeService, RegimeInputs};
