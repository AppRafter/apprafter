// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The latest reconcile the operator abandoned at its deadline (WI-400), for
//! the three status commands whose object the resource-claim controllers
//! reconcile: `app status` (under its claims table), `volume status` and
//! `db status`.
//!
//! # Where the fact lives
//!
//! A pass that runs past its deadline is abandoned and writes nothing to the
//! object's status: a resource-claim provisioner pass after 120s for a claim
//! or a SharedDatabase and 60s for a SharedVolume, a resource-claim
//! scheduler pass after 120s for a claim. Every status write there is a
//! full-body server-side apply, so a "timed out" body would prune whatever it
//! omitted, the claim's allocation included. What the pass leaves instead is
//! a `Warning` Event, reason [`REASON`], regarding the object, whose
//! `reportingController` names the controller whose pass was cut. There is
//! nothing on the object to read, so these commands read that Event.
//!
//! # What the line says, and for how long
//!
//! The newest such Event about the object, with its age: "last timed out 3
//! minutes ago (did not finish within 120s)". It prints until the controller
//! that reported it writes the object's status again, or until the apiserver
//! expires the Event (`--event-ttl`, one hour by default), whichever comes
//! first. A stall that persists writes no status and times out again on the
//! next try (the provisioner retries one deadline after a timeout, the
//! scheduler 30 seconds after), so it leaves a fresh Event every few minutes
//! and its age stays short.
//!
//! The status write is read from the object's `metadata.managedFields`
//! ([`status_written_since`]): a write to the `status` subresource, under
//! one of the reporting controller's own field managers, in a later second
//! than the Event. The managers are matched by their exact names, never by
//! prefix. The ACL resync loop writes a claim's key count under the
//! provisioner's `-size` field manager on its own schedule, and a timed-out
//! claim pass pokes that loop just before it publishes the Event: a line
//! that went quiet on that write would hide a stall that persists, first on
//! the Dragonfly claims WI-402 is about. Another controller's write says
//! nothing about the pass either.
//!
//! Where no such write can be read, the line stays: an Event without a
//! reporter this CLI knows, an object read without its `managedFields`, a
//! write in the Event's own second. A pass that finishes without changing
//! the status moves no `managedFields` time either, so after such a
//! recovery the line stays until the Event expires, and its age keeps
//! growing.
//!
//! An Event about an earlier object of the same name does not count: when
//! the Event's `regarding.uid` and the object's uid are both present, they
//! must match.
//!
//! # Quiet otherwise
//!
//! An operator from before WI-400, an object with no timeout in the Event's
//! lifetime, a reader without `list` on `events.events.k8s.io`, an apiserver
//! that refuses the field selector: each one prints nothing (a failed read
//! is logged at `debug`). The status commands already say so when the
//! cluster cannot be reached, so a second failure line for the same
//! apiserver would add nothing. A permission warning on every `status` run
//! for a reader who may not list Events would be the kind of noise those
//! commands were trimmed of (WI-394).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tracing::debug;

use crate::commands::helper_interrupt::refuse_if_interrupted;

/// The reason of the Event the provisioner leaves on an object whose pass it
/// abandoned (`deadline_event::REASON` there; a test below reads it).
pub(crate) const REASON: &str = "ReconcileTimedOut";

/// `regarding.kind` of a SharedVolume's Events.
pub(crate) const SHARED_VOLUME_KIND: &str = "SharedVolume";
/// `regarding.kind` of a SharedDatabase's Events.
pub(crate) const SHARED_DATABASE_KIND: &str = "SharedDatabase";

/// The group every kind above belongs to, matched on `regarding.apiVersion`.
/// A kind alone is not unique: Kubernetes 1.32+ serves a DRA
/// `ResourceClaim` of its own (`resource.k8s.io`), and the field selector
/// matches the kind only.
const GROUP_PREFIX: &str = "apprafter.io/";

/// Group-qualified. Bare `events` is the core/v1 view of the same objects,
/// and it names the fields below differently (`involvedObject.*`).
const EVENTS_RESOURCE: &str = "events.events.k8s.io";

/// `reportingController` of the provisioner's Events, about a claim, a
/// SharedVolume or a SharedDatabase (its `REPORTER_CONTROLLER`; a test below
/// reads it).
const PROVISIONER_REPORTER: &str = "apprafter-resourceclaim-provisioner";

