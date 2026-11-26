# Long-lived foundation, applied once from a laptop with local state:
# - the Google APIs the platform needs
# - a versioned GCS bucket for the Terraform state of every environment
# - an Artifact Registry repository for container images
# - keyless GitHub Actions authentication (Workload Identity Federation),
#   restricted to this one repository, with two narrow service accounts

locals {
  services = [
    "artifactregistry.googleapis.com",
    "cloudresourcemanager.googleapis.com",
    "compute.googleapis.com",
    "container.googleapis.com",
    "iam.googleapis.com",
    "iamcredentials.googleapis.com",
    "sts.googleapis.com",
    "storage.googleapis.com",
  ]

  state_bucket = coalesce(var.state_bucket_name, "${var.project_id}-tfstate")
}

resource "google_project_service" "this" {
  for_each = toset(local.services)

  project            = var.project_id
  service            = each.value
  disable_on_destroy = false
}

resource "google_storage_bucket" "tfstate" {
  project                     = var.project_id
  name                        = local.state_bucket
  location                    = var.region
  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"

  versioning {
    enabled = true
  }

  lifecycle_rule {
    condition {
      num_newer_versions = 10
    }
    action {
      type = "Delete"
    }
  }

  depends_on = [google_project_service.this]
}

resource "google_artifact_registry_repository" "images" {
  project       = var.project_id
  location      = var.region
  repository_id = "images"
  format        = "DOCKER"
  description   = "Container images for the platform."

  # Keep storage costs flat: keep the 10 newest versions of each image,
  # delete anything untagged after 7 days.
  cleanup_policy_dry_run = false

  cleanup_policies {
    id     = "keep-recent"
    action = "KEEP"
    most_recent_versions {
      keep_count = 10
    }
  }

  cleanup_policies {
    id     = "delete-untagged"
    action = "DELETE"
    condition {
      tag_state  = "UNTAGGED"
      older_than = "604800s"
    }
  }

  depends_on = [google_project_service.this]
}

# --- GitHub Actions: keyless authentication -------------------------------

resource "google_iam_workload_identity_pool" "github" {
  project                   = var.project_id
  workload_identity_pool_id = "github"
  display_name              = "GitHub Actions"

  depends_on = [google_project_service.this]
}

resource "google_iam_workload_identity_pool_provider" "github" {
  project                            = var.project_id
  workload_identity_pool_id          = google_iam_workload_identity_pool.github.workload_identity_pool_id
  workload_identity_pool_provider_id = "github-actions"
  display_name                       = "GitHub Actions OIDC"

  attribute_mapping = {
    "google.subject"       = "assertion.sub"
    "attribute.repository" = "assertion.repository"
    "attribute.ref"        = "assertion.ref"
  }

  # Tokens from any other repository are rejected outright.
  attribute_condition = "assertion.repository == '${var.github_repository}'"

  oidc {
    issuer_uri = "https://token.actions.githubusercontent.com"
  }
}

locals {
  github_principal = "principalSet://iam.googleapis.com/${google_iam_workload_identity_pool.github.name}/attribute.repository/${var.github_repository}"
}

# Read-only account for `terraform plan` in pull requests.
resource "google_service_account" "ci_plan" {
  project      = var.project_id
  account_id   = "ci-terraform-plan"
  display_name = "CI: terraform plan (read-only)"
}

resource "google_project_iam_member" "ci_plan_viewer" {
  project = var.project_id
  role    = "roles/viewer"
  member  = "serviceAccount:${google_service_account.ci_plan.email}"
}

# Plan needs to read state and take the state lock.
resource "google_storage_bucket_iam_member" "ci_plan_state" {
  bucket = google_storage_bucket.tfstate.name
  role   = "roles/storage.objectUser"
  member = "serviceAccount:${google_service_account.ci_plan.email}"
}

resource "google_service_account_iam_member" "ci_plan_wif" {
  service_account_id = google_service_account.ci_plan.name
  role               = "roles/iam.workloadIdentityUser"
  member             = local.github_principal
}

# Push-only account for publishing images from CI.
resource "google_service_account" "ci_images" {
  project      = var.project_id
  account_id   = "ci-image-publisher"
  display_name = "CI: push container images"
}

resource "google_artifact_registry_repository_iam_member" "ci_images_writer" {
  project    = var.project_id
  location   = google_artifact_registry_repository.images.location
  repository = google_artifact_registry_repository.images.name
  role       = "roles/artifactregistry.writer"
  member     = "serviceAccount:${google_service_account.ci_images.email}"
}

resource "google_service_account_iam_member" "ci_images_wif" {
  service_account_id = google_service_account.ci_images.name
  role               = "roles/iam.workloadIdentityUser"
  member             = local.github_principal
}
