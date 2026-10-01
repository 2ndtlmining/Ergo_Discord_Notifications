//! Wallet balances and incoming-transaction alerts (#6), replacing main.py.
//!
//! Wallets are watched by address through the public explorer, so no node
//! API keys are needed. On startup the newest block height per wallet is
//! recorded silently; after that every new incoming transaction is reported,
//! so restarts no longer re-announce old transactions.

use std::collections::HashMap;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::discord::{Discord, Embed};
use crate::monitor::{record_alert, Shared};

const NANO: f64 = 1_000_000_000.0;
const TX_URL: &str = "https://explorer.ergoplatform.com/en/transactions";
const ADDRESS_URL: &str = "https://explorer.ergoplatform.com/en/addresses";

#[derive(Debug, Clone, Serialize)]
pub struct WalletState {
    pub id: String,
    pub name: String,
    pub address: String,
    /// Set when the wallet belongs to a monitored node (bot / pool wallet).
    pub node_id: Option<String>,
    pub balance_erg: Option<f64>,
    pub total_txs: Option<u64>,
    pub last_tx: Option<TxSummary>,
    pub last_ok: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TxSummary {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub height: u64,
    /// Net change for this wallet (received minus spent), in ERG.
    pub value_erg: f64,
}

#[derive(Debug, Deserialize)]
struct TxPage {
    items: Vec<Tx>,
    total: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tx {
    id: String,
    timestamp: i64,
    inclusion_height: u64,
    inputs: Vec<Io>,
    outputs: Vec<Io>,
}

#[derive(Debug, Deserialize)]
struct Io {
    address: Option<String>,
    value: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Balance {
    nano_ergs: u64,
}

impl Tx {
    fn net_nano(&self, address: &str) -> i64 {
        let sum = |ios: &[Io]| -> i64 {
            ios.iter()
                .filter(|io| io.address.as_deref() == Some(address))
                .map(|io| io.value as i64)
                .sum()
        };
        sum(&self.outputs) - sum(&self.inputs)
    }

    fn summary(&self, address: &str) -> TxSummary {
        TxSummary {
            id: self.id.clone(),
            timestamp: DateTime::from_timestamp_millis(self.timestamp).unwrap_or_default(),
            height: self.inclusion_height,
            value_erg: self.net_nano(address) as f64 / NANO,
        }
    }
}

/// Incoming transactions (net positive for `address`) above `after_height`, oldest first.
fn incoming_since(page: &TxPage, address: &str, after_height: u64) -> Vec<TxSummary> {
    let mut txs: Vec<TxSummary> = page
        .items
        .iter()
        .filter(|t| t.inclusion_height > after_height && t.net_nano(address) > 0)
        .map(|t| t.summary(address))
        .collect();
    txs.reverse();
    txs
}

/// Standalone wallets plus wallets attached to nodes.
pub fn targets(config: &Config) -> Vec<WalletState> {
    let node_wallets = config.nodes.iter().filter_map(|n| {
        Some(WalletState::new(
            &n.id,
            &n.name,
            n.wallet_address.as_ref()?,
            Some(n.id.clone()),
        ))
    });
    let standalone = config
        .wallets
        .iter()
        .map(|w| WalletState::new(&w.id, &w.name, &w.address, None));
    node_wallets.chain(standalone).collect()
}

impl WalletState {
    fn new(id: &str, name: &str, address: &str, node_id: Option<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            address: address.into(),
            node_id,
            balance_erg: None,
            total_txs: None,
            last_tx: None,
            last_ok: None,
            last_error: None,
        }
    }
}

/// Transactions per explorer request, and how many pages one check may read
/// to catch up on a busy wallet (#26).
const PAGE: u64 = 50;
const MAX_PAGES: u64 = 6;

/// Balance plus every transaction newer than `seen` (newest first), paging
/// back as needed. The bool is true when there were more new transactions
/// than one check reads, so some older ones were skipped.
async fn fetch(
    client: &reqwest::Client,
    base: &str,
    address: &str,
    seen: Option<u64>,
) -> Result<(u64, TxPage, bool), String> {
    let get = |path: String| async move {
        client
            .get(format!("{base}/api/v1/addresses/{address}/{path}"))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())
    };
    let page_at = |offset: u64| get(format!("transactions?limit={PAGE}&offset={offset}"));
    let (balance, first) = tokio::try_join!(get("balance/confirmed".into()), page_at(0))?;
    let balance: Balance = balance.json().await.map_err(|e| e.to_string())?;
    let mut page: TxPage = first.json().await.map_err(|e| e.to_string())?;

