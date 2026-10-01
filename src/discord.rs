//! Discord webhook delivery with embeds and rate-limit handling (#5).
//!
//! The poll loops never wait on Discord (#24): `Discord::send` only queues a
//! message, and one worker task posts them in order, retrying through rate
//! limits and outages. Each alert's delivery result is written back to the
//! shared state for `/api/alerts` and the dashboard.

use std::collections::VecDeque;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::monitor::Shared;

/// Messages kept while Discord is unreachable; the oldest are dropped first.
const MAX_QUEUED: usize = 50;
/// A message still undelivered after this long is dropped as no longer useful.
const GIVE_UP_AFTER_MINUTES: i64 = 60;
const RETRY_QUEUE_EVERY: Duration = Duration::from_secs(30);

/// `1882085` -> `1,882,085`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Whether Discord delivery is working, for `/api/status` and the dashboard (#29).
#[derive(Debug, Clone, Default, Serialize)]
pub struct DiscordStatus {
    pub enabled: bool,
    pub last_sent: Option<DateTime<Utc>>,
    /// Set while delivery is failing; cleared by the next successful send.
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    /// Messages waiting to be delivered.
    pub queued: usize,
}

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

/// Delivery state of a recorded alert, as shown in `/api/alerts`.
pub mod delivery {
    pub const PENDING: &str = "pending";
    pub const SENT: &str = "sent";
    pub const FAILED: &str = "failed";
    /// No webhook configured; the alert was only logged.
    pub const OFF: &str = "off";
}

/// One Discord message: up to 10 embeds and an optional @mention.
#[derive(Debug)]
struct Outgoing {
    embeds: Vec<Embed>,
    ping: bool,
    /// Alert records (`AlertRecord::id`) to mark sent or failed.
    alert_ids: Vec<u64>,
    at: DateTime<Utc>,
}

/// Cheap handle the poll loops use to queue messages.
#[derive(Clone)]
pub struct Discord {
    tx: Option<mpsc::UnboundedSender<Outgoing>>,
}

/// Posts the queued messages; started with `Worker::run`.
pub struct Worker {
    rx: Option<mpsc::UnboundedReceiver<Outgoing>>,
    hook: Option<Webhook>,
    shared: Shared,
}

struct Webhook {
    client: reqwest::Client,
    url: String,
    mention: Option<String>,
    icon_base_url: String,
    /// Embed titles link here when set (`DASHBOARD_URL`).
    dashboard_url: Option<String>,
    footer: String,
}

impl Webhook {
    fn from_config(client: reqwest::Client, config: &Config, commit: &str) -> Option<Self> {
        Some(Self {
            client,
            url: config.discord_webhook_url.clone()?,
            mention: config.discord_user.clone(),
            icon_base_url: config.discord_icon_base_url.clone(),
            dashboard_url: config.dashboard_url.clone(),
            footer: format!("Ergo Monitor · {commit}"),
        })
    }
}

/// `ergo-monitor test-alert`: posts one sample alert right away and reports
/// the result, to check the webhook without waiting for a real problem (#29).
pub async fn test_alert(
    client: reqwest::Client,
    config: &Config,
    commit: &str,
) -> Result<(), String> {
    let hook = Webhook::from_config(client, config, commit)
        .ok_or("DISCORD_WEBHOOK_URL is not set in .env")?;
    let embed = Embed::new("ok", "Test alert", "Discord alerts are working").description(
        "Sent by `ergo-monitor test-alert`. Real alerts look like this, with an @mention for problems.",
    );
    let msg = Outgoing {
        embeds: vec![embed],
        ping: true,
        alert_ids: vec![],
        at: Utc::now(),
    };
    hook.post(&msg).await.map_err(|f| match f {
        Failure::Permanent(e) => {
            format!("{e}: Discord rejected the webhook; check DISCORD_WEBHOOK_URL")
        }
        Failure::Transient(e) => format!("{e}: couldn't reach Discord"),
    })
}

enum Failure {
    /// Bad webhook or payload: retrying won't help.
    Permanent(String),
    /// Rate limit, Discord outage or network trouble: retry later.
    Transient(String),
}

impl Discord {
    pub fn new(
        client: reqwest::Client,
        config: &Config,
        commit: &str,
        shared: Shared,
    ) -> (Self, Worker) {
        let hook = Webhook::from_config(client, config, commit);
        let (tx, rx) = if hook.is_some() {
            let (tx, rx) = mpsc::unbounded_channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        (Self { tx }, Worker { rx, hook, shared })
    }

    /// The delivery state a newly recorded alert starts in.
    pub fn initial_delivery(&self) -> &'static str {
        if self.tx.is_some() {
            delivery::PENDING
        } else {
            delivery::OFF
        }
    }

