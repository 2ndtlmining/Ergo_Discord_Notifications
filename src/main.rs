mod config;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;

use crate::config::Config;

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
    log_config(&config);

    let app = Router::new().route("/healthz", get(healthz));
    let addr = SocketAddr::from(([0, 0, 0, 0], config.http_port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("ergo-monitor {COMMIT} listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("shut down cleanly");
    Ok(())
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