    let mut truncated = false;
    if let Some(after) = seen {
        let mut offset = PAGE;
        while offset < page.total
            && page
                .items
                .last()
                .is_some_and(|t| t.inclusion_height > after)
        {
            if offset >= PAGE * MAX_PAGES {
                truncated = true;
                break;
            }
            let more: TxPage = page_at(offset)
                .await?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            if more.items.is_empty() {
                break;
            }
            page.items.extend(more.items);
            offset += PAGE;
        }
    }
    Ok((balance.nano_ergs, page, truncated))
}

/// The mainnet explorer first, then the P2P explorer if that fails (#26).
async fn fetch_any(
    client: &reqwest::Client,
    bases: &[String],
    address: &str,
    seen: Option<u64>,
) -> Result<(u64, TxPage, bool), String> {
    let mut errors = Vec::new();
    for base in bases {
        match fetch(client, base, address, seen).await {
            Ok(r) => return Ok(r),
            Err(e) => errors.push(format!("{base}: {e}")),
        }
    }
    Err(errors.join("; "))
}

pub async fn run(
    config: Config,
    client: reqwest::Client,
    discord: Discord,
    shared: Shared,
) -> &'static str {
    let wallets = targets(&config);
    if wallets.is_empty() {
        // Nothing to watch; stay alive so the supervisor doesn't treat this as a crash.
        return std::future::pending().await;
    }
    shared.write().await.wallets = wallets.clone();

    // Highest block height already seen per wallet; absent until the first successful poll.
    let mut seen: HashMap<String, u64> = HashMap::new();
    let mut interval = tokio::time::interval(StdDuration::from_secs(config.wallet_poll_seconds));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        let mut bases = vec![config.explorer_mainnet_api.clone()];
        if config.explorer_p2p_api != config.explorer_mainnet_api {
            bases.push(config.explorer_p2p_api.clone());
        }
        let tasks: Vec<_> = wallets
            .iter()
            .map(|w| {
                let (c, bases, addr) = (client.clone(), bases.clone(), w.address.clone());
                let after = seen.get(&w.id).copied();
                tokio::spawn(async move { fetch_any(&c, &bases, &addr, after).await })
            })
            .collect();

        let mut alerts = Vec::new();
        let mut updated = shared.read().await.wallets.clone();
        for (w, task) in updated.iter_mut().zip(tasks) {
            match task.await.unwrap_or_else(|e| Err(e.to_string())) {
                Err(e) => {
                    tracing::warn!("wallet {}: {e}", w.name);
                    w.last_error = Some(e);
                }
                Ok((nano, page, truncated)) => {
                    if truncated {
                        tracing::warn!(
                            "wallet {}: more than {} transactions since the last check; \
                             only the newest are announced",
                            w.name,
                            PAGE * MAX_PAGES
                        );
                    }
                    let top = page
                        .items
                        .iter()
                        .map(|t| t.inclusion_height)
                        .max()
                        .unwrap_or(0);
                    if let Some(&after) = seen.get(&w.id) {
                        for tx in incoming_since(&page, &w.address, after)
                            .into_iter()
                            .filter(|t| t.value_erg >= config.wallet_min_alert_erg)
                        {
                            alerts.push(received_embed(w, &tx, nano, page.total));
                        }
                    }
                    let prev = seen.get(&w.id).copied().unwrap_or(0);
                    seen.insert(w.id.clone(), top.max(prev));
                    w.balance_erg = Some(nano as f64 / NANO);
                    w.total_txs = Some(page.total);
                    w.last_tx = page.items.first().map(|t| t.summary(&w.address));
                    w.last_ok = Some(Utc::now());
                    w.last_error = None;
                }
            }
        }

        let queued = {
            let mut state = shared.write().await;
            state.wallets = updated;
            alerts
                .into_iter()
                .map(|embed| {
                    let id = record_alert(&mut state, &embed, discord.initial_delivery());
                    // Payouts are routine; only @mention for them if asked to (#34).
                    (embed, config.wallet_mention, id)
                })
                .collect()
        };
        discord.send_alerts(queued);
    }
}

fn received_embed(w: &WalletState, tx: &TxSummary, balance_nano: u64, total: u64) -> Embed {
    let whose = if w.node_id.is_some() {
        format!("{} wallet", w.name)
    } else {
        w.name.clone()
    };
    Embed::new(
        "received",
        &format!("Received · {whose}"),
        &format!("+{:.4} ERG", tx.value_erg),
    )
    .description(&format!("[View transaction]({TX_URL}/{})", tx.id))
    .field(
        "Balance",
        &format!("{:.4} ERG", balance_nano as f64 / NANO),
        true,
    )
    .field("Transactions", &crate::discord::thousands(total), true)
    .field("Block", &crate::discord::thousands(tx.height), true)
    .field(
        "Time",
        &tx.timestamp.format("%Y-%m-%d %H:%M UTC").to_string(),
        true,
    )
    .field(
        "Address",
        &format!("[{}]({ADDRESS_URL}/{})", short(&w.address), w.address),
        true,
    )
}

