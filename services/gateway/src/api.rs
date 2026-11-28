//! HTTP API: `/v1/embeddings`, health checks and request bookkeeping.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::backend::{Backend, BackendError};
use crate::metrics::{CodeLabel, DropLabel, Metrics};
use crate::scheduler::{self, Row, Scheduler};
use crate::validate::{self, Invalid, Limits};

/// Largest request body accepted: 64k residues plus JSON overhead, with room.
const MAX_BODY_BYTES: usize = 1024 * 1024;
const REQUEST_ID: &str = "x-request-id";
const REQUEST_TIMEOUT: &str = "x-request-timeout";

#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub default: Duration,
    pub max: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            default: Duration::from_secs(30),
            max: Duration::from_secs(120),
        }
    }
}

pub struct AppState {
    pub limits: Limits,
    pub timeouts: Timeouts,
    pub metrics: Arc<Metrics>,
    pub scheduler: Scheduler,
    /// Set on SIGTERM: readiness fails so Kubernetes stops routing here.
    pub shutting_down: AtomicBool,
}

impl AppState {
    /// Starts the batch dispatcher, so it must be called inside a Tokio runtime.
    pub fn new(
        backend: Backend,
        limits: Limits,
        timeouts: Timeouts,
        batching: scheduler::Config,
    ) -> Self {
        let metrics = Arc::new(Metrics::new(batching.policy.as_str()));
        Self {
            limits,
            timeouts,
            scheduler: Scheduler::start(backend, batching, metrics.clone()),
            metrics,
            shutting_down: AtomicBool::new(false),
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/embeddings", post(embeddings))
        .route_layer(middleware::from_fn_with_state(state.clone(), observe))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn(request_id))
        .with_state(state)
}

/// Router for the metrics port, kept off the API port.
pub fn metrics_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/metrics",
            get(|State(s): State<Arc<AppState>>| async move {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/openmetrics-text; version=1.0.0; charset=utf-8",
                    )],
                    s.metrics.render(),
                )
            }),
        )
        .with_state(state)
}

// --- request and response bodies ----------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbedRequest {
    sequences: Vec<String>,
    #[serde(default)]
    encoding_format: EncodingFormat,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum EncodingFormat {
    #[default]
    Float,
    Base64,
}

#[derive(Serialize)]
struct EmbedResponse<'a> {
    model: &'a str,
    dim: usize,
    data: Vec<Item<'a>>,
    usage: Usage,
}

#[derive(Serialize)]
struct Item<'a> {
    index: usize,
    embedding: Embedding<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Embedding<'a> {
    Float(&'a [f32]),
    /// Little-endian float32, like the OpenAI API's base64 format.
    Base64(String),
}

/// Sizes of a completed request; also attached to the response for the log line.
#[derive(Serialize, Clone, Copy, Debug)]
struct Usage {
    sequences: usize,
    residues: usize,
}

// --- errors --------------------------------------------------------------------

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }
}

impl From<Invalid> for ApiError {
    fn from(e: Invalid) -> Self {
        Self::new(e.status(), e.code(), e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": { "code": self.code, "message": self.message } });
        let mut resp = (self.status, axum::Json(body)).into_response();
        if let Some(secs) = self.retry_after {
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        resp
    }
}

/// serde's messages can quote parts of the input, so callers get only the
/// position of the problem.
fn json_error(e: &serde_json::Error) -> ApiError {
    let what = match e.classify() {
        serde_json::error::Category::Data => "unexpected fields or value types",
        _ => "malformed JSON",
    };
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_body",
        format!(
            "{what} at line {}, column {}; expected {{\"sequences\": [...], \"encoding_format\": \"float\" | \"base64\"}}",
            e.line(),
            e.column()
        ),
    )
}

fn backend_error(e: &BackendError) -> ApiError {
    match e {
        BackendError::Timeout => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "deadline_exceeded",
            "the request's deadline passed before the embeddings were ready",
        ),
        BackendError::Unreachable | BackendError::Status(500..) => ApiError {
            retry_after: Some(1),
            ..ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backend_unavailable",
                "the model server is unavailable; retry shortly",
            )
        },
        // The gateway validated the batch, so a 4xx or a malformed reply is our bug.
        BackendError::Status(_) | BackendError::Invalid(_) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error",
        ),
    }
}

// --- handlers ------------------------------------------------------------------

fn request_timeout(headers: &HeaderMap, t: &Timeouts) -> Result<Duration, ApiError> {
    let Some(raw) = headers.get(REQUEST_TIMEOUT) else {
        return Ok(t.default);
    };
    let secs = raw
        .to_str()
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_timeout",
                "X-Request-Timeout must be a positive number of seconds",
            )
        })?;
    Ok(Duration::from_secs_f64(secs).min(t.max))
}

