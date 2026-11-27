# EsmEmbedJobStalled

**Severity:** warning · **Source:** `esm_embed_progress_ratio`

## Meaning
An embedding job's progress hasn't changed for at least 15 minutes (10-minute
window plus 5 minutes pending) and it isn't finished. A single batch normally
takes seconds.

## Impact
The run will hit its 60-minute deadline and fail without producing results,
while still holding a node, and cost.

## Diagnose
```bash
POD=<pod from the alert>
kubectl -n inference describe pod $POD     # restarts, OOM, node conditions
kubectl -n inference logs $POD --tail=20   # last completed batch
kubectl top pod -n inference $POD          # CPU near zero = stuck; at limit = slow
kubectl describe node "$(kubectl -n inference get pod $POD -o jsonpath='{.spec.nodeName}')"
```

| What you see | Likely cause |
|---|---|
| CPU at its limit, memory near its limit | Swapping or throttling on a huge batch |
| CPU near zero | Deadlock, or waiting on I/O (output volume full?) |
| Node `NotReady` or under pressure | Node problem; spot node being reclaimed |

## Fix
Delete the Job and resubmit, with a smaller `--max-tokens` if the batch was huge:
```bash
kubectl -n inference delete job <job_name>
make embed-job
```

## Afterwards
If it recurs with the same input, collect the input's length distribution
(`esm-embed --plan-only` prints batch statistics) and open an issue.