/// `reportingController` of the scheduler's Events, about a claim (its
/// `reconcile::EVENT_REPORTER_CONTROLLER`; a test below reads it).
const SCHEDULER_REPORTER: &str = "apprafter-resourceclaim-scheduler";

/// The field managers under which the provisioner writes an object's status
/// from inside that object's own pass:
/// - `resourceclaim-provisioner`, its `FIELD_MANAGER`: a claim's status, and
///   a SharedVolume's and a SharedDatabase's (the `apply_params()` of
///   `shared_volume.rs` and of `shared_database.rs`);
/// - `resourceclaim-provisioner-jetstream`, its `JETSTREAM_FIELD_MANAGER`: a
///   jetstream claim's inventory, refreshed inside the claim's pass.
///
/// Not `resourceclaim-provisioner-size`: the ACL resync loop writes under
/// it outside any pass (see the module docs).
const PROVISIONER_STATUS_MANAGERS: &[&str] = &[
    "resourceclaim-provisioner",
    "resourceclaim-provisioner-jetstream",
];

/// The field manager under which the scheduler writes a claim's status (its
/// `FIELD_MANAGER`).
const SCHEDULER_STATUS_MANAGERS: &[&str] = &["resourceclaim-scheduler"];

/// The status field managers of the controller an Event names as its
/// reporter. `None` for a reporter this CLI does not know.
fn status_managers(reporter: &str) -> Option<&'static [&'static str]> {
    match reporter {
        PROVISIONER_REPORTER => Some(PROVISIONER_STATUS_MANAGERS),
        SCHEDULER_REPORTER => Some(SCHEDULER_STATUS_MANAGERS),
        _ => None,
    }
}

/// One abandoned pass, as its Event reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReconcileTimeout {
    /// When the pass was given up: the latest of the Event's own times.
    pub at: DateTime<Utc>,
    /// The deadline it ran past, read from the Event's note. `None` when the
    /// note does not say.
    pub deadline_secs: Option<u64>,
    /// `regarding.uid`: which object of that name the pass was about.
    /// `None` when the Event does not say.
    pub uid: Option<String>,
    /// `reportingController`: the controller whose pass was cut. `None` when
    /// the Event does not say.
    pub reporter: Option<String>,
}

/// `kubectl get events.events.k8s.io -n <ns> --field-selector
/// reason=ReconcileTimedOut,regarding.kind=<kind>[,regarding.name=<name>]
/// -o json`.
///
/// `name` is `None` for `app status`, which reads the Events of every claim
/// in the namespace in one LIST and picks its own claims out of them.
pub(crate) fn event_list_args(namespace: &str, kind: &str, name: Option<&str>) -> Vec<String> {
    let mut selector = format!("reason={REASON},regarding.kind={kind}");
    if let Some(name) = name {
        selector.push_str(",regarding.name=");
        selector.push_str(name);
    }
    vec![
        "get".to_string(),
        EVENTS_RESOURCE.to_string(),
        "-n".to_string(),
        namespace.to_string(),
        "--field-selector".to_string(),
        selector,
        "-o".to_string(),
        "json".to_string(),
    ]
}

/// The `items` of a finished Event LIST, or nothing.
///
/// Any failure reads as "no Events": see the module docs for why this read
/// never warns.
pub(crate) fn interpret_event_list(success: bool, stdout: &[u8], stderr: &str) -> Vec<Value> {
    if !success {
        debug!(
            stderr = %stderr.trim(),
            "the ReconcileTimedOut Events could not be listed; no timeout line"
        );
        return Vec::new();
    }
    match serde_json::from_slice::<Value>(stdout) {
        Ok(list) => list
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        Err(e) => {
            debug!(error = %e, "the ReconcileTimedOut Event list did not parse; no timeout line");
            Vec::new()
        }
    }
}

/// The [`REASON`] Events about `kind` objects in `namespace` (about one
/// object when `name` is given). Never fails: a read that does not work is
/// an empty list.
pub(crate) fn read_events(
    namespace: &str,
    kind: &str,
    name: Option<&str>,
    kubeconfig: &Path,
) -> Vec<Value> {
    if refuse_if_interrupted().is_err() {
        return Vec::new();
    }
    match Command::new("kubectl")
        .args(event_list_args(namespace, kind, name))
        .env("KUBECONFIG", kubeconfig)
        .output()
    {
        Ok(out) => interpret_event_list(
            out.status.success(),
            &out.stdout,
            &String::from_utf8_lossy(&out.stderr),
        ),
        Err(e) => {
            debug!(error = %e, "could not start kubectl for the ReconcileTimedOut Events");
            Vec::new()
        }
    }
}

