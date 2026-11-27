# Service level objectives

Three objectives cover what the platform's users (initially one research lab)
depend on: that screens finish, that they don't waste money, and that
operators can see what happened when they don't.

All windows are **7 days**, because Prometheus keeps 7 days of data. Rules
live in [`deploy/manifests/observability/`](../deploy/manifests/observability)
and are unit-tested with `promtool` in
[`tests/prometheus/`](../tests/prometheus) (`make test-rules`).

| # | Objective | Target | SLI (recording rule) | Alert |
|---|---|---|---|---|
| 1 | Embedding jobs succeed | ≥ 95% of jobs | `esm_embed:job_success_ratio:7d` | [EsmEmbedJobSuccessSLOBreached](runbooks/EsmEmbedJobSuccessSLOBreached.md) |
| 2 | Compute isn't wasted on padding | ≤ 10% of computed tokens | `esm_embed:padding_ratio:7d` | [EsmEmbedPaddingBudgetExceeded](runbooks/EsmEmbedPaddingBudgetExceeded.md) |
| 3 | Logs are collected and stored | ≥ 99.5% of the time | `logging:availability:7d` | [LogPipelineErrorBudgetBurn](runbooks/LogPipelineErrorBudgetBurn.md) (fast and slow) |

## 1. Embedding job success

**SLI:** jobs that succeeded ÷ jobs that finished (succeeded or failed), over
7 days, counting each `esm-embed-*` Job in the `inference` namespace once.

**Why 95%, not 99%:** at lab scale there are tens of jobs a week, not
thousands. With 30 jobs, 99% would allow zero failures, so a single spot-node
reclaim would breach it. 95% allows about one failure in 20, which is still
strict enough that a pattern of failures shows quickly.

**Design notes:**
- Jobs are deleted 24 hours after they finish, but their metrics stay in
  Prometheus for 7 days, so `max_over_time(...[7d])` still counts them.
- When no jobs ran, the SLI has **no value**, not 0%. A quiet week must not
  look like an outage (unit-tested).
- A single failure also raises [EsmEmbedJobFailed](runbooks/EsmEmbedJobFailed.md)
  immediately, so it's handled before it threatens the objective.

## 2. Batching efficiency

**SLI:** padding token positions ÷ all computed token positions, across all
jobs in 7 days.

This is a **cost objective**. A transformer computes padding positions like
real residues, so padding is paid for in GPU or CPU time and produces nothing.
It's the operational form of the platform's research question: how should
sequences be scheduled to minimise cost per protein?

**Why 10%:** the first benchmark measured 42% padding with fixed batches and
5% with length-aware batching. 10% leaves room for awkward inputs while still
flagging anyone batching naively.

**Design note:** each job pod has its own counters, which vanish when the pod
does. Taking each series' maximum over 7 days (its final value) counts
short-lived jobs in full, where `increase()` would miss each pod's first
sample (unit-tested).

## 3. Log pipeline availability

**SLI:** the fraction of time in which **both** Alloy (collection) and Loki
(storage) are up. A component with no scrape target at all counts as down.

**Why it matters here:** logs are how failures get investigated, and Alloy
also hosts the lab-data filter (see
[PossibleSequenceInLogs](runbooks/PossibleSequenceInLogs.md)).

**Alerting: multi-window, multi-burn-rate.** The error budget is 0.5% of 7
days, about 50 minutes. Rather than alerting on any downtime, alerts fire on
how fast the budget is burning:

| Alert | Burn rate | Long window | Short window | Budget gone in | Severity |
|---|---|---|---|---|---|
| Fast | 14.4× | 1 hour | 5 minutes | about 12 hours | critical |
| Slow | 6× | 6 hours | 30 minutes | about 1 day | warning |

The long window ignores blips (a 3-minute outage doesn't page; unit-tested).
The short window makes the alert clear soon after recovery.

## Other alerts

Symptom alerts with no SLO behind them:

| Alert | Fires when |
|---|---|
| [EsmEmbedJobFailed](runbooks/EsmEmbedJobFailed.md) | an embedding job fails |
| [EsmEmbedJobStalled](runbooks/EsmEmbedJobStalled.md) | a running job hasn't progressed for 15 minutes |
| [EsmEmbedJobRunningTooLong](runbooks/EsmEmbedJobRunningTooLong.md) | a job passes 45 of its 60 allowed minutes |
| [EsmEmbedHighPadding](runbooks/EsmEmbedHighPadding.md) | a running job wastes over 25% on padding (info) |
| [PossibleSequenceInLogs](runbooks/PossibleSequenceInLogs.md) | a service logged something that looks like a protein sequence (critical) |
| [ArgoCDAppUnhealthy](runbooks/ArgoCDAppUnhealthy.md) | a GitOps application is unhealthy for 15 minutes |
| [ArgoCDAppOutOfSync](runbooks/ArgoCDAppOutOfSync.md) | the cluster hasn't matched git for 30 minutes |

kube-prometheus-stack's standard Kubernetes alerts (crash-looping pods, full
volumes, and so on) remain enabled alongside these.

## Known issues

- **Grafana cold start on a single small node.** When every pod starts at
  once (after a node or Docker restart), Grafana's health checks can time
  out and it restarts a few times before settling. Seen on the local
  cluster; it recovers on its own. A separate startup probe would fix it,
  but the chart doesn't expose one for the Grafana container.

## Notifications

Alertmanager sends alerts to a Discord channel. Routing is in
`deploy/values/kube-prometheus-stack.yaml`:

| Severity | Notified | First message | Repeats while unresolved |
|---|---|---|---|
| critical | yes | within about 10 seconds | hourly |
| warning | yes | after a 30-second grouping window | every 4 hours |
| info | no (dashboards only) | — | — |

- A resolved message follows each problem.
- Alerts with the same name and namespace are grouped into one message.
- A critical alert suppresses the warning for the same problem, and the fast
  burn-rate alert suppresses the slow one.
- Every message carries the alert's runbook link.
- The webhook URL is a secret: it lives only in the Kubernetes Secret
  `monitoring/alertmanager-discord` (`make discord-secret`, which prompts
  without echoing). If the Secret is missing, Alertmanager still runs; it
  just can't deliver.
- `make test-notification` sends a test alert end to end.

### Dead man's switch

Alerting can't report its own failure. To cover that, the always-firing
`Watchdog` alert is routed to a healthchecks.io ping URL about once a
minute. healthchecks.io expects a ping at least every 5 minutes (plus 5
minutes' grace) and otherwise reports the check DOWN to the same Discord
channel. That covers Prometheus, Alertmanager, the cluster or its network
going down. See [the runbook](runbooks/DeadMansSwitch.md).

The ping URL lives only in the Secret `monitoring/alertmanager-deadman`
(`make deadman-secret`). Pause the check at healthchecks.io before stopping
the cluster on purpose.

## Not yet in place

- **Longer windows.** 30-day SLOs need longer retention (or a long-term store);
  7 days matches what Prometheus currently keeps.
