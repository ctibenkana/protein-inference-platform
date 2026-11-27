# ArgoCDAppOutOfSync

**Severity:** warning · **Source:** `argocd_app_info` (Argo CD application controller)

## Meaning
For 30 minutes, the cluster hasn't matched what git says it should be.
Automated sync is on for every application, so this almost always means a sync
is failing, or someone changed the cluster by hand and self-heal can't
correct it.

## Impact
The last change pushed to `main` isn't live. Fixes and rollbacks also won't
apply until the sync works.

## Diagnose
```bash
APP=<name from the alert>
kubectl -n argocd get application $APP -o jsonpath='{.status.operationState.message}{"\n"}{range .status.conditions[*]}{.type}: {.message}{"\n"}{end}'
```

| Message contains | Likely cause |
|---|---|
| `ComparisonError` … `serverSideDiff` | The cluster rejects the change in a dry run (an immutable field, or an invalid combination) |
| `the server could not find the requested resource` | A custom resource whose CRD isn't installed yet |
| `failed to fetch` / repository errors | GitHub unreachable, or a chart version that doesn't exist |

## Fix
- **Rejected change:** change the manifest so the cluster accepts it. Example
  from this repository: moving Grafana from `RollingUpdate` to `Recreate` was
  rejected; `maxSurge: 0` achieved the same without changing the type.
- **Missing CRD:** wait for the app that installs it (usually `monitoring`)
  and Argo CD's retry, or sync manually once it exists.
- **Repository errors:** check that the chart version exists:
  `helm search repo <chart> --versions`.

## Afterwards
CI renders and validates every manifest, but it can't catch everything a live
cluster rejects. If this got past CI, consider adding a check for it.
