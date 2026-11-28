use std::sync::Arc;

use clap::Parser;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use gateway::api::{self, AppState};
use gateway::backend::Backend;
use gateway::config::{Config, LogFormat};
use gateway::server::{self, Shutdown};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let cfg = Config::parse();
    if let Err(msg) = cfg.check() {
        eprintln!("error: {msg}");
        std::process::exit(2);
    }
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    match cfg.log_format {
        LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_env_filter(filter)
            .init(),
        LogFormat::Text => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }

    let state = Arc::new(AppState::new(
        Backend::new(&cfg.backend_url),
        cfg.limits(),
        cfg.timeouts(),
        cfg.batching(),
    ));
    let api_listener = TcpListener::bind(cfg.listen).await?;
    let metrics_listener = TcpListener::bind(cfg.metrics_listen).await?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        listen = %cfg.listen,
        metrics = %cfg.metrics_listen,
        backend = %cfg.backend_url,
        batching = cfg.batching.as_str(),
        max_batch_tokens = cfg.max_batch_tokens,
        max_padding = cfg.max_padding,
        max_wait_ms = cfg.max_wait_ms,
        max_queue_tokens = cfg.max_queue_tokens,
        drain_timeout_seconds = cfg.drain_timeout,
        "gateway started"
    );

    // Metrics stay up until the process exits, so the final values can be scraped.
    tokio::spawn(axum::serve(metrics_listener, api::metrics_router(state.clone())).into_future());

    let shutdown = Shutdown {
        delay: cfg.shutdown_delay(),
        drain_timeout: cfg.drain_timeout(),
    };
    let outcome = server::serve(api_listener, state, shutdown_signal(), shutdown).await?;
    tracing::info!(?outcome, "stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing a SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}
