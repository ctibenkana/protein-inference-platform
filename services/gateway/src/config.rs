//! Command-line flags, each also settable through a `GATEWAY_*` variable.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};

use crate::api::Timeouts;
use crate::auth::Defaults;
use crate::batcher::{Budget, Policy};
use crate::scheduler;
use crate::validate::{Limits, SPECIAL_TOKENS};

#[derive(Parser, Debug)]
#[command(version, about = "Inference gateway for protein embeddings")]
pub struct Config {
    /// Address for the embeddings API.
    #[arg(long, env = "GATEWAY_LISTEN", default_value = "0.0.0.0:8080")]
    pub listen: SocketAddr,

    /// Address for Prometheus metrics (kept off the API port).
    #[arg(long, env = "GATEWAY_METRICS_LISTEN", default_value = "0.0.0.0:9100")]
    pub metrics_listen: SocketAddr,

    /// Base URL of the model server, e.g. http://127.0.0.1:8000 (same pod)
    #[arg(long, env = "GATEWAY_BACKEND_URL")]
    pub backend_url: String,

    /// Sequences per request.
    #[arg(long, env = "GATEWAY_MAX_SEQUENCES", default_value_t = 64)]
    pub max_sequences: usize,

    /// Residues summed over one request (about 8 full-length proteins);
    /// bulk work belongs in the batch job.
    #[arg(long, env = "GATEWAY_MAX_REQUEST_RESIDUES", default_value_t = 8_192)]
    pub max_request_residues: usize,

    /// Residues in one sequence (ESM-2: 1,022).
    #[arg(long, env = "GATEWAY_MAX_RESIDUES", default_value_t = 1_022)]
    pub max_residues: usize,

    /// Deadline for requests without an X-Request-Timeout header, in seconds.
    #[arg(long, env = "GATEWAY_DEFAULT_TIMEOUT", default_value_t = 30.0)]
    pub default_timeout: f64,

    /// Longest deadline a caller may ask for, in seconds.
    #[arg(long, env = "GATEWAY_MAX_TIMEOUT", default_value_t = 120.0)]
    pub max_timeout: f64,

    /// After SIGTERM, seconds to keep serving while Kubernetes stops routing
    /// new requests here, before draining.
    #[arg(long, env = "GATEWAY_SHUTDOWN_DELAY", default_value_t = 5.0)]
    pub shutdown_delay: f64,

    /// How queued sequences are grouped into batches.
    #[arg(long, env = "GATEWAY_BATCHING", value_enum, default_value_t = Policy::LengthAware)]
    pub batching: Policy,

    /// Padded token positions per batch: batch size x (longest + 2).
    #[arg(long, env = "GATEWAY_MAX_BATCH_TOKENS", default_value_t = 8_192)]
    pub max_batch_tokens: usize,

    /// Sequences per batch (the model server accepts up to 256).
    #[arg(long, env = "GATEWAY_MAX_BATCH_SEQUENCES", default_value_t = 256)]
    pub max_batch_sequences: usize,

    /// Highest share of a length-aware batch that may be padding.
    #[arg(long, env = "GATEWAY_MAX_PADDING", default_value_t = 0.10)]
    pub max_padding: f64,

    /// Longest a sequence waits for others to share its batch, in
    /// milliseconds. 0: send as soon as the model server is free; batches
    /// still form while it is busy. Load tests found no benefit in waiting.
    #[arg(long, env = "GATEWAY_MAX_WAIT_MS", default_value_t = 0)]
    pub max_wait_ms: u64,

    /// Tokens (residues + 2 per sequence) that may wait in the queue; a
    /// request that would exceed it gets 503 at once instead of waiting.
    /// The default is about 1.2 s of work for ESM-2 8M on 2 CPU threads;
    /// scale it with the model server's throughput.
    #[arg(long, env = "GATEWAY_MAX_QUEUE_TOKENS", default_value_t = 16_384)]
    pub max_queue_tokens: usize,

    /// After shutdown starts, seconds to wait for requests in progress
    /// before exiting anyway; keep it below the pod's grace period.
    #[arg(long, env = "GATEWAY_DRAIN_TIMEOUT", default_value_t = 30.0)]
    pub drain_timeout: f64,

    /// Batches in flight to the model server at once (its replica count).
    #[arg(long, env = "GATEWAY_BACKEND_CONCURRENCY", default_value_t = 1)]
    pub backend_concurrency: usize,

    /// JSON file listing callers and the SHA-256 of their API keys (see
    /// src/auth.rs); re-read every --callers-reload seconds.
    #[arg(long, env = "GATEWAY_CALLERS_FILE")]
    pub callers_file: Option<PathBuf>,

