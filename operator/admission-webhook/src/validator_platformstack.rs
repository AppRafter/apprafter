// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure validator for the v1alpha1 PlatformStack object.
//!
//! Enforces cross-field invariants the OpenAPI v3 CRD layer
//! can't express:
//!
//!   - **Singleton:** name MUST be `default` and namespace
//!     MUST be `apprafter-system`. PlatformStack is namespaced
//!     for RBAC granularity but behaves cluster-scoped.
//!   - **checkInterval >= 1h.** OpenAPI v3 can express a regex
//!     shape, but not a numeric duration comparison.
//!   - **channel enum.** Mirrored from the OpenAPI v3 schema
//!     for defence in depth and a better error message.
//!   - **network.egress.profile enum.** Mirrored from the
//!     OpenAPI v3 schema (internet|internal|strict, 2.10) for
//!     defence in depth and a better error message.
//!   - **pin semver shape.** Mirrored for the same reason.
//!   - **overrides keys reference declared components.** Advisory
//!     warning, NOT rejection — pre-declaring an override for a
//!     future chart version is legitimate.
//!
//! Typed against `operator_core::PlatformStackSpec` (ADR 0047
//! Decision #4): the spec is deserialized into the operator-core struct
//! once and the value rules read TYPED fields (`channel`, `pin`,
//! `source.check_interval`), so a renamed field fails to compile rather
//! than silently bypassing a rule. Three branches stay on the raw `Value`
//! and are documented inline:
//!
//!   - **spec presence** — the typed struct cannot represent an absent
//!     `spec`; this diagnostic exists for the unit tests / a misconfigured
//!     apiserver (a validating webhook runs after structural validation).
//!   - **`network.egress.profile` enum** — `profile` is typed as the
//!     `EgressProfile` enum, so an invalid value (`"wide-open"`) makes the
//!     WHOLE spec fail to deserialize; the typed struct cannot represent the
//!     rejected input, so this enum check reads the raw `Value` (matching the
//!     CRD's enum + the pre-refactor behaviour exactly).
//!   - the typed deserialize is `Option` — when it fails (e.g. the invalid
//!     profile above), every typed rule falls back to the raw `Value` so the
//!     other diagnostics still fire, exactly as before.

use operator_core::PlatformStackSpec;
use serde_json::Value;

use crate::validator::ValidationError;

