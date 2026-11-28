# GatewayAvailabilityBurnFast / GatewayAvailabilityBurnSlow

**Severity:** critical (fast) / warning (slow) · **SLO:** [embeddings API availability](../slos.md#4-embeddings-api-availability), 99% of requests without a 5xx over 7 days

## Meaning
The embeddings API is answering too many requests with a 5xx, so the SLO's
error budget is draining:

| Alert | Condition | Budget gone in |
|---|---|---|
| Fast | over 14.4% of requests failed in the last hour, and in the last 5 minutes | about 12 hours |
| Slow | over 6% failed in the last 6 hours, and in the last 30 minutes | about 1 day |

Both need some traffic (30 requests an hour, 60 in 6 hours), so a handful of
failures at a quiet time doesn't alert. 4xx answers are callers' mistakes
and don't count.

## Impact
Lab users' scripts get errors. Well-behaved clients retry after
`Retry-After`, so short bursts may go unnoticed; sustained ones don't.

## Diagnose
Which 5xx? The response's `error.code` tells them apart, and so do metrics:

```promql
sum by (code) (rate(gateway_request_duration_seconds_count[5m]))
sum by (reason) (rate(gateway_rejected_total[5m]))             # 503 overloaded
sum by (outcome) (rate(gateway_backend_request_duration_seconds_count[5m]))
```

| What you see | Meaning | Go to |
|---|---|---|
| `gateway_rejected_total{reason="queue_full"}` rising | **Overloaded:** more work than the model server can do; the queue limit is refusing the excess (503 `overloaded`) | [Overload](#overload) |
| backend outcome `unreachable` or `server_error` | **Model server failing:** 503 `backend_unavailable` after one retry | [Model server](#model-server-failing) |
| backend outcome `invalid_response`, or 500s | **A bug:** the model server answered something the gateway didn't expect | [Bug](#bug) |
| 504s | Deadlines passing; usually overload, see the latency runbook | [GatewayLatencyBurn](GatewayLatencyBurn.md) |

```bash
kubectl -n inference get pods -l app.kubernetes.io/name=embedding-service
kubectl -n inference logs deploy/embedding-service -c gateway --since=15m | grep -v '"level":"INFO"'
kubectl -n inference logs deploy/embedding-service -c model-server --since=15m
```

## Fix

### Overload
Demand exceeds capacity. Measured capacity per replica is in the
[load tests](../benchmarks/gateway-load-tests.md); compare with
`sum(rate(gateway_sequences_total[5m]))`.

- Short term: nothing is broken; refusals protect latency for accepted work.
  Ask heavy users to send bulk work to the batch job (`make embed-job`), not
  the online API.
- If demand is lasting: add a replica (`replicas` in
  `deploy/manifests/inference/embedding-service.yaml`), or more CPU threads
  per model server. Don't simply raise `GATEWAY_MAX_QUEUE_TOKENS`: it turns
  fast refusals into slow answers (E2 in the load tests).

### Model server failing
Check the `model-server` container: `OOMKilled` (raise its memory limit),
crash loops after a new image (roll back the digest in git), or stuck
batches (liveness restarts it). The gateway retries each failed batch once,
so failures here are persistent, not blips.

### Bug
`invalid_response` means the gateway and model server disagree about the
protocol, usually after updating one image without the other. Roll back the
last image change in `embedding-service.yaml`.

## Afterwards
```promql
gateway:availability:7d
```
Below 99%, prioritise capacity or reliability work over new features.
