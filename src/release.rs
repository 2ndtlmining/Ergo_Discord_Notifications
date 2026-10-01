//! Latest Ergo node releases from GitHub, to flag outdated nodes (#16).
//!
//! Ergo ships parallel release lines on the same day, e.g. 6.0.7 (marked
//! stable) and 6.1.7 (marked pre-release), so a node is compared with the
//! newest release of its own `major.minor` line, not just GitHub's "latest".

use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::monitor::Shared;

const RELEASES_URL: &str = "https://api.github.com/repos/ergoplatform/ergo/releases?per_page=50";
/// Releases are rare and the unauthenticated GitHub API allows 60 calls an hour.
const CHECK_EVERY: StdDuration = StdDuration::from_secs(6 * 3600);
const RETRY_AFTER: StdDuration = StdDuration::from_secs(30 * 60);

#[derive(Debug, Clone, Serialize)]
pub struct Release {
    /// Without the leading `v`, e.g. `6.0.7`.
    pub version: String,
    pub url: String,
    /// GitHub's pre-release flag (Ergo uses it for the newer line).
    pub prerelease: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatestRelease {
    /// Newest release GitHub marks stable.
    pub version: String,
    pub url: String,
    /// Newest release of each `major.minor` line, newest line first.
    pub lines: Vec<Release>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

/// `v6.0.7` -> [6, 0, 7]. Release candidates and other suffixed tags
/// (`v6.0.4RC2`, `v6.5.0-RC3`) are not final releases and give `None`.
fn release_parts(tag: &str) -> Option<[u64; 3]> {
    let mut it = tag.trim_start_matches('v').split('.');
    let mut out = [0; 3];
    for slot in &mut out {
        let p = it.next()?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = p.parse().ok()?;
    }
    it.next().is_none().then_some(out)
}

fn summarize(releases: Vec<GithubRelease>) -> Option<LatestRelease> {
    let mut finals: Vec<([u64; 3], GithubRelease)> = releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| release_parts(&r.tag_name).map(|p| (p, r)))
        .collect();
    finals.sort_by_key(|(p, _)| std::cmp::Reverse(*p));
    let as_release = |r: &GithubRelease| Release {
        version: r.tag_name.trim_start_matches('v').to_string(),
        url: r.html_url.clone(),
        prerelease: r.prerelease,
    };
    let stable = finals.iter().find(|(_, r)| !r.prerelease)?;
    let mut lines: Vec<Release> = Vec::new();
    let mut last_line = None;
    for (p, r) in &finals {
        if last_line != Some((p[0], p[1])) {
            last_line = Some((p[0], p[1]));
            lines.push(as_release(r));
        }
    }
    let stable = as_release(&stable.1);
    Some(LatestRelease {
        version: stable.version,
        url: stable.url,
        lines,
        checked_at: Utc::now(),
    })
}

async fn fetch(client: &reqwest::Client) -> Result<LatestRelease, String> {
    let releases = client
        .get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json::<Vec<GithubRelease>>()
        .await
        .map_err(|e| format!("unexpected response: {e}"))?;
    summarize(releases).ok_or_else(|| "no stable release found".into())
}

pub async fn run(client: reqwest::Client, shared: Shared) -> &'static str {
    loop {
        let wait = match fetch(&client).await {
            Ok(latest) => {
                let lines: Vec<_> = latest.lines.iter().map(|l| l.version.as_str()).collect();
                tracing::info!(
                    "latest Ergo node release: {} (newest per line: {})",
                    latest.version,
                    lines.join(", ")
                );
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

/// Numeric parts of a running version: `6.0.7` and `v6.0.7-SNAPSHOT` -> [6, 0, 7].
fn parts(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split('.')
        .map_while(|p| {
            let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        })
        .collect()
}

/// The release a node running `running` should be on: the newest of its own
/// `major.minor` line, or the stable release when its line isn't published.
pub fn target_for<'a>(running: &str, latest: &'a LatestRelease) -> (&'a str, &'a str) {
    let p = parts(running);
    latest
        .lines
        .iter()
        .find(|l| {
            let lp = parts(&l.version);
            p.len() >= 2 && lp[..2] == p[..2]
        })
        .map(|l| (l.version.as_str(), l.url.as_str()))
        .unwrap_or((latest.version.as_str(), latest.url.as_str()))
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
    use super::*;

    #[test]
    fn compares_versions() {
        assert_eq!(is_outdated("6.0.3", "6.0.7"), Some(true));
        assert_eq!(is_outdated("6.0.7", "6.0.7"), Some(false));
        assert_eq!(is_outdated("5.0.22", "6.0.7"), Some(true));
        // Numeric, not string, comparison.
        assert_eq!(is_outdated("6.0.10", "6.0.7"), Some(false));
        assert_eq!(is_outdated("6.1.0-RC1", "6.0.7"), Some(false));
        assert_eq!(is_outdated("6.0", "6.0.1"), Some(true));
        assert_eq!(is_outdated("v6.0.7", "6.0.7"), Some(false));
        assert_eq!(is_outdated("unknown", "6.0.7"), None);
    }

    fn gh(tag: &str, prerelease: bool) -> GithubRelease {
        GithubRelease {
            tag_name: tag.into(),
            html_url: format!("https://github.com/ergoplatform/ergo/releases/tag/{tag}"),
            prerelease,
            draft: false,
        }
    }

    #[test]
    fn newest_release_per_line() {
        // Shape of the real GitHub list on 2026-10-02.
        let latest = summarize(vec![
            gh("v6.1.7", true),
            gh("v6.0.7", false),
            gh("v6.1.6", true),
            gh("v6.0.6", false),
            gh("v6.0.4RC2", true),
            gh("v6.5.0-RC3", true),
        ])
        .unwrap();
        assert_eq!(latest.version, "6.0.7");
        let lines: Vec<_> = latest.lines.iter().map(|l| l.version.as_str()).collect();
        assert_eq!(lines, ["6.1.7", "6.0.7"]);

        assert_eq!(target_for("6.1.6", &latest).0, "6.1.7");
        assert_eq!(target_for("6.1.7", &latest).0, "6.1.7");
        assert_eq!(target_for("6.0.3", &latest).0, "6.0.7");
        // A line with no final release (devnet 6.5 RCs) falls back to stable.
        assert_eq!(target_for("6.5.0-RC3", &latest).0, "6.0.7");
        assert_eq!(target_for("5.0.22", &latest).0, "6.0.7");
    }
}
