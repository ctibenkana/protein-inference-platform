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
#   make gateway-demo  (call the embeddings API on the cluster)
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
        test-rules discord-secret deadman-secret test-notification local-silences gateway-demo kind-up kind-down context-guard embed-image embed-job argocd grafana-secret status ui-argocd ui-grafana

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

# --- Alert rules ---------------------------------------------------------------
#   make test-rules  (check syntax, run promtool unit tests, check runbooks)
#   Needs promtool and Python with PyYAML.

PYTHON ?= python3

test-rules:
	$(PYTHON) scripts/extract_rules.py .rules-build deploy/manifests/observability/*.rules.yaml
	promtool check rules .rules-build/*.yaml
	promtool test rules tests/prometheus/*.test.yaml
	$(PYTHON) scripts/check_runbooks.py .rules-build/*.yaml

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

# --- Alert notifications (Discord) -------------------------------------------
#   make discord-secret     (store the webhook URL in the cluster; prompts, no echo)
#   make deadman-secret     (store the healthchecks.io ping URL; prompts, no echo)
#   make test-notification  (send a test alert through Alertmanager)
#   make local-silences     (kind only: silence alerts that are false on kind)

ALERTMANAGER_PORT ?= 9393

discord-secret: context-guard
	@printf "Discord webhook URL (input hidden): "; \
	IFS= read -rs url; echo; \
	n=$$(printf '%s' "$$url" | grep -o 'https://' | wc -l | tr -d ' '); \
	if [ "$$n" -gt 1 ]; then \
		echo "The input contains $$n URLs (pasted more than once?). Paste it exactly once; nothing changed."; exit 1; \
	fi; \
	if ! printf '%s' "$$url" | grep -Eq '^https://(discord|discordapp)\.com/api/webhooks/[0-9]+/[A-Za-z0-9_-]+$$'; then \
		echo "That doesn't look like a single Discord webhook URL; nothing changed."; exit 1; \
	fi; \
	printf '%s' "$$url" | kubectl -n monitoring create secret generic alertmanager-discord \
		--from-file=webhook-url=/dev/stdin --dry-run=client -o yaml | kubectl apply -f - >/dev/null && \
	echo "Stored in Secret monitoring/alertmanager-discord. Alertmanager picks it up within about a minute."

# Dead man's switch: the healthchecks.io ping URL for the Watchdog heartbeat.
deadman-secret: context-guard
	@printf "healthchecks.io ping URL (input hidden): "; \
	IFS= read -rs url; echo; \
	n=$$(printf '%s' "$$url" | grep -o 'https://' | wc -l | tr -d ' '); \
	if [ "$$n" -gt 1 ]; then \
		echo "The input contains $$n URLs (pasted more than once?). Paste it exactly once; nothing changed."; exit 1; \
	fi; \
	if ! printf '%s' "$$url" | grep -Eq '^https://hc-ping\.com/[A-Za-z0-9_-]+(/[A-Za-z0-9_-]+)?$$'; then \
		echo "That doesn't look like a single healthchecks.io ping URL (https://hc-ping.com/...); nothing changed."; exit 1; \
	fi; \
	printf '%s' "$$url" | kubectl -n monitoring create secret generic alertmanager-deadman \
		--from-file=ping-url=/dev/stdin --dry-run=client -o yaml | kubectl apply -f - >/dev/null && \
	echo "Stored in Secret monitoring/alertmanager-deadman. Pings start within about two minutes."

test-notification: context-guard
	@kubectl -n monitoring port-forward svc/kps-kube-prometheus-stack-alertmanager $(ALERTMANAGER_PORT):9093 >/dev/null 2>&1 & pf=$$!; \
	sleep 3; \
	ends=$$(date -u -v+5M +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '+5 min' +%Y-%m-%dT%H:%M:%SZ); \
	curl -fsS -X POST localhost:$(ALERTMANAGER_PORT)/api/v2/alerts -H 'Content-Type: application/json' -d "[{ \
		\"labels\": {\"alertname\": \"PlatformNotificationTest\", \"severity\": \"warning\", \"namespace\": \"monitoring\"}, \
		\"annotations\": {\"summary\": \"Test notification from protein-inference-platform\", \
			\"description\": \"If you can read this, Alertmanager can reach Discord. It resolves by itself in 5 minutes.\", \
			\"runbook_url\": \"https://github.com/ctibenkana/protein-inference-platform/blob/main/docs/slos.md\"}, \
		\"endsAt\": \"$$ends\"}]" && \
	echo "Test alert sent; expect a Discord message within about 30 seconds, and a resolved message after 5 minutes."; \
	kill $$pf

# kind nodes run inside Docker and can't see clock sync, so this alert is
# always false locally. Silenced for 30 days; never applied on GKE.
local-silences: context-guard
	@test "$(KCTX)" = "kind-pip-local" || (echo "local-silences is for the kind cluster only" && exit 1)
	@kubectl -n monitoring port-forward svc/kps-kube-prometheus-stack-alertmanager $(ALERTMANAGER_PORT):9093 >/dev/null 2>&1 & pf=$$!; \
	sleep 3; \
	start=$$(date -u +%Y-%m-%dT%H:%M:%SZ); \
	end=$$(date -u -v+30d +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '+30 days' +%Y-%m-%dT%H:%M:%SZ); \
	curl -fsS -X POST localhost:$(ALERTMANAGER_PORT)/api/v2/silences -H 'Content-Type: application/json' -d "{ \
		\"matchers\": [{\"name\": \"alertname\", \"value\": \"NodeClockNotSynchronising\", \"isRegex\": false, \"isEqual\": true}], \
		\"startsAt\": \"$$start\", \"endsAt\": \"$$end\", \"createdBy\": \"make local-silences\", \
		\"comment\": \"kind nodes run in Docker and cannot observe clock sync\"}" >/dev/null && \
	echo "Silenced NodeClockNotSynchronising for 30 days (kind only)."; \
	kill $$pf

# --- ESM-2 embedding job -----------------------------------------------------
#   make embed-image  (build the image locally)
#   make embed-job    (run one job on the current cluster, print its summary)

EMBED_IMAGE ?= esm-embed:dev

embed-image:
	docker build -t $(EMBED_IMAGE) workloads/esm-embed

embed-job: context-guard
	@# kind can't see images that only exist locally; load them. Registry images are pulled.
	@if [ "$(KCTX)" = "kind-pip-local" ] && docker image inspect $(EMBED_IMAGE) >/dev/null 2>&1; then \
		kind load docker-image $(EMBED_IMAGE) --name pip-local; fi
	@job=$$(sed 's|image: IMAGE|image: $(EMBED_IMAGE)|' workloads/esm-embed/k8s/job.yaml | kubectl create -f - -o name) && \
	echo "submitted $$job, waiting for it to finish..." && \
	kubectl -n inference wait --for=condition=complete $$job --timeout=20m && \
	kubectl -n inference logs $$job | grep ' summary ' | sed 's/.* summary //'

# --- Inference gateway -------------------------------------------------------
#   make gateway-demo  (send two proteins through the gateway on the cluster)

GATEWAY_PORT ?= 8088

gateway-demo: context-guard
	@kubectl -n inference port-forward svc/gateway $(GATEWAY_PORT):80 >/dev/null 2>&1 & pf=$$!; \
	sleep 3; \
	curl -sS localhost:$(GATEWAY_PORT)/v1/embeddings -H 'content-type: application/json' \
		-d '{"sequences": ["MKTAYIAKQRQISFVKSHFSRQLEERLGLIEVQ", "MSEQNNTEMTFQIQRIYTKDISFEAPNAPHVFQKDW"]}' | \
	python3 scripts/show_embeddings.py; \
	kill $$pf

ui-argocd:
	@echo "Argo CD: http://localhost:$(ARGOCD_PORT)  user: admin  password:"
	@kubectl -n argocd get secret argocd-initial-admin-secret -o jsonpath='{.data.password}' | base64 -d; echo
	kubectl -n argocd port-forward svc/argocd-server $(ARGOCD_PORT):80

ui-grafana:
	@echo "Grafana: http://localhost:$(GRAFANA_PORT)  user: admin  password:"
	@kubectl -n monitoring get secret grafana-admin -o jsonpath='{.data.admin-password}' | base64 -d; echo
	kubectl -n monitoring port-forward svc/kps-grafana $(GRAFANA_PORT):80
