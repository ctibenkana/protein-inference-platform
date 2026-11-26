variable "project_id" {
  description = "Google Cloud project ID."
  type        = string
}

variable "region" {
  description = "Region for the network."
  type        = string
  default     = "europe-west1"
}

variable "zone" {
  description = "Zone for the cluster. Must be inside var.region."
  type        = string
  default     = "europe-west1-b"
}

variable "authorized_networks" {
  description = "CIDR blocks allowed to reach the control plane, for example your public IP as a /32."
  type = list(object({
    cidr_block   = string
    display_name = string
  }))
}
