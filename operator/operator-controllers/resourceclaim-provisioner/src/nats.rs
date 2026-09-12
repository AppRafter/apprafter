// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Assembles [`ClaimView`]s from the live `ResourceClaim` set (2.5d Task
//! 7, ADR 0061 §6 amendment).
//!
//! Pure by construction: no Kubernetes client, no network, no clock —
//! `&[ResourceClaim]` plus the seed's quota mapping in, `Vec<ClaimView>`
//! out. Listing the claims (the actual `kube::Api::list` call) is I/O and
//! belongs to the next pass; everything that can be got wrong lives in
//! this transformation, which is why it is isolated here and fully
//! testable without a cluster.
//!
//! The prerequisite that makes this possible: `ResourceClaimSpec.jetstream`
//! (`operator-core::resourceclaim::ResourceClaimJetStream`) now carries
//! `dynamicStreams`/`streams`/`consume` on the claim itself — before that
//! landed, this function could only ever have produced empty declarations
//! for every claim, because `JetStreamNeed::as_service_need()` discards
//! all three when projecting onto the generic claim shape.

use std::collections::BTreeMap;

use operator_core::{
    JetStreamStream as CoreJetStreamStream, ResourceClaim, ResourceClaimCondition,
};

use crate::nats_accounts::{
    account_name, nats_durable_name, nats_stream_name, ClaimView, ConsumeView, StreamView,
};
use crate::nats_client;

/// Fallback `#Size` when a claim's `spec.size` is absent (`needs.jetstream`
/// makes it optional; nothing upstream fills a default today — confirmed
/// by reading `generate_resource_claims`, which omits the key entirely
/// rather than defaulting it, and by grepping every existing backend
/// (pg/redis/disk), none of which reads `spec.size` for sizing at all —
/// jetstream is the first consumer). `"small"` is a judgment call, not a
/// given: the second-smallest of five tiers, a reasonable floor rather
/// than either extreme. Named so the choice is visible at its one use
/// site instead of buried in a literal.
const DEFAULT_SIZE: &str = "small";

/// Builds one [`ClaimView`] per jetstream `ResourceClaim` in `claims`.
/// Non-jetstream claims (a different `spec.type`, or a jetstream claim
/// somehow missing its `spec.jetstream` sub-block) are silently skipped
/// — this function may be handed a mixed batch by a future caller, and a
/// caller listing "every claim in the namespace" rather than pre-filtering
/// is a reasonable thing to want to support cheaply.
///
/// `size_bytes` is the seed's `#Size` → bytes mapping
/// (`jetstream-integrated`'s `config.sizeBytes`,
/// `platform-stack/cue/service_providers.cue`) — injected, not read from
/// a live `ServiceProvider`, so this function stays pure. Each claim's
/// `quota_bytes` is resolved UNCLAMPED here — Task 8a's ceiling clamps
/// the NAMESPACE SUM, not each term (ADR 0061 §6), and summing/clamping
/// happens one layer up, in `nats_accounts::render_accounts_file`. This
/// function would silently defeat that design if it pre-clamped a single
/// claim's own quota — see `quota_bytes_is_the_unclamped_per_claim_size`
/// below.
///
/// EVERY claim this function accepts is included in the output —
/// unfiltered by app. `nats_accounts::deny_vector`'s class (B) covers an
/// app's own declared streams only because `render_account` is handed
/// `peers` INCLUDING the app being rendered itself; a caller that built a
/// peers-minus-self slice upstream of this function (or that filtered
/// this function's own OUTPUT down before grouping by namespace) would
/// lose class (B) with nothing failing anywhere. This function does not
/// do that filtering and never should.
pub fn claim_views(claims: &[ResourceClaim], size_bytes: &BTreeMap<String, u64>) -> Vec<ClaimView> {
    claims
        .iter()
        .filter_map(|claim| claim_view(claim, size_bytes))
        .collect()
}

/// The application that declared `claim` — resolved from
/// `metadata.ownerReferences`, NOT parsed from `metadata.name`.
///
/// `generate_resource_claims` (`operator-controllers/application`) sets
/// exactly one ownerReference on every claim it creates: `{kind:
/// "Application", name: <the real app name>, controller: true}` — read
/// directly from that function before writing this one, rather than
/// assumed. This is the same mechanism `reaper.rs` already uses
/// elsewhere in this crate to track claim/parent relationships (matching
/// on a structured reference, not a name), so it is not a new pattern
/// introduced here.
///
/// A name-parse (stripping `-jetstream` off `metadata.name`, since
/// jetstream claims are always named `<app>-jetstream` — ADR 0061 §6,
/// scalar-only, no `(type, name)` identity) was considered and rejected:
/// `claim_name` DNS-1123-folds the app name (lowercases; `.`/`_` → `-`;
/// truncates to 63 bytes) before composing the claim name, so recovering
/// the app from the claim name is LOSSY — an app called `My_App` composes
/// claim name `my-app-jetstream`, and parsing that back would silently
/// substitute the WRONG, folded name into every stream/user/subject this
/// module composes. `ownerReferences[0].name` carries the real,
/// unfolded name, because the Application controller wrote it directly
/// from `app.metadata.name` (never folded) at claim-creation time.
///
/// Defensive `None` return (not a panic, not an empty-string fallback)
/// for a claim missing this reference — every PRODUCTION claim has it
/// (admission rejects user-created claims, and `generate_resource_claims`
/// sets it unconditionally), so this is dead code in practice, the same
/// shape as `render_account`'s own `peers.is_empty()` guard: cheap
/// insurance against a caller this module cannot see, not a case
/// expected to fire.
pub(crate) fn declaring_app(claim: &ResourceClaim) -> Option<String> {
    claim
        .metadata
        .owner_references
        .as_ref()?
        .iter()
        .find(|r| r.kind == "Application" && r.controller == Some(true))
        .map(|r| r.name.clone())
}

/// One claim's view, or `None` if `claim` is not a jetstream claim (a
/// different `spec.type`, or `spec.jetstream` absent) or is missing its
/// declaring Application (see [`declaring_app`]).
fn claim_view(claim: &ResourceClaim, size_bytes: &BTreeMap<String, u64>) -> Option<ClaimView> {
    if claim.spec.type_ != "jetstream" {
        return None;
    }
    let js = claim.spec.jetstream.as_ref()?;
    let namespace = claim.metadata.namespace.clone()?;
    let app = declaring_app(claim)?;

    let size = claim.spec.size.as_deref().unwrap_or(DEFAULT_SIZE);
    // An unrecognised size name (the CRD's closed enum should make this
    // unreachable in production) floors to 0 rather than silently
    // picking some OTHER size's bytes — a visibly-wrong zero quota is
    // safer than a plausible-looking wrong number.
    let quota_bytes = size_bytes.get(size).copied().unwrap_or(0);

    let streams: Vec<StreamView> = js
        .streams
        .iter()
        .map(|s: &CoreJetStreamStream| StreamView {
            name: s.name.clone(),
            // Carried through, not read — part 3's NACK rendering and
            // the capture detector are the readers. Leave it alone.
            subjects: s.subjects.clone(),
            allow_purge: s.allow_purge,
            // 2.5f: read by the conditions, never by the accounts-file
            // renderer — see `StreamView`'s own doc.
            retention: s.retention.clone(),
            max_bytes: s.max_bytes.clone(),
        })
        .collect();

    // L1: `owner` is the DECLARING claim's own app when `from` is
    // omitted (an own-stream consume) — never the app currently being
    // iterated by some OUTER caller, which this function has no notion
    // of anyway (each claim is mapped independently). Getting this
    // wrong composes a deny-class-(D) pattern
    // (`nats_stream_name(&consume.owner, &consume.stream)`) against a
    // stream that does not exist, silently — the protection this class
    // exists to provide on a NEIGHBOUR's declared consumer never fires.
    let consumes: Vec<ConsumeView> = js
        .consume
        .iter()
        .map(|c| ConsumeView {
            owner: c.from.clone().unwrap_or_else(|| app.clone()),
            stream: c.stream.clone(),
            durable: c.durable.clone(),
        })
        .collect();

    Some(ClaimView {
        namespace,
        app,
        dynamic_streams: js.dynamic_streams,
        streams,
        consumes,
        quota_bytes,
    })
}

/// The per-namespace management user's Secret name in `nats-system`
/// (2.5d Task 10). `mgr_<namespace>` (`nats_accounts::mgr_user`) is a NATS
/// username, `_`-joined — not DNS-1123, so it cannot be the Secret name
/// directly. This just prepends a fixed prefix to the namespace VERBATIM
/// (no fold, no transform): a Kubernetes namespace name is already
/// DNS-1123, so the composed name is too, and two different namespaces can
/// never collide because the namespace itself is never altered, only
/// prefixed — unlike `nats_accounts::account_name`'s `_`-fold, there is no
/// separate injectivity argument to make here.
///
/// **A gap in the Task 6/9/10 brief, though not in the ADR**: ADR 0061 §1
/// ("Deployment") names `mgr_<ns>` as living in `nats-system` ("The
/// accounts Secret, the `mgr_<ns>` credential and the NACK CRs live there
/// too") but does not say how its password is SOURCED — and
/// `nats_accounts::render_account` REFUSES to render an account whose
/// `mgr_<ns>` password is empty (`AccountsFileError::EmptyPassword`, ADR
/// 0042 §10's "refuse don't clamp" carried over from Dragonfly), so
/// something durable has to hold it before the FIRST render for a
/// namespace. This mirrors `dragonfly`'s own admin-Secret precedent
/// exactly: a shared,
/// platform-internal credential, read-or-create, un-owned (it is not
/// claim-scoped — deleting the last claim in a namespace must not delete
/// the management identity a future claim in the SAME namespace would
/// need again).
pub fn mgr_secret_name(namespace: &str) -> String {
    format!("nats-mgr-{namespace}")
}

/// Reads `config.sizeBytes` — the `#Size` → bytes map the
/// `jetstream-integrated` seed carries (`service_providers.cue`) — off a
/// live `ServiceProvider.spec.config` into the `BTreeMap<String, u64>`
/// [`claim_views`] wants. Total: an absent `sizeBytes` key, a
/// non-object value, or a non-numeric entry is SKIPPED rather than
/// panicking or fabricating a zero map entry — an absent entry already
/// floors a claim's `quota_bytes` to 0 via `claim_view`'s own
/// "unrecognised size" path (a visibly-wrong zero), so this function does
/// not need its own failure mode on top of that one.
pub fn size_bytes_map(config: &serde_json::Value) -> BTreeMap<String, u64> {
    config
        .get("sizeBytes")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default()
}

/// The NATS connection Secret for one claim (2.5d Task 9, ADR 0061 §6) —
/// lands in the claim's OWN namespace, owner-ref'd to the `ResourceClaim`
/// so it cascades on delete (the pattern at `reconcile.rs`'s
/// `redis_connection_secret_object` / cnpg's `connection_secret_object`).
/// Carries EXACTLY the eight keys
/// `admission_webhook::validator::JETSTREAM_FIELDS` names —
/// `connection_secret_key_set_matches_the_webhooks_jetstream_fields`
/// (this module's own test) asserts the SET against that constant rather
/// than a hand-copy.
///
/// `url` embeds `user`/`pass` (`nats://user:pass@host:port`), matching
/// `redis_connection_secret_object`'s own `redis://user:pass@host:port/db`
/// convention — this is a STORED credential field, not a log line, so the
/// leak-safety reasoning in `nats_client.rs` (which deliberately never
/// embeds a password in a URL it might echo into an error) does not apply
/// here.
#[allow(clippy::too_many_arguments)]
pub fn connection_secret_object(
    name: &str,
    ns: &str,
    cv: &ClaimView,
    pass: &str,
    host: &str,
    port: u16,
    owner_uid: &str,
    owner_name: &str,
) -> serde_json::Value {
    let user = cv.user();
    let url = format!("nats://{user}:{pass}@{host}:{port}");
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
            "ownerReferences": [{
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "ResourceClaim",
                "name": owner_name,
                "uid": owner_uid,
                "controller": true,
                "blockOwnerDeletion": true,
            }],
        },
        "type": "Opaque",
        "stringData": {
            "url":           url,
            "host":          host,
            "port":          port.to_string(),
            "user":          user,
            "pass":          pass,
            "account":       cv.account(),
            "subjectPrefix": cv.subject_prefix(),
            "inboxPrefix":   cv.inbox_prefix(),
        },
    })
}

/// The whole-cluster accounts fragment Secret (2.5d Task 10, ADR 0061
/// §2/§3) — UNOWNED (no ownerReference: it is platform-scoped, not
/// claim-scoped, and must outlive any single claim's lifecycle), single
/// key `accounts.conf` matching `component_nats.cue`'s own
/// `"accounts$include": "./accounts-secret/accounts.conf"`.
pub fn accounts_secret_object(name: &str, ns: &str, accounts_conf: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
        },
        "type": "Opaque",
        "stringData": {
            "accounts.conf": accounts_conf,
        },
    })
}

/// A per-namespace management user's password Secret (2.5d Task 10) —
/// UNOWNED, same reasoning as [`accounts_secret_object`]: the identity is
/// platform-scoped, not tied to any one claim's lifecycle.
///
/// Carries `user` as well as `password` (2.5e Task 2): NACK's `Account`
/// CRD (`jetstream.nats.io/v1beta2`, verified against the CRD the `nack`
/// chart 0.35.0 actually installs) reads BOTH the username and the
/// password off SECRET KEYS named by `spec.user.user` /
/// `spec.user.password` — those two CRD fields name KEYS inside this
/// Secret, not literal value strings — so the Secret has to carry the
/// username itself for `user.user` to point at anything.
pub fn mgr_secret_object(name: &str, ns: &str, user: &str, password: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
        },
        "type": "Opaque",
        "stringData": {
            "user": user,
            "password": password,
        },
    })
}