/// The newest abandoned pass of each `kind` object named in `events`.
///
/// Only a `Warning` with [`REASON`] about a `kind` in the AppRafter group
/// counts. The field selector already asked for the reason and the kind, so
/// those two are re-checked only because this function is also handed a
/// list from somewhere else, in the tests. The group check is NOT redundant,
/// because nothing upstream checks it (see [`GROUP_PREFIX`]).
pub(crate) fn latest_by_name(events: &[Value], kind: &str) -> BTreeMap<String, ReconcileTimeout> {
    let mut out: BTreeMap<String, ReconcileTimeout> = BTreeMap::new();
    for event in events {
        let text = |pointer: &str| event.pointer(pointer).and_then(Value::as_str);
        let ours = text("/type") == Some("Warning")
            && text("/reason") == Some(REASON)
            && text("/regarding/kind") == Some(kind)
            && text("/regarding/apiVersion").is_some_and(|v| v.starts_with(GROUP_PREFIX));
        if !ours {
            continue;
        }
        let (Some(name), Some(at)) = (text("/regarding/name"), occurred_at(event)) else {
            continue;
        };
        if out.get(name).is_some_and(|seen| seen.at >= at) {
            continue;
        }
        out.insert(
            name.to_string(),
            ReconcileTimeout {
                at,
                deadline_secs: text("/note").and_then(deadline_secs),
                uid: text("/regarding/uid").map(str::to_string),
                reporter: text("/reportingController").map(str::to_string),
            },
        );
    }
    out
}

/// When the Event last happened: the latest of `series.lastObservedTime`,
/// `eventTime`, `deprecatedLastTimestamp` and `metadata.creationTimestamp`.
///
/// The provisioner's recorder sets only `eventTime`, because it builds a
/// fresh recorder per publish and so never folds a repeat into a series.
/// The other three are read so that an Event from a recorder that does still
/// dates right.
fn occurred_at(event: &Value) -> Option<DateTime<Utc>> {
    [
        "/series/lastObservedTime",
        "/eventTime",
        "/deprecatedLastTimestamp",
        "/metadata/creationTimestamp",
    ]
    .iter()
    .filter_map(|pointer| event.pointer(pointer).and_then(Value::as_str))
    .filter_map(|raw| DateTime::parse_from_rfc3339(raw).ok())
    .map(|at| at.with_timezone(&Utc))
    .max()
}

/// The `N` of "did not finish within Ns" in the Event's note.
///
/// That phrase is the Display of `operator_core::deadline::ReconcileTimedOut`,
/// which a test below reads. A note in any other words has no deadline to
/// report, and the line is printed without it.
fn deadline_secs(note: &str) -> Option<u64> {
    let rest = note.split_once("did not finish within ")?.1;
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits == 0 || !rest[digits..].starts_with('s') {
        return None;
    }
    rest[..digits].parse().ok()
}

/// Whether the controller that reported `timeout` has written `object`'s
/// status since: an entry of `metadata.managedFields` for the `status`
/// subresource, under one of that controller's own field managers (exact
/// names), whose `time` is a later second than [`ReconcileTimeout::at`].
///
/// kubectl strips `managedFields` unless it is asked for them, so `object`
/// must be read with
/// [`kubectl_get_json_showing_managed_fields`](crate::commands::k8s_helpers::kubectl_get_json_showing_managed_fields).
/// `false` whenever the write cannot be placed after the Event: an unknown
/// or missing reporter, no `managedFields`, or a write in the Event's own
/// second (`managedFields` times are whole seconds).
fn status_written_since(object: &Value, timeout: &ReconcileTimeout) -> bool {
    let Some(managers) = timeout.reporter.as_deref().and_then(status_managers) else {
        return false;
    };
    let Some(entries) = object
        .pointer("/metadata/managedFields")
        .and_then(Value::as_array)
    else {
        return false;
    };
    let event_second = timeout.at.timestamp();
    entries.iter().any(|entry| {
        let text = |key: &str| entry.get(key).and_then(Value::as_str);
        text("subresource") == Some("status")
            && text("manager").is_some_and(|manager| managers.contains(&manager))
            && text("time")
                .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                .is_some_and(|written| written.timestamp() > event_second)
    })
}

