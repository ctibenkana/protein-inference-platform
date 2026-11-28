#!/usr/bin/env python3
"""Generate deploy/manifests/observability/dashboards/gateway.json.

The dashboard is written as code so every panel shares the same datasource,
layout and style; edit this file and re-run it rather than editing the JSON.

    python3 scripts/make_gateway_dashboard.py
"""

import json
import pathlib

OUT = pathlib.Path(__file__).resolve().parent.parent / "deploy/manifests/observability/dashboards/gateway.json"
PROM = {"type": "prometheus", "uid": "prometheus"}
LOKI = {"type": "loki", "uid": "loki"}

panels = []


def add(panel, x, y, w, h):
    panel["gridPos"] = {"x": x, "y": y, "w": w, "h": h}
    panel["id"] = len(panels) + 1
    panels.append(panel)


def target(expr, legend=None, ref="A"):
    t = {"refId": ref, "expr": expr, "datasource": PROM}
    if legend:
        t["legendFormat"] = legend
    return t


def stat(title, description, expr, unit, steps, decimals=1):
    return {
        "type": "stat",
        "title": title,
        "description": description,
        "datasource": PROM,
        "targets": [target(expr)],
        "fieldConfig": {
            "defaults": {
                "unit": unit,
                "decimals": decimals,
                "noValue": "no traffic",
                "thresholds": {"mode": "absolute", "steps": steps},
            },
            "overrides": [],
        },
        "options": {
            "colorMode": "background",
            "graphMode": "area",
            "textMode": "value",
            "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False},
        },
    }


def series(title, description, targets, unit, threshold=None, stack=False):
    defaults = {"unit": unit, "custom": {"lineWidth": 2, "fillOpacity": 10}}
    if stack:
        defaults["custom"]["stacking"] = {"mode": "normal"}
    if threshold is not None:
        # A dashed red line at the objective or limit.
        defaults["custom"]["thresholdsStyle"] = {"mode": "dashed"}
        defaults["thresholds"] = {
            "mode": "absolute",
            "steps": [{"color": "transparent", "value": None}, {"color": "red", "value": threshold}],
        }
    return {
        "type": "timeseries",
        "title": title,
        "description": description,
        "datasource": PROM,
        "targets": targets,
        "fieldConfig": {"defaults": defaults, "overrides": []},
        "options": {"legend": {"displayMode": "list", "placement": "bottom"}, "tooltip": {"mode": "multi"}},
    }


red_green = lambda good: [{"color": "red", "value": None}, {"color": "green", "value": good}]  # noqa: E731
green_red = lambda bad: [{"color": "green", "value": None}, {"color": "red", "value": bad}]  # noqa: E731

# --- row 1: SLOs and the two dials that drive them -----------------------------
add(stat("Availability, 7 days (SLO ≥ 99%)",
         "Requests answered without a 5xx. 4xx (callers' mistakes) count as good; 503 for a full queue counts as bad.",
         "gateway:availability:7d", "percentunit", red_green(0.99), 2), 0, 0, 6, 5)
add(stat("Within 1 s, 7 days (SLO ≥ 95%)",
         "Successful requests answered within 1 second.",
         "gateway:latency_good:7d", "percentunit", red_green(0.95)), 6, 0, 6, 5)
add(stat("Padding now (SLO ≤ 10%)",
         "Share of computed token positions that were padding, last 5 minutes. Length-aware batching caps each batch at 10%.",
         "sum(rate(gateway_pad_tokens_total[5m])) / (sum(rate(gateway_tokens_computed_total[5m])) > 0)",
         "percentunit", green_red(0.10)), 12, 0, 6, 5)
add(stat("Queue fill now",
         "Tokens waiting ÷ the admission limit. At 100%, new requests are refused with 503.",
         "sum(gateway_queue_tokens) / sum(gateway_queue_capacity_tokens)",
         "percentunit", [{"color": "green", "value": None}, {"color": "orange", "value": 0.5}, {"color": "red", "value": 0.9}], 0), 18, 0, 6, 5)

# --- row 2: traffic and latency -----------------------------------------------
add(series("Requests by status",
           "Embedding requests per second, by HTTP status.",
           [target("sum by (code) (rate(gateway_request_duration_seconds_count[1m]))", "{{code}}")],
           "reqps", stack=True), 0, 5, 12, 8)
add(series("Latency of successful requests",
           "Percentiles from the gateway's request histogram. The dashed line is the 1 s objective.",
           [target(f'histogram_quantile({q}, sum by (le) (rate(gateway_request_duration_seconds_bucket{{code="200"}}[2m])))', f"p{int(q * 100)}", r)
            for q, r in ((0.5, "A"), (0.9, "B"), (0.99, "C"))],
           "s", threshold=1), 12, 5, 12, 8)

