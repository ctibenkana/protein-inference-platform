# ArgoCDAppUnhealthy

**Severity:** warning · **Source:** `argocd_app_info` (Argo CD application controller)

## Meaning
An Argo CD application has been `Degraded`, `Missing` or `Unknown` for 15
minutes. `Progressing` (a rollout in progress) doesn't alert.

## Impact
Whatever the application deploys isn't working as intended: a crashing pod, a
failed rollout, or a missing resource.

## Diagnose
```bash
APP=<name from the alert>
make status                                     # all applications at a glance
kubectl -n argocd get application $APP -o jsonpath='{range .status.resources[?(@.health.status!="Healthy")]}{.kind}/{.name}: {.health.status} {.health.message}{"\n"}{end}'
```
Then inspect the unhealthy resource, usually a Deployment or StatefulSet:
```bash
kubectl -n <namespace> describe deploy/<name>
kubectl -n <namespace> get pods
kubectl -n <namespace> logs <pod> --previous
```
`make ui-argocd` shows the same information graphically.

## Fix
- **Started with a commit:** `git revert` it and push; Argo CD rolls back.
- **Resources:** a pod `Pending` or `OOMKilled` on the system node; raise
  limits or reduce what runs there.
- **Startup too slow for probes:** this happened with Grafana's first start;
  see the commit history of `deploy/values/kube-prometheus-stack.yaml`.

## Afterwards
If the fix was a revert, reproduce the failure on the local kind cluster
before trying the change again.
