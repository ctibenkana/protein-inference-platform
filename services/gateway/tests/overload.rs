//! Admission control: a bounded queue that refuses work quickly.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;

use common::*;
use gateway::batcher::Policy;
use gateway::validate::Limits;

fn m(n: usize) -> String {
    "M".repeat(n)
}

async fn small_queue(
    max_queue_tokens: usize,
) -> (
    Router,
    std::sync::Arc<Fake>,
    std::sync::Arc<gateway::api::AppState>,
) {
    let mut b = batching(Policy::LengthAware);
    b.max_queue_tokens = max_queue_tokens;
    setup_with(Limits::default(), b).await
}

/// Occupies the only backend slot, so later requests stay queued.
async fn occupy_backend(app: &Router) -> tokio::task::JoinHandle<Reply> {
    let app = app.clone();
    let h = tokio::spawn(async move { embed(&app, json!({ "sequences": ["MK"] })).await });
    tokio::time::sleep(Duration::from_millis(40)).await;
    h
}

#[tokio::test]
async fn full_queue_refuses_at_once_with_retry_after() {
    let (app, fake, state) = small_queue(100).await;
    fake.delay_ms.store(300, Ordering::SeqCst);
    let blocker = occupy_backend(&app).await;

    // 62 tokens queued; 52 more would make 114 > 100.
    let app2 = app.clone();
    let queued = tokio::spawn(async move { embed(&app2, json!({ "sequences": [m(60)] })).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(state.scheduler.queued_tokens(), 62);

    let started = Instant::now();
    let r = embed(&app, json!({ "sequences": [m(50)] })).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.body["error"]["code"], "overloaded");
    assert_eq!(r.headers["retry-after"], "1");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "refused without waiting"
    );

    // A request that still fits is accepted.
    let app3 = app.clone();
    let fits = tokio::spawn(async move { embed(&app3, json!({ "sequences": [m(30)] })).await });

    assert_eq!(blocker.await.unwrap().status, StatusCode::OK);
    assert_eq!(queued.await.unwrap().status, StatusCode::OK);
    assert_eq!(fits.await.unwrap().status, StatusCode::OK);
    assert_eq!(
        state.scheduler.queued_tokens(),
        0,
        "every reservation released"
    );
    let text = state.metrics.render();
    assert!(
        text.contains("gateway_rejected_total{reason=\"queue_full\"} 1"),
        "{text}"
    );
    assert!(text.contains("gateway_queue_capacity_tokens 100"));
    assert!(
        fake.batches().iter().flatten().all(|s| s.len() != 50),
        "refused work never ran"
    );
}

#[tokio::test]
async fn admission_is_all_or_nothing() {
    let (app, fake, state) = small_queue(100).await;
    fake.delay_ms.store(200, Ordering::SeqCst);
    let blocker = occupy_backend(&app).await;
    // 42 + 42 fits; 42 + 42 + 42 = 126 doesn't: none of the three is queued.
    let r = embed(&app, json!({ "sequences": [m(40), m(40), m(40)] })).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(state.scheduler.queued_tokens(), 0);
    blocker.await.unwrap();
    assert_eq!(fake.sequences_seen(), 1, "only the blocker ran");
}

#[tokio::test]
async fn expired_and_cancelled_work_gives_its_space_back() {
    let (app, fake, state) = small_queue(1000).await;
    fake.delay_ms.store(300, Ordering::SeqCst);
    let blocker = occupy_backend(&app).await;
    let mut req = embed_req(&json!({ "sequences": [m(100), m(200)] }));
    req.headers_mut()
        .insert("x-request-timeout", "0.05".parse().unwrap());
    assert_eq!(send(&app, req).await.status, StatusCode::GATEWAY_TIMEOUT);
    blocker.await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(state.scheduler.queued_tokens(), 0);
}

/// Many simultaneous requests against a small queue: every request is
/// either answered or refused, the model sees exactly the accepted work,
/// and no queue space leaks.
#[tokio::test]
async fn stress_accounting_is_exact() {
    let (app, fake, state) = small_queue(4_000).await;
    fake.delay_ms.store(20, Ordering::SeqCst);
    let mut tasks = Vec::new();
    for i in 0..200usize {
        let app = app.clone();
        // Two sequences per request, varied lengths; distinct per request.
        let seqs = vec![m(50 + i % 400), m(60 + (i * 7) % 500)];
        tasks.push(tokio::spawn(async move {
            (embed(&app, json!({ "sequences": seqs })).await, seqs)
        }));
    }
    let (mut ok, mut refused, mut accepted_residues) = (0, 0, 0);
    for t in tasks {
        let (r, seqs) = t.await.unwrap();
        match r.status {
            StatusCode::OK => {
                ok += 1;
                accepted_residues += seqs.iter().map(String::len).sum::<usize>();
                let want: Vec<f32> = seqs.iter().map(|s| s.len() as f32).collect();
                assert_eq!(
                    r.first_values(),
                    want,
                    "results routed to the right request"
                );
            }
            StatusCode::SERVICE_UNAVAILABLE => {
                assert_eq!(r.body["error"]["code"], "overloaded");
                refused += 1;
            }
            other => panic!("unexpected {other}: {}", r.body),
        }
    }
    assert_eq!(ok + refused, 200);
    assert!(refused > 0, "the queue limit was reached");
    assert!(ok > 0);
    let seen: usize = fake.batches().iter().flatten().map(String::len).sum();
    assert_eq!(
        seen, accepted_residues,
        "the model ran exactly the accepted work"
    );
    assert_eq!(state.scheduler.queued_tokens(), 0, "no queue space leaked");
    assert!(state.metrics.render().contains(&format!(
        "gateway_rejected_total{{reason=\"queue_full\"}} {refused}"
    )));
}
