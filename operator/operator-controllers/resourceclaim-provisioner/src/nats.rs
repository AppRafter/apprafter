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

use operator_core::{JetStreamStream as CoreJetStreamStream, ResourceClaim};

use crate::nats_accounts::{
    account_name, nats_durable_name, nats_stream_name, ClaimView, ConsumeView, StreamView,
};

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
}
