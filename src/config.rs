//! Settings loaded from environment variables (and `.env` when present).
//!
//! Nodes and wallets use numbered variables so they are easy to read in a
//! `.env` file: `NODE_1_NAME`, `NODE_1_URL`, `NODE_1_WALLET_ADDRESS`,
//! `WALLET_1_NAME`, `WALLET_1_ADDRESS`. Gaps in the numbering are allowed.

use std::collections::{BTreeMap, HashSet};

use anyhow::{bail, Context, Result};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Config {
    #[serde(skip)]
    pub discord_webhook_url: Option<String>,
    #[serde(skip)]
    pub discord_user: Option<String>,
    pub discord_icon_base_url: String,
    pub explorer_mainnet_api: String,
    pub explorer_p2p_api: String,
    pub lag_threshold_blocks: u64,
    pub node_poll_seconds: u64,
    pub wallet_poll_seconds: u64,
    /// Incoming transactions below this many ERG are not announced (dust).
    pub wallet_min_alert_erg: f64,
    pub alert_cooldown_minutes: u64,
    pub http_port: u16,
    /// @mention on incoming wallet transactions too, not only on problems.
    pub wallet_mention: bool,
    /// Public address of the dashboard; Discord alert titles link to it.
    pub dashboard_url: Option<String>,
    pub nodes: Vec<NodeConfig>,
    pub wallets: Vec<WalletConfig>,
    /// Likely mistakes that don't stop the monitor; logged at startup (#30).
    #[serde(skip)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeConfig {
    pub id: String,
    pub name: String,
    pub url: String,
    pub wallet_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WalletConfig {
    pub id: String,
    pub name: String,
    pub address: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_vars(std::env::vars())
    }

    pub fn from_vars(vars: impl IntoIterator<Item = (String, String)>) -> Result<Self> {
        let vars: BTreeMap<String, String> = vars
            .into_iter()
            .map(|(k, v)| (k, v.trim().to_string()))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let get = |key: &str| vars.get(key).cloned();

        let config = Config {
            discord_webhook_url: get("DISCORD_WEBHOOK_URL"),
            discord_user: get("DISCORD_USER"),
            // Discord fetches the icons itself, so they must be publicly reachable.
            discord_icon_base_url: base_url(get("DISCORD_ICON_BASE_URL").unwrap_or(
                "https://raw.githubusercontent.com/2ndtlmining/Ergo_Discord_Notifications/main/assets/discord".into(),
            )),
            explorer_mainnet_api: base_url(
                get("EXPLORER_MAINNET_API").unwrap_or("https://api.ergoplatform.com".into()),
            ),
            explorer_p2p_api: base_url(
                get("EXPLORER_P2P_API").unwrap_or("https://api-102.ergoplatform.com".into()),
            ),
            lag_threshold_blocks: number(&vars, "LAG_THRESHOLD_BLOCKS", 5)?,
            node_poll_seconds: number(&vars, "NODE_POLL_SECONDS", 30)?,
            wallet_poll_seconds: number(&vars, "WALLET_POLL_SECONDS", 300)?,
            wallet_min_alert_erg: number(&vars, "WALLET_MIN_ALERT_ERG", 0.0)?,
            alert_cooldown_minutes: number(&vars, "ALERT_COOLDOWN_MINUTES", 30)?,
            http_port: number(&vars, "HTTP_PORT", 7777)?,
            wallet_mention: flag(&vars, "WALLET_MENTION")?,
            dashboard_url: get("DASHBOARD_URL").map(base_url),
            nodes: nodes(&vars)?,
            wallets: wallets(&vars)?,
            warnings: unknown_keys(&vars),
        };
        let mut config = config;

        if config.node_poll_seconds == 0 || config.wallet_poll_seconds == 0 {
            bail!("NODE_POLL_SECONDS and WALLET_POLL_SECONDS must be greater than 0");
        }
        // Upper bounds keep the duration maths from overflowing.
        for (key, value, max) in [
            ("NODE_POLL_SECONDS", config.node_poll_seconds, 3600),
            ("WALLET_POLL_SECONDS", config.wallet_poll_seconds, 86_400),
            (
                "ALERT_COOLDOWN_MINUTES",
                config.alert_cooldown_minutes,
                10_080,
            ),
        ] {
            if value > max {
                bail!("{key}={value} is too large (maximum {max})");
            }
        }
        let mut ids = HashSet::new();
        for id in config.nodes.iter().map(|n| &n.id) {
            if !ids.insert(id) {
                bail!("two nodes share the name/id '{id}'; node names must be unique");
            }
        }
        let mut ids = HashSet::new();
        for id in config.wallets.iter().map(|w| &w.id) {
            if !ids.insert(id) {
                bail!("two wallets share the name/id '{id}'; wallet names must be unique");
            }
        }

        let addresses = config
            .nodes
            .iter()
            .filter_map(|n| {
                Some((
                    format!("wallet of node '{}'", n.name),
                    n.wallet_address.as_ref()?,
                ))
            })
            .chain(
                config
                    .wallets
                    .iter()
                    .map(|w| (format!("wallet '{}'", w.name), &w.address)),
            );
        let mut warnings: Vec<String> = addresses
            .filter(|(_, a)| !looks_like_ergo_address(a))
            .map(|(who, a)| format!("{who}: '{a}' doesn't look like an Ergo address; it won't be found on the explorer"))
            .collect();
        if let Some(url) = &config.discord_webhook_url {
            let is_webhook = url.starts_with("https://discord.com/api/webhooks/")
                || url.starts_with("https://discordapp.com/api/webhooks/");
            if !is_webhook {
                warnings.push("DISCORD_WEBHOOK_URL doesn't look like a Discord webhook URL (https://discord.com/api/webhooks/...)".into());
            } else if url.contains("/000000000000000000/") {
                warnings.push("DISCORD_WEBHOOK_URL is still the example value from .env.example; alerts won't arrive".into());
            }
        }
        config.warnings.append(&mut warnings);
        Ok(config)
    }
}