    /// Serve requests without an API key as "anonymous". Local development
    /// only; one of this or --callers-file is required.
    #[arg(long, env = "GATEWAY_ALLOW_ANONYMOUS")]
    pub allow_anonymous: bool,

    /// Seconds between checks of the callers file for changes.
    #[arg(long, env = "GATEWAY_CALLERS_RELOAD", default_value_t = 30.0)]
    pub callers_reload: f64,

    /// Default rate per caller, in residues per minute (callers may override).
    #[arg(
        long,
        env = "GATEWAY_DEFAULT_RESIDUES_PER_MINUTE",
        default_value_t = 120_000
    )]
    pub default_residues_per_minute: u64,

    /// Default share of the queue per caller: tokens of its requests in
    /// progress (callers may override). Below --max-queue-tokens, so one
    /// caller can't fill the queue alone.
    #[arg(
        long,
        env = "GATEWAY_DEFAULT_CALLER_INFLIGHT_TOKENS",
        default_value_t = 12_288
    )]
    pub default_caller_inflight_tokens: usize,

    #[arg(long, env = "GATEWAY_LOG_FORMAT", value_enum, default_value_t = LogFormat::Json)]
    pub log_format: LogFormat,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LogFormat {
    Json,
    Text,
}

impl Config {
    pub fn limits(&self) -> Limits {
        Limits {
            max_sequences: self.max_sequences,
            max_request_residues: self.max_request_residues,
            max_residues: self.max_residues,
        }
    }

    pub fn timeouts(&self) -> Timeouts {
        Timeouts {
            default: Duration::from_secs_f64(self.default_timeout),
            max: Duration::from_secs_f64(self.max_timeout),
        }
    }

    pub fn batching(&self) -> scheduler::Config {
        scheduler::Config {
            policy: self.batching,
            budget: Budget {
                max_tokens: self.max_batch_tokens,
                max_sequences: self.max_batch_sequences,
                max_padding: self.max_padding,
            },
            max_wait: Duration::from_millis(self.max_wait_ms),
            concurrency: self.backend_concurrency,
            max_queue_tokens: self.max_queue_tokens,
        }
    }

    /// Settings that can't work together.
    pub fn check(&self) -> Result<(), String> {
        let longest = self.max_residues + SPECIAL_TOKENS;
        if self.max_batch_tokens < longest {
            return Err(format!(
                "--max-batch-tokens ({}) must fit one sequence of --max-residues ({longest} tokens)",
                self.max_batch_tokens
            ));
        }
        if self.callers_file.is_none() && !self.allow_anonymous {
            return Err(
                "set --callers-file (API keys), or --allow-anonymous for local development".into(),
            );
        }
        // Otherwise the largest valid request could never be admitted.
        let largest = self.largest_request_tokens();
        if self.default_caller_inflight_tokens < largest {
            return Err(format!(
                "--default-caller-inflight-tokens ({}) must hold the largest request allowed ({largest} tokens)",
                self.default_caller_inflight_tokens
            ));
        }
        if self.default_residues_per_minute < self.max_request_residues as u64 {
            return Err("--default-residues-per-minute must allow the largest request (--max-request-residues)".into());
        }
        if self.max_queue_tokens < largest {
            return Err(format!(
                "--max-queue-tokens ({}) must hold the largest request allowed ({largest} tokens)",
                self.max_queue_tokens
            ));
        }
        if !(0.0..1.0).contains(&self.max_padding) {
            return Err("--max-padding must be at least 0 and below 1".into());
        }
        if self.max_batch_sequences == 0 || self.backend_concurrency == 0 {
            return Err(
                "--max-batch-sequences and --backend-concurrency must be at least 1".into(),
            );
        }
        Ok(())
    }

    /// Tokens of the largest request allowed (residues + 2 per sequence).
    fn largest_request_tokens(&self) -> usize {
        self.max_request_residues + self.max_sequences * SPECIAL_TOKENS
    }

    pub fn auth_defaults(&self) -> Defaults {
        Defaults {
            residues_per_minute: self.default_residues_per_minute,
            max_inflight_tokens: self.default_caller_inflight_tokens,
            largest_request_residues: self.max_request_residues as u64,
            largest_request_tokens: self.largest_request_tokens(),
        }
    }

    pub fn callers_reload(&self) -> Duration {
        Duration::from_secs_f64(self.callers_reload)
    }

    pub fn shutdown_delay(&self) -> Duration {
        Duration::from_secs_f64(self.shutdown_delay)
    }

    pub fn drain_timeout(&self) -> Duration {
        Duration::from_secs_f64(self.drain_timeout)
    }
}
