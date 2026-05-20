// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// `_loaderValues` exposes the subset of values the CLI loader
// (`apprafter cluster-bootstrap`) needs to install Cilium +
// Argo CD before Argo CD takes over reconciliation from the
// platform-stack chart. The CLI's `build.rs` reads these
// fields at compile time via `cue export -e _loaderValues.<comp>`
// and emits Rust `const &str` constants the loader uses
// verbatim.
//
// Why hidden (`_` prefix): operators don't override these. The
// loader is an internal CLI implementation detail; chart
// users interact through `_components.*.values` overlays
// instead.
//
// Walk-fix #6 (v0.1.103) surfaced the duplication-as-drift
// class: `component_cilium.cue` carried `k8sServiceHost:
// "auto"` while the CLI loader carried `"127.0.0.1"`; Argo
// CD applied the chart values on top of the loader's
// Deployment and cilium-operator crashed with `KUBERNETES_SERVICE_HOST=auto`.
// The invariant below makes that crash impossible by
// construction — chart values ≡ loader values for Cilium.
_loaderValues: {
	// Cilium values — byte-identical to `_components.cilium.values`.
	cilium: _components.cilium.values
}

// Invariant: chart's cilium values ARE the loader values. CUE
// unifies left + right; if any future edit makes them
// diverge, `cue vet` fails with `incompatible values`.
_components: cilium: values: _loaderValues.cilium