fn number<T: std::str::FromStr>(
    vars: &BTreeMap<String, String>,
    key: &str,
    default: T,
) -> Result<T> {
    match vars.get(key) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .ok()
            .with_context(|| format!("{key}='{v}' is not a valid number")),
    }
}

/// Indexes `n` found in keys shaped like `{prefix}_{n}_{suffix}`.
fn indexes(vars: &BTreeMap<String, String>, prefix: &str) -> Vec<u32> {
    let mut found: Vec<u32> = vars
        .keys()
        .filter_map(|k| k.strip_prefix(prefix)?.strip_prefix('_'))
        .filter_map(|rest| rest.split_once('_')?.0.parse().ok())
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

fn nodes(vars: &BTreeMap<String, String>) -> Result<Vec<NodeConfig>> {
    indexes(vars, "NODE")
        .into_iter()
        .map(|n| {
            let name = vars
                .get(&format!("NODE_{n}_NAME"))
                .with_context(|| format!("NODE_{n}_NAME is missing"))?;
            let url = vars
                .get(&format!("NODE_{n}_URL"))
                .with_context(|| format!("NODE_{n}_URL is missing for node '{name}'"))?;
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                bail!("NODE_{n}_URL='{url}' must start with http:// or https://");
            }
            Ok(NodeConfig {
                id: slug(name),
                name: name.clone(),
                url: base_url(url.clone()),
                wallet_address: vars.get(&format!("NODE_{n}_WALLET_ADDRESS")).cloned(),
            })
        })
        .collect()
}

fn flag(vars: &BTreeMap<String, String>, key: &str) -> Result<bool> {
    match vars.get(key).map(|v| v.to_lowercase()) {
        None => Ok(false),
        Some(v) if ["1", "true", "yes", "on"].contains(&v.as_str()) => Ok(true),
        Some(v) if ["0", "false", "no", "off"].contains(&v.as_str()) => Ok(false),
        Some(v) => bail!("{key}='{v}' should be true or false"),
    }
}

