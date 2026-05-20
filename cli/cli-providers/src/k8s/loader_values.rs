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
}