fn short(address: &str) -> String {
    if address.len() <= 16 {
        return address.into();
    }
    format!("{}…{}", &address[..8], &address[address.len() - 6..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "9fMe";

    fn io(address: &str, value: u64) -> Io {
        Io {
            address: Some(address.into()),
            value,
        }
    }

    fn tx(id: &str, height: u64, inputs: Vec<Io>, outputs: Vec<Io>) -> Tx {
        Tx {
            id: id.into(),
            timestamp: 1_790_416_460_444,
            inclusion_height: height,
            inputs,
            outputs,
        }
    }

    #[test]
    fn net_value_ignores_change() {
        // Spent 10, got 7 back as change: net -3, not "+7 received".
        let t = tx("a", 1, vec![io(ME, 10)], vec![io(ME, 7), io("9fOther", 3)]);
        assert_eq!(t.net_nano(ME), -3);
        let t = tx("b", 1, vec![io("9fOther", 5)], vec![io(ME, 5)]);
        assert_eq!(t.net_nano(ME), 5);
    }

    #[test]
    fn only_new_incoming_transactions_oldest_first() {
        let page = TxPage {
            total: 4,
            items: vec![
                tx("newest-in", 12, vec![], vec![io(ME, 2_000_000_000)]),
                tx("new-out", 11, vec![io(ME, 9)], vec![io("9fOther", 9)]),
                tx("older-in", 11, vec![], vec![io(ME, 1)]),
                tx("already-seen", 10, vec![], vec![io(ME, 1)]),
            ],
        };
        let got = incoming_since(&page, ME, 10);
        let ids: Vec<_> = got.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["older-in", "newest-in"]);
        assert_eq!(got[1].value_erg, 2.0);
    }

    #[test]
    fn parses_explorer_page() {
        let page: TxPage = serde_json::from_str(
            r#"{"total":780,"items":[{"id":"0952","blockId":"ff94","inclusionHeight":1881485,
            "timestamp":1790416460444,"index":4,"numConfirmations":631,
            "inputs":[{"boxId":"x","value":5,"address":"9fOther","assets":[]}],
            "outputs":[{"boxId":"y","value":5,"address":"9fMe","assets":[]}]}]}"#,
        )
        .unwrap();
        assert_eq!(page.total, 780);
        assert_eq!(page.items[0].inclusion_height, 1_881_485);
        assert_eq!(page.items[0].net_nano(ME), 5);
    }

    /// A fake explorer holding `total` transactions at heights total..1
    /// (newest first). Returns its base URL.
    async fn fake_explorer(total: u64) -> String {
        use axum::{extract::Query, routing::get, Json, Router};
        use std::collections::HashMap;
        let app = Router::new()
            .route(
                "/api/v1/addresses/{addr}/balance/confirmed",
                get(|| async { Json(serde_json::json!({ "nanoErgs": 5_000_000_000u64 })) }),
            )
            .route(
                "/api/v1/addresses/{addr}/transactions",
                get(move |Query(q): Query<HashMap<String, u64>>| async move {
                    let (limit, offset) = (q["limit"], q["offset"]);
                    let items: Vec<_> = (0..limit)
                        .map(|i| total.saturating_sub(offset + i))
                        .filter(|h| *h > 0)
                        .map(|h| {
                            serde_json::json!({
                                "id": format!("tx{h}"), "timestamp": 1_790_416_460_444u64,
                                "inclusionHeight": h, "inputs": [],
                                "outputs": [{ "address": ME, "value": 1 }],
                            })
                        })
                        .collect();
                    Json(serde_json::json!({ "items": items, "total": total }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[tokio::test]
    async fn pages_back_to_the_last_seen_transaction() {
        let base = fake_explorer(500).await;
        let client = reqwest::Client::new();
        // 120 new transactions since height 380: three pages of 50.
        let (nano, page, truncated) = fetch(&client, &base, ME, Some(380)).await.unwrap();
        assert_eq!(nano, 5_000_000_000);
        assert!(!truncated);
        assert_eq!(incoming_since(&page, ME, 380).len(), 120);
        // Far behind: stops after MAX_PAGES and says so.
        let (_, page, truncated) = fetch(&client, &base, ME, Some(1)).await.unwrap();
        assert!(truncated);
        assert_eq!(page.items.len() as u64, PAGE * MAX_PAGES);
        // First check (nothing seen yet): one page only.
        let (_, page, _) = fetch(&client, &base, ME, None).await.unwrap();
        assert_eq!(page.items.len() as u64, PAGE);
    }

    #[tokio::test]
    async fn falls_back_to_the_second_explorer() {
        let good = fake_explorer(10).await;
        let bases = ["http://127.0.0.1:1".to_string(), good];
        let (_, page, _) = fetch_any(&reqwest::Client::new(), &bases, ME, None)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 10);
    }

    #[test]
    fn shortens_addresses() {
        assert_eq!(
            short("9fExampleAddressForTestsxxxxxxxxxxxxxxxxxxxxxAbCdEf"),
            "9fExampl…AbCdEf"
        );
    }
}
