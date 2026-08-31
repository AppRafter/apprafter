// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Reading a subprocess's stderr well enough to say something useful.
//!
//! The D11 audit found 584 `CliError::Other` construction sites against
//! roughly eight files that construct a typed variant, and two families
//! dominating: `kubectl` (39 spawn sites) and `restic` (7). Their raw
//! stderr was being pasted into the catch-all verbatim, which is how the
//! quickstart ended up documenting `× spawn kubectl: No such file or
//! directory (os error 2)` as expected output — a documented UX that is
//! a catch-all error means the taxonomy has stopped describing the
//! product.
//!
//! The recurring failures are few, and each has a different remedy. That
//! is the whole argument for classifying rather than forwarding: an
//! operator who cannot reach the apiserver, one whose token lacks a
//! permission, and one whose cluster is missing a CRD are three
//! different problems that currently render as the same wall of text.
//!
//! # Scope
//!
//! These are **pure functions over the captured stderr**, so they test
//! without a cluster and without a binary. They are deliberately
//! conservative: anything unrecognised classifies as [`Other`] and
//! renders the original text unchanged. A wrong-but-confident
//! classification is worse than no classification — it sends the reader
//! somewhere else — so the patterns below only match phrasings upstream
//! actually emits.
//!
//! [`Other`]: KubectlFailure::Other

/// The `kubectl` failures worth telling apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KubectlFailure {
    /// The apiserver could not be reached at all.
    Unreachable,
    /// Reached, authenticated, and refused by RBAC.
    Forbidden,
    /// The kind itself is not served — almost always a CRD that has not
    /// been installed yet, which on this platform means the chart has
    /// not synced.
    KindNotServed,
    /// The kind is served; this named object is not there.
    ObjectNotFound,
    /// Unrecognised. Rendered verbatim.
    Other,
}

impl KubectlFailure {
    /// What to try next, or `None` when there is nothing better to say
    /// than the original stderr.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::Unreachable => Some(
                "The cluster's apiserver did not answer. Check that the target is running \
                 (`apprafter target ip`), that your kubeconfig points at it \
                 (`apprafter kubeconfig`), and that nothing between you and port 6443 is \
                 blocking — the origin firewall leaves 6443 open by design, so a refusal \
                 here is usually a stopped node or a stale kubeconfig.",
            ),
            Self::Forbidden => Some(
                "The apiserver was reached and refused the request. The credential is valid \
                 but lacks permission for this resource. If this is the platform's own \
                 service account, the operator's RBAC and the code have drifted apart — that \
                 has happened before and it always needs the verb added in the same change \
                 as the code that uses it.",
            ),
            Self::KindNotServed => Some(
                "The apiserver does not serve that kind. On this platform a missing custom \
                 resource almost always means the platform chart has not finished syncing: \
                 check `apprafter platform status`, and give Argo CD a moment on a freshly \
                 bootstrapped cluster.",
            ),
            Self::ObjectNotFound => None,
            Self::Other => None,
        }
    }
}

/// Classify a `kubectl` stderr blob.
///
/// Order matters: `Forbidden` is checked before the not-found family
/// because the apiserver says "not found" for a kind the caller may not
/// list, and treating that as a missing object would send the reader to
/// the wrong place entirely.
pub fn classify_kubectl(stderr: &str) -> KubectlFailure {
    let s = stderr.to_lowercase();

    if s.contains("connection refused")
        || s.contains("i/o timeout")
        || s.contains("no route to host")
        || s.contains("could not be reached")
        || s.contains("dial tcp")
        || s.contains("connect: network is unreachable")
        || s.contains("unable to connect to the server")
    {
        return KubectlFailure::Unreachable;
    }
    if s.contains("forbidden") || s.contains("is not allowed") || s.contains("unauthorized") {
        return KubectlFailure::Forbidden;
    }
    // "the server doesn't have a resource type" / "no matches for kind"
    // are the kind-level shapes; "not found" alone is object-level.
    if s.contains("doesn't have a resource type")
        || s.contains("no matches for kind")
        || s.contains("the server could not find the requested resource")
    {
        return KubectlFailure::KindNotServed;
    }
    if s.contains("not found") {
        return KubectlFailure::ObjectNotFound;
    }
    KubectlFailure::Other
}

/// The `restic` failures worth telling apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResticFailure {
    /// The passphrase did not open the repository.
    WrongPassphrase,
    /// The repository is absent or unreadable at that location.
    RepoMissing,
    /// A stale lock from an interrupted run.
    Locked,
    /// Unrecognised. Rendered verbatim.
    Other,
}

impl ResticFailure {
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::WrongPassphrase => Some(
                "restic rejected the passphrase. The repository itself is intact — this is a \
                 credential mismatch, not damage. Backups sealed by this platform use the \
                 passphrase stored with the target; `RESTIC_PASSWORD` in your environment \
                 overrides it and is the usual cause of a surprise here.",
            ),
            Self::RepoMissing => Some(
                "No repository at that location. For a remote repo, check the endpoint and \
                 the S3-style credentials; for a local one, that the path exists and is the \
                 repository root rather than a directory above it.",
            ),
            Self::Locked => Some(
                "The repository carries a lock from a run that did not finish. If nothing \
                 else is using it, `apprafter backup unlock` clears it — that command is \
                 built to work without a cluster, for exactly this situation.",
            ),
            Self::Other => None,
        }
    }
}

