terraform {
  required_version = ">= 1.9"

  # Local state on purpose: this root creates the remote state bucket.
  # Keep terraform.tfstate out of git (see .gitignore) and back it up.

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 8.12"
    }
  }
}

provider "google" {
  project = var.project_id
  region  = var.region
}
