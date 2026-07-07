//! In-memory sentiment state with exponential decay.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;

use crate::config::SharedAppConfig;
use crate::db::Database;
use crate::sentiment::fetcher::SentimentFetcher;
use crate::sentiment::gate::symbol_base;

#[derive(Debug, Clone, Default)]
pub struct SentimentState {
    pub global_score: f64,
    pub fear_greed: Option<i64>,
    pub symbol_scores: HashMap<String, f64>,
    pub last_headlines: Vec<Value>,
    pub updated_at: String,
}

pub struct SentimentService {
    config: SharedAppConfig,
    db: Arc<Database>,
    fetcher: SentimentFetcher,
    state: RwLock<SentimentState>,
}

impl SentimentService {
    pub fn new(config: SharedAppConfig, db: Arc<Database>) -> Self {
        Self {
            config,
            db,
            fetcher: SentimentFetcher::new(),
            state: RwLock::new(SentimentState::default()),
        }
    }

    pub async fn snapshot(&self) -> SentimentState {
        self.state.read().await.clone()
    }

    pub async fn global_score(&self) -> f64 {
        self.state.read().await.global_score
    }

    pub async fn symbol_score(&self, symbol: &str) -> Option<f64> {
        let base = symbol_base(symbol);
        self.state.read().await.symbol_scores.get(&base).copied()
    }

    pub async fn status_json(&self) -> Value {
        let s = self.state.read().await;
        json!({
            "global_score": (s.global_score * 1000.0).round() / 1000.0,
            "fear_greed": s.fear_greed,
            "symbol_scores": s.symbol_scores,
            "headline_count": s.last_headlines.len(),
            "updated_at": s.updated_at,
            "headlines": s.last_headlines.iter().take(20).collect::<Vec<_>>(),
        })
    }

    pub async fn poll_once(&self) {
        let cfg = self.config.read().unwrap().sentiment.clone();
        if !cfg.enabled {
            return;
        }

        let headlines = self.fetcher.fetch_all().await;
        let fear_greed = self.fetcher.fetch_fear_greed().await;
        let now = Utc::now();
        let half_life = cfg.decay_half_life_sec.max(60.0);
        let ln2 = 2.0_f64.ln();

        let mut global = 0.0f64;
        let mut global_w = 0.0f64;
        let mut per_symbol: HashMap<String, (f64, f64)> = HashMap::new();
        let mut stored: Vec<Value> = Vec::new();
        let mut seen_headlines: HashSet<String> = HashSet::new();

        for h in &headlines {
            let dedup_key = headline_dedup_key(&h.url, &h.source, &h.title);
            if !seen_headlines.insert(dedup_key) {
                continue;
            }

            let age_sec = parse_age_sec(&h.published_at, now);
            let weight = (-ln2 * age_sec / half_life).exp();
            global += h.score * weight;
            global_w += weight;

            for sym in &h.symbols {
                let e = per_symbol.entry(sym.clone()).or_insert((0.0, 0.0));
                e.0 += h.score * weight;
                e.1 += weight;
            }

            let _ = self
                .db
                .insert_news_item(&h.source, &h.title, &h.url, h.score, &h.symbols, &h.published_at)
                .await;

            stored.push(json!({
                "source": h.source,
                "title": h.title,
                "url": h.url,
                "score": (h.score * 1000.0).round() / 1000.0,
                "symbols": h.symbols,
                "published_at": h.published_at,
            }));
        }

        stored.sort_by(|a, b| {
            let ta = a
                .get("published_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp())
                .unwrap_or(0);
            let tb = b
                .get("published_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp())
                .unwrap_or(0);
            tb.cmp(&ta)
        });

        let global_score = if global_w > 0.0 {
            (global / global_w).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let mut symbol_scores = HashMap::new();
        for (sym, (sum, w)) in per_symbol {
            if w > 0.0 {
                symbol_scores.insert(sym, (sum / w).clamp(-1.0, 1.0));
            }
        }

        {
            let mut s = self.state.write().await;
            s.global_score = global_score;
            s.fear_greed = fear_greed;
            s.symbol_scores = symbol_scores;
            s.last_headlines = stored;
            s.updated_at = now.to_rfc3339();
        }

        info!(
            "Sentiment updated: global={global_score:.3} fear_greed={fear_greed:?} symbols={}",
            self.state.read().await.symbol_scores.len()
        );
    }

    pub async fn run_loop(self: Arc<Self>) {
        loop {
            let interval = self
                .config
                .read()
                .unwrap()
                .sentiment
                .poll_interval_sec
                .max(60);
            self.poll_once().await;
            tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
        }
    }
}

fn parse_age_sec(published_at: &str, now: chrono::DateTime<Utc>) -> f64 {
    chrono::DateTime::parse_from_rfc3339(published_at)
        .map(|d| now.signed_duration_since(d.with_timezone(&Utc)).num_seconds().max(0) as f64)
        .unwrap_or(0.0)
}

fn headline_dedup_key(url: &str, source: &str, title: &str) -> String {
    let trimmed = url.trim();
    if !trimmed.is_empty() {
        trimmed.to_string()
    } else {
        format!("{source}::{title}")
    }
}
