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
    fn argocd_loader_values_restore_application_health_for_sync_waves() {
        // Argo CD removed the `argoproj.io/Application` health assessment in
        // 1.8 and v2.13.1 still ships none, so gitops-engine completes a
        // child-Application sync task the instant its apply succeeds and a
        // wave made of child Applications never waits for anything. Without
        // this key the chart's declared `syncWave` order is decoration:
        // `gateway-api-crds` (-25) and `cilium` (-20) are applied ~2s apart
        // and race, and the run where cilium wins has a cilium-operator that
        // logged `Required GatewayAPI resources are not found` and a cluster
        // whose ingress can never serve a request — while every Argo CD
        // Application reads Synced/Healthy. Measured on a real kind+Cilium
        // cluster by `e2e/gateway-order-probe.sh`.
        //
        // Pinned in the LOADER constant specifically: the first root sync —
        // the one that decides that order — runs under the Argo CD
        // `cluster-bootstrap` helm-installs, so losing the key here reopens
        // the race even if `component_argocd.cue` still carries it.
        assert!(
            ARGOCD_LOADER_VALUES_YAML
                .contains("resource.customizations.health.argoproj.io_Application"),
            "missing the argoproj.io/Application health check — the chart's sync waves \
             stop ordering anything:\n{ARGOCD_LOADER_VALUES_YAML}"
        );
    }

    /// The `(group, kind)` pairs one AppProject permits on namespaced
    /// resources, read out of the loader YAML.
    ///
    /// Parsed rather than grepped, and that is the whole point of the
    /// helper: `ARGOCD_LOADER_VALUES_YAML.contains("SharedDatabase")` is
    /// satisfied by the kind sitting in the `platform` project, which is
    /// where it does a user bundle no good at all. The question is which
    /// project lists it, so the test has to descend into one.
    fn namespaced_whitelist(project: &str) -> Vec<(String, String)> {
        let doc: serde_yaml::Value =
            serde_yaml::from_str(ARGOCD_LOADER_VALUES_YAML).expect("loader values are YAML");
        let entries = doc["configs"]["projects"][project]["namespaceResourceWhitelist"]
            .as_sequence()
            .unwrap_or_else(|| {
                panic!(
                    "no `configs.projects.{project}.namespaceResourceWhitelist` in the loader \
                     values:\n{ARGOCD_LOADER_VALUES_YAML}"
                )
            });
        entries
            .iter()
            .map(|e| {
                (
                    e["group"].as_str().unwrap_or_default().to_string(),
                    e["kind"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn apps_project_permits_every_kind_a_user_bundle_can_render() {
        // 0.2.76. An AppProject refusal fails the WHOLE sync, not the
        // offending resource: a bundle of five applications and two
        // `SharedDatabase`s landed nothing and sat in SyncFailed with
        // `resource apprafter.io:SharedDatabase is not permitted in
        // project apps`, every workload Missing.
        //
        // The list was written in B.1.79a against the Phase-1 surface,
        // where `Application` was the only `apprafter.io` kind a bundle
        // could hold, and neither 2.6c (`SharedVolume`, ADR 0049) nor
        // 2.29 (`SharedDatabase`, ADR 0066) came back to it. This test
        // is the thing that makes the NEXT such CRD fail loudly here
        // instead of on somebody's cluster.
        //
        // Pinned on the LOADER constant because that is the copy a fresh
        // `cluster-bootstrap` installs Argo CD with, before the umbrella
        // has synced its own wave -30 AppProjects. Both read the same
        // `_appProjects` map, so a kind dropped from the map fails here.
        let permitted = namespaced_whitelist("apps");
        for (group, kind) in [
            ("apprafter.io", "Application"),
            ("apprafter.io", "SharedDatabase"),
            ("apprafter.io", "SharedVolume"),
            ("", "ConfigMap"),
            ("", "Secret"),
            ("bitnami.com", "SealedSecret"),
            ("gateway.networking.k8s.io", "HTTPRoute"),
        ] {
            assert!(
                permitted.contains(&(group.to_string(), kind.to_string())),
                "the `apps` AppProject does not permit {group}:{kind} — a bundle declaring \
                 one fails its ENTIRE sync, not just that resource. Permitted: {permitted:?}"
            );
        }
    }

    #[test]
    fn apps_project_refuses_operator_authored_kinds() {
        // The other half of the rule, and the reason the list is not
        // simply `apprafter.io/*`. `ResourceClaim`, `MigrationPlan` and
        // `RetainedClaim` are written by the operator under its own
        // field manager; a bundle able to render one could hand Argo CD
        // a competing copy, and the argument would be settled by
        // whichever reconcile happened to run last. `SourceCredential`
        // is a registry credential `apprafter registry add` writes into
        // `apprafter-system` — platform configuration that happens to be
        // a CR, not application payload.
        let permitted = namespaced_whitelist("apps");
        for kind in [
            "ResourceClaim",
            "MigrationPlan",
            "RetainedClaim",
            "SourceCredential",
        ] {
            assert!(
                !permitted.contains(&("apprafter.io".to_string(), kind.to_string())),
                "the `apps` AppProject permits apprafter.io:{kind}, which a user bundle has \
                 no business rendering. Permitted: {permitted:?}"
            );
        }
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
