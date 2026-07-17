// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Orchestration primitives: namespace discovery and run-outcome type.

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Outcome of one backup runner invocation.
pub enum RunOutcome {
    /// The backup completed successfully.  `snapshot` is the restic snapshot ID
    /// when available (monolithic mode produces one; sequential may produce many
    /// and this carries the last one, or `None` when not yet known).
    Success { snapshot: Option<String> },
    /// The backup failed with a human-readable `error` message.
    Failure { error: String },
}

impl RunOutcome {
    /// Returns `0` on success and `1` on failure — suitable for `process::exit`.
    pub fn exit_code(&self) -> i32 {
        match self {
            RunOutcome::Success { .. } => 0,
            RunOutcome::Failure { .. } => 1,
        }
    }
}

/// Collect and deduplicate (sorted) the namespace of every Application returned
/// by the Kubernetes API list response `apps`.
///
/// `apps` must be a JSON object with an `"items"` array where each element has
/// a `"metadata"."namespace"` string field.  Items whose namespace is absent or
/// not a string are silently skipped.
pub fn resolve_namespaces(apps: &serde_json::Value) -> Vec<String> {
    let mut ns: Vec<String> = apps["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["metadata"]["namespace"].as_str().map(str::to_string))
        .collect();
    ns.sort();
    ns.dedup();
    ns
}

// ---------------------------------------------------------------------------
// Tests (written first — TDD red phase)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_namespaces_dedups_and_sorts() {
        let apps = serde_json::json!({"items":[
            {"metadata":{"namespace":"b"}},{"metadata":{"namespace":"a"}},{"metadata":{"namespace":"a"}}]});
        assert_eq!(
            resolve_namespaces(&apps),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn resolve_namespaces_empty_when_no_items() {
        assert!(resolve_namespaces(&serde_json::json!({"items":[]})).is_empty());
    }

    #[test]
    fn exit_code_zero_success_one_failure() {
        assert_eq!(RunOutcome::Success { snapshot: None }.exit_code(), 0);
        assert_eq!(RunOutcome::Failure { error: "x".into() }.exit_code(), 1);
    }
}
