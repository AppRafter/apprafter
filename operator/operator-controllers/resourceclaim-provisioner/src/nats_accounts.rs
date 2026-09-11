// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Renders the NATS accounts file from the live claim set (ADR 0061 §3).
//!
//! Pure by construction: no Kubernetes client, no network, no clock. The
//! file is derived WHOLE on every write and never patched, because one
//! application's deny vector depends on other applications' declarations
//! — so any claim change anywhere invalidates other users' rules.
//!
//! This is the first of three pieces sharing this file (ADR 0061 §3 /
//! §6): the input view and naming derivations (this task), the allow
//! list and deny vector (next), and the file renderer (after that). Only
//! the first is here — the other two are deliberately not anticipated.

/// One live jetstream claim, flattened to what the renderer needs.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimView {
    pub namespace: String,
    pub app: String,
    pub dynamic_streams: bool,
    pub streams: Vec<StreamView>,
    pub consumes: Vec<ConsumeView>,
    pub quota_bytes: u64,
}

/// A stream this application declares.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamView {
    pub name: String,
    pub subjects: Vec<String>,
    pub allow_purge: bool,
}

/// A declared durable consumer this application holds on `owner`'s stream.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsumeView {
    /// Owning application. Equal to the consumer's own app for an own-stream
    /// consume.
    pub owner: String,
    /// The owner's DECLARED stream name, not the composed NATS name.
    pub stream: String,
    pub durable: String,
}

impl ClaimView {
    /// `_` is not legal in a DNS-1123 namespace name, so `-` → `_` is
    /// injective and two namespaces can never fold onto one account.
    pub fn account(&self) -> String {
        format!("ns_{}", self.namespace.replace('-', "_"))
    }
    pub fn user(&self) -> String {
        format!("claim_{}_{}_jetstream", self.namespace, self.app)
    }
    pub fn subject_prefix(&self) -> String {
        format!("{}.", self.app)
    }
    pub fn inbox_prefix(&self) -> String {
        format!("_INBOX_{}_{}", self.namespace, self.app)
    }
}

/// The per-account management user name — the identity the provisioner's
/// own NATS connection authenticates as when administering this account
/// (stream/consumer CRUD, ACL updates), distinct from any claim's own
/// `user()`.
pub fn mgr_user(namespace: &str) -> String {
    format!("mgr_{namespace}")
}

/// NATS-side stream name.
///
/// Collision-free within a namespace/account, by the SAME argument as
/// [`ClaimView::account`]: the join character is `_`, and `_` is illegal
/// in both operands, so the encoding is self-delimiting (there is
/// exactly one way to read a composed name back into `owner_app` and
/// `declared`, because no `_` inside either half could ever be mistaken
/// for the separator). This module does not, and cannot, enforce that
/// premise on its own — it has no validation of any kind (see the module
/// doc). The premise holds because `validate_jetstream_need` in
/// `admission-webhook/src/validator.rs` rejects any `streams[].name`
/// (the `declared` here) that is not a DNS-1123 label, and DNS-1123's
/// alphabet (`[a-z0-9-]`) excludes `_` by definition; `owner_app` is a
/// Kubernetes object name, which is a DNS-1123 subdomain and so is ALSO
/// `_`-free unconditionally. A reader of this file alone cannot see that
/// the premise is enforced anywhere — it lives in the webhook, not here.
///
/// This was `-`-joined until a review round found the same ambiguity
/// `account()` was checked against and did not have: `owner_app` and
/// `declared` could both contain `-`, so `("a", "b-c")` and `("a-b",
/// "c")` composed to the identical string. That also broke ADR 0061
/// §4.2's soundness proof for the deny vector's position patterns, which
/// assumes a durable can collide with a stream name only WITHIN one
/// application — a `-`-composed collision could cross applications, past
/// the point the webhook's within-one-application check ever looked.
pub fn nats_stream_name(owner_app: &str, declared: &str) -> String {
    format!("{owner_app}_{declared}")
}

