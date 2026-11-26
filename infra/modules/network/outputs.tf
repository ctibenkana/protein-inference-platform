output "network_id" {
  description = "ID of the VPC."
  value       = google_compute_network.this.id
}

output "subnetwork_id" {
  description = "ID of the node subnet."
  value       = google_compute_subnetwork.nodes.id
}

output "pods_range_name" {
  description = "Name of the secondary range used for pods."
  value       = "pods"
}

output "services_range_name" {
  description = "Name of the secondary range used for Services."
  value       = "services"
}
