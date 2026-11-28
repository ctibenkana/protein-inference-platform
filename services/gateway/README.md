# gateway

An HTTP API for protein embeddings, written in Rust, in front of the ESM-2
[model server](../../workloads/esm-embed#model-server). The gateway owns the
scheduling decisions (validation, deadlines, retries and dynamic
batching); the model server only runs the batches it is given.

Design and plan: [docs/design/gateway.md](../../docs/design/gateway.md).

> **Status:** step 3 of 6. Dynamic, length-aware batching across requests:
> with 300 proteins arriving as 300 simultaneous requests, padding fell from
> 43% (FIFO) to under 5% and all requests finished 2–2.6× sooner. Overload
> protection (bounded queue, fast rejection) is next.

## API

```bash
curl -s gateway/v1/embeddings -H 'content-type: application/json' \
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
| 413 | over 64 sequences or 65,536 residues in one request |
| 422 | empty sequence, over 1,022 residues, or not amino-acid letters; the message names the position |
| 503 + `Retry-After` | model server unavailable (after one retry) |
| 504 | deadline passed |

On the cluster, try it with `make gateway-demo`.

## Batching

Sequences from different requests share batches. The oldest queued sequence
always goes in the next batch (nothing starves); the rest are the ones that
add the least padding, up to a token budget and a **padding cap** (10%), so
the padding SLO holds by construction. A lone request at low load waits at
most `--max-wait-ms` (10 ms). See the
[design](../../docs/design/gateway.md#batching) for the rules and their
property tests.

| Flag | Default | Meaning |
|---|---|---|
| `--batching` | `length-aware` | or `fifo`, the baseline for comparisons |
| `--max-batch-tokens` | 8192 | batch size × (longest + 2) |
| `--max-padding` | 0.10 | highest padding share of a length-aware batch |
| `--max-wait-ms` | 10 | longest a sequence waits for company |
| `--backend-concurrency` | 1 | batches in flight (model-server replicas) |

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
- **Shutdown.** On SIGTERM readiness fails at once, the gateway keeps serving
  for `--shutdown-delay` while endpoints update, then drains in-flight
  requests.
- **Metrics** on a separate port (9100): latency by status, queue depth and
  wait, why each batch was sent, dropped items, batch sizes, backend
  outcomes, and tokens computed vs. padding, so the platform's padding SLO
  covers live traffic too.
- **Abandoned work is never run.** Items whose caller gave up or whose
  deadline passed are dropped from the queue before batching.

## Build and run

```bash
cargo test                                   # unit + integration tests (fake model server)
cargo run -- --backend-url http://127.0.0.1:8000 --log-format text
```

All flags can be set as `GATEWAY_*` environment variables; see `--help`.

The image is a static musl binary on `distroless/static` (no shell, no libc,
non-root), cross-compiled for amd64 and arm64 without emulation, and signed
with Sigstore by CI.