    /// Queues embeds as one or more messages (10 embeds each) and returns
    /// immediately. `ping` adds the configured @mention.
    pub fn send(&self, embeds: Vec<Embed>, alert_ids: Vec<u64>, ping: bool) {
        let Some(tx) = &self.tx else {
            for e in &embeds {
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
        let at = Utc::now();
        let mut ids = alert_ids.into_iter();
        for chunk in embeds.chunks(10) {
            let msg = Outgoing {
                embeds: chunk.to_vec(),
                ping,
                alert_ids: ids.by_ref().take(chunk.len()).collect(),
                at,
            };
            if tx.send(msg).is_err() {
                tracing::error!("discord worker has stopped; alert not sent");
            }
        }
    }

    /// Queues recorded alerts `(embed, ping, alert id)`: the pinged ones in
    /// one message, the rest in another.
    pub fn send_alerts(&self, alerts: Vec<(Embed, bool, u64)>) {
        let (pinged, quiet): (Vec<_>, Vec<_>) = alerts.into_iter().partition(|a| a.1);
        for (group, ping) in [(pinged, true), (quiet, false)] {
            if !group.is_empty() {
                let (embeds, ids) = group.into_iter().map(|(e, _, id)| (e, id)).unzip();
                self.send(embeds, ids, ping);
            }
        }
    }
}

impl Worker {
    pub async fn run(self) {
        let (Some(mut rx), Some(hook)) = (self.rx, self.hook) else {
            // Discord disabled: nothing to deliver, but stay alive for the supervisor.
            return std::future::pending().await;
        };
        let mut queue: VecDeque<Outgoing> = VecDeque::new();
        loop {
            if queue.is_empty() {
                match rx.recv().await {
                    Some(m) => queue.push_back(m),
                    None => return,
                }
            }
            while let Ok(m) = rx.try_recv() {
                queue.push_back(m);
            }
            while queue.len() > MAX_QUEUED {
                if let Some(dropped) = queue.pop_front() {
                    tracing::error!("discord queue full; dropped an alert from {}", dropped.at);
                    let err = Some("queue full".to_string());
                    report(
                        &self.shared,
                        &dropped.alert_ids,
                        delivery::FAILED,
                        err,
                        queue.len(),
                    )
                    .await;
                }
            }

            let Some(msg) = queue.front() else { continue };
            match hook.post(msg).await {
                Ok(()) => {
                    report(
                        &self.shared,
                        &msg.alert_ids,
                        delivery::SENT,
                        None,
                        queue.len() - 1,
                    )
                    .await;
                    queue.pop_front();
                }
                Err(Failure::Permanent(e)) => {
                    tracing::error!("discord webhook rejected the message, not retrying: {e}");
                    let err = Some(format!("Discord rejected the webhook ({e})"));
                    report(
                        &self.shared,
                        &msg.alert_ids,
                        delivery::FAILED,
                        err,
                        queue.len() - 1,
                    )
                    .await;
                    queue.pop_front();
                }
                Err(Failure::Transient(e)) => {
                    if Utc::now() - msg.at > chrono::Duration::minutes(GIVE_UP_AFTER_MINUTES) {
                        tracing::error!(
                            "discord unreachable for over {GIVE_UP_AFTER_MINUTES} minutes, gave up on an alert: {e}"
                        );
                        let err = Some(format!("Discord unreachable ({e})"));
                        report(
                            &self.shared,
                            &msg.alert_ids,
                            delivery::FAILED,
                            err,
                            queue.len() - 1,
                        )
                        .await;
                        queue.pop_front();
                    } else {
                        let err = Some(format!("Discord unreachable ({e}); retrying"));
                        report(&self.shared, &[], delivery::PENDING, err, queue.len()).await;
                        tracing::warn!(
                            "discord unreachable ({e}); {} message(s) queued, retrying in {}s",
                            queue.len(),
                            RETRY_QUEUE_EVERY.as_secs()
                        );
                        tokio::time::sleep(RETRY_QUEUE_EVERY).await;
                    }
                }
            }
        }
    }
}

/// Records a delivery outcome on the alerts and in the Discord status.
async fn report(
    shared: &Shared,
    ids: &[u64],
    state: &'static str,
    error: Option<String>,
    queued: usize,
) {
    let now = Utc::now();
    let mut s = shared.write().await;
    for a in s.alerts.iter_mut().filter(|a| ids.contains(&a.id)) {
        a.delivery = state;
    }
    let d = &mut s.discord;
    d.queued = queued;
    match error {
        None => {
            d.last_sent = Some(now);
            d.last_error = None;
            d.last_error_at = None;
        }
        Some(e) => {
            d.last_error = Some(e);
            d.last_error_at = Some(now);
        }
    }
}

impl Webhook {
    fn body(&self, msg: &Outgoing) -> serde_json::Value {
        let mention = self.mention.as_ref().filter(|_| msg.ping);
        json!({
            "username": "Ergo Monitor",
            "avatar_url": format!("{}/avatar.png", self.icon_base_url),
            "content": mention.map(|id| format!("<@{id}>")).unwrap_or_default(),
            "allowed_mentions": { "users": mention.into_iter().collect::<Vec<_>>() },
            "embeds": msg.embeds.iter().map(|e| json!({
                "author": {
                    "name": e.author,
                    "icon_url": format!("{}/{}.png", self.icon_base_url, e.icon),
                },
                "title": e.title,
                "url": self.dashboard_url,
                "description": e.description,
                "color": color(&e.icon),
                "fields": e.fields.iter().map(|(name, value, inline)| json!({
                    "name": name, "value": value, "inline": inline,
                })).collect::<Vec<_>>(),
                "footer": { "text": self.footer },
                // When the alert happened, not when a retry finally got through.
                "timestamp": msg.at.to_rfc3339(),
            })).collect::<Vec<_>>(),
        })
    }

    /// A few quick attempts; longer outages are retried from the worker's queue.
    async fn post(&self, msg: &Outgoing) -> Result<(), Failure> {
        let body = self.body(msg);
        let mut last = String::new();
        for attempt in 1..=4u32 {
            match self.client.post(&self.url).json(&body).send().await {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) if r.status().as_u16() == 429 => {
                    let wait = r
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|v| v["retry_after"].as_f64())
                        .unwrap_or(2.0)
                        .clamp(0.5, 60.0);
                    tracing::warn!("discord rate limited, retrying in {wait:.1}s");
                    last = "rate limited (HTTP 429)".into();
                    tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                    continue;
                }
                Ok(r) if r.status().is_server_error() => last = format!("HTTP {}", r.status()),
                Ok(r) => return Err(Failure::Permanent(format!("HTTP {}", r.status()))),
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
        }
        Err(Failure::Transient(last))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::{http::StatusCode, routing::post, Json, Router};
    use tokio::sync::RwLock;

    use super::*;
    use crate::monitor::{record_alert, AppState, Settings};

    /// A fake webhook that answers with `codes` in turn and records each body.
    async fn fake_webhook(
        codes: Vec<u16>,
    ) -> (String, Arc<std::sync::Mutex<Vec<serde_json::Value>>>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let b = bodies.clone();
        let app = Router::new().route(
            "/hook",
            post(move |Json(body): Json<serde_json::Value>| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                b.lock().unwrap().push(body);
                let code = codes.get(n).copied().unwrap_or(204);
                async move { StatusCode::from_u16(code).unwrap() }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, bodies)
    }

    async fn deliver(
        codes: Vec<u16>,
        alerts: Vec<(Embed, bool)>,
    ) -> (Vec<&'static str>, Vec<serde_json::Value>) {
        let (url, bodies) = fake_webhook(codes).await;
        let settings = Settings {
            lag_threshold_blocks: 5,
            node_poll_seconds: 30,
            wallet_poll_seconds: 300,
        };
        let shared: Shared = Arc::new(RwLock::new(AppState::new("test", settings)));
        let mut config = Config::from_vars([
            ("DISCORD_WEBHOOK_URL".to_string(), url),
            ("DISCORD_USER".to_string(), "123".to_string()),
        ])
        .unwrap();
        config.discord_icon_base_url = "https://icons".into();
        let (discord, worker) =
            Discord::new(reqwest::Client::new(), &config, "test", shared.clone());
        tokio::spawn(worker.run());
        let queued = {
            let mut s = shared.write().await;
            alerts
                .into_iter()
                .map(|(e, ping)| {
                    let id = record_alert(&mut s, &e, discord.initial_delivery());
                    (e, ping, id)
                })
                .collect()
        };
        discord.send_alerts(queued);
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let s = shared.read().await;
            if s.alerts.iter().all(|a| a.delivery != delivery::PENDING) {
                let states = s.alerts.iter().rev().map(|a| a.delivery).collect();
                return (states, bodies.lock().unwrap().clone());
            }
        }
        panic!("alerts still pending");
    }

    #[tokio::test]
    async fn retries_server_errors_and_batches_by_ping() {
        let (states, bodies) = deliver(
            vec![500],
            vec![
                (Embed::new("down", "Down", "A"), true),
                (Embed::new("down", "Down", "B"), true),
                (Embed::new("ok", "Recovered", "C"), false),
            ],
        )
        .await;
        assert_eq!(states, ["sent", "sent", "sent"]);
        // 1 failed attempt, then the pinged message (2 embeds), then the quiet one.
        assert_eq!(bodies.len(), 3);
        assert_eq!(bodies[1]["embeds"].as_array().unwrap().len(), 2);
        assert_eq!(bodies[1]["content"], "<@123>");
        assert_eq!(bodies[2]["content"], "");
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_882_085), "1,882,085");
    }

    #[tokio::test]
    async fn rejected_webhook_marks_failed_without_retrying() {
        let (states, bodies) =
            deliver(vec![404], vec![(Embed::new("down", "Down", "A"), true)]).await;
        assert_eq!(states, ["failed"]);
        assert_eq!(bodies.len(), 1);
    }
}
