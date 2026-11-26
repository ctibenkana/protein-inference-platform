terraform {
  required_version = ">= 1.9"

  # Partial configuration: the bucket is passed at init time with
  # -backend-config="bucket=<name>" so no project details live in git.
  backend "gcs" {
    prefix = "envs/dev"
  }

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
