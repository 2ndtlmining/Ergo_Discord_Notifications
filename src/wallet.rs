//! Wallet balances and incoming-transaction alerts (#6), replacing main.py.
//!
//! Wallets are watched by address through the public explorer, so no node
//! API keys are needed. When both explorers fail, a monitored node whose
//! indexer (`extraIndex`) is caught up answers instead. On startup the newest
//! block height per wallet is recorded silently; after that every new incoming
//! transaction is reported, so restarts no longer re-announce old transactions.

use std::collections::HashMap;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::discord::{Discord, Embed};
use crate::monitor::{record_alert, NodeState, Shared};

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
    /// Where the last successful read came from: an explorer's host, or
    /// `node <name>` when both explorers failed.
    pub source: Option<String>,
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

/// A node's `/blockchain/balance`: confirmed and unconfirmed, each a `Balance`.
#[derive(Deserialize)]
struct NodeBalance {
    confirmed: Option<Balance>,
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
            source: None,
        }
    }
}

/// Transactions per explorer request, and how many pages one check may read
/// to catch up on a busy wallet (#26).
const PAGE: u64 = 50;
const MAX_PAGES: u64 = 6;

/// How long one wallet request may take. Longer than the client's 10 s: a
/// mining or pool address with a long history is slow to answer, and at 10 s
/// its checks failed now and then while every other wallet was fine.
const WALLET_TIMEOUT: StdDuration = StdDuration::from_secs(30);

/// The gap between wallets starting their check, so a restart does not ask the
/// explorer for every wallet in the same instant.
const STAGGER: StdDuration = StdDuration::from_millis(300);

/// Why a request failed, in a few words. `reqwest`'s own message is "error
/// sending request for url (...)" whatever happened: the cause -- a timeout,
/// a refused connection, a DNS failure -- is in its source chain, and the log
/// never said which.
fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return format!("timed out after {}s", WALLET_TIMEOUT.as_secs());
    }
    if let Some(status) = e.status() {
        return format!("HTTP {}", status.as_u16());
    }
    let mut cause: &dyn std::error::Error = e;
    while let Some(next) = cause.source() {
        cause = next;
    }
    if e.is_decode() {
        format!("unexpected response: {cause}")
    } else {
        cause.to_string()
    }
}

/// Balance plus every transaction newer than `seen` (newest first), paging
/// back as needed. The bool is true when there were more new transactions
/// than one check reads, so some older ones were skipped.
async fn fetch(
    client: &reqwest::Client,
    base: &str,
    address: &str,
    seen: Option<u64>,
) -> Result<(u64, TxPage, bool), String> {
    let get = |what: &'static str, path: String| async move {
        client
            .get(format!("{base}/api/v1/addresses/{address}/{path}"))
            .timeout(WALLET_TIMEOUT)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("{what}: {}", describe(&e)))
    };
    let page_at = |offset: u64| {
        get(
            "transactions",
            format!("transactions?limit={PAGE}&offset={offset}"),
        )
    };
    let (balance, first) =
        tokio::try_join!(get("balance", "balance/confirmed".into()), page_at(0))?;
    let balance: Balance = balance.json().await.map_err(|e| describe(&e))?;
    let mut page: TxPage = first.json().await.map_err(|e| describe(&e))?;

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
                .map_err(|e| describe(&e))?;
            if more.items.is_empty() {
                break;
            }
            page.items.extend(more.items);
            offset += PAGE;
        }
    }
    Ok((balance.nano_ergs, page, truncated))
}

