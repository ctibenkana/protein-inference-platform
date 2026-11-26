# Session workflow for the dev cluster:
#   make init   (once per clone)
#   make up     (start of a work session: create network + cluster)
#   make creds  (point kubectl at the cluster)
#   make down   (end of the session: destroy it, stop paying for it)
#   Requires TF_STATE_BUCKET, e.g.  export TF_STATE_BUCKET=my-project-tfstate
#
# GitOps layer, on any cluster kubectl points at (GKE or local kind):
#   make kind-up   (local only: create a kind cluster in Docker)
#   make argocd    (install Argo CD once; it then syncs deploy/ from git)
#   make status    (list Argo CD applications and their health)
#   make ui-argocd / make ui-grafana   (open the UIs via port-forward)
#   make kind-down (local only: delete the kind cluster)

ENV_DIR := infra/envs/dev
TF      := terraform -chdir=$(ENV_DIR)

ARGOCD_CHART_VERSION := 10.20.0

# GitOps targets refuse to run unless kubectl points at one of these, so a
# stray context can never receive an install by accident.
ALLOWED_CONTEXTS := kind-pip-local gke_%_pip-dev
KCTX = $(shell kubectl config current-context 2>/dev/null)

# Local ports for the UIs; override if busy, e.g. make ui-grafana GRAFANA_PORT=3400
GRAFANA_PORT ?= 3300
ARGOCD_PORT  ?= 8380

.PHONY: help check init plan up down creds fmt validate \
        kind-up kind-down context-guard embed-image embed-job argocd grafana-secret status ui-argocd ui-grafana

help:
	@grep -E '^#' Makefile | sed 's/^# \{0,1\}//'

check:
	@test -n "$(TF_STATE_BUCKET)" || (echo "TF_STATE_BUCKET is not set" && exit 1)

init: check
	$(TF) init -input=false -backend-config="bucket=$(TF_STATE_BUCKET)"

plan:
	$(TF) plan

up:
	$(TF) apply

down:
	$(TF) destroy

creds:
	@eval "$$($(TF) output -raw get_credentials)"

fmt:
	terraform fmt -recursive infra

validate:
	@for d in infra/bootstrap $(ENV_DIR); do \
		terraform -chdir=$$d init -backend=false -input=false >/dev/null && \
		terraform -chdir=$$d validate || exit 1; \
	done

# --- GitOps layer ------------------------------------------------------------

kind-up:
	kind create cluster --config deploy/local/kind.yaml

kind-down:
	kind delete cluster --name pip-local

context-guard:
	@test -n "$(filter $(ALLOWED_CONTEXTS),$(KCTX))" || \
		(echo "Refusing: kubectl context is '$(KCTX)', expected kind-pip-local or the pip-dev GKE cluster" && exit 1)
	@echo "kubectl context: $(KCTX)"

# Grafana's admin password is generated here and stored only in the cluster.
grafana-secret: context-guard
	@kubectl create namespace monitoring --dry-run=client -o yaml | kubectl apply -f - >/dev/null
	@kubectl -n monitoring get secret grafana-admin >/dev/null 2>&1 || \
		kubectl -n monitoring create secret generic grafana-admin \
			--from-literal=admin-user=admin \
			--from-literal=admin-password="$$(openssl rand -base64 24)"
	@echo "grafana-admin secret present in namespace monitoring"

argocd: grafana-secret
	helm repo add argo https://argoproj.github.io/argo-helm --force-update >/dev/null
	helm upgrade --install argocd argo/argo-cd --version $(ARGOCD_CHART_VERSION) \
		--namespace argocd --create-namespace \
		--values deploy/values/argo-cd.yaml --wait --timeout 10m
	kubectl apply -f deploy/root.yaml
	@echo "Argo CD installed; it now syncs deploy/apps from git. Run: make status"

status:
	@kubectl -n argocd get applications.argoproj.io \
		-o custom-columns=APP:.metadata.name,SYNC:.status.sync.status,HEALTH:.status.health.status

# --- ESM-2 embedding job -----------------------------------------------------
#   make embed-image  (build the image locally)
#   make embed-job    (run one job on the current cluster, print its summary)

EMBED_IMAGE ?= esm-embed:dev

embed-image:
	docker build -t $(EMBED_IMAGE) workloads/esm-embed

embed-job: context-guard
	@if [ "$(KCTX)" = "kind-pip-local" ]; then kind load docker-image $(EMBED_IMAGE) --name pip-local; fi
	@job=$$(sed 's|image: IMAGE|image: $(EMBED_IMAGE)|' workloads/esm-embed/k8s/job.yaml | kubectl create -f - -o name) && \
	echo "submitted $$job, waiting for it to finish..." && \
	kubectl -n inference wait --for=condition=complete $$job --timeout=20m && \
	kubectl -n inference logs $$job | grep ' summary ' | sed 's/.* summary //'

ui-argocd:
	@echo "Argo CD: http://localhost:$(ARGOCD_PORT)  user: admin  password:"
	@kubectl -n argocd get secret argocd-initial-admin-secret -o jsonpath='{.data.password}' | base64 -d; echo
	kubectl -n argocd port-forward svc/argocd-server $(ARGOCD_PORT):80

ui-grafana:
	@echo "Grafana: http://localhost:$(GRAFANA_PORT)  user: admin  password:"
	@kubectl -n monitoring get secret grafana-admin -o jsonpath='{.data.admin-password}' | base64 -d; echo
	kubectl -n monitoring port-forward svc/kps-grafana $(GRAFANA_PORT):80
