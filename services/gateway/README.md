# gateway

An HTTP API for protein embeddings, written in Rust, in front of the ESM-2
[model server](../../workloads/esm-embed#model-server). The gateway owns the
scheduling decisions (validation, deadlines, retries and dynamic
batching); the model server only runs the batches it is given.

Design and plan: [docs/design/gateway.md](../../docs/design/gateway.md).

> **Status:** all 6 steps of the [plan](../../docs/design/gateway.md#implementation-plan) built
> and running on the local cluster. Length-aware batching carries 3–4× the
> load of FIFO at the same latency ([load tests](../../docs/benchmarks/gateway-load-tests.md));
> a bounded queue refuses excess work in milliseconds; availability and
> latency SLOs have burn-rate alerts, runbooks and a dashboard
> ([SLOs](../../docs/slos.md#4-embeddings-api-availability)); dependencies are
> checked daily with cargo-deny.

## API

```bash
curl -s gateway/v1/embeddings -H "Authorization: Bearer $API_KEY" \
  -H 'content-type: application/json' \
  -d '{"sequences": ["MKTAYIAKQRQISFVKSHFSRQ", "MSEQNNTEMTFQIQRIYTKD"]}'
```

```json
{ "model": "facebook/esm2_t6_8M_UR50D@c731040…", "dim": 320,
  "data": [ { "index": 0, "embedding": [-0.04, -0.336, …] }, … ],
  "usage": { "sequences": 2, "residues": 42 } }
```

- Shaped like the OpenAI embeddings API. `"encoding_format": "base64"` returns
  little-endian float32, about four times smaller.
- `X-Request-Timeout: <seconds>` sets the deadline (default 30, max 120).
- `X-Request-Id` is echoed back (or generated) for tracing a request in logs.

| Status | Meaning |
|---|---|
| 400 | malformed body or bad timeout header |
| 401 `unauthenticated` | missing or unknown API key |
| 413 | over 64 sequences or 8,192 residues in one request (bulk work: use the batch job) |
| 422 | empty sequence, over 1,022 residues, or not amino-acid letters; the message names the position |
| 429 `rate_limited` + `Retry-After` | this key's residues-per-minute allowance is used up; `Retry-After` says when enough has refilled |
| 429 `caller_in_flight` + `Retry-After` | this key already has its share of the queue in progress |
| 503 `overloaded` + `Retry-After` | the shared queue is full; nothing was queued |
| 503 `backend_unavailable` + `Retry-After` | model server unavailable (after one retry) |
| 504 | deadline passed |

On the cluster, try it with `make gateway-demo`. There the gateway runs in
the same pod as the model server, one to one, with the model server as a
native sidecar on localhost (see [the manifest](../../deploy/manifests/inference/embedding-service.yaml)
and why: [E5](../../docs/benchmarks/gateway-load-tests.md#e5-in-the-cluster)).

## Batching

Sequences from different requests share batches. The oldest queued sequence
always goes in the next batch (nothing starves); the rest are the ones that
add the least padding, up to a token budget and a **padding cap** (10%), so
the padding SLO holds by construction. A lone request at low load waits at
most `--max-wait-ms` (default 0: nothing waits when the model server is free; batches form while it is busy). See the
[design](../../docs/design/gateway.md#batching) for the rules and their
property tests.

| Flag | Default | Meaning |
|---|---|---|
| `--batching` | `length-aware` | or `fifo`, the baseline for comparisons |
| `--max-batch-tokens` | 8192 | batch size × (longest + 2) |
| `--max-padding` | 0.10 | highest padding share of a length-aware batch |
| `--max-wait-ms` | 0 | longest a sequence waits for company (load tests found no benefit in waiting) |
| `--backend-concurrency` | 1 | batches in flight (model-server replicas) |
| `--max-queue-tokens` | 16384 | queued tokens before new requests are refused (~1.2 s of work here) |
| `--drain-timeout` | 30 | seconds to finish work in progress on shutdown |
| `--callers-file` | none | API keys (hashes) and limits; required unless `--allow-anonymous` |
| `--default-residues-per-minute` | 120000 | per-caller rate unless the callers file sets one |
| `--default-caller-inflight-tokens` | 12288 | per-caller share of the queue unless set |

## API keys and quotas

Every request needs `Authorization: Bearer <key>`; health checks don't. The
gateway holds only SHA-256 hashes of keys, in a callers file (a Kubernetes
Secret on the cluster) that it re-reads every 30 s, so keys are added and
revoked without a restart:

```bash
make api-key CALLER=lab-01          # prints the new key once, in your terminal
make api-key-revoke CALLER=lab-01
make api-keys                       # callers and limits, never keys
```

Caller ids are pseudonyms (`lab-01`): they appear in metrics and logs, and
who holds which id is kept outside the platform. Each caller has two limits,
both answered with 429 and `Retry-After`:

- **Residues per minute** (default 120,000), as a token bucket holding up to
  a minute's allowance. Cost follows residues, so the limit does too.
- **A share of the queue** (default 12,288 tokens of requests in progress,
  of the shared 16,384), so one heavy caller can't fill the queue and push
  everyone else into 503s.

A request refused by the shared queue (503) doesn't use up the caller's
allowance. Callers marked `synthetic` (the load generator) are labelled
`traffic="synthetic"` in metrics and left out of the SLOs.

## Behaviour worth knowing

- **Privacy.** Sequences never appear in logs, metrics or errors; errors refer
  to positions. Tests fail if an error body ever echoes input, including
  serde's own messages, which quote unknown fields.
- **Retries.** Embedding is idempotent, so a batch that hits a connection
  error or 5xx is retried once within the latest deadline in it. Timeouts are
  not retried. If the retry fails, every request in the batch gets 503.
- **Readiness** reflects only the gateway. It doesn't follow the model server:
  otherwise one model-server restart would remove every gateway replica from
  the Service and callers would get connection errors instead of a 503.
- **Overload.** A request is admitted whole or refused whole, at once, with
  503 and `Retry-After`; the queue never grows without bound. Clients should
  retry with backoff.
- **Shutdown.** On SIGTERM readiness fails at once, the gateway keeps serving
  for `--shutdown-delay` while endpoints update, then finishes requests in
  progress (including queued sequences) for at most `--drain-timeout`.
- **Metrics** on a separate port (9100): latency by status, queue depth and
  wait, why each batch was sent, dropped items, batch sizes, backend
  outcomes, and tokens computed vs. padding, so the platform's padding SLO
  covers live traffic too.
- **Abandoned work is never run.** Items whose caller gave up or whose
  deadline passed are dropped from the queue before batching.

## Load testing

`loadgen` (in this crate, shipped in the image) is an open-loop generator:
Poisson arrivals at fixed rates, latency measured from each request's
scheduled time. `bench/run.sh` reproduces the local experiments;
`make loadtest` runs it as a Job against the deployed gateway. Results and
method: [docs/benchmarks/gateway-load-tests.md](../../docs/benchmarks/gateway-load-tests.md).

## Build and run

```bash
cargo test                                   # unit + integration tests (fake model server)
cargo run -- --backend-url http://127.0.0.1:8000 --log-format text
```

All flags can be set as `GATEWAY_*` environment variables; see `--help`.

The image is a static musl binary on `distroless/static` (no shell, no libc,
non-root), cross-compiled for amd64 and arm64 without emulation, and signed
with Sigstore by CI.