async fn embeddings(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let deadline = Instant::now() + request_timeout(&headers, &s.timeouts)?;
    let body = body.map_err(|r| {
        ApiError::new(
            r.status(),
            "invalid_body",
            "request body could not be read or is too large",
        )
    })?;
    let mut req: EmbedRequest = serde_json::from_slice(&body).map_err(|e| json_error(&e))?;
    let residues = validate::validate(&mut req.sequences, &s.limits)?;
    let usage = Usage {
        sequences: req.sequences.len(),
        residues,
    };
    // Refuse now rather than queue work that would wait too long: a fast
    // 503 lets the caller back off or go elsewhere.
    if s.scheduler.admit(&req.sequences).is_err() {
        s.metrics
            .rejected
            .get_or_create(&DropLabel {
                reason: "queue_full",
            })
            .inc();
        return Err(ApiError {
            retry_after: Some(1),
            ..ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "overloaded",
                "the gateway's queue is full; retry shortly",
            )
        });
    }
    s.metrics.sequences.inc_by(usage.sequences as u64);
    s.metrics.residues.inc_by(usage.residues as u64);

    let format = req.encoding_format;
    let pending: Vec<_> = req
        .sequences
        .into_iter()
        .map(|seq| s.scheduler.submit(seq, deadline))
        .collect();
    let rows = tokio::time::timeout_at(deadline, collect(pending))
        .await
        .unwrap_or_else(|_| Err(backend_error(&BackendError::Timeout)))?;

    let data = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let embedding = match format {
                EncodingFormat::Float => Embedding::Float(row.embedding()),
                EncodingFormat::Base64 => Embedding::Base64(
                    B64.encode(
                        row.embedding()
                            .iter()
                            .flat_map(|v| v.to_le_bytes())
                            .collect::<Vec<u8>>(),
                    ),
                ),
            };
            Item { index, embedding }
        })
        .collect();
    let first = &rows[0].batch;
    let mut resp = axum::Json(EmbedResponse {
        model: &first.model,
        dim: first.dim,
        data,
        usage,
    })
    .into_response();
    resp.extensions_mut().insert(usage);
    Ok(resp)
}

/// Waits for every sequence of a request, in request order. Returning early
/// drops the remaining receivers, which cancels their queued items.
async fn collect(
    pending: Vec<tokio::sync::oneshot::Receiver<scheduler::Reply>>,
) -> Result<Vec<Row>, ApiError> {
    let mut rows = Vec::with_capacity(pending.len());
    for rx in pending {
        match rx.await {
            Ok(Ok(row)) => rows.push(row),
            Ok(Err(e)) => return Err(backend_error(&e)),
            Err(_) => {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                ));
            }
        }
    }
    Ok(rows)
}

/// Readiness reflects only the gateway itself. If it followed the model
/// server, a model-server restart would take every gateway replica out of
/// the Service, and callers would get connection errors instead of a clear
/// 503 with Retry-After.
async fn readyz(State(s): State<Arc<AppState>>) -> (StatusCode, &'static str) {
    if s.shutting_down.load(Ordering::Relaxed) {
        (StatusCode::SERVICE_UNAVAILABLE, "shutting down")
    } else {
        (StatusCode::OK, "ready")
    }
}

// --- middleware ----------------------------------------------------------------

#[derive(Clone)]
struct RequestId(String);

/// Accepts a caller's X-Request-Id if it is short and plain, otherwise makes
/// one; echoes it on the response so callers can quote it in bug reports.
async fn request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 64
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    req.extensions_mut().insert(RequestId(id.clone()));
    let mut resp = next.run(req).await;
    resp.headers_mut().insert(
        REQUEST_ID,
        HeaderValue::from_str(&id).expect("validated or generated id is a valid header"),
    );
    resp
}

/// Metrics and one log line per embedding request: sizes and timings only.
async fn observe(State(s): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let id = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0.clone())
        .unwrap_or_default();
    let started = Instant::now();
    s.metrics.in_flight.inc();
    let resp = next.run(req).await;
    s.metrics.in_flight.dec();
    let elapsed = started.elapsed();
    let code = resp.status().as_u16();
    s.metrics
        .requests
        .get_or_create(&CodeLabel { code })
        .observe(elapsed.as_secs_f64());
    let usage = resp.extensions().get::<Usage>().copied();
    tracing::info!(
        request_id = %id,
        status = code,
        duration_ms = elapsed.as_millis() as u64,
        sequences = usage.map(|u| u.sequences),
        residues = usage.map(|u| u.residues),
        "embeddings request"
    );
    resp
}
