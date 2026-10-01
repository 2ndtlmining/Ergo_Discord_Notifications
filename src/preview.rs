//! `ergo-monitor preview`: serves the dashboard with fixed fake data (one node
//! in every condition, modelled on real cases) so the UI can be viewed and
//! screenshotted without real nodes, explorers, or Discord.

use chrono::{Duration, Utc};

use crate::discord::Embed;
use crate::explorer::ExplorerState;
use crate::monitor::{record_alert, AppState, NodeState, Reference, Summary};
use crate::release::LatestRelease;
use crate::wallet::{TxSummary, WalletState};

const TIP: u64 = 1_882_085;

pub fn state(commit: &'static str) -> AppState {
    let now = Utc::now();
    let mut s = AppState::new(commit);
    s.generated_at = Some(now);
    s.reference = Reference {
        height: Some(TIP),
        sources: ["mainnet", "p2p"]
            .into_iter()
            .map(|name| ExplorerState {
                name: name.into(),
                url: format!("https://{name}.example"),
                height: Some(TIP),
                ok: true,
                error: None,
                last_ok: Some(now),
            })
            .collect(),
    };

    s.latest_release = Some(LatestRelease {
        version: "6.0.7".into(),
        url: "https://github.com/ergoplatform/ergo/releases/tag/v6.0.7".into(),
        checked_at: now,
    });

    let node =
        |id: &str, name: &str, host: &str, condition: &'static str, detail: &str| NodeState {
            id: id.into(),
            name: name.into(),
            url: format!("http://{host}:9053"),
            wallet_address: None,
            status: match condition {
                "indexer-behind" => "behind",
                c => c,
            },
            condition,
            detail: detail.into(),
            status_since: Some(now - Duration::minutes(47)),
            full_height: Some(TIP),
            headers_height: Some(TIP),
            indexed_height: Some(TIP),
            full_lag: Some(0),
            indexed_lag: Some(0),
            sync_progress: None,
            peers: Some(30),
            version: Some("6.0.7".into()),
            version_outdated: Some(false),
            is_mining: false,
            is_explorer: true,
            latency_ms: Some(3),
            last_ok: Some(now),
            last_error: None,
            runbook: None,
        };

    let mut grid = node("grid-bot", "Grid Bot", "192.168.1.10", "ok", "In sync");
    grid.wallet_address = Some("9fGridBotExampleAddressxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into());
    let mut pool = node(
        "mining-pool",
        "Mining Pool",
        "192.168.1.14",
        "ok",
        "In sync",
    );
    pool.is_mining = true;
    pool.latency_ms = Some(2);
    let mut explorer = node(
        "hosted-explorer",
        "Hosted Explorer",
        "192.168.1.11",
        "indexer-behind",
        "Node is in sync, but its indexer is 454 blocks behind",
    );
    explorer.indexed_height = Some(TIP - 454);
    explorer.indexed_lag = Some(454);
    explorer.runbook = Some("docs/runbooks/indexer-behind.md");
    let mut rent = node(
        "storage-rent",
        "Storage Rent",
        "192.168.1.12",
        "syncing",
        "Initial sync 19.5% complete, 1514173 blocks remaining",
    );
    rent.full_height = Some(367_912);
    rent.indexed_height = Some(367_912);
    rent.full_lag = Some(TIP - 367_912);
    rent.indexed_lag = Some(TIP - 367_912);
    rent.sync_progress = Some(367_912.0 / TIP as f64);
    rent.peers = Some(45);
    rent.runbook = Some("docs/runbooks/node-behind.md");
    rent.status_since = Some(now - Duration::hours(26));
    let mut stuck = node(
        "sr-bot",
        "SR Bot",
        "192.168.1.15",
        "behind",
        "Node is 14 blocks behind the chain tip",
    );
    stuck.full_height = Some(TIP - 14);
    stuck.indexed_height = None;
    stuck.full_lag = Some(14);
    stuck.indexed_lag = None;
    stuck.is_explorer = false;
    stuck.peers = Some(3);
    stuck.version = Some("6.0.3".into());
    stuck.version_outdated = Some(true);
    stuck.runbook = Some("docs/runbooks/node-behind.md");
    stuck.status_since = Some(now - Duration::minutes(12));
    let mut duck = node(
        "duckpools",
        "Duckpools",
        "192.168.1.13",
        "down",
        "Connection refused: host is up but the node API is not listening (node process stopped?)",
    );
    duck.wallet_address = Some("9fDuckpoolsExampleAddressxxxxxxxxxxxxxxxxxxxxxxxxxx".into());
    duck.full_height = Some(TIP - 310);
    duck.headers_height = None;
    duck.indexed_height = None;
    duck.full_lag = None;
    duck.indexed_lag = None;
    duck.peers = None;
    duck.version = Some("6.0.7".into());
    duck.latency_ms = None;
    duck.last_ok = Some(now - Duration::minutes(9));
    duck.last_error = duck.detail.clone().into();
    duck.runbook = Some("docs/runbooks/node-down.md");
    duck.status_since = Some(now - Duration::minutes(8));

    s.nodes = vec![grid, explorer, duck, rent, pool, stuck];
    let count = |c: &str| s.nodes.iter().filter(|n| n.status == c).count();
    s.summary = Summary {
        total: s.nodes.len(),
        ok: count("ok"),
        behind: count("behind"),
        down: count("down"),
        syncing: count("syncing"),
        unknown: count("unknown"),
    };

    let wallet =
        |id: &str, name: &str, node: bool, bal: f64, txs: u64, value: f64, mins: i64| WalletState {
            id: id.into(),
            name: name.into(),
            address: format!("9f{id}ExampleAddressxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
            node_id: node.then(|| id.into()),
            balance_erg: Some(bal),
            total_txs: Some(txs),
            last_tx: Some(TxSummary {
                id: "0952a273d2a008a8e1795ed241f52d7596d9726199e2913cf80a5e92a3de083b".into(),
                timestamp: now - Duration::minutes(mins),
                height: TIP - 3,
                value_erg: value,
            }),
            last_ok: Some(now),
            last_error: None,
        };
    s.wallets = vec![
        wallet(
            "grid-bot",
            "Grid Bot",
            true,
            265.0573,
            962,
            1.4747,
            60 * 24 * 35,
        ),
        wallet("duckpools", "Duckpools", true, 18.0322, 5208, 0.0002, 380),
        wallet("mining", "Mining", false, 2010.7468, 1387, 12.5, 14),
        wallet("rosen", "Rosen", false, 1.889, 3182, -0.002, 60 * 24 * 5),
    ];

    for e in [
        Embed::new("received", "Received · Mining", "+12.5000 ERG")
            .description("[View transaction](https://explorer.ergoplatform.com)"),
        Embed::new("behind", "Behind", "SR Bot").description("**Node is 14 blocks behind the chain tip**"),
        Embed::new("down", "Down", "Duckpools").description(
            "**Connection refused: host is up but the node API is not listening (node process stopped?)**",
        ),
        Embed::new("ok", "Recovered", "Mining Pool")
            .description("Back in sync at height **1882070** after being behind for 6m."),
    ] {
        record_alert(&mut s, &e);
    }
    s
}
