use serde_json::{json, Value};
use sqlx::Row;

use crate::db::Database;
use crate::error::Result;

impl Database {
    pub async fn insert_news_item(
        &self,
        source: &str,
        title: &str,
        url: &str,
        score: f64,
        symbols: &[String],
        published_at: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let symbols_json = serde_json::to_string(symbols).unwrap_or_else(|_| "[]".into());
        sqlx::query(
            "INSERT OR IGNORE INTO news_items (source, title, url, score, symbols, published_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(source)
        .bind(title)
        .bind(url)
        .bind(score)
        .bind(symbols_json)
        .bind(published_at)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn get_recent_news(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT source, title, url, score, symbols, published_at, created_at \
             FROM news_items \
             WHERE id IN ( \
               SELECT MIN(id) FROM news_items \
               GROUP BY COALESCE(NULLIF(trim(url), ''), source || '::' || title) \
             ) \
             ORDER BY published_at DESC, id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let symbols: String = row.try_get("symbols").unwrap_or_else(|_| "[]".into());
                let symbols_v: Value = serde_json::from_str(&symbols).unwrap_or(json!([]));
                json!({
                    "source": row.try_get::<String, _>("source").unwrap_or_default(),
                    "title": row.try_get::<String, _>("title").unwrap_or_default(),
                    "url": row.try_get::<String, _>("url").unwrap_or_default(),
                    "score": row.try_get::<f64, _>("score").unwrap_or(0.0),
                    "symbols": symbols_v,
                    "published_at": row.try_get::<String, _>("published_at").unwrap_or_default(),
                    "created_at": row.try_get::<String, _>("created_at").unwrap_or_default(),
                })
            })
            .collect())
    }

    pub async fn prune_old_news(&self, keep_days: i64) -> Result<()> {
        sqlx::query("DELETE FROM news_items WHERE created_at < datetime('now', ?||' days')")
            .bind(format!("-{}", keep_days))
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
