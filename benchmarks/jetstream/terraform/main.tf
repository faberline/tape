terraform {
  required_version = ">= 1.5.0"
  required_providers {
    google = { source = "hashicorp/google", version = "~> 6.0" }
  }
}
variable "project_id" { type = string }
variable "region" {
  type    = string
  default = "us-central1"
}
variable "run_id" { type = string }
provider "google" {
  project = var.project_id
  region  = var.region
}
resource "google_container_cluster" "benchmark" {
  name                     = "tape-jetstream-${var.run_id}"
  location                 = var.region
  deletion_protection      = false
  remove_default_node_pool = true
  initial_node_count       = 1
  node_config {
    machine_type = "e2-standard-4"
    labels       = { benchmark-role = "tester" }
  }
}
resource "google_container_node_pool" "tape" {
  name       = "durable-v1-tape-${var.run_id}"
  cluster    = google_container_cluster.benchmark.name
  location   = var.region
  node_count = 1
  node_config {
    machine_type = "e2-standard-4"
    labels       = { benchmark-role = "tape" }
  }
}
resource "google_container_node_pool" "nats" {
  name       = "durable-v1-nats-${var.run_id}"
  cluster    = google_container_cluster.benchmark.name
  location   = var.region
  node_count = 1
  node_config {
    machine_type = "e2-standard-4"
    labels       = { benchmark-role = "nats" }
  }
}
resource "google_container_node_pool" "tester" {
  name       = "durable-v1-tester-${var.run_id}"
  cluster    = google_container_cluster.benchmark.name
  location   = var.region
  node_count = 1
  node_config {
    machine_type = "e2-standard-4"
    labels       = { benchmark-role = "tester" }
  }
}
output "cluster_name" { value = google_container_cluster.benchmark.name }
output "cluster_location" { value = google_container_cluster.benchmark.location }
