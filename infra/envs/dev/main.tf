# Dev environment: created at the start of a work session and destroyed
# at the end (`make up` / `make down`). Everything that must outlive a
# session lives in infra/bootstrap.

locals {
  name = "pip-dev"
  labels = {
    env     = "dev"
    project = "protein-inference-platform"
  }
}

module "network" {
  source = "../../modules/network"

  project_id = var.project_id
  region     = var.region
  name       = local.name
}

module "gke" {
  source = "../../modules/gke"

  project_id          = var.project_id
  name                = local.name
  zone                = var.zone
  network_id          = module.network.network_id
  subnetwork_id       = module.network.subnetwork_id
  pods_range_name     = module.network.pods_range_name
  services_range_name = module.network.services_range_name
  authorized_networks = var.authorized_networks
  labels              = local.labels
}
