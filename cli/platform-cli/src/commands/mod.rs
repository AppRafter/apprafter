// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Subcommand handlers. Each module implements one verb.

pub mod apply;
pub mod argocd_password;
pub mod auth;
pub mod bootstrap_all;
pub mod cluster_bootstrap;
pub mod destroy;
pub mod doctor;
pub mod hcloud;
pub mod import;
pub mod init;
pub mod kubeconfig;
pub mod login;
pub mod plan;
pub mod status;
pub mod target;
pub mod target_wizard;
pub mod upgrade_tier;
pub mod whoami;
