// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Self-heal backfill helpers and the split fact-drift / deferred-intent guard
//! for `apprafter apply`. (2.16h)
//!
//! ## Responsibilities
//!
//! * [`backfill_from`] — given a live server list and a known `server_id`,
//!   returns the `(server_type, region)` observed on that server so callers
//!   can persist them into `HetznerCloudState` (fact) and/or `TargetConfig`
//!   (preference) when either is currently `None`.
//!
//! * [`classify_guard`] — classifies the relationship between the recorded
//!   fact (`state.server_type`), the operator's preference
//!   (`target.server_type`), and the live running type, producing one of:
//!   - [`Guard::FactDriftWarn`]: the machine was changed outside AppRafter.
//!   - [`Guard::Intent`]: the preference differs from reality — remind the
//!     operator how to apply it.
//!   - [`Guard::Silent`]: everything matches; nothing to say.
//!
//! Both functions are pure — no I/O, no panics. Tests live at the bottom.

use crate::hetzner_cloud::types::Server;

// ────────────────────────────────────────────────────────────────────────────
// BackfillOutcome
// ────────────────────────────────────────────────────────────────────────────

/// Result of matching a `server_id` against the live server list.
#[derive(Debug, Clone, PartialEq)]
pub enum BackfillOutcome {
    /// No server in the list matched `server_id`. Nothing to adopt.
    Skip,
    /// More than one server matched `server_id` (shouldn't happen in practice;
    /// Hetzner IDs are unique within a project, but the type is constructed
    /// defensively). The caller should warn and skip any write.
    AmbiguousSkip,
    /// Exactly one server matched. The `server_type` and `region` fields are
    /// `Some(name)` when the live API returned them, `None` otherwise.
    Adopt {
        server_type: Option<String>,
        region: Option<String>,
    },
}

