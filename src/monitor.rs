//! The polling loop: explorers + nodes -> shared state -> alerts.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::alerts::{human_duration, Event, Tracker};
use crate::config::Config;
use crate::discord::{thousands, Discord, DiscordStatus, Embed};
use crate::explorer::{self, ExplorerState};
use crate::node::{self, Health};
use crate::release::{self, LatestRelease};
use crate::wallet::WalletState;

/// Consecutive polls a new condition must be seen before alerting.
const CONFIRM_AFTER: u32 = 2;
const EXPLORERS_KEY: &str = "__explorers";
/// Runbook paths in alerts link here, so they open from a phone (#34).
const REPO_BLOB_URL: &str = "https://github.com/2ndtlmining/Ergo_Discord_Notifications/blob/main";

/// After a crash Docker restarts the same container, so a file in it survives
/// restarts but not a redeploy (which recreates the container). A startup
/// summary is skipped if the last one was this recent, so a crash loop
/// doesn't flood the channel (#27).
const SUMMARY_MIN_GAP_MINUTES: i64 = 10;

/// Whether to post the startup summary, recording the time when it does.
/// `STATE_DIR` unset (local runs, tests) means always post.
fn should_post_summary(dir: Option<&std::path::Path>, now: DateTime<Utc>) -> bool {
    let Some(dir) = dir else { return true };
    let file = dir.join("last-startup-summary");
    let last = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| s.trim().parse::<DateTime<Utc>>().ok());
    if let Some(last) = last {
        if now - last < Duration::minutes(SUMMARY_MIN_GAP_MINUTES) {
            tracing::warn!(
                "restarted within {SUMMARY_MIN_GAP_MINUTES} minutes of the last startup summary \
                 (last at {last}); not posting another. Check the logs above for why it restarted."
            );
            return false;
        }
    }
    if let Err(e) =
        std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&file, now.to_rfc3339()))
    {
        tracing::warn!(
            "could not record the startup summary time in {}: {e}",
            file.display()
        );
    }
    true
}

fn runbook_link(path: &str) -> String {
    format!("[{path}]({REPO_BLOB_URL}/{path})")
}

pub type Shared = Arc<RwLock<AppState>>;

#[derive(Debug, Clone, Serialize)]
pub struct AppState {
    pub schema_version: u32,
    pub commit: &'static str,
    pub started_at: DateTime<Utc>,
    pub generated_at: Option<DateTime<Utc>>,
    pub settings: Settings,
    pub reference: Reference,
    pub summary: Summary,
    pub nodes: Vec<NodeState>,
    pub wallets: Vec<WalletState>,
    /// Newest Ergo node release on GitHub; null until the first check succeeds.
    pub latest_release: Option<LatestRelease>,
    pub discord: DiscordStatus,
    /// Served separately at /api/alerts.
    #[serde(skip)]
    pub alerts: VecDeque<AlertRecord>,
    #[serde(skip)]
    next_alert_id: u64,
    /// False in preview mode, where the data is fixed and never refreshed.
    #[serde(skip)]
    pub live: bool,
}

/// The settings the dashboard and API clients need to interpret the data.
#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub lag_threshold_blocks: u64,
    pub node_poll_seconds: u64,
    pub wallet_poll_seconds: u64,
}

impl From<&Config> for Settings {
    fn from(c: &Config) -> Self {
        Self {
            lag_threshold_blocks: c.lag_threshold_blocks,
            node_poll_seconds: c.node_poll_seconds,
            wallet_poll_seconds: c.wallet_poll_seconds,
        }
    }
}

const MAX_ALERTS: usize = 100;

/// An alert as sent to Discord, kept in memory for the API and dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct AlertRecord {
    pub id: u64,
    pub at: DateTime<Utc>,
    /// Condition / icon name: ok, down, behind, indexer-behind, syncing, unreachable, received.
    pub kind: String,
    pub headline: String,
    pub subject: String,
    pub detail: String,
    /// Discord delivery: pending | sent | failed | off (no webhook configured).
    pub delivery: &'static str,
}

