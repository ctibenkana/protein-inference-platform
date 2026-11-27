# LogPipelineErrorBudgetBurnFast / LogPipelineErrorBudgetBurnSlow

**Severity:** critical (fast) / warning (slow) · **SLO:** [log pipeline availability](../slos.md#3-log-pipeline-availability), 99.5% over 7 days

## Meaning
Alloy (collects logs) or Loki (stores them) has been down long enough that the
SLO's error budget is draining:

| Alert | Condition | Budget gone in |
|---|---|---|
| Fast | down more than 7.2% of the last hour, and still down in the last 5 minutes | about 12 hours |
| Slow | down more than 3% of the last 6 hours, and in the last 30 minutes | about 1 day |

Requiring both a long and a short window means short blips don't alert, and
the alert clears soon after recovery.

## Impact
Logs produced while the pipeline is down are delayed or lost, which makes
incidents harder to investigate. **The sequence filter also lives in Alloy**,
but when Alloy is down nothing is shipped at all, so no unfiltered logs reach
storage.

## Diagnose
```bash
kubectl -n monitoring get pods -l 'app.kubernetes.io/name in (alloy, loki)'
kubectl -n monitoring describe pod <the unhealthy pod>
kubectl -n monitoring logs deploy/alloy --tail=50
kubectl -n monitoring logs statefulset/loki --tail=50
kubectl -n argocd get application alloy loki   # a bad sync?
```

| What you see | Likely cause |
|---|---|
| `OOMKilled` | Log volume spike; memory limit too low |
| Loki `CrashLoopBackOff`, disk errors | Loki's 10 Gi volume is full |
| Pod `Pending` | System node out of CPU or memory |
| Started right after a commit | A bad values change; check the Argo CD sync |

## Fix
- **Full volume:** shorten `limits_config.retention_period` in
  `deploy/values/loki.yaml`, or increase `singleBinary.persistence.size`.
- **OOM:** raise the memory limit in the chart's values file and push.
- **Bad change:** revert the commit; Argo CD syncs the previous version.

## Afterwards
Check how much error budget remains:
```promql
logging:availability:7d
```
Below 99.5%, prioritise pipeline reliability work.
