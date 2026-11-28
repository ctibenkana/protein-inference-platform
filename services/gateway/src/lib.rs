//! Inference gateway: an HTTP API for protein embeddings in front of the
//! ESM-2 model server. See `docs/design/gateway.md`.
//!
//! Privacy rule for every module: sequences never appear in logs, metrics or
//! error messages; errors refer to a sequence's position in the request.

pub mod api;
pub mod backend;
pub mod batcher;
pub mod config;
pub mod metrics;
pub mod scheduler;
pub mod server;
pub mod validate;
