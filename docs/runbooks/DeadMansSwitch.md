# Dead man's switch (healthchecks.io: check is DOWN)

**Severity:** critical · **Source:** an external check at healthchecks.io, not Prometheus

## Meaning
Prometheus' `Watchdog` alert fires all the time while monitoring works.
Alertmanager forwards it to a healthchecks.io ping URL about once a minute.
healthchecks.io expects a ping every 5 minutes, allows a 5-minute grace
period, and then reports the check **DOWN** to Discord.

So this alarm means **no ping for over 10 minutes**: at least one link in
the chain is broken, and **the platform's own alerting may be unable to tell
you about anything else.**

```
Prometheus ──Watchdog──▶ Alertmanager ──ping──▶ healthchecks.io ──DOWN──▶ Discord
```

## Impact
Until it's fixed, assume nothing is being monitored: real problems won't
raise alerts.

## Diagnose
Work along the chain, from the far end back:

1. **Is the cluster up at all?**
   ```bash
   kubectl get nodes            # local: is Docker Desktop running?
   ```
2. **Prometheus and Alertmanager running?**
   ```bash
   kubectl -n monitoring get pods -l 'app.kubernetes.io/name in (prometheus, alertmanager)'
   ```
3. **Is Watchdog firing?** In Prometheus (`kubectl -n monitoring port-forward svc/kps-kube-prometheus-stack-prometheus 9090`):
   ```promql
   ALERTS{alertname="Watchdog"}
   ```
   No result: Prometheus isn't evaluating rules, or the default rules are missing.
4. **Is Alertmanager pinging successfully?**
   ```bash
   kubectl -n monitoring logs alertmanager-kps-kube-prometheus-stack-alertmanager-0 -c alertmanager --since=15m | grep -i "deadman\|webhook"
   ```
   - `read url_file … no such file`: the Secret `alertmanager-deadman` is
     missing; run `make deadman-secret`.
   - HTTP 4xx: the ping URL is wrong, or the check was deleted at healthchecks.io.
   - Timeouts or DNS errors: the cluster can't reach the internet (Cloud
     NAT on GKE; the network locally).
5. **healthchecks.io itself.** Its page shows the time of the last ping
   received, which tells you when the chain broke.

## Fix
Restore whichever link is broken. The check returns to **UP** on the next
successful ping, with no action needed at healthchecks.io.

## Planned downtime
Stopping the cluster on purpose (shutting down Docker Desktop locally, or
`make down` on GKE) also stops the pings. **Pause the check at
healthchecks.io first**; a paused check resumes by itself when pings return.
