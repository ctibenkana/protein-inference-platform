//! Prometheus metrics. Labels are low-cardinality and never carry request content.

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct CodeLabel {
    pub code: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct OutcomeLabel {
    pub outcome: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct VersionLabel {
    pub version: &'static str,
}

type HistogramFamily<L> = Family<L, Histogram, fn() -> Histogram>;

// Embedding requests on CPU take tens of milliseconds to tens of seconds.
fn latency_buckets() -> Histogram {
    Histogram::new([
        0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
    ])
}

pub struct Metrics {
    registry: Registry,
    /// Completed `/v1/embeddings` requests by HTTP status.
    pub requests: HistogramFamily<CodeLabel>,
    /// Sequences and residues accepted for embedding.
    pub sequences: Counter,
    pub residues: Counter,
    /// Calls to the model server by outcome.
    pub backend: HistogramFamily<OutcomeLabel>,
    /// Batches sent, and their size.
    pub batch_sequences: Histogram,
    /// Token positions computed, and how many of them were padding.
    pub tokens_computed: Counter,
    pub pad_tokens: Counter,
    /// Requests currently being handled.
    pub in_flight: Gauge,
}

impl Metrics {
    pub fn new() -> Self {
        let requests = HistogramFamily::<CodeLabel>::new_with_constructor(latency_buckets);
        let backend = HistogramFamily::<OutcomeLabel>::new_with_constructor(latency_buckets);
        let batch_sequences = Histogram::new([1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0]);
        let (sequences, residues) = (Counter::default(), Counter::default());
        let (tokens_computed, pad_tokens) = (Counter::default(), Counter::default());
        let in_flight = Gauge::default();
        let info = Family::<VersionLabel, Gauge>::default();
        info.get_or_create(&VersionLabel {
            version: env!("CARGO_PKG_VERSION"),
        })
        .set(1);

        let mut r = Registry::with_prefix("gateway");
        r.register(
            "request_duration_seconds",
            "Embedding requests by HTTP status.",
            requests.clone(),
        );
        r.register(
            "sequences",
            "Sequences accepted for embedding.",
            sequences.clone(),
        );
        r.register(
            "residues",
            "Residues accepted for embedding.",
            residues.clone(),
        );
        r.register(
            "backend_request_duration_seconds",
            "Calls to the model server by outcome.",
            backend.clone(),
        );
        r.register(
            "batch_sequences",
            "Sequences per batch sent to the model server.",
            batch_sequences.clone(),
        );
        r.register(
            "tokens_computed",
            "Token positions computed, including padding.",
            tokens_computed.clone(),
        );
        r.register(
            "pad_tokens",
            "Token positions spent on padding.",
            pad_tokens.clone(),
        );
        r.register(
            "requests_in_flight",
            "Embedding requests being handled.",
            in_flight.clone(),
        );
        r.register("build_info", "Gateway version.", info);

        Self {
            registry: r,
            requests,
            sequences,
            residues,
            backend,
            batch_sequences,
            tokens_computed,
            pad_tokens,
            in_flight,
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        prometheus_client::encoding::text::encode(&mut out, &self.registry)
            .expect("writing to a String cannot fail");
        out
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