# --- row 3: error-budget burn -----------------------------------------------------
add(series("Availability burn rate",
           "How fast the 1% error budget is burning (1 = exactly on budget). Fast-burn alert at 14.4× over 1 h and 5 min; slow at 6× over 6 h and 30 min.",
           [target("gateway:errors:ratio_rate5m / 0.01", "5 min", "A"),
            target("gateway:errors:ratio_rate1h / 0.01", "1 h", "B"),
            target("gateway:errors:ratio_rate6h / 0.01", "6 h", "C")],
           "x", threshold=14.4), 0, 13, 12, 8)
add(series("Latency burn rate",
           "How fast the 5% budget for requests over 1 s is burning. Same thresholds as availability.",
           [target("gateway:slow:ratio_rate5m / 0.05", "5 min", "A"),
            target("gateway:slow:ratio_rate1h / 0.05", "1 h", "B"),
            target("gateway:slow:ratio_rate6h / 0.05", "6 h", "C")],
           "x", threshold=14.4), 12, 13, 12, 8)

# --- row 4: where the time goes -----------------------------------------------------
add(series("Queue wait vs. model time (p99)",
           "Latency is queueing plus compute: time from arrival to being batched, and the model server's time per batch.",
           [target("histogram_quantile(0.99, sum by (le) (rate(gateway_queue_wait_seconds_bucket[2m])))", "queue wait", "A"),
            target("histogram_quantile(0.99, sum by (le) (rate(gateway_backend_request_duration_seconds_bucket[2m])))", "model server per batch", "B")],
           "s"), 0, 21, 12, 8)
add(series("Queue",
           "Tokens admitted and not yet batched, against the admission limit.",
           [target("sum(gateway_queue_tokens)", "queued tokens", "A"),
            target("sum(gateway_queue_capacity_tokens)", "limit", "B")],
           "short"), 12, 21, 12, 8)

# --- row 5: batching and refusals ---------------------------------------------------
add(series("Batching",
           "Mean sequences per batch (left). Batches form only while the model server is busy, so at low load most hold one sequence.",
           [target("sum(rate(gateway_batch_sequences_sum[2m])) / sum(rate(gateway_batch_sequences_count[2m]))", "sequences per batch", "A"),
            target("sum(rate(gateway_pad_tokens_total[2m])) / sum(rate(gateway_tokens_computed_total[2m])) * 100", "padding %", "B")],
           "short"), 0, 29, 12, 8)
add(series("Refused and dropped",
           "Requests refused before queueing (queue full), and queued sequences dropped without running (deadline passed, caller gone).",
           [target("sum by (reason) (rate(gateway_rejected_total[1m]))", "refused: {{reason}}", "A"),
            target("sum by (reason) (rate(gateway_dropped_total[1m]))", "dropped: {{reason}}", "B")],
           "reqps"), 12, 29, 12, 8)

# --- row 6: model server and logs -------------------------------------------------------
add(series("Model server",
           "Residues embedded per second, and the share of time spent computing (1 = always busy).",
           [target('sum(rate(esm_embed_residues_total{strategy="served"}[1m]))', "residues/s", "A"),
            target('sum(rate(esm_embed_batch_seconds_sum{strategy="served"}[1m]))', "busy share", "B")],
           "short"), 0, 37, 12, 8)
add(series("Model server calls by outcome",
           "Batches sent to the model server. Anything but ok is retried once.",
           [target("sum by (outcome) (rate(gateway_backend_request_duration_seconds_count[1m]))", "{{outcome}}")],
           "reqps", stack=True), 12, 37, 12, 8)
add({
    "type": "logs",
    "title": "Gateway warnings and errors",
    "description": "Gateway log lines above INFO. Request logs carry sizes and timings only; sequences are never logged.",
    "datasource": LOKI,
    "targets": [{"refId": "A", "expr": '{namespace="inference", container="gateway"} |= "level" != "\\"level\\":\\"INFO\\""', "datasource": LOKI}],
    "options": {"showTime": True, "wrapLogMessage": True, "sortOrder": "Descending"},
}, 0, 45, 24, 8)

dashboard = {
    "uid": "embeddings-api",
    "title": "Embeddings API: SLOs, queue and batching",
    "tags": ["slo", "gateway", "protein-inference-platform"],
    "timezone": "browser",
    "schemaVersion": 39,
    "version": 1,
    "editable": True,
    "time": {"from": "now-3h", "to": "now"},
    "refresh": "30s",
    "panels": panels,
}
OUT.write_text(json.dumps(dashboard, indent=2, ensure_ascii=False) + "\n")
print(f"wrote {OUT.relative_to(OUT.parents[4])}: {len(panels)} panels")
