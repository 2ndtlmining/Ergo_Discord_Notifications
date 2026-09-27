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
    pub explorer_mainnet_api: String,
    pub explorer_p2p_api: String,
    pub lag_threshold_blocks: u64,
    pub node_poll_seconds: u64,
    pub wallet_poll_seconds: u64,
    pub alert_cooldown_minutes: u64,
    pub http_port: u16,
    pub nodes: Vec<NodeConfig>,
    pub wallets: Vec<WalletConfig>,
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
            explorer_mainnet_api: base_url(
                get("EXPLORER_MAINNET_API").unwrap_or("https://api.ergoplatform.com".into()),
            ),
            explorer_p2p_api: base_url(
                get("EXPLORER_P2P_API").unwrap_or("https://api-102.ergoplatform.com".into()),
            ),
            lag_threshold_blocks: number(&vars, "LAG_THRESHOLD_BLOCKS", 5)?,
            node_poll_seconds: number(&vars, "NODE_POLL_SECONDS", 30)?,
            wallet_poll_seconds: number(&vars, "WALLET_POLL_SECONDS", 300)?,
            alert_cooldown_minutes: number(&vars, "ALERT_COOLDOWN_MINUTES", 30)?,
            http_port: number(&vars, "HTTP_PORT", 7777)?,
            nodes: nodes(&vars)?,
            wallets: wallets(&vars)?,
        };

        if config.node_poll_seconds == 0 || config.wallet_poll_seconds == 0 {
            bail!("NODE_POLL_SECONDS and WALLET_POLL_SECONDS must be greater than 0");
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
        assert!(err(&[
            ("NODE_1_NAME", "Grid Bot"),
            ("NODE_1_URL", "http://a"),
            ("NODE_2_NAME", "grid-bot"),
            ("NODE_2_URL", "http://b"),
        ])
        .contains("unique"));
    }
}
