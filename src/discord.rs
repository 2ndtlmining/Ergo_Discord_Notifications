//! Discord webhook delivery with embeds and rate-limit handling (#5).

use std::time::Duration;

use serde_json::json;

/// Embed side-bar colours, keyed by the same condition names as the icons
/// in `assets/discord/` (see scripts/gen_icons.py).
pub fn color(condition: &str) -> u32 {
    match condition {
        "ok" => 0x30A46C,
        "down" => 0xE5484D,
        "behind" | "indexer-behind" => 0xF5A524,
        "syncing" => 0x3E8BFF,
        "received" => 0xFF5A1F,
        _ => 0x8B8D98,
    }
}

#[derive(Debug, Clone)]
pub struct Embed {
    /// Condition name; selects the author icon and side-bar colour.
    pub icon: String,
    pub author: String,
    pub title: String,
    pub description: String,
    pub fields: Vec<(String, String, bool)>,
}

impl Embed {
    pub fn new(icon: &str, author: &str, title: &str) -> Self {
        Self {
            icon: icon.into(),
            author: author.into(),
            title: title.into(),
            description: String::new(),
            fields: Vec::new(),
        }
    }

    pub fn description(mut self, text: &str) -> Self {
        self.description = text.into();
        self
    }

    pub fn field(mut self, name: &str, value: &str, inline: bool) -> Self {
        // Discord limits: 25 fields, 256-char names, 1024-char values.
        if self.fields.len() < 25 {
            let value: String = value.chars().take(1024).collect();
            self.fields
                .push((name.chars().take(256).collect(), value, inline));
        }
        self
    }
}

#[derive(Clone)]
pub struct Discord {
    client: reqwest::Client,
    webhook_url: Option<String>,
    mention: Option<String>,
    icon_base_url: String,
}

impl Discord {
    pub fn new(
        client: reqwest::Client,
        webhook_url: Option<String>,
        mention: Option<String>,
        icon_base_url: String,
    ) -> Self {
        Self {
            client,
            webhook_url,
            mention,
            icon_base_url,
        }
    }

    /// Sends up to 10 embeds (Discord's limit per message). `ping` adds the
    /// configured @mention; used for problems, not for summaries.
    pub async fn send(&self, embeds: &[Embed], ping: bool) {
        let Some(url) = &self.webhook_url else {
            for e in embeds {
                tracing::info!(
                    "(discord disabled) [{}] {} | {} | {}",
                    e.icon,
                    e.author,
                    e.title,
                    e.description
                );
            }
            return;
        };
        for chunk in embeds.chunks(10) {
            let mention = self.mention.as_ref().filter(|_| ping);
            let body = json!({
                "username": "Ergo Monitor",
                "avatar_url": format!("{}/avatar.png", self.icon_base_url),
                "content": mention.map(|id| format!("<@{id}>")).unwrap_or_default(),
                "allowed_mentions": { "users": mention.into_iter().collect::<Vec<_>>() },
                "embeds": chunk.iter().map(|e| json!({
                    "author": {
                        "name": e.author,
                        "icon_url": format!("{}/{}.png", self.icon_base_url, e.icon),
                    },
                    "title": e.title,
                    "description": e.description,
                    "color": color(&e.icon),
                    "fields": e.fields.iter().map(|(name, value, inline)| json!({
                        "name": name, "value": value, "inline": inline,
                    })).collect::<Vec<_>>(),
                    "footer": { "text": "Ergo Monitor" },
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
