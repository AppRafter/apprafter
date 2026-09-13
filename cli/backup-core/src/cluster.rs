// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Cluster identity inside a restic repository (E1–E4).
//!
//! # Why a repository needs an identity at all
//!
//! Two clusters can legitimately write to one restic repository — the
//! "move to a bigger machine" runbook has the old and the new cluster alive
//! at the same time, both carrying the same `spec.backup`. Before this
//! module every snapshot of every cluster was tagged `<release-name>-<time>`
//! where the release name was the constant `platform`, so the two clusters'
//! snapshots differed only by timestamp. That made `latest` able to roll out
//! the *other* cluster's run, and made the retention planner delete the
//! other cluster's snapshots.
//!
//! # The identity
//!
//! The machine key is the **`kube-system` namespace UID** — the de-facto
//! standard cluster identifier. It needs no generated state, it is readable
//! both in-cluster and from a kubeconfig, it already exists on clusters that
//! predate this code, and a *restored* cluster is a different Kubernetes
//! cluster with a different UID, so a clone cannot inherit it.
//!
//! It is carried as the first component of the restic **run tag**:
//!
//! ```text
//! <kube-system-uid>-<rfc3339>[-ns-<a>_<b>]
//! ```
//!
//! [`run_tag`] writes that form and [`tag_cluster_uid`] reads it back; they
//! are inverses and must be changed together.
//!
//! A human-readable label lives separately, in the restic `--host`
//! (`spec.backup.clusterName`), because a UUID alone is unusable when an
//! operator has to pick a snapshot out of a listing. The host is NOT an
//! identity: it is replayed by restore, so a clone inherits it. Every filter
//! in this module keys on the UID, which a clone cannot inherit.
//!
//! # Legacy snapshots
//!
//! Snapshots written before this existed carry no UID in their tag. They are
//! deliberately treated as belonging to **this** cluster
//! ([`SnapshotOrigin::Legacy`] counts as owned) so they stay prunable and
//! restorable. The cost is stated rather than hidden: in a repository two
//! clusters already shared, the other cluster's old snapshots are attributed
//! to whoever asks first. `apprafter backup list` marks them so that
//! attribution is visible rather than assumed silently.

use serde_json::Value;

/// Length of a canonical RFC-4122 UUID: `8-4-4-4-12` plus four dashes.
const UUID_LEN: usize = 36;

/// The dash-separated group lengths of a canonical UUID.
const UUID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];

/// Is `s` exactly a canonical lowercase-or-uppercase hex UUID?
///
/// Kubernetes stamps `metadata.uid` in this form on every object, so this is
/// the shape [`tag_cluster_uid`] looks for at the head of a run tag. Written
/// by hand rather than pulled in as a dependency: the only question is
/// "could this prefix be a k8s UID", and a parser that accepted more (braces,
/// URNs, no dashes) would make the tag format ambiguous.
pub fn is_uuid(s: &str) -> bool {
    if s.len() != UUID_LEN {
        return false;
    }
    let mut groups = s.split('-');
    for expected in UUID_GROUPS {
        let Some(g) = groups.next() else {
            return false;
        };
        if g.len() != expected || !g.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
    }
    groups.next().is_none()
}

/// The cluster UID a run tag carries, or `None` when the tag is not in the
/// identified format.
///
/// The identified format is `<uuid>-<rest>`; `None` covers both the legacy
/// `<release-name>-<rfc3339>` form and anything else a repository may hold.
/// A bare UUID with no timestamp is NOT accepted — the tag must be a run tag,
/// and a run tag always has a timestamp after the identity.
pub fn tag_cluster_uid(tag: &str) -> Option<&str> {
    if tag.len() <= UUID_LEN {
        return None;
    }
    if tag.as_bytes()[UUID_LEN] != b'-' {
        return None;
    }
    let candidate = &tag[..UUID_LEN];
    is_uuid(candidate).then_some(candidate)
}

