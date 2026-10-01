//! Latest Ergo node release from GitHub, to flag outdated nodes (#16).

use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::monitor::Shared;

const LATEST_URL: &str = "https://api.github.com/repos/ergoplatform/ergo/releases/latest";
/// Releases are rare and the unauthenticated GitHub API allows 60 calls an hour.
const CHECK_EVERY: StdDuration = StdDuration::from_secs(6 * 3600);
const RETRY_AFTER: StdDuration = StdDuration::from_secs(30 * 60);

#[derive(Debug, Clone, Serialize)]
pub struct LatestRelease {
    /// Without the leading `v`, e.g. `6.0.7`.
    pub version: String,
    pub url: String,
    pub checked_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

async fn fetch(client: &reqwest::Client) -> Result<LatestRelease, String> {
    let r = client
        .get(LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json::<GithubRelease>()
        .await
        .map_err(|e| format!("unexpected response: {e}"))?;
    Ok(LatestRelease {
        version: r.tag_name.trim_start_matches('v').to_string(),
        url: r.html_url,
        checked_at: Utc::now(),
    })
}

pub async fn run(client: reqwest::Client, shared: Shared) {
    loop {
        let wait = match fetch(&client).await {
            Ok(latest) => {
                tracing::info!("latest Ergo node release: {}", latest.version);
                shared.write().await.latest_release = Some(latest);
                CHECK_EVERY
            }
            Err(e) => {
                tracing::warn!("could not check the latest Ergo release: {e}");
                RETRY_AFTER
            }
        };
        tokio::time::sleep(wait).await;
    }
}

/// Numeric parts of a version: `6.0.7` and `v6.0.7-SNAPSHOT` -> [6, 0, 7].
fn parts(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split('.')
        .map_while(|p| {
            let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        })
        .collect()
}

/// `Some(true)` when `running` is older than `latest`; `None` when either
/// can't be read as a version.
pub fn is_outdated(running: &str, latest: &str) -> Option<bool> {
    let (mut a, mut b) = (parts(running), parts(latest));
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    Some(a < b)
}

#[cfg(test)]
mod tests {
    use super::is_outdated;

    #[test]
    fn compares_versions() {
        assert_eq!(is_outdated("6.0.3", "6.0.7"), Some(true));
        assert_eq!(is_outdated("6.0.7", "6.0.7"), Some(false));
        assert_eq!(is_outdated("5.0.22", "6.0.7"), Some(true));
        // Numeric, not string, comparison.
        assert_eq!(is_outdated("6.0.10", "6.0.7"), Some(false));
        // Newer than the latest release (a release candidate) is not outdated.
        assert_eq!(is_outdated("6.1.0-RC1", "6.0.7"), Some(false));
        assert_eq!(is_outdated("6.0", "6.0.1"), Some(true));
        assert_eq!(is_outdated("v6.0.7", "6.0.7"), Some(false));
        assert_eq!(is_outdated("unknown", "6.0.7"), None);
    }
}
