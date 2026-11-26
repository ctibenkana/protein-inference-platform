"""Prometheus metrics, served on a port while the job runs."""

from __future__ import annotations

from prometheus_client import Counter, Gauge, Histogram, start_http_server

LABELS = ["strategy"]

sequences = Counter("esm_embed_sequences_total", "Sequences embedded.", LABELS)
residues = Counter("esm_embed_residues_total", "Real residues processed.", LABELS)
tokens_computed = Counter(
    "esm_embed_tokens_computed_total",
    "Token positions computed, including padding.",
    LABELS,
)
pad_tokens = Counter(
    "esm_embed_pad_tokens_total", "Token positions spent on padding.", LABELS
)
batch_seconds = Histogram(
    "esm_embed_batch_seconds",
    "Wall time per batch.",
    LABELS,
    buckets=(0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60, 120),
)
residues_per_second = Gauge(
    "esm_embed_residues_per_second", "Throughput of the most recent batch.", LABELS
)
run_info = Gauge(
    "esm_embed_run_info", "Static run information.", ["strategy", "model", "version"]
)
progress = Gauge("esm_embed_progress_ratio", "Fraction of sequences done.", LABELS)


def serve(port: int) -> None:
    if port > 0:
        start_http_server(port)