/// Keeps an alert for the API and dashboard; returns its id for delivery tracking.
pub fn record_alert(state: &mut AppState, embed: &Embed, delivery: &'static str) -> u64 {
    state.next_alert_id += 1;
    let id = state.next_alert_id;
    state.alerts.push_front(AlertRecord {
        id,
        delivery,
        at: Utc::now(),
        kind: embed.icon.clone(),
        headline: embed.author.clone(),
        subject: embed.title.clone(),
        detail: plain_text(&embed.description),
    });
    state.alerts.truncate(MAX_ALERTS);
    id
}

/// Discord markdown to plain text: drops `**` and turns `[text](url)` into `text`.
fn plain_text(md: &str) -> String {
    let mut out = String::new();
    let mut rest = md.replace("**", "");
    while let Some(open) = rest.find('[') {
        let Some(mid) = rest[open..].find("](").map(|i| open + i) else {
            break;
        };
        let Some(close) = rest[mid..].find(')').map(|i| mid + i) else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..mid]);
        rest = rest[close + 1..].to_string();
    }
    out.push_str(&rest);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn startup_summary_is_rate_limited() {
        use chrono::{Duration, Utc};
        let dir = std::env::temp_dir().join(format!("ergo-monitor-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t0 = Utc::now();
        assert!(super::should_post_summary(Some(&dir), t0));
        // A crash-loop restart a minute later: skipped.
        assert!(!super::should_post_summary(
            Some(&dir),
            t0 + Duration::minutes(1)
        ));
        // Long after: posted again.
        assert!(super::should_post_summary(
            Some(&dir),
            t0 + Duration::minutes(11)
        ));
        // No state dir: always post.
        assert!(super::should_post_summary(None, t0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn freshness_follows_the_poll_interval() {
        use super::{AppState, Settings};
        use chrono::Duration;
        let settings = Settings {
            lag_threshold_blocks: 5,
            node_poll_seconds: 30,
            wallet_poll_seconds: 300,
        };
        let mut s = AppState::new("test", settings);
        let now = s.started_at;
        // Before the first poll: fresh during the grace period (3 x 30s + 30s).
        assert!(s.is_fresh(now + Duration::seconds(100)));
        assert!(!s.is_fresh(now + Duration::seconds(130)));
        s.generated_at = Some(now + Duration::seconds(200));
        assert!(s.is_fresh(now + Duration::seconds(300)));
        assert!(!s.is_fresh(now + Duration::seconds(400)));
        // Preview data never refreshes and is always "fresh".
        s.live = false;
        assert!(s.is_fresh(now + Duration::days(1)));
    }

    #[test]
    fn strips_discord_markdown() {
        assert_eq!(
            super::plain_text("**Down** see [View transaction](https://x/y) now"),
            "Down see View transaction now"
        );
        assert_eq!(super::plain_text("no links"), "no links");
    }
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
    /// Newest release of the node's own line (e.g. 6.1.x), else the stable release.
    pub latest_version: Option<String>,
    pub latest_version_url: Option<String>,
    /// True when `version` is older than `latest_version`; null if either is unknown.
    pub version_outdated: Option<bool>,
    pub is_mining: bool,
    pub is_explorer: bool,
    pub latency_ms: Option<u64>,
    pub last_ok: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub runbook: Option<&'static str>,
}

impl AppState {
    pub fn new(commit: &'static str, settings: Settings) -> Self {
        Self {
            schema_version: 1,
            commit,
            started_at: Utc::now(),
            generated_at: None,
            settings,
            reference: Reference::default(),
            summary: Summary::default(),
            nodes: Vec::new(),
            wallets: Vec::new(),
            latest_release: None,
            discord: DiscordStatus::default(),
            alerts: VecDeque::new(),
            next_alert_id: 0,
            live: true,
        }
    }

    /// Whether the node poll loop is still producing fresh data (#23): false
    /// when the last poll is older than three intervals, or when no poll has
    /// finished that long after startup.
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        if !self.live {
            return true;
        }
        let limit = Duration::seconds(3 * self.settings.node_poll_seconds as i64 + 30);
        now - self.generated_at.unwrap_or(self.started_at) <= limit
    }
}

pub async fn run(
    config: Config,
    client: reqwest::Client,
    discord: Discord,
    shared: Shared,
) -> &'static str {
    let mut trackers: HashMap<String, Tracker> = HashMap::new();
    let mut interval = tokio::time::interval(StdDuration::from_secs(config.node_poll_seconds));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut first = true;
    let mut last = Poll::default();

    loop {
        interval.tick().await;
        // Only the release info comes from the shared state; the previous poll
        // is kept here, so the whole state (alerts, wallets) isn't cloned (#36).
        let latest = shared.read().await.latest_release.clone();
        let mut state = poll(&config, &client, &last, latest.as_ref()).await;
        let alerts = evaluate(&config, &mut state, &mut trackers, Utc::now());
        let queued = {
            // Only touch the fields this loop owns; the wallet loop writes `wallets`.
            let mut s = shared.write().await;
            s.generated_at = Some(state.generated_at);
            s.reference = state.reference.clone();
            s.summary = state.summary.clone();
            s.nodes = state.nodes.clone();
            alerts
                .into_iter()
                .map(|(embed, ping)| {
                    let id = record_alert(&mut s, &embed, discord.initial_delivery());
                    (embed, ping, id)
                })
                .collect::<Vec<_>>()
        };

        // Queued, not awaited: a slow or rate-limited Discord never delays polling (#24).
        if first {
            first = false;
            // The first check doubles as a startup reachability report in the logs (#30).
            for n in &state.nodes {
                tracing::info!("first check: node [{}] {}: {}", n.id, n.condition, n.detail);
            }
            let dir = std::env::var_os("STATE_DIR").map(std::path::PathBuf::from);
            if should_post_summary(dir.as_deref(), Utc::now()) {
                discord.send(vec![startup_summary(&state)], vec![], false);
            }
        }
        discord.send_alerts(queued);
        last = state;
    }
}

/// What one poll produces: the parts of `AppState` the node loop owns.
#[derive(Default)]
struct Poll {
    generated_at: DateTime<Utc>,
    reference: Reference,
    summary: Summary,
    nodes: Vec<NodeState>,
}

async fn poll(
    config: &Config,
    client: &reqwest::Client,
    previous: &Poll,
    latest_release: Option<&LatestRelease>,
) -> Poll {
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
        let mut probe = task
            .await
            .unwrap_or_else(|e| Err(node::DownReason::Unreachable(e.to_string())));
        let prev = previous.nodes.iter().find(|p| p.id == cfg.id);
        // A failed indexer request isn't "no indexer": judge on the last known
        // indexed height rather than flipping to OK (#25).
        if let Ok(p) = &mut probe {
            if p.indexer_error.is_some() {
                p.indexed_height = prev.and_then(|n| n.indexed_height);
            }
        }
        let health = node::classify(&probe, reference, config.lag_threshold_blocks);
        let ok = probe.as_ref().ok();
        let info = ok.map(|p| &p.info);
        let full = info.and_then(|i| i.full_height);
        let indexed = ok.and_then(|p| p.indexed_height);
        let version = info
            .and_then(|i| i.app_version.clone())
            .or(prev.and_then(|p| p.version.clone()));
        let target = version
            .as_deref()
            .zip(latest_release)
            .map(|(v, latest)| release::target_for(v, latest));
        let version_outdated = version
            .as_deref()
            .zip(target)
            .and_then(|(v, (t, _))| release::is_outdated(v, t));
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
            version,
            latest_version: target.map(|(v, _)| v.to_string()),
            latest_version_url: target.map(|(_, u)| u.to_string()),
            version_outdated,
            is_mining: info.is_some_and(|i| i.is_mining),
            is_explorer: info.is_some_and(|i| i.is_explorer),
            latency_ms: ok.map(|p| p.latency_ms),
            last_ok: if ok.is_some() {
                Some(now)
            } else {
                prev.and_then(|p| p.last_ok)
            },
            last_error: match &probe {
                Err(e) => Some(e.to_string()),
                Ok(p) => p.indexer_error.as_ref().map(|e| format!("Indexer: {e}")),
            },
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
    Poll {
        generated_at: now,
        reference: Reference {
            height: reference,
            sources,
        },
        summary,
        nodes,
    }
}

/// Feeds every subject's condition to its tracker and returns the alerts to send.
fn evaluate(
    config: &Config,
    state: &mut Poll,
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
                Embed::new("ok", "Explorers reachable", "Reference height restored")
                    .description("Lag checks have resumed."),
                false,
            )
        } else {
            (
                Embed::new(
                    "unreachable",
                    "Explorers unreachable",
                    "No reference height",
                )
                .description(
                    "Mainnet and P2P explorer APIs are not responding. Lag alerts are \
                         paused until one is back; down alerts still work.",
                )
                .field(
                    "Runbook",
                    &runbook_link("docs/runbooks/explorers-unreachable.md"),
                    false,
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
            let tip = state.reference.height;
            match tracker.observe(n.condition, now, CONFIRM_AFTER, remind) {
                Some(Event::Changed { from, to, lasted }) if to == "ok" => out.push((
                    Embed::new("ok", "Recovered", &n.name).description(&format!(
                        "Back in sync{} after being {} for {}.",
                        n.full_height
                            .map(|h| format!(" at height **{}**", thousands(h)))
                            .unwrap_or_default(),
                        label(&from).to_lowercase(),
                        human_duration(lasted)
                    )),
                    false,
                )),
                Some(Event::Changed { from, .. }) => {
                    let mut e = problem_embed(n, label(n.condition), tip);
                    if from != "ok" {
                        e = e.field("Previously", label(&from), true);
                    }
                    out.push((e, true));
                }
                Some(Event::Reminder { since, .. }) => {
                    let author = format!(
                        "Still {} · {}",
                        label(n.condition).to_lowercase(),
                        human_duration(now - since)
                    );
                    out.push((problem_embed(n, &author, tip), n.condition != "syncing"));
                }
                None => {}
            }
        }
        n.status_since = tracker.since();
    }
    out
}

fn problem_embed(n: &NodeState, author: &str, tip: Option<u64>) -> Embed {
    let h = |v: Option<u64>| v.map(thousands).unwrap_or("n/a".into());
    let mut e = Embed::new(n.condition, author, &n.name).description(&format!("**{}**", n.detail));
    if n.condition != "down" {
        e = e
            .field("Node height", &h(n.full_height), true)
            .field("Indexed", &h(n.indexed_height), true)
            .field("Chain tip", &h(tip), true);
    }
    e = e.field("Endpoint", &format!("`{}`", n.url), false);
    if let Some(rb) = n.runbook {
        e = e.field("Runbook", &runbook_link(rb), false);
    }
    e
}

fn startup_summary(state: &Poll) -> Embed {
    let s = &state.summary;
    // The worst condition sets the icon, so a down node shows red, not yellow.
    let icon = ["down", "behind", "indexer-behind", "syncing", "unknown"]
        .into_iter()
        .find(|c| state.nodes.iter().any(|n| n.condition == *c))
        .unwrap_or("ok");
    let mut e = Embed::new(
        icon,
        "Ergo Monitor started",
        &format!("{} of {} nodes healthy", s.ok, s.total),
    )
    .description(&format!(
        "Chain tip **{}**",
        state
            .reference
            .height
            .map(thousands)
            .unwrap_or("unknown (explorers unreachable)".into())
    ));
    for n in &state.nodes {
        e = e.field(
            &n.name,
            &format!("`{}`  {}", label(n.condition), n.detail),
            false,
        );
    }
    e
}

fn label(condition: &str) -> &'static str {
    match condition {
        "ok" => "OK",
        "down" => "Down",
        "behind" => "Behind",
        "indexer-behind" => "Indexer behind",
        "syncing" => "Syncing",
        "unreachable" => "Unreachable",
        _ => "Unknown",
    }
}
