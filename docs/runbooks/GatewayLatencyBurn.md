# GatewayLatencyBurnFast / GatewayLatencyBurnSlow

**Severity:** critical (fast) / warning (slow) · **SLO:** [embeddings API latency](../slos.md#5-embeddings-api-latency), 95% of successful requests within 1 s over 7 days

## Meaning
Too many successful requests are taking over 1 second:

| Alert | Condition | Budget gone in |
|---|---|---|
| Fast | over 72% slower than 1 s in the last hour, and in the last 5 minutes | about 12 hours |
| Slow | over 30% slower than 1 s in the last 6 hours, and in the last 30 minutes | about 1 day |

Both need some traffic (30 requests an hour, 60 in 6 hours).

## Impact
Interactive users wait; scripts with short timeouts start failing.

## Diagnose
Latency is queueing plus compute. Find which one grew:

```promql
# time waiting to be batched
histogram_quantile(0.99, sum by (le) (rate(gateway_queue_wait_seconds_bucket[5m])))
# time in the model server per batch
histogram_quantile(0.99, sum by (le) (rate(gateway_backend_request_duration_seconds_bucket[5m])))
# how full the queue is
gateway_queue_tokens / gateway_queue_capacity_tokens
# demand vs. the measured capacity
sum(rate(gateway_sequences_total[5m]))
# padding (should stay under 10% with length-aware batching)
rate(gateway_pad_tokens_total[5m]) / rate(gateway_tokens_computed_total[5m])
```

| What you see | Likely cause |
|---|---|
| Queue wait high, queue often full, demand near capacity | **Load:** close to or past capacity (latency rises steeply near it; see E1 in the load tests) |
| Backend time high at normal demand | **Slower model server:** CPU contention on the node, a slower node, or a larger model |
| Padding well over 10% | Batching misconfigured (`GATEWAY_BATCHING=fifo`, or `GATEWAY_MAX_PADDING` raised) |
| Requests carry many long sequences | **Heavier requests:** bulk work sent to the online API |

```bash
kubectl -n inference get pods -l app.kubernetes.io/name=embedding-service -o wide
kubectl top pod -n inference --containers   # if metrics-server is installed
kubectl -n inference logs deploy/embedding-service -c gateway | grep "gateway started"
```

## Fix
- **Load:** add a replica, or more CPU (threads plus request) per model
  server. Lowering `GATEWAY_MAX_QUEUE_TOKENS` bounds latency for accepted
  requests at the cost of refusing more (E2), which moves budget burn from
  this SLO to availability; it's a choice, not a fix.
- **Slower model server:** check the node; on GKE, check spot preemptions
  and the machine type.
- **Batching settings:** restore the defaults (length-aware, 10% cap).
- **Heavy requests:** point bulk users at the batch job.

## Afterwards
```promql
gateway:latency_good:7d
```
