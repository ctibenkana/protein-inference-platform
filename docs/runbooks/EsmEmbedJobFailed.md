# EsmEmbedJobFailed

**Severity:** warning · **Source:** `kube_job_status_failed` (kube-state-metrics)

## Meaning
An `esm-embed-*` Job in the `inference` namespace has a failed pod. The Job
allows one retry (`backoffLimit: 1`), so this fires on the first failure and
the run may still recover on its retry.

## Impact
Embeddings for this run were not produced. Repeated failures consume the
[job-success SLO](../slos.md#1-embedding-job-success) error budget.

## Diagnose
```bash
JOB=<job_name from the alert>
kubectl -n inference describe job $JOB          # conditions, events, deadline
kubectl -n inference get pods -l job-name=$JOB  # pod status and reason
kubectl -n inference logs -l job-name=$JOB --tail=50
```

| What you see | Likely cause |
|---|---|
| Exit code 2, log says `invalid input` | Bad FASTA: non-amino-acid characters, empty records or duplicate ids |
| `OOMKilled` | Batch too large for the memory limit |
| `DeadlineExceeded` | Ran past 60 minutes (see [EsmEmbedJobRunningTooLong](EsmEmbedJobRunningTooLong.md)) |
| `ImagePullBackOff` / `ErrImagePull` | Wrong image tag, or registry unreachable |
| Pod evicted, or node gone | Spot node reclaimed; resubmitting usually succeeds |

Logs never contain sequences or record ids. To locate a bad record, run
`esm-embed --plan-only` on the input file locally: it validates without
loading the model.

## Fix
- **Bad input:** fix the FASTA and resubmit.
- **OOMKilled:** resubmit with a smaller `--max-tokens` (or `--batch-size`),
  or raise `resources.limits.memory` in `workloads/esm-embed/k8s/job.yaml`.
- **Spot reclaim:** resubmit. If it recurs, check spot availability in the zone.
- **Image:** check the tag exists: `docker buildx imagetools inspect <image>`.

## Afterwards
Failed Jobs are deleted 24 hours after finishing (`ttlSecondsAfterFinished`).
Their metrics remain in Prometheus for 7 days.