/// What follows a status command's own label, or `None` when there is
/// nothing to say: no abandoned pass, one about an earlier object of the
/// same name, or one the reporting controller has since written the
/// object's status after ([`status_written_since`]).
///
/// The uids are compared only when both are present. A timeout that cannot
/// be placed is shown rather than hidden.
pub(crate) fn timeout_summary(
    object: &Value,
    timeout: Option<&ReconcileTimeout>,
    now: DateTime<Utc>,
) -> Option<String> {
    let timeout = timeout?;
    let object_uid = object.pointer("/metadata/uid").and_then(Value::as_str);
    if timeout
        .uid
        .as_deref()
        .zip(object_uid)
        .is_some_and(|(theirs, ours)| theirs != ours)
    {
        return None;
    }
    if status_written_since(object, timeout) {
        return None;
    }
    let ago = cli_core::timefmt::humanise_relative(
        now.signed_duration_since(timeout.at)
            .max(chrono::Duration::zero()),
    );
    let within = timeout
        .deadline_secs
        .map(|n| format!(" (did not finish within {n}s)"))
        .unwrap_or_default();
    Some(format!(
        "last timed out {ago}{within}; the operator retries on its own"
    ))
}

/// [`timeout_summary`] for one object read by name, from the Events
/// [`read_events`] returned for it.
pub(crate) fn object_summary(
    object: &Value,
    kind: &str,
    events: &[Value],
    now: DateTime<Utc>,
) -> Option<String> {
    let name = object.pointer("/metadata/name").and_then(Value::as_str)?;
    timeout_summary(object, latest_by_name(events, kind).get(name), now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    /// 12:00:00 UTC on the day the owner decided this surface.
    fn noon() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 2, 12, 0, 0).unwrap()
    }

    /// The Event the provisioner publishes, as `events.k8s.io/v1` returns it:
    /// `eventTime` with microseconds, no series, no deprecated timestamps,
    /// and `regarding` as `object_ref` builds it, uid included.
    fn timeout_event(kind: &str, name: &str, event_time: &str, deadline: u64) -> Value {
        json!({
            "apiVersion": "events.k8s.io/v1", "kind": "Event",
            "metadata": {
                "name": format!("{name}.17a3b0c2d4e5f607"), "namespace": "apps",
                "creationTimestamp": "2026-10-02T11:00:00Z"
            },
            "eventTime": event_time,
            "type": "Warning",
            "reason": "ReconcileTimedOut",
            "action": "Reconcile",
            "note": format!(
                "{kind} reconcile did not finish within {deadline}s: the controller abandoned \
                 this pass and will retry it; nothing was written to this object's status"
            ),
            "regarding": {
                "apiVersion": "apprafter.io/v1alpha1", "kind": kind,
                "name": name, "namespace": "apps", "uid": "u-1"
            },
            "reportingController": "apprafter-resourceclaim-provisioner",
            "reportingInstance": "apprafter-operator-0"
        })
    }

    /// The object a status command read, as far as these functions look at
    /// it: its name and its uid.
    fn object(name: &str, uid: &str) -> Value {
        json!({ "metadata": { "name": name, "namespace": "apps", "uid": uid } })
    }

    /// A pass abandoned `ago` before noon, about the object with uid `u-1`.
    fn timed_out(ago: chrono::Duration, deadline_secs: Option<u64>) -> ReconcileTimeout {
        ReconcileTimeout {
            at: noon() - ago,
            deadline_secs,
            uid: Some("u-1".to_string()),
            reporter: Some(PROVISIONER_REPORTER.to_string()),
        }
    }

    #[test]
    fn one_object_is_field_selected_by_reason_kind_and_name() {
        assert_eq!(
            event_list_args("apps", "SharedVolume", Some("data")).join(" "),
            "get events.events.k8s.io -n apps --field-selector \
             reason=ReconcileTimedOut,regarding.kind=SharedVolume,regarding.name=data -o json"
        );
    }

    #[test]
    fn a_namespace_of_claims_is_one_list_by_reason_and_kind() {
        assert_eq!(
            event_list_args("apps", "ResourceClaim", None).join(" "),
            "get events.events.k8s.io -n apps --field-selector \
             reason=ReconcileTimedOut,regarding.kind=ResourceClaim -o json"
        );
    }

    #[test]
    fn the_newest_timeout_of_each_object_wins() {
        let events = [
            timeout_event(
                "ResourceClaim",
                "web-pg",
                "2026-10-02T11:57:00.000001Z",
                120,
            ),
            timeout_event(
                "ResourceClaim",
                "web-pg",
                "2026-10-02T11:59:00.500000Z",
                120,
            ),
            timeout_event(
                "ResourceClaim",
                "web-pg",
                "2026-10-02T11:58:00.000000Z",
                120,
            ),
            timeout_event(
                "ResourceClaim",
                "web-redis",
                "2026-10-02T11:50:00.000000Z",
                120,
            ),
        ];
        let latest = latest_by_name(&events, "ResourceClaim");
        assert_eq!(latest.len(), 2, "{latest:?}");
        assert_eq!(
            latest["web-pg"].at,
            Utc.with_ymd_and_hms(2026, 10, 2, 11, 59, 0).unwrap()
                + chrono::Duration::milliseconds(500)
        );
        assert_eq!(latest["web-pg"].deadline_secs, Some(120));
        assert_eq!(latest["web-pg"].uid.as_deref(), Some("u-1"));
        assert_eq!(
            latest["web-redis"].at,
            Utc.with_ymd_and_hms(2026, 10, 2, 11, 50, 0).unwrap()
        );
    }

    #[test]
    fn a_series_dates_the_event_by_its_last_observation() {
        let mut event = timeout_event("SharedVolume", "data", "2026-10-02T11:40:00.000000Z", 60);
        event["series"] = json!({ "count": 3, "lastObservedTime": "2026-10-02T11:58:30.000000Z" });
        let latest = latest_by_name(&[event], "SharedVolume");
        assert_eq!(
            latest["data"].at,
            Utc.with_ymd_and_hms(2026, 10, 2, 11, 58, 30).unwrap()
        );
    }

    #[test]
    fn a_dra_resource_claim_with_the_same_reason_is_not_ours() {
        // The field selector matches `regarding.kind` only, so it also
        // returns an Event about a `resource.k8s.io` ResourceClaim of the
        // same name.
        let mut dra = timeout_event(
            "ResourceClaim",
            "web-pg",
            "2026-10-02T11:59:00.000000Z",
            120,
        );
        dra["regarding"]["apiVersion"] = json!("resource.k8s.io/v1");
        assert!(latest_by_name(&[dra], "ResourceClaim").is_empty());
    }

    #[test]
    fn only_a_warning_with_the_timeout_reason_about_this_kind_counts() {
        let mut normal = timeout_event("SharedVolume", "data", "2026-10-02T11:59:00.000000Z", 60);
        normal["type"] = json!("Normal");
        let mut other_reason =
            timeout_event("SharedVolume", "data", "2026-10-02T11:59:00.000000Z", 60);
        other_reason["reason"] = json!("CapacityWarning");
        let other_kind =
            timeout_event("SharedDatabase", "data", "2026-10-02T11:59:00.000000Z", 120);
        assert!(latest_by_name(&[normal, other_reason, other_kind], "SharedVolume").is_empty());
    }

    #[test]
    fn the_deadline_is_read_from_the_note_and_left_out_when_absent() {
        assert_eq!(
            deadline_secs("SharedVolume reconcile did not finish within 60s: abandoned"),
            Some(60)
        );
        assert_eq!(deadline_secs("reconcile did not finish within s"), None);
        assert_eq!(
            deadline_secs("reconcile did not finish within 60 seconds"),
            None
        );
        assert_eq!(deadline_secs("the pass was abandoned"), None);
    }

    #[test]
    fn a_timeout_is_one_line_with_its_age() {
        let timeout = timed_out(chrono::Duration::minutes(3), Some(120));
        assert_eq!(
            timeout_summary(&object("web-pg", "u-1"), Some(&timeout), noon()).as_deref(),
            Some(
                "last timed out 3 minutes ago (did not finish within 120s); the operator \
                 retries on its own"
            )
        );
    }

    #[test]
    fn a_note_without_a_deadline_still_prints_the_line() {
        let timeout = timed_out(chrono::Duration::minutes(3), None);
        assert_eq!(
            timeout_summary(&object("web-pg", "u-1"), Some(&timeout), noon()).as_deref(),
            Some("last timed out 3 minutes ago; the operator retries on its own")
        );
    }

    #[test]
    fn a_recreated_object_does_not_inherit_its_predecessors_timeout() {
        // `app remove` and a re-add within the Event's hour: the same name,
        // a new uid.
        let timeout = timed_out(chrono::Duration::minutes(3), Some(120));
        assert_eq!(
            timeout_summary(&object("web-pg", "u-2"), Some(&timeout), noon()),
            None
        );
        // When either side has no uid, the two cannot be told apart, and the
        // timeout is shown rather than hidden.
        let without_uid = ReconcileTimeout {
            uid: None,
            ..timeout.clone()
        };
        assert!(timeout_summary(&object("web-pg", "u-2"), Some(&without_uid), noon()).is_some());
        let unnamed = json!({ "metadata": { "name": "web-pg" } });
        assert!(timeout_summary(&unnamed, Some(&timeout), noon()).is_some());
    }

    #[test]
    fn no_timeout_prints_nothing() {
        let claim = object("web-pg", "u-1");
        assert_eq!(timeout_summary(&claim, None, noon()), None);
        assert_eq!(object_summary(&claim, "ResourceClaim", &[], noon()), None);
    }

    #[test]
    fn a_clock_behind_the_event_reads_just_now() {
        let timeout = ReconcileTimeout {
            at: noon() + chrono::Duration::seconds(20),
            deadline_secs: Some(60),
            uid: None,
            reporter: None,
        };
        let line = timeout_summary(&json!({}), Some(&timeout), noon()).unwrap();
        assert!(
            line.starts_with("last timed out just now (did not finish within 60s)"),
            "{line}"
        );
    }

    #[test]
    fn object_summary_reads_the_events_about_that_object_only() {
        let volume = object("data", "u-1");
        let events = [
            timeout_event("SharedVolume", "other", "2026-10-02T11:59:00.000000Z", 60),
            timeout_event("SharedVolume", "data", "2026-10-02T11:58:00.000000Z", 60),
        ];
        assert_eq!(
            object_summary(&volume, "SharedVolume", &events, noon()).as_deref(),
            Some(
                "last timed out 2 minutes ago (did not finish within 60s); the operator \
                 retries on its own"
            )
        );
        assert_eq!(
            object_summary(&volume, "SharedVolume", &events[..1], noon()),
            None
        );
    }

    #[test]
    fn a_list_that_failed_or_did_not_parse_is_no_events() {
        let forbidden = "Error from server (Forbidden): events.events.k8s.io is forbidden";
        assert!(interpret_event_list(false, b"", forbidden).is_empty());
        assert!(interpret_event_list(true, b"not json", "").is_empty());
        assert!(interpret_event_list(true, br#"{"kind":"List"}"#, "").is_empty());
        let one = br#"{"items":[{"reason":"ReconcileTimedOut"}]}"#;
        assert_eq!(interpret_event_list(true, one, "").len(), 1);
    }

    /// One `metadata.managedFields` entry, as `kubectl get
    /// --show-managed-fields` returns it: an apply by `manager` at `time`
    /// (whole seconds, as the apiserver records it), to the status
    /// subresource when `subresource` says so.
    fn write(manager: &str, subresource: Option<&str>, time: &str) -> Value {
        let mut entry = json!({
            "manager": manager, "operation": "Apply", "apiVersion": "apprafter.io/v1alpha1",
            "time": time, "fieldsType": "FieldsV1", "fieldsV1": { "f:status": {} }
        });
        if let Some(subresource) = subresource {
            entry["subresource"] = json!(subresource);
        }
        entry
    }

    /// [`object`], read with its `managedFields`: these writes.
    fn written(name: &str, uid: &str, writes: &[Value]) -> Value {
        let mut read = object(name, uid);
        read["metadata"]["managedFields"] = json!(writes);
        read
    }

    /// The provisioner's claim timeout of 11:57:00, three minutes before noon.
    fn claim_timeout_at_1157() -> [Value; 1] {
        [timeout_event(
            "ResourceClaim",
            "web-pg",
            "2026-10-02T11:57:00.000000Z",
            120,
        )]
    }

    #[test]
    fn a_status_write_by_the_reporter_since_the_timeout_quiets_the_line() {
        // A later pass of the provisioner got through and wrote the status:
        // the timeout is over, and the line goes (O2, WI-394).
        for manager in [
            "resourceclaim-provisioner",
            "resourceclaim-provisioner-jetstream",
        ] {
            let claim = written(
                "web-pg",
                "u-1",
                &[write(manager, Some("status"), "2026-10-02T11:58:00Z")],
            );
            assert_eq!(
                object_summary(&claim, "ResourceClaim", &claim_timeout_at_1157(), noon()),
                None,
                "a status write under {manager} after the timeout"
            );
        }
    }

    #[test]
    fn a_timeout_newer_than_the_reporters_last_status_write_is_shown() {
        // Timed out again: the provisioner's last status write predates the
        // newest Event.
        let claim = written(
            "web-pg",
            "u-1",
            &[write(
                "resourceclaim-provisioner",
                Some("status"),
                "2026-10-02T11:56:00Z",
            )],
        );
        assert_eq!(
            object_summary(&claim, "ResourceClaim", &claim_timeout_at_1157(), noon()).as_deref(),
            Some(
                "last timed out 3 minutes ago (did not finish within 120s); the operator \
                 retries on its own"
            )
        );
        // `managedFields` times are whole seconds, so a write in the Event's
        // own second cannot be placed after it, and the line stays.
        let same_second = written(
            "web-pg",
            "u-1",
            &[write(
                "resourceclaim-provisioner",
                Some("status"),
                "2026-10-02T11:57:00Z",
            )],
        );
        let mut late_in_the_second = claim_timeout_at_1157();
        late_in_the_second[0]["eventTime"] = json!("2026-10-02T11:57:00.400000Z");
        for events in [claim_timeout_at_1157(), late_in_the_second] {
            assert!(
                object_summary(&same_second, "ResourceClaim", &events, noon()).is_some(),
                "{events:?}"
            );
        }
    }

    #[test]
    fn another_controllers_status_write_does_not_quiet_it() {
        // The scheduler wrote the claim's status after the provisioner's
        // pass was cut: that says nothing about the provisioner's pass.
        let scheduler_wrote = written(
            "web-pg",
            "u-1",
            &[write(
                "resourceclaim-scheduler",
                Some("status"),
                "2026-10-02T11:58:00Z",
            )],
        );
        assert!(object_summary(
            &scheduler_wrote,
            "ResourceClaim",
            &claim_timeout_at_1157(),
            noon()
        )
        .is_some());
        // Nor does a write by the provisioner's own manager outside the
        // status (a finalizer, a label).
        let outside_the_status = written(
            "web-pg",
            "u-1",
            &[write(
                "resourceclaim-provisioner",
                None,
                "2026-10-02T11:58:00Z",
            )],
        );
        assert!(object_summary(
            &outside_the_status,
            "ResourceClaim",
            &claim_timeout_at_1157(),
            noon()
        )
        .is_some());
    }

    #[test]
    fn the_acl_loops_size_write_does_not_quiet_it() {
        // `resourceclaim-provisioner-size` is the provisioner's, but the ACL
        // resync loop writes it on its own schedule, and a cut claim pass
        // pokes that loop just before it publishes the Event.
        let claim = written(
            "web-pg",
            "u-1",
            &[write(
                "resourceclaim-provisioner-size",
                Some("status"),
                "2026-10-02T11:57:02Z",
            )],
        );
        assert!(
            object_summary(&claim, "ResourceClaim", &claim_timeout_at_1157(), noon()).is_some()
        );
    }

    #[test]
    fn a_timeout_whose_recovery_cannot_be_read_is_shown() {
        let newer_write = [write(
            "resourceclaim-provisioner",
            Some("status"),
            "2026-10-02T11:58:00Z",
        )];
        // No `managedFields`: a plain `kubectl get` strips them.
        assert!(object_summary(
            &object("web-pg", "u-1"),
            "ResourceClaim",
            &claim_timeout_at_1157(),
            noon()
        )
        .is_some());
        // A reporter this CLI does not know, or none at all: whose status
        // write would count is unknown.
        let claim = written("web-pg", "u-1", &newer_write);
        let mut unknown = claim_timeout_at_1157();
        unknown[0]["reportingController"] = json!("apprafter-something-else");
        let mut unreported = claim_timeout_at_1157();
        unreported[0]
            .as_object_mut()
            .unwrap()
            .remove("reportingController");
        for events in [unknown, unreported] {
            assert!(
                object_summary(&claim, "ResourceClaim", &events, noon()).is_some(),
                "{events:?}"
            );
        }
    }

    #[test]
    fn a_scheduler_timeout_is_shown_until_the_scheduler_writes_the_claims_status() {
        let mut scheduler_cut = claim_timeout_at_1157();
        scheduler_cut[0]["reportingController"] = json!("apprafter-resourceclaim-scheduler");
        let provisioner_wrote = written(
            "web-pg",
            "u-1",
            &[write(
                "resourceclaim-provisioner",
                Some("status"),
                "2026-10-02T11:58:00Z",
            )],
        );
        assert_eq!(
            object_summary(&provisioner_wrote, "ResourceClaim", &scheduler_cut, noon()).as_deref(),
            Some(
                "last timed out 3 minutes ago (did not finish within 120s); the operator \
                 retries on its own"
            ),
            "the provisioner's write says nothing about the scheduler's pass"
        );
        let scheduler_wrote = written(
            "web-pg",
            "u-1",
            &[write(
                "resourceclaim-scheduler",
                Some("status"),
                "2026-10-02T11:58:00Z",
            )],
        );
        assert_eq!(
            object_summary(&scheduler_wrote, "ResourceClaim", &scheduler_cut, noon()),
            None
        );
    }

    /// A source file of the operator workspace: the same repository, a
    /// separate cargo workspace, so it is read rather than imported.
    fn operator_source(relative: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../operator")
            .join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "{}: the operator's own declaration could not be read, so this test \
                 would have judged nothing: {e}",
                path.display()
            )
        })
    }

    #[test]
    fn the_reason_is_the_one_the_provisioner_publishes() {
        // A rename on one side alone would leave these commands field-selecting
        // for an Event nobody writes, and printing nothing, forever.
        let src = operator_source("operator-core/src/deadline_event.rs");
        let declared = format!("pub const REASON: &str = \"{REASON}\";");
        assert!(
            src.contains(&declared),
            "deadline_event.rs no longer declares `{declared}`"
        );
    }

    #[test]
    fn the_deadline_wording_is_the_operators_own() {
        let src = operator_source("operator-core/src/deadline.rs");
        assert!(
            src.contains("#[error(\"reconcile did not finish within {}s\", .after.as_secs())]"),
            "ReconcileTimedOut's Display changed; `deadline_secs` reads that phrase"
        );
    }

    #[test]
    fn the_reporters_and_their_status_managers_are_the_operators_own() {
        // A rename on the operator side alone would read as a reporter or a
        // manager this CLI does not know: every timeout would then stay
        // shown until its Event expires, never quiet after a recovery.
        const PROVISIONER: &str = "operator-controllers/resourceclaim-provisioner/src";
        const SCHEDULER: &str = "operator-controllers/resourceclaim-scheduler/src";
        let declares = |file: &str, constant: &str, value: &str| {
            let declaration = format!("const {constant}: &str = \"{value}\";");
            assert!(
                operator_source(file).contains(&declaration),
                "{file} no longer declares `{declaration}`"
            );
        };
        let lib = format!("{PROVISIONER}/lib.rs");
        declares(&lib, "REPORTER_CONTROLLER", PROVISIONER_REPORTER);
        assert_eq!(
            PROVISIONER_STATUS_MANAGERS,
            [
                "resourceclaim-provisioner",
                "resourceclaim-provisioner-jetstream"
            ]
        );
        declares(&lib, "FIELD_MANAGER", "resourceclaim-provisioner");
        declares(
            &lib,
            "JETSTREAM_FIELD_MANAGER",
            "resourceclaim-provisioner-jetstream",
        );
        // The ACL loop's manager is the provisioner's too, and stays out.
        declares(&lib, "SIZE_FIELD_MANAGER", "resourceclaim-provisioner-size");
        assert!(!PROVISIONER_STATUS_MANAGERS.contains(&"resourceclaim-provisioner-size"));
        // A SharedVolume's and a SharedDatabase's status are applied under
        // the provisioner's `FIELD_MANAGER`.
        let under_field_manager = "fn apply_params() -> PatchParams {\n    \
                                   PatchParams::apply(FIELD_MANAGER).force()\n}";
        for file in ["shared_volume.rs", "shared_database.rs"] {
            let src = operator_source(&format!("{PROVISIONER}/{file}"));
            assert!(
                src.contains(under_field_manager),
                "{file} no longer applies the status under the provisioner's FIELD_MANAGER"
            );
        }
        declares(
            &format!("{SCHEDULER}/reconcile.rs"),
            "EVENT_REPORTER_CONTROLLER",
            SCHEDULER_REPORTER,
        );
        assert_eq!(SCHEDULER_STATUS_MANAGERS, ["resourceclaim-scheduler"]);
        declares(
            &format!("{SCHEDULER}/lib.rs"),
            "FIELD_MANAGER",
            "resourceclaim-scheduler",
        );
    }
}
