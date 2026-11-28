# protein-inference-platform

A low-cost, production-style Kubernetes platform on Google Cloud for running
protein language model inference (starting with ESM-2 embedding and scoring)
and LLM serving with vLLM.

Cost is treated as a design constraint, not an afterthought: the cluster is
built to cost very little when idle, scale to zero when there is no work, and
be created or destroyed from code in minutes.

> **Status:** GCP foundation, network and GKE cluster as code, with CI.
> GitOps and observability layer (Argo CD, Prometheus, Grafana, Loki, Alloy)
> running and verified on a local cluster; GKE deployment waits on billing.
> First workload: [esm-embed](workloads/esm-embed), ESM-2 protein embeddings,
> where length-aware batching cut padding from 42% to 5% and doubled
> throughput in a first local run. Online serving: a Rust
> [inference gateway](services/gateway) with dynamic length-aware batching
> across callers, which carries 3–4× the traffic of FIFO batching at the
> same latency, with availability and latency SLOs, burn-rate alerts and
> runbooks. See the [roadmap](#roadmap).

## Write-ups

- [Why FIFO batching collapses: a padding feedback loop in protein-model serving](docs/writeups/fifo-batching-collapse.md):
  length-aware batching carries 3–4× the load of FIFO at the same latency,
  and why FIFO's padding turns load into more load.

## Architecture

```
                         GitHub
     ┌──────────────────────────────────────────────────┐
     │  pull request ──► CI: fmt · validate · tflint ·  │
     │                       trivy · terraform plan     │
     └────────────────────────────┬─────────────────────┘
                                  │ OIDC (no stored keys)
                                  ▼
 Google Cloud ─────────────────────────────────────────────────────────
  bootstrap (long-lived)              envs/dev (per work session)
  ┌──────────────────────────┐        ┌──────────────────────────────┐
  │ Terraform state bucket   │        │ VPC, subnet, Cloud NAT       │
  │ Artifact Registry        │        │ GKE zonal cluster            │
  │ Workload Identity        │        │  ├─ system pool: 1 small     │
  │   Federation for GitHub  │        │  │  spot node (platform)     │
  │ CI service accounts      │        │  └─ workers pool: spot,      │
  │   (plan: read-only,      │        │     autoscale 0 → 3, tainted │
  │    images: push-only)    │        │     for batch inference      │
  └──────────────────────────┘        └──────────────────────────────┘
```

## Design decisions

### Cost

| Decision | Why |
|---|---|
| **Zonal** GKE Standard cluster | GKE's free tier covers the management fee of one zonal cluster per billing account. |
| **Spot** VMs for all nodes | Spot capacity is much cheaper than on-demand. The system node moves to on-demand once real users depend on the service. |
| Worker pool **scales to zero** and is **tainted** | Workers exist only while batch jobs run. The taint stops platform pods from keeping a worker alive. |
| Dev cluster is **destroyed after each session** | `make up` / `make down`. Long-lived pieces live in `bootstrap`, so nothing is lost. |
| Google-managed logging and metrics **limited to system components** | Workload observability will be self-hosted, avoiding per-GB ingestion charges. |
| Registry **cleanup policies** | Keep the 10 newest versions per image, delete untagged images after 7 days. |
| GKE **cost allocation** enabled | Cost can be broken down per namespace and label in billing reports. |

Measured monthly costs will be published here once the platform has run for a
full month. Until then the target is roughly **$30–80/month** at light use,
which is an estimate.

### Security

- **Private nodes.** No node has a public IP; outbound traffic goes through Cloud NAT.
- **Control plane restricted to listed IPs.** `authorized_networks` is required
  and rejects `0.0.0.0/0`, so the API cannot be left open by accident.
- **Keyless CI.** GitHub Actions authenticates with OIDC through Workload
  Identity Federation. There are no service-account keys anywhere, and only
  this repository's tokens are accepted.
- **Least privilege.** CI has a read-only plan account and a push-only image
  account. Nodes run as a dedicated service account with five narrow roles, not
  the default Compute Engine account.
- **Workload Identity** on all nodes, so pods get their own Google identities
  and can't read node credentials.
- **Shielded nodes** with secure boot, the hardened COS image, and
  **Dataplane V2**, which enforces Kubernetes network policies.
- **Supply chain.** Every GitHub Action is pinned to a full commit SHA, and the
  workflow token is read-only by default.
- **Scanned on every change.** CI fails on any high or critical finding from
  trivy.

Accepted findings in dev, with reasons:

| Finding | Decision |
|---|---|
| VPC flow logs off | Billed per GB. To be enabled with sampling in the production environment. |
| Google-managed encryption keys (no CMEK) | Customer-managed keys add cost and operational work; revisit if lab data requires it. |

### Delivery

- **CI checks and plans; it never applies.** Pull requests run format, validate,
  lint, security scan and a `terraform plan` shown in the job summary. Applies
  run from an operator's machine for now.
- **The cluster pulls; nothing pushes to it.** Argo CD watches `deploy/` in this
  repository and syncs the cluster to it, so CI never holds cluster credentials.
- **App of apps.** One root Application (`deploy/root.yaml`) syncs everything
  in `deploy/apps/`. Argo CD manages its own installation the same way, so even
  upgrades to the deployer go through a pull request.
- **Every chart is pinned** to an exact version, with values kept in
  `deploy/values/`. CI renders each application exactly as Argo CD would and
  validates every object, including custom resources, against its schema.

### Observability

| Component | Role |
|---|---|
| Prometheus + Alertmanager (kube-prometheus-stack) | Metrics, 7-day retention, picks up monitors from every namespace |
| Grafana | Dashboards over Prometheus and Loki; admin password generated in-cluster, never in git |
| Loki (single binary) | Log storage, 7-day retention |
| Grafana Alloy | Tails pod logs through the Kubernetes API and ships them to Loki |

All components have explicit resource requests and limits, sized to fit
together on the single small system node.

**SLOs and alerting.** Three service level objectives (job success, batching
efficiency as a cost objective, and log pipeline availability with
multi-window burn-rate alerts), 11 alerts, and a runbook for every alert. The
rules are unit-tested with `promtool`, and CI fails if an alert has no
runbook. See [docs/slos.md](docs/slos.md) and [docs/runbooks/](docs/runbooks).
A Grafana dashboard shows SLO status, throughput, padding and job logs.
Alerts notify a Discord channel by severity, with the runbook link in every
message; the webhook URL lives only in a Kubernetes Secret.
A dead man's switch (healthchecks.io) raises the alarm if alerting itself
stops working.

**Lab data never reaches the logs.** Services must not log protein
sequences. As a second safeguard, Alloy drops any log line containing a run
of 30 or more amino-acid letters before it's stored, and counts the drops.

### Workload isolation

- The `inference` namespace enforces the Kubernetes **restricted** Pod
  Security level: no root, no privilege escalation, no host access.
- Its **network policies deny all incoming traffic** except Prometheus
  scraping. Services will open specific paths as they're added.
- `monitoring` is the only privileged namespace, because node-exporter needs
  host access to read node metrics.

## Repository layout

```
infra/
  bootstrap/      long-lived foundation (local state; creates the state bucket)
  envs/dev/       dev network + cluster (remote state in GCS)
  modules/
    network/      VPC, subnet with pod/service ranges, router, Cloud NAT
    gke/          cluster, node pools, node service account
deploy/
  root.yaml       root Argo CD Application (app of apps)
  apps/           one Argo CD Application per component, charts pinned
  values/         Helm values for each chart
  manifests/      plain manifests: namespaces, Pod Security, network policies
  local/kind.yaml local cluster for developing without cloud costs
  manifests/observability/  SLO recording rules, alerts, Grafana dashboard
docs/
  slos.md         objectives, why each target, burn-rate design
  runbooks/       one runbook per alert
tests/prometheus/ promtool unit tests for every alert and SLO rule
workloads/
  esm-embed/      ESM-2 embedding job: code, tests, Dockerfile, Job template
scripts/
  render_apps.py  renders each application as Argo CD would (used by CI)
.github/workflows/
  terraform.yml   fmt, validate, tflint, trivy, plan
  gitops.yml      render charts, validate every object, test alert rules, check runbooks
  esm-embed.yml   test, multi-arch image build, keyless signing, releases
Makefile          session and GitOps commands (make help)
```

## Getting started

Prerequisites: a Google Cloud project with billing enabled, `gcloud`,
Terraform 1.9+, and owner or editor rights on the project.

```bash
gcloud auth login
gcloud auth application-default login

# 1. Foundation (once)
cd infra/bootstrap
terraform init
terraform apply -var project_id=YOUR_PROJECT -var github_repository=OWNER/protein-inference-platform
cd ../..

# 2. Dev environment (each session)
cp infra/envs/dev/terraform.tfvars.example infra/envs/dev/terraform.tfvars   # set project + your IP
export TF_STATE_BUCKET=YOUR_PROJECT-tfstate
make init     # once per clone
make up       # create network + cluster
make creds    # configure kubectl
kubectl get nodes
make down     # destroy at the end of the session
```

To enable `terraform plan` in CI, set these repository variables from the
bootstrap outputs: `GCP_PROJECT_ID`, `GCP_WIF_PROVIDER`, `GCP_PLAN_SA`,
`TF_STATE_BUCKET`, `TF_AUTHORIZED_NETWORKS`.

### GitOps layer, on GKE or locally

The same configuration runs on GKE or on a local
[kind](https://kind.sigs.k8s.io/) cluster in Docker (about 4 GB of memory):

```bash
make kind-up      # local only; on GKE, run `make creds` instead
make argocd       # installs Argo CD; it then syncs everything in deploy/
make status       # wait until every application is Synced / Healthy
make ui-grafana   # http://localhost:3300 (prints the admin password)
make ui-argocd    # http://localhost:8380 (ports overridable: GRAFANA_PORT=, ARGOCD_PORT=)
make embed-image  # build the ESM-2 job image
make embed-job    # run one embedding job, print its summary
make test-rules   # promtool unit tests for alerts and SLOs, runbook check
make discord-secret      # store the Discord webhook URL (prompts, no echo)
make deadman-secret      # store the healthchecks.io ping URL (prompts, no echo)
make test-notification   # send a test alert through Alertmanager
make kind-down
```

## Roadmap

1. **Foundation** ✅ GCP foundation, network, GKE, CI with lint and security scanning
2. **GitOps** ✅ Argo CD (self-managed), app of apps, pinned Helm charts, CI rendering and schema validation; verified on a local cluster
3. **Observability** ✅ Prometheus, Grafana, Loki, Alloy; SLOs, alerts and runbooks, unit-tested and verified live (a failed job, a heavily padded run and a leaked sequence each raised their alert in Alertmanager)
4. **Releases** ✅ multi-arch images on GHCR, Sigstore keyless signing, SBOM and provenance, tag-driven releases
5. **First workloads:** ESM-2 embedding job ✅ (verified on the local cluster, metrics in Prometheus); inference gateway in Rust, in progress ([design](docs/design/gateway.md)): API, model server, deadlines, retries, network policies ✅, dynamic length-aware batching ✅, overload protection (bounded queue, bounded drain) ✅, open-loop load tests ✅ ([results](docs/benchmarks/gateway-load-tests.md)), availability and latency SLOs with burn-rate alerts, runbooks and dashboard ✅, cargo-deny ✅; then a small model served with vLLM
6. **Spot GPU pool** for benchmark days, scaling to zero
7. **Cost report:** measured monthly cost and cost per benchmark run

## License

[Apache 2.0](LICENSE)
