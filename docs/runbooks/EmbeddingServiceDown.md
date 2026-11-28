# EmbeddingServiceDown

**Severity:** critical · **Source:** Prometheus scrape status (`up`)

## Meaning
For 5 minutes, Prometheus hasn't seen both containers of the embedding
service (the gateway and its model server sidecar) up. Unlike the SLO
burn-rate alerts, this one fires without any traffic, so it also catches an
outage at a quiet time.

## Impact
The embeddings API is probably unavailable. Batch embedding jobs
(`make embed-job`) are separate and unaffected.

## Diagnose
```bash
kubectl -n inference get deploy,pods -l app.kubernetes.io/name=embedding-service
kubectl -n inference describe pod -l app.kubernetes.io/name=embedding-service
kubectl -n inference logs deploy/embedding-service -c model-server --tail=50
kubectl -n inference logs deploy/embedding-service -c gateway --tail=50
kubectl -n argocd get application inference    # a bad sync?
```

| What you see | Likely cause |
|---|---|
| Pod `Pending` | No node with 2 free CPUs and 1 GiB for the model server |
| `model-server` restarting, `OOMKilled` | Memory limit (2 GiB) too low for the model |
| `model-server` startup probe failing | Model failed to load (bad image, missing weights); the gateway doesn't start until it loads |
| `ImagePullBackOff` | Wrong digest in git, or the registry is unreachable |
| Pods fine but `up` is 0 | Scraping broke: PodMonitor, port names, or network policy |
| Started right after a commit | A bad change; check the Argo CD sync |

```promql
up{namespace="inference"}
```

## Fix
- **Bad change or image:** revert the commit; Argo CD syncs the previous version.
- **Resources:** adjust requests and limits in
  `deploy/manifests/inference/embedding-service.yaml` (and node size on GKE).
- **Scraping only:** the service may be serving; confirm with
  `make gateway-demo`, then fix the PodMonitor or ports.