/// Classify a `restic` stderr blob.
pub fn classify_restic(stderr: &str) -> ResticFailure {
    let s = stderr.to_lowercase();

    if s.contains("wrong password")
        || s.contains("wrong passphrase")
        || s.contains("is not a repository or is corrupted")
    {
        return ResticFailure::WrongPassphrase;
    }
    if s.contains("repository is already locked") || s.contains("unable to create lock") {
        return ResticFailure::Locked;
    }
    if s.contains("unable to open config file")
        || s.contains("no such file or directory")
        || s.contains("specified key does not exist")
        || s.contains("does not exist")
    {
        return ResticFailure::RepoMissing;
    }
    ResticFailure::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_reads_the_three_kubectl_shapes_that_actually_recur() {
        assert_eq!(
            classify_kubectl(
                "Unable to connect to the server: dial tcp 1.2.3.4:6443: connect: \
                 connection refused"
            ),
            KubectlFailure::Unreachable
        );
        assert_eq!(
            classify_kubectl(
                "Error from server (Forbidden): resourceclaims.apprafter.io is forbidden: \
                 User \"system:serviceaccount:apprafter-system:apprafter-operator\" cannot \
                 delete resource"
            ),
            KubectlFailure::Forbidden
        );
        assert_eq!(
            classify_kubectl("error: the server doesn't have a resource type \"platformstacks\""),
            KubectlFailure::KindNotServed
        );
    }

    #[test]
    fn a_forbidden_kind_is_not_mistaken_for_a_missing_object() {
        // The apiserver says "not found" for things the caller may not
        // list. Classifying that as ObjectNotFound would send the reader
        // hunting for an object when the real problem is a missing verb
        // — a mistake this codebase has already paid for twice, in the
        // ADR 0048 anchor 403 and the 0.2.31 MigrationPlan GC.
        let stderr = "Error from server (Forbidden): configmaps \
                      \"platform-migration-anchor\" not found is forbidden";
        assert_eq!(classify_kubectl(stderr), KubectlFailure::Forbidden);
    }

    #[test]
    fn an_ordinary_missing_object_stays_ordinary() {
        assert_eq!(
            classify_kubectl(
                "Error from server (NotFound): applications.apprafter.io \"web\" not found"
            ),
            KubectlFailure::ObjectNotFound
        );
        // And carries no hint: there is nothing to add to "it is not there".
        assert!(KubectlFailure::ObjectNotFound.hint().is_none());
    }

    #[test]
    fn it_separates_a_wrong_passphrase_from_a_broken_repository() {
        // The distinction that matters most: one is a credential typo
        // and the other is missing data. Today both render as one raw
        // stderr blob, so an operator cannot tell whether their backups
        // still exist.
        assert_eq!(
            classify_restic("Fatal: wrong password or no key found"),
            ResticFailure::WrongPassphrase
        );
        assert_eq!(
            classify_restic(
                "Fatal: unable to open config file: Stat: The specified key does not exist."
            ),
            ResticFailure::RepoMissing
        );
        assert_eq!(
            classify_restic(
                "Fatal: unable to create lock in backend: repository is already locked exclusively"
            ),
            ResticFailure::Locked
        );
    }

    #[test]
    fn the_lock_hint_names_the_command_built_for_it() {
        let hint = ResticFailure::Locked.hint().expect("locked has a remedy");
        assert!(hint.contains("backup unlock"), "{hint}");
    }

    #[test]
    fn anything_unrecognised_classifies_as_other_and_adds_nothing() {
        // Conservative by construction: a confident wrong classification
        // sends the reader somewhere else, which is worse than handing
        // them the original text.
        assert_eq!(
            classify_kubectl("something entirely new"),
            KubectlFailure::Other
        );
        assert_eq!(
            classify_restic("something entirely new"),
            ResticFailure::Other
        );
        assert!(KubectlFailure::Other.hint().is_none());
        assert!(ResticFailure::Other.hint().is_none());
    }

    #[test]
    fn every_classified_failure_with_a_remedy_states_one() {
        // A variant that classifies but says nothing useful has bought
        // the reader nothing over the catch-all it replaced.
        for k in [
            KubectlFailure::Unreachable,
            KubectlFailure::Forbidden,
            KubectlFailure::KindNotServed,
        ] {
            assert!(k.hint().is_some_and(|h| h.len() > 60), "{k:?}");
        }
        for r in [
            ResticFailure::WrongPassphrase,
            ResticFailure::RepoMissing,
            ResticFailure::Locked,
        ] {
            assert!(r.hint().is_some_and(|h| h.len() > 60), "{r:?}");
        }
    }
}
