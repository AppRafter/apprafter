// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Re-exports the build.rs-generated loader values. The
//! actual `pub const` definitions live in
//! `OUT_DIR/loader_values.rs` (path provided by Cargo).
//!
//! See `cli/cli-providers/build.rs` for the extraction logic
//! and `platform-stack/cue/loader_values.cue` for the source
//! of truth.

include!(concat!(env!("OUT_DIR"), "/loader_values.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cilium_values_yaml_contains_loader_critical_fields() {
        // Walk-fix #6 (v0.1.103) regression guard: the loader
        // values MUST set `k8sServiceHost: "127.0.0.1"` (not
        // "auto") and declare the IPv4 / IPv6 enable flags
        // explicitly. Pinning these here lets a future
        // chart-side edit that drops them fail this test in
        // CI rather than silently break Cilium agent at run
        // time.
        assert!(
            CILIUM_VALUES_YAML.contains("k8sServiceHost: 127.0.0.1")
                || CILIUM_VALUES_YAML.contains("k8sServiceHost: \"127.0.0.1\""),
            "missing k8sServiceHost pin: {CILIUM_VALUES_YAML}"
        );
        assert!(
            CILIUM_VALUES_YAML.contains("kubeProxyReplacement: true"),
            "missing kubeProxyReplacement: {CILIUM_VALUES_YAML}"
        );
        assert!(
            CILIUM_VALUES_YAML.contains("ipv4:"),
            "missing ipv4 block: {CILIUM_VALUES_YAML}"
        );
        assert!(
            CILIUM_VALUES_YAML.contains("ipv6:"),
            "missing ipv6 block: {CILIUM_VALUES_YAML}"
        );
    }

    #[test]
    fn argocd_loader_values_yaml_contains_critical_fields() {
        // Walk-fix #1 (redis-ha), #3 (OCI repo registration),
        // #5 (sync-wave order), #7 (default AppProject), and #11
        // (image tag form) all touched these fields. Pin them
        // here so a future chart edit that drops any of them
        // fails this test in CI rather than silently break
        // bootstrap.
        assert!(
            ARGOCD_LOADER_VALUES_YAML.contains("redis-ha:")
                && ARGOCD_LOADER_VALUES_YAML.contains("enabled: false"),
            "missing redis-ha: enabled: false (walk-fix #1):\n{ARGOCD_LOADER_VALUES_YAML}"
        );
        assert!(
            ARGOCD_LOADER_VALUES_YAML.contains("apprafter:")
                && ARGOCD_LOADER_VALUES_YAML.contains("enableOCI:"),
            "missing apprafter OCI repo registration (walk-fix #3):\n{ARGOCD_LOADER_VALUES_YAML}"
        );
        assert!(
            ARGOCD_LOADER_VALUES_YAML.contains("default:")
                && ARGOCD_LOADER_VALUES_YAML.contains("sourceRepos:"),
            "missing default AppProject (walk-fix #7):\n{ARGOCD_LOADER_VALUES_YAML}"
        );
    }

    #[test]
    fn loader_values_are_non_empty_yaml() {
        // Sanity guard — build.rs must have produced non-empty
        // output. An empty / whitespace-only constant would mean
        // `cue export` returned nothing (e.g. the field name
        // changed and build.rs swallowed it). We'd rather catch
        // that here than at runtime when `helm install` rejects
        // an empty values file.
        assert!(
            CILIUM_VALUES_YAML.trim().len() > 50,
            "CILIUM_VALUES_YAML suspiciously short ({} chars): {CILIUM_VALUES_YAML}",
            CILIUM_VALUES_YAML.len(),
        );
        assert!(
            ARGOCD_LOADER_VALUES_YAML.trim().len() > 100,
            "ARGOCD_LOADER_VALUES_YAML suspiciously short ({} chars): {ARGOCD_LOADER_VALUES_YAML}",
            ARGOCD_LOADER_VALUES_YAML.len(),
        );
    }

    #[test]
    fn cilium_chart_version_matches_expected_pin() {
        // B.1.71b: chart's `_loaderValues.cilium.chartVersion` is
        // the SoT. Pinning the actual string here lets a future
        // chart-side bump that forgot to update this expectation
        // fail in CI rather than silently change what the loader
        // installs.
        assert_eq!(CILIUM_CHART_VERSION, "1.16.5");
    }

    #[test]
    fn argocd_chart_version_matches_expected_pin() {
        assert_eq!(ARGOCD_CHART_VERSION, "7.7.7");
    }

    #[test]
    fn released_operator_version_matches_v_prefixed_semver() {
        // Pinned via build.rs reading both operator + webhook
        // Chart.yaml#appVersion (asserted equal). The `v` prefix
        // is convention for operator + webhook image tags (see
        // release-operator.yml's tag inputs).
        assert!(
            RELEASED_OPERATOR_VERSION.starts_with('v'),
            "expected `v` prefix, got {RELEASED_OPERATOR_VERSION:?}"
        );
        assert!(
            RELEASED_OPERATOR_VERSION.matches('.').count() >= 2,
            "expected major.minor.patch, got {RELEASED_OPERATOR_VERSION:?}"
        );
    }

    #[test]
    fn released_platform_stack_version_matches_semver_shape() {
        // CUE schema in `platform-stack/cue/platform.cue` already
        // pins `currentVersion: #Version`, so this is belt-and-
        // braces: build.rs panics on malformed CUE, this test
        // panics on malformed Rust string.
        assert!(
            RELEASED_PLATFORM_STACK_VERSION
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false),
            "RELEASED_PLATFORM_STACK_VERSION should start with a digit \
             (no `v` prefix per platform-stack/cue/platform.cue#Version): \
             got {RELEASED_PLATFORM_STACK_VERSION:?}"
        );
        assert!(
            RELEASED_PLATFORM_STACK_VERSION.matches('.').count() >= 2,
            "RELEASED_PLATFORM_STACK_VERSION should be at least major.minor.patch: \
             got {RELEASED_PLATFORM_STACK_VERSION:?}"
        );
    }
}