/// The `Account` CR's Kubernetes object name for a namespace (2.5e Task
/// 2) — `ns-<namespace>`, hyphen-joined and DNS-1123 by construction (a
/// Kubernetes namespace name already is). Deliberately NOT the same
/// string as [`nats_accounts::account_name`]'s `ns_<namespace>`
/// (underscore-joined, the NATS-side account name that lives inside
/// `render_accounts_file`'s output) — a Kubernetes object name and a
/// value inside a config file are different worlds, and this function
/// exists so nothing conflates them by reusing one string for both.
pub fn account_k8s_name(namespace: &str) -> String {
    format!("ns-{namespace}")
}

/// The NACK `Account` CR for one namespace-account (2.5e Task 2, ADR
/// 0061 §1/§8) — CONNECTION CONFIGURATION telling NACK how to authenticate
/// as `mgr_<ns>` when it creates/updates `Stream`/`Consumer` objects in
/// this account, not an account definition of its own (the account
/// itself is the config-file entry `render_accounts_file` owns).
///
/// `spec.name` is the NATS-side account (`nats_accounts::account_name`) —
/// verified against the real CRD (`jetstream.nats.io/v1beta2`): both
/// `Account.spec.name` and `Stream.spec.account`/`Consumer.spec.account`
/// carry the SAME `^[^.*>]*$` NATS-subject-safe pattern, but they are NOT
/// the same kind of reference — `Account.spec.name` is this account's own
/// NATS-side identity, while `Stream`/`Consumer`'s `account` field is a
/// KUBERNETES OBJECT NAME (confirmed by reading the nack controller
/// source, `controller.go`'s `getAccountOverrides`, which resolves it via
/// `c.ji.Accounts(ns).Get(ctx, account, ...)` — a Kubernetes API GET by
/// object name, not a NATS-side lookup). This function's OWN `spec.name`
/// is the account's NATS identity; [`account_k8s_name`] is what a
/// `Stream`/`Consumer`'s `account` field must be set to instead.
///
/// `spec.user.user` / `spec.user.password` NAME KEYS inside
/// `mgr_secret_name`'s Secret (see [`mgr_secret_object`]'s own doc) —
/// fixed at `"user"`/`"password"` because this function and
/// `mgr_secret_object` are the only producer/consumer pair for this
/// Secret and choose the key names together.
pub fn account_object(
    namespace: &str,
    nats_ns: &str,
    server_url: &str,
    mgr_secret_name: &str,
) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "jetstream.nats.io/v1beta2",
        "kind": "Account",
        "metadata": {
            "name": account_k8s_name(namespace),
            "namespace": nats_ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
        },
        "spec": {
            "name": account_name(namespace),
            "servers": [server_url],
            "user": {
                "secret": { "name": mgr_secret_name },
                "user": "user",
                "password": "password",
            },
        },
    })
}

/// Parse a Kubernetes resource-quantity string (binary SI `Ki/Mi/Gi/Ti/Pi`,
/// decimal SI `k/M/G/T/P`, or a bare integer) to a base-unit byte count
/// (2.5e Task 3) — NACK's `Stream.spec.maxBytes` is a plain `integer`
/// (confirmed against the CRD the `nack` chart installs), while
/// `#JetStreamStream.maxBytes` (the CUE/webhook-validated claim field) is
/// a Kubernetes quantity STRING like `"1Gi"`, so something has to convert
/// between the two.
///
/// A hand-rolled parser, not a shared one: `admission-webhook`'s own
/// `quantity_to_f64` already solves this identical problem, but pulling
/// it in would make a validation-only crate a PRODUCTION dependency of
/// this one — the wrong direction of coupling (mirrors why
/// `PLATFORMSTACK_NAME`/`PLATFORMSTACK_NAMESPACE` are duplicated between
/// the CLI and the operator rather than shared: separate workspaces, no
/// common dependency worth taking on for a few lines). By the time a
/// claim reaches this code the webhook has ALREADY validated `maxBytes`
/// is a well-formed quantity, so this function's own `None` return on
/// malformed input is defensive, not a second gate anything relies on
/// being reachable.
pub fn quantity_bytes(q: &str) -> Option<i64> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    let idx = q.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(q.len());
    let (num, unit) = q.split_at(idx);
    let n: f64 = num.parse().ok()?;
    let mul: f64 = match unit {
        "" => 1.0,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        _ => return None,
    };
    Some((n * mul).round() as i64)
}

/// One declared stream's `Stream` CR (2.5e Task 3, ADR 0061 §1/§8) —
/// object name `<ns>-<owner app>-<declared name>` (Kubernetes, DNS-1123),
/// `spec.name` the NATS-side name from
/// [`nats_accounts::nats_stream_name`] — the SAME derivation the
/// deny-vector/allow-list already use, not a second copy of the join
/// formula (the whole reason that function's own doc argues the join is
/// collision-free is worth nothing if a second implementation of it can
/// drift).
///
/// **No `ownerReference` — deliberately, not an oversight.** This object
/// lives in `nats-system` while the declaring `ResourceClaim` lives in
/// the application's own namespace, and Kubernetes forbids a
/// cross-namespace owner reference outright. Deleted EXPLICITLY by the
/// orchestration that applies this (mirrors exactly how `needs.disk`
/// deletes its own unowned PVC — same shape, same reason).
///
/// `spec.account` is [`account_k8s_name`]'s output — the Kubernetes
/// object name of the `Account` CR in the SAME namespace, NOT the
/// NATS-side account name (see [`account_object`]'s own doc for the
/// verified distinction).
#[allow(clippy::too_many_arguments)]
pub fn stream_object(
    ns: &str,
    nats_ns: &str,
    owner_app: &str,
    declared_name: &str,
    subjects: &[String],
    storage: &str,
    retention: &str,
    max_age: &str,
    max_bytes: i64,
    account_object_name: &str,
) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "jetstream.nats.io/v1beta2",
        "kind": "Stream",
        "metadata": {
            "name": format!("{ns}-{owner_app}-{declared_name}"),
            "namespace": nats_ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
        },
        "spec": {
            "name": nats_stream_name(owner_app, declared_name),
            "subjects": subjects,
            "storage": storage,
            "retention": retention,
            "maxAge": max_age,
            "maxBytes": max_bytes,
            "account": account_object_name,
        },
    })
}

/// One declared `consume` entry's `Consumer` CR (2.5e Task 3, ADR 0061
/// §1/§8) — object name `<ns>-<consumer app>-<declared durable>`,
/// `spec.durableName` from [`nats_accounts::nats_durable_name`]
/// (consumer-keyed, not owner-keyed — see that function's own doc for
/// why the asymmetry is the point, not a bug).
///
/// **`spec.streamName` is the NATS-SIDE stream name
/// (`nats_accounts::nats_stream_name(owner, stream)`), NOT the `Stream`
/// CR's own Kubernetes object name.** Verified against the nack
/// controller source (`jsmclient.go`'s `LoadConsumer`/`NewConsumer`,
/// which pass this value straight into the JetStream manager client
/// library — a NATS protocol call, never a Kubernetes API call) — unlike
/// `spec.account` on this SAME object, which IS a Kubernetes object name
/// ([`account_k8s_name`]'s output, resolved via `controller.go`'s
/// `getAccountOverrides`). The two fields on one CR pointing at two
/// different KINDS of name is exactly the plausible-looking mistake this
/// round's own standing instruction ("verify a CRD field, don't inherit
/// a recollection of it") exists to catch — confirmed by reading both
/// code paths, not assumed from the field names' surface symmetry.
///
/// No `ownerReference`, same reasoning as [`stream_object`].
pub fn consumer_object(
    ns: &str,
    nats_ns: &str,
    consumer_app: &str,
    declared_durable: &str,
    owner_app: &str,
    owner_declared_stream_name: &str,
    account_object_name: &str,
) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "jetstream.nats.io/v1beta2",
        "kind": "Consumer",
        "metadata": {
            "name": format!("{ns}-{consumer_app}-{declared_durable}"),
            "namespace": nats_ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
        },
        "spec": {
            "durableName": nats_durable_name(consumer_app, declared_durable),
            "streamName": nats_stream_name(owner_app, owner_declared_stream_name),
            "ackPolicy": "explicit",
            "deliverPolicy": "all",
            "account": account_object_name,
        },
    })
}

// ---------------------------------------------------------------------------
// 2.5f — the observed-stream inventory, the capture detector, and the
// conditions (ADR 0061 §5 "Detection", §7 "Gating", §6 quota)
// ---------------------------------------------------------------------------
//
// Everything below is PURE: `&[StreamSummary]` (what the provisioner
// observed in NATS) plus the namespace's `ClaimView`s (what was declared)
// in, findings out. No client, no clock — `observed_at`/`now` are injected
// — so every rule here is unit-pinned rather than only observable on a
// live cluster, which for a detector is the difference between a rule and
// a hope.
//
// All of it runs off ONE listing. ADR 0061 §5: "Detection runs on the
// provisioner resync that already lists the account's streams." A second
// `STREAM.LIST` pass would not merely cost a round trip, it would let the
// inventory and the detector disagree about what was there.

/// Whether every one of `subjects` lies under `<app>.`.
///
/// **Prefix, with the separating dot, and nothing cleverer.** `feeder.`
/// does not prefix `feederbot.orders`, so two applications whose names
/// share a prefix cannot sweep (or accuse) each other. A subject equal to
/// the bare app name, or a bare `>`, is NOT under the prefix.
///
/// An EMPTY subject list is `false`, not vacuously true: a stream with no
/// subjects (a mirror, or one built from `sources` — ADR 0061 §4.1's read
/// vector) cannot be attributed by subject at all, and every rule keyed on
/// this function keys on subjects, never on names. Treating it as "wholly
/// under" would sweep, or clear, exactly the streams there is no evidence
/// about.
///
/// Lives here rather than in `gc.rs` (where it was written first, 2.5e)
/// because 2.5f gave it three more callers — the inventory, the detector
/// and the GC all decide "is this stream this application's" by the same
/// sentence, and two copies of it would be two rules the day one is
/// edited.
pub(crate) fn subjects_wholly_under(subjects: &[String], app_prefix: &str) -> bool {
    !subjects.is_empty() && subjects.iter().all(|s| s.starts_with(app_prefix))
}

/// Whether ANY of `subjects` lies under `<app>.` — the weaker half of
/// [`subjects_wholly_under`], and the one that decides whether a stream is
/// an application's CONCERN at all (ADR 0061 §5: "a stream whose subjects
/// touch `<app>.`").
pub(crate) fn subjects_touch(subjects: &[String], app_prefix: &str) -> bool {
    subjects.iter().any(|s| s.starts_with(app_prefix))
}

/// Every stream DECLARED anywhere in the namespace, keyed by its composed
/// NATS-side name, carrying the declaring application and the declaration.
///
/// The single index four rules below consult. "Declared by SOME
/// application in the namespace" is the legitimacy test in ADR 0061 §5 and
/// the exclusion in §7's `unattributed` — **including when the declaring
/// application is not the one being reported on**, which is what keeps a
/// neighbour's approved fan-in stream (§6) out of both. Getting that wrong
/// in the narrow direction — "the victim's OWN declared streams" — would
/// have the detector flag an approved stream and, at the `delete` step,
/// have the platform destroy live data.
fn declared_by_composed_name(peers: &[ClaimView]) -> BTreeMap<String, (&str, &StreamView)> {
    let mut out = BTreeMap::new();
    for p in peers {
        for s in &p.streams {
            out.insert(nats_stream_name(&p.app, &s.name), (p.app.as_str(), s));
        }
    }
    out
}

/// What the provisioner OBSERVED in this application's account, as
/// `ResourceClaim.status.streams` (ADR 0061 §9).
///
/// Three disjoint lists over the OBSERVED set, never over the declared
/// one: a declared stream that does not exist yet appears in none of them
/// (the `AwaitingStreamCreation` gate is what reports that), and a
/// neighbour's stream that never touches this application appears in none
/// of them either — it is not this claim's business.
///
/// Three consumers, and this is the ONLY mechanism serving all three:
/// MigrationPlan enumeration (the Application controller holds no NATS
/// client and reads the claim's status), `apprafter app status` through
/// the generic `status.size.bytes` path, and the `QuotaExceeded`
/// pre-flight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamInventory {
    /// Streams THIS application declared, observed live.
    pub declared: Vec<String>,
    /// Undeclared streams whose subjects lie WHOLLY under `<app>.`.
    pub dynamic: Vec<String>,
    /// Streams that touch `<app>.` but are neither wholly under it nor
    /// declared by ANY application in the namespace — plus subject-less
    /// streams, which cannot be attributed by subject at all.
    pub unattributed: Vec<String>,
    /// RFC3339. Injected, so a stale inventory is visibly stale rather
    /// than silently current.
    pub observed_at: String,
}

