//! Discord webhook delivery with embeds and rate-limit handling (#5).

use std::time::Duration;

use serde_json::json;

pub const RED: u32 = 0xE5484D;
pub const AMBER: u32 = 0xF5A524;
pub const BLUE: u32 = 0x3E8BFF;
pub const GREEN: u32 = 0x30A46C;
pub const GREY: u32 = 0x8B8D98;

#[derive(Debug, Clone)]
pub struct Embed {
    pub title: String,
    pub description: String,
    pub color: u32,
}

#[derive(Clone)]
pub struct Discord {
    client: reqwest::Client,
    webhook_url: Option<String>,
    mention: Option<String>,
}

impl Discord {
    pub fn new(
        client: reqwest::Client,
        webhook_url: Option<String>,
        mention: Option<String>,
    ) -> Self {
        Self {
            client,
            webhook_url,
            mention,
        }
    }

    /// Sends up to 10 embeds (Discord's limit per message). `ping` adds the
    /// configured @mention; used for problems, not for summaries.
    pub async fn send(&self, embeds: &[Embed], ping: bool) {
        let Some(url) = &self.webhook_url else {
            for e in embeds {
                tracing::info!("(discord disabled) {}: {}", e.title, e.description);
            }
            return;
        };
        for chunk in embeds.chunks(10) {
            let mention = self.mention.as_ref().filter(|_| ping);
            let body = json!({
                "username": "Ergo Monitor",
                "content": mention.map(|id| format!("<@{id}>")).unwrap_or_default(),
                "allowed_mentions": { "users": mention.into_iter().collect::<Vec<_>>() },
                "embeds": chunk.iter().map(|e| json!({
                    "title": e.title,
                    "description": e.description,
                    "color": e.color,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })).collect::<Vec<_>>(),
            });
            self.post(url, &body).await;
        }
    }

    async fn post(&self, url: &str, body: &serde_json::Value) {
        for attempt in 1..=3 {
            match self.client.post(url).json(body).send().await {
                Ok(r) if r.status().is_success() => return,
                Ok(r) if r.status().as_u16() == 429 => {
                    let wait = r
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|v| v["retry_after"].as_f64())
                        .unwrap_or(2.0);
                    tracing::warn!("discord rate limited, retrying in {wait:.1}s");
                    tokio::time::sleep(Duration::from_secs_f64(wait.min(60.0))).await;
                }
                Ok(r) => {
                    tracing::error!("discord webhook failed: HTTP {}", r.status());
                    return;
                }
                Err(e) => {
                    tracing::error!("discord webhook error (attempt {attempt}): {e}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
}
