// SPDX-License-Identifier: FSL-1.1-MIT
//! Kubernetes-side install helpers used by `platform-cli
//! cluster-bootstrap`. Trait-based seams over `helm` + `kubectl`
//! shell-outs, plus pure builders for the manifests / values we
//! ship.

pub mod argocd_values;
pub mod cilium_values;
pub mod helm;
pub mod kubectl;
pub mod network_policy;

pub use argocd_values::{argocd_values_yaml, ARGOCD_CHART_VERSION};
pub use cilium_values::cilium_values_yaml;
pub use helm::{HelmCli, HelmRunner, HelmUpgradeArgs, CILIUM_CHART_VERSION};
pub use kubectl::{
    gateway_api_crds_url, KubectlCli, KubectlRunner, ManifestSource, GATEWAY_API_VERSION,
};
pub use network_policy::default_deny_network_policy_yaml;
