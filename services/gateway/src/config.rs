//! Command-line flags, each also settable through a `GATEWAY_*` variable.

use std::net::SocketAddr;
use std::time::Duration;

use clap::{Parser, ValueEnum};

use crate::api::Timeouts;
use crate::validate::Limits;

#[derive(Parser, Debug)]
#[command(version, about = "Inference gateway for protein embeddings")]
pub struct Config {
    /// Address for the embeddings API.
    #[arg(long, env = "GATEWAY_LISTEN", default_value = "0.0.0.0:8080")]
    pub listen: SocketAddr,

    /// Address for Prometheus metrics (kept off the API port).
    #[arg(long, env = "GATEWAY_METRICS_LISTEN", default_value = "0.0.0.0:9100")]
    pub metrics_listen: SocketAddr,

    /// Base URL of the model server, e.g. http://esm-embed-server:8000
    #[arg(long, env = "GATEWAY_BACKEND_URL")]
    pub backend_url: String,

    /// Sequences per request.
    #[arg(long, env = "GATEWAY_MAX_SEQUENCES", default_value_t = 64)]
    pub max_sequences: usize,

    /// Residues summed over one request.
    #[arg(long, env = "GATEWAY_MAX_REQUEST_RESIDUES", default_value_t = 65_536)]
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

    pub fn shutdown_delay(&self) -> Duration {
        Duration::from_secs_f64(self.shutdown_delay)
    }
}
