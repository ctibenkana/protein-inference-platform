//! The gateway's HTTP API against a fake model server on a real socket.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use gateway::validate::Limits;

const SECRET: &str = "MKTAYIAKQRQISFVKSHFSRQLEERLGLIEVQ";

/// Behaviour of the fake model server.
#[derive(Default)]
struct Fake {
    calls: AtomicUsize,
    /// Answer this many calls with HTTP 503 before succeeding.
    fail_first: AtomicUsize,
    /// Sleep this long before answering.
    delay_ms: AtomicUsize,
    /// Return one embedding too few.
    short_reply: AtomicBool,
}

/// Embedding of a sequence: (length, number of A, 1.0).
fn fake_vector(s: &str) -> [f32; 3] {
    [s.len() as f32, s.matches('A').count() as f32, 1.0]
}

async fn fake_batch(
    State(f): State<Arc<Fake>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    f.calls.fetch_add(1, Ordering::SeqCst);
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
    let mut seqs: Vec<String> = serde_json::from_value(body["sequences"].clone()).unwrap();
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

/// Starts a fake model server and a gateway pointed at it.
async fn setup() -> (Router, Arc<Fake>, Arc<AppState>) {
    setup_with(Limits::default()).await
}

async fn setup_with(limits: Limits) -> (Router, Arc<Fake>, Arc<AppState>) {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route("/v1/batch", post(fake_batch))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());

    let state = Arc::new(AppState::new(
        Backend::new(&format!("http://{addr}")),
        limits,
        Timeouts {
            default: Duration::from_secs(5),
            max: Duration::from_secs(10),
        },
    ));
    (api::router(state.clone()), fake, state)
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

async fn send(app: &Router, req: Request<Body>) -> Reply {
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

fn embed_req(body: &Value) -> Request<Body> {
    Request::post("/v1/embeddings")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn embed(app: &Router, body: Value) -> Reply {
    send(app, embed_req(&body)).await
}

// --- happy path ----------------------------------------------------------------

#[tokio::test]
async fn returns_embeddings_in_request_order() {
    let (app, fake, _) = setup().await;
    let r = embed(&app, json!({"sequences": ["MKTA", "maa", "M"]})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["model"], "fake@1");
    assert_eq!(r.body["dim"], 3);
    assert_eq!(r.body["usage"], json!({"sequences": 3, "residues": 8}));
    let got: Vec<Vec<f32>> = r.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, item)| {
            assert_eq!(item["index"], i);
            serde_json::from_value(item["embedding"].clone()).unwrap()
        })
        .collect();
    // "maa" was upper-cased before reaching the model.
    assert_eq!(
        got,
        vec![
            fake_vector("MKTA").to_vec(),
            fake_vector("MAA").to_vec(),
            fake_vector("M").to_vec()
        ]
    );
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        1,
        "one request is one batch"
    );
}

