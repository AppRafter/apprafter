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
    /// Key files without a config: an `init` that died between writing
    /// the master key and writing the config. restic refuses to init
    /// over those keys, so the location is wedged until they are gone.
    HalfInitialised,
    /// The store accepted the credentials and refused the write.
    WriteDenied,
    /// The store rejected the credentials themselves.
    BadCredentials,
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
            Self::HalfInitialised => Some(
                "The location holds restic key files but no repository config — what an \
                 `init` leaves behind when it dies between writing the master key and \
                 writing the config. restic will not init over those keys, so every retry \
                 fails the same way until they are gone. Two ways out: delete the `keys/` \
                 prefix under the repository path and re-run (an interrupted init has \
                 nothing else worth keeping), or leave it alone and point `--prefix` at a \
                 fresh path inside the same bucket. One caveat: if the location also holds \
                 `data/` and `snapshots/`, this is not an interrupted init but a real \
                 repository that lost its config — deleting the keys will not bring it \
                 back, and the snapshots are unreadable without that config.",
            ),
            Self::WriteDenied => Some(
                "The store answered and refused the write. The credentials were accepted, \
                 so this is a permission problem rather than a wrong key or a wrong \
                 passphrase: restic needs read, write AND delete on the whole repository \
                 prefix — it creates `config`, `keys/`, `data/`, `index/` and `snapshots/`, \
                 and removes them again on prune. Check what the access key is granted and \
                 whether a bucket policy narrows it, then re-run. When the refusal names \
                 `config` in particular, the store may be taking writes under a path while \
                 refusing them at the root of the bucket — `--prefix <path>` puts the whole \
                 repository one level down, which is a fine permanent arrangement and the \
                 usual way to keep several clusters in one bucket.",
            ),
            Self::BadCredentials => Some(
                "The store rejected the credentials themselves — an unknown access key or a \
                 signature that did not match — so nothing was read or written. Check \
                 `S3_ACCESS_KEY_ID` and `S3_SECRET_ACCESS_KEY` in the credential file: the \
                 values are passed through exactly as written, so surrounding quotes or a \
                 trailing space become part of the secret. A signature mismatch with a key \
                 you know is good usually means the endpoint and the key belong to \
                 different regions.",
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
    // Before the missing-repository family on purpose: the three below all
    // travel inside sentences that also say "does not exist" or "unable to
    // open config file", and each has a remedy the generic one would hide.
    if s.contains("already contains keys") {
        return ResticFailure::HalfInitialised;
    }
    if s.contains("invalidaccesskeyid")
        || s.contains("signaturedoesnotmatch")
        || s.contains("access key id you provided does not exist")
        || s.contains("request signature we calculated does not match")
    {
        return ResticFailure::BadCredentials;
    }
    if s.contains("access denied") || s.contains("accessdenied") {
        return ResticFailure::WriteDenied;
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
    fn an_init_that_died_after_writing_its_key_is_not_a_missing_repository() {
        // The shape a failed `backup enable` leaves behind: `init` wrote
        // `keys/<id>`, died before `config`, and every retry from then on
        // is refused by restic itself. Classifying it as RepoMissing sends
        // the reader to check their endpoint and credentials, which are
        // fine — the bucket is wedged, and nothing but deleting those keys
        // (or moving to a fresh prefix) unwedges it.
        assert_eq!(
            classify_restic(
                "Fatal: create key in repository at s3:https://nbg1.your-objectstorage.com/b \
                 failed: repository already contains keys"
            ),
            ResticFailure::HalfInitialised
        );
    }

    #[test]
    fn the_half_initialised_hint_offers_both_ways_out() {
        let hint = ResticFailure::HalfInitialised
            .hint()
            .expect("a wedged location has a remedy");
        assert!(hint.contains("keys/"), "names what to delete: {hint}");
        assert!(
            hint.contains("--prefix"),
            "names the no-delete way out: {hint}"
        );
    }

    #[test]
    fn a_refused_write_is_not_a_refused_credential() {
        // Hetzner Object Storage, real capture: the access key reads and
        // lists fine — `init` got as far as PutObject and was refused. The
        // remedy is a grant on the bucket, and saying "bad credentials"
        // here sends the reader to rotate a key that is not the problem.
        assert_eq!(
            classify_restic(
                "Save(<config/0000000000>) failed: client.PutObject: Access Denied.\n\
                 Fatal: create key in repository at s3:https://h/b failed: \
                 client.PutObject: Access Denied."
            ),
            ResticFailure::WriteDenied
        );
    }

    #[test]
    fn the_write_denied_hint_offers_the_prefix_that_actually_unblocked_one() {
        // A bucket that refuses `config` at its root and accepts the same
        // repository one path down is not hypothetical — that is what the
        // report this variant came from turned out to be, and `--prefix`
        // was what fixed it. A hint that only says "check your grants"
        // withholds the move that works.
        let hint = ResticFailure::WriteDenied
            .hint()
            .expect("a refused write has a remedy");
        assert!(hint.contains("--prefix"), "offers the way around: {hint}");
    }

    // Whether a refused `init` left a key file behind is NOT asserted
    // here. This hint is static, so it could only ever say "may have";
    // the caller reads the object name out of restic's own stderr and
    // states it as fact instead (`init_leftover_note`, tested against
    // both captures in `platform-cli`). Two texts saying the same thing
    // one confidence apart is how a help block stops being read.

    #[test]
    fn a_rejected_key_is_told_apart_from_a_refused_write() {
        // The one case where "bad credentials" is the true answer.
        assert_eq!(
            classify_restic(
                "Fatal: unable to open config file: The Access Key Id you provided does not \
                 exist in our records.: InvalidAccessKeyId"
            ),
            ResticFailure::BadCredentials
        );
        assert_eq!(
            classify_restic(
                "Fatal: Save(<lock/x>) failed: The request signature we calculated does not \
                 match the signature you provided.: SignatureDoesNotMatch"
            ),
            ResticFailure::BadCredentials
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
            ResticFailure::HalfInitialised,
            ResticFailure::WriteDenied,
            ResticFailure::BadCredentials,
        ] {
            assert!(r.hint().is_some_and(|h| h.len() > 60), "{r:?}");
        }
    }
}
