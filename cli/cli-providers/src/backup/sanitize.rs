// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! CR sanitizer: strips ephemeral/identity fields from a live CR JSON so it
//! replays cleanly into a fresh cluster (no stale UIDs, no status).

use serde_json::Value;

const STRIP_META: &[&str] = &[
    "uid",
    "resourceVersion",
    "creationTimestamp",
    "managedFields",
    "ownerReferences",
    "generation",
    "selfLink",
];

/// Sanitize a live CR JSON for replay into a fresh cluster: drop `status`
/// and ephemeral/identity `metadata` fields; keep name/namespace/labels/
/// annotations/spec. NEVER carries ownerReferences (stale UIDs).
pub fn sanitize_cr(raw: &Value) -> Value {
    let mut out = raw.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.remove("status");
        if let Some(meta) = obj.get_mut("metadata").and_then(Value::as_object_mut) {
            for k in STRIP_META {
                meta.remove(*k);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_status_and_ephemeral_metadata() {
        let raw = json!({
            "apiVersion": "apprafter.io/v1alpha1", "kind": "Application",
            "metadata": {
                "name": "alpha", "namespace": "demo",
                "labels": {"app": "alpha"},
                "uid": "abc", "resourceVersion": "123", "generation": 2,
                "creationTimestamp": "2026-01-01T00:00:00Z",
                "managedFields": [{"manager": "x"}],
                "ownerReferences": [{"uid": "parent"}]
            },
            "spec": {"base": {"image": "x"}},
            "status": {"phase": "Ready"}
        });
        let s = sanitize_cr(&raw);
        assert!(s.get("status").is_none());
        let m = &s["metadata"];
        assert_eq!(m["name"], "alpha");
        assert_eq!(m["namespace"], "demo");
        assert_eq!(m["labels"]["app"], "alpha");
        for k in [
            "uid",
            "resourceVersion",
            "generation",
            "creationTimestamp",
            "managedFields",
            "ownerReferences",
        ] {
            assert!(m.get(k).is_none(), "metadata.{k} must be stripped");
        }
        assert_eq!(s["spec"]["base"]["image"], "x");
    }
}
