// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! In-cluster scheduled-backup runner library (2.6d-4). The `apprafter-backup`
//! binary is a thin wrapper over this crate.
pub mod config;
pub mod kube_rs_exec;
pub mod orchestrate;
pub mod restic_child;
pub mod staging;
pub mod status;
pub mod stop;
pub mod tls;
pub mod webhook;
