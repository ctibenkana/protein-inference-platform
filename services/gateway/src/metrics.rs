//! Prometheus metrics. Labels are low-cardinality and never carry request content.

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RequestLabel {
    pub code: u16,
    /// "live" or "synthetic" (load tests); the SLOs count only live traffic.
    pub traffic: &'static str,
}

/// Caller ids are operator-chosen pseudonyms from the callers file, so the
/// number of values stays small and none of them names a person.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct CallerCodeLabel {
    pub caller: String,
    pub code: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct CallerLabel {
    pub caller: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct OutcomeLabel {
    pub outcome: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TriggerLabel {
    pub trigger: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DropLabel {
    pub reason: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct InfoLabel {
    version: &'static str,
    policy: &'static str,
}

type HistogramFamily<L> = Family<L, Histogram, fn() -> Histogram>;

/// Embedding requests on CPU take tens of milliseconds to tens of seconds.
fn latency_buckets() -> Histogram {
    Histogram::new([
        0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
    ])
}

pub struct Metrics {
    registry: Registry,
    /// Completed `/v1/embeddings` requests by HTTP status and traffic type.
    pub requests: HistogramFamily<RequestLabel>,
    /// Requests and residues per caller.
    pub caller_requests: Family<CallerCodeLabel, Counter>,
    pub caller_residues: Family<CallerLabel, Counter>,
    /// Callers in the current callers file, and failed reloads of it.
    pub callers: Gauge,
    pub callers_reload_errors: Counter,
    /// Requests currently being handled.
    pub in_flight: Gauge,
    /// Sequences and residues accepted for embedding.
    pub sequences: Counter,
    pub residues: Counter,
    /// Items waiting to be batched, and their tokens.
    pub queue_items: Gauge,
    pub queue_tokens: Gauge,
    /// The admission limit, so dashboards can show how full the queue is.
    pub queue_capacity: Gauge,
    /// Requests refused before queueing, by reason.
    pub rejected: Family<DropLabel, Counter>,
    /// Time from arrival to being put in a batch.
    pub queue_wait: Histogram,
    /// Items removed from the queue without being run.
    pub dropped: Family<DropLabel, Counter>,
    /// Batches sent, by what triggered them.
    pub batch_trigger: Family<TriggerLabel, Counter>,
    /// Sequences per batch.
    pub batch_sequences: Histogram,
    /// Token positions computed, and how many of them were padding.
    pub tokens_computed: Counter,
    pub pad_tokens: Counter,
    /// Calls to the model server by outcome.
    pub backend: HistogramFamily<OutcomeLabel>,
}

impl Metrics {
    pub fn new(policy: &'static str) -> Self {
        let m = Self {
            registry: Registry::with_prefix("gateway"),
            requests: HistogramFamily::new_with_constructor(latency_buckets),
            caller_requests: Family::default(),
            caller_residues: Family::default(),
            callers: Gauge::default(),
            callers_reload_errors: Counter::default(),
            in_flight: Gauge::default(),
            sequences: Counter::default(),
            residues: Counter::default(),
            queue_items: Gauge::default(),
            queue_tokens: Gauge::default(),
            queue_capacity: Gauge::default(),
            rejected: Family::default(),
            queue_wait: Histogram::new([
                0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
            ]),
            dropped: Family::default(),
            batch_trigger: Family::default(),
            batch_sequences: Histogram::new([1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0]),
            tokens_computed: Counter::default(),
            pad_tokens: Counter::default(),
            backend: HistogramFamily::new_with_constructor(latency_buckets),
        };
        let info = Family::<InfoLabel, Gauge>::default();
        info.get_or_create(&InfoLabel {
            version: env!("CARGO_PKG_VERSION"),
            policy,
        })
        .set(1);

        let mut r = m.registry;
        r.register(
            "request_duration_seconds",
            "Embedding requests by HTTP status and traffic type (live or synthetic).",
            m.requests.clone(),
        );
        r.register(
            "caller_requests",
            "Embedding requests per caller (API key) and HTTP status.",
            m.caller_requests.clone(),
        );
        r.register(
            "caller_residues",
            "Residues embedded per caller (API key).",
            m.caller_residues.clone(),
        );
        r.register("callers", "Callers in the callers file.", m.callers.clone());
        r.register(
            "callers_reload_errors",
            "Failed reloads of the callers file (the previous list stays in force).",
            m.callers_reload_errors.clone(),
        );
        r.register(
            "requests_in_flight",
            "Embedding requests being handled.",
            m.in_flight.clone(),
        );
        r.register(
            "sequences",
            "Sequences accepted for embedding.",
            m.sequences.clone(),
        );
        r.register(
            "residues",
            "Residues accepted for embedding.",
            m.residues.clone(),
        );
        r.register(
            "queue_items",
            "Sequences waiting to be batched.",
            m.queue_items.clone(),
        );
        r.register(
            "queue_tokens",
            "Tokens (residues plus special tokens) admitted and not yet batched.",
            m.queue_tokens.clone(),
        );
        r.register(
            "queue_capacity_tokens",
            "Admission limit for queued tokens.",
            m.queue_capacity.clone(),
        );
        r.register(
            "rejected",
            "Requests refused before queueing, by reason.",
            m.rejected.clone(),
        );
        r.register(
            "queue_wait_seconds",
            "Time from arrival to being put in a batch.",
            m.queue_wait.clone(),
        );
        r.register(
            "dropped",
            "Queued sequences dropped without running, by reason.",
            m.dropped.clone(),
        );
        r.register(
            "batches",
            "Batches sent to the model server, by trigger.",
            m.batch_trigger.clone(),
        );
        r.register(
            "batch_sequences",
            "Sequences per batch sent to the model server.",
            m.batch_sequences.clone(),
        );
        r.register(
            "tokens_computed",
            "Token positions computed, including padding.",
            m.tokens_computed.clone(),
        );
        r.register(
            "pad_tokens",
            "Token positions spent on padding.",
            m.pad_tokens.clone(),
        );
        r.register(
            "backend_request_duration_seconds",
            "Calls to the model server by outcome.",
            m.backend.clone(),
        );
        r.register("build_info", "Gateway version and batching policy.", info);
        Self { registry: r, ..m }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        prometheus_client::encoding::text::encode(&mut out, &self.registry)
            .expect("writing to a String cannot fail");
        out
    }
}