/// Classify `observed` from `me`'s point of view (ADR 0061 §9).
///
/// `peers` is every jetstream claim in the namespace **including `me`** —
/// the same whole-namespace input `render_accounts_file` takes, for the
/// same reason: legitimacy is a property of the namespace's declarations
/// as a set, not of any one claim's.
pub fn stream_inventory(
    observed: &[nats_client::StreamSummary],
    me: &ClaimView,
    peers: &[ClaimView],
    observed_at: &str,
) -> StreamInventory {
    let declared_ns = declared_by_composed_name(peers);
    let mine = me.subject_prefix();
    let mut out = StreamInventory {
        observed_at: observed_at.to_string(),
        ..Default::default()
    };
    for s in observed {
        // Declared by SOMEBODY in this namespace. `me`'s own go in
        // `declared`; a neighbour's — fan-in included — are attributed,
        // just not to us, and belong in NO list here. They must never
        // reach `unattributed`: that is the list the GC and the detector
        // read as "nobody vouches for this."
        if let Some((owner, _)) = declared_ns.get(&s.name) {
            if *owner == me.app {
                out.declared.push(s.name.clone());
            }
            continue;
        }
        if subjects_wholly_under(&s.subjects, &mine) {
            out.dynamic.push(s.name.clone());
        } else if subjects_touch(&s.subjects, &mine) || s.subjects.is_empty() {
            out.unattributed.push(s.name.clone());
        }
    }
    for list in [&mut out.declared, &mut out.dynamic, &mut out.unattributed] {
        list.sort();
        list.dedup();
    }
    out
}

/// Total bytes held by the streams this claim can be said to own — its
/// own declared streams plus its dynamic ones. Feeds `status.size.bytes`
/// (the generic path `apprafter app status` already renders).
///
/// `unattributed` is deliberately EXCLUDED: those bytes belong to nobody
/// this code can name, and charging them to whichever claim happens to
/// share a prefix would make one tenant's figure move when another's
/// stream grew.
pub fn inventory_held_bytes(
    observed: &[nats_client::StreamSummary],
    inventory: &StreamInventory,
) -> u64 {
    observed
        .iter()
        .filter(|s| {
            inventory.declared.iter().any(|n| n == &s.name)
                || inventory.dynamic.iter().any(|n| n == &s.name)
        })
        .map(|s| s.bytes)
        .sum()
}

/// What to do about a detected capture (ADR 0061 §5's policy ladder).
///
/// **There is no `quarantine`.** The ADR names it as the third rung, and
/// it stays unbuilt on purpose: revoking the culprit's
/// `STREAM.CREATE`/`UPDATE` needs ATTRIBUTION, attribution needs a live
/// `$JS.EVENT.ADVISORY.API` subscription that identifies the requesting
/// user, and that subscription has not been verified (ADR 0061
/// "Pre-merge verification" item 6). NATS records no creator on a stream
/// and `StreamConfig.metadata` is client-supplied. So the variant is
/// absent from this enum rather than present-and-unimplemented: a name in
/// an enum is a promise, and an unparseable `quarantine` in a provider
/// config falls back to `Report` with a warning rather than silently
/// doing nothing under a reassuring name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CapturePolicy {
    /// Condition + event on the victim's claim. The default, and the only
    /// value the shipped seed sets.
    #[default]
    Report,
    /// Delete the offending stream. Exposure is bounded by the resync
    /// interval, not by the detector.
    Delete,
}

/// Read `config.capturePolicy` off a matched `ServiceProvider.spec.config`.
/// Anything absent or unrecognised — `quarantine` included — is
/// [`CapturePolicy::Report`], returned alongside the raw string so the
/// caller can say what it ignored rather than silently downgrading.
pub fn capture_policy(config: &serde_json::Value) -> (CapturePolicy, Option<String>) {
    match config.get("capturePolicy").and_then(|v| v.as_str()) {
        None => (CapturePolicy::Report, None),
        Some("report") => (CapturePolicy::Report, None),
        Some("delete") => (CapturePolicy::Delete, None),
        Some(other) => (CapturePolicy::Report, Some(other.to_string())),
    }
}

/// Streams that have captured subjects under `<me>.` (ADR 0061 §5).
///
/// A stream whose subjects touch `<app>.` is legitimate **iff**:
///
/// 1. it is a declared stream of *some* application in the namespace —
///    `declared_by_composed_name`, which is why an approved fan-in stream
///    belonging to a NEIGHBOUR is not a capture; or
/// 2. it is a dynamic stream of an application that HOLDS
///    `dynamicStreams: true` and whose subjects lie wholly under that
///    application's own prefix.
///
/// **Rule 2's `dynamicStreams` clause is not decoration.** ADR 0061 §5
/// states the rule without it, but §3 states the consequence the clause is
/// required for: the flag's "primary value is not the reduced surface but
/// that it makes §5's capture detection EXACT for every opted-out
/// application." An application without the flag has no
/// `$JS.API.STREAM.CREATE` at all (`nats_accounts::allow_list` grants it
/// only under the flag), so an undeclared stream sitting wholly under its
/// prefix provably was not created by it — and without the clause the
/// detector would accept exactly that stream as "theirs, presumably" and
/// see nothing. With it, a capture aimed at an opted-out neighbour is
/// caught with no attribution required. The cost is the residue case the
/// ADR already names (§4.1): flipping the flag from `true` to `false`
/// leaves that application's own old dynamic streams reported as
/// captures, which is precisely what "§5's `delete` step is what handles
/// residue" means.
///
/// A SUBJECT-LESS stream is never reported here, though the inventory does
/// list it as `unattributed`. It does not "touch `<app>.`" by any reading,
/// and the `delete` rung acts on this list — so flagging a stream the rule
/// has no evidence about would turn a detector into a data-loss primitive.
/// That asymmetry is the conservative direction and is the point.
pub fn foreign_subject_captures(
    observed: &[nats_client::StreamSummary],
    me: &ClaimView,
    peers: &[ClaimView],
) -> Vec<String> {
    let declared_ns = declared_by_composed_name(peers);
    let mine = me.subject_prefix();
    let mut out: Vec<String> = observed
        .iter()
        .filter(|s| subjects_touch(&s.subjects, &mine))
        .filter(|s| !declared_ns.contains_key(&s.name))
        .filter(|s| {
            !peers.iter().any(|p| {
                p.dynamic_streams && subjects_wholly_under(&s.subjects, &p.subject_prefix())
            })
        })
        .map(|s| s.name.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whether two NATS subject patterns can both match one concrete subject.
///
/// Token-wise, because NATS wildcards are whole-token (ADR 0061's opening
/// constraint): `*` matches exactly one token, `>` matches one or more
/// TRAILING tokens, and a literal matches only itself. The
/// one-or-more part is what makes `a.>` not overlap `a` — a difference a
/// `starts_with` test would get wrong in the direction that matters, by
/// reporting an overlap that JetStream itself does not see.
fn subjects_overlap(a: &str, b: &str) -> bool {
    let at: Vec<&str> = a.split('.').collect();
    let bt: Vec<&str> = b.split('.').collect();
    let mut i = 0;
    loop {
        match (at.get(i), bt.get(i)) {
            (None, None) => return true,
            // One pattern ran out while the other still has tokens, and
            // neither ended in `>` (that is handled below, before either
            // could run out) — no concrete subject satisfies both.
            (None, Some(_)) | (Some(_), None) => return false,
            (Some(x), Some(y)) => {
                if *x == ">" || *y == ">" {
                    return true;
                }
                if *x != "*" && *y != "*" && x != y {
                    return false;
                }
                i += 1;
            }
        }
    }
}

/// Whether any subject in `a` overlaps any subject in `b`.
fn any_subject_overlap(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| b.iter().any(|y| subjects_overlap(x, y)))
}

/// Whether a declaration asked for `retention: workqueue`.
///
/// `None` is `limits` — the CRD's own default when the manifest says
/// nothing — so absence reads as "no", never as "unknown". The whole
/// point of the workqueue-specific conditions is that ADR 0061 §4.1's
/// drain primitive is destructive ONLY against a workqueue origin.
fn is_workqueue(s: &StreamView) -> bool {
    s.retention.as_deref() == Some("workqueue")
}

/// The namespace quota pre-flight (ADR 0061 §6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuotaVerdict {
    /// The account's `max_file`, exactly as `render_accounts_file` will
    /// write it — sum of the namespace's claims, clamped by the tier
    /// ceiling.
    pub budget_bytes: u64,
    /// What is already promised, plus whichever of `me`'s own pending
    /// declarations fit.
    pub reserved_bytes: u64,
    /// `me`'s own DECLARED stream names (as declared, not composed) that
    /// must not be applied to NACK this pass.
    pub blocked: Vec<String>,
}

/// Decide, BEFORE creating anything, whether `me`'s declared streams fit
/// the namespace account (ADR 0061 §6: "`maxBytes` is required on every
/// declared stream and the namespace total is checked before creation,
/// surfacing `QuotaExceeded` rather than an opaque server error through
/// NACK" — the opaque error being nats-server's `insufficient storage
/// resources available (10047)`, which names neither the quota nor the
/// stream that exhausted it).
///
/// **Only streams that do not exist YET can be blocked, and that is the
/// property that keeps this safe to run on a live claim.** An existing
/// stream's bytes are already reserved inside NATS; refusing it would
/// change nothing there and would flip a healthy application unready on a
/// neighbour's edit. So an observed stream is a fait accompli, counted
/// into `reserved_bytes` and never listed in `blocked` — which makes the
/// verdict monotone: a claim that fits today cannot be un-fitted by
/// anything except its own new declarations.
///
/// A negative `max_bytes` on an observed stream (JetStream's "unlimited")
/// contributes ZERO rather than subtracting. It reserves nothing the
/// account can promise; the account's own `max_file` is what actually
/// bounds it.
///
/// **`budget_bytes` can briefly exceed the account's real `max_file`, and
/// the error runs in the safe direction.** `peers` is every jetstream
/// claim in the namespace, while `render_accounts_file` sums only the
/// claims that already have a connection Secret — so during the window
/// where a sibling claim exists but has not finished its own first
/// provisioning pass, this budget is the larger number. The consequence is
/// that the pre-flight is momentarily PERMISSIVE: a declaration it lets
/// through may still meet nats-server's `10047`, which is exactly the
/// pre-2.5f behaviour and self-corrects on the next pass. The reverse — a
/// pre-flight refusing a stream the server would have accepted — would be
/// a wedged claim needing a manifest edit to escape, and cannot happen
/// here.
pub fn quota_verdict(
    observed: &[nats_client::StreamSummary],
    me: &ClaimView,
    peers: &[ClaimView],
    ceiling_bytes: u64,
) -> QuotaVerdict {
    let budget = crate::nats_accounts::namespace_quota_bytes(peers, ceiling_bytes);
    let declared_ns = declared_by_composed_name(peers);
    let exists = |composed: &str| observed.iter().any(|o| o.name == composed);

    // Everything already promised: every declaration in the namespace
    // EXCEPT `me`'s own not-yet-created ones (those are what this verdict
    // is deciding about), plus every observed stream nobody declared.
    let mut reserved: u64 = 0;
    for (composed, (owner, s)) in &declared_ns {
        if *owner == me.app && !exists(composed) {
            continue;
        }
        reserved = reserved.saturating_add(declared_bytes(&s.max_bytes));
    }
    for o in observed {
        if declared_ns.contains_key(&o.name) {
            continue;
        }
        if o.max_bytes > 0 {
            reserved = reserved.saturating_add(o.max_bytes as u64);
        }
    }

    // Fit `me`'s pending declarations in declaration order — the order the
    // manifest lists them, so which one is refused is a stable, explicable
    // fact about the manifest rather than an artefact of a sort.
    let mut blocked = Vec::new();
    for s in &me.streams {
        let composed = nats_stream_name(&me.app, &s.name);
        if exists(&composed) {
            continue;
        }
        let want = declared_bytes(&s.max_bytes);
        if reserved.saturating_add(want) > budget {
            blocked.push(s.name.clone());
        } else {
            reserved = reserved.saturating_add(want);
        }
    }

    QuotaVerdict {
        budget_bytes: budget,
        reserved_bytes: reserved,
        blocked,
    }
}

/// A declared `maxBytes` quantity as bytes, floored at 0. The admission
/// webhook has already rejected an empty or malformed value, so the
/// fallback is defensive: an unparseable declaration reserves NOTHING
/// rather than blocking the namespace on a number nobody can read.
fn declared_bytes(max_bytes: &str) -> u64 {
    quantity_bytes(max_bytes).unwrap_or(0).max(0) as u64
}

