output "state_bucket" {
  description = "Terraform state bucket. Set as the GitHub variable TF_STATE_BUCKET."
  value       = google_storage_bucket.tfstate.name
}

output "image_registry" {
  description = "Docker registry path for platform images."
  value       = "${google_artifact_registry_repository.images.location}-docker.pkg.dev/${var.project_id}/${google_artifact_registry_repository.images.repository_id}"
}

output "workload_identity_provider" {
  description = "Full provider name. Set as the GitHub variable GCP_WIF_PROVIDER."
  value       = google_iam_workload_identity_pool_provider.github.name
}

output "ci_plan_service_account" {
  description = "Service account for terraform plan. Set as the GitHub variable GCP_PLAN_SA."
  value       = google_service_account.ci_plan.email
}

output "ci_images_service_account" {
  description = "Service account for image pushes. Set as the GitHub variable GCP_IMAGES_SA."
  value       = google_service_account.ci_images.email
}
