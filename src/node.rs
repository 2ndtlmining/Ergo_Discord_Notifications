//! Polling a single Ergo node and deciding its health (#4).

use std::fmt;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// A node whose block processing trails its own headers by more than this
/// is doing an initial sync rather than falling behind (~1 day of blocks).
pub const SYNCING_GAP: u64 = 720;
/// Nodes are on the LAN; a healthy one answers in milliseconds.
const NODE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub full_height: Option<u64>,
    pub headers_height: Option<u64>,
    pub peers_count: Option<u32>,
    pub app_version: Option<String>,
    #[serde(default)]
    pub is_mining: bool,
    #[serde(default)]
    pub is_explorer: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexedHeight {
    indexed_height: u64,
}

#[derive(Debug, Clone)]
pub struct Probe {
    pub info: NodeInfo,
    /// `None` when the node has no extra index (`extraIndex = false`), or
    /// when the indexer request failed (see `indexer_error`).
    pub indexed_height: Option<u64>,
    /// The indexer endpoint failed for a reason other than "not enabled".
    pub indexer_error: Option<String>,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownReason {
    Refused,
    Timeout,
    Unreachable(String),
    Http(u16),
    BadResponse(String),
}

impl fmt::Display for DownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused => write!(
                f,
                "Connection refused: host is up but the node API is not listening (node process stopped?)"
            ),
            Self::Timeout => write!(f, "Timed out: host or network unreachable, or node hung"),
            Self::Unreachable(e) => write!(f, "Could not connect: {e}"),
            Self::Http(code) => write!(f, "Node API returned HTTP {code}"),
            Self::BadResponse(e) => write!(f, "Unexpected response from node API: {e}"),
        }
    }
}

fn down_reason(e: &reqwest::Error) -> DownReason {
    if e.is_timeout() {
        return DownReason::Timeout;
    }
    if let Some(status) = e.status() {
        return DownReason::Http(status.as_u16());
    }
    // The OS error ("Connection refused" / os error 111) is buried in the source chain.
    let mut chain = String::new();
    let mut source: Option<&dyn std::error::Error> = Some(e);
    while let Some(err) = source {
        chain.push_str(&err.to_string().to_lowercase());
        chain.push(' ');
        source = err.source();
    }
    if chain.contains("refused") {
        DownReason::Refused
    } else if e.is_decode() {
        DownReason::BadResponse(e.to_string())
    } else if chain.contains("timed out") {
        DownReason::Timeout
    } else {
        DownReason::Unreachable(e.to_string())
    }
}

pub async fn probe(client: &reqwest::Client, url: &str) -> Result<Probe, DownReason> {
    // Both requests at once, so a slow indexer doesn't delay the poll (#25).
    let info = async {
        let started = Instant::now();
        let info = client
            .get(format!("{url}/info"))
            .timeout(NODE_TIMEOUT)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| down_reason(&e))?
            .json::<NodeInfo>()
            .await
            .map_err(|e| DownReason::BadResponse(e.to_string()))?;
        Ok::<_, DownReason>((info, started.elapsed().as_millis() as u64))
    };
    let indexed = async {
        let resp = client
            .get(format!("{url}/blockchain/indexedHeight"))
            .timeout(NODE_TIMEOUT)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if indexer_disabled(resp.status().as_u16()) {
            return Ok(None);
        }
        resp.error_for_status()
            .map_err(|e| e.to_string())?
            .json::<IndexedHeight>()
            .await
            .map(|h| Some(h.indexed_height))
            .map_err(|e| format!("unexpected response: {e}"))
    };
    let (info, indexed) = tokio::join!(info, indexed);
    let (info, latency_ms) = info?;
    let (indexed_height, indexer_error) = match indexed {
        Ok(h) => (h, None),
        Err(e) => (None, Some(e)),
    };
    Ok(Probe {
        info,
        indexed_height,
        indexer_error,
        latency_ms,
    })
}

/// Nodes without `extraIndex` don't serve the indexer routes. Only these
/// statuses mean "no indexer"; timeouts and server errors are failures.
fn indexer_disabled(status: u16) -> bool {
    matches!(status, 400 | 404 | 501)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Health {
    Ok,
    /// Node's own block height is behind the reference.
    Behind {
        lag: u64,
    },
    /// Node is at the tip but its extra index is not.
    IndexerBehind {
        lag: u64,
    },
    /// Initial sync: headers are ahead of processed blocks.
    Syncing {
        progress: f64,
        remaining: u64,
    },
    Down(DownReason),
    /// Node is up but there is no reference height to compare with.
    Unknown,
}

impl Health {
    /// Stable key used for alert state tracking and the API `condition` field.
    pub fn condition(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Behind { .. } => "behind",
            Self::IndexerBehind { .. } => "indexer-behind",
            Self::Syncing { .. } => "syncing",
            Self::Down(_) => "down",
            Self::Unknown => "unknown",
        }
    }

    pub fn runbook(&self) -> Option<&'static str> {
        match self {
            Self::Behind { .. } | Self::Syncing { .. } => Some("docs/runbooks/node-behind.md"),
            Self::IndexerBehind { .. } => Some("docs/runbooks/indexer-behind.md"),
            Self::Down(_) => Some("docs/runbooks/node-down.md"),
            Self::Ok | Self::Unknown => None,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::Ok => "In sync".into(),
            Self::Behind { lag } => format!("Node is {lag} blocks behind the chain tip"),
            Self::IndexerBehind { lag } => {
                format!("Node is in sync, but its indexer is {lag} blocks behind")
            }
            Self::Syncing {
                progress,
                remaining,
            } => format!(
                "Initial sync {:.1}% complete, {remaining} blocks remaining",
                progress * 100.0
            ),
            Self::Down(reason) => reason.to_string(),
            Self::Unknown => "Up, but lag is unknown (explorers unreachable)".into(),
        }
    }
}

