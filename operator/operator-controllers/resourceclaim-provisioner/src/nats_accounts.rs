// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Renders the NATS accounts file from the live claim set (ADR 0061 §3).
//!
//! Pure by construction: no Kubernetes client, no network, no clock. The
//! file is derived WHOLE on every write and never patched, because one
//! application's deny vector depends on other applications' declarations
//! — so any claim change anywhere invalidates other users' rules.
//!
//! Three pieces, built across three tasks and landed in this order: the
//! input view and naming derivations ([`ClaimView`] and friends,
//! [`nats_stream_name`], [`nats_durable_name`]), the allow list and deny
//! vector (`allow_list`, `deny_vector`), and the file renderer
//! ([`render_accounts_file`]) — which is the only one a caller outside
//! this module needs. `mgr_user`/`nats_stream_name`/`nats_durable_name`
//! stay `pub` for that same future caller (part 2); `allow_list` and
//! `deny_vector` are private (round-7 review: they were `pub` with the
//! doc claiming "exported because the tests need it," which was never
//! true — `mod tests` below is a CHILD module and sees private items via
//! `use super::*`, the same as every other private helper in this file).

use std::collections::BTreeMap;
use std::fmt::Write as _;

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
///
/// `retention` and `max_bytes` are carried for 2.5f's conditions, not for
/// the accounts file — `render_account` reads neither, and deliberately:
/// `NamespaceDrainRisk` and `WorkqueueSubjectOverlap` are facts about
/// `retention: workqueue` (ADR 0061 §4.1's drain primitive only bites a
/// workqueue origin), and `QuotaExceeded` is arithmetic over `maxBytes`
/// (§6). Both live on the DECLARATION, so the claim view is the only
/// place they can reach the provisioner; a second flattening pass over
/// the same claims would be a second thing to keep in step.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamView {
    pub name: String,
    pub subjects: Vec<String>,
    pub allow_purge: bool,
    /// `limits` (the CRD default when absent) | `interest` | `workqueue`.
    /// `None` means the manifest said nothing, which NACK renders as
    /// `limits` — so a reader asking "is this a workqueue" must treat
    /// `None` as "no", never as "unknown".
    pub retention: Option<String>,
    /// The declared Kubernetes quantity STRING (`"1Gi"`), not bytes —
    /// parsed at the use site by [`crate::nats::quantity_bytes`]. Required
    /// by the CRD, so it is never empty on a claim that reached this far.
    pub max_bytes: String,
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

/// The NATS account name for a namespace. `_` is not legal in a DNS-1123
/// namespace name, so `-` → `_` is injective and two namespaces can never
/// fold onto one account. A free function, not inlined at both call sites
/// ([`ClaimView::account`] and `render_account`, which knows only a
/// `&str` namespace, not a whole [`ClaimView`]) — Round-7 review found the
/// two had drifted into independent copies of this same formula, and
/// deleting `render_account`'s own copy left the whole suite green
/// because every render-path fixture used a hyphen-free namespace, so the
/// two never had a chance to disagree
/// (`the_account_key_matches_claimview_account_for_a_hyphenated_namespace`
/// closes that gap).
///
/// `pub` (2.5e Task 2): `nats::account_object` needs the SAME NATS-side
/// name for the Account CR's `spec.name` — reusing this fold rather than
/// adding a third independent copy, the exact drift round-7 review
/// already found once between this function and `render_account`'s own
/// (now-deleted) copy.
pub fn account_name(namespace: &str) -> String {
    format!("ns_{}", namespace.replace('-', "_"))
}