/// The cluster UID a snapshot's tag set carries, or `None` when no tag is in
/// the identified format (a legacy or foreign-tool snapshot).
pub fn snapshot_cluster_uid(tags: &[String]) -> Option<&str> {
    tags.iter().find_map(|t| tag_cluster_uid(t))
}

/// Where a snapshot came from, as far as its tags can say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotOrigin {
    /// Its tag carries THIS cluster's `kube-system` UID.
    ThisCluster,
    /// Its tag carries a DIFFERENT cluster's UID — never ours to forget.
    OtherCluster,
    /// No tag carries a UID at all: written before cluster identity existed
    /// (or untagged). Attributed to this cluster BY ASSUMPTION.
    Legacy,
}

/// The tag list restic reports for one snapshot document.
///
/// Every narrowing decision here keys on tags, so there is exactly one answer
/// to "what does an untagged snapshot look like": an empty list, never a
/// missing field that a caller might treat differently.
pub fn snapshot_tags(snapshot: &Value) -> Vec<String> {
    snapshot
        .pointer("/tags")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Every cluster UID a `restic snapshots --json` listing carries, sorted and
/// deduplicated.
///
/// The answer to "whose snapshots are in this repository". Two commands refuse
/// rather than guess and then have to NAME the candidates: `latest` when none
/// of the snapshots are this cluster's and more than one other cluster is
/// present, and an offline `backup prune --cluster-uid` whose UID this
/// repository has never seen. Legacy snapshots contribute nothing — they carry
/// no identity, which is precisely why they are attributed by assumption.
pub fn cluster_uids_in<'a>(snapshots: impl IntoIterator<Item = &'a Value>) -> Vec<String> {
    let mut uids: Vec<String> = snapshots
        .into_iter()
        .filter_map(|s| {
            let tags = snapshot_tags(s);
            snapshot_cluster_uid(&tags).map(str::to_string)
        })
        .collect();
    uids.sort();
    uids.dedup();
    uids
}

/// Classify one snapshot's tag set against this cluster's UID.
pub fn classify_snapshot(tags: &[String], this_cluster_uid: &str) -> SnapshotOrigin {
    match snapshot_cluster_uid(tags) {
        Some(uid) if uid == this_cluster_uid => SnapshotOrigin::ThisCluster,
        Some(_) => SnapshotOrigin::OtherCluster,
        None => SnapshotOrigin::Legacy,
    }
}

/// May this cluster act on the snapshot — select it as `latest`, forget it?
///
/// True for its own snapshots and for legacy ones (the stated, accepted
/// widening); false for another identified cluster's. This is THE predicate
/// every repo-wide listing is narrowed through before it feeds a decision.
pub fn owned_by_this_cluster(tags: &[String], this_cluster_uid: &str) -> bool {
    !matches!(
        classify_snapshot(tags, this_cluster_uid),
        SnapshotOrigin::OtherCluster
    )
}

/// The restic run tag: the cluster's `kube-system` UID, the run's RFC-3339
/// start time, and — for a namespace-narrowed local pull — the namespaces.
///
/// Every snapshot of one run shares this tag; it is both the run's grouping
/// key and its cluster attribution. Because the UID leads, two clusters'
/// run-tag namespaces are provably disjoint: a grouping pass can never merge
/// two clusters' runs even if a filter upstream is bypassed.
///
/// INVARIANT: [`tag_cluster_uid`] is the inverse of this function's leading
/// component. Changing the separator or the order breaks every filter that
/// reads a repository written by an older build.
pub fn run_tag(cluster_uid: &str, created_at: &str, subset_namespaces: &[String]) -> String {
    let base = format!("{cluster_uid}-{created_at}");
    if subset_namespaces.is_empty() {
        base
    } else {
        format!("{base}-ns-{}", subset_namespaces.join("_"))
    }
}

/// `metadata.uid` of a `Namespace` document, as the cluster's machine key.
///
/// Pure half of the read: the impure halves are `engine::read_cluster_uid`
/// (in-cluster runner, kube-rs) and the CLI's kubectl equivalent, so the
/// "what counts as a usable answer" rule is shared and tested once.
/// `None` when the document has no non-empty `metadata.uid` — which is the
/// shape an RBAC-truncated or absent object arrives in, and must never be
/// mistaken for an identity.
pub fn cluster_uid_of(namespace_json: &Value) -> Option<String> {
    namespace_json
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
}

