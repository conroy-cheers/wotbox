mod api;
mod background;
mod channel;
mod config;
mod db;
mod dedupe;
mod entity;
mod migration;
mod model;
mod plex;
mod provider;
mod qbittorrent;
mod release_matcher;
mod tracker;

use anyhow::{Context, Result};
use api::{AppState, router, spawn_channel_scheduler, spawn_reconciler};
use background::spawn_background_workers;
use clap::Parser;
use config::{Cli, Config};
use tower_http::{
    compression::CompressionLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing();
    let config = Config::load(cli.config.as_deref())?;
    let state = AppState::new(&config).await?;
    let background_runtime = spawn_background_workers(state.clone()).await?;
    spawn_reconciler(state.clone());
    spawn_channel_scheduler(state.clone());

    let app = router(state)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));
    let address = format!("{}:{}", config.listen_address, config.port);
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("bind {address}"))?;
    tracing::info!(%address, base_path = %config.base_path, "Wotbox listening");
    let (server_shutdown, server_shutdown_signal) = tokio::sync::oneshot::channel();
    let shutdown = tokio::spawn(async move {
        shutdown_signal().await;
        let _ = server_shutdown.send(());
        background_runtime.shutdown().await;
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = server_shutdown_signal.await;
        })
        .await?;
    shutdown.await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("wotbox=info,tower_http=info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
