# EsmEmbedJobRunningTooLong

**Severity:** warning · **Source:** `kube_job_status_start_time`, `kube_job_status_active`

## Meaning
An embedding job has been running for over 45 minutes. Kubernetes stops it at
60 minutes (`activeDeadlineSeconds: 3600`) and marks it failed.

## Impact
If it doesn't finish in time, the whole run is lost: outputs are written at
the end.

## Diagnose
Is it making progress, or stuck?
```promql
esm_embed_progress_ratio{pod=~"<job_name>-.*"}
```
- **Progressing steadily:** the input is simply too large for one Job at this throughput.
- **Flat:** treat as [EsmEmbedJobStalled](EsmEmbedJobStalled.md).

## Fix
- **Too large:** you can't extend a running Job's deadline. Let it finish if the
  remaining progress fits in the time left; otherwise split the input into
  several FASTA files and submit one Job per file.
- **Slow node:** check that the pod is on a worker node, not the small system
  node: `kubectl -n inference get pod -o wide`.

## Afterwards
If large screens are normal, raise `activeDeadlineSeconds` in
`workloads/esm-embed/k8s/job.yaml` and this alert's threshold together
(keep the alert at 75% of the deadline).
