//! Client for the model server (`esm-embed-server`).

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// Couldn't connect, or the connection broke.
    #[error("backend unreachable")]
    Unreachable,
    /// The model server answered with an error status.
    #[error("backend returned HTTP {0}")]
    Status(u16),
    #[error("backend timed out")]
    Timeout,
    /// The response didn't match what was asked for.
    #[error("backend response invalid: {0}")]
    Invalid(&'static str),
}

impl BackendError {
    /// Worth one more attempt? Embedding is idempotent, so a retry is safe;
    /// it only helps when the failure might be on a different replica or
    /// transient (connection errors and 5xx, including "still loading").
    pub fn retryable(&self) -> bool {
        match self {
            Self::Unreachable => true,
            Self::Status(code) => *code >= 500,
            Self::Timeout | Self::Invalid(_) => false,
        }
    }

    /// Label value for metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Status(c) if *c >= 500 => "server_error",
            Self::Status(_) => "client_error",
            Self::Timeout => "timeout",
            Self::Invalid(_) => "invalid_response",
        }
    }
}

/// One embedding per input sequence, in input order.
#[derive(Debug)]
pub struct BatchOutput {
    pub model: String,
    pub dim: usize,
    /// Row-major, `count x dim`.
    pub vectors: Vec<f32>,
}

impl BatchOutput {
    pub fn row(&self, i: usize) -> &[f32] {
        &self.vectors[i * self.dim..(i + 1) * self.dim]
    }
}

#[derive(Deserialize)]
struct WireBatch {
    model: String,
    dim: usize,
    count: usize,
    embeddings: String,
}

#[derive(Clone)]
pub struct Backend {
    client: reqwest::Client,
    batch_url: String,
}

impl Backend {
    pub fn new(base_url: &str) -> Self {
        let base = base_url.trim_end_matches('/');
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .expect("HTTP client configuration is static");
        Self {
            client,
            batch_url: format!("{base}/v1/batch"),
        }
    }

    /// Embeds `sequences` as one batch, exactly as given.
    pub async fn embed(
        &self,
        sequences: &[String],
        timeout: Duration,
    ) -> Result<BatchOutput, BackendError> {
        let body = serde_json::to_vec(&serde_json::json!({ "sequences": sequences }))
            .expect("strings always serialise");
        let resp = self
            .client
            .post(&self.batch_url)
            .header("content-type", "application/json")
            .body(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(classify)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BackendError::Status(status.as_u16()));
        }
        let bytes = resp.bytes().await.map_err(classify)?;
        let wire: WireBatch = serde_json::from_slice(&bytes)
            .map_err(|_| BackendError::Invalid("not the expected JSON"))?;
        if wire.count != sequences.len() || wire.dim == 0 {
            return Err(BackendError::Invalid("wrong number of embeddings"));
        }
        let raw = B64
            .decode(wire.embeddings)
            .map_err(|_| BackendError::Invalid("embeddings are not base64"))?;
        if raw.len() != wire.count * wire.dim * 4 {
            return Err(BackendError::Invalid("embedding data has the wrong size"));
        }
        // Length checked above, so there is no remainder.
        let vectors = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        Ok(BatchOutput {
            model: wire.model,
            dim: wire.dim,
            vectors,
        })
    }
}

fn classify(err: reqwest::Error) -> BackendError {
    if err.is_timeout() {
        BackendError::Timeout
    } else {
        BackendError::Unreachable
    }
}
