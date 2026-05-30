// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Kubernetes-side helpers — the `*_yaml` renderers and the
//! Helm + kubectl process wrappers used by the CLI loader
//! (`commands/cluster_bootstrap.rs`). After Track B.1.71 the
//! `*_yaml` renderers are gone; only `helm`, `kubectl`,
//! `image_ref`, and the surviving values modules remain.

pub mod channel_latest;
pub mod helm;
pub mod image_ref;
pub mod kubectl;
pub mod loader_values;
pub mod sealing;

pub use channel_latest::resolve_latest_platform_stack_version;
pub use helm::{HelmCli, HelmRunner, HelmUpgradeArgs};
pub use image_ref::{
    resolve_image_ref, APPRAFTER_ADMISSION_WEBHOOK_DEFAULT_IMAGE, APPRAFTER_OPERATOR_DEFAULT_IMAGE,
    APPRAFTER_PLATFORM_STACK_CHART_NAME, APPRAFTER_PLATFORM_STACK_DEFAULT_REPO,
};
pub use kubectl::{
    gateway_api_crds_url, KubectlCli, KubectlRunner, ManifestSource, APPRAFTER_CLI_FIELD_MANAGER,
    GATEWAY_API_VERSION,
};
pub use loader_values::{
    ARGOCD_CHART_VERSION, ARGOCD_LOADER_VALUES_YAML, CILIUM_CHART_VERSION, CILIUM_VALUES_YAML,
    RELEASED_OPERATOR_VERSION, RELEASED_PLATFORM_STACK_VERSION,
};
