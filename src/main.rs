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
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::monitor::{AppState, Settings, Shared};

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
    let command = std::env::args().nth(1);
    let preview = command.as_deref() == Some("preview");
    if !preview {
        log_config(&config);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .user_agent(concat!("ergo-monitor/", env!("CARGO_PKG_VERSION")))
        .build()?;

    if command.as_deref() == Some("test-alert") {
        // docker compose exec ergo-monitor ergo-monitor test-alert
        match discord::test_alert(client, &config, COMMIT).await {
            Ok(()) => {
                println!("Test alert sent. Check your Discord channel.");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("Test alert failed: {e}");
                std::process::exit(1);
            }
        }
    }
    // Background loops run forever; if one stops or panics the process exits
    // so Docker restarts it, instead of serving frozen data (#23).
    let mut tasks: JoinSet<&'static str> = JoinSet::new();
    let shared: Shared = if preview {
        tracing::info!("preview mode: fake data, no polling, no Discord");
        Arc::new(RwLock::new(preview::state(COMMIT)))
    } else {
        let mut state = AppState::new(COMMIT, Settings::from(&config));
        state.discord.enabled = config.discord_webhook_url.is_some();
        let shared: Shared = Arc::new(RwLock::new(state));
        let (discord, worker) =
            discord::Discord::new(client.clone(), &config, COMMIT, shared.clone());
        tasks.spawn(async move {
            worker.run().await;
            "discord delivery"
        });
        tasks.spawn(monitor::run(
            config.clone(),
            client.clone(),
            discord.clone(),
            shared.clone(),
        ));
        tasks.spawn(release::run(client.clone(), shared.clone()));
        tasks.spawn(wallet::run(config.clone(), client, discord, shared.clone()));
        shared
    };

    let app = Router::new()
        .route(
            "/",
            get(|h: HeaderMap| asset(h, "text/html; charset=utf-8", INDEX_HTML)),
        )
        .route(
            "/app.css",
            get(|h: HeaderMap| asset(h, "text/css; charset=utf-8", APP_CSS)),
        )
        .route(
            "/app.js",
            get(|h: HeaderMap| asset(h, "text/javascript; charset=utf-8", APP_JS)),
        )
        .route("/healthz", get(healthz))
        .route("/api/status", get(status))
        .route("/api/alerts", get(alerts))
        .route("/api/nodes/{id}", get(node))
        .with_state(shared);
    let addr = SocketAddr::from(([0, 0, 0, 0], config.http_port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("ergo-monitor {COMMIT} listening on http://{addr}");
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    tokio::select! {
        result = server => result?,
        Some(ended) = tasks.join_next() => {
            match ended {
                Ok(name) => tracing::error!("{name} loop stopped unexpectedly; exiting so Docker restarts the monitor"),
                Err(e) => tracing::error!("background task crashed ({e}); exiting so Docker restarts the monitor"),
            }
            std::process::exit(1);
        }
    }
    tracing::info!("shut down cleanly");
    Ok(())
}

// The dashboard is compiled into the binary; no files needed at runtime.
const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");

/// The dashboard files never change while the binary runs, so the browser
/// can revalidate with an ETag and get a body-less 304 (#36).
async fn asset(
    headers: HeaderMap,
    content_type: &'static str,
    body: &'static str,
) -> impl IntoResponse {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    let etag = format!("\"{:016x}\"", hasher.finish());
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == etag);
    let status = if fresh {
        StatusCode::NOT_MODIFIED
    } else {
        StatusCode::OK
    };
    (
        status,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CACHE_CONTROL, "no-cache".to_string()),
            (header::ETAG, etag),
        ],
        if fresh { "" } else { body },
    )
}

/// Serialised straight from the shared state under the read lock, without
/// cloning it first (#36).
fn json_bytes(value: &impl serde::Serialize) -> impl IntoResponse {
    match serde_json::to_vec(value) {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain")],
            e.to_string().into_bytes(),
        ),
    }
}

async fn status(State(shared): State<Shared>) -> impl IntoResponse {
    json_bytes(&*shared.read().await)
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

async fn alerts(State(shared): State<Shared>) -> impl IntoResponse {
    json_bytes(&shared.read().await.alerts)
}

/// 503 when the node poll loop has stopped producing fresh data, so the
/// Docker health check (and `deploy.sh`) can tell (#23).
async fn healthz(State(shared): State<Shared>) -> (StatusCode, Json<Value>) {
    let s = shared.read().await;
    let fresh = s.is_fresh(chrono::Utc::now());
    let code = if fresh {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = json!({
        "status": if fresh { "ok" } else { "stale" },
        "commit": COMMIT,
        "built_at": BUILT_AT,
        "last_poll": s.generated_at,
    });
    (code, Json(body))
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
    for w in &config.warnings {
        tracing::warn!("config: {w}");
    }
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