/// The same answer from a node's own indexer (`extraIndex`), for when both
/// explorers are down.
///
/// `/blockchain/transaction/byAddress` and `/blockchain/balance` take the
/// address as the raw request body, and their transactions have the
/// explorer's field names, so they parse into the same `TxPage`. The API does
/// not specify the order of those transactions, so it is detected: a page
/// that runs oldest first means the newest are at the far end of the list.
///
/// One page only. This is the fallback for an outage, not the catch-up path;
/// a check that finds more than one page of new transactions says so, as the
/// explorer path does past `MAX_PAGES`.
async fn fetch_from_node(
    client: &reqwest::Client,
    url: &str,
    address: &str,
    seen: Option<u64>,
) -> Result<(u64, TxPage, bool), String> {
    let post = |what: &'static str, path: String| async move {
        client
            .post(format!("{url}{path}"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(address.to_string())
            .timeout(WALLET_TIMEOUT)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("{what}: {}", describe(&e)))
    };
    let page_at = |offset: u64| {
        post(
            "transactions",
            format!("/blockchain/transaction/byAddress?offset={offset}&limit={PAGE}"),
        )
    };
    let (balance, first) =
        tokio::try_join!(post("balance", "/blockchain/balance".into()), page_at(0))?;
    let balance: NodeBalance = balance.json().await.map_err(|e| describe(&e))?;
    let nano = balance
        .confirmed
        .ok_or("balance: the node returned no confirmed balance")?
        .nano_ergs;
    let mut page: TxPage = first.json().await.map_err(|e| describe(&e))?;
    if oldest_first(&page) && page.total > page.items.len() as u64 {
        page = page_at(page.total.saturating_sub(PAGE))
            .await?
            .json()
            .await
            .map_err(|e| describe(&e))?;
    }
    page.items
        .sort_by_key(|t| std::cmp::Reverse(t.inclusion_height));
    let truncated = seen.is_some_and(|after| {
        page.total > page.items.len() as u64
            && page
                .items
                .last()
                .is_some_and(|t| t.inclusion_height > after)
    });
    Ok((nano, page, truncated))
}

fn oldest_first(page: &TxPage) -> bool {
    matches!(
        (page.items.first(), page.items.last()),
        (Some(first), Some(last)) if first.inclusion_height < last.inclusion_height
    )
}

/// Where a wallet can be read from, in the order they are tried.
#[derive(Debug, Clone, PartialEq)]
enum Source {
    Explorer(String),
    Node { name: String, url: String },
}

impl Source {
    fn label(&self) -> String {
        match self {
            Self::Explorer(base) => base
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .to_string(),
            Self::Node { name, .. } => format!("node {name}"),
        }
    }
}

/// The two explorers, then every monitored node whose indexer can answer: up,
/// not syncing, and indexed to within `lag` blocks of its own chain. A node
/// is judged against itself because the moment this matters is when the
/// explorers -- the reference height -- are the thing that is down.
fn sources(config: &Config, nodes: &[NodeState]) -> Vec<Source> {
    let mut out = vec![Source::Explorer(config.explorer_mainnet_api.clone())];
    if config.explorer_p2p_api != config.explorer_mainnet_api {
        out.push(Source::Explorer(config.explorer_p2p_api.clone()));
    }
    out.extend(
        nodes
            .iter()
            .filter(|n| n.status != "down" && n.status != "syncing")
            .filter(|n| {
                matches!((n.full_height, n.indexed_height),
                    (Some(full), Some(indexed)) if full.saturating_sub(indexed) <= config.lag_threshold_blocks)
            })
            .map(|n| Source::Node {
                name: n.name.clone(),
                url: n.url.trim_end_matches('/').to_string(),
            }),
    );
    out
}

struct Fetched {
    nano: u64,
    page: TxPage,
    truncated: bool,
    source: String,
    /// The sources that failed before this one answered.
    skipped: Vec<String>,
}

