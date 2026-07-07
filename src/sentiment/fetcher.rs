//! Fetch free news/sentiment sources (no API keys).

use chrono::Utc;
use serde_json::Value;
use tracing::debug;

use crate::sentiment::scorer::{extract_symbols, score_text};

const USER_AGENT: &str = "MexcTradingBot/1.0 (sentiment; +https://github.com)";

#[derive(Debug, Clone)]
pub struct HeadlineItem {
    pub source: String,
    pub title: String,
    pub url: String,
    pub score: f64,
    pub symbols: Vec<String>,
    pub published_at: String,
}

pub struct SentimentFetcher {
    client: reqwest::Client,
}

impl SentimentFetcher {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }

    pub async fn fetch_all(&self) -> Vec<HeadlineItem> {
        let mut items = Vec::new();
        items.extend(self.fetch_rss("cointelegraph", "https://cointelegraph.com/rss").await);
        items.extend(self.fetch_rss("coindesk", "https://www.coindesk.com/arc/outboundfeeds/rss/").await);
        items.extend(self.fetch_rss("decrypt", "https://decrypt.co/feed").await);
        items.extend(self.fetch_reddit().await);
        items
    }

    pub async fn fetch_fear_greed(&self) -> Option<i64> {
        let url = "https://api.alternative.me/fng/?limit=1&format=json";
        let resp = self.client.get(url).send().await.ok()?;
        let body: Value = resp.json().await.ok()?;
        body.get("data")?
            .as_array()?
            .first()?
            .get("value")?
            .as_str()?
            .parse()
            .ok()
    }

    async fn fetch_rss(&self, source: &str, url: &str) -> Vec<HeadlineItem> {
        let feed = match self.client.get(url).send().await {
            Ok(r) => match r.bytes().await {
                Ok(b) => feed_rs::parser::parse(&b[..]),
                Err(e) => {
                    debug!("RSS bytes {source}: {e}");
                    return vec![];
                }
            },
            Err(e) => {
                debug!("RSS fetch {source}: {e}");
                return vec![];
            }
        };
        let feed = match feed {
            Ok(f) => f,
            Err(e) => {
                debug!("RSS parse {source}: {e}");
                return vec![];
            }
        };
        feed.entries
            .into_iter()
            .take(25)
            .filter_map(|entry| {
                let title = entry.title.map(|t| t.content).unwrap_or_default();
                if title.is_empty() {
                    return None;
                }
                let url = entry
                    .links
                    .first()
                    .map(|l| l.href.clone())
                    .unwrap_or_default();
                let published_at = entry
                    .published
                    .or(entry.updated)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_else(|| Utc::now().to_rfc3339());
                let score = score_text(&title);
                let symbols = extract_symbols(&title);
                Some(HeadlineItem {
                    source: source.into(),
                    title,
                    url,
                    score,
                    symbols,
                    published_at,
                })
            })
            .collect()
    }

    async fn fetch_reddit(&self) -> Vec<HeadlineItem> {
        let url = "https://www.reddit.com/r/CryptoCurrency/hot.json?limit=15";
        let resp = match self.client.get(url).header("Accept", "application/json").send().await {
            Ok(r) => r,
            Err(e) => {
                debug!("Reddit fetch: {e}");
                return vec![];
            }
        };
        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                debug!("Reddit parse: {e}");
                return vec![];
            }
        };
        let mut out = Vec::new();
        if let Some(children) = body
            .get("data")
            .and_then(|d| d.get("children"))
            .and_then(|c| c.as_array())
        {
            for child in children {
                let data = match child.get("data") {
                    Some(d) => d,
                    None => continue,
                };
                let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if title.is_empty() {
                    continue;
                }
                let url = data
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let published_at = data
                    .get("created_utc")
                    .and_then(|v| v.as_f64())
                    .map(|ts| {
                        chrono::DateTime::from_timestamp(ts as i64, 0)
                            .map(|d| d.to_rfc3339())
                            .unwrap_or_else(|| Utc::now().to_rfc3339())
                    })
                    .unwrap_or_else(|| Utc::now().to_rfc3339());
                let score = score_text(&title);
                let symbols = extract_symbols(&title);
                out.push(HeadlineItem {
                    source: "reddit".into(),
                    title,
                    url,
                    score,
                    symbols,
                    published_at,
                });
            }
        }
        out
    }
}

impl Default for SentimentFetcher {
    fn default() -> Self {
        Self::new()
    }
}
