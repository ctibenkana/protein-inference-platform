# protein-inference-platform

A low-cost, production-style Kubernetes platform on Google Cloud for running
protein language model inference (starting with ESM-2 embedding and scoring)
and LLM serving with vLLM.

Cost is treated as a design constraint, not an afterthought: the cluster is
built to cost very little when idle, scale to zero when there is no work, and
be created or destroyed from code in minutes.

> **Status:** stage 1 of the platform. GCP foundation, network and GKE
> cluster as code, plus CI. Next come GitOps (Argo CD + Helm), observability
> (Prometheus, Grafana, Loki) and the first inference workloads. See the
> [roadmap](#roadmap).

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
- **The cluster will pull, not be pushed to.** Deployments will come from Argo CD
  watching this repository, so CI never needs credentials for the cluster.

## Repository layout

```
infra/
  bootstrap/      long-lived foundation (local state; creates the state bucket)
  envs/dev/       dev network + cluster (remote state in GCS)
  modules/
    network/      VPC, subnet with pod/service ranges, router, Cloud NAT
    gke/          cluster, node pools, node service account
.github/workflows/terraform.yml
Makefile          session workflow: init, up, creds, down
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

## Roadmap

1. **Foundation** ✅ GCP foundation, network, GKE, CI with lint and security scanning
2. **GitOps:** Argo CD, Helm charts, app-of-apps layout
3. **Observability:** Prometheus, Grafana, Loki; SLOs, alerts and runbooks
4. **Releases:** container builds with signed images and versioned releases
5. **First workloads:** a small model served with vLLM; ESM-2 embedding jobs on the worker pool
6. **Spot GPU pool** for benchmark days, scaling to zero
7. **Cost report:** measured monthly cost and cost per benchmark run

## License

[Apache 2.0](LICENSE)