#[tokio::test]
async fn base64_encoding_matches_floats() {
    let (app, _, _) = setup().await;
    let r = embed(
        &app,
        json!({"sequences": ["MKA"], "encoding_format": "base64"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let raw = B64
        .decode(r.body["data"][0]["embedding"].as_str().unwrap())
        .unwrap();
    let (chunks, rest) = raw.as_chunks::<4>();
    assert!(rest.is_empty());
    let floats: Vec<f32> = chunks.iter().map(|b| f32::from_le_bytes(*b)).collect();
    assert_eq!(floats, fake_vector("MKA"));
}

#[tokio::test]
async fn request_id_is_echoed_or_generated() {
    let (app, _, _) = setup().await;
    let mut req = embed_req(&json!({"sequences": ["MK"]}));
    req.headers_mut()
        .insert("x-request-id", "lab-run-42".parse().unwrap());
    assert_eq!(send(&app, req).await.headers["x-request-id"], "lab-run-42");

    let mut req = embed_req(&json!({"sequences": ["MK"]}));
    req.headers_mut().insert(
        "x-request-id",
        "has spaces and is not plain".parse().unwrap(),
    );
    let id = send(&app, req).await.headers["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(id.len(), 36, "replaced with a UUID: {id}");
}

// --- rejected requests ---------------------------------------------------------

#[tokio::test]
async fn invalid_requests_never_reach_the_model() {
    let (app, fake, _) = setup_with(Limits {
        max_sequences: 2,
        max_request_residues: 100,
        max_residues: 50,
    })
    .await;
    let cases = [
        (json!({"sequences": []}), 400, "no_sequences"),
        (
            json!({"sequences": ["M", "M", "M"]}),
            413,
            "too_many_sequences",
        ),
        (
            json!({"sequences": ["M".repeat(50), "M".repeat(51)]}),
            422,
            "sequence_too_long",
        ),
        (json!({"sequences": ["M", "MK1"]}), 422, "invalid_residue"),
        (json!({"sequences": ["M", ""]}), 422, "empty_sequence"),
        (
            json!({"sequences": ["M"], "extra": true}),
            400,
            "invalid_body",
        ),
        (
            json!({"sequences": ["M"], "encoding_format": "hex"}),
            400,
            "invalid_body",
        ),
        (json!({"sequences": [7]}), 400, "invalid_body"),
    ];
    for (body, status, code) in cases {
        let r = embed(&app, body.clone()).await;
        assert_eq!(r.status.as_u16(), status, "{body} -> {}", r.body);
        assert_eq!(r.body["error"]["code"], code, "{body}");
    }
    let r = send(
        &app,
        Request::post("/v1/embeddings")
            .body(Body::from("{not json"))
            .unwrap(),
    )
    .await;
    assert_eq!(
        (r.status, r.body["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_body"))
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn errors_never_echo_sequences() {
    let (app, _, _) = setup().await;
    for body in [
        json!({"sequences": [format!("{SECRET}!")]}),
        json!({"sequences": [SECRET.repeat(40)]}),
        json!({"sequences": [SECRET], "encoding_format": SECRET}),
        json!({"sequences": [SECRET], SECRET: 1}),
    ] {
        let r = embed(&app, body).await;
        assert!(r.status.is_client_error());
        assert!(!r.body.to_string().contains(SECRET), "{}", r.body);
    }
}

#[tokio::test]
async fn oversized_body_is_413() {
    let (app, _, _) = setup().await;
    let r = embed(&app, json!({"sequences": ["M".repeat(2 * 1024 * 1024)]})).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn bad_timeout_header_is_400() {
    let (app, _, _) = setup().await;
    for bad in ["0", "-1", "soon", "NaN"] {
        let mut req = embed_req(&json!({"sequences": ["MK"]}));
        req.headers_mut()
            .insert("x-request-timeout", bad.parse().unwrap());
        let r = send(&app, req).await;
        assert_eq!(
            (r.status, r.body["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_timeout")),
            "{bad}"
        );
    }
}

// --- model server failures -----------------------------------------------------

#[tokio::test]
async fn one_transient_failure_is_retried() {
    let (app, fake, _) = setup().await;
    fake.fail_first.store(1, Ordering::SeqCst);
    let r = embed(&app, json!({"sequences": ["MK"]})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn persistent_failure_is_503_with_retry_after() {
    let (app, fake, _) = setup().await;
    fake.fail_first.store(5, Ordering::SeqCst);
    let r = embed(&app, json!({"sequences": ["MK"]})).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.body["error"]["code"], "backend_unavailable");
    assert_eq!(r.headers["retry-after"], "1");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2, "exactly one retry");
}

#[tokio::test]
async fn unreachable_model_server_is_503() {
    let state = Arc::new(AppState::new(
        Backend::new("http://127.0.0.1:9"),
        Limits::default(),
        Timeouts::default(),
    ));
    let r = embed(&api::router(state), json!({"sequences": ["MK"]})).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn deadline_is_enforced() {
    let (app, fake, _) = setup().await;
    fake.delay_ms.store(1_000, Ordering::SeqCst);
    let mut req = embed_req(&json!({"sequences": ["MK"]}));
    req.headers_mut()
        .insert("x-request-timeout", "0.2".parse().unwrap());
    let started = std::time::Instant::now();
    let r = send(&app, req).await;
    assert_eq!(r.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(r.body["error"]["code"], "deadline_exceeded");
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "gave up at the deadline, not after the backend"
    );
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        1,
        "timeouts are not retried"
    );
}

#[tokio::test]
async fn malformed_model_reply_is_500() {
    let (app, fake, _) = setup().await;
    fake.short_reply.store(true, Ordering::SeqCst);
    let r = embed(&app, json!({"sequences": ["MK", "MA"]})).await;
    assert_eq!(
        (r.status, r.body["error"]["code"].as_str()),
        (StatusCode::INTERNAL_SERVER_ERROR, Some("internal"))
    );
}

// --- health and metrics --------------------------------------------------------

#[tokio::test]
async fn readiness_ignores_model_server_but_follows_shutdown() {
    let (app, fake, state) = setup().await;
    let get = |path: &'static str| Request::get(path).body(Body::empty()).unwrap();
    assert_eq!(send(&app, get("/healthz")).await.status, StatusCode::OK);
    assert_eq!(send(&app, get("/readyz")).await.status, StatusCode::OK);

    // Model server failing: the gateway stays in the Service and answers 503s.
    fake.fail_first.store(10, Ordering::SeqCst);
    assert_eq!(send(&app, get("/readyz")).await.status, StatusCode::OK);
    let r = embed(&app, json!({"sequences": ["MK"]})).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);

    state.shutting_down.store(true, Ordering::SeqCst);
    assert_eq!(
        send(&app, get("/readyz")).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(send(&app, get("/healthz")).await.status, StatusCode::OK);
}

#[tokio::test]
async fn metrics_record_padding_and_outcomes() {
    let (app, fake, state) = setup().await;
    // Lengths 10 and 4: 2 x (10 + 2) = 24 tokens computed, 18 real, 6 padding.
    embed(&app, json!({"sequences": ["M".repeat(10), "MKTA"]})).await;
    fake.fail_first.store(5, Ordering::SeqCst);
    embed(&app, json!({"sequences": ["MK"]})).await;
    embed(&app, json!({"sequences": []})).await;

    let text = state.metrics.render();
    for line in [
        "gateway_tokens_computed_total 24",
        "gateway_pad_tokens_total 6",
        "gateway_sequences_total 3",
        "gateway_residues_total 16",
        "gateway_request_duration_seconds_count{code=\"200\"} 1",
        "gateway_request_duration_seconds_count{code=\"503\"} 1",
        "gateway_request_duration_seconds_count{code=\"400\"} 1",
        "gateway_backend_request_duration_seconds_count{outcome=\"server_error\"} 2",
        "gateway_backend_request_duration_seconds_count{outcome=\"ok\"} 1",
        "gateway_requests_in_flight 0",
    ] {
        assert!(text.contains(line), "missing {line:?} in\n{text}");
    }
    assert!(!text.contains("MKTA"));
}
