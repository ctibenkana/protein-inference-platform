# Zonal GKE Standard cluster, sized for a small budget:
# - zonal, so the cluster management fee can fall under GKE's free tier
# - one small "system" node for platform services (Argo CD, monitoring)
# - a "workers" pool of spot nodes that scales to zero when no jobs run
# - private nodes, Workload Identity, shielded nodes, least-privilege node SA

locals {
  node_sa_roles = [
    "roles/logging.logWriter",
    "roles/monitoring.metricWriter",
    "roles/monitoring.viewer",
    "roles/stackdriver.resourceMetadata.writer",
    "roles/artifactregistry.reader",
  ]
}

resource "google_service_account" "nodes" {
  project      = var.project_id
  account_id   = "${var.name}-nodes"
  display_name = "GKE nodes for ${var.name}"
}

resource "google_project_iam_member" "nodes" {
  for_each = toset(local.node_sa_roles)

  project = var.project_id
  role    = each.value
  member  = "serviceAccount:${google_service_account.nodes.email}"
}

resource "google_container_cluster" "this" {
  project  = var.project_id
  name     = var.name
  location = var.zone

  network         = var.network_id
  subnetwork      = var.subnetwork_id
  networking_mode = "VPC_NATIVE"

  # Dataplane V2 (eBPF): enforces Kubernetes NetworkPolicy at no extra
  # cost, which later keeps lab workloads isolated from everything else.
  datapath_provider = "ADVANCED_DATAPATH"

  # No default node pool: initial_node_count and node_config are omitted,
  # and the node pools below are managed as separate resources.

  deletion_protection = var.deletion_protection

  release_channel {
    channel = "REGULAR"
  }

  ip_allocation_policy {
    cluster_secondary_range_name  = var.pods_range_name
    services_secondary_range_name = var.services_range_name
  }

  private_cluster_config {
    enable_private_nodes    = true
    enable_private_endpoint = false
  }

  # The control plane only accepts connections from listed CIDRs.
  master_authorized_networks_config {
    dynamic "cidr_blocks" {
      for_each = var.authorized_networks
      content {
        cidr_block   = cidr_blocks.value.cidr_block
        display_name = cidr_blocks.value.display_name
      }
    }
  }

  workload_identity_config {
    workload_pool = "${var.project_id}.svc.id.goog"
  }

  # Platform metrics and logs are self-hosted (Prometheus, Grafana, Loki),
  # so Google-managed collection is limited to system components.
  logging_config {
    enable_components = ["SYSTEM_COMPONENTS"]
  }

  monitoring_config {
    enable_components = ["SYSTEM_COMPONENTS"]
    managed_prometheus {
      enabled = false
    }
  }

  cost_management_config {
    enabled = true
  }

  resource_labels = var.labels
}

resource "google_container_node_pool" "system" {
  project    = var.project_id
  name       = "system"
  location   = var.zone
  cluster    = google_container_cluster.this.name
  node_count = 1

  management {
    auto_repair  = true
    auto_upgrade = true
  }

  node_config {
    machine_type    = var.system_machine_type
    image_type      = "COS_CONTAINERD"
    spot            = var.system_spot
    disk_type       = "pd-balanced"
    disk_size_gb    = 30
    service_account = google_service_account.nodes.email
    oauth_scopes    = ["https://www.googleapis.com/auth/cloud-platform"]
    labels          = merge(var.labels, { pool = "system" })

    workload_metadata_config {
      mode = "GKE_METADATA"
    }

    shielded_instance_config {
      enable_secure_boot          = true
      enable_integrity_monitoring = true
    }
  }
}

resource "google_container_node_pool" "workers" {
  project            = var.project_id
  name               = "workers"
  location           = var.zone
  cluster            = google_container_cluster.this.name
  initial_node_count = 0

  autoscaling {
    total_min_node_count = 0
    total_max_node_count = var.workers_max_nodes
    location_policy      = "ANY"
  }

  management {
    auto_repair  = true
    auto_upgrade = true
  }

  node_config {
    machine_type    = var.workers_machine_type
    image_type      = "COS_CONTAINERD"
    spot            = true
    disk_type       = "pd-balanced"
    disk_size_gb    = 50
    service_account = google_service_account.nodes.email
    oauth_scopes    = ["https://www.googleapis.com/auth/cloud-platform"]
    labels          = merge(var.labels, { pool = "workers" })

    # Only pods that tolerate this taint (batch inference jobs) land here,
    # so platform services never keep a worker node alive.
    taint {
      key    = "workload"
      value  = "batch"
      effect = "NO_SCHEDULE"
    }

    workload_metadata_config {
      mode = "GKE_METADATA"
    }

    shielded_instance_config {
      enable_secure_boot          = true
      enable_integrity_monitoring = true
    }
  }

  lifecycle {
    # The autoscaler changes the node count; don't fight it on every apply.
    ignore_changes = [initial_node_count]
  }
}