/// Settings this program reads, apart from the numbered node/wallet keys.
const KNOWN_KEYS: &[&str] = &[
    "DISCORD_WEBHOOK_URL",
    "DISCORD_USER",
    "DISCORD_ICON_BASE_URL",
    "EXPLORER_MAINNET_API",
    "EXPLORER_P2P_API",
    "LAG_THRESHOLD_BLOCKS",
    "NODE_POLL_SECONDS",
    "WALLET_POLL_SECONDS",
    "WALLET_MIN_ALERT_ERG",
    "WALLET_MENTION",
    "ALERT_COOLDOWN_MINUTES",
    "HTTP_PORT",
    "DASHBOARD_URL",
];

/// Warnings for settings with our prefixes that nothing reads, e.g.
/// `NODE_1_WALLET` instead of `NODE_1_WALLET_ADDRESS`, which would otherwise
/// be silently ignored.
fn unknown_keys(vars: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    for key in vars.keys() {
        if KNOWN_KEYS.contains(&key.as_str()) {
            continue;
        }
        let expected: Vec<String> = match numbered(key) {
            Some(("NODE", n, field)) => {
                if ["NAME", "URL", "WALLET_ADDRESS"].contains(&field) {
                    continue;
                }
                ["NAME", "URL", "WALLET_ADDRESS"]
                    .iter()
                    .map(|f| format!("NODE_{n}_{f}"))
                    .collect()
            }
            Some(("WALLET", n, field)) => {
                if ["NAME", "ADDRESS"].contains(&field) {
                    continue;
                }
                ["NAME", "ADDRESS"]
                    .iter()
                    .map(|f| format!("WALLET_{n}_{f}"))
                    .collect()
            }
            _ if ["NODE_", "WALLET_", "DISCORD_", "EXPLORER_"]
                .iter()
                .any(|p| key.starts_with(p)) =>
            {
                KNOWN_KEYS.iter().map(|k| k.to_string()).collect()
            }
            _ => continue,
        };
        let best = expected
            .iter()
            // A truncated name (NODE_1_WALLET) beats a merely similar one (NODE_1_NAME).
            .min_by_key(|k| (!k.starts_with(key.as_str()), edit_distance(key, k)))
            .map(|k| format!("; did you mean {k}?"))
            .unwrap_or_default();
        out.push(format!(
            "{key} is not a setting this monitor reads, so it is ignored{best}"
        ));
    }
    out
}

