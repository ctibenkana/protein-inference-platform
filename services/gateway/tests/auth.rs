//! API keys, per-caller quotas and synthetic traffic, end to end.

mod common;

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};

use common::*;
use gateway::api::{self, AppState, Timeouts};
use gateway::auth::{Callers, Defaults, sha256};
use gateway::backend::Backend;
use gateway::batcher::Policy;
use gateway::validate::Limits;

const DEFAULTS: Defaults = Defaults {
    residues_per_minute: 6_000,
    max_inflight_tokens: 1_000,
    largest_request_residues: 200,
    largest_request_tokens: 216, // 200 residues + 2 x 8 sequences
};

fn hex(h: [u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

struct Gateway {
    app: Router,
    fake: Arc<Fake>,
    state: Arc<AppState>,
    _file: tempfile::NamedTempFile,
}

/// A gateway with callers from a file, in front of the fake model server.
async fn gateway(callers: Value, allow_anonymous: bool, max_queue_tokens: usize) -> Gateway {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(callers.to_string().as_bytes()).unwrap();
    let registry = Arc::new(Callers::new(Some(file.path().into()), DEFAULTS));
    registry.reload().unwrap();

    let (fake, url) = start_fake().await;
    let mut b = batching(Policy::LengthAware);
    b.max_queue_tokens = max_queue_tokens;
    let state = Arc::new(
        AppState::new(
            Backend::new(&url),
            Limits {
                max_sequences: 8,
                max_request_residues: 200,
                max_residues: 100,
            },
            Timeouts {
                default: Duration::from_secs(5),
                max: Duration::from_secs(10),
            },
            b,
        )
        .with_auth(registry, allow_anonymous),
    );
    Gateway {
        app: api::router(state.clone()),
        fake,
        state,
        _file: file,
    }
}

fn caller(id: &str, key: &str, extra: Value) -> Value {
    let mut c = json!({ "id": id, "key_sha256": hex(sha256(key)) });
    c.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    c
}

fn request(key: Option<&str>, seqs: &[String]) -> Request<Body> {
    let mut r = Request::post("/v1/embeddings").header("content-type", "application/json");
    if let Some(k) = key {
        r = r.header("authorization", format!("Bearer {k}"));
    }
    r.body(Body::from(json!({ "sequences": seqs }).to_string()))
        .unwrap()
}

fn m(n: usize) -> String {
    "M".repeat(n)
}

#[tokio::test]
async fn requests_need_a_valid_key() {
    let g = gateway(
        json!({ "callers": [caller("lab-01", "key-a", json!({}))] }),
        false,
        4096,
    )
    .await;
    for (key, status) in [
        (None, 401),
        (Some("wrong"), 401),
        (Some(""), 401),
        (Some("key-a"), 200),
    ] {
        let r = send(&g.app, request(key, &[m(5)])).await;
        assert_eq!(r.status.as_u16(), status, "{key:?}: {}", r.body);
        if status == 401 {
            assert_eq!(r.body["error"]["code"], "unauthenticated");
            assert_eq!(r.headers["www-authenticate"], "Bearer");
        }
    }
    // A malformed header is refused too.
    let mut req = request(None, &[m(5)]);
    req.headers_mut()
        .insert("authorization", "Basic a2V5LWE=".parse().unwrap());
    assert_eq!(send(&g.app, req).await.status, StatusCode::UNAUTHORIZED);

    assert_eq!(
        g.fake.sequences_seen(),
        1,
        "refused requests never reach the model"
    );
    let text = g.state.metrics.render();
    assert!(
        text.contains("gateway_rejected_total{reason=\"unauthenticated\"} 4"),
        "{text}"
    );
    assert!(text.contains("gateway_caller_requests_total{caller=\"lab-01\",code=\"200\"} 1"));
    assert!(text.contains("gateway_caller_residues_total{caller=\"lab-01\"} 5"));
}

#[tokio::test]
async fn health_checks_need_no_key() {
    let g = gateway(json!({ "callers": [] }), false, 4096).await;
    for path in ["/healthz", "/readyz"] {
        assert_eq!(
            send(&g.app, Request::get(path).body(Body::empty()).unwrap())
                .await
                .status,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn anonymous_mode_serves_requests_without_a_key_but_not_bad_keys() {
    let g = gateway(json!({ "callers": [] }), true, 4096).await;
    assert_eq!(
        send(&g.app, request(None, &[m(5)])).await.status,
        StatusCode::OK
    );
    assert_eq!(
        send(&g.app, request(Some("wrong"), &[m(5)])).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn rate_limit_answers_429_with_retry_after() {
    // 300 residues per minute = 5 per second.
    let g = gateway(
        json!({ "callers": [caller("lab-01", "k", json!({ "residues_per_minute": 300 }))] }),
        false,
        4096,
    )
    .await;
    for _ in 0..3 {
        assert_eq!(
            send(&g.app, request(Some("k"), &[m(100)])).await.status,
            StatusCode::OK
        );
    }
    let r = send(&g.app, request(Some("k"), &[m(50)])).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.body["error"]["code"], "rate_limited");
    assert_eq!(
        r.headers["retry-after"], "10",
        "50 residues at 5 per second"
    );
    assert_eq!(g.fake.sequences_seen(), 3);
}

#[tokio::test]
async fn one_caller_cannot_take_the_whole_queue() {
    // Each caller may have 300 tokens in progress; the shared queue holds 4096.
    let g = gateway(
        json!({ "callers": [
            caller("heavy", "k1", json!({ "max_inflight_tokens": 300 })),
            caller("light", "k2", json!({ "max_inflight_tokens": 300 })),
        ] }),
        false,
        4096,
    )
    .await;
    g.fake.delay_ms.store(300, Ordering::SeqCst);
    let app = g.app.clone();
    let first =
        tokio::spawn(async move { send(&app, request(Some("k1"), &[m(100), m(99)])).await });
    tokio::time::sleep(Duration::from_millis(40)).await;

    // heavy holds 203 tokens; 102 more would exceed its 300.
    let r = send(&g.app, request(Some("k1"), &[m(100)])).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.body["error"]["code"], "caller_in_flight");
    // Another caller is unaffected.
    assert_eq!(
        send(&g.app, request(Some("k2"), &[m(100)])).await.status,
        StatusCode::OK
    );

    assert_eq!(first.await.unwrap().status, StatusCode::OK);
    // Once heavy's request finished, its share is free again.
    assert_eq!(
        send(&g.app, request(Some("k1"), &[m(100)])).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_full_shared_queue_does_not_cost_the_caller_its_allowance() {
    // 300 residues per minute; the shared queue holds 180 tokens.
    let g = gateway(
        json!({ "callers": [caller("lab-01", "k", json!({ "residues_per_minute": 300 }))] }),
        false,
        180,
    )
    .await;
    g.fake.delay_ms.store(300, Ordering::SeqCst);
    let app = g.app.clone();
    let blocker = tokio::spawn(async move { send(&app, request(Some("k"), &[m(2)])).await }); // allowance 298
    tokio::time::sleep(Duration::from_millis(40)).await;
    let app = g.app.clone();
    let queued = tokio::spawn(async move { send(&app, request(Some("k"), &[m(100)])).await }); // 198
    tokio::time::sleep(Duration::from_millis(40)).await;

    let r = send(&g.app, request(Some("k"), &[m(100)])).await;
    assert_eq!(
        (r.status, r.body["error"]["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("overloaded"))
    );
    blocker.await.unwrap();
    queued.await.unwrap();
    // Refunded: 198 left, so 150 more fit. Without the refund only 98 would.
    assert_eq!(
        send(&g.app, request(Some("k"), &[m(80), m(70)]))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn synthetic_traffic_is_labelled_for_the_slos() {
    let g = gateway(
        json!({ "callers": [
            caller("lab-01", "k1", json!({})),
            caller("loadtest", "k2", json!({ "synthetic": true })),
        ] }),
        false,
        4096,
    )
    .await;
    send(&g.app, request(Some("k1"), &[m(5)])).await;
    send(&g.app, request(Some("k2"), &[m(5)])).await;
    send(&g.app, request(Some("k2"), &[m(6)])).await;
    let text = g.state.metrics.render();
    assert!(
        text.contains("gateway_request_duration_seconds_count{code=\"200\",traffic=\"live\"} 1"),
        "{text}"
    );
    assert!(
        text.contains(
            "gateway_request_duration_seconds_count{code=\"200\",traffic=\"synthetic\"} 2"
        )
    );
}
