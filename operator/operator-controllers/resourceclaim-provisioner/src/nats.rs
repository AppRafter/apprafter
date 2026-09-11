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

use crate::nats_accounts::{ClaimView, ConsumeView, StreamView};

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
fn declaring_app(claim: &ResourceClaim) -> Option<String> {
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
}
