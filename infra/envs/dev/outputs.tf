output "cluster_name" {
  description = "Name of the GKE cluster."
  value       = module.gke.cluster_name
}

output "cluster_location" {
  description = "Zone of the GKE cluster."
  value       = module.gke.cluster_location
}

output "get_credentials" {
  description = "Command to point kubectl at the cluster."
  value       = "gcloud container clusters get-credentials ${module.gke.cluster_name} --zone ${module.gke.cluster_location} --project ${var.project_id}"
}
