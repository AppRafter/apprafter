// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The `RetainedClaim` snapshot must be create-if-absent, never a re-apply.
//!
//! # The deadlock this pins
//!
//! `snapshot_retained_claim` runs on the deletion path, BEFORE the provisioner
//! releases its finalizer, and its old form applied the snapshot blind on the
//! stated assumption that a repeat is "byte-identical".
//!
//! It is not. The snapshot's name is deterministic per (namespace, claim), a
//! `RetainedClaim` spec is IMMUTABLE by admission, and `retainUntil` is derived
//! from *this* deletion's `deletionTimestamp`. So when a claim is deleted,
//! retained, recreated and deleted AGAIN inside the grace window — faster than
//! the intervening provision can cancel the first snapshot, which the
//! re-provision arms do — the second apply carries a different `retainUntil`
//! against the same name and the apiserver answers:
//!
//! ```text
//! RetainedClaim.apprafter.io "claim-demo-sqlite-disk" is invalid:
//!   spec: Invalid value: RetainedClaim spec is immutable
//! ```
//!
//! Because that line precedes the finalizer release, the claim never finishes
//! deleting. It sits in `Terminating` forever, the owning Application sits at
//! `Ready=False: paused awaiting ResourceClaim provisioning` forever, and the
//! only way out is a human editing the finalizer off by hand. The reconcile
//! retries the same 422 every thirty seconds meanwhile.
//!
//! Observed live by the 2.22 e2e battery on `needs-disk-walk`: two deletions
//! 42 seconds apart (`deletionTimestamp` 16:49:55 against a surviving snapshot
//! whose `retainUntil` was computed from 16:49:13). Nothing about it is
//! disk-specific — the deterministic name and the `retainUntil` derivation are
//! shared by every backend; the disk walk is simply the one that churned.
//!
//! # Why a source test
//!
//! The rejection lives in the admission webhook, so no in-process test can
//! observe it, and a unit test over the built body would pass — the body is
//! correct. What must hold is the WIRING: the apply is reached only when no
//! snapshot exists, and a lost race is tolerated rather than propagated.

use std::fs;
use std::path::{Path, PathBuf};

fn reconcile_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("reconcile.rs")
}

/// The body of `async fn snapshot_retained_claim`, from its signature to the
/// next top-level `fn` at column zero.
fn snapshot_fn_body(src: &str) -> &str {
    let start = src
        .find("async fn snapshot_retained_claim")
        .expect("snapshot_retained_claim not found — was it renamed?");
    let rest = &src[start..];
    // The function ends at the first line that closes it at column zero.
    let end = rest
        .find("\n}\n")
        .expect("could not find the end of snapshot_retained_claim");
    &rest[..end]
}

#[test]
fn the_snapshot_is_guarded_by_an_existence_check() {
    let src = fs::read_to_string(reconcile_src()).expect("read reconcile.rs");
    let body = snapshot_fn_body(&src);

    let guard = body.find("get_opt").expect(
        "snapshot_retained_claim must GET before it applies: a blind apply 422s on the \
                immutable spec of a snapshot left by an earlier deletion, and it runs BEFORE the \
                finalizer release, so the claim wedges in Terminating forever",
    );
    let apply = body
        .find("Patch::Apply")
        .expect("snapshot_retained_claim no longer applies a snapshot — was it restructured?");

    assert!(
        guard < apply,
        "the existence check must come BEFORE the apply, not after it"
    );
}

#[test]
fn a_lost_creation_race_is_tolerated_rather_than_propagated() {
    // Two reconciles can both observe "absent" and both apply; the loser gets
    // 409/422. Propagating it would reproduce the same wedge by a narrower
    // route, because the finalizer release is still downstream.
    let src = fs::read_to_string(reconcile_src()).expect("read reconcile.rs");
    let body = snapshot_fn_body(&src);

    assert!(
        body.contains("409") && body.contains("422"),
        "the snapshot apply must tolerate a concurrent create (409/Conflict and \
         422/Invalid-immutable); found neither code in snapshot_retained_claim"
    );
}

#[test]
fn the_grace_clock_is_not_restarted_by_a_second_deletion() {
    // The existing snapshot wins on purpose. Re-applying with the newer
    // `retainUntil` — even if the webhook allowed it — would let a delete /
    // recreate / delete loop extend a seven-day window without bound.
    let src = fs::read_to_string(reconcile_src()).expect("read reconcile.rs");
    let body = snapshot_fn_body(&src);
    let guard_pos = body.find("get_opt").expect("existence check present");
    let early_return = body[guard_pos..]
        .find("return Ok(())")
        .expect("the existing-snapshot branch must RETURN rather than fall through to the apply");
    let apply_pos = body.find("Patch::Apply").expect("apply present");

    assert!(
        guard_pos + early_return < apply_pos,
        "the existing-snapshot branch must return before the apply is reached"
    );
}
