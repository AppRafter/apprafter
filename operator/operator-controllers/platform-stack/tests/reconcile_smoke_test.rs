// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Real-cluster smoke for PlatformController. Gated by
//! `APPRAFTER_K8S_SMOKE=1` and `#[ignore]` so the standard
//! `cargo test` run does NOT need a live cluster.
//!
//! Invocation (from a `nix develop` shell with a kubeconfig
//! pointing at a cluster carrying the platform-stack chart
//! and the default PlatformStack CR):
//!
//!   APPRAFTER_K8S_SMOKE=1 cargo test \
//!     -p operator-controllers-platform-stack \
//!     --test reconcile_smoke_test \
//!     -- --ignored
//!
//! Per the AppRafter feedback memory, environment-conditioned
//! silent skips are forbidden — when the gate variable is unset
//! and the test runs, panic instead of returning Ok.

#[tokio::test]
#[ignore]
async fn reconcile_smoke_patches_parent_application() {
    if std::env::var("APPRAFTER_K8S_SMOKE").as_deref() != Ok("1") {
        panic!(
            "this test requires APPRAFTER_K8S_SMOKE=1 (and a kubeconfig pointing at a \
             cluster with the platform-stack chart applied and the default PlatformStack CR)"
        );
    }

    // Acceptance walk for B.1.73's PlatformController on a live
    // cluster. Suggested check sequence — not yet automated:
    //
    //   1. Read the current parent Application's
    //      `spec.source.targetRevision`.
    //   2. Run one reconcile cycle against the live cluster.
    //   3. Verify the parent's `metadata.managedFields` contains
    //      an entry with `manager == "platform-controller"`
    //      owning `spec.source.targetRevision`.
    //   4. Verify `PlatformStack.status.{targetVersion,
    //      currentVersion, availableVersion, lastUpstreamCheck}`
    //      populated.
    //   5. Manually edit `Application.spec.source.targetRevision`
    //      via kubectl; expect revert within ≤ 30s and
    //      `UnauthorizedSourceModification=True` condition.
    //
    // Implementation uses `kube::Client::try_default()` and the
    // same `operator_controllers_platform_stack::run` entry
    // point.
    //
    // Marked `unimplemented!` until the first walk under
    // controlled conditions establishes the assertion shape.
    unimplemented!(
        "smoke test scaffold — fill in when running against a real cluster (see comment above)"
    )
}
