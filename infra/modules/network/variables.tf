variable "project_id" {
  description = "Google Cloud project ID."
  type        = string
}

variable "region" {
  description = "Region for the subnet, router and NAT."
  type        = string
}

variable "name" {
  description = "Name prefix for network resources."
  type        = string
}

variable "nodes_cidr" {
  description = "Primary range for node IPs."
  type        = string
  default     = "10.10.0.0/20"
}

variable "pods_cidr" {
  description = "Secondary range for pod IPs."
  type        = string
  default     = "10.20.0.0/16"
}

variable "services_cidr" {
  description = "Secondary range for Service IPs."
  type        = string
  default     = "10.30.0.0/20"
}
