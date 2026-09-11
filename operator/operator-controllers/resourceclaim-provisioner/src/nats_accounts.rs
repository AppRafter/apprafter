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
/// NOT proven collision-free, unlike [`ClaimView::account`] — checked and
/// found wanting: `owner_app` is a Kubernetes object name and `declared`
/// is `#JetStreamStream.name` (an unconstrained CUE `string` — no
/// DNS-1123 or other format rule anywhere in CUE/CRD/webhook today), and
/// BOTH may contain `-`, which is also the join character here. The
/// encoding is not self-delimiting: `("a", "b-c")` and `("a-b", "c")`
/// both produce `"a-b-c"`. Two different applications in one
/// namespace/account could therefore be assigned the same NATS stream
/// name for two different declared streams — NATS itself has no
/// per-application sub-namespace, so this is a real collision, not a
/// cosmetic one. Left exactly as specified for this task: closing it
/// would mean changing the encoding (a reserved separator, a format
/// constraint on declared names, or an explicit collision check), which
/// would also change every string this module's tests pin — a decision
/// for whichever task owns provisioning, not this one.
pub fn nats_stream_name(owner_app: &str, declared: &str) -> String {
    format!("{owner_app}-{declared}")
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
/// asymmetry is the point. Carries the SAME unproven-collision-freedom
/// caveat as `nats_stream_name` above, for the same reason (both operands
/// can contain `-`).
pub fn nats_durable_name(consumer_app: &str, declared: &str) -> String {
    format!("{consumer_app}-{declared}")
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
            "feeder-blocks-head"
        );
        // the durable carries the CONSUMING application, so two applications
        // may each hold `indexer` on one shared stream without colliding
        assert_eq!(
            nats_durable_name("indexer-app", "indexer"),
            "indexer-app-indexer"
        );
    }
}