/// Streams declared by ANOTHER application that already cover `<me>.`
/// (ADR 0061 §7, the `PrefixPreCaptured` half of trigger #16).
///
/// Trigger #16 fires on any foreign first token, "including inert ones" —
/// a declaration naming `newapp.orders.>` passes the gate at a time when
/// no `newapp` exists, because narrowing the trigger to live applications
/// "would need a sibling lookup the webhook does not have and would
/// reopen pre-positioned capture." This condition is what covers the gap
/// from the other side: when `newapp` finally arrives, it is TOLD that its
/// prefix is already inside somebody else's stream.
///
/// Computed from DECLARATIONS, never from `observed` — deliberately. It is
/// the only condition here that does not need a working NATS connection,
/// and an arriving application should learn this on its very first
/// reconcile, before any stream exists.
///
/// A neighbour's fan-in stream is a legitimate, gated construct (ADR 0061
/// §6), so this is a REPORT, not a fault: the platform is not refusing
/// anything, it is refusing to let it be a surprise.
pub fn prefix_pre_captured(me: &ClaimView, peers: &[ClaimView]) -> Vec<String> {
    let mine = me.subject_prefix();
    let mut out: Vec<String> = peers
        .iter()
        .filter(|p| p.app != me.app)
        .flat_map(|p| {
            p.streams
                .iter()
                .filter(|s| subjects_touch(&s.subjects, &mine))
                .map(move |s| nats_stream_name(&p.app, &s.name))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Workqueue streams in the namespace that a `dynamicStreams: true`
/// neighbour could drain (ADR 0061 §4.1).
///
/// The measurement this exists for: a user denied every `$JS.API.*`
/// subject addressing a stream can still read it by creating a second
/// stream with `sources: [{name: <denied>}]`, because the server performs
/// the copy in the ACCOUNT's context and never inspects a request body
/// while evaluating permissions. Against a `workqueue` origin that copy
/// DRAINS it — measured: an origin holding three messages reported
/// `Messages: 0` afterwards. So the risk is real, unpreventable inside one
/// account, and exactly bounded by who holds `dynamicStreams`.
///
/// **Narrowed: the workqueue's OWN owner holding the flag is not a risk to
/// itself.** The un-narrowed rule fires on every namespace where the one
/// application that declared a workqueue also creates dynamic streams —
/// which is a completely ordinary shape, and a condition that is always on
/// is a condition nobody reads.
///
/// Surfaced on BOTH parties (the ADR's own mitigation wording): the
/// workqueue's owner, who is at risk, and the flag holder, who IS the
/// risk. Neither learns it from the other otherwise.
pub fn namespace_drain_risk(me: &ClaimView, peers: &[ClaimView]) -> Vec<String> {
    let holders: Vec<&str> = peers
        .iter()
        .filter(|p| p.dynamic_streams)
        .map(|p| p.app.as_str())
        .collect();
    let mut out = Vec::new();
    for p in peers {
        let others: Vec<&str> = holders.iter().copied().filter(|h| *h != p.app).collect();
        if others.is_empty() {
            continue;
        }
        let concerns_me = me.app == p.app || (me.dynamic_streams && me.app != p.app);
        if !concerns_me {
            continue;
        }
        for s in p.streams.iter().filter(|s| is_workqueue(s)) {
            out.push(format!(
                "{} (dynamicStreams held by {})",
                nats_stream_name(&p.app, &s.name),
                others.join(", ")
            ));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Streams whose subjects overlap a declared WORKQUEUE stream's.
///
/// JetStream gives a workqueue stream exclusive ownership of its subjects:
/// a second stream collecting any of them either refuses to be created or
/// takes delivery the workqueue was counting on. Both directions are
/// reported — declared-vs-declared as well as declared-vs-dynamic —
/// because the declared-vs-declared case is the one a manifest review can
/// still prevent, and it is the one a detector built only against dynamic
/// streams would never mention.
///
/// An undeclared (dynamic) overlap is surfaced on the WORKQUEUE's owner
/// only: nothing attributes a dynamic stream to an application, so there
/// is no second party to tell.
pub fn workqueue_subject_overlap(
    observed: &[nats_client::StreamSummary],
    me: &ClaimView,
    peers: &[ClaimView],
) -> Vec<String> {
    let declared_ns = declared_by_composed_name(peers);
    let mut out = Vec::new();
    for (w_name, (w_owner, w)) in &declared_ns {
        if !is_workqueue(w) {
            continue;
        }
        for (o_name, (o_owner, o)) in &declared_ns {
            if o_name == w_name || !any_subject_overlap(&w.subjects, &o.subjects) {
                continue;
            }
            if me.app == *w_owner || me.app == *o_owner {
                out.push(format!(
                    "{w_name} (workqueue) overlaps the declared stream {o_name}"
                ));
            }
        }
        if me.app != *w_owner {
            continue;
        }
        for s in observed {
            if declared_ns.contains_key(&s.name) || !any_subject_overlap(&w.subjects, &s.subjects) {
                continue;
            }
            out.push(format!(
                "{w_name} (workqueue) overlaps the undeclared stream {}",
                s.name
            ));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `consume` entries naming a stream that does not exist and that nobody
/// declares.
///
/// An OBSERVED stream of the right composed name clears the entry even
/// when no declaration matches: an owner holding `dynamicStreams: true`
/// may legitimately create the stream itself, and reporting a working
/// consumer as broken forever is worse than the missed case. The
/// declaration is checked first, so the ordinary path needs no NATS
/// evidence at all.
pub fn consume_target_missing(
    observed: &[nats_client::StreamSummary],
    me: &ClaimView,
    peers: &[ClaimView],
) -> Vec<String> {
    let mut out = Vec::new();
    for c in &me.consumes {
        let declared = peers
            .iter()
            .any(|p| p.app == c.owner && p.streams.iter().any(|s| s.name == c.stream));
        if declared {
            continue;
        }
        let composed = nats_stream_name(&c.owner, &c.stream);
        if observed.iter().any(|o| o.name == composed) {
            continue;
        }
        out.push(format!("{composed} (durable {})", c.durable));
    }
    out.sort();
    out.dedup();
    out
}

/// `me`'s declared streams whose composed NATS name is already taken by a
/// live stream carrying DIFFERENT subjects.
///
/// The within-one-application collisions — a duplicate `streams[].name`,
/// and a `consume[].durable` equal to one of this application's own stream
/// names (ADR 0061 §4.2's soundness premise) — are rejected by the
/// admission webhook before a claim exists, and cross-application
/// collisions are impossible by construction because the `_` join is
/// injective (§6). What remains is the RUNTIME case none of that reaches:
/// a stream of that exact name already sitting in the account, created by
/// someone who is not NACK — a `dynamicStreams` neighbour squatting the
/// name ahead of NACK's own create, or residue from a previous
/// application of the same name.
///
/// It matters because the composed name is what the deny vector keys on:
/// while the squat stands, the permission rules written for the declared
/// stream apply to the squatter's data instead.
///
/// Subjects are compared as SETS: NACK writes them back in declaration
/// order, but nothing in the protocol promises that, and a false alarm
/// caused by a reordering would train a reader to ignore the condition.
pub fn stream_name_conflict(
    observed: &[nats_client::StreamSummary],
    me: &ClaimView,
) -> Vec<String> {
    let mut out = Vec::new();
    for s in &me.streams {
        let composed = nats_stream_name(&me.app, &s.name);
        let Some(live) = observed.iter().find(|o| o.name == composed) else {
            continue;
        };
        let mut want: Vec<&String> = s.subjects.iter().collect();
        let mut got: Vec<&String> = live.subjects.iter().collect();
        want.sort();
        want.dedup();
        got.sort();
        got.dedup();
        if want != got {
            out.push(format!(
                "{composed} exists with subjects {:?}, not the declared {:?}",
                live.subjects, s.subjects
            ));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Condition types this module writes onto a jetstream `ResourceClaim`.
/// Present means FIRING; a condition that stops applying is removed, not
/// flipped to `False` — so `kubectl get` shows the live problems and
/// nothing else. (`Ready` is not among them: it belongs to the
/// provisioner's own field manager and is written elsewhere.)
pub const COND_FOREIGN_SUBJECT_CAPTURE: &str = "ForeignSubjectCapture";
pub const COND_QUOTA_EXCEEDED: &str = "QuotaExceeded";
pub const COND_PREFIX_PRE_CAPTURED: &str = "PrefixPreCaptured";
pub const COND_NAMESPACE_DRAIN_RISK: &str = "NamespaceDrainRisk";
pub const COND_WORKQUEUE_SUBJECT_OVERLAP: &str = "WorkqueueSubjectOverlap";
pub const COND_CONSUME_TARGET_MISSING: &str = "ConsumeTargetMissing";
pub const COND_STREAM_NAME_CONFLICT: &str = "StreamNameConflict";

/// Every condition type this module may write — the set a caller has to
/// know to reason about what a jetstream claim's status can carry.
pub const JETSTREAM_CONDITION_TYPES: &[&str] = &[
    COND_FOREIGN_SUBJECT_CAPTURE,
    COND_QUOTA_EXCEEDED,
    COND_PREFIX_PRE_CAPTURED,
    COND_NAMESPACE_DRAIN_RISK,
    COND_WORKQUEUE_SUBJECT_OVERLAP,
    COND_CONSUME_TARGET_MISSING,
    COND_STREAM_NAME_CONFLICT,
];

/// One condition, preserving `lastTransitionTime` across passes so a
/// standing problem does not look new on every resync (the same hot-loop
/// guard `reconcile::ready_condition` uses for `Ready`). `status` is
/// always `"True"` — see [`JETSTREAM_CONDITION_TYPES`] for why absence,
/// not `False`, is how a cleared condition reads.
fn condition(
    type_: &str,
    reason: &str,
    message: String,
    now: &str,
    prior: &[ResourceClaimCondition],
) -> ResourceClaimCondition {
    let last_transition_time = prior
        .iter()
        .find(|c| c.type_ == type_ && c.status == "True")
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| now.to_string());
    ResourceClaimCondition {
        type_: type_.to_string(),
        status: "True".to_string(),
        reason: Some(reason.to_string()),
        message: Some(message),
        last_transition_time,
    }
}

/// Everything one resync learns about a jetstream claim.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JetStreamSignals {
    pub inventory: StreamInventory,
    /// NATS-side names of streams that captured subjects under `<me>.`.
    /// The `delete` rung acts on exactly this list.
    pub captures: Vec<String>,
    pub quota: QuotaVerdict,
    /// Bytes held by this claim's own streams — `status.size.bytes`.
    pub held_bytes: u64,
    /// Only the conditions that FIRE, in a stable order.
    pub conditions: Vec<ResourceClaimCondition>,
}

/// Compute every 2.5f signal for one claim from one observation.
///
/// Pure: `now` and `observed_at` are injected and `prior` is the claim's
/// existing conditions (for `lastTransitionTime` preservation). The
/// caller does the I/O — listing streams once, applying the capture
/// policy, writing the status — so the RULES are all testable without a
/// cluster and the plumbing has nothing in it to get wrong.
pub fn jetstream_signals(
    observed: &[nats_client::StreamSummary],
    me: &ClaimView,
    peers: &[ClaimView],
    ceiling_bytes: u64,
    now: &str,
    prior: &[ResourceClaimCondition],
) -> JetStreamSignals {
    let inventory = stream_inventory(observed, me, peers, now);
    let held_bytes = inventory_held_bytes(observed, &inventory);
    let captures = foreign_subject_captures(observed, me, peers);
    let quota = quota_verdict(observed, me, peers, ceiling_bytes);

    let mut conditions = Vec::new();
    if !captures.is_empty() {
        conditions.push(condition(
            COND_FOREIGN_SUBJECT_CAPTURE,
            "ForeignSubjects",
            format!(
                "{} stream(s) in this account carry subjects under {:?} that no declaration in \
                 this namespace accounts for: {}. NATS records no creator on a stream, so the \
                 platform reports the capture without naming who made it.",
                captures.len(),
                me.subject_prefix(),
                captures.join(", ")
            ),
            now,
            prior,
        ));
    }
    if !quota.blocked.is_empty() {
        conditions.push(condition(
            COND_QUOTA_EXCEEDED,
            "NamespaceQuotaExhausted",
            format!(
                "declared stream(s) {} were NOT created: the {} account has {} bytes of file \
                 quota and {} are already promised. Raise needs.jetstream.size on a claim in \
                 this namespace, or lower the stream's maxBytes.",
                quota.blocked.join(", "),
                me.account(),
                quota.budget_bytes,
                quota.reserved_bytes
            ),
            now,
            prior,
        ));
    }
    let pre_captured = prefix_pre_captured(me, peers);
    if !pre_captured.is_empty() {
        conditions.push(condition(
            COND_PREFIX_PRE_CAPTURED,
            "PrefixDeclaredElsewhere",
            format!(
                "another application in this namespace declares stream(s) collecting subjects \
                 under {:?}: {}. This is a gated fan-in declaration, not a fault — but this \
                 application's traffic is being collected by a stream it does not own.",
                me.subject_prefix(),
                pre_captured.join(", ")
            ),
            now,
            prior,
        ));
    }
    let drain = namespace_drain_risk(me, peers);
    if !drain.is_empty() {
        conditions.push(condition(
            COND_NAMESPACE_DRAIN_RISK,
            "DynamicStreamsInNamespace",
            format!(
                "workqueue stream(s) {} share an account with an application allowed to create \
                 streams at will. A stream built with `sources` against a workqueue origin \
                 DRAINS it — the copy runs in the account's context, so no permission refuses \
                 it. Isolation here is the approval on dynamicStreams, not a rule the server \
                 enforces.",
                drain.join(", ")
            ),
            now,
            prior,
        ));
    }
    let overlap = workqueue_subject_overlap(observed, me, peers);
    if !overlap.is_empty() {
        conditions.push(condition(
            COND_WORKQUEUE_SUBJECT_OVERLAP,
            "SubjectsOverlapWorkqueue",
            format!(
                "a workqueue stream shares subjects with another stream in this account: {}. A \
                 workqueue deletes a message once acknowledged, so whichever stream takes \
                 delivery decides what the other never sees.",
                overlap.join("; ")
            ),
            now,
            prior,
        ));
    }
    let missing = consume_target_missing(observed, me, peers);
    if !missing.is_empty() {
        conditions.push(condition(
            COND_CONSUME_TARGET_MISSING,
            "NoSuchStream",
            format!(
                "consume entries name stream(s) that no application in this namespace declares \
                 and that do not exist: {}. The durable cannot be created until the owning \
                 application declares the stream.",
                missing.join(", ")
            ),
            now,
            prior,
        ));
    }
    let conflict = stream_name_conflict(observed, me);
    if !conflict.is_empty() {
        conditions.push(condition(
            COND_STREAM_NAME_CONFLICT,
            "NameTakenInAccount",
            format!(
                "declared stream name(s) are already taken in this account by a stream that is \
                 not this declaration: {}. The composed name is what the account's permission \
                 rules key on, so those rules currently apply to the other stream's data.",
                conflict.join("; ")
            ),
            now,
            prior,
        ));
    }

    JetStreamSignals {
        inventory,
        captures,
        quota,
        held_bytes,
        conditions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
    use operator_core::{
        JetStreamConsume, JetStreamStream, ResourceClaimJetStream, ResourceClaimSpec,
    };

    fn app_owner(app: &str) -> OwnerReference {
        OwnerReference {
            api_version: "apprafter.io/v1alpha1".into(),
            kind: "Application".into(),
            name: app.into(),
            uid: format!("{app}-uid"),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }
    }

    /// A jetstream `ResourceClaim` declared by `app` in `namespace`,
    /// carrying `js` as its sub-block. Mirrors exactly what
    /// `generate_resource_claims` produces: name `<app>-jetstream`, one
    /// ownerReference (`kind: Application`, `controller: true`).
    fn jetstream_claim(namespace: &str, app: &str, js: ResourceClaimJetStream) -> ResourceClaim {
        ResourceClaim {
            metadata: ObjectMeta {
                name: Some(format!("{app}-jetstream")),
                namespace: Some(namespace.to_string()),
                owner_references: Some(vec![app_owner(app)]),
                ..Default::default()
            },
            spec: ResourceClaimSpec {
                type_: "jetstream".into(),
                name: None,
                selector: BTreeMap::from([("tier".to_string(), "integrated".to_string())]),
                size: None,
                persistent: None,
                jetstream: Some(js),
            },
            status: None,
        }
    }

    fn size_map() -> BTreeMap<String, u64> {
        BTreeMap::from([
            ("nano".to_string(), 64 * (1 << 20)),
            ("small".to_string(), 256 * (1 << 20)),
            ("medium".to_string(), 1 << 30),
            ("large".to_string(), 2 << 30),
            ("xlarge".to_string(), 4 << 30),
        ])
    }

    #[test]
    fn own_stream_consume_resolves_owner_to_the_declaring_application() {
        // `consume: [{stream: "x", durable: "d"}]` with `from` OMITTED,
        // declared by app `indexer`, must yield ConsumeView{owner:
        // "indexer", …} — even with a SECOND, DIFFERENT app ("feeder")
        // present in the same namespace. A single-app fixture would
        // pass whether or not the resolution is correct (this branch's
        // own precedent, generalised): the only way to prove `owner`
        // comes from the DECLARING claim, not some other claim or
        // context in the batch, is to make a wrong answer land on a
        // REAL, different, present app.
        let claims = vec![
            jetstream_claim("demo", "feeder", ResourceClaimJetStream::default()),
            jetstream_claim(
                "demo",
                "indexer",
                ResourceClaimJetStream {
                    dynamic_streams: false,
                    streams: vec![],
                    consume: vec![JetStreamConsume {
                        from: None,
                        stream: "x".into(),
                        durable: "d".into(),
                    }],
                },
            ),
        ];
        let views = claim_views(&claims, &size_map());
        let indexer = views
            .iter()
            .find(|v| v.app == "indexer")
            .expect("indexer view present");
        assert_eq!(indexer.consumes.len(), 1);
        assert_eq!(indexer.consumes[0].owner, "indexer");
        assert_eq!(indexer.consumes[0].stream, "x");
        assert_eq!(indexer.consumes[0].durable, "d");
    }

    #[test]
    fn foreign_stream_consume_keeps_the_named_owner() {
        // The other half of L1: when `from` IS given, it must survive
        // unchanged — not be overwritten by the declaring app.
        let claims = vec![jetstream_claim(
            "demo",
            "indexer",
            ResourceClaimJetStream {
                dynamic_streams: false,
                streams: vec![],
                consume: vec![JetStreamConsume {
                    from: Some("feeder".into()),
                    stream: "blocks-head".into(),
                    durable: "idx".into(),
                }],
            },
        )];
        let views = claim_views(&claims, &size_map());
        assert_eq!(views[0].consumes[0].owner, "feeder");
    }

    #[test]
    fn every_claim_is_present_in_the_output_including_its_own() {
        // L2: `deny_vector`'s class (B) only reaches an app's own
        // declared streams because `render_account` is handed `peers`
        // INCLUDING the app being rendered. This function's own
        // contract is what makes that possible one layer up: every
        // claim handed in must appear in the output, never filtered —
        // a caller building a peers-minus-self slice loses class (B)
        // with nothing failing, and that caller can only build a
        // correct peer set if THIS function never drops a claim first.
        let claims = vec![
            jetstream_claim("demo", "feeder", ResourceClaimJetStream::default()),
            jetstream_claim("demo", "indexer", ResourceClaimJetStream::default()),
        ];
        let views = claim_views(&claims, &size_map());
        assert_eq!(views.len(), 2, "{views:?}");
        assert!(views.iter().any(|v| v.app == "feeder"));
        assert!(views.iter().any(|v| v.app == "indexer"));
    }

    #[test]
    fn quota_bytes_is_the_unclamped_per_claim_size_never_reduced_by_the_namespace_sum() {
        // L3 (Task 8a's property, asserted at THIS layer too): the
        // ceiling clamps the NAMESPACE SUM, not each claim, and this is
        // the layer that PRODUCES each claim's quota_bytes. Two "large"
        // claims in the SAME namespace must each carry the FULL
        // unclamped size — not size/2, not a value already capped at
        // some ceiling this function was never even given (it has no
        // ceiling parameter at all; only `render_accounts_file` does).
        let mut sizes = BTreeMap::new();
        sizes.insert("large".to_string(), 2u64 << 30);
        let mut a = jetstream_claim("demo", "a", ResourceClaimJetStream::default());
        a.spec.size = Some("large".into());
        let mut b = jetstream_claim("demo", "b", ResourceClaimJetStream::default());
        b.spec.size = Some("large".into());
        let views = claim_views(&[a, b], &sizes);
        assert_eq!(views.len(), 2);
        for v in &views {
            assert_eq!(v.quota_bytes, 2u64 << 30, "{v:?}");
        }
    }

    #[test]
    fn size_defaults_when_absent() {
        let claim = jetstream_claim("demo", "feeder", ResourceClaimJetStream::default());
        assert!(claim.spec.size.is_none());
        let views = claim_views(&[claim], &size_map());
        assert_eq!(
            views[0].quota_bytes,
            256 * (1 << 20),
            "must use DEFAULT_SIZE"
        );
    }

    #[test]
    fn an_unrecognised_size_floors_to_zero_rather_than_a_wrong_size() {
        let mut claim = jetstream_claim("demo", "feeder", ResourceClaimJetStream::default());
        claim.spec.size = Some("unknown-size".into());
        let views = claim_views(&[claim], &size_map());
        assert_eq!(views[0].quota_bytes, 0);
    }

    #[test]
    fn non_jetstream_claims_are_skipped() {
        let mut pg = jetstream_claim("demo", "feeder", ResourceClaimJetStream::default());
        pg.spec.type_ = "pg".into();
        pg.spec.jetstream = None;
        let views = claim_views(&[pg], &size_map());
        assert!(views.is_empty(), "{views:?}");
    }

    #[test]
    fn a_jetstream_typed_claim_without_the_sub_block_is_skipped() {
        // Defensive: `spec.type == "jetstream"` alone is not enough —
        // `spec.jetstream` could in principle be absent (a stale claim
        // predating the prerequisite, or a hand-crafted test fixture).
        let mut claim = jetstream_claim("demo", "feeder", ResourceClaimJetStream::default());
        claim.spec.jetstream = None;
        let views = claim_views(&[claim], &size_map());
        assert!(views.is_empty(), "{views:?}");
    }

    #[test]
    fn a_claim_without_an_application_owner_is_skipped() {
        let mut claim = jetstream_claim("demo", "feeder", ResourceClaimJetStream::default());
        claim.metadata.owner_references = None;
        let views = claim_views(&[claim], &size_map());
        assert!(views.is_empty(), "{views:?}");
    }

    #[test]
    fn streams_and_dynamic_streams_carry_through_unchanged() {
        let js = ResourceClaimJetStream {
            dynamic_streams: true,
            streams: vec![JetStreamStream {
                name: "orders".into(),
                subjects: vec!["shop.orders.>".into(), "billing.orders.>".into()],
                storage: Some("file".into()),
                retention: Some("workqueue".into()),
                max_age: Some("24h".into()),
                max_bytes: "1Gi".into(),
                allow_purge: true,
            }],
            consume: vec![],
        };
        let claim = jetstream_claim("demo", "feeder", js);
        let views = claim_views(&[claim], &size_map());
        assert_eq!(views.len(), 1);
        let v = &views[0];
        assert!(v.dynamic_streams);
        assert_eq!(v.streams.len(), 1);
        assert_eq!(v.streams[0].name, "orders");
        assert_eq!(
            v.streams[0].subjects,
            vec!["shop.orders.>".to_string(), "billing.orders.>".to_string()]
        );
        assert!(v.streams[0].allow_purge);
    }

    // --- mgr_secret_name (2.5d Task 10) -------------------------------

    #[test]
    fn mgr_secret_name_is_dns1123_and_injective_by_verbatim_namespace() {
        // Unlike `account_name`'s `_`-fold (which needs the injectivity
        // argument on its own doc), this just PREPENDS a fixed prefix to
        // the namespace verbatim — a Kubernetes namespace name is already
        // DNS-1123, and two different namespaces can never produce the
        // same secret name because the namespace itself is never
        // transformed, only prefixed.
        assert_eq!(mgr_secret_name("demo"), "nats-mgr-demo");
        assert_eq!(mgr_secret_name("demo-ns"), "nats-mgr-demo-ns");
        assert_ne!(mgr_secret_name("a"), mgr_secret_name("b"));
    }

    // --- size_bytes_map (2.5d Task 10 — reads jetstream-integrated's
    // config.sizeBytes) ---------------------------------------------

    #[test]
    fn size_bytes_map_reads_the_seed_shape() {
        // Mirrors service_providers.cue's own literal `sizeBytes` block
        // (jetstream-integrated) — nano/small/medium/large/xlarge, each a
        // bare JSON number of bytes.
        let cfg = serde_json::json!({
            "namespace": "nats-system",
            "sizeBytes": {
                "nano": 67108864u64,
                "small": 268435456u64,
                "medium": 1073741824u64,
                "large": 2147483648u64,
                "xlarge": 4294967296u64,
            },
            "ceilingBytes": 4294967296u64,
        });
        let map = size_bytes_map(&cfg);
        assert_eq!(map.get("nano"), Some(&67108864));
        assert_eq!(map.get("small"), Some(&268435456));
        assert_eq!(map.get("xlarge"), Some(&4294967296));
        assert_eq!(map.len(), 5, "{map:?}");
    }

    #[test]
    fn size_bytes_map_is_empty_when_the_config_key_is_absent() {
        // A provider config that omits `sizeBytes` entirely (a malformed
        // seed, or a future non-integrated provider that doesn't offer
        // sizes) must not panic — every claim then floors to
        // `quota_bytes: 0` via `claim_view`'s own "unrecognised size"
        // path, a visibly-wrong zero rather than a fabricated map entry.
        let map = size_bytes_map(&serde_json::json!({}));
        assert!(map.is_empty(), "{map:?}");
    }

    #[test]
    fn size_bytes_map_skips_a_non_numeric_entry_rather_than_panicking() {
        // Defensive: a hand-edited or future-drifted config could carry a
        // string or object where a number belongs. Skipping (not
        // panicking, not defaulting to 0 silently as a MAP entry — an
        // absent map entry already floors to 0 via claim_view) keeps this
        // function total.
        let cfg = serde_json::json!({"sizeBytes": {"small": "not-a-number", "large": 5u64}});
        let map = size_bytes_map(&cfg);
        assert_eq!(map.len(), 1, "{map:?}");
        assert_eq!(map.get("large"), Some(&5));
    }

    // --- connection_secret_object (2.5d Task 9, ADR 0061 §6) ----------

    fn sample_view() -> ClaimView {
        ClaimView {
            namespace: "demo".into(),
            app: "feeder".into(),
            dynamic_streams: false,
            streams: vec![],
            consumes: vec![],
            quota_bytes: 1 << 30,
        }
    }

    #[test]
    fn connection_secret_key_set_matches_the_webhooks_jetstream_fields() {
        // Round-1 review's standing objection: no fourth hand-copy of the
        // eight-field vocabulary. Assert the SET (order-independent) of
        // `stringData` keys against `admission_webhook::validator::JETSTREAM_FIELDS`
        // — a `[dev-dependencies]`-only cross-crate reference (see this
        // crate's Cargo.toml), not a literal list re-typed here.
        let cv = sample_view();
        let s = connection_secret_object(
            "feeder-jetstream-conn",
            "demo",
            &cv,
            "pw",
            "nats.nats-system.svc",
            4222,
            "uid-1",
            "feeder-jetstream",
        );
        let got: std::collections::BTreeSet<&str> = s["stringData"]
            .as_object()
            .expect("stringData is an object")
            .keys()
            .map(String::as_str)
            .collect();
        let want: std::collections::BTreeSet<&str> = admission_webhook::validator::JETSTREAM_FIELDS
            .iter()
            .copied()
            .collect();
        assert_eq!(
            got, want,
            "connection Secret key set must match JETSTREAM_FIELDS exactly"
        );
    }

    #[test]
    fn connection_secret_carries_the_expected_values() {
        let cv = sample_view();
        let s = connection_secret_object(
            "feeder-jetstream-conn",
            "demo",
            &cv,
            "s3cr3t",
            "nats.nats-system.svc",
            4222,
            "uid-1",
            "feeder-jetstream",
        );
        assert_eq!(s["apiVersion"], "v1");
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "feeder-jetstream-conn");
        assert_eq!(s["metadata"]["namespace"], "demo");
        assert_eq!(s["type"], "Opaque");
        let sd = &s["stringData"];
        assert_eq!(sd["host"], "nats.nats-system.svc");
        assert_eq!(sd["port"], "4222");
        assert_eq!(sd["user"], cv.user());
        assert_eq!(sd["pass"], "s3cr3t");
        assert_eq!(sd["account"], cv.account());
        assert_eq!(sd["subjectPrefix"], cv.subject_prefix());
        assert_eq!(sd["inboxPrefix"], cv.inbox_prefix());
        assert_eq!(
            sd["url"],
            format!("nats://{}:s3cr3t@nats.nats-system.svc:4222", cv.user())
        );
    }

    // --- accounts_secret_object / mgr_secret_object (2.5d Task 10) ----

    #[test]
    fn accounts_secret_object_carries_the_one_key_the_chart_includes() {
        let s = accounts_secret_object("nats-accounts", "nats-system", "ns_demo: {}\n");
        assert_eq!(s["apiVersion"], "v1");
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "nats-accounts");
        assert_eq!(s["metadata"]["namespace"], "nats-system");
        assert_eq!(s["type"], "Opaque");
        assert_eq!(s["stringData"]["accounts.conf"], "ns_demo: {}\n");
        // Platform-scoped, not claim-scoped — never owner-ref'd to any one
        // ResourceClaim (deleting one claim must not cascade-delete the
        // WHOLE accounts file every other namespace's users depend on).
        assert!(s["metadata"].get("ownerReferences").is_none());
    }

    #[test]
    fn mgr_secret_object_carries_the_user_and_password_keys() {
        // 2.5e Task 2: NACK's own Account CRD reads the username from a
        // SECRET KEY (spec.user.user names the KEY, not the literal
        // value) — so this Secret must carry the username string itself,
        // not just the password, or the Account CR has nothing to point
        // `user.user` at.
        let s = mgr_secret_object("nats-mgr-demo", "nats-system", "mgr_demo", "s3cr3t");
        assert_eq!(s["metadata"]["name"], "nats-mgr-demo");
        assert_eq!(s["metadata"]["namespace"], "nats-system");
        assert_eq!(s["stringData"]["user"], "mgr_demo");
        assert_eq!(s["stringData"]["password"], "s3cr3t");
        assert!(s["metadata"].get("ownerReferences").is_none());
    }

    // --- account_k8s_name / account_object (2.5e Task 2, NACK Account CR) ---

    #[test]
    fn account_k8s_name_is_dns1123_and_distinct_from_the_nats_side_name() {
        // `ns-<namespace>` (hyphen, DNS-1123) — deliberately NOT the same
        // string as `nats_accounts::account_name`'s `ns_<namespace>`
        // (underscore, NATS-side): the two live in different worlds (a
        // Kubernetes object name vs. a value inside a config file) and
        // conflating them would be a coincidence, not a design.
        assert_eq!(account_k8s_name("demo"), "ns-demo");
        assert_eq!(account_k8s_name("demo-ns"), "ns-demo-ns");
    }

    #[test]
    fn account_object_carries_the_nats_side_name_and_servers_and_user_ref() {
        let a = account_object(
            "demo",
            "nats-system",
            "nats://nats.nats-system.svc:4222",
            "nats-mgr-demo",
        );
        assert_eq!(a["apiVersion"], "jetstream.nats.io/v1beta2");
        assert_eq!(a["kind"], "Account");
        assert_eq!(a["metadata"]["name"], "ns-demo");
        assert_eq!(a["metadata"]["namespace"], "nats-system");
        // spec.name is the NATS-side account (nats_accounts::account_name),
        // NOT the Kubernetes object name — verified against the real NACK
        // CRD/controller (jetstream.nats.io v1beta2, nack v0.35.0): this is
        // what render_accounts_file's own `ns_demo: { ... }` key IS.
        assert_eq!(a["spec"]["name"], "ns_demo");
        assert_eq!(
            a["spec"]["servers"],
            serde_json::json!(["nats://nats.nats-system.svc:4222"])
        );
        // `user.user`/`user.password` NAME KEYS inside the referenced
        // Secret (verified against the CRD schema + controller.go's own
        // getAccountOverrides, which reads secret.Data[user.User] /
        // secret.Data[user.Password]) — they are not the literal
        // username/password strings themselves.
        assert_eq!(a["spec"]["user"]["secret"]["name"], "nats-mgr-demo");
        assert_eq!(a["spec"]["user"]["user"], "user");
        assert_eq!(a["spec"]["user"]["password"], "password");
    }

    #[test]
    fn connection_secret_is_owner_refd_to_the_resourceclaim_for_cascade_delete() {
        // The pattern at reconcile.rs's redis/cnpg connection-secret
        // builders: a controller ownerReference so the Secret cascades on
        // claim delete, never orphaned.
        let cv = sample_view();
        let s = connection_secret_object(
            "feeder-jetstream-conn",
            "demo",
            &cv,
            "pw",
            "nats.nats-system.svc",
            4222,
            "uid-xyz",
            "feeder-jetstream",
        );
        let owner = &s["metadata"]["ownerReferences"][0];
        assert_eq!(owner["apiVersion"], "apprafter.io/v1alpha1");
        assert_eq!(owner["kind"], "ResourceClaim");
        assert_eq!(owner["name"], "feeder-jetstream");
        assert_eq!(owner["uid"], "uid-xyz");
        assert_eq!(owner["controller"], true);
        assert_eq!(owner["blockOwnerDeletion"], true);
    }

    // --- quantity_bytes (2.5e Task 3) ---------------------------------

    #[test]
    fn quantity_bytes_parses_binary_si_suffixes() {
        assert_eq!(quantity_bytes("1Gi"), Some(1073741824));
        assert_eq!(quantity_bytes("256Mi"), Some(268435456));
        assert_eq!(quantity_bytes("1Ki"), Some(1024));
    }

    #[test]
    fn quantity_bytes_parses_decimal_si_suffixes_and_bare_numbers() {
        assert_eq!(quantity_bytes("1G"), Some(1_000_000_000));
        assert_eq!(quantity_bytes("512"), Some(512));
    }

    #[test]
    fn quantity_bytes_rejects_malformed_input() {
        // The admission webhook (validator.rs's own is_k8s_quantity) has
        // already rejected a malformed maxBytes before a claim ever
        // reaches this code — this is defensive, not a second gate the
        // product relies on being reachable.
        assert_eq!(quantity_bytes(""), None);
        assert_eq!(quantity_bytes("not-a-quantity"), None);
        assert_eq!(quantity_bytes("1Xi"), None);
    }

    // --- stream_object / consumer_object (2.5e Task 3, NACK CRs) -----

    #[test]
    fn stream_object_carries_the_nats_side_name_and_declared_shape() {
        let s = stream_object(
            "demo",
            "nats-system",
            "streamapp",
            "orders",
            &["streamapp.orders.>".to_string()],
            "file",
            "limits",
            "",
            1_073_741_824,
            "ns-demo",
        );
        assert_eq!(s["apiVersion"], "jetstream.nats.io/v1beta2");
        assert_eq!(s["kind"], "Stream");
        // Object name: <ns>-<app>-<name> (K8s, DNS-1123).
        assert_eq!(s["metadata"]["name"], "demo-streamapp-orders");
        assert_eq!(s["metadata"]["namespace"], "nats-system");
        // spec.name: the NATS-side name (nats_accounts::nats_stream_name) —
        // the SAME derivation the deny-vector/allow-list already use, not
        // a second copy of the join formula.
        assert_eq!(s["spec"]["name"], "streamapp_orders");
        assert_eq!(
            s["spec"]["subjects"],
            serde_json::json!(["streamapp.orders.>"])
        );
        assert_eq!(s["spec"]["storage"], "file");
        assert_eq!(s["spec"]["retention"], "limits");
        assert_eq!(s["spec"]["maxBytes"], 1_073_741_824i64);
        // spec.account: the KUBERNETES OBJECT NAME of the Account CR in
        // the SAME namespace — verified against controller.go's
        // getAccountOverrides, which does `c.ji.Accounts(ns).Get(ctx,
        // account, ...)` (a K8s API GET by object name), NOT the
        // NATS-side account name (`Account.spec.name`). Getting this
        // backwards fails only inside a live reconcile, per this
        // round's own standing instruction to verify rather than
        // inherit a CRD field's meaning.
        assert_eq!(s["spec"]["account"], "ns-demo");
    }

    #[test]
    fn stream_object_omits_max_age_when_absent() {
        let s = stream_object(
            "demo",
            "nats-system",
            "streamapp",
            "orders",
            &["streamapp.orders.>".to_string()],
            "file",
            "limits",
            "",
            1_073_741_824,
            "ns-demo",
        );
        assert_eq!(
            s["spec"]["maxAge"], "",
            "empty maxAge matches the CRD's own default (\"\")"
        );
    }

    #[test]
    fn stream_object_carries_max_age_when_present() {
        let s = stream_object(
            "demo",
            "nats-system",
            "streamapp",
            "orders",
            &["streamapp.orders.>".to_string()],
            "file",
            "limits",
            "24h",
            1_073_741_824,
            "ns-demo",
        );
        assert_eq!(s["spec"]["maxAge"], "24h");
    }

    #[test]
    fn consumer_object_carries_the_nats_side_durable_and_stream_names() {
        let c = consumer_object(
            "demo",
            "nats-system",
            "consumerapp",
            "reader",
            "streamapp",
            "orders",
            "ns-demo",
        );
        assert_eq!(c["apiVersion"], "jetstream.nats.io/v1beta2");
        assert_eq!(c["kind"], "Consumer");
        // Object name: <ns>-<consumer app>-<durable>.
        assert_eq!(c["metadata"]["name"], "demo-consumerapp-reader");
        assert_eq!(c["metadata"]["namespace"], "nats-system");
        // durableName: nats_durable_name(consumer_app, declared) —
        // consumer-keyed, NOT owner-keyed (see nats_accounts's own doc on
        // why the asymmetry is the point).
        assert_eq!(c["spec"]["durableName"], "consumerapp_reader");
        // streamName: the NATS-SIDE stream name (nats_stream_name(owner,
        // stream)) — verified against jsmclient.go's LoadConsumer/
        // NewConsumer, which pass it straight to the JetStream manager
        // client, NOT to a Kubernetes API call. This is NOT the Stream
        // CR's own Kubernetes object name — a plausible-looking mistake
        // this test exists to pin against.
        assert_eq!(c["spec"]["streamName"], "streamapp_orders");
        assert_eq!(c["spec"]["account"], "ns-demo");
    }

    // =================================================================
    // 2.5f — inventory (Task 5), capture detector (Task 6), conditions
    // (Task 7). ADR 0061 §5/§6/§7.
    // =================================================================

    /// A claim view with no declarations. Every 2.5f fixture starts here
    /// and adds exactly the one thing under test, so a rule can never pass
    /// on a property some unrelated field happened to carry.
    fn cv(ns: &str, app: &str) -> ClaimView {
        ClaimView {
            namespace: ns.into(),
            app: app.into(),
            dynamic_streams: false,
            streams: vec![],
            consumes: vec![],
            quota_bytes: 1 << 30,
        }
    }

    /// A declared stream. `retention: None` is `limits` (the CRD default).
    fn sv(name: &str, subjects: &[&str], retention: Option<&str>, max_bytes: &str) -> StreamView {
        StreamView {
            name: name.into(),
            subjects: subjects.iter().map(|s| s.to_string()).collect(),
            allow_purge: false,
            retention: retention.map(str::to_string),
            max_bytes: max_bytes.into(),
        }
    }

    /// An observed stream with no size facts — `-1` is JetStream's
    /// "unlimited", which every quota reader must treat as reserving
    /// nothing.
    fn obs(name: &str, subjects: &[&str]) -> nats_client::StreamSummary {
        nats_client::StreamSummary {
            name: name.into(),
            subjects: subjects.iter().map(|s| s.to_string()).collect(),
            max_bytes: -1,
            bytes: 0,
        }
    }

    fn obs_sized(
        name: &str,
        subjects: &[&str],
        max_bytes: i64,
        bytes: u64,
    ) -> nats_client::StreamSummary {
        nats_client::StreamSummary {
            name: name.into(),
            subjects: subjects.iter().map(|s| s.to_string()).collect(),
            max_bytes,
            bytes,
        }
    }

    const NOW: &str = "2026-09-12T00:00:00+00:00";

    // --- Task 5: the status inventory (ADR 0061 §9) -------------------

    #[test]
    fn inventory_splits_declared_dynamic_and_unattributed() {
        let mut me = cv("demo", "feeder");
        me.streams = vec![sv("orders", &["feeder.orders.>"], None, "1Gi")];
        let observed = vec![
            obs("feeder_orders", &["feeder.orders.>"]), // declared by me
            obs("scratch", &["feeder.tmp.>"]),          // wholly mine, undeclared
            obs("mixed", &["feeder.x", "elsewhere.y"]), // mixed, undeclared
        ];
        let inv = stream_inventory(&observed, &me, std::slice::from_ref(&me), NOW);
        assert_eq!(inv.declared, vec!["feeder_orders".to_string()]);
        assert_eq!(inv.dynamic, vec!["scratch".to_string()]);
        assert_eq!(inv.unattributed, vec!["mixed".to_string()]);
        assert_eq!(inv.observed_at, NOW);
    }

    #[test]
    fn a_declared_stream_is_never_unattributed_even_when_a_neighbour_declared_it() {
        // THE rule this subphase's review found stated two different ways.
        // `collector` declares a FAN-IN stream (ADR 0061 §6) collecting
        // `feeder.`'s traffic alongside its own — a supported, gated shape.
        // Under the narrow reading ("declared by the claim being reported
        // on") it is unattributed to `feeder`, and the GC's own sweep
        // consults exactly that classification — so the narrow reading
        // ends with the platform reporting, and at §5's `delete` rung
        // DESTROYING, a live approved stream.
        let feeder = cv("demo", "feeder");
        let mut collector = cv("demo", "collector");
        collector.streams = vec![sv(
            "inbox",
            &["collector.in.>", "feeder.orders.>"],
            None,
            "1Gi",
        )];
        let peers = vec![feeder.clone(), collector.clone()];
        let observed = vec![obs(
            "collector_inbox",
            &["collector.in.>", "feeder.orders.>"],
        )];

        let feeder_inv = stream_inventory(&observed, &feeder, &peers, NOW);
        assert!(
            feeder_inv.unattributed.is_empty(),
            "a stream declared by ANY application in the namespace is attributed — \
             fan-in is a supported shape, not a capture: {feeder_inv:?}"
        );
        assert!(feeder_inv.declared.is_empty(), "{feeder_inv:?}");
        assert!(feeder_inv.dynamic.is_empty(), "{feeder_inv:?}");

        // ...and it is attributed to the application that DID declare it.
        let collector_inv = stream_inventory(&observed, &collector, &peers, NOW);
        assert_eq!(collector_inv.declared, vec!["collector_inbox".to_string()]);
    }

    #[test]
    fn a_subjectless_stream_is_unattributed_never_dynamic() {
        // A `sources`/`mirror` stream (ADR 0061 §4.1's read vector) carries
        // no subjects of its own. It cannot be attributed by subject at
        // all, and the whole rule keys on subjects — so it is reported,
        // never claimed.
        let me = cv("demo", "feeder");
        let observed = vec![obs("siphon", &[])];
        let inv = stream_inventory(&observed, &me, std::slice::from_ref(&me), NOW);
        assert!(inv.dynamic.is_empty(), "{inv:?}");
        assert_eq!(inv.unattributed, vec!["siphon".to_string()]);
    }

    #[test]
    fn a_neighbours_stream_that_never_touches_me_is_in_no_list_of_mine() {
        let me = cv("demo", "feeder");
        let mut other = cv("demo", "indexer");
        other.dynamic_streams = true;
        let peers = vec![me.clone(), other];
        let observed = vec![obs("indexer_scratch", &["indexer.tmp.>"])];
        let inv = stream_inventory(&observed, &me, &peers, NOW);
        assert!(
            inv.declared.is_empty() && inv.dynamic.is_empty() && inv.unattributed.is_empty(),
            "not this claim's business: {inv:?}"
        );
    }

    #[test]
    fn a_declared_stream_that_does_not_exist_yet_is_in_no_list() {
        // The inventory reports what was OBSERVED, never what was
        // declared. "Declared but absent" is the AwaitingStreamCreation
        // gate's job, and a claim whose stream NACK never created must not
        // read as if it had one.
        let mut me = cv("demo", "feeder");
        me.streams = vec![sv("orders", &["feeder.orders.>"], None, "1Gi")];
        let inv = stream_inventory(&[], &me, std::slice::from_ref(&me), NOW);
        assert!(inv.declared.is_empty(), "{inv:?}");
    }

    #[test]
    fn prefix_attribution_is_dot_delimited_so_similar_app_names_cannot_claim_each_other() {
        let feeder = cv("demo", "feeder");
        let feederbot = cv("demo", "feederbot");
        let peers = vec![feeder.clone(), feederbot.clone()];
        let observed = vec![obs("botstream", &["feederbot.orders.>"])];

        let feeder_inv = stream_inventory(&observed, &feeder, &peers, NOW);
        assert!(
            feeder_inv.dynamic.is_empty() && feeder_inv.unattributed.is_empty(),
            "`feeder.` must not prefix `feederbot.orders.>`: {feeder_inv:?}"
        );
        let bot_inv = stream_inventory(&observed, &feederbot, &peers, NOW);
        assert_eq!(bot_inv.dynamic, vec!["botstream".to_string()]);
    }

    #[test]
    fn held_bytes_counts_declared_and_dynamic_but_never_unattributed() {
        let mut me = cv("demo", "feeder");
        me.streams = vec![sv("orders", &["feeder.orders.>"], None, "1Gi")];
        let observed = vec![
            obs_sized("feeder_orders", &["feeder.orders.>"], -1, 100),
            obs_sized("scratch", &["feeder.tmp.>"], -1, 20),
            obs_sized("mixed", &["feeder.x", "elsewhere.y"], -1, 9_000),
        ];
        let inv = stream_inventory(&observed, &me, std::slice::from_ref(&me), NOW);
        assert_eq!(
            inventory_held_bytes(&observed, &inv),
            120,
            "unattributed bytes belong to nobody this code can name, so charging \
             them here would make one tenant's figure move when another's stream grew"
        );
    }

    // --- Task 6: the foreign-subject capture detector (ADR 0061 §5) ---

    #[test]
    fn capture_detection_table() {
        // `me` = feeder, NO dynamicStreams (so detection is exact for it —
        // ADR 0061 §3). `collector` declares a fan-in stream over feeder's
        // prefix; `roamer` holds dynamicStreams.
        let me = cv("demo", "feeder");
        let mut collector = cv("demo", "collector");
        collector.streams = vec![sv(
            "inbox",
            &["collector.in.>", "feeder.orders.>"],
            None,
            "1Gi",
        )];
        let mut roamer = cv("demo", "roamer");
        roamer.dynamic_streams = true;
        let peers = vec![me.clone(), collector, roamer];

        let cases: &[(&str, nats_client::StreamSummary, bool)] = &[
            (
                "a neighbour's DECLARED fan-in stream over my prefix — legitimate (ADR §6), \
                 and the case the narrow definition got wrong",
                obs("collector_inbox", &["collector.in.>", "feeder.orders.>"]),
                false,
            ),
            (
                "a dynamic stream of an application that HOLDS the flag, wholly under its \
                 own prefix — legitimate, and it does not touch me anyway",
                obs("roamer_scratch", &["roamer.tmp.>"]),
                false,
            ),
            (
                "a stream that never touches my prefix",
                obs("elsewhere", &["elsewhere.>"]),
                false,
            ),
            (
                "a subject-less stream — no evidence either way, and the `delete` rung acts \
                 on this list",
                obs("siphon", &[]),
                false,
            ),
            (
                "an UNDECLARED stream wholly under MY prefix, and I do not hold the flag — \
                 I provably could not have created it",
                obs("squatter", &["feeder.orders.stolen"]),
                true,
            ),
            (
                "a MIXED undeclared stream touching my prefix — no single application's \
                 prefix covers it",
                obs("mixed", &["feeder.x", "roamer.y"]),
                true,
            ),
        ];

        for (why, stream, expect_capture) in cases {
            let got = foreign_subject_captures(std::slice::from_ref(stream), &me, &peers);
            assert_eq!(
                !got.is_empty(),
                *expect_capture,
                "{why} — got {got:?} for {stream:?}"
            );
        }
    }

    #[test]
    fn my_own_dynamic_stream_is_not_a_capture_when_i_hold_the_flag() {
        let mut me = cv("demo", "feeder");
        me.dynamic_streams = true;
        let observed = vec![obs("scratch", &["feeder.tmp.>"])];
        assert!(
            foreign_subject_captures(&observed, &me, std::slice::from_ref(&me)).is_empty(),
            "an application allowed to create streams at will created this one"
        );
    }

    #[test]
    fn the_same_stream_is_a_capture_once_the_flag_is_off() {
        // The mutation half of the test above, and the property ADR 0061
        // §3 claims outright ("makes §5's capture detection exact for every
        // opted-out application"). Drop the `p.dynamic_streams &&` clause
        // from `foreign_subject_captures` and this is the assertion that
        // turns red — nothing else would.
        //
        // It is also ADR §4.1's residue case stated forwards: flipping the
        // flag `true` -> `false` does NOT undo an existing capture, and
        // §5's `delete` step is what handles the leftovers.
        let me = cv("demo", "feeder");
        let observed = vec![obs("scratch", &["feeder.tmp.>"])];
        assert_eq!(
            foreign_subject_captures(&observed, &me, std::slice::from_ref(&me)),
            vec!["scratch".to_string()]
        );
    }

    #[test]
    fn capture_policy_defaults_to_report_and_never_promises_quarantine() {
        assert_eq!(
            capture_policy(&serde_json::json!({})).0,
            CapturePolicy::Report
        );
        assert_eq!(
            capture_policy(&serde_json::json!({"capturePolicy": "report"})).0,
            CapturePolicy::Report
        );
        assert_eq!(
            capture_policy(&serde_json::json!({"capturePolicy": "delete"})).0,
            CapturePolicy::Delete
        );
        // `quarantine` is in ADR 0061 §5's ladder but needs attribution
        // (verification item 6) — so it is not a variant, and asking for
        // it falls back to `report` LOUDLY rather than silently doing
        // nothing under a reassuring name.
        let (policy, ignored) = capture_policy(&serde_json::json!({"capturePolicy": "quarantine"}));
        assert_eq!(policy, CapturePolicy::Report);
        assert_eq!(ignored.as_deref(), Some("quarantine"));
    }

    // --- Task 7: QuotaExceeded (ADR 0061 §6) --------------------------

    #[test]
    fn quota_blocks_a_declaration_that_does_not_fit_the_namespace_account() {
        let mut me = cv("demo", "feeder");
        me.quota_bytes = 256 * (1 << 20); // budget: one small claim
        me.streams = vec![sv("big", &["feeder.big.>"], None, "1Gi")];
        let v = quota_verdict(&[], &me, std::slice::from_ref(&me), u64::MAX);
        assert_eq!(v.budget_bytes, 256 * (1 << 20));
        assert_eq!(v.blocked, vec!["big".to_string()]);
    }

    #[test]
    fn quota_does_not_fire_when_the_declarations_fit() {
        let mut me = cv("demo", "feeder");
        me.quota_bytes = 1 << 30;
        me.streams = vec![sv("ok", &["feeder.ok.>"], None, "256Mi")];
        let v = quota_verdict(&[], &me, std::slice::from_ref(&me), u64::MAX);
        assert!(v.blocked.is_empty(), "{v:?}");
        assert_eq!(v.reserved_bytes, 256 * (1 << 20));
    }

    #[test]
    fn quota_counts_a_neighbours_declarations_against_my_headroom() {
        // The ceiling clamps the namespace SUM (ADR 0061 §6), so a
        // neighbour's declaration is not somebody else's problem.
        let mut me = cv("demo", "feeder");
        me.quota_bytes = 256 * (1 << 20);
        me.streams = vec![sv("mine", &["feeder.x.>"], None, "256Mi")];
        let mut hog = cv("demo", "hog");
        hog.quota_bytes = 0;
        hog.streams = vec![sv("huge", &["hog.x.>"], None, "256Mi")];
        let peers = vec![me.clone(), hog];
        let v = quota_verdict(&[], &me, &peers, u64::MAX);
        assert_eq!(v.budget_bytes, 256 * (1 << 20));
        assert_eq!(v.blocked, vec!["mine".to_string()], "{v:?}");
    }

    #[test]
    fn quota_never_blocks_a_stream_that_already_exists() {
        // Monotonicity, and it is what keeps this safe to run on a live
        // claim: an existing stream's bytes are already reserved inside
        // NATS, so refusing it changes nothing there and would only flip a
        // healthy application unready because a NEIGHBOUR edited its
        // manifest.
        let mut me = cv("demo", "feeder");
        me.quota_bytes = 1; // budget of one byte: nothing new could fit
        me.streams = vec![sv("live", &["feeder.live.>"], None, "1Gi")];
        let observed = vec![obs_sized("feeder_live", &["feeder.live.>"], 1 << 30, 5)];
        let v = quota_verdict(&observed, &me, std::slice::from_ref(&me), u64::MAX);
        assert!(
            v.blocked.is_empty(),
            "an existing stream is a fait accompli: {v:?}"
        );
    }

    #[test]
    fn quota_counts_an_undeclared_streams_reservation_but_not_an_unlimited_one() {
        let mut me = cv("demo", "feeder");
        me.quota_bytes = 300 * (1 << 20);
        me.streams = vec![sv("mine", &["feeder.x.>"], None, "256Mi")];

        // An UNLIMITED dynamic stream reserves nothing the account can
        // promise — `-1` must never be subtracted.
        let unlimited = vec![obs_sized("scratch", &["feeder.tmp.>"], -1, 0)];
        assert!(
            quota_verdict(&unlimited, &me, std::slice::from_ref(&me), u64::MAX)
                .blocked
                .is_empty()
        );

        // A SIZED one does.
        let sized = vec![obs_sized("scratch", &["feeder.tmp.>"], 256 << 20, 0)];
        assert_eq!(
            quota_verdict(&sized, &me, std::slice::from_ref(&me), u64::MAX).blocked,
            vec!["mine".to_string()]
        );
    }

    #[test]
    fn quota_budget_is_the_clamped_namespace_sum_the_accounts_file_will_write() {
        let mut a = cv("demo", "a");
        a.quota_bytes = 2 << 30;
        let mut b = cv("demo", "b");
        b.quota_bytes = 2 << 30;
        let peers = vec![a.clone(), b];
        let v = quota_verdict(&[], &a, &peers, 3 << 30);
        assert_eq!(
            v.budget_bytes,
            3 << 30,
            "the tier ceiling clamps the SUM, not each term — and the pre-flight has to \
             check against the same number render_accounts_file writes into max_file"
        );
    }

    // --- Task 7: PrefixPreCaptured (ADR 0061 §7 trigger #16) ----------

    #[test]
    fn prefix_pre_captured_fires_when_a_neighbour_already_collects_my_prefix() {
        let me = cv("demo", "newapp");
        let mut incumbent = cv("demo", "incumbent");
        incumbent.streams = vec![sv("inbox", &["newapp.orders.>"], None, "1Gi")];
        let peers = vec![me.clone(), incumbent];
        assert_eq!(
            prefix_pre_captured(&me, &peers),
            vec!["incumbent_inbox".to_string()]
        );
    }

    #[test]
    fn prefix_pre_captured_does_not_fire_on_my_own_declarations_or_a_neighbours_own_prefix() {
        let mut me = cv("demo", "newapp");
        me.streams = vec![sv("mine", &["newapp.orders.>"], None, "1Gi")];
        let mut neighbour = cv("demo", "incumbent");
        neighbour.streams = vec![sv("theirs", &["incumbent.orders.>"], None, "1Gi")];
        let peers = vec![me.clone(), neighbour];
        assert!(
            prefix_pre_captured(&me, &peers).is_empty(),
            "my own declaration over my own prefix is what a prefix is FOR"
        );
    }

    // --- Task 7: NamespaceDrainRisk (ADR 0061 §4.1) -------------------

    #[test]
    fn drain_risk_fires_on_the_workqueue_owner_and_on_the_flag_holder() {
        let mut owner = cv("demo", "queues");
        owner.streams = vec![sv("jobs", &["queues.jobs.>"], Some("workqueue"), "1Gi")];
        let mut roamer = cv("demo", "roamer");
        roamer.dynamic_streams = true;
        let peers = vec![owner.clone(), roamer.clone()];

        let on_owner = namespace_drain_risk(&owner, &peers);
        assert_eq!(on_owner.len(), 1, "{on_owner:?}");
        assert!(on_owner[0].contains("queues_jobs"), "{on_owner:?}");
        assert!(on_owner[0].contains("roamer"), "{on_owner:?}");

        // "on both parties" — the flag holder is told it IS the risk.
        let on_holder = namespace_drain_risk(&roamer, &peers);
        assert_eq!(on_holder.len(), 1, "{on_holder:?}");
    }

    #[test]
    fn drain_risk_does_not_fire_when_the_workqueues_own_owner_is_the_only_flag_holder() {
        // The narrowing. An application that declares a workqueue AND
        // creates streams at will is not a risk to itself, and firing
        // there would make the condition permanent background noise on a
        // completely ordinary shape.
        let mut solo = cv("demo", "queues");
        solo.dynamic_streams = true;
        solo.streams = vec![sv("jobs", &["queues.jobs.>"], Some("workqueue"), "1Gi")];
        let peers = vec![solo.clone()];
        assert!(namespace_drain_risk(&solo, &peers).is_empty());
    }

    #[test]
    fn drain_risk_does_not_fire_without_a_workqueue_or_without_a_flag_holder() {
        // Two separate "must not fire" halves, because each could be
        // broken alone.
        let mut owner = cv("demo", "queues");
        owner.streams = vec![sv("jobs", &["queues.jobs.>"], Some("workqueue"), "1Gi")];
        let quiet = cv("demo", "quiet"); // no dynamicStreams
        let peers = vec![owner.clone(), quiet];
        assert!(
            namespace_drain_risk(&owner, &peers).is_empty(),
            "no application in this namespace can create a sourcing stream"
        );

        let mut limits_owner = cv("demo", "queues");
        limits_owner.streams = vec![sv("jobs", &["queues.jobs.>"], None, "1Gi")];
        let mut roamer = cv("demo", "roamer");
        roamer.dynamic_streams = true;
        let peers2 = vec![limits_owner.clone(), roamer];
        assert!(
            namespace_drain_risk(&limits_owner, &peers2).is_empty(),
            "the drain is destructive only against a WORKQUEUE origin (ADR 0061 §4.1) — \
             `retention: None` is `limits`"
        );
    }

    // --- Task 7: WorkqueueSubjectOverlap ------------------------------

    #[test]
    fn workqueue_overlap_fires_declared_vs_declared() {
        // The direction a detector built only against dynamic streams
        // would never mention — and the only one a manifest review can
        // still prevent.
        let mut owner = cv("demo", "queues");
        owner.streams = vec![sv("jobs", &["queues.jobs.>"], Some("workqueue"), "1Gi")];
        let mut other = cv("demo", "audit");
        other.streams = vec![sv("tap", &["queues.jobs.high"], None, "1Gi")];
        let peers = vec![owner.clone(), other.clone()];

        let on_owner = workqueue_subject_overlap(&[], &owner, &peers);
        assert_eq!(on_owner.len(), 1, "{on_owner:?}");
        assert!(on_owner[0].contains("audit_tap"), "{on_owner:?}");
        // Both parties own a side, so both are told.
        assert_eq!(workqueue_subject_overlap(&[], &other, &peers).len(), 1);
    }

    #[test]
    fn workqueue_overlap_fires_declared_vs_dynamic() {
        let mut owner = cv("demo", "queues");
        owner.streams = vec![sv("jobs", &["queues.jobs.>"], Some("workqueue"), "1Gi")];
        let peers = vec![owner.clone()];
        let observed = vec![obs("siphon", &["queues.jobs.high"])];
        let got = workqueue_subject_overlap(&observed, &owner, &peers);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("siphon"), "{got:?}");
    }

    #[test]
    fn workqueue_overlap_does_not_fire_on_disjoint_subjects_or_a_non_workqueue() {
        let mut owner = cv("demo", "queues");
        owner.streams = vec![sv("jobs", &["queues.jobs.>"], Some("workqueue"), "1Gi")];
        let mut other = cv("demo", "audit");
        other.streams = vec![sv("own", &["audit.log.>"], None, "1Gi")];
        let peers = vec![owner.clone(), other];
        assert!(
            workqueue_subject_overlap(&[obs("x", &["elsewhere.>"])], &owner, &peers).is_empty()
        );

        // Same subjects, but the collector is `limits`, not `workqueue` —
        // nothing is consumed away, so there is nothing to report.
        let mut limits_owner = cv("demo", "queues");
        limits_owner.streams = vec![sv("jobs", &["queues.jobs.>"], None, "1Gi")];
        let mut tapper = cv("demo", "audit");
        tapper.streams = vec![sv("tap", &["queues.jobs.high"], None, "1Gi")];
        let peers2 = vec![limits_owner.clone(), tapper];
        assert!(workqueue_subject_overlap(&[], &limits_owner, &peers2).is_empty());
    }

    #[test]
    fn subject_overlap_is_whole_token_and_a_trailing_wildcard_needs_a_token() {
        for (a, b, expect) in [
            ("a.b", "a.b", true),
            ("a.>", "a.b.c", true),
            ("a.>", "a", false), // `>` matches ONE OR MORE tokens
            ("a", "a.>", false),
            ("a.*.c", "a.b.c", true),
            ("a.*", "a.b.c", false),
            ("a.b", "a.c", false),
            ("ab.c", "a.c", false),
            (">", "anything.at.all", true),
        ] {
            assert_eq!(subjects_overlap(a, b), expect, "{a:?} vs {b:?}");
            assert_eq!(subjects_overlap(b, a), expect, "{b:?} vs {a:?} (symmetric)");
        }
    }

    // --- Task 7: ConsumeTargetMissing ---------------------------------

    #[test]
    fn consume_target_missing_fires_when_nobody_declares_the_stream() {
        let mut me = cv("demo", "indexer");
        me.consumes = vec![ConsumeView {
            owner: "feeder".into(),
            stream: "blocks-head".into(),
            durable: "idx".into(),
        }];
        let feeder = cv("demo", "feeder"); // present, declares nothing
        let peers = vec![me.clone(), feeder];
        let got = consume_target_missing(&[], &me, &peers);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("feeder_blocks-head"), "{got:?}");
    }

    #[test]
    fn consume_target_missing_does_not_fire_when_declared_or_when_the_stream_exists() {
        let mut me = cv("demo", "indexer");
        me.consumes = vec![ConsumeView {
            owner: "feeder".into(),
            stream: "blocks-head".into(),
            durable: "idx".into(),
        }];

        let mut feeder = cv("demo", "feeder");
        feeder.streams = vec![sv("blocks-head", &["feeder.blocks.>"], None, "1Gi")];
        let declared_peers = vec![me.clone(), feeder];
        assert!(consume_target_missing(&[], &me, &declared_peers).is_empty());

        // An owner holding `dynamicStreams` may create it itself; a
        // working consumer must not be reported broken forever.
        let mut roamer = cv("demo", "feeder");
        roamer.dynamic_streams = true;
        let dynamic_peers = vec![me.clone(), roamer];
        let observed = vec![obs("feeder_blocks-head", &["feeder.blocks.>"])];
        assert!(consume_target_missing(&observed, &me, &dynamic_peers).is_empty());
    }

    // --- Task 7: StreamNameConflict -----------------------------------

    #[test]
    fn stream_name_conflict_fires_when_the_composed_name_is_squatted() {
        let mut me = cv("demo", "feeder");
        me.streams = vec![sv("orders", &["feeder.orders.>"], None, "1Gi")];
        // Somebody got there first with different subjects — the composed
        // name is what the deny vector keys on, so its rules now apply to
        // this other stream's data.
        let observed = vec![obs("feeder_orders", &["squatter.junk.>"])];
        let got = stream_name_conflict(&observed, &me);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("feeder_orders"), "{got:?}");
    }

    #[test]
    fn stream_name_conflict_does_not_fire_on_our_own_stream_or_a_reordering() {
        let mut me = cv("demo", "feeder");
        me.streams = vec![sv(
            "orders",
            &["feeder.orders.>", "feeder.returns.>"],
            None,
            "1Gi",
        )];
        // Same set, different order — NACK writes them back in declaration
        // order today, but nothing in the protocol promises that, and a
        // false alarm here trains a reader to ignore the condition.
        let observed = vec![obs(
            "feeder_orders",
            &["feeder.returns.>", "feeder.orders.>"],
        )];
        assert!(stream_name_conflict(&observed, &me).is_empty());
        // And nothing at all when the stream does not exist yet.
        assert!(stream_name_conflict(&[], &me).is_empty());
    }

    // --- Task 7: assembly ---------------------------------------------

    #[test]
    fn a_healthy_namespace_produces_no_conditions_at_all() {
        // The test every "does not fire" half depends on: if this claim
        // carried a standing condition, every negative assertion elsewhere
        // could pass for the wrong reason.
        let mut feeder = cv("demo", "feeder");
        feeder.streams = vec![sv("orders", &["feeder.orders.>"], None, "256Mi")];
        let mut indexer = cv("demo", "indexer");
        indexer.consumes = vec![ConsumeView {
            owner: "feeder".into(),
            stream: "orders".into(),
            durable: "idx".into(),
        }];
        let peers = vec![feeder.clone(), indexer];
        let observed = vec![obs_sized(
            "feeder_orders",
            &["feeder.orders.>"],
            256 << 20,
            7,
        )];
        let s = jetstream_signals(&observed, &feeder, &peers, u64::MAX, NOW, &[]);
        assert!(s.conditions.is_empty(), "{:?}", s.conditions);
        assert_eq!(s.inventory.declared, vec!["feeder_orders".to_string()]);
        assert_eq!(s.held_bytes, 7);
        assert!(s.captures.is_empty());
    }

    #[test]
    fn signals_carry_every_firing_condition_and_only_those() {
        let mut me = cv("demo", "feeder");
        me.quota_bytes = 1; // one byte of budget
        me.streams = vec![sv("orders", &["feeder.orders.>"], None, "1Gi")];
        me.consumes = vec![ConsumeView {
            owner: "ghost".into(),
            stream: "nope".into(),
            durable: "d".into(),
        }];
        let observed = vec![obs("squatter", &["feeder.stolen.>"])];
        let s = jetstream_signals(
            &observed,
            &me,
            std::slice::from_ref(&me),
            u64::MAX,
            NOW,
            &[],
        );
        let types: Vec<&str> = s.conditions.iter().map(|c| c.type_.as_str()).collect();
        assert!(types.contains(&COND_FOREIGN_SUBJECT_CAPTURE), "{types:?}");
        assert!(types.contains(&COND_QUOTA_EXCEEDED), "{types:?}");
        assert!(types.contains(&COND_CONSUME_TARGET_MISSING), "{types:?}");
        assert!(!types.contains(&COND_NAMESPACE_DRAIN_RISK), "{types:?}");
        assert!(!types.contains(&COND_PREFIX_PRE_CAPTURED), "{types:?}");
        assert!(!types.contains(&COND_STREAM_NAME_CONFLICT), "{types:?}");
        for c in &s.conditions {
            assert_eq!(c.status, "True", "present means firing: {c:?}");
            assert!(JETSTREAM_CONDITION_TYPES.contains(&c.type_.as_str()));
        }
    }

    #[test]
    fn a_standing_condition_keeps_its_first_transition_time() {
        // Every status write wakes this controller again, so a condition
        // whose timestamp moved on every resync would both lie about when
        // the problem started and feed a write loop.
        let me = cv("demo", "feeder");
        let observed = vec![obs("squatter", &["feeder.stolen.>"])];
        let first = jetstream_signals(
            &observed,
            &me,
            std::slice::from_ref(&me),
            u64::MAX,
            NOW,
            &[],
        );
        let later = jetstream_signals(
            &observed,
            &me,
            std::slice::from_ref(&me),
            u64::MAX,
            "2026-09-13T00:00:00+00:00",
            &first.conditions,
        );
        assert_eq!(
            later.conditions[0].last_transition_time, NOW,
            "an unchanged condition must keep its original transition time"
        );
    }
}