pub fn classify(
    probe: &Result<Probe, DownReason>,
    reference: Option<u64>,
    threshold: u64,
) -> Health {
    let p = match probe {
        Err(reason) => return Health::Down(reason.clone()),
        Ok(p) => p,
    };
    let Some(tip) = reference else {
        return Health::Unknown;
    };
    let full = p.info.full_height.unwrap_or(0);
    let headers = p.info.headers_height.unwrap_or(0);

    let lag = tip.saturating_sub(full);
    if lag > threshold {
        let gap = headers.saturating_sub(full);
        if gap > SYNCING_GAP {
            return Health::Syncing {
                // Against the chain tip: headers may still be downloading too.
                progress: full as f64 / tip.max(1) as f64,
                remaining: lag,
            };
        }
        return Health::Behind { lag };
    }
    match p.indexed_height {
        Some(indexed) if tip.saturating_sub(indexed) > threshold => Health::IndexerBehind {
            lag: tip.saturating_sub(indexed),
        },
        _ => Health::Ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(full: u64, headers: u64, indexed: Option<u64>) -> Result<Probe, DownReason> {
        Ok(Probe {
            info: NodeInfo {
                full_height: Some(full),
                headers_height: Some(headers),
                ..Default::default()
            },
            indexed_height: indexed,
            indexer_error: None,
            latency_ms: 5,
        })
    }

    #[test]
    fn classifies_real_world_cases() {
        let tip = Some(1_882_064);
        // Healthy, with and without extraIndex.
        assert_eq!(
            classify(&up(1_882_064, 1_882_064, Some(1_882_064)), tip, 5),
            Health::Ok
        );
        assert_eq!(
            classify(&up(1_882_060, 1_882_064, None), tip, 5),
            Health::Ok
        );
        // Hosted explorer: node in sync, indexer 433 behind.
        assert_eq!(
            classify(&up(1_882_064, 1_882_064, Some(1_881_631)), tip, 5),
            Health::IndexerBehind { lag: 433 }
        );
        // Storage Rent: headers at tip, blocks at 302k -> syncing.
        let h = classify(&up(302_360, 1_882_064, Some(302_362)), tip, 5);
        assert!(
            matches!(
                h,
                Health::Syncing {
                    remaining: 1_579_704,
                    ..
                }
            ),
            "{h:?}"
        );
        // Fresh resync: no blocks yet, headers still downloading.
        let fresh = Ok(Probe {
            info: NodeInfo {
                full_height: None,
                headers_height: Some(165_350),
                ..Default::default()
            },
            indexed_height: Some(0),
            indexer_error: None,
            latency_ms: 2,
        });
        assert_eq!(
            classify(&fresh, tip, 5),
            Health::Syncing {
                progress: 0.0,
                remaining: 1_882_064
            }
        );
        // Stuck node: headers not ahead either -> behind.
        assert_eq!(
            classify(&up(1_882_050, 1_882_050, None), tip, 5),
            Health::Behind { lag: 14 }
        );
        // Exactly at the threshold is still OK.
        assert_eq!(
            classify(&up(1_882_059, 1_882_064, None), tip, 5),
            Health::Ok
        );
    }

    #[test]
    fn only_not_found_means_no_indexer() {
        assert!(indexer_disabled(404) && indexer_disabled(400));
        assert!(!indexer_disabled(500) && !indexer_disabled(503) && !indexer_disabled(200));
    }

    #[test]
    fn down_and_unknown() {
        assert_eq!(
            classify(&Err(DownReason::Refused), Some(1), 5),
            Health::Down(DownReason::Refused)
        );
        assert_eq!(classify(&up(10, 10, None), None, 5), Health::Unknown);
    }

    #[test]
    fn parses_node_info() {
        let info: NodeInfo = serde_json::from_str(
            r#"{"name":"x","appVersion":"6.0.3","fullHeight":1882064,"headersHeight":1882064,
                "peersCount":30,"isMining":false,"isExplorer":true,"stateType":"utxo"}"#,
        )
        .unwrap();
        assert_eq!(info.full_height, Some(1_882_064));
        assert_eq!(info.peers_count, Some(30));
        assert!(info.is_explorer && !info.is_mining);
        // fullHeight is null on a node that has not processed any blocks yet.
        let fresh: NodeInfo = serde_json::from_str(r#"{"fullHeight":null}"#).unwrap();
        assert_eq!(fresh.full_height, None);
    }
}
