//! Reference chain height from the public explorers (#3).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct ExplorerState {
    pub name: String,
    pub url: String,
    pub height: Option<u64>,
    pub ok: bool,
    pub error: Option<String>,
    pub last_ok: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct NetworkState {
    height: u64,
}

pub async fn fetch_height(client: &reqwest::Client, base: &str) -> Result<u64, String> {
    let resp = client
        .get(format!("{base}/api/v1/networkState"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    resp.json::<NetworkState>()
        .await
        .map(|s| s.height)
        .map_err(|e| format!("unexpected response: {e}"))
}

/// The chain tip nodes are compared against: the highest height any
/// reachable explorer reports, or `None` when none are reachable.
pub fn reference_height(sources: &[ExplorerState]) -> Option<u64> {
    sources
        .iter()
        .filter(|s| s.ok)
        .filter_map(|s| s.height)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(height: Option<u64>, ok: bool) -> ExplorerState {
        ExplorerState {
            name: "x".into(),
            url: "x".into(),
            height,
            ok,
            error: None,
            last_ok: None,
        }
    }

    #[test]
    fn reference_is_max_of_reachable_explorers() {
        assert_eq!(
            reference_height(&[src(Some(100), true), src(Some(102), true)]),
            Some(102)
        );
        // A down explorer's stale height is ignored.
        assert_eq!(
            reference_height(&[src(Some(200), false), src(Some(100), true)]),
            Some(100)
        );
        assert_eq!(
            reference_height(&[src(None, false), src(None, false)]),
            None
        );
    }
}
