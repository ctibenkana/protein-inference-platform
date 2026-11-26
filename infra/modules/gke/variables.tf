variable "project_id" {
  description = "Google Cloud project ID."
  type        = string
}

variable "name" {
  description = "Cluster name, also used as a prefix for related resources."
  type        = string
}

variable "zone" {
  description = "Zone for the cluster. Zonal keeps the management fee within GKE's free tier (one zonal cluster per billing account)."
  type        = string
}

variable "network_id" {
  description = "VPC ID from the network module."
  type        = string
}

variable "subnetwork_id" {
  description = "Subnet ID from the network module."
  type        = string
}

variable "pods_range_name" {
  description = "Secondary range name for pods."
  type        = string
}

variable "services_range_name" {
  description = "Secondary range name for Services."
  type        = string
}

variable "authorized_networks" {
  description = "CIDR blocks allowed to reach the control plane endpoint, for example your own IP as a /32. Required, so the API is never open to the whole internet by accident."
  type = list(object({
    cidr_block   = string
    display_name = string
  }))

  validation {
    condition     = length(var.authorized_networks) > 0
    error_message = "Set at least one authorized network, e.g. your public IP as a /32."
  }

  validation {
    condition     = alltrue([for n in var.authorized_networks : can(cidrhost(n.cidr_block, 0)) && n.cidr_block != "0.0.0.0/0"])
    error_message = "Each cidr_block must be a valid CIDR, and 0.0.0.0/0 is not allowed."
  }
}

variable "deletion_protection" {
  description = "Block terraform destroy on the cluster. Keep false while the dev cluster is created and destroyed per work session."
  type        = bool
  default     = false
}

variable "system_machine_type" {
  description = "Machine type for the always-present system node."
  type        = string
  default     = "e2-standard-2"
}

variable "system_spot" {
  description = "Run the system node on spot capacity. Cheaper, but the node can be reclaimed; set false once lab users depend on the service."
  type        = bool
  default     = true
}

variable "workers_machine_type" {
  description = "Machine type for batch worker nodes."
  type        = string
  default     = "e2-standard-4"
}

variable "workers_max_nodes" {
  description = "Upper bound for the worker pool autoscaler (also a spending cap)."
  type        = number
  default     = 3
}

variable "labels" {
  description = "Labels applied to the cluster and nodes, used for cost reporting."
  type        = map(string)
  default     = {}
}