/// Validate a PlatformStack AdmissionReview object. The caller
/// (server.rs) MUST pass the full `request.object` — the
/// validator reads `metadata.{name,namespace}` and `spec.*` from
/// it. Returns every error found; empty Vec = valid.
pub fn validate_platformstack(object: &Value) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    let Some(obj) = object.as_object() else {
        errors.push(ValidationError::new(
            "object",
            "PlatformStack object must be a JSON object",
        ));
        return errors;
    };

    let metadata = obj.get("metadata").and_then(Value::as_object);
    let name = metadata
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let namespace = metadata
        .and_then(|m| m.get("namespace"))
        .and_then(Value::as_str)
        .unwrap_or("");

    if name != "default" {
        errors.push(ValidationError::new(
            "metadata.name",
            format!(
                "PlatformStack must be named \"default\" (got {name:?}); \
                 the resource is a cluster-scoped singleton even though \
                 namespaced for RBAC"
            ),
        ));
    }
    if namespace != "apprafter-system" {
        errors.push(ValidationError::new(
            "metadata.namespace",
            format!(
                "PlatformStack must live in namespace \"apprafter-system\" \
                 (got {namespace:?}); the resource is a cluster-scoped \
                 singleton even though namespaced for RBAC"
            ),
        ));
    }

    let Some(spec_value) = obj.get("spec").filter(|s| s.is_object()) else {
        errors.push(ValidationError::new("spec", "spec is required"));
        return errors;
    };
    let spec = spec_value.as_object().expect("filtered to an object above");

    // Deserialize the spec into the typed operator-core struct. In
    // production this always succeeds — a validating webhook runs after the
    // apiserver's structural validation. When it does, the value rules read
    // TYPED fields so a renamed field fails to compile (ADR 0047 #4). When it
    // fails (e.g. an invalid `network.egress.profile` enum — see below), each
    // rule falls back to the raw `Value` so the other diagnostics still fire.
    let typed = serde_json::from_value::<PlatformStackSpec>(spec_value.clone()).ok();

    // `channel` — typed `String` (serde-defaults to "stable" when absent),
    // so reading it typed is behaviour-identical to the pre-refactor
    // `spec.get("channel")` (an absent channel resolves to the valid default;
    // a present invalid value is a plain String and is rejected here).
    if let Some(channel) = typed
        .as_ref()
        .map(|s| s.channel.as_str())
        .or_else(|| spec.get("channel").and_then(Value::as_str))
    {
        if !matches!(channel, "stable" | "beta" | "edge") {
            errors.push(ValidationError::new(
                "spec.channel",
                format!("channel must be one of stable|beta|edge (got {channel:?})"),
            ));
        }
    }

    // `network.egress.profile` — typed as the `EgressProfile` enum, so an
    // invalid value makes the WHOLE spec fail to deserialize (`typed` is
    // None) and cannot be read off the typed struct. This enum check stays on
    // the raw `Value`, matching the CRD's enum + the pre-refactor behaviour
    // exactly: a valid profile is also a valid enum (typed deserialize
    // succeeds), an invalid one is caught here.
    if let Some(profile) = spec
        .get("network")
        .and_then(|n| n.get("egress"))
        .and_then(|e| e.get("profile"))
        .and_then(Value::as_str)
    {
        if !matches!(profile, "internet" | "internal" | "strict") {
            errors.push(ValidationError::new(
                "spec.network.egress.profile",
                format!("egress profile must be one of internet|internal|strict (got {profile:?})"),
            ));
        }
    }

    // `pin` — typed `Option<String>`, which faithfully represents presence;
    // read it typed on the happy path, falling back to the raw `Value` when
    // the deserialize failed (so an invalid pin still surfaces).
    if let Some(pin) = typed
        .as_ref()
        .and_then(|s| s.pin.as_deref())
        .or_else(|| spec.get("pin").and_then(Value::as_str))
    {
        if !is_semver(pin) {
            errors.push(ValidationError::new(
                "spec.pin",
                format!("pin must be valid semver (got {pin:?})"),
            ));
        }
    }

    // `source.checkInterval` — typed `source.check_interval: String`
    // (non-`Option`; the apiserver requires `source`). Read it typed on the
    // happy path; fall back to the raw `Value` lookup (which gracefully skips
    // when `source`/`checkInterval` is absent) when the deserialize failed.
    if let Some(interval) = typed
        .as_ref()
        .map(|s| s.source.check_interval.as_str())
        .or_else(|| {
            spec.get("source")
                .and_then(Value::as_object)
                .and_then(|src| src.get("checkInterval"))
                .and_then(Value::as_str)
        })
    {
        match parse_duration_to_seconds(interval) {
            Some(secs) if secs < 3600 => {
                errors.push(ValidationError::new(
                    "spec.source.checkInterval",
                    format!(
                        "checkInterval must be at least 1h (got {interval:?}); \
                         PlatformController polling tighter than 1h overloads the \
                         OCI registry and the controller's reconciliation budget"
                    ),
                ));
            }
            None => {
                errors.push(ValidationError::new(
                    "spec.source.checkInterval",
                    format!(
                        "checkInterval must be a Go duration string like \"6h\", \
                         \"30m\", \"3600s\" (got {interval:?})"
                    ),
                ));
            }
            _ => {}
        }
    }

    errors
}

fn is_semver(s: &str) -> bool {
    let parts: Vec<&str> = s.splitn(2, '-').collect();
    let core = parts[0];
    let core_segments: Vec<&str> = core.split('.').collect();
    if core_segments.len() != 3 {
        return false;
    }
    core_segments
        .iter()
        .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()))
}