/// Resolve the `(server_type, region)` from the server pinned by `server_id`.
///
/// * 0 matches → [`BackfillOutcome::Skip`] (not an error; the project-scoped
///   token may be in a state where the server is temporarily absent from the
///   listing, or the cluster was provisioned through a different token scope).
/// * >1 matches → [`BackfillOutcome::AmbiguousSkip`] (caller should warn).
/// * Exactly 1 match → [`BackfillOutcome::Adopt`] with both optional fields
///   populated from `server.server_type.name` and `server.location.name`.
pub fn backfill_from(servers: &[Server], server_id: u64) -> BackfillOutcome {
    let mut matched: Vec<&Server> = servers.iter().filter(|s| s.id == server_id).collect();
    match matched.len() {
        0 => BackfillOutcome::Skip,
        1 => {
            let s = matched.remove(0);
            BackfillOutcome::Adopt {
                server_type: s.server_type.as_ref().map(|st| st.name.clone()),
                region: s.location.as_ref().map(|loc| loc.name.clone()),
            }
        }
        _ => BackfillOutcome::AmbiguousSkip,
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Guard
// ────────────────────────────────────────────────────────────────────────────

/// Classification returned by [`classify_guard`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Guard {
    /// The *recorded fact* (`state.server_type`) disagrees with the live
    /// running type. The machine was changed outside AppRafter.
    FactDriftWarn,
    /// The *recorded fact* is absent or matches, but the *preference*
    /// (`target.server_type`) differs from the live type. The operator has
    /// asked for a different machine but hasn't re-provisioned yet.
    Intent,
    /// No mismatch — nothing to say.
    Silent,
}

/// Classify the relationship between stored fact, operator preference, and
/// live state.
///
/// Decision table (first matching row wins):
///
/// | `state`     | `target`    | `live` | Result           |
/// |-------------|-------------|--------|------------------|
/// | `Some(s)`, `s != live` | any  | any | `FactDriftWarn` |
/// | None or match | `Some(t)`, `t != live` | any | `Intent` |
/// | anything    | anything    | any    | `Silent`         |
pub fn classify_guard(state: Option<&str>, target: Option<&str>, live: &str) -> Guard {
    if let Some(s) = state {
        if s != live {
            return Guard::FactDriftWarn;
        }
    }
    if let Some(t) = target {
        if t != live {
            return Guard::Intent;
        }
    }
    Guard::Silent
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hetzner_cloud::types::{LocationRef, ServerStatus, ServerTypeRef};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_server(id: u64, type_name: &str, location: &str) -> Server {
        Server {
            id,
            name: format!("s{id}"),
            status: ServerStatus::Running,
            labels: Default::default(),
            public_net: None,
            server_type: Some(ServerTypeRef {
                name: type_name.to_string(),
            }),
            location: Some(LocationRef {
                name: location.to_string(),
            }),
        }
    }

    fn make_server_no_type(id: u64) -> Server {
        Server {
            id,
            name: format!("s{id}"),
            status: ServerStatus::Running,
            labels: Default::default(),
            public_net: None,
            server_type: None,
            location: None,
        }
    }

    // ── backfill_from ─────────────────────────────────────────────────────────

    #[test]
    fn backfill_from_empty_list_is_skip() {
        assert_eq!(backfill_from(&[], 42), BackfillOutcome::Skip);
    }

    #[test]
    fn backfill_from_matching_id_adopts_type_and_region() {
        let servers = vec![
            make_server(1, "cx22", "nbg1"),
            make_server(2, "ccx23", "fsn1"),
        ];
        assert_eq!(
            backfill_from(&servers, 2),
            BackfillOutcome::Adopt {
                server_type: Some("ccx23".to_string()),
                region: Some("fsn1".to_string()),
            }
        );
    }

    #[test]
    fn backfill_from_id_not_present_is_skip() {
        let servers = vec![make_server(10, "cx22", "nbg1")];
        assert_eq!(backfill_from(&servers, 99), BackfillOutcome::Skip);
    }

    #[test]
    fn backfill_from_two_servers_with_same_id_is_ambiguous_skip() {
        // Construct two entries with the same id — shouldn't happen in
        // production but the type must handle it gracefully.
        let servers = vec![
            make_server(7, "cx22", "nbg1"),
            make_server(7, "ccx23", "fsn1"),
        ];
        assert_eq!(backfill_from(&servers, 7), BackfillOutcome::AmbiguousSkip);
    }

    #[test]
    fn backfill_from_server_without_type_adopts_none_fields() {
        let servers = vec![make_server_no_type(5)];
        assert_eq!(
            backfill_from(&servers, 5),
            BackfillOutcome::Adopt {
                server_type: None,
                region: None,
            }
        );
    }

    // ── classify_guard ────────────────────────────────────────────────────────

    #[test]
    fn guard_fact_drift_when_state_differs_from_live() {
        assert_eq!(
            classify_guard(Some("cx22"), Some("ccx43"), "ccx23"),
            Guard::FactDriftWarn
        );
    }

    #[test]
    fn guard_fact_drift_even_when_target_matches_live() {
        // state disagrees → FactDriftWarn regardless of target.
        assert_eq!(
            classify_guard(Some("cx22"), Some("ccx23"), "ccx23"),
            Guard::FactDriftWarn
        );
    }

    #[test]
    fn guard_intent_when_state_none_and_target_differs_from_live() {
        assert_eq!(classify_guard(None, Some("ccx43"), "cx22"), Guard::Intent);
    }

    #[test]
    fn guard_intent_when_state_matches_and_target_differs_from_live() {
        assert_eq!(
            classify_guard(Some("cx22"), Some("ccx43"), "cx22"),
            Guard::Intent
        );
    }

    #[test]
    fn guard_silent_when_state_matches_live() {
        assert_eq!(
            classify_guard(Some("cx22"), Some("cx22"), "cx22"),
            Guard::Silent
        );
    }

    #[test]
    fn guard_silent_when_all_none() {
        assert_eq!(classify_guard(None, None, "cx22"), Guard::Silent);
    }

    #[test]
    fn guard_silent_when_state_matches_and_target_none() {
        assert_eq!(classify_guard(Some("cx22"), None, "cx22"), Guard::Silent);
    }

    #[test]
    fn guard_silent_when_state_none_and_target_matches_live() {
        assert_eq!(classify_guard(None, Some("cx22"), "cx22"), Guard::Silent);
    }
}
