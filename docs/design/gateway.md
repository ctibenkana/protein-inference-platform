# Design: inference gateway

**Status:** built (steps 1–6) · **Owner:** platform · **Last updated:** 2026-11-28

## Problem

ESM-2 embedding runs today as a batch Job: a FASTA file in, a file of vectors
out. The lab will also want embeddings *on request*: a script or notebook
sends a few proteins and waits for the vectors. Serving that well has the
same cost problem the batch job solved, in a harder form:

- Each request carries only a few sequences, so running each request as its
  own batch leaves the model under-used. Combining requests helps
  throughput, but makes every caller wait for the slowest member of the batch.
- Proteins vary from ~50 to over 1,000 residues, so combining requests
  blindly brings back the padding waste that the batch job cut from 42% to 5%.
- With a small CPU (later GPU) budget, overload is the normal state at peaks,
  not an exception. The service has to say "not now" quickly rather than
  queue forever.

## Goals

1. An HTTP API for embeddings that a lab script can call directly.
2. **Dynamic, length-aware batching** across requests, keeping padding within
   the existing 10% padding SLO while bounding the wait it adds.
3. **Explicit overload behaviour:** a bounded queue, fast rejection, deadlines,
   and no compute spent on requests whose caller has gone.
4. Latency, padding and availability measured live and covered by SLOs, alerts
   and runbooks, like the rest of the platform.
5. A backend interface that the vLLM stage plugs into later.

**Not goals now:** authentication and per-user quotas (next, once the lab
uses it), GPUs, autoscaling, multiple models.

## Architecture

```
 lab script ──HTTP/JSON──▶ gateway (Rust) ──HTTP──▶ esm-embed server (Python)
                           │ validate                │ runs exactly the batch
                           │ admit or reject         │ it is given, one at a time
                           │ queue (bounded)         │
                           │ batch (length-aware)    │
                           │ dispatch, retry once    │
                           │ split results           │
                           └─ /metrics ─▶ Prometheus ◀─ /metrics
```

The scheduling decisions live in the gateway; the model server only does
arithmetic. That split is the same as vLLM's (scheduler in front of the
engine), keeps the Python side small, and lets the batching policy be
tested without a model.

## API

`POST /v1/embeddings`

```json
{ "sequences": ["MKTAYIAKQR...", "MSEQNNTEM..."],
  "encoding_format": "float" }
```

```json
{ "model": "facebook/esm2_t6_8M_UR50D@c731040",
  "dim": 320,
  "data": [ { "index": 0, "embedding": [0.012, -0.443, ...] },
            { "index": 1, "embedding": [ ... ] } ],
  "usage": { "sequences": 2, "residues": 1834 } }
```

- Shaped like the OpenAI embeddings API, so existing clients and habits carry over.
  `encoding_format: "base64"` returns little-endian float32, which is about four
  times smaller than JSON numbers.
- Results are in request order; each sequence is embedded independently, so a
  vector doesn't depend on what it was batched with (already covered by the
  invariance test in esm-embed).
- Optional header `X-Request-Timeout: <seconds>` (default 30, max 120).

| Status | When |
|---|---|
| 400 | malformed JSON or unknown fields |
| 413 | more than 64 sequences, or more than 64k residues, in one request |
| 422 | a sequence is empty, too long (> 1,022 residues), or has characters outside the amino-acid alphabet; the error names the **index**, never the sequence |
| 503 + `Retry-After` | queue full, or no healthy backend |
| 504 | deadline passed before the batch completed |

## Batching

Each request is split into one work item per sequence. Items wait in one
queue, and a dispatcher forms a batch whenever a backend slot is free and
either:

- the queued tokens could fill the **token budget** (batch size × (longest
  + 2) ≤ `max_batch_tokens`, default 8,192, the same rule as the batch job), or
- the **oldest item has waited `max_wait`** (default 0: load tests found no
  benefit in waiting; see below).

While every slot is busy, nothing is sent, so batches fill up by themselves
under load; at low load a lone request waits at most `max_wait`.

**Which items go together (`length-aware`):** the oldest item anchors the
batch; then, repeatedly, the item that adds the least padding joins, as long
as the batch stays within the token budget **and its padding stays within
`max_padding`** (default 10%). Consequences, each checked by a property test:

- *No starvation.* Every batch contains the oldest item left, so each item
  runs after at most as many batches as there are items ahead of it.
- *Padding bounded by construction.* No batch exceeds the cap, so total
  padding can't either: with the cap at 10%, the platform's 10% padding SLO
  holds for live traffic whatever the mix of lengths.
- *Full batches.* When a batch is sent, nothing left in the queue could have
  joined it within the limits.

The cap matters for mixed queues: without it, a 1,000-residue protein at the
front would take a handful of 50-residue proteins along, and about 95% of
that batch would be padding. With it, they wait for a batch of their own.

**Baseline (`fifo`):** items in arrival order up to the same token budget, no
padding cap. Both policies are selectable at start-up (`--batching`) so they
can be compared under the same load, as the batch job compares `fixed` with
`token_budget`.

A request's response is sent when all of its items are done, so a request
can span several batches, and each item's result is routed back through its
own channel, so batches can mix requests.

**First measurement** (laptop CPU, ESM-2 8M with 4 threads, one gateway, 300
human proteins sent as 300 simultaneous single-sequence requests, two runs
each in opposite orders):

