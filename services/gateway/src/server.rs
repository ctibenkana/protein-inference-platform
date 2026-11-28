//! Serving with a bounded, observable shutdown.
//!
//! When `signal` fires: readiness fails at once and the API keeps serving
//! for `delay`, because Kubernetes takes a moment to stop routing new
//! requests to a terminating pod. Then no new connections are accepted, and
//! requests in progress (including their queued sequences) are given up to
//! `drain_timeout` to finish. The two together must stay below the pod's
//! `terminationGracePeriodSeconds`, or Kubernetes kills the process first.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::api::{self, AppState};

#[derive(Debug, Clone, Copy)]
pub struct Shutdown {
    pub delay: Duration,
    pub drain_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every request in progress finished.
    Drained,
    /// The drain timeout passed with requests still in progress.
    TimedOut { in_flight: i64 },
}

pub async fn serve(
    listener: TcpListener,
    state: Arc<AppState>,
    signal: impl Future<Output = ()> + Send + 'static,
    shutdown: Shutdown,
) -> std::io::Result<Outcome> {
    let (draining_tx, mut draining_rx) = watch::channel(false);
    let st = state.clone();
    let graceful = async move {
        signal.await;
        st.shutting_down.store(true, Ordering::Relaxed);
        tracing::info!(
            delay_seconds = shutdown.delay.as_secs_f64(),
            queued_tokens = st.scheduler.queued_tokens(),
            "shutdown requested; readiness failing, still serving"
        );
        tokio::time::sleep(shutdown.delay).await;
        tracing::info!(
            in_flight = st.metrics.in_flight.get(),
            queued_tokens = st.scheduler.queued_tokens(),
            "no longer accepting connections; finishing requests in progress"
        );
        let _ = draining_tx.send(true);
    };

    let server = axum::serve(listener, api::router(state.clone())).with_graceful_shutdown(graceful);
    let deadline = async move {
        // An error means the sender is gone, which only happens once the
        // server has finished; the other branch has then already won.
        if draining_rx.wait_for(|draining| *draining).await.is_ok() {
            tokio::time::sleep(shutdown.drain_timeout).await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    tokio::select! {
        result = server => {
            result?;
            Ok(Outcome::Drained)
        }
        () = deadline => {
            let in_flight = state.metrics.in_flight.get();
            tracing::warn!(in_flight, "drain timeout reached; exiting with requests unfinished");
            Ok(Outcome::TimedOut { in_flight })
        }
    }
}
