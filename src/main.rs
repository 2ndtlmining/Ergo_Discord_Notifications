mod alerts;
mod config;
mod discord;
mod explorer;
mod monitor;
mod node;
mod preview;
mod release;
mod wallet;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::monitor::{AppState, Shared};

/// Baked in by the Dockerfile's GIT_SHA / BUILT_AT build args.
const COMMIT: &str = match option_env!("ERGO_MONITOR_COMMIT") {
    Some(v) => v,
    None => "dev",
};
const BUILT_AT: &str = match option_env!("ERGO_MONITOR_BUILT_AT") {
    Some(v) => v,
    None => "unknown",
};

#[tokio::main]
async fn main() -> Result<()> {
    // A missing .env is fine: in Docker the same values arrive via --env-file.
    let _ = dotenvy::dotenv();

    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(healthcheck().await);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("invalid configuration: {e:#}");
            std::process::exit(2);
        }
    };
    let preview = std::env::args().nth(1).as_deref() == Some("preview");
    if !preview {
        log_config(&config);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .user_agent(concat!("ergo-monitor/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let discord = discord::Discord::new(
        client.clone(),
        config.discord_webhook_url.clone(),
        config.discord_user.clone(),
        config.discord_icon_base_url.clone(),
    );
    let shared: Shared = if preview {
        tracing::info!("preview mode: fake data, no polling, no Discord");
        Arc::new(RwLock::new(preview::state(COMMIT)))
    } else {
        let shared: Shared = Arc::new(RwLock::new(AppState::new(COMMIT)));
        tokio::spawn(monitor::run(
            config.clone(),
            client.clone(),
            discord.clone(),
            shared.clone(),
        ));
        tokio::spawn(release::run(client.clone(), shared.clone()));
        tokio::spawn(wallet::run(config.clone(), client, discord, shared.clone()));
        shared
    };

    let app = Router::new()
        .route("/", get(|| asset("text/html; charset=utf-8", INDEX_HTML)))
        .route(
            "/app.css",
            get(|| asset("text/css; charset=utf-8", APP_CSS)),
        )
        .route(
            "/app.js",
            get(|| asset("text/javascript; charset=utf-8", APP_JS)),
        )
        .route("/healthz", get(healthz))
        .route("/api/status", get(status))
        .route("/api/alerts", get(alerts))
        .route("/api/nodes/{id}", get(node))
        .with_state(shared);
    let addr = SocketAddr::from(([0, 0, 0, 0], config.http_port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("ergo-monitor {COMMIT} listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("shut down cleanly");
    Ok(())
}

// The dashboard is compiled into the binary; no files needed at runtime.
const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");

async fn asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
}

async fn status(State(shared): State<Shared>) -> Json<AppState> {
    Json(shared.read().await.clone())
}

async fn node(
    State(shared): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<monitor::NodeState>, (StatusCode, Json<Value>)> {
    let state = shared.read().await;
    state
        .nodes
        .iter()
        .find(|n| n.id == id)
        .cloned()
        .map(Json)
        .ok_or_else(|| {
            let known: Vec<_> = state.nodes.iter().map(|n| n.id.as_str()).collect();
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("no node with id '{id}'"), "known_ids": known })),
            )
        })
}

async fn alerts(State(shared): State<Shared>) -> Json<Vec<monitor::AlertRecord>> {
    Json(shared.read().await.alerts.iter().cloned().collect())
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "commit": COMMIT, "built_at": BUILT_AT }))
}

/// `ergo-monitor healthcheck`, used by Docker's HEALTHCHECK (the image has no curl).
async fn healthcheck() -> i32 {
    let port = std::env::var("HTTP_PORT").unwrap_or_else(|_| "7777".into());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("http client");
    match client
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => 0,
        _ => 1,
    }
}

fn log_config(config: &Config) {
    if config.discord_webhook_url.is_none() {
        tracing::warn!("DISCORD_WEBHOOK_URL not set: Discord alerts are disabled");
    } else if config.discord_user.is_some() {
        tracing::info!("Discord alerts enabled, with @mention");
    } else {
        tracing::info!("Discord alerts enabled");
    }
    tracing::info!(
        "reference explorers: mainnet={} p2p={}",
        config.explorer_mainnet_api,
        config.explorer_p2p_api
    );
    tracing::info!(
        "lag threshold {} blocks, node poll {}s, wallet poll {}s",
        config.lag_threshold_blocks,
        config.node_poll_seconds,
        config.wallet_poll_seconds
    );
    if config.nodes.is_empty() {
        tracing::warn!("no nodes configured (set NODE_1_NAME / NODE_1_URL)");
    }
    for n in &config.nodes {
        let wallet = if n.wallet_address.is_some() {
            " +wallet"
        } else {
            ""
        };
        tracing::info!("node [{}] {} -> {}{wallet}", n.id, n.name, n.url);
    }
    for w in &config.wallets {
        tracing::info!("wallet [{}] {}", w.id, w.name);
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutdown signal received");
}