impl ClaimView {
    pub fn account(&self) -> String {
        account_name(&self.namespace)
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
///
/// The last four entries are [`ack_fc_patterns`]`(stream, None)` — the
/// STREAM-ONLY form (no durable), because a total denial has no
/// consumer to name. Round-7 review: this used to carry its own inline
/// copy of those same four lines instead of calling that function, which
/// is why `ack_fc_patterns`'s `None` branch went uncovered — nothing
/// called it. Delegating here makes the branch live.
fn total_denial_patterns(stream: &str) -> Vec<String> {
    let mut list = vec![
        format!("$JS.API.*.*.{stream}"),
        format!("$JS.API.*.*.{stream}.>"),
        format!("$JS.API.*.*.*.{stream}"),
        format!("$JS.API.*.*.*.{stream}.>"),
    ];
    list.extend(ack_fc_patterns(stream, None));
    list
}

/// Both acknowledgement subject forms, always. v1 puts the stream at token
/// 3 and v2 at token 5 (domain, degenerating to `_` but always present,
/// then the account hash). A v1-only rule does not match a v2 subject —
/// measured — and the `*.*` wildcard means the account hash never needs
/// deriving.
///
/// Two callers: `deny_vector`'s class (D), always with `durable:
/// Some(_)` (a specific consumer's cursor), and [`total_denial_patterns`]
/// class (C), always with `None` (no consumer to name — the whole stream
/// is denied). Kept as `Option`, not two functions or a narrowed `&str`,
/// because both shapes are the SAME four-pattern structure over a
/// different `tail`. This used to be true only in the signature — the
/// `None` branch had no caller until `total_denial_patterns` was
/// refactored to call it instead of carrying its own duplicate literals
/// (round-7 review); both branches are exercised by this module's tests
/// now.
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
fn allow_list(me: &ClaimView) -> Vec<String> {
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
    // opt-in, never a default. `deny_vector`'s class (B) carves every
    // declared stream's exact composed name back out of all four of
    // these verbs (CREATE included, since round-7 review: CREATE was
    // missing from that carve-out until then, which let a
    // dynamic_streams app squat a namespace-mate's declared stream name
    // ahead of NACK's own create) — the grant here is namespace-wide by
    // verb, the carve-out there is per-stream and unconditional, so none
    // of the four ever reaches a stream that went through the migration
    // gate.
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
fn deny_vector(me: &ClaimView, namespace_claims: &[ClaimView]) -> Vec<String> {
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
    // CREATE is included (round-7 review, H2): its absence let a
    // dynamic_streams app CREATE a stream under a namespace-mate's
    // already-declared, not-yet-materialised name — subjects are not
    // permission-checked at creation, so the squatter's own subjects
    // would win the race, NACK's later create of the real stream would
    // fail 10058, and §5's detector could read the squatted stream as
    // legitimate. PURGE is deliberately absent: granted positively,
    // per-stream, in `allow_list`, not carved out of a blanket deny here.
    for claim in namespace_claims {
        for stream in &claim.streams {
            let composed = nats_stream_name(&claim.app, &stream.name);
            list.push(format!("$JS.API.STREAM.CREATE.{composed}"));
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

/// Rendering this file successfully MUST NOT be able to produce an
/// account whose authentication can be bypassed (ADR 0042 §10, the same
/// finding this design inherits from the Dragonfly ACL work: the
/// dangerous failure mode of a credential file is not lockout but
/// SILENTLY DISABLED authentication — a file that parses while omitting
/// a user's password leaves the server accepting everyone as that user).
/// Both variants are refusals, never a partial or best-effort render.
#[derive(Debug, thiserror::Error)]
pub enum AccountsFileError {
    #[error("refusing to render: empty password for user {0}")]
    EmptyPassword(String),
    #[error("refusing to render: account {0} would carry no users")]
    EmptyAccount(String),
    /// 2.5d Task 8b: the per-namespace ceiling (`namespace_ceiling_bytes`
    /// on [`render_accounts_file`]) bounds ONE account; nothing bounded N
    /// namespaces together, so N namespaces could jointly promise more
    /// `max_file` than the server's shared JetStream PVC (or more
    /// `max_mem` than its `max_memory_store`) actually has — NATS does
    /// not refuse this itself; the accounts compete for capacity
    /// promised twice, surfacing as an unattributable allocation failure
    /// in whoever asks last. REFUSE, not a proportional clamp: a tenant
    /// that cannot have what it asked for should be told, not silently
    /// handed less than its manifest says while every OTHER tenant is
    /// silently handed less too.
    #[error(
        "refusing to render: total quota across all namespaces ({total} bytes) exceeds the platform-wide budget ({budget} bytes)"
    )]
    GlobalBudgetExceeded { total: u64, budget: u64 },
}

/// Floor under [`account_max_mem_bytes`]'s fraction: keeps a namespace
/// with a small file quota still able to use `storage: memory` AT ALL.
/// Round-7 review (H3): a flat `0` ceiling made `storage: memory` fail
/// with an opaque `10028 insufficient memory resources` from
/// nats-server — despite `#JetStreamStream.storage` offering `file |
/// memory` in CUE, and the webhook's own rejection message for
/// `persistent` recommending memory BY NAME
/// (`streams[].storage: file | memory`) as the per-stream persistence
/// knob. ADR 0061 §1 makes "one server serves both persistence classes"
/// a deliberate reason this design did not copy Dragonfly's two-pool
/// shape, so `memory` staying usable is load-bearing, not incidental.
const ACCOUNT_MAX_MEM_FLOOR_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

/// Denominator of [`account_max_mem_bytes`]'s fraction of the namespace's
/// FILE quota. `1/10`: a memory-storage stream lives in the SERVER's own
/// RSS on a ~4 GB Tier-1 node — not on disk — so its cost is shared with
/// EVERY OTHER tenant's process on that node instead of being
/// per-account-isolated the way file storage is; granting the full file
/// quota (or an ungoverned large fraction of it) would be a real OOM
/// vector.
const ACCOUNT_MAX_MEM_FRACTION_DIVISOR: u64 = 10;

/// The account-JetStream memory ceiling (ADR 0061 §3): a documented
/// FRACTION of the namespace's own summed file quota
/// ([`ACCOUNT_MAX_MEM_FRACTION_DIVISOR`]), floored at
/// [`ACCOUNT_MAX_MEM_FLOOR_BYTES`] — not the flat `0` this constant
/// replaced (round-7 review, H3), and not the file quota itself (see
/// that constant's own doc for why a memory ceiling can't just mirror
/// `max_file`). Both numbers are a conservative Tier-1 FALLBACK, not a
/// considered budget: part 2 makes this tier-aware, sourced from the
/// `jetstream-integrated` seed's own memory reservation, and this
/// function is what falls back until that lands.
fn account_max_mem_bytes(total_file_quota_bytes: u64) -> u64 {
    (total_file_quota_bytes / ACCOUNT_MAX_MEM_FRACTION_DIVISOR).max(ACCOUNT_MAX_MEM_FLOOR_BYTES)
}

/// One subject list, NATS-config-array-formatted: `["a", "b"]`, or `[]`
/// for an empty list (valid syntax — an account with no PURGE grants, or
/// no denials, renders one).
fn fmt_subject_list(subjects: &[String]) -> String {
    let quoted: Vec<String> = subjects.iter().map(|s| format!("{s:?}")).collect();
    format!("[{}]", quoted.join(", "))
}

/// The clamped per-namespace file quota (2.5d Task 8a, ADR 0061 §6): the
/// SUM of `peers`' individual `quota_bytes`, clamped at `ceiling_bytes`
/// — "the tier ceiling clamping the SUM rather than each term". A
/// namespace whose declared claims sum past the ceiling gets exactly the
/// ceiling, once, not each claim silently capped before summing (the
/// wrong semantics a caller outside this module would be forced into —
/// `render_account` is the only place that sees the whole namespace's
/// `peers` at once, so this is the only place that CAN sum before
/// clamping). Also the input `render_accounts_file` sums ACROSS
/// namespaces for Task 8b's global-budget check, so the two ceilings
/// compose: this function's return value is exactly what each account's
/// `max_file` promises, and the global check is a sum of promises, not
/// of raw requests.
///
/// `pub(crate)` for 2.5f: `nats::quota_verdict`'s `QuotaExceeded` pre-flight
/// (ADR 0061 §6) must check declarations against the SAME number this
/// renders into the account's `max_file`, or the pre-flight would refuse
/// streams the server would have accepted (or, worse, pass ones it will
/// not). One derivation, two readers — never a second copy of the
/// sum-then-clamp order, which is the part that is easy to get backwards.
pub(crate) fn namespace_quota_bytes(peers: &[ClaimView], ceiling_bytes: u64) -> u64 {
    peers
        .iter()
        .map(|c| c.quota_bytes)
        .sum::<u64>()
        .min(ceiling_bytes)
}

/// One namespace's account block: the `jetstream` limits, the management
/// user (full access, including the SHARED `_INBOX.>` tree — it sets no
/// custom inbox prefix of its own, ADR 0061 §3, so denying it `_INBOX.>`
/// would stop it making a single request), then one user per claim in
/// `peers`, each carrying that claim's own [`allow_list`]/[`deny_vector`]
/// as its publish permissions and a subscribe grant scoped to its own
/// subject and inbox prefixes only — never the shared `_INBOX.>` tree,
/// which would let it read every other claim's in-flight replies.
///
/// Takes `peers` rather than the whole file's claims so it is
/// independently unit-testable against a deliberately empty slice:
/// [`render_accounts_file`] can never call this with an empty `peers` for
/// any namespace it derives from `claims` (grouping only ever inserts a
/// namespace key alongside its first claim), so THIS function's own
/// `peers.is_empty()` check — before any indexing, before computing
/// anything that would need `peers[0]` — is otherwise dead code with no
/// test able to reach it through the public API.
fn render_account(
    namespace: &str,
    peers: &[ClaimView],
    ceiling_bytes: u64,
    password: &dyn Fn(&str) -> String,
) -> Result<String, AccountsFileError> {
    let account = account_name(namespace);

    if peers.is_empty() {
        return Err(AccountsFileError::EmptyAccount(account));
    }

    let mgr = mgr_user(namespace);
    let mgr_pw = password(&mgr);
    if mgr_pw.is_empty() {
        // This and the per-claim check below are separate guards over
        // separate users; a fixture that empties EVERY password at once
        // (as `refuses_to_render_a_file_that_would_disable_authentication`
        // does) cannot tell which one fired if the other is broken — each
        // is isolated by its own test
        // (`refuses_to_render_when_only_the_manager_password_is_empty` /
        // `..._a_claim_users_password_is_empty`), found necessary only by
        // mutation-testing this check on its own.
        return Err(AccountsFileError::EmptyPassword(mgr));
    }

    let total_quota = namespace_quota_bytes(peers, ceiling_bytes);
    let max_mem = account_max_mem_bytes(total_quota);

    let mut out = String::new();
    writeln!(out, "{account}: {{").unwrap();
    writeln!(
        out,
        "  jetstream: {{ max_mem: {max_mem}, max_file: {total_quota} }}"
    )
    .unwrap();
    writeln!(out, "  users: [").unwrap();
    writeln!(out, "    {{").unwrap();
    writeln!(out, "      user: {mgr:?}").unwrap();
    writeln!(out, "      password: {mgr_pw:?}").unwrap();
    writeln!(out, "      permissions: {{").unwrap();
    writeln!(out, "        publish: {{ allow: [\">\"] }}").unwrap();
    writeln!(out, "        subscribe: {{ allow: [\">\", \"_INBOX.>\"] }}").unwrap();
    writeln!(out, "      }}").unwrap();
    writeln!(out, "    }}").unwrap();

    for c in peers {
        let user = c.user();
        let pw = password(&user);
        if pw.is_empty() {
            // See the mgr-level check above — same reasoning, isolated by
            // `refuses_to_render_when_only_a_claim_users_password_is_empty`.
            return Err(AccountsFileError::EmptyPassword(user));
        }
        let allow = allow_list(c);
        let deny = deny_vector(c, peers);
        let subscribe_own = format!("{}>", c.subject_prefix());
        let subscribe_inbox = format!("{}.>", c.inbox_prefix());

        writeln!(out, "    {{").unwrap();
        writeln!(out, "      user: {user:?}").unwrap();
        writeln!(out, "      password: {pw:?}").unwrap();
        writeln!(out, "      permissions: {{").unwrap();
        writeln!(out, "        publish: {{").unwrap();
        writeln!(out, "          allow: {}", fmt_subject_list(&allow)).unwrap();
        writeln!(out, "          deny: {}", fmt_subject_list(&deny)).unwrap();
        writeln!(out, "        }}").unwrap();
        writeln!(
            out,
            "        subscribe: {{ allow: [{subscribe_own:?}, {subscribe_inbox:?}] }}"
        )
        .unwrap();
        writeln!(out, "      }}").unwrap();
        writeln!(out, "    }}").unwrap();
    }

    writeln!(out, "  ]").unwrap();
    writeln!(out, "}}").unwrap();
    Ok(out)
}

/// Renders the WHOLE NATS accounts fragment from the live claim set (ADR
/// 0061 §3) — one [`render_account`] block per namespace, grouped and
/// sorted deterministically (`BTreeMap` by namespace, then each
/// namespace's claims explicitly sorted by app — a `BTreeMap` only
/// orders its KEYS, not what gets pushed into each value) so an
/// unchanged derivation produces byte-identical output regardless of
/// `claims`' input order: the property that lets a caller skip the write
/// when nothing changed.
///
/// Emits no `SYS` account: ADR 0061 §2 puts that in the chart's
/// statically-owned base `nats.conf`, which `include`s this fragment —
/// this function's job stops at the accounts the LIVE CLAIM SET
/// produces.
///
/// **The output IS wrapped in `accounts: { ... }`** — walk-found (2.5d
/// part-2 closure, first live kind cluster run): `component_nats.cue`'s
/// `config.merge`'s `accounts$include` key splices this fragment's
/// `include` at the TOP LEVEL of `nats.conf`, sibling to `jetstream` /
/// `feature_flags` / `port` — confirmed by rendering the actual chart
/// (`helm template nats nats/nats`), whose OWN rendered config defines no
/// `accounts: {}` key anywhere. This fragment is therefore the SOLE
/// source of that key; emitting bare `ns_demo: { ... }` at that spliced
/// position is a naked, unrecognised top-level field, and nats-server
/// refuses it outright: `.../accounts.conf:1:0: unknown field "ns_demo"`,
/// observed verbatim on a real cluster before this wrapper existed.
/// `the_rendered_file_is_wrapped_in_an_accounts_block` (this module's own
/// test) pins the shape structurally, independent of any content-only
/// assertion — see that test's own doc for why a content-only check
/// could not have caught the miss.
///
/// Two ceilings, two different scopes (2.5d Task 8, ADR 0061 §6 and its
/// round-7 review follow-up — both are values passed in, never read
/// from anywhere, so this function stays pure):
///
/// - `namespace_ceiling_bytes` bounds ONE namespace's account — the
///   `#Size`-mapping tier ceiling, clamping the SUM of that namespace's
///   claims (never each term; see [`namespace_quota_bytes`]).
/// - `global_budget_bytes` bounds the SUM ACROSS EVERY namespace this
///   call renders — nothing enforced this before Task 8b existed, so N
///   namespaces could jointly promise more `max_file`/`max_mem` than the
///   server's shared JetStream PVC / `max_memory_store` actually has.
///   NATS does not refuse this itself: the accounts compete for
///   capacity promised twice, surfacing as an unattributable allocation
///   failure in whoever asks last. This function REFUSES instead — a
///   tenant that cannot have what it asked for should be told, not
///   silently handed less than its manifest says (see
///   [`AccountsFileError::GlobalBudgetExceeded`]'s own doc for why a
///   proportional clamp was rejected). Checked BEFORE any namespace is
///   rendered, against the SUM of each namespace's ALREADY-clamped
///   `namespace_quota_bytes` — the actual promises the file is about to
///   make, not the raw pre-clamp requests.
///
/// `password` is injected rather than read (e.g. from a Kubernetes
/// Secret) so this function stays pure — no client, no network, no clock
/// — and its tests need no fixtures.
pub fn render_accounts_file(
    claims: &[ClaimView],
    namespace_ceiling_bytes: u64,
    global_budget_bytes: u64,
    password: &dyn Fn(&str) -> String,
) -> Result<String, AccountsFileError> {
    let mut by_namespace: BTreeMap<String, Vec<ClaimView>> = BTreeMap::new();
    for c in claims {
        by_namespace
            .entry(c.namespace.clone())
            .or_default()
            .push(c.clone());
    }

    let global_total: u64 = by_namespace
        .values()
        .map(|peers| namespace_quota_bytes(peers, namespace_ceiling_bytes))
        .sum();
    if global_total > global_budget_bytes {
        return Err(AccountsFileError::GlobalBudgetExceeded {
            total: global_total,
            budget: global_budget_bytes,
        });
    }

    let mut out = String::new();
    writeln!(out, "accounts: {{").unwrap();
    for (namespace, mut peers) in by_namespace {
        peers.sort_by(|a, b| a.app.cmp(&b.app));
        out.push_str(&render_account(
            &namespace,
            &peers,
            namespace_ceiling_bytes,
            password,
        )?);
    }
    writeln!(out, "}}").unwrap();
    Ok(out)
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
            retention: None,
            max_bytes: "1Gi".into(),
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
    fn dynamic_stream_verbs_and_purge_also_land_at_depth_5_or_6() {
        // Round-7 review coverage gap: the test above iterates
        // allow_list(&claim(...)) with dynamic_streams: false and no
        // streams, so it never walks the four entries gated by
        // `me.dynamic_streams` or the `allow_purge`-gated
        // STREAM.PURGE.<name> entry — 17 of 22 possible allow_list
        // entries, not all 22. Same depth formula as the test above,
        // applied to a fixture that turns every one of those on, so a
        // future entry added inside either branch can't slip past
        // silently the way the ones already there could have.
        let mut c = claim("demo", "feeder");
        c.dynamic_streams = true;
        c.streams = vec![StreamView {
            name: "blocks-head".into(),
            subjects: vec!["feeder.blocks.head.>".into()],
            allow_purge: true,
            retention: None,
            max_bytes: "1Gi".into(),
        }];
        for subj in allow_list(&c) {
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
                "{subj} puts the stream token at depth {depth}; the deny \
                 patterns cover 5 and 6 only."
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
    fn a_declared_streams_create_is_denied_even_under_dynamic_streams() {
        // Round-7 review (H2, the pre-emption finding, reproduced against
        // nats-server 2.14.3): allow_list grants $JS.API.STREAM.CREATE.>
        // under dynamic_streams, and class (B) denied
        // UPDATE/DELETE/MSG.DELETE on every declared stream but not
        // CREATE. A dynamic_streams app could therefore squat a
        // namespace-mate's DECLARED stream name — with subjects of its
        // own choosing, since subjects are not permission-checked at
        // creation — before NACK ever created the real one; NACK's later
        // create then fails 10058 and §5's detector may read the
        // squatted stream as legitimate. Works on the app's own declared
        // names too, with no second gate.
        let mut all = ns_with_two_apps();
        all[0].dynamic_streams = true; // feeder, the stream's OWNER
        let allow = allow_list(&all[0]);
        assert!(
            allow.contains(&"$JS.API.STREAM.CREATE.>".to_string()),
            "dynamic_streams must still grant CREATE broadly — the \
             boundary belongs in the deny vector, not the allow list"
        );
        let d = deny_vector(&all[0], &all);
        assert!(
            d.contains(&"$JS.API.STREAM.CREATE.feeder_blocks-head".to_string()),
            "but the declared stream's own composed name must be denied, \
             same as UPDATE/DELETE/MSG.DELETE: {d:?}"
        );
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

    #[test]
    fn renders_one_account_per_namespace_with_a_user_per_claim() {
        let mut claims = ns_with_two_apps();
        claims.push(claim("other", "solo"));
        let out = render_accounts_file(&claims, u64::MAX, u64::MAX, &|u| format!("pw-{u}"))
            .expect("renders");
        assert!(out.contains("ns_demo: {"), "{out}");
        assert!(out.contains("ns_other: {"), "{out}");
        assert!(
            out.contains("user: \"claim_demo_feeder_jetstream\""),
            "{out}"
        );
        assert!(
            out.contains("user: \"claim_demo_indexer_jetstream\""),
            "{out}"
        );
        assert!(out.contains("user: \"mgr_demo\""), "{out}");
        assert!(out.contains("user: \"mgr_other\""), "{out}");
    }

    #[test]
    fn the_rendered_file_is_wrapped_in_an_accounts_block() {
        // Walk-found bug (2.5d part-2 closure, live kind cluster):
        // `nats-server: /etc/nats-config/accounts-secret/accounts.conf:1:0:
        // unknown field "ns_demo"`. The chart's `config.merge`
        // (`component_nats.cue`) splices this fragment's `include` at the
        // TOP LEVEL of nats.conf, sibling to `jetstream`/`feature_flags`/
        // `port` — confirmed by rendering the real chart
        // (`helm template nats nats/nats`), which emits no `accounts: {}`
        // key of its own anywhere. So this fragment is the ONLY source of
        // that key, and a bare `ns_demo: { ... }` at that position is a
        // naked, unrecognised top-level field — exactly the walk's error.
        //
        // A content-only assertion (`.contains("ns_demo: {")`, this
        // module's own long-standing style, unchanged above) cannot see a
        // missing WRAPPER — `ns_demo: {` is equally present whether or not
        // it sits inside `accounts: { ... }`. That is precisely why the
        // whole existing suite passed while this shipped broken; this is
        // a STRUCTURAL assertion, deliberately independent of it.
        let out = render_accounts_file(&ns_with_two_apps(), u64::MAX, u64::MAX, &|u| {
            format!("pw-{u}")
        })
        .unwrap();
        assert!(
            out.starts_with("accounts: {"),
            "the fragment must supply the WHOLE `accounts` key (the chart's \
             nats.conf defines none of its own) — got:\n{out}"
        );
        assert!(
            out.trim_end().ends_with('}'),
            "the accounts wrapper must be closed: {out}"
        );
    }

    #[test]
    fn the_management_user_keeps_inbox_access() {
        // NACK sets no custom inbox prefix; denying it `_INBOX.>` would stop
        // it making a single request (ADR 0061 §3).
        let out = render_accounts_file(&ns_with_two_apps(), u64::MAX, u64::MAX, &|u| {
            format!("pw-{u}")
        })
        .unwrap();
        let mgr = out.split("user: \"mgr_demo\"").nth(1).expect("mgr block");
        assert!(mgr.contains("_INBOX.>"), "mgr must keep _INBOX.>: {mgr}");
    }

    #[test]
    fn a_claim_user_subscribes_only_to_its_own_prefixes() {
        let out = render_accounts_file(&ns_with_two_apps(), u64::MAX, u64::MAX, &|u| {
            format!("pw-{u}")
        })
        .unwrap();
        let block = out
            .split("user: \"claim_demo_feeder_jetstream\"")
            .nth(1)
            .unwrap();
        let sub = block
            .split("subscribe")
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap();
        assert!(sub.contains("feeder.>"));
        assert!(sub.contains("_INBOX_demo_feeder.>"));
        assert!(
            !sub.contains("\"_INBOX.>\""),
            "the shared inbox tree must not be granted"
        );
    }

    #[test]
    fn output_is_byte_stable_across_input_order() {
        let a = ns_with_two_apps();
        let mut b = a.clone();
        b.reverse();
        assert_eq!(
            render_accounts_file(&a, u64::MAX, u64::MAX, &|u| format!("pw-{u}")).unwrap(),
            render_accounts_file(&b, u64::MAX, u64::MAX, &|u| format!("pw-{u}")).unwrap(),
            "an unchanged derivation must compare equal so the write can be skipped"
        );
    }

    #[test]
    fn refuses_to_render_a_file_that_would_disable_authentication() {
        let err = render_accounts_file(&ns_with_two_apps(), u64::MAX, u64::MAX, &|_| String::new())
            .unwrap_err();
        assert!(err.to_string().contains("empty password"), "{err}");
    }

    #[test]
    fn refuses_to_render_when_only_the_manager_password_is_empty() {
        // The mgr-level and per-claim `EmptyPassword` checks are separate
        // code paths, but the fixture above sets EVERY password empty at
        // once — bypassing either check alone still leaves the OTHER one
        // firing, so that test cannot tell them apart (confirmed by
        // mutation: disabling the mgr-only check turned zero tests red
        // while that fixture stayed in place). This isolates the mgr-only
        // check: every claim user gets a real password, only `mgr_demo`'s
        // is empty.
        let err = render_accounts_file(&ns_with_two_apps(), u64::MAX, u64::MAX, &|u| {
            if u == "mgr_demo" {
                String::new()
            } else {
                format!("pw-{u}")
            }
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("mgr_demo"),
            "must name the manager user specifically: {err}"
        );
    }

    #[test]
    fn refuses_to_render_when_only_a_claim_users_password_is_empty() {
        // Symmetric isolation for the per-claim check: mgr and every OTHER
        // claim user get a real password, only one claim user's is empty.
        let err = render_accounts_file(&ns_with_two_apps(), u64::MAX, u64::MAX, &|u| {
            if u == "claim_demo_feeder_jetstream" {
                String::new()
            } else {
                format!("pw-{u}")
            }
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("claim_demo_feeder_jetstream"),
            "must name the claim user specifically: {err}"
        );
    }

    #[test]
    fn account_quota_is_the_sum_over_the_namespace() {
        let mut claims = ns_with_two_apps();
        claims[0].quota_bytes = 1 << 30;
        claims[1].quota_bytes = 2 << 30;
        let out =
            render_accounts_file(&claims, u64::MAX, u64::MAX, &|u| format!("pw-{u}")).unwrap();
        assert!(out.contains("max_file: 3221225472"), "{out}");
    }

    #[test]
    fn account_max_mem_is_a_nonzero_fraction_of_the_file_quota() {
        // Round-7 review (H3): ACCOUNT_MAX_MEM_BYTES was a flat 0, making
        // `storage: memory` fail with an opaque `10028 insufficient
        // memory resources` from nats-server — despite the CUE type
        // offering it and the webhook's own rejection message for
        // `persistent` recommending it BY NAME as the per-stream
        // persistence knob.
        let mut claims = ns_with_two_apps();
        for c in &mut claims {
            c.quota_bytes = 10 * (1 << 30); // 10 GiB each, well above the floor
        }
        let total_quota: u64 = claims.iter().map(|c| c.quota_bytes).sum();
        let out =
            render_accounts_file(&claims, u64::MAX, u64::MAX, &|u| format!("pw-{u}")).unwrap();
        let max_mem: u64 = out
            .split("max_mem: ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(max_mem > 0, "max_mem must be non-zero: {out}");
        assert_eq!(
            max_mem,
            total_quota / 10,
            "must be exactly the documented fraction of the summed file quota"
        );
    }

    #[test]
    fn account_max_mem_floors_for_a_tiny_quota() {
        let mut claims = ns_with_two_apps();
        for c in &mut claims {
            c.quota_bytes = 1024; // tiny — the fraction alone would be ~200 bytes
        }
        let out =
            render_accounts_file(&claims, u64::MAX, u64::MAX, &|u| format!("pw-{u}")).unwrap();
        let max_mem: u64 = out
            .split("max_mem: ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            max_mem, ACCOUNT_MAX_MEM_FLOOR_BYTES,
            "a tiny file quota must not leave storage: memory unusable: {out}"
        );
    }

    #[test]
    fn the_namespace_sum_is_clamped_at_the_ceiling_not_each_claim() {
        // 2.5d Task 8a (ADR 0061 §6): the tier ceiling clamps the SUM of
        // a namespace's claims, not each term. render_account computes
        // that sum internally from `peers`, so a caller could only ever
        // clamp each `quota_bytes` before passing it in — the wrong
        // semantics, and not fixable at the call site; the ceiling has
        // to be threaded through and applied to the sum itself. Three
        // 1Gi claims under a 2Gi ceiling must render `max_file: 2Gi` —
        // not 3Gi (unclamped) and not 1Gi (a per-claim clamp would
        // produce this only if it clamped and then divided, but the
        // exact-equality assertion below pins the one correct answer
        // regardless of which wrong formula a bad implementation used).
        let claims = vec![claim("demo", "a"), claim("demo", "b"), claim("demo", "c")];
        let ceiling = 2 * (1u64 << 30); // 2Gi; each claim defaults to 1Gi
        let out = render_accounts_file(&claims, ceiling, u64::MAX, &|u| format!("pw-{u}"))
            .expect("renders");
        assert!(out.contains(&format!("max_file: {ceiling}")), "{out}");
    }

    #[test]
    fn the_namespace_ceiling_does_not_affect_a_sum_already_under_it() {
        // The other half of 8a's clamp: a sum that already fits must
        // pass through unchanged, not always collapse to the ceiling
        // (which a backwards `max()` instead of `min()` would do
        // without any of the "exceeds the ceiling" tests noticing —
        // they only ever check the OVER case).
        let claims = ns_with_two_apps(); // demo: 2 claims, 1Gi each = 2Gi
        let out =
            render_accounts_file(&claims, 10 * (1u64 << 30), u64::MAX, &|u| format!("pw-{u}"))
                .expect("renders");
        assert!(
            out.contains(&format!("max_file: {}", 2 * (1u64 << 30))),
            "{out}"
        );
    }

    #[test]
    fn refuses_to_render_when_the_global_sum_exceeds_the_platform_budget() {
        // 2.5d Task 8b: the per-namespace ceiling (8a) bounds ONE
        // account; nothing bounds N namespaces together. Two namespaces
        // — "demo" (2Gi, unclamped by a generous per-namespace ceiling)
        // and "other" (3Gi) — sum to 5Gi, over a 4Gi platform budget.
        let mut claims = ns_with_two_apps(); // demo: 2 claims, 1Gi each = 2Gi
        let mut solo = claim("other", "solo");
        solo.quota_bytes = 3 * (1u64 << 30); // 3Gi
        claims.push(solo);
        let budget = 4 * (1u64 << 30);
        let err =
            render_accounts_file(&claims, u64::MAX, budget, &|u| format!("pw-{u}")).unwrap_err();
        assert!(err.to_string().contains("platform-wide budget"), "{err}");
    }

    #[test]
    fn a_render_that_fits_the_global_budget_is_unaffected() {
        let claims = ns_with_two_apps(); // demo: 2 claims, 1Gi each = 2Gi
        let out =
            render_accounts_file(&claims, u64::MAX, 10 * (1u64 << 30), &|u| format!("pw-{u}"))
                .expect("renders — the budget is not exceeded");
        assert!(out.contains("ns_demo: {"), "{out}");
    }

    #[test]
    fn the_account_key_matches_claimview_account_for_a_hyphenated_namespace() {
        // Round-7 review coverage gap: render_account independently
        // re-derived the "ns_<namespace>" fold instead of calling
        // ClaimView::account() — deleting render_account's OWN
        // `.replace('-', '_')` left the whole suite green, because every
        // existing render-path fixture used a hyphen-free namespace
        // ("demo", "other"), so the two independent implementations never
        // had a chance to disagree.
        //
        // The expected key is HARDCODED, not derived via `c.account()`:
        // an earlier draft of this test computed `expected_key` by
        // calling `c.account()`, so a mutation to the shared fold (both
        // call sites now route through `account_name`) moved BOTH sides
        // together and this test never went red — caught only because
        // the mutation-testing pass ran it and got zero failures, the
        // same shape as the Task 7 `allow_purge` and Task 8 mgr-password
        // findings. Asserting against a literal locks down the render
        // path independently of whatever `account_name` currently
        // computes.
        let c = claim("demo-ns", "app");
        let out = render_accounts_file(&[c], u64::MAX, u64::MAX, &|u| format!("pw-{u}")).unwrap();
        assert!(
            out.contains("ns_demo_ns: {"),
            "expected key \"ns_demo_ns: {{\" in:\n{out}"
        );
    }

    #[test]
    fn render_account_refuses_an_empty_namespace() {
        // Not reachable through render_accounts_file's public API: its
        // BTreeMap grouping only ever inserts a namespace key alongside
        // its first claim, so `peers.is_empty()` can never be true for
        // any namespace it derives from `claims`. Tested directly
        // against `render_account` (this module's own private helper)
        // instead — the "check emptiness before indexing" guard is
        // defensive coding for whatever calls this function next, not
        // dead code no test can reach.
        let err = render_account("demo", &[], u64::MAX, &|u| format!("pw-{u}")).unwrap_err();
        assert!(err.to_string().contains("no users"), "{err}");
    }

    /// Unit tests above assert SHAPE (does the output contain the right
    /// substrings); only `nats-server` itself accepts or rejects GRAMMAR,
    /// and a file that parses while being wrong is this design's
    /// characteristic failure (ADR 0042 §10) — so this is the one check
    /// in the module that actually asks the server, not the string.
    ///
    /// Builds a small standalone `nats.conf` around `render_accounts_file`'s
    /// output in a temp dir and BOOTS a real `nats:2-alpine` server
    /// against it, waiting for the server's own `Server is ready` banner.
    ///
    /// **It boots the server rather than running `nats-server -t`, and
    /// that is not a stylistic preference.** `-t` is a config-file
    /// *parse* check; several whole-server invariants are evaluated only
    /// at startup, so it can pass on a config the deployed server then
    /// refuses. Measured during 2.5e, on a config shape this module was
    /// briefly about to produce: a `no_auth_user` naming a user no
    /// account defines passes `-t` with `configuration file … is valid`
    /// and then exits 1 at boot with `present, but users/nkeys are not
    /// defined`. A `-t`-based version of this test stayed GREEN through a
    /// deliberate mutation that a boot caught immediately — the exact
    /// class of silently-non-matching check this file's own comments keep
    /// warning about. Asking the question the deployed server asks is
    /// this test's entire job.
    ///
    /// **The `include` sits at TOP LEVEL, mirroring `component_nats.cue`'s
    /// `config.merge`'s `accounts$include` key exactly — it must NOT
    /// drift from that placement.** Confirmed by rendering the actual
    /// chart (`helm template nats nats/nats`): its own `nats.conf` has no
    /// `accounts: {}` key of its own anywhere, and no `system_account`
    /// either — `include ./accounts-secret/accounts.conf;` is a bare
    /// top-level statement, sibling to `jetstream`/`feature_flags`/`port`.
    /// This harness used to nest the include inside its OWN
    /// `accounts: { "$SYS": {...}, include "..." }` block — which made a
    /// BARE fragment (no `accounts` wrapper of its own) look valid here
    /// while being rejected by the real chart's placement
    /// (`nats-server: .../accounts.conf:1:0: unknown field "ns_demo"`,
    /// reproduced verbatim on a live kind cluster before this fix). The
    /// two shapes are each internally consistent and disagree with each
    /// other, which is exactly how the walk found a bug this test could
    /// not: whichever one drifts from the chart is the one that lies.
    ///
    /// Run: cargo test -p operator-controllers-resourceclaim-provisioner \
    ///        rendered_file_is_valid_nats_config -- --ignored --nocapture
    #[test]
    #[ignore = "needs podman"]
    fn rendered_file_is_valid_nats_config() {
        // Round-7 review coverage gap: this used to render only
        // ns_with_two_apps() (one namespace, two claims, neither with an
        // empty deny vector). Pushing `other`/`solo` (the same fixture
        // `renders_one_account_per_namespace_with_a_user_per_claim`
        // uses) exercises two CONCATENATED account blocks — grammar a
        // single-account fragment can't check — and `solo`'s own
        // `deny: []` (it shares no namespace claims with anyone, so
        // deny_vector has nothing to add), neither of which nats-server
        // had been asked to parse before.
        let mut claims = ns_with_two_apps();
        claims.push(claim("other", "solo"));
        // Both shapes, in one test: the populated render, and the
        // ZERO-CLAIM render — the state the file lands in when the
        // cluster's last jetstream claim goes away, and one the walk
        // reaches every time it tears its fixtures down. `accounts: {}`
        // with nothing inside is a shape no unit assertion in this module
        // has any opinion about; whether a server will BOOT on it is a
        // question only the server answers.
        for (label, claims) in [
            ("populated", claims.as_slice()),
            ("zero-claim", [].as_slice()),
        ] {
            let fragment = render_accounts_file(claims, u64::MAX, u64::MAX, &|u| format!("pw-{u}"))
                .expect("renders");

            // Mirrors `component_nats.cue`'s `config.merge` EXACTLY: the
            // `include` is a bare TOP-LEVEL statement, sibling to
            // `jetstream`/`feature_flags`/`port`. Nothing else is added
            // here — in particular NO `no_auth_user`, which ADR 0061 §2
            // forbids; a harness that quietly added one would make this
            // test agree with a config the chart does not render, and
            // would hide the very failure the boot is here to catch.
            let nats_conf = r#"
port: 4222
jetstream: {
  store_dir: "/tmp/nats-check-store"
}
include "accounts.conf"
"#
            .to_string();

            let dir = std::env::temp_dir().join(format!(
                "nats-accounts-check-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            std::fs::write(dir.join("nats.conf"), &nats_conf).expect("write nats.conf");
            std::fs::write(dir.join("accounts.conf"), &fragment).expect("write accounts.conf");

            let mount = format!("{}:/etc/nats:ro,Z", dir.display());
            let container = format!("nats-accounts-check-{label}-{}", std::process::id());
            let _ = std::process::Command::new("podman")
                .args(["rm", "-f", &container])
                .output();
            let run = std::process::Command::new("podman")
                .args([
                    "run",
                    "-d",
                    "--name",
                    &container,
                    "-v",
                    &mount,
                    "nats:2-alpine",
                    "-c",
                    "/etc/nats/nats.conf",
                ])
                .output()
                .expect("run podman — is it installed?");
            assert!(
                run.status.success(),
                "podman run failed: {}",
                String::from_utf8_lossy(&run.stderr)
            );

            // Poll for the server's own readiness banner, or for the
            // container having exited (a refused config exits 1 almost
            // immediately — no need to wait out the whole budget).
            let mut ready = false;
            let mut logs = String::new();
            for _ in 0..50 {
                let out = std::process::Command::new("podman")
                    .args(["logs", &container])
                    .output()
                    .expect("podman logs");
                logs = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                if logs.contains("Server is ready") {
                    ready = true;
                    break;
                }
                let state = std::process::Command::new("podman")
                    .args(["inspect", &container, "--format", "{{.State.Status}}"])
                    .output()
                    .expect("podman inspect");
                if String::from_utf8_lossy(&state.stdout).trim() == "exited" {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }

            let _ = std::process::Command::new("podman")
                .args(["rm", "-f", &container])
                .output();
            let _ = std::fs::remove_dir_all(&dir);

            assert!(
                ready,
                "nats-server never became ready on the {label} rendered config:\n\
                 --- nats.conf ---\n{nats_conf}\n\
                 --- accounts.conf ---\n{fragment}\n\
                 --- server log ---\n{logs}"
            );
        }
    }
}
