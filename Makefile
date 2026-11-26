# Session workflow for the dev cluster:
#   make init   (once per clone)
#   make up     (start of a work session: create network + cluster)
#   make creds  (point kubectl at the cluster)
#   make down   (end of the session: destroy it, stop paying for it)
#
# Requires TF_STATE_BUCKET, e.g.  export TF_STATE_BUCKET=my-project-tfstate

ENV_DIR := infra/envs/dev
TF      := terraform -chdir=$(ENV_DIR)

.PHONY: help check init plan up down creds fmt validate

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
