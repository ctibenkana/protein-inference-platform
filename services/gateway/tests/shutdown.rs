//! Shutdown: readiness fails first, work in progress finishes, and the
//! drain is bounded.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::oneshot;

use common::*;
use gateway::api::{AppState, Timeouts};
use gateway::backend::Backend;
use gateway::batcher::Policy;
use gateway::server::{self, Outcome, Shutdown};
use gateway::validate::Limits;

struct Running {
    url: String,
    stop: oneshot::Sender<()>,
    done: tokio::task::JoinHandle<std::io::Result<Outcome>>,
}

async fn start(fake_url: &str, shutdown: Shutdown) -> Running {
    let state = Arc::new(AppState::new(
        Backend::new(fake_url),
        Limits::default(),
        Timeouts {
            default: Duration::from_secs(10),
            max: Duration::from_secs(10),
        },
        batching(Policy::LengthAware),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (stop, signal) = oneshot::channel::<()>();
    let done = tokio::spawn(server::serve(
        listener,
        state,
        async move {
            let _ = signal.await;
        },
        shutdown,
    ));
    Running { url, stop, done }
}

fn post(url: &str, len: usize) -> tokio::task::JoinHandle<reqwest::Result<reqwest::StatusCode>> {
    let (client, url) = (reqwest::Client::new(), format!("{url}/v1/embeddings"));
    tokio::spawn(async move {
        let body = json!({ "sequences": ["M".repeat(len)] }).to_string();
        Ok(client
            .post(url)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await?
            .status())
    })
}

async fn ready(url: &str) -> u16 {
    reqwest::get(format!("{url}/readyz"))
        .await
        .map(|r| r.status().as_u16())
        .unwrap_or(0)
}

#[tokio::test]
async fn work_in_progress_and_queued_finishes_after_shutdown() {
    let (fake, fake_url) = start_fake().await;
    fake.delay_ms.store(200, Ordering::SeqCst);
    let s = start(
        &fake_url,
        Shutdown {
            delay: Duration::from_millis(150),
            drain_timeout: Duration::from_secs(5),
        },
    )
    .await;

    // One running, one queued behind it (lengths far apart: separate batches).
    let running = post(&s.url, 10);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let queued = post(&s.url, 900);
    tokio::time::sleep(Duration::from_millis(30)).await;

    s.stop.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(ready(&s.url).await, 503, "readiness fails during the delay");

    assert_eq!(running.await.unwrap().unwrap(), 200);
    assert_eq!(queued.await.unwrap().unwrap(), 200);
    assert_eq!(s.done.await.unwrap().unwrap(), Outcome::Drained);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn drain_is_bounded() {
    let (fake, fake_url) = start_fake().await;
    fake.delay_ms.store(5_000, Ordering::SeqCst);
    let s = start(
        &fake_url,
        Shutdown {
            delay: Duration::ZERO,
            drain_timeout: Duration::from_millis(200),
        },
    )
    .await;

    let _stuck = post(&s.url, 10);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = Instant::now();
    s.stop.send(()).unwrap();
    let outcome = s.done.await.unwrap().unwrap();
    assert_eq!(outcome, Outcome::TimedOut { in_flight: 1 });
    assert!(
        started.elapsed() < Duration::from_millis(600),
        "{:?}",
        started.elapsed()
    );
}