/// The first source that answers (#26): the mainnet explorer, then the P2P
/// explorer, then the indexing nodes.
async fn fetch_any(
    client: &reqwest::Client,
    sources: &[Source],
    address: &str,
    seen: Option<u64>,
) -> Result<Fetched, String> {
    let mut errors = Vec::new();
    for source in sources {
        let result = match source {
            Source::Explorer(base) => fetch(client, base, address, seen).await,
            Source::Node { url, .. } => fetch_from_node(client, url, address, seen).await,
        };
        match result {
            Ok((nano, page, truncated)) => {
                return Ok(Fetched {
                    nano,
                    page,
                    truncated,
                    source: source.label(),
                    skipped: errors,
                })
            }
            Err(e) => errors.push(format!("{}: {e}", source.label())),
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
        let sources = sources(&config, &shared.read().await.nodes);
        let tasks: Vec<_> = wallets
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let (c, sources, addr) = (client.clone(), sources.clone(), w.address.clone());
                let after = seen.get(&w.id).copied();
                tokio::spawn(async move {
                    tokio::time::sleep(STAGGER * i as u32).await;
                    fetch_any(&c, &sources, &addr, after).await
                })
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
                Ok(Fetched {
                    nano,
                    page,
                    truncated,
                    source,
                    skipped,
                }) => {
                    if !skipped.is_empty() {
                        tracing::info!(
                            "wallet {}: read from {source} ({})",
                            w.name,
                            skipped.join("; ")
                        );
                    }
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
                    w.source = Some(source);
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
        let sources = [
            Source::Explorer("http://127.0.0.1:1".to_string()),
            Source::Explorer(good),
        ];
        let got = fetch_any(&reqwest::Client::new(), &sources, ME, None)
            .await
            .unwrap();
        assert_eq!(got.page.items.len(), 10);
        assert_eq!(got.skipped.len(), 1);
    }

    /// A fake node indexer holding `total` transactions at heights total..1,
    /// served newest first or (`oldest_first`) the other way round. It
    /// insists on the address as the raw POST body, as the node does.
    async fn fake_node(total: u64, oldest_first: bool) -> String {
        use axum::{extract::Query, routing::post, Json, Router};
        use std::collections::HashMap;
        let app = Router::new()
            .route(
                "/blockchain/balance",
                post(|body: String| async move {
                    assert_eq!(body, ME);
                    Json(serde_json::json!({
                        "confirmed": { "nanoErgs": 7_000_000_000u64, "tokens": [] },
                        "unconfirmed": { "nanoErgs": 0, "tokens": [] }
                    }))
                }),
            )
            .route(
                "/blockchain/transaction/byAddress",
                post(
                    move |Query(q): Query<HashMap<String, u64>>, body: String| async move {
                        assert_eq!(body, ME);
                        let (limit, offset) = (q["limit"], q["offset"]);
                        let heights: Vec<u64> = if oldest_first {
                            (1..=total).collect()
                        } else {
                            (1..=total).rev().collect()
                        };
                        let items: Vec<_> = heights
                            .into_iter()
                            .skip(offset as usize)
                            .take(limit as usize)
                            .map(|h| {
                                serde_json::json!({
                                    "id": format!("tx{h}"), "timestamp": 1_790_416_460_444u64,
                                    "inclusionHeight": h, "numConfirmations": 1, "index": 0,
                                    "inputs": [], "dataInputs": [],
                                    "outputs": [{ "address": ME, "value": 1, "ergoTree": "00" }],
                                })
                            })
                            .collect();
                        Json(serde_json::json!({ "items": items, "total": total }))
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[tokio::test]
    async fn falls_back_to_an_indexing_node_when_both_explorers_fail() {
        let node = fake_node(10, false).await;
        let sources = [
            Source::Explorer("http://127.0.0.1:1".to_string()),
            Source::Explorer("http://127.0.0.1:2".to_string()),
            Source::Node {
                name: "Hosted Explorer".into(),
                url: node,
            },
        ];
        let got = fetch_any(&reqwest::Client::new(), &sources, ME, Some(7))
            .await
            .unwrap();
        assert_eq!(got.source, "node Hosted Explorer");
        assert_eq!(got.skipped.len(), 2);
        assert_eq!(got.nano, 7_000_000_000);
        assert_eq!(got.page.total, 10);
        assert_eq!(incoming_since(&got.page, ME, 7).len(), 3);
    }

    /// The node's API does not say which way its pages run. Either way the
    /// newest transactions come back, newest first.
    #[tokio::test]
    async fn reads_the_newest_page_whichever_way_the_node_orders_it() {
        for oldest_first in [false, true] {
            let node = fake_node(120, oldest_first).await;
            let (_, page, truncated) =
                fetch_from_node(&reqwest::Client::new(), &node, ME, Some(100))
                    .await
                    .unwrap();
            assert_eq!(page.items.len() as u64, PAGE, "oldest_first={oldest_first}");
            assert_eq!(
                page.items[0].inclusion_height, 120,
                "oldest_first={oldest_first}"
            );
            assert!(page
                .items
                .windows(2)
                .all(|w| w[0].inclusion_height > w[1].inclusion_height));
            assert_eq!(incoming_since(&page, ME, 100).len(), 20);
            assert!(!truncated, "twenty new ones fit in the one page");

            // Further behind than one page: says so rather than paging.
            let (_, _, truncated) = fetch_from_node(&reqwest::Client::new(), &node, ME, Some(10))
                .await
                .unwrap();
            assert!(truncated, "oldest_first={oldest_first}");
        }
    }

    fn a_node(
        name: &str,
        status: &'static str,
        full: Option<u64>,
        indexed: Option<u64>,
    ) -> NodeState {
        NodeState {
            id: name.to_lowercase(),
            name: name.into(),
            url: format!("http://{name}:9053/"),
            wallet_address: None,
            status,
            condition: status,
            detail: String::new(),
            status_since: None,
            full_height: full,
            headers_height: full,
            indexed_height: indexed,
            full_lag: None,
            indexed_lag: None,
            sync_progress: None,
            peers: None,
            version: None,
            latest_version: None,
            latest_version_url: None,
            version_outdated: None,
            is_mining: false,
            is_explorer: indexed.is_some(),
            latency_ms: None,
            last_ok: None,
            last_error: None,
            runbook: None,
        }
    }

    #[test]
    fn only_nodes_whose_indexer_can_answer_are_fallbacks() {
        let config = Config::from_vars(std::iter::empty()).unwrap();
        let nodes = [
            a_node("Indexed", "ok", Some(1_000), Some(998)),
            // Explorers down: the node's status is unknown, and it still counts.
            a_node("Unknown", "unknown", Some(1_000), Some(1_000)),
            a_node("NoIndex", "ok", Some(1_000), None),
            a_node("IndexBehind", "ok", Some(1_000), Some(900)),
            a_node("Down", "down", Some(1_000), Some(1_000)),
            a_node("Syncing", "syncing", Some(1_000), Some(1_000)),
        ];
        let labels: Vec<String> = sources(&config, &nodes).iter().map(Source::label).collect();
        assert_eq!(
            labels,
            [
                "api.ergoplatform.com",
                "api-102.ergoplatform.com",
                "node Indexed",
                "node Unknown"
            ]
        );
        // The trailing slash is dropped, so paths join cleanly.
        assert!(sources(&config, &nodes).contains(&Source::Node {
            name: "Indexed".into(),
            url: "http://Indexed:9053".into()
        }));
    }

    /// The log said "error sending request for url (...)" whatever happened.
    #[tokio::test]
    async fn failures_say_what_went_wrong() {
        let client = reqwest::Client::new();
        let refused = fetch(&client, "http://127.0.0.1:1", ME, None)
            .await
            .unwrap_err();
        assert!(refused.to_lowercase().contains("refused"), "{refused}");
        assert!(!refused.contains("error sending request"), "{refused}");

        use axum::{http::StatusCode, routing::get, Router};
        let app = Router::new().fallback(get(|| async { StatusCode::TOO_MANY_REQUESTS }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let limited = fetch(&client, &base, ME, None).await.unwrap_err();
        assert!(limited.ends_with("HTTP 429"), "{limited}");
    }

    #[test]
    fn shortens_addresses() {
        assert_eq!(
            short("9fExampleAddressForTestsxxxxxxxxxxxxxxxxxxxxxAbCdEf"),
            "9fExampl…AbCdEf"
        );
    }
}
