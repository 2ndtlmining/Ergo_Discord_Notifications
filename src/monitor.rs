//! The polling loop: explorers + nodes -> shared state -> alerts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::alerts::{human_duration, Event, Tracker};
use crate::config::Config;
use crate::discord::{self, Discord, Embed};
use crate::explorer::{self, ExplorerState};
use crate::node::{self, Health};

/// Consecutive polls a new condition must be seen before alerting.
const CONFIRM_AFTER: u32 = 2;
const EXPLORERS_KEY: &str = "__explorers";

pub type Shared = Arc<RwLock<AppState>>;

#[derive(Debug, Clone, Serialize)]
pub struct AppState {
    pub schema_version: u32,
    pub commit: &'static str,
    pub generated_at: Option<DateTime<Utc>>,
    pub reference: Reference,
    pub summary: Summary,
    pub nodes: Vec<NodeState>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Reference {
    pub height: Option<u64>,
    pub sources: Vec<ExplorerState>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub total: usize,
    pub ok: usize,
    pub behind: usize,
    pub down: usize,
    pub syncing: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeState {
    pub id: String,
    pub name: String,
    pub url: String,
    pub wallet_address: Option<String>,
    /// ok | behind | down | syncing | unknown
    pub status: &'static str,
    /// Finer grained than `status`: adds `indexer-behind`.
    pub condition: &'static str,
    pub detail: String,
    pub status_since: Option<DateTime<Utc>>,
    pub full_height: Option<u64>,
    pub headers_height: Option<u64>,
    pub indexed_height: Option<u64>,
    pub full_lag: Option<u64>,
    pub indexed_lag: Option<u64>,
    pub sync_progress: Option<f64>,
    pub peers: Option<u32>,
    pub version: Option<String>,
    pub is_mining: bool,
    pub is_explorer: bool,
    pub latency_ms: Option<u64>,
    pub last_ok: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub runbook: Option<&'static str>,
}

impl AppState {
    pub fn new(commit: &'static str) -> Self {
        Self {
            schema_version: 1,
            commit,
            generated_at: None,
            reference: Reference::default(),
            summary: Summary::default(),
            nodes: Vec::new(),
        }
    }
}

pub async fn run(config: Config, client: reqwest::Client, discord: Discord, shared: Shared) {
    let mut trackers: HashMap<String, Tracker> = HashMap::new();
    let mut interval = tokio::time::interval(StdDuration::from_secs(config.node_poll_seconds));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut first = true;

    loop {
        interval.tick().await;
        let previous = shared.read().await.clone();
        let mut state = poll(&config, &client, &previous).await;
        let alerts = evaluate(&config, &mut state, &mut trackers, Utc::now());
        *shared.write().await = state.clone();

        if first {
            first = false;
            discord.send(&[startup_summary(&state)], false).await;
        }
        for (embed, ping) in alerts {
            discord.send(&[embed], ping).await;
        }
    }
}

async fn poll(config: &Config, client: &reqwest::Client, previous: &AppState) -> AppState {
    let now = Utc::now();
    let explorers = [
        ("mainnet", config.explorer_mainnet_api.clone()),
        ("p2p", config.explorer_p2p_api.clone()),
    ];
    // Spawn everything first so all requests run concurrently.
    let explorer_tasks: Vec<_> = explorers
        .iter()
        .map(|(_, url)| {
            let (c, u) = (client.clone(), url.clone());
            tokio::spawn(async move { explorer::fetch_height(&c, &u).await })
        })
        .collect();
    let node_tasks: Vec<_> = config
        .nodes
        .iter()
        .map(|n| {
            let (c, u) = (client.clone(), n.url.clone());
            tokio::spawn(async move { node::probe(&c, &u).await })
        })
        .collect();

    let mut sources = Vec::new();
    for ((name, url), task) in explorers.into_iter().zip(explorer_tasks) {
        let result = task.await.unwrap_or_else(|e| Err(e.to_string()));
        let prev = previous.reference.sources.iter().find(|s| s.name == name);
        sources.push(ExplorerState {
            name: name.into(),
            ok: result.is_ok(),
            height: result
                .as_ref()
                .ok()
                .copied()
                .or(prev.and_then(|p| p.height)),
            error: result.as_ref().err().cloned(),
            last_ok: if result.is_ok() {
                Some(now)
            } else {
                prev.and_then(|p| p.last_ok)
            },
            url,
        });
    }
    let reference = explorer::reference_height(&sources);

    let mut nodes = Vec::new();
    for (cfg, task) in config.nodes.iter().zip(node_tasks) {
        let probe = task
            .await
            .unwrap_or_else(|e| Err(node::DownReason::Unreachable(e.to_string())));
        let health = node::classify(&probe, reference, config.lag_threshold_blocks);
        let prev = previous.nodes.iter().find(|p| p.id == cfg.id);
        let ok = probe.as_ref().ok();
        let info = ok.map(|p| &p.info);
        let full = info.and_then(|i| i.full_height);
        let indexed = ok.and_then(|p| p.indexed_height);
        nodes.push(NodeState {
            id: cfg.id.clone(),
            name: cfg.name.clone(),
            url: cfg.url.clone(),
            wallet_address: cfg.wallet_address.clone(),
            status: match health {
                Health::Ok => "ok",
                Health::Behind { .. } | Health::IndexerBehind { .. } => "behind",
                Health::Syncing { .. } => "syncing",
                Health::Down(_) => "down",
                Health::Unknown => "unknown",
            },
            condition: health.condition(),
            detail: health.detail(),
            status_since: None,
            full_height: full.or(prev.and_then(|p| p.full_height)),
            headers_height: info.and_then(|i| i.headers_height),
            indexed_height: indexed,
            full_lag: reference.zip(full).map(|(r, f)| r.saturating_sub(f)),
            indexed_lag: reference.zip(indexed).map(|(r, i)| r.saturating_sub(i)),
            sync_progress: match health {
                Health::Syncing { progress, .. } => Some(progress),
                _ => None,
            },
            peers: info.and_then(|i| i.peers_count),
            version: info.and_then(|i| i.app_version.clone()),
            is_mining: info.is_some_and(|i| i.is_mining),
            is_explorer: info.is_some_and(|i| i.is_explorer),
            latency_ms: ok.map(|p| p.latency_ms),
            last_ok: if ok.is_some() {
                Some(now)
            } else {
                prev.and_then(|p| p.last_ok)
            },
            last_error: probe.as_ref().err().map(|e| e.to_string()),
            runbook: health.runbook(),
        });
    }

    let count = |s: &str| nodes.iter().filter(|n| n.status == s).count();
    let summary = Summary {
        total: nodes.len(),
        ok: count("ok"),
        behind: count("behind"),
        down: count("down"),
        syncing: count("syncing"),
        unknown: count("unknown"),
    };
    AppState {
        generated_at: Some(now),
        reference: Reference {
            height: reference,
            sources,
        },
        summary,
        nodes,
        ..previous.clone()
    }
}

/// Feeds every subject's condition to its tracker and returns the alerts to send.
fn evaluate(
    config: &Config,
    state: &mut AppState,
    trackers: &mut HashMap<String, Tracker>,
    now: DateTime<Utc>,
) -> Vec<(Embed, bool)> {
    let cooldown = Duration::minutes(config.alert_cooldown_minutes as i64);
    let mut out = Vec::new();

    let explorers_ok = state.reference.height.is_some();
    let tracker = trackers.entry(EXPLORERS_KEY.into()).or_default();
    let cond = if explorers_ok { "ok" } else { "unreachable" };
    if let Some(Event::Changed { .. }) =
        tracker.observe(cond, now, CONFIRM_AFTER, Duration::days(365))
    {
        out.push(if explorers_ok {
            (
                embed(
                    "✅ Explorers reachable again",
                    "Lag checks have resumed.",
                    discord::GREEN,
                ),
                false,
            )
        } else {
            (
                embed(
                    "⚠️ Both explorers unreachable",
                    "Mainnet and P2P explorer APIs are not responding. Node lag alerts are \
                     paused until one is back; down alerts still work.\n\
                     Runbook: `docs/runbooks/explorers-unreachable.md`",
                    discord::GREY,
                ),
                true,
            )
        });
    }

    for n in &mut state.nodes {
        // Without a reference we can't tell OK from behind; keep the last confirmed state.
        let tracker = trackers.entry(n.id.clone()).or_default();
        if n.condition != "unknown" {
            let remind = if n.condition == "syncing" {
                Duration::hours(24)
            } else {
                cooldown
            };
            match tracker.observe(n.condition, now, CONFIRM_AFTER, remind) {
                Some(Event::Changed { from, to, lasted }) if to == "ok" => out.push((
                    embed(
                        &format!("✅ {}: recovered", n.name),
                        &format!(
                            "Back in sync{} after being **{}** for {}.",
                            n.full_height
                                .map(|h| format!(" at height {h}"))
                                .unwrap_or_default(),
                            label(&from).to_lowercase(),
                            human_duration(lasted)
                        ),
                        discord::GREEN,
                    ),
                    false,
                )),
                Some(Event::Changed { from, .. }) => {
                    let mut body = problem_body(n, state.reference.height);
                    if from != "ok" {
                        body.push_str(&format!("\nPreviously: {}", label(&from).to_lowercase()));
                    }
                    out.push((
                        embed(
                            &format!("{} {}: {}", emoji(n.condition), n.name, label(n.condition)),
                            &body,
                            color(n.condition),
                        ),
                        true,
                    ));
                }
                Some(Event::Reminder { since, .. }) => out.push((
                    embed(
                        &format!(
                            "{} {}: still {}",
                            emoji(n.condition),
                            n.name,
                            label(n.condition).to_lowercase()
                        ),
                        &format!(
                            "For {}.\n{}",
                            human_duration(now - since),
                            problem_body(n, state.reference.height)
                        ),
                        color(n.condition),
                    ),
                    n.condition != "syncing",
                )),
                None => {}
            }
        }
        n.status_since = tracker.since();
    }
    out
}

fn problem_body(n: &NodeState, tip: Option<u64>) -> String {
    let mut s = format!("**{}**", n.detail);
    let h = |v: Option<u64>| v.map(|x| x.to_string()).unwrap_or("?".into());
    if n.condition != "down" {
        s.push_str(&format!(
            "\nNode height {} · Indexed {} · Chain tip {}",
            h(n.full_height),
            n.indexed_height
                .map(|x| x.to_string())
                .unwrap_or("n/a".into()),
            h(tip)
        ));
    }
    s.push_str(&format!("\nNode: `{}`", n.url));
    if let Some(rb) = n.runbook {
        s.push_str(&format!("\nRunbook: `{rb}`"));
    }
    s
}

fn startup_summary(state: &AppState) -> Embed {
    let mut lines = vec![format!(
        "Chain tip: **{}**",
        state
            .reference
            .height
            .map(|h| h.to_string())
            .unwrap_or("unknown (explorers unreachable)".into())
    )];
    for n in &state.nodes {
        lines.push(format!(
            "{} **{}**: {}",
            emoji(n.condition),
            n.name,
            n.detail
        ));
    }
    let healthy = state.summary.ok == state.summary.total;
    embed(
        &format!(
            "Ergo Monitor started · {}/{} nodes healthy",
            state.summary.ok, state.summary.total
        ),
        &lines.join("\n"),
        if healthy {
            discord::GREEN
        } else {
            discord::AMBER
        },
    )
}

fn embed(title: &str, description: &str, color: u32) -> Embed {
    Embed {
        title: title.into(),
        description: description.into(),
        color,
    }
}

fn emoji(condition: &str) -> &'static str {
    match condition {
        "ok" => "✅",
        "down" => "🔴",
        "behind" => "🟠",
        "indexer-behind" => "🟡",
        "syncing" => "🔵",
        _ => "⚪",
    }
}

fn label(condition: &str) -> &'static str {
    match condition {
        "ok" => "OK",
        "down" => "DOWN",
        "behind" => "BEHIND",
        "indexer-behind" => "INDEXER BEHIND",
        "syncing" => "SYNCING",
        "unreachable" => "UNREACHABLE",
        _ => "UNKNOWN",
    }
}

fn color(condition: &str) -> u32 {
    match condition {
        "ok" => discord::GREEN,
        "down" => discord::RED,
        "behind" | "indexer-behind" => discord::AMBER,
        "syncing" => discord::BLUE,
        _ => discord::GREY,
    }
}
