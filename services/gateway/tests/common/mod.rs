//! Shared test harness: a fake model server on a real socket, and a gateway
//! pointed at it.

#![allow(dead_code)] // each test file uses a different subset

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use gateway::api::{self, AppState, Timeouts};
use gateway::backend::Backend;
use gateway::batcher::{Budget, Policy};
use gateway::scheduler;
use gateway::validate::Limits;

pub const SECRET: &str = "MKTAYIAKQRQISFVKSHFSRQLEERLGLIEVQ";

/// Behaviour of the fake model server.
#[derive(Default)]
pub struct Fake {
    pub calls: AtomicUsize,
    /// Every batch received, in order.
    pub batches: Mutex<Vec<Vec<String>>>,
    /// Answer this many calls with HTTP 503 before succeeding.
    pub fail_first: AtomicUsize,
    /// Sleep this long before answering.
    pub delay_ms: AtomicUsize,
    /// Return one embedding too few.
    pub short_reply: AtomicBool,
}

impl Fake {
    pub fn batches(&self) -> Vec<Vec<String>> {
        self.batches.lock().unwrap().clone()
    }

    pub fn sequences_seen(&self) -> usize {
        self.batches().iter().map(Vec::len).sum()
    }
}

/// Embedding of a sequence: (length, number of A, 1.0). Tests use sequences
/// of distinct lengths, so the first value identifies the sequence.
pub fn fake_vector(s: &str) -> [f32; 3] {
    [s.len() as f32, s.matches('A').count() as f32, 1.0]
}

async fn fake_batch(
    State(f): State<Arc<Fake>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    f.calls.fetch_add(1, Ordering::SeqCst);
    let mut seqs: Vec<String> = serde_json::from_value(body["sequences"].clone()).unwrap();
    f.batches.lock().unwrap().push(seqs.clone());
    tokio::time::sleep(Duration::from_millis(
        f.delay_ms.load(Ordering::SeqCst) as u64
    ))
    .await;
    if f.fail_first.load(Ordering::SeqCst) > 0 {
        f.fail_first.fetch_sub(1, Ordering::SeqCst);
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "model is still loading"})),
        );
    }
    if f.short_reply.load(Ordering::SeqCst) {
        seqs.pop();
    }
    let bytes: Vec<u8> = seqs
        .iter()
        .flat_map(|s| fake_vector(s))
        .flat_map(f32::to_le_bytes)
        .collect();
    (
        StatusCode::OK,
        Json(
            json!({"model": "fake@1", "dim": 3, "count": seqs.len(), "embeddings": B64.encode(bytes)}),
        ),
    )
}

pub fn batching(policy: Policy) -> scheduler::Config {
    scheduler::Config {
        policy,
        budget: Budget {
            max_tokens: 8192,
            max_sequences: 256,
            max_padding: 0.10,
        },
        max_wait: Duration::from_millis(5),
        concurrency: 1,
    }
}

/// Starts a fake model server and returns its address.
pub async fn start_fake() -> (Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route("/v1/batch", post(fake_batch))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    (fake, format!("http://{addr}"))
}

/// Starts a fake model server and a gateway pointed at it.
pub async fn setup() -> (Router, Arc<Fake>, Arc<AppState>) {
    setup_with(Limits::default(), batching(Policy::LengthAware)).await
}

pub async fn setup_with(
    limits: Limits,
    batching: scheduler::Config,
) -> (Router, Arc<Fake>, Arc<AppState>) {
    let (fake, url) = start_fake().await;
    let state = Arc::new(AppState::new(
        Backend::new(&url),
        limits,
        Timeouts {
            default: Duration::from_secs(5),
            max: Duration::from_secs(10),
        },
        batching,
    ));
    (api::router(state.clone()), fake, state)
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Value,
}

impl Reply {
    /// The first value of each returned embedding (the fake's sequence length).
    pub fn first_values(&self) -> Vec<f32> {
        self.body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["embedding"][0].as_f64().unwrap() as f32)
            .collect()
    }
}

pub async fn send(app: &Router, req: Request<Body>) -> Reply {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    Reply {
        status,
        headers,
        body,
    }
}

pub fn embed_req(body: &Value) -> Request<Body> {
    Request::post("/v1/embeddings")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub async fn embed(app: &Router, body: Value) -> Reply {
    send(app, embed_req(&body)).await
}