/// The namespace whose UID identifies a Kubernetes cluster.
pub const IDENTITY_NAMESPACE: &str = "kube-system";

/// The error every cluster-identity read fails with, naming the RBAC rule
/// that is the only thing that can be missing in production.
///
/// The in-cluster runner reads this through its own ServiceAccount, so a
/// missing `namespaces: [get]` rule surfaces as a 403 at 03:00 and nowhere
/// else. Naming the rule in the message is the difference between a
/// five-minute fix and a debugging session.
pub fn identity_read_error(detail: &str) -> String {
    format!(
        "cannot read the `{IDENTITY_NAMESPACE}` namespace UID, which identifies this cluster in \
         the backup repository: {detail}\n\nThe in-cluster runner needs \
         `apiGroups: [\"\"], resources: [\"namespaces\"], verbs: [\"get\"]` in the \
         `apprafter-backup` ClusterRole; an operator running this from the CLI needs the same \
         read on their kubeconfig."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "11111111-2222-3333-4444-555555555555";
    const B: &str = "99999999-8888-7777-6666-555555555555";

    fn tags(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_canonical_uuid_is_recognised_and_near_misses_are_not() {
        assert!(is_uuid(A));
        assert!(is_uuid("AABBCCDD-1122-3344-5566-778899AABBCC"));
        // An actual `kube-system` UID, copied off a live apiserver — the
        // fixtures above are hand-made, and the only thing that matters is
        // that the shape Kubernetes really stamps parses.
        assert!(is_uuid("251d1b54-1d86-4bfc-9325-9304a339f4f1"));
        assert_eq!(
            tag_cluster_uid("251d1b54-1d86-4bfc-9325-9304a339f4f1-2026-09-13T01:00:00Z"),
            Some("251d1b54-1d86-4bfc-9325-9304a339f4f1")
        );
        // Wrong length, wrong grouping, non-hex, extra group.
        assert!(!is_uuid("11111111-2222-3333-4444-55555555555"));
        assert!(!is_uuid("111111112222333344445555555555555555"));
        assert!(!is_uuid("1111111g-2222-3333-4444-555555555555"));
        assert!(!is_uuid("11111111-2222-3333-4444-5555-55555555"));
        assert!(!is_uuid("platform"));
    }

    #[test]
    fn run_tag_and_tag_cluster_uid_are_inverses() {
        let t = run_tag(A, "2026-09-11T03:00:00.123456789+00:00", &[]);
        assert!(t.starts_with(A));
        assert_eq!(tag_cluster_uid(&t), Some(A));
        // …including the namespace-narrowed local-pull form.
        let t = run_tag(A, "2026-09-11T03:00:00Z", &["demo".into(), "prod".into()]);
        assert_eq!(t, format!("{A}-2026-09-11T03:00:00Z-ns-demo_prod"));
        assert_eq!(tag_cluster_uid(&t), Some(A));
    }

    #[test]
    fn a_legacy_tag_carries_no_uid() {
        // The exact shape every pre-fix snapshot in a live repository has.
        assert_eq!(tag_cluster_uid("platform-2026-09-11T03:00:00+00:00"), None);
        assert_eq!(tag_cluster_uid("prod-cluster-2026-09-11T03:00:00Z"), None);
        // A bare UUID with nothing after it is not a run tag.
        assert_eq!(tag_cluster_uid(A), None);
        assert_eq!(tag_cluster_uid(&format!("{A}-")), Some(A));
        assert_eq!(tag_cluster_uid(""), None);
    }

    #[test]
    fn classification_separates_this_cluster_from_another_and_from_legacy() {
        let mine = tags(&[&format!("{A}-2026-09-11T03:00:00Z")]);
        let theirs = tags(&[&format!("{B}-2026-09-11T03:00:01Z")]);
        let legacy = tags(&["platform-2026-09-11T03:00:00Z"]);
        let untagged: Vec<String> = Vec::new();

        assert_eq!(classify_snapshot(&mine, A), SnapshotOrigin::ThisCluster);
        assert_eq!(classify_snapshot(&theirs, A), SnapshotOrigin::OtherCluster);
        assert_eq!(classify_snapshot(&legacy, A), SnapshotOrigin::Legacy);
        assert_eq!(classify_snapshot(&untagged, A), SnapshotOrigin::Legacy);
    }

    #[test]
    fn ownership_covers_mine_and_legacy_but_never_another_cluster() {
        let mine = tags(&[&format!("{A}-2026-09-11T03:00:00Z")]);
        let theirs = tags(&[&format!("{B}-2026-09-11T03:00:01Z")]);
        let legacy = tags(&["platform-2026-09-11T03:00:00Z"]);

        assert!(owned_by_this_cluster(&mine, A));
        assert!(owned_by_this_cluster(&legacy, A), "stated widening");
        assert!(
            !owned_by_this_cluster(&theirs, A),
            "another cluster's snapshot is never ours to act on"
        );
        // …and the relation is symmetric: from B's side, A's is foreign.
        assert!(!owned_by_this_cluster(&mine, B));
        assert!(owned_by_this_cluster(&theirs, B));
    }

    #[test]
    fn the_uids_a_listing_carries_are_sorted_deduplicated_and_never_legacy() {
        let snaps = vec![
            serde_json::json!({"id":"1","tags":[format!("{B}-2026-09-11T03:00:00Z")]}),
            serde_json::json!({"id":"2","tags":[format!("{A}-2026-09-11T03:00:00Z")]}),
            serde_json::json!({"id":"3","tags":[format!("{A}-2026-09-12T03:00:00Z")]}),
            // Legacy and untagged carry no identity and must not appear as a
            // cluster: they are the ones attributed BY ASSUMPTION.
            serde_json::json!({"id":"4","tags":["platform-2026-09-11T03:00:00Z"]}),
            serde_json::json!({"id":"5"}),
        ];
        assert_eq!(cluster_uids_in(&snaps), vec![A.to_string(), B.to_string()]);
        assert!(cluster_uids_in(&[]).is_empty());
        // Borrowed entries too — the listing reaches this from a narrowed
        // `Vec<&Value>` as well as from the parsed `Vec<Value>`.
        let borrowed: Vec<&Value> = snaps.iter().collect();
        assert_eq!(
            cluster_uids_in(borrowed.iter().copied()),
            vec![A.to_string(), B.to_string()]
        );
    }

    #[test]
    fn snapshot_tags_reads_a_listing_entry_and_survives_a_missing_field() {
        let s = serde_json::json!({"tags":["a","b"]});
        assert_eq!(snapshot_tags(&s), vec!["a".to_string(), "b".to_string()]);
        // A snapshot restic wrote with no tags at all, and one whose `tags`
        // is null — both are "no tags", never a panic or a missing answer.
        assert!(snapshot_tags(&serde_json::json!({})).is_empty());
        assert!(snapshot_tags(&serde_json::json!({"tags":null})).is_empty());
    }

    #[test]
    fn cluster_uid_of_reads_the_namespace_and_refuses_an_empty_one() {
        let ns = serde_json::json!({"metadata":{"name":"kube-system","uid":A}});
        assert_eq!(cluster_uid_of(&ns).as_deref(), Some(A));
        assert_eq!(
            cluster_uid_of(&serde_json::json!({"metadata":{"uid":""}})),
            None
        );
        assert_eq!(cluster_uid_of(&serde_json::json!({})), None);
    }

    #[test]
    fn the_identity_error_names_the_rbac_rule_that_can_be_missing() {
        let e = identity_read_error("forbidden");
        assert!(e.contains("namespaces"), "{e}");
        assert!(e.contains("apprafter-backup"), "{e}");
    }
}
