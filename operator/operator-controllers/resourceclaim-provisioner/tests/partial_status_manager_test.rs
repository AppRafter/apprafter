// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A partial status apply must not run under the provisioner's own manager.
//!
//! Server-side apply REPLACES a field manager's owned field-set on every
//! apply — it does not accumulate. So a body carrying only `status.size`,
//! applied under the manager that also owns `ready` / `instance` / `dbnum` /
//! `connectionSecretRef`, **deletes all of them**.
//!
//! That is measured, not reasoned. On a real apiserver (Kubernetes 1.35, this
//! repository's own CRD) a claim whose status read
//!
//! ```text
//! {conditions, connectionSecretRef, dbnum, instance, ready: true}
//! ```
//!
//! read exactly `{size}` after one size-only apply under the same manager, and
//! kept everything after the same apply under a dedicated one.
//!
//! The consequences compound rather than stopping at a missing field: a pruned
//! `ready` makes `should_provision` true again, so a live claim re-provisions;
//! a re-provision allocates a `dbnum` the freed one is now available for, which
//! is the 2.6 isolation breach; and allocation `FLUSHDB`s, which destroys the
//! tenant's data. `reconcile.rs`'s own doc-comment on `status_apply_body`
//! records this hazard for the allocation checkpoint — the 2.22d size sampler
//! reintroduced it at a new address, on claims that are already live.
//!
//! # Why a source test rather than a unit test
//!
//! The behaviour lives in the apiserver, so no in-process test can observe it;
//! and a unit test over the *body* would pass, because the body is correct —
//! the defect is which manager carries it. This asserts the wiring: any
//! `patch_status` whose body mentions `size` must be applied with
//! `size_apply_params`. Recorded as **D19**.

use std::fs;
use std::path::{Path, PathBuf};

fn src(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file)
}

/// Files that write claim status.
const STATUS_WRITERS: &[&str] = &["reconcile.rs", "acl_reconcile.rs"];

/// Every `patch_status` call site, as (file, line-number, the window of source
/// leading up to it). The window is where the body literal is built.
fn patch_status_sites() -> Vec<(String, usize, String)> {
    let mut sites = Vec::new();
    for f in STATUS_WRITERS {
        let body = fs::read_to_string(src(f)).expect("source file is readable");
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("patch_status(") {
                continue;
            }
            // Look back far enough to cover the `json!` body and forward far
            // enough to cover a multi-line call.
            let lo = i.saturating_sub(30);
            let hi = (i + 12).min(lines.len());
            sites.push((f.to_string(), i + 1, lines[lo..hi].join("\n")));
        }
    }
    sites
}

#[test]
fn a_size_only_status_apply_uses_the_dedicated_manager() {
    let mut offenders = Vec::new();
    for (file, line, window) in patch_status_sites() {
        // A body that writes the size sample. `"size"` as a JSON key is the
        // marker; `size_apply_params` is the required manager.
        let writes_size = window.contains("\"size\"");
        if writes_size && !window.contains("size_apply_params") {
            offenders.push(format!("{file}:{line}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "these status applies carry only `status.size` but run under the provisioner's own \
         field manager, so server-side apply will PRUNE `ready`, `instance`, `dbnum` and \
         `connectionSecretRef` from a live claim — re-provisioning it, freeing its dbnum to \
         another tenant, and FLUSHDB-ing its data (D19): {offenders:?}"
    );
}

#[test]
fn the_site_list_is_not_vacuous() {
    // If the scan finds no size-writing call site, the test above passes over
    // an empty set — the degenerate case that lets the guard rot. Two sites
    // write the sample today: the Postgres refresh and the redis ACL loop.
    let sizey: Vec<_> = patch_status_sites()
        .into_iter()
        .filter(|(_, _, w)| w.contains("\"size\""))
        .collect();
    assert!(
        sizey.len() >= 2,
        "expected at least two size-writing status applies (pg refresh + redis ACL loop), \
         found {} — either they moved and this test no longer checks anything, or a sampler \
         was deleted",
        sizey.len()
    );
}

#[test]
fn the_two_managers_are_distinct() {
    // The whole fix is that they differ. If someone "tidies" them into one
    // constant the pruning returns, and every other assertion here would
    // still pass.
    assert_ne!(
        operator_controllers_resourceclaim_provisioner::FIELD_MANAGER,
        operator_controllers_resourceclaim_provisioner::SIZE_FIELD_MANAGER,
    );
}