| Policy | Padding | All 300 done | Median latency | p99 latency |
|---|---|---|---|---|
| fifo | 43–44% | 31–40 s | 15–21 s | 31–39 s |
| length-aware | 4.4–4.7% | 15.4 s | 6.3–6.4 s | 15 s |

**Under continuous load** ([load tests](../benchmarks/gateway-load-tests.md)),
length-aware batching carries 3–4× the load of fifo at the same latency.
fifo falls into a feedback loop: mixed batches add padding, padding adds
work, work lengthens the queue, a longer queue makes bigger, more mixed
batches. With `max_wait` at 0, batches form only while the model server is
busy, so batching costs nothing at low load.

**Deployment: one gateway per model server.** Running two gateway
replicas in front of one model server made p99 3–4× worse in the cluster
([E5](../benchmarks/gateway-load-tests.md#e5-in-the-cluster)): each batched
half the traffic, and their batches then queued first-come, first-served in
the model server. So the gateway and the model server share a pod, one to
one; the model server is a native sidecar that starts first and stops after
the gateway has drained, and listens on localhost only. Scaling out adds
both together. Spreading traffic across several such pods by queue length,
rather than by connection, is the next step once there are several.

## Overload and failure

| Situation | Behaviour |
|---|---|
| Queue would exceed `max_queue_tokens` | Refused with 503 `overloaded` + `Retry-After` in milliseconds; never queued. |
| Item's deadline passes while queued | Dropped before it is batched; request gets 504. |
| Caller disconnects or gives up | Its items are dropped before batching. |
| Model server connection error or 5xx | The batch is retried once (embedding is idempotent), then every request in it gets 503. |
| Model server down or loading | Requests get 503 + `Retry-After`. The gateway's own readiness does **not** follow the model server: if it did, one model-server restart would remove every gateway replica from the Service and callers would see connection errors instead of a clear answer. |
| Shutdown (SIGTERM) | Readiness fails at once; serving continues for `shutdown_delay` while endpoints update; then no new connections, and requests in progress, including their queued sequences, get up to `drain_timeout` to finish. |

**Admission control.** Space is reserved for a whole request before any of
it is queued, so a request is queued completely or not at all. It is counted
at admission, not when the dispatcher picks items up, so a burst of
simultaneous requests can't overshoot the limit; space is released as each
sequence is batched or dropped. A stress test sends 200 simultaneous
requests at a small queue and checks that every request is answered or
refused, the model runs exactly the accepted work, and the counter returns
to zero.

The limit trades latency for accepted work. Same 300-request burst on a
laptop CPU:

| `max_queue_tokens` | Accepted | Refused | Slowest accepted | Refusal answered within |
|---|---|---|---|---|
| 200,000 (holds everything) | 300 | 0 | 8.4 s | n/a |
| 30,000 | 83 | 217 | 2.0 s | 6 ms |

A deadline-aware limit (refuse what can't finish before its deadline, from
the measured drain rate) would be the next refinement.

## Privacy

As elsewhere on the platform: no sequences or caller identifiers in logs,
metrics, traces or error messages. Errors refer to sequence *indices*. The
log pipeline's sequence detector stays in place as a second line of defence,
and a test checks that error bodies never echo input.

## Observability and SLOs

Metrics (gateway): request latency by status, queue wait, batch size and
tokens, **padding fraction (live)**, queue depth in tokens, rejections by
reason, backend latency and errors.

SLOs over 7 days; the latency target comes from the load tests, not a guess:

| SLO | Target |
|---|---|
| Availability: requests answered without 5xx | ≥ 99% |
| Latency: requests answered within 1 s (planned load: 20 single-protein requests/s per model-server replica; [measured](../benchmarks/gateway-load-tests.md)) | ≥ 95% |
| Padding: share of computed tokens that are padding | ≤ 10% (existing cost SLO) |

## Implementation plan

Each step ends with something running on the kind cluster.

1. **Model server:** `esm-embed serve`, which runs given batches over HTTP.
   Stdlib HTTP only, no new dependencies.
2. **Gateway skeleton (Rust):** the API, validation, one request = one batch,
   metrics, logs, health checks, container image, Kubernetes manifests, Argo CD.
3. **Dynamic batching:** queue, dispatcher, both policies; property tests
   (nothing lost or duplicated, budget respected, oldest item always included).
4. **Overload:** bounded queue, deadlines, cancellation, retries, graceful shutdown.
5. **Load test:** a small Rust load generator replaying real length
   distributions at Poisson arrival rates; `fifo` vs `length_aware`
   (throughput, padding, p50/p99). Results set the latency SLO.
6. **SLOs, alerts, runbooks, dashboard, CI** (fmt, clippy, tests, cargo-deny,
   signed multi-arch image).

## Choices made

| Choice | Alternative | Why |
|---|---|---|
| Rust (axum, tokio) | Python (FastAPI) | Predictable latency under load, no GIL in the scheduler; matches the target roles. |
| Separate model server | Model inside the gateway (candle, ONNX) | Mirrors the vLLM split; the model stack stays PyTorch, as researchers use it. |
| Queue measured in tokens | Measured in requests | Cost follows residues, not request count. |
| Reject at a bound (503) | Unbounded queue | A late answer is a failure anyway; fast rejection lets clients back off. |