/// NATS-side durable name. Takes the CONSUMING application — the opposite
/// of `nats_stream_name`, which takes the OWNING one — because a durable
/// name is chosen by whoever declares the `consume` entry, not by the
/// stream's owner. Two different applications may each declare a durable
/// called `indexer` on the SAME shared stream; without the consumer's own
/// prefix here, the second application's `indexer` would collide with the
/// first's and silently attach to its existing cursor — sharing (and
/// corrupting) its delivery position — instead of getting its own. Do not
/// "simplify" this to match `nats_stream_name`'s owner-keyed shape; the
/// asymmetry is the point. Carries the SAME collision-freedom argument as
/// `nats_stream_name` above, enforced the same way: `validate_jetstream_need`
/// requires `consume[].durable` (the `declared` here) to be a DNS-1123
/// label too, for the identical reason.
pub fn nats_durable_name(consumer_app: &str, declared: &str) -> String {
    format!("{consumer_app}_{declared}")
}

/// Class (C): total denial of a stream, expressed by TOKEN POSITION rather
/// than by verb name. A verb list is complete only until the next NATS
/// release — `CONSUMER.RESET` arrived in the pinned one — and a silently
/// non-matching deny rule is this design's characteristic failure.
///
/// Complete against every operation `allow_list` admits (ADR 0061 §4.2),
/// NOT against all of NATS — measured, `$JS.API.A.B.C.D.E.S` (depth 8)
/// publishes under all four patterns and no finite pattern set closes
/// that; extending the set moves the edge, it does not generalise. What
/// makes these four patterns sufficient is that `allow_list` never admits
/// anything past depth 6 — see its own doc and
/// `every_allowed_api_subject_puts_the_stream_at_depth_5_or_6`.
fn total_denial_patterns(stream: &str) -> Vec<String> {
    vec![
        format!("$JS.API.*.*.{stream}"),
        format!("$JS.API.*.*.{stream}.>"),
        format!("$JS.API.*.*.*.{stream}"),
        format!("$JS.API.*.*.*.{stream}.>"),
        format!("$JS.ACK.{stream}.>"),
        format!("$JS.ACK.*.*.{stream}.>"),
        format!("$JS.FC.{stream}.>"),
        format!("$JS.FC.*.*.{stream}.>"),
    ]
}

/// Both acknowledgement subject forms, always. v1 puts the stream at token
/// 3 and v2 at token 5 (domain, degenerating to `_` but always present,
/// then the account hash). A v1-only rule does not match a v2 subject —
/// measured — and the `*.*` wildcard means the account hash never needs
/// deriving.
///
/// `deny_vector`'s class (D) is the only caller in this task, always with
/// `durable: Some(_)` — the `None` (stream-only) branch is unreachable
/// from anything this task calls. It overlaps, entry for entry, with
/// `total_denial_patterns`'s own inline `$JS.ACK`/`$JS.FC` lines when it
/// IS reached, which is between the two FUNCTIONS' possible outputs, not
/// between anything `deny_vector` emits twice today
/// (`total_denial_patterns` does not call this function; it carries its
/// own copy, deliberately, per its own doc). Kept as `Option`, not
/// narrowed to `&str`, because the signature is shared design surface for
/// a later task in this same file (§3/§6's file renderer) — not because
/// this task uses both branches.
fn ack_fc_patterns(stream: &str, durable: Option<&str>) -> Vec<String> {
    let tail = match durable {
        Some(d) => format!("{stream}.{d}"),
        None => stream.to_string(),
    };
    vec![
        format!("$JS.ACK.{tail}.>"),
        format!("$JS.ACK.*.*.{tail}.>"),
        format!("$JS.FC.{tail}.>"),
        format!("$JS.FC.*.*.{tail}.>"),
    ]
}

