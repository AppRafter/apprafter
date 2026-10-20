// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `MigrationPlan`s, as `kubectl get migrationplan -A -o json` lists them.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::time::Ago;

/// The plan `namespace/name`, made 3 hours ago, scoped to the platform in
/// `apprafter-system` and to its application elsewhere; `classification` its risk, `phase`
/// its status (`None`: the controller has written none, which reads as waiting).
pub fn plan(
    now: DateTime<Utc>,
    namespace: &str,
    name: &str,
    classification: Option<&str>,
    phase: Option<&str>,
) -> Value {
    let scope = if namespace == super::platform::NAMESPACE {
        "platform"
    } else {
        "app"
    };
    let mut plan = json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "MigrationPlan",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "creationTimestamp": Ago::Hours(3).before(now)
        },
        "spec": { "scope": { "type": scope } }
    });
    if let Some(classification) = classification {
        plan["spec"]["risks"]["classification"] = json!(classification);
    }
    if let Some(phase) = phase {
        plan["status"]["phase"] = json!(phase);
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_carries_what_it_was_given() {
        let now = DateTime::parse_from_rfc3339("2026-10-16T12:00:00Z")
            .expect("a test time")
            .with_timezone(&Utc);
        let waiting = plan(now, "shop", "shop-1", Some("destructive"), None);
        assert_eq!(waiting["spec"]["risks"]["classification"], "destructive");
        assert_eq!(waiting["spec"]["scope"]["type"], "app");
        assert!(waiting.get("status").is_none());
        let decided = plan(now, "apprafter-system", "p-1", None, Some("approved"));
        assert_eq!(decided["spec"]["scope"]["type"], "platform");
        assert_eq!(decided["status"]["phase"], "approved");
        assert!(decided["spec"].get("risks").is_none());
    }
}
