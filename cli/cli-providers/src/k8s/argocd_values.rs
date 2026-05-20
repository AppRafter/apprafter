// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Argo CD upstream Helm chart version. The loader values
//! themselves are derived from the platform-stack chart's
//! `_loaderValues.argocd` field via build.rs (see Track
//! B.1.71). This constant stays here so existing call sites
//! that import `ARGOCD_CHART_VERSION` from `k8s::*` keep
//! working without churn.

/// Pinned Argo CD chart version. Loader installs Argo CD at
/// this version via `helm install`; chart's
/// `component_argocd.cue` pins the SAME version (lockstep).
pub const ARGOCD_CHART_VERSION: &str = "7.7.7";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argocd_chart_version_is_pinned() {
        // Regression guard — the const must match the version
        // the chart's `component_argocd.cue` pulls. A mismatch
        // means the chart adopts a release the loader did not
        // install.
        assert_eq!(ARGOCD_CHART_VERSION, "7.7.7");
    }
}