/// `NODE_3_URL` -> ("NODE", 3, "URL").
fn numbered(key: &str) -> Option<(&str, u32, &str)> {
    let (prefix, rest) = key.split_once('_')?;
    let (n, field) = rest.split_once('_')?;
    Some((prefix, n.parse().ok()?, field))
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != *cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Ergo addresses are base58 (no 0, O, I or l) and at least ~50 characters.
fn looks_like_ergo_address(a: &str) -> bool {
    const BASE58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    a.len() >= 40 && a.chars().all(|c| BASE58.contains(c))
}

fn wallets(vars: &BTreeMap<String, String>) -> Result<Vec<WalletConfig>> {
    indexes(vars, "WALLET")
        .into_iter()
        .map(|n| {
            let name = vars
                .get(&format!("WALLET_{n}_NAME"))
                .with_context(|| format!("WALLET_{n}_NAME is missing"))?;
            let address = vars
                .get(&format!("WALLET_{n}_ADDRESS"))
                .with_context(|| format!("WALLET_{n}_ADDRESS is missing for wallet '{name}'"))?;
            Ok(WalletConfig {
                id: slug(name),
                name: name.clone(),
                address: address.clone(),
            })
        })
        .collect()
}

fn base_url(url: String) -> String {
    url.trim_end_matches('/').to_string()
}

/// "Duckpools Bot" -> "duckpools-bot"; used as a stable id in the API.
pub fn slug(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config> {
        Config::from_vars(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    #[test]
    fn defaults_apply_when_unset() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.lag_threshold_blocks, 5);
        assert_eq!(c.node_poll_seconds, 30);
        assert_eq!(c.http_port, 7777);
        assert_eq!(c.explorer_p2p_api, "https://api-102.ergoplatform.com");
        assert!(c.nodes.is_empty() && c.wallets.is_empty());
        assert!(c.discord_webhook_url.is_none());
    }

    #[test]
    fn parses_numbered_nodes_and_wallets_with_gaps() {
        let c = cfg(&[
            ("NODE_2_NAME", "Grid Bot"),
            ("NODE_2_URL", "http://10.0.0.2:9053/"),
            ("NODE_2_WALLET_ADDRESS", "9fGrid"),
            ("NODE_1_NAME", "Hosted Explorer"),
            ("NODE_1_URL", "http://10.0.0.1:9053"),
            ("WALLET_5_NAME", "Mining"),
            ("WALLET_5_ADDRESS", "9fMining"),
        ])
        .unwrap();
        assert_eq!(c.nodes.len(), 2);
        assert_eq!(c.nodes[0].id, "hosted-explorer");
        assert_eq!(c.nodes[0].wallet_address, None);
        assert_eq!(c.nodes[1].url, "http://10.0.0.2:9053");
        assert_eq!(c.nodes[1].wallet_address.as_deref(), Some("9fGrid"));
        assert_eq!(c.wallets[0].id, "mining");
    }

    #[test]
    fn warns_about_likely_mistakes() {
        let c = cfg(&[
            ("NODE_1_NAME", "Grid Bot"),
            ("NODE_1_URL", "http://a"),
            ("NODE_1_WALLET", "9f..."),
            ("WALLET_1_NAME", "Mining"),
            (
                "WALLET_1_ADDRESS",
                "9fExampleMiningAddressxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            ),
            ("DISCORD_WEBHOK_URL", "https://x"),
            (
                "DISCORD_WEBHOOK_URL",
                "https://discord.com/api/webhooks/000000000000000000/xxxxxxxx",
            ),
            ("PATH", "/usr/bin"),
        ])
        .unwrap();
        let w = c.warnings.join("\n");
        assert!(
            w.contains("NODE_1_WALLET is not a setting")
                && w.contains("did you mean NODE_1_WALLET_ADDRESS?"),
            "{w}"
        );
        assert!(w.contains("did you mean DISCORD_WEBHOOK_URL?"), "{w}");
        assert!(w.contains("wallet 'Mining'"), "{w}");
        assert!(w.contains("example value"), "{w}");
        assert!(!w.contains("PATH"), "{w}");
        assert_eq!(c.warnings.len(), 4, "{w}");

        let ok = cfg(&[
            ("WALLET_1_NAME", "Grid"),
            (
                "WALLET_1_ADDRESS",
                "9eoM6oqHBziMxQPUPvhoHuLBRZpdnRMnqedcuFmj6UrQFXmYeHX",
            ),
            ("WALLET_POLL_SECONDS", "60"),
            ("WALLET_MENTION", "yes"),
        ])
        .unwrap();
        assert!(ok.warnings.is_empty(), "{:?}", ok.warnings);
        assert!(ok.wallet_mention);
        assert!(cfg(&[("WALLET_MENTION", "maybe")]).is_err());
    }

    #[test]
    fn blank_values_count_as_unset() {
        let c = cfg(&[("DISCORD_WEBHOOK_URL", "  "), ("LAG_THRESHOLD_BLOCKS", "")]).unwrap();
        assert!(c.discord_webhook_url.is_none());
        assert_eq!(c.lag_threshold_blocks, 5);
    }

    #[test]
    fn rejects_bad_input_with_clear_errors() {
        let err = |pairs: &[(&str, &str)]| cfg(pairs).unwrap_err().to_string();
        assert!(err(&[("NODE_1_NAME", "A")]).contains("NODE_1_URL is missing"));
        assert!(err(&[("NODE_1_URL", "http://x")]).contains("NODE_1_NAME is missing"));
        assert!(err(&[("NODE_1_NAME", "A"), ("NODE_1_URL", "10.0.0.1:9053")]).contains("http://"));
        assert!(err(&[("LAG_THRESHOLD_BLOCKS", "five")]).contains("not a valid number"));
        assert!(err(&[("ALERT_COOLDOWN_MINUTES", "99999999999")]).contains("too large"));
        assert!(err(&[
            ("NODE_1_NAME", "Grid Bot"),
            ("NODE_1_URL", "http://a"),
            ("NODE_2_NAME", "grid-bot"),
            ("NODE_2_URL", "http://b"),
        ])
        .contains("unique"));
    }
}