/// The security boundary (ADR 0061 §4/§6): every subject `me`'s claim user
/// may PUBLISH to. [`deny_vector`] only carves holes in what THIS admits —
/// it can never widen the grant, and must never be asked to. Adding an
/// entry here is a security change, WHATEVER prompted it — there is no
/// "just a convenience" addition to an allow list.
///
/// Every `$JS.API.*` entry places the stream token, once substituted, at
/// ABSOLUTE token position 5 or 6 — not by luck, but because that is
/// exactly what [`total_denial_patterns`]'s four patterns cover, and this
/// list is the ONLY thing that has to stay inside that boundary for §4.2's
/// completeness argument to hold. `every_allowed_api_subject_puts_the_stream_at_depth_5_or_6`
/// enforces it: adding a subject whose verb path is longer than 2 or 3
/// tokens (placing the stream at 7+) turns it red — the one test in this
/// file that runs BACKWARDS from the usual "does the code reject bad
/// input" and instead asks "does the code refuse to ADMIT an operation
/// the deny vector cannot reach."
pub fn allow_list(me: &ClaimView) -> Vec<String> {
    let mut list = vec![
        format!("{}>", me.subject_prefix()),
        "$JS.ACK.>".to_string(),
        "$JS.FC.>".to_string(),
        "$JS.API.INFO".to_string(),
        "$JS.API.STREAM.NAMES".to_string(),
        "$JS.API.STREAM.INFO.>".to_string(),
        "$JS.API.STREAM.MSG.GET.>".to_string(),
        "$JS.API.DIRECT.GET.>".to_string(),
        "$JS.API.CONSUMER.CREATE.>".to_string(),
        "$JS.API.CONSUMER.DURABLE.CREATE.>".to_string(),
        "$JS.API.CONSUMER.INFO.>".to_string(),
        "$JS.API.CONSUMER.DELETE.>".to_string(),
        "$JS.API.CONSUMER.LIST.>".to_string(),
        "$JS.API.CONSUMER.NAMES.>".to_string(),
        "$JS.API.CONSUMER.MSG.NEXT.>".to_string(),
        "$JS.API.CONSUMER.PAUSE.>".to_string(),
        "$JS.API.CONSUMER.RESET.>".to_string(),
    ];

    // PURGE is granted POSITIVELY, by exact composed stream name, never as
    // a blanket `$JS.API.STREAM.PURGE.>` — so it can only ever name a
    // stream `me` itself declared and opted into via `allowPurge`, never a
    // neighbour's.
    for stream in &me.streams {
        if stream.allow_purge {
            list.push(format!(
                "$JS.API.STREAM.PURGE.{}",
                nats_stream_name(&me.app, &stream.name)
            ));
        }
    }

    // "All my streams are dynamic" (ADR 0061 §6) — a deliberate, reviewed
    // opt-in, never a default. See `deny_vector`'s class (B) for why this
    // does not reach a DECLARED stream even once granted here: the grant
    // is namespace-wide by verb, the carve-out is per-stream and
    // unconditional.
    if me.dynamic_streams {
        list.extend([
            "$JS.API.STREAM.CREATE.>".to_string(),
            "$JS.API.STREAM.UPDATE.>".to_string(),
            "$JS.API.STREAM.DELETE.>".to_string(),
            "$JS.API.STREAM.MSG.DELETE.>".to_string(),
        ]);
    }

    list.sort();
    list.dedup();
    list
}

