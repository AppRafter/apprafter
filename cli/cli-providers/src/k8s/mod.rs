// SPDX-License-Identifier: FSL-1.1-MIT
//! Kubernetes-side install helpers used by `platform-cli
//! cluster-bootstrap`. Trait-based seams over `helm` + `kubectl`
//! shell-outs, plus pure builders for the manifests / values we
//! ship.

pub mod admission_webhook;
pub mod application_crd;
pub mod argocd_gateway;
pub mod argocd_values;
pub mod backstage_app_config;
pub mod backstage_manifests;
pub mod bootstrap_app;
pub mod cert_manager_values;
pub mod cilium_values;
pub mod helm;
pub mod image_ref;
pub mod issuer;
pub mod kubectl;
pub mod network_policy;
pub mod operator_values;

pub use admission_webhook::{admission_webhook_yaml, APPRAFTER_SYSTEM_NAMESPACE};
pub use application_crd::{
    application_crd_yaml, APPLICATION_CRD_GROUP, APPLICATION_CRD_PLURAL, APPLICATION_CRD_VERSION,
};
pub use argocd_gateway::argocd_gateway_yaml;
pub use argocd_values::{argocd_values_yaml, ARGOCD_CHART_VERSION};
pub use backstage_app_config::backstage_app_config_yaml;
pub use backstage_manifests::{backstage_manifests_yaml, BACKSTAGE_DEFAULT_IMAGE};
pub use bootstrap_app::{bootstrap_application_yaml, BOOTSTRAP_APP_DEFAULT_PATH};
pub use cert_manager_values::{cert_manager_values_yaml, CERT_MANAGER_CHART_VERSION};
pub use cilium_values::cilium_values_yaml;
pub use helm::{HelmCli, HelmRunner, HelmUpgradeArgs, CILIUM_CHART_VERSION};
pub use image_ref::{
    resolve_image_ref, APPRAFTER_ADMISSION_WEBHOOK_DEFAULT_IMAGE, APPRAFTER_OPERATOR_DEFAULT_IMAGE,
    RELEASED_OPERATOR_VERSION,
};
pub use issuer::{selfsigned_cluster_issuer_yaml, APPRAFTER_SELFSIGNED_ISSUER};
pub use kubectl::{
    gateway_api_crds_url, KubectlCli, KubectlRunner, ManifestSource, GATEWAY_API_VERSION,
};
pub use network_policy::default_deny_network_policy_yaml;
pub use operator_values::operator_values_yaml;
