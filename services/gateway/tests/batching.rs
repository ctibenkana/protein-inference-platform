//! Dynamic batching across concurrent requests, against the fake model server.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;

use common::*;
use gateway::backend::{Backend, BackendError};
use gateway::batcher::{Budget, Policy};
use gateway::metrics::Metrics;
use gateway::scheduler::Scheduler;
use gateway::validate::Limits;

fn m(n: usize) -> String {
    "M".repeat(n)
}

/// Sends one request per sequence, concurrently, 2 ms apart so arrival
/// order is known. Returns the replies in the same order.
async fn send_each(app: &Router, seqs: &[String]) -> Vec<Reply> {
    let mut tasks = Vec::new();
    for s in seqs {
        let (app, s) = (app.clone(), s.clone());
        tasks.push(tokio::spawn(async move {
            embed(&app, json!({ "sequences": [s] })).await
        }));
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let mut out = Vec::new();
    for t in tasks {
        out.push(t.await.unwrap());
    }
    out
}

/// Occupies the backend's only slot, so the next requests queue up together.
async fn occupy_backend(app: &Router) -> tokio::task::JoinHandle<Reply> {
    let app = app.clone();
    let blocker = tokio::spawn(async move { embed(&app, json!({ "sequences": ["MK"] })).await });
    tokio::time::sleep(Duration::from_millis(40)).await; // past max_wait: it has been sent
    blocker
}

fn sorted_lengths(batches: &[Vec<String>]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = batches
        .iter()
        .map(|b| {
            let mut l: Vec<usize> = b.iter().map(String::len).collect();
            l.sort_unstable();
            l
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn idle_request_waits_at_most_max_wait() {
    let (app, _, _) = setup().await;
    let started = Instant::now();
    assert_eq!(
        embed(&app, json!({"sequences": ["MKTA"]})).await.status,
        StatusCode::OK
    );
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn concurrent_requests_share_a_batch_and_get_their_own_results() {
    let (app, fake, _) = setup().await;
    fake.delay_ms.store(150, Ordering::SeqCst);
    let blocker = occupy_backend(&app).await;

    let seqs: Vec<String> = (100..108).map(m).collect();
    let replies = send_each(&app, &seqs).await;
    assert_eq!(blocker.await.unwrap().status, StatusCode::OK);

    for (r, s) in replies.iter().zip(&seqs) {
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        assert_eq!(
            r.first_values(),
            [s.len() as f32],
            "each caller gets its own embedding"
        );
    }
    // The blocker alone, then all eight queued requests in one batch.
    assert_eq!(
        fake.batches().iter().map(Vec::len).collect::<Vec<_>>(),
        [1, 8]
    );
}

#[tokio::test]
async fn a_request_can_span_batches_and_keeps_its_order() {
    let mut b = batching(Policy::LengthAware);
    b.budget = Budget {
        max_tokens: 30,
        max_sequences: 256,
        max_padding: 0.10,
    };
    let (app, fake, _) = setup_with(Limits::default(), b).await;
    // 10 and 9 fit (2 x 12 = 24 tokens); adding 11 would need 39.
    let r = embed(&app, json!({"sequences": [m(10), m(9), m(11)]})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.first_values(), [10.0, 9.0, 11.0]);
    assert_eq!(sorted_lengths(&fake.batches()), [vec![9, 10], vec![11]]);
}

/// Mixed lengths queued together: length-aware batching separates long from
/// short; FIFO puts them in one rectangle and pays for the padding.
#[tokio::test]
async fn length_aware_separates_lengths_fifo_does_not() {
    let seqs: Vec<String> = [500, 200, 480, 210, 490, 205].map(m).to_vec();

    let (app, fake, state) = setup().await;
    fake.delay_ms.store(150, Ordering::SeqCst);
    let blocker = occupy_backend(&app).await;
    send_each(&app, &seqs).await;
    blocker.await.unwrap();
    let batches = fake.batches();
    assert_eq!(
        sorted_lengths(&batches[1..]),
        [vec![200, 205, 210], vec![480, 490, 500]]
    );
    let aware = padding(&state.metrics.render());

    let (app, fake, state) = setup_with(Limits::default(), batching(Policy::Fifo)).await;
    fake.delay_ms.store(150, Ordering::SeqCst);
    let blocker = occupy_backend(&app).await;
    send_each(&app, &seqs).await;
    blocker.await.unwrap();
    assert_eq!(fake.batches()[1].len(), 6, "FIFO: one batch of everything");
    let fifo = padding(&state.metrics.render());

    assert!(aware < 0.05, "length-aware padding {aware:.3}");
    assert!(fifo > 0.25, "fifo padding {fifo:.3}");
}

fn padding(metrics: &str) -> f64 {
    let value = |name: &str| -> f64 {
        metrics
            .lines()
            .find_map(|l| l.strip_prefix(name)?.trim().parse().ok())
            .unwrap_or_else(|| panic!("{name} missing"))
    };
    value("gateway_pad_tokens_total ") / value("gateway_tokens_computed_total ")
}

#[tokio::test]
async fn a_failed_batch_fails_every_request_in_it_after_one_retry() {
    let (app, fake, _) = setup().await;
    fake.delay_ms.store(100, Ordering::SeqCst);
    let blocker = occupy_backend(&app).await;
    fake.fail_first.store(10, Ordering::SeqCst);
    let replies = send_each(&app, &[m(50), m(51)]).await;
    blocker.await.unwrap();
    for r in replies {
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    }
    // Blocker (failed + retried) and the shared batch (failed + retried).
    assert_eq!(fake.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_request_whose_deadline_passes_in_the_queue_never_runs() {
    let (app, fake, state) = setup().await;
    fake.delay_ms.store(300, Ordering::SeqCst);
    let blocker = occupy_backend(&app).await;

    let mut req = embed_req(&json!({"sequences": [m(77)]}));
    req.headers_mut()
        .insert("x-request-timeout", "0.1".parse().unwrap());
    let started = Instant::now();
    let r = send(&app, req).await;
    assert_eq!(r.status, StatusCode::GATEWAY_TIMEOUT);
    assert!(started.elapsed() < Duration::from_millis(250));

    blocker.await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await; // let the dispatcher sweep
    assert!(
        fake.batches().iter().flatten().all(|s| s.len() != 77),
        "expired work was not sent"
    );
    assert!(
        state
            .metrics
            .render()
            .contains("gateway_dropped_total{reason=\"cancelled\"} 1")
    );
}

/// At the scheduler level: an item whose caller is still waiting but whose
/// deadline passed is answered with a timeout, not run.
#[tokio::test]
async fn scheduler_fails_items_past_their_deadline() {
    let (fake, url) = start_fake().await;
    fake.delay_ms.store(200, Ordering::SeqCst);
    let metrics = Arc::new(Metrics::new("length_aware"));
    let sched = Scheduler::start(
        Backend::new(&url),
        batching(Policy::LengthAware),
        metrics.clone(),
    );
    let now = tokio::time::Instant::now();

    let blocker = sched.submit(m(5), now + Duration::from_secs(5));
    tokio::time::sleep(Duration::from_millis(40)).await;
    let late = sched.submit(m(6), now + Duration::from_millis(80));
    assert!(matches!(late.await.unwrap(), Err(BackendError::Timeout)));
    assert!(blocker.await.unwrap().is_ok());
    assert_eq!(fake.sequences_seen(), 1);
    assert!(
        metrics
            .render()
            .contains("gateway_dropped_total{reason=\"deadline\"} 1")
    );
}

/// Two backend slots: two batches run at the same time.
#[tokio::test]
async fn concurrency_allows_parallel_batches() {
    let mut b = batching(Policy::LengthAware);
    b.concurrency = 2;
    let (app, fake, _) = setup_with(Limits::default(), b).await;
    fake.delay_ms.store(200, Ordering::SeqCst);
    let started = Instant::now();
    // Lengths far apart, so they can't share a batch.
    let replies = send_each(&app, &[m(10), m(500)]).await;
    assert!(replies.iter().all(|r| r.status == StatusCode::OK));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    assert!(
        started.elapsed() < Duration::from_millis(380),
        "ran in parallel: {:?}",
        started.elapsed()
    );
}