/// The holes carved out of [`allow_list`] (ADR 0061 §4/§6) — three
/// classes, never a widening. There is no class (A) or (E): both were
/// restatements of exclusions an exhaustive `allow_list` already makes by
/// never granting them in the first place.
///
/// Do NOT restore a blanket deny for the constrained (non-`dynamic_streams`)
/// mode here. If a test seems to want one, `allow_list` is the thing that
/// is wrong, not this function.
pub fn deny_vector(me: &ClaimView, namespace_claims: &[ClaimView]) -> Vec<String> {
    let mut list = Vec::new();

    // (B) Every declared stream in the NAMESPACE, including `me`'s own —
    // unconditional: independent of `dynamic_streams` and of ownership,
    // and therefore identical no matter which claim in the namespace this
    // is computed for. `allow_list` grants
    // STREAM.{CREATE,UPDATE,DELETE,MSG.DELETE} only when `dynamic_streams`
    // is true; emitting this deny regardless of that flag is what keeps
    // the grant from ever reaching a stream that went through the
    // migration gate the moment the flag flips — an app can mutate only
    // an UNDECLARED stream it creates dynamically (never in
    // `namespace_claims`, so never denied here), not a declared one.
    // PURGE is deliberately absent: granted positively, per-stream, in
    // `allow_list`, not carved out of a blanket deny here.
    for claim in namespace_claims {
        for stream in &claim.streams {
            let composed = nats_stream_name(&claim.app, &stream.name);
            list.push(format!("$JS.API.STREAM.UPDATE.{composed}"));
            list.push(format!("$JS.API.STREAM.DELETE.{composed}"));
            list.push(format!("$JS.API.STREAM.MSG.DELETE.{composed}"));
        }
    }

    // (C) Every declared stream `me` neither owns nor consumes: total
    // denial by position (`total_denial_patterns`), not by verb name.
    for claim in namespace_claims {
        for stream in &claim.streams {
            let owns = claim.app == me.app;
            let consumes = me
                .consumes
                .iter()
                .any(|c| c.owner == claim.app && c.stream == stream.name);
            if owns || consumes {
                continue;
            }
            let composed = nats_stream_name(&claim.app, &stream.name);
            list.extend(total_denial_patterns(&composed));
        }
    }

    // (D) Every declared consumer belonging to ANOTHER application, on
    // ANY stream — including one `me` itself owns. This protects a
    // neighbour's durable (its position in the stream, and its acks)
    // from the stream's own owner just as much as from any other
    // application; a producer must not be able to reset or forge acks
    // against a consumer it does not hold.
    for claim in namespace_claims {
        if claim.app == me.app {
            continue;
        }
        for consume in &claim.consumes {
            let composed_stream = nats_stream_name(&consume.owner, &consume.stream);
            let composed_durable = nats_durable_name(&claim.app, &consume.durable);
            list.push(format!(
                "$JS.API.CONSUMER.*.{composed_stream}.{composed_durable}"
            ));
            list.push(format!(
                "$JS.API.CONSUMER.*.*.{composed_stream}.{composed_durable}"
            ));
            list.extend(ack_fc_patterns(&composed_stream, Some(&composed_durable)));
        }
    }

    list.sort();
    list.dedup();
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(ns: &str, app: &str) -> ClaimView {
        ClaimView {
            namespace: ns.to_string(),
            app: app.to_string(),
            dynamic_streams: false,
            streams: vec![],
            consumes: vec![],
            quota_bytes: 1 << 30,
        }
    }

    #[test]
    fn names_are_derived_not_stored() {
        let c = claim("demo-ns", "feeder");
        assert_eq!(c.account(), "ns_demo_ns");
        assert_eq!(c.user(), "claim_demo-ns_feeder_jetstream");
        assert_eq!(c.subject_prefix(), "feeder.");
        assert_eq!(c.inbox_prefix(), "_INBOX_demo-ns_feeder");
        assert_eq!(mgr_user("demo-ns"), "mgr_demo-ns");
    }

    #[test]
    fn stream_and_durable_names_carry_their_owner() {
        assert_eq!(
            nats_stream_name("feeder", "blocks-head"),
            "feeder_blocks-head"
        );
        // the durable carries the CONSUMING application, so two applications
        // may each hold `indexer` on one shared stream without colliding
        assert_eq!(
            nats_durable_name("indexer-app", "indexer"),
            "indexer-app_indexer"
        );
    }

    fn ns_with_two_apps() -> Vec<ClaimView> {
        let mut a = claim("demo", "feeder");
        a.streams = vec![StreamView {
            name: "blocks-head".into(),
            subjects: vec!["feeder.blocks.head.>".into()],
            allow_purge: false,
        }];
        let mut b = claim("demo", "indexer");
        b.consumes = vec![ConsumeView {
            owner: "feeder".into(),
            stream: "blocks-head".into(),
            durable: "idx".into(),
        }];
        vec![a, b]
    }

    #[test]
    fn the_allow_list_omits_everything_dangerous_rather_than_denying_it() {
        // An explicit allow list is exhaustive, so these are excluded by NOT
        // appearing. Restating them as denies would suggest the allow list is
        // not exhaustive — the misreading most likely to cause a widening.
        let a = allow_list(&claim("demo", "feeder"));
        for never in [
            "$JS.API.STREAM.LIST",
            "$JS.API.STREAM.SNAPSHOT.>",
            "$JS.API.STREAM.RESTORE.>",
            "$JS.API.STREAM.LEADER.STEPDOWN.>",
            "$JS.API.>",
        ] {
            assert!(
                !a.iter().any(|p| p == never),
                "{never} must not be granted: {a:?}"
            );
        }
        assert!(a.contains(&"feeder.>".to_string()));
        assert!(a.contains(&"$JS.API.STREAM.INFO.>".to_string()));
        assert!(a.contains(&"$JS.ACK.>".to_string()));
    }

    #[test]
    fn every_allowed_api_subject_puts_the_stream_at_depth_5_or_6() {
        // The four position patterns are complete against the ALLOW LIST, not
        // against all of NATS — measured, depth 8 leaks through any finite
        // set. This test is what makes "complete against a closed set" a fact
        // rather than a claim.
        //
        // "Depth" is the ABSOLUTE token position the stream name lands at
        // once a caller substitutes it: for a `.>`-terminated entry, `>`'s
        // FIRST matched token is always the stream name — one position
        // PAST the literal prefix (`trim_end_matches(".>").split('.').count()`
        // alone, without the `+ 1`, undercounts every such entry by
        // exactly one, which would make this assertion pass on a bad
        // depth-6-prefix / true-depth-7 entry — the one case this test
        // exists to catch). A concrete entry that already carries a
        // composed name (`STREAM.PURGE.<name>`, no trailing wildcard) has
        // no such extra slot, so no `+ 1` there — it IS its own last
        // token.
        for subj in allow_list(&claim("demo", "feeder")) {
            if !subj.starts_with("$JS.API.")
                || subj == "$JS.API.INFO"
                || subj == "$JS.API.STREAM.NAMES"
            {
                continue;
            }
            let depth = match subj.strip_suffix(".>") {
                Some(prefix) => prefix.split('.').count() + 1,
                None => subj.split('.').count(),
            };
            assert!(
                depth == 5 || depth == 6,
                "{subj} puts the stream token at depth {depth}; the deny patterns \
                 cover 5 and 6 only. Granting it requires a matching pattern pair \
                 in total_denial_patterns()."
            );
        }
    }

    #[test]
    fn dynamic_streams_is_an_allow_list_decision_not_a_deny_one() {
        let mut c = claim("demo", "feeder");
        let constrained = allow_list(&c);
        for mutating in [
            "$JS.API.STREAM.CREATE.>",
            "$JS.API.STREAM.UPDATE.>",
            "$JS.API.STREAM.DELETE.>",
            "$JS.API.STREAM.MSG.DELETE.>",
        ] {
            assert!(
                !constrained.iter().any(|p| p == mutating),
                "constrained mode must not be granted {mutating}"
            );
        }
        c.dynamic_streams = true;
        let permissive = allow_list(&c);
        for mutating in [
            "$JS.API.STREAM.CREATE.>",
            "$JS.API.STREAM.UPDATE.>",
            "$JS.API.STREAM.DELETE.>",
            "$JS.API.STREAM.MSG.DELETE.>",
        ] {
            assert!(
                permissive.contains(&mutating.to_string()),
                "missing {mutating}"
            );
        }
        for m in [&constrained, &permissive] {
            assert!(m.contains(&"$JS.API.CONSUMER.CREATE.>".to_string()));
            assert!(m.contains(&"$JS.API.CONSUMER.MSG.NEXT.>".to_string()));
        }
    }

    #[test]
    fn allow_purge_grants_purge_on_the_owners_own_stream_only() {
        // PURGE is granted POSITIVELY, by exact stream name, rather than by
        // carving a hole in a blanket deny — so it can only ever name a stream
        // the declaring application owns.
        let mut all = ns_with_two_apps();
        all[0].streams[0].allow_purge = true;
        let owner = allow_list(&all[0]);
        assert!(
            owner.contains(&"$JS.API.STREAM.PURGE.feeder_blocks-head".to_string()),
            "allowPurge must grant PURGE on the owner's own stream: {owner:?}"
        );
        assert!(
            !owner.iter().any(|p| p.starts_with("$JS.API.STREAM.UPDATE")),
            "allowPurge must not grant UPDATE in constrained mode"
        );
        let neighbour = allow_list(&all[1]);
        assert!(
            !neighbour.iter().any(|p| p.contains("PURGE")),
            "allowPurge is the owner's alone: {neighbour:?}"
        );
    }

    #[test]
    fn allow_purge_false_grants_no_purge_even_to_the_owner() {
        // The mutation-test round-8 gap: the above test only ever exercises
        // `allow_purge: true` — it never proves the guard does anything,
        // since removing it entirely (granting PURGE unconditionally for
        // every one of `me`'s own streams) left all other tests green.
        // `ns_with_two_apps()`'s stream defaults to `allow_purge: false`,
        // unmodified here.
        let all = ns_with_two_apps();
        let owner = allow_list(&all[0]);
        assert!(
            !owner.iter().any(|p| p.contains("PURGE")),
            "allowPurge: false (the default) must grant no PURGE at all, \
             even to the stream's own owner: {owner:?}"
        );
    }

    #[test]
    fn class_b_denies_mutation_of_the_apps_own_declared_stream() {
        // Only reachable when dynamicStreams is on — the mutating verbs are
        // otherwise absent from the allow list. The deny is what stops a
        // permissive app editing the stream the migration gate approved.
        let mut all = ns_with_two_apps();
        all[0].dynamic_streams = true;
        let d = deny_vector(&all[0], &all); // feeder, the OWNER
        assert!(
            d.contains(&"$JS.API.STREAM.UPDATE.feeder_blocks-head".to_string()),
            "own declared streams must be immutable from the app side"
        );
        assert!(d.contains(&"$JS.API.STREAM.DELETE.feeder_blocks-head".to_string()));
        assert!(d.contains(&"$JS.API.STREAM.MSG.DELETE.feeder_blocks-head".to_string()));
    }

    #[test]
    fn class_c_denies_by_position_for_a_stream_neither_owned_nor_consumed() {
        let mut all = ns_with_two_apps();
        all[1].consumes.clear(); // indexer no longer consumes it
        let d = deny_vector(&all[1], &all);
        for p in [
            "$JS.API.*.*.feeder_blocks-head",
            "$JS.API.*.*.feeder_blocks-head.>",
            "$JS.API.*.*.*.feeder_blocks-head",
            "$JS.API.*.*.*.feeder_blocks-head.>",
            "$JS.ACK.feeder_blocks-head.>",
            "$JS.ACK.*.*.feeder_blocks-head.>",
            "$JS.FC.feeder_blocks-head.>",
            "$JS.FC.*.*.feeder_blocks-head.>",
        ] {
            assert!(d.contains(&p.to_string()), "missing {p}");
        }
        assert!(
            !d.iter().any(|p| p.contains("STREAM.INFO")),
            "class C must not enumerate verbs — a verb list is complete only \
             until the next NATS release"
        );
    }

    #[test]
    fn a_consumed_stream_keeps_its_read_verbs() {
        let all = ns_with_two_apps();
        let d = deny_vector(&all[1], &all); // indexer, which DOES consume
        assert!(
            !d.iter().any(|p| p == "$JS.API.*.*.feeder_blocks-head"),
            "a consumed stream must stay readable"
        );
        assert!(
            d.contains(&"$JS.API.STREAM.DELETE.feeder_blocks-head".to_string()),
            "but it must stay immutable"
        );
    }

    #[test]
    fn class_d_protects_a_neighbours_declared_consumer_on_a_shared_stream() {
        let all = ns_with_two_apps();
        let d = deny_vector(&all[0], &all); // feeder, the stream's OWNER
        for p in [
            "$JS.API.CONSUMER.*.feeder_blocks-head.indexer_idx",
            "$JS.API.CONSUMER.*.*.feeder_blocks-head.indexer_idx",
            "$JS.ACK.feeder_blocks-head.indexer_idx.>",
            "$JS.ACK.*.*.feeder_blocks-head.indexer_idx.>",
        ] {
            assert!(
                d.contains(&p.to_string()),
                "the producer must not be able to delete or forge acks against \
                 the consumer's durable: missing {p}"
            );
        }
    }

    #[test]
    fn the_vector_is_sorted_and_deduplicated() {
        let all = ns_with_two_apps();
        let d = deny_vector(&all[0], &all);
        let mut sorted = d.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(d, sorted, "output must be byte-stable across runs");
    }
}
