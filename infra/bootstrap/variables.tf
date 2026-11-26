variable "project_id" {
  description = "Google Cloud project ID."
  type        = string
}

variable "region" {
  description = "Region for the state bucket and image registry."
  type        = string
  default     = "europe-west1"
}

variable "github_repository" {
  description = "GitHub repository allowed to authenticate, as owner/name."
  type        = string
}

variable "state_bucket_name" {
  description = "Name of the Terraform state bucket. Defaults to <project_id>-tfstate."
  type        = string
  default     = null
}
