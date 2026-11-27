# EsmEmbedJobSuccessSLOBreached

**Severity:** warning · **SLO:** [embedding job success](../slos.md#1-embedding-job-success), 95% over 7 days

## Meaning
Fewer than 95% of `esm-embed` jobs that ran in the last 7 days succeeded.
Unlike [EsmEmbedJobFailed](EsmEmbedJobFailed.md), which reports one run, this
means failures are a pattern.

## Impact
Users (the lab) can't rely on submitted screens completing. While the SLO is
breached, prioritise reliability work over new features.

## Diagnose
List the failures behind the number. In Prometheus or Grafana Explore:
```promql
max_over_time(kube_job_status_failed{namespace="inference", job_name=~"esm-embed-.+"}[7d]) > 0
```
Then check whether they share a cause:
- the same input or submitter (bad FASTA files)
- the same time window (node problems, spot reclaims, a bad deploy)
- `OOMKilled` (memory limit too low for current batch settings)

For Jobs still in the cluster, use the commands in
[EsmEmbedJobFailed](EsmEmbedJobFailed.md#diagnose).

## Fix
Fix the shared cause. Common ones:
- **Input validation errors:** give users `esm-embed --plan-only` to check files before submitting.
- **Spot reclaims:** set `system_spot` and the worker pool to on-demand for critical screens.
- **Memory:** lower the default `--max-tokens` or raise the limit.

## Afterwards
The alert clears once the 7-day ratio is back above 95%, which can take days.
Record the cause and fix in the incident notes.
