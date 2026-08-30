// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The `behaviour-claim` check, run against the pages that earned it.
//!
//! ADR 0057's rule for the drift gate was that every row of its
//! measured drift table becomes a fixture the gate must reproduce as a
//! failure — "without those fixtures this section is a claim; with them
//! it is a red test". This file is that, for the class ADR 0058 added.
//!
//! The fixtures are not synthetic. Each one is a real page as it stood
//! at a real commit, pulled out of git history, in the state that
//! shipped to readers. A check written after the fact will always pass
//! on the corpus it was written against; the only evidence that it
//! would have caught anything is that it catches what it missed.

use docsgen::behaviour::{self, CLAIMS};

fn repo_root() -> std::path::PathBuf {
    let mut dir = std::env::current_dir().expect("cwd");
    loop {
        if dir.join("cue.mod/module.cue").exists() {
            return dir;
        }
        assert!(dir.pop(), "no cue.mod/module.cue above the test's cwd");
    }
}

/// A file as it stood at a commit. `None` when the object is not in
/// this checkout — a shallow clone, or history rewritten — which is a
/// skip rather than a failure: the fixture is unavailable, not wrong.
fn at_commit(root: &std::path::Path, rev: &str, path: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &format!("{rev}:{path}")])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn it_catches_the_stale_backup_defect_section() {
    // docs/operator-guide/backup-restore.md at 894bdfd — the commit that
    // added e2e assertions proving BOTH documented defects fixed, while
    // the page still described them as current behaviour with manual
    // workarounds. `docsgen gate` was green on this exact text: every
    // name in the passage resolved.
    let root = repo_root();
    let Some(page) = at_commit(&root, "894bdfd", "docs/operator-guide/backup-restore.md") else {
        eprintln!("fixture commit 894bdfd unavailable in this checkout — skipping");
        return;
    };

    let found = behaviour::falsified(&root, &page, CLAIMS).expect("judged");
    let phrases: Vec<&str> = found.iter().map(|(c, _)| c.phrase).collect();

    assert!(
        phrases.contains(&"resolve a kubeconfig as their first statement"),
        "the backup-verb defect went unreported on the page that carried it: {phrases:?}"
    );
    assert!(
        phrases.contains(&"keeps reporting the old machine"),
        "the target-machine defect went unreported on the page that carried it: {phrases:?}"
    );
}

#[test]
fn it_catches_the_stale_cilium_runbook_entry() {
    // docs/operator-guide/troubleshooting.md at 894bdfd told operators to
    // roll the Cilium DaemonSet by hand after a chart bump, long after
    // the chart began stamping a config checksum that rolls it.
    let root = repo_root();
    let Some(page) = at_commit(&root, "894bdfd", "docs/operator-guide/troubleshooting.md") else {
        eprintln!("fixture commit 894bdfd unavailable in this checkout — skipping");
        return;
    };

    let phrases: Vec<&str> = behaviour::falsified(&root, &page, CLAIMS)
        .expect("judged")
        .iter()
        .map(|(c, _)| c.phrase)
        .collect();
    assert!(
        phrases.contains(&"rollout restart ds/cilium"),
        "the Cilium entry went unreported on the page that carried it: {phrases:?}"
    );
}

#[test]
fn it_catches_the_stale_redis_capture_claim() {
    // docs/reference/cli/export.md at 894bdfd — generated from a doc
    // comment that claimed redis is never captured, republished verbatim
    // after cli v0.2.51 shipped persistent-redis snapshots. The
    // generated tree is out of the drift gate's scope by decision, which
    // is why the claim survived there in particular; this check reads
    // the text either way.
    let root = repo_root();
    let Some(page) = at_commit(&root, "894bdfd", "docs/reference/cli/export.md") else {
        eprintln!("fixture commit 894bdfd unavailable in this checkout — skipping");
        return;
    };

    let phrases: Vec<&str> = behaviour::falsified(&root, &page, CLAIMS)
        .expect("judged")
        .iter()
        .map(|(c, _)| c.phrase)
        .collect();
    assert!(
        phrases.contains(&"redis contents are not captured"),
        "the redis claim went unreported on the page that carried it: {phrases:?}"
    );
}

#[test]
fn the_corrected_pages_are_silent() {
    // The other half: the same pages as they stand now report nothing.
    // Without this, a check that flagged every page would pass every
    // assertion above.
    let root = repo_root();
    for path in [
        "docs/operator-guide/backup-restore.md",
        "docs/operator-guide/troubleshooting.md",
        "docs/reference/cli/export.md",
    ] {
        let page = std::fs::read_to_string(root.join(path)).expect("read");
        let found = behaviour::falsified(&root, &page, CLAIMS).expect("judged");
        let shown: Vec<String> = found
            .iter()
            .map(|(c, l)| format!("{path}:{l} {}", c.phrase))
            .collect();
        assert!(shown.is_empty(), "{}", shown.join("\n"));
    }
}
