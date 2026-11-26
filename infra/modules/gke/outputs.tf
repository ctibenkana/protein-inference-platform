output "cluster_name" {
  description = "Name of the GKE cluster."
  value       = google_container_cluster.this.name
}

output "cluster_location" {
  description = "Zone of the GKE cluster."
  value       = google_container_cluster.this.location
}

output "node_service_account" {
  description = "Email of the node service account."
  value       = google_service_account.nodes.email
}
