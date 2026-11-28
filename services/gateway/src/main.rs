use std::sync::Arc;
use std::sync::atomic::Ordering;

use clap::Parser;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use gateway::api::{self, AppState};
use gateway::backend::Backend;
use gateway::config::{Config, LogFormat};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let cfg = Config::parse();
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
    ));
    let api_listener = TcpListener::bind(cfg.listen).await?;
    let metrics_listener = TcpListener::bind(cfg.metrics_listen).await?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        listen = %cfg.listen,
        metrics = %cfg.metrics_listen,
        backend = %cfg.backend_url,
        "gateway started"
    );

    // Metrics stay up until the process exits, so the final values can be scraped.
    tokio::spawn(axum::serve(metrics_listener, api::metrics_router(state.clone())).into_future());

    let delay = cfg.shutdown_delay();
    let draining = state.clone();
    axum::serve(api_listener, api::router(state))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Fail readiness first and keep serving briefly: Kubernetes takes
            // a moment to stop sending new requests to a terminating pod.
            draining.shutting_down.store(true, Ordering::Relaxed);
            tracing::info!(
                delay_seconds = delay.as_secs_f64(),
                "shutdown requested; draining"
            );
            tokio::time::sleep(delay).await;
        })
        .await?;
    tracing::info!("stopped");
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
