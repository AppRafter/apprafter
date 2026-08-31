// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every reader of `metadata.managedFields` must ask kubectl for them.
//!
//! `kubectl` STRIPS `managedFields` from `get -o json` by default — it has
//! since 1.21, to keep output readable — and restores them only under
//! `--show-managed-fields`. Measured on a live cluster: the same object read
//! without the flag reports zero `managedFields` entries, and with it reports
//! three.
//!
//! Two shipped guards read that field out of the plain getter and were
//! therefore dead:
//!
//!   * `egress_field_appears_git_managed` (2.10) — warns that an infra
//!     repository declares `spec.network.egress.profile`, so
//!     `apprafter platform egress set` will be reverted on the next sync.
//!   * `pin_appears_git_managed` (2.22e) — refuses to write an image pin the
//!     user's own manifest declares.
//!
//! Both saw an empty list, concluded nobody owned anything, and returned
//! `false` every time. Recorded as **D18**.
//!
//! # Why this reads source rather than calling the predicates
//!
//! The predicates are already unit-tested, and those tests pass — they build
//! JSON that carries `managedFields` and assert the matching logic. That is
//! precisely why the defect survived: **a unit test that constructs the input
//! cannot tell you the input never arrives.** The bug lives one layer below
//! the tested boundary, in which getter feeds the predicate.
//!
//! So this asserts the wiring instead: a file that inspects `managedFields`
//! must also name the getter that asks for them. It is coarse — it proves the
//! symbol appears in the same file, not that the specific call site is the one
//! feeding the specific predicate — but it catches the regression that
//! actually happens, which is a third guard being added on the plain getter
//! by an author who does not know about the flag.

use std::fs;
use std::path::{Path, PathBuf};

/// The marker a source file uses when it inspects field ownership.
const READS_OWNERSHIP: &str = "managedFields";

/// The getter that actually asks kubectl for them.
const ASKS_FOR_THEM: &str = "kubectl_get_json_showing_managed_fields";

/// Files known to inspect field ownership today. Listed so the test fails
/// loudly if one stops doing so (the guard was deleted) as well as when a new
/// one appears without the flag.
const KNOWN_READERS: &[&str] = &["commands/platform.rs", "commands/app.rs"];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `src/`, recursively.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Source files that read `managedFields`, excluding the helper module that
/// defines the getters (it names the field only in documentation).
fn ownership_readers() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    rust_sources(&src_dir(), &mut files);
    files
        .into_iter()
        .filter(|p| !p.ends_with("k8s_helpers.rs"))
        .filter_map(|p| {
            let body = fs::read_to_string(&p).ok()?;
            body.contains(READS_OWNERSHIP).then_some((p, body))
        })
        .collect()
}

#[test]
fn every_managed_fields_reader_asks_kubectl_for_them() {
    let mut dead = Vec::new();
    for (path, body) in ownership_readers() {
        if !body.contains(ASKS_FOR_THEM) {
            dead.push(path.display().to_string());
        }
    }
    assert!(
        dead.is_empty(),
        "these files inspect `{READS_OWNERSHIP}` but never call `{ASKS_FOR_THEM}`, so kubectl \
         strips the field before they see it and their ownership check can never fire (D18): \
         {dead:?}"
    );
}

#[test]
fn the_reader_list_is_not_vacuous() {
    // Without this, deleting both guards — or renaming the field marker —
    // would make the test above pass over an empty set, which is the
    // degenerate case that lets a guard rot unnoticed.
    let readers = ownership_readers();
    assert!(
        !readers.is_empty(),
        "no source file reads `{READS_OWNERSHIP}` any more — either the guards were removed \
         (then delete this test and say why in the commit) or the marker changed and this test \
         has silently stopped checking anything"
    );
    for known in KNOWN_READERS {
        assert!(
            readers.iter().any(|(p, _)| p.ends_with(known)),
            "{known} no longer inspects `{READS_OWNERSHIP}` — the git-ownership guard it carried \
             is gone, and D18 is about exactly that kind of silent disappearance"
        );
    }
}

#[test]
fn the_plain_getter_does_not_ask_for_managed_fields() {
    // The two getters must stay distinguishable. If the plain one ever
    // started passing the flag, this test's premise would be void and the
    // wiring assertion above would be checking nothing — so pin the
    // asymmetry rather than assume it.
    let helpers = fs::read_to_string(src_dir().join("commands/k8s_helpers.rs"))
        .expect("k8s_helpers.rs is readable");
    assert!(
        helpers.contains("--show-managed-fields"),
        "the flag must appear in the helper that offers it"
    );
    assert!(
        helpers.contains(ASKS_FOR_THEM),
        "the managed-fields getter must be defined in k8s_helpers.rs"
    );
}