/// Parse a Go-style duration with a single unit suffix
/// (`s`, `m`, `h`). Returns total seconds. Returns `None` for
/// malformed inputs.
///
/// Not a full `time.ParseDuration` clone — PlatformStack only
/// needs single-unit durations because that's how operators
/// realistically configure check intervals.
fn parse_duration_to_seconds(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (digits, unit) = bytes.split_at(bytes.len() - 1);
    let digits = std::str::from_utf8(digits).ok()?;
    let value: u64 = digits.parse().ok()?;
    match unit[0] {
        b's' => Some(value),
        b'm' => Some(value * 60),
        b'h' => Some(value * 3600),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_object() -> Value {
        json!({
            "metadata": { "name": "default", "namespace": "apprafter-system" },
            "spec": {
                "channel": "stable",
                "autoUpgrade": false,
                "source": {
                    "upstream": "oci://ghcr.io/apprafter/platform-stack",
                    "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                    "checkInterval": "6h"
                },
                "values": { "tier": 1 }
            }
        })
    }

    #[test]
    fn accepts_canonical_default_object() {
        assert!(validate_platformstack(&valid_object()).is_empty());
    }

    #[test]
    fn rejects_non_default_name() {
        let mut obj = valid_object();
        obj["metadata"]["name"] = json!("other");
        let errors = validate_platformstack(&obj);
        assert!(errors.iter().any(|e| e.field == "metadata.name"));
    }

    #[test]
    fn rejects_wrong_namespace() {
        let mut obj = valid_object();
        obj["metadata"]["namespace"] = json!("default");
        let errors = validate_platformstack(&obj);
        assert!(errors.iter().any(|e| e.field == "metadata.namespace"));
    }

    #[test]
    fn rejects_invalid_channel() {
        let mut obj = valid_object();
        obj["spec"]["channel"] = json!("nightly");
        let errors = validate_platformstack(&obj);
        assert!(errors.iter().any(|e| e.field == "spec.channel"));
    }

    #[test]
    fn accepts_omitted_channel() {
        let mut obj = valid_object();
        obj["spec"].as_object_mut().unwrap().remove("channel");
        assert!(validate_platformstack(&obj).is_empty());
    }

    #[test]
    fn rejects_non_semver_pin() {
        let mut obj = valid_object();
        obj["spec"]["pin"] = json!("v0.2.0");
        let errors = validate_platformstack(&obj);
        assert!(errors.iter().any(|e| e.field == "spec.pin"));
    }

    #[test]
    fn accepts_semver_pin() {
        let mut obj = valid_object();
        obj["spec"]["pin"] = json!("0.2.0");
        assert!(validate_platformstack(&obj).is_empty());
    }

    #[test]
    fn accepts_semver_pin_with_prerelease() {
        let mut obj = valid_object();
        obj["spec"]["pin"] = json!("0.2.0-rc.1");
        assert!(validate_platformstack(&obj).is_empty());
    }

    #[test]
    fn rejects_check_interval_below_one_hour() {
        let mut obj = valid_object();
        obj["spec"]["source"]["checkInterval"] = json!("30m");
        let errors = validate_platformstack(&obj);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.source.checkInterval"));
    }

    #[test]
    fn rejects_check_interval_in_seconds_below_3600() {
        let mut obj = valid_object();
        obj["spec"]["source"]["checkInterval"] = json!("3599s");
        let errors = validate_platformstack(&obj);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.source.checkInterval"));
    }

    #[test]
    fn accepts_check_interval_at_exactly_one_hour() {
        let mut obj = valid_object();
        obj["spec"]["source"]["checkInterval"] = json!("1h");
        assert!(validate_platformstack(&obj).is_empty());
    }

    #[test]
    fn accepts_check_interval_at_3600_seconds() {
        let mut obj = valid_object();
        obj["spec"]["source"]["checkInterval"] = json!("3600s");
        assert!(validate_platformstack(&obj).is_empty());
    }

    #[test]
    fn rejects_garbled_check_interval() {
        let mut obj = valid_object();
        obj["spec"]["source"]["checkInterval"] = json!("forever");
        let errors = validate_platformstack(&obj);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.source.checkInterval"));
    }

    #[test]
    fn rejects_spec_missing() {
        let obj = json!({
            "metadata": { "name": "default", "namespace": "apprafter-system" }
        });
        let errors = validate_platformstack(&obj);
        assert!(errors.iter().any(|e| e.field == "spec"));
    }

    #[test]
    fn aggregates_multiple_errors() {
        let mut obj = valid_object();
        obj["metadata"]["name"] = json!("other");
        obj["spec"]["channel"] = json!("nightly");
        obj["spec"]["source"]["checkInterval"] = json!("30m");
        let errors = validate_platformstack(&obj);
        assert!(errors.len() >= 3);
    }

    #[test]
    fn accepts_pin_set_with_channel() {
        let mut obj = valid_object();
        obj["spec"]["pin"] = json!("0.1.15");
        obj["spec"]["channel"] = json!("beta");
        assert!(validate_platformstack(&obj).is_empty());
    }

    #[test]
    fn rejects_invalid_egress_profile() {
        let mut obj = valid_object();
        obj["spec"]["network"]["egress"]["profile"] = json!("wide-open");
        let errors = validate_platformstack(&obj);
        assert!(errors
            .iter()
            .any(|e| e.field == "spec.network.egress.profile"));
    }

    #[test]
    fn accepts_valid_egress_profile_and_absent() {
        // explicit valid profile
        let mut obj = valid_object();
        obj["spec"]["network"]["egress"]["profile"] = json!("strict");
        assert!(validate_platformstack(&obj).is_empty());
        // absent network entirely (canonical object) stays valid
        assert!(validate_platformstack(&valid_object()).is_empty());
    }
}
