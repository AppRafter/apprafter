// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `manifest.json` builder for 2.6d backup/restore.

use crate::backup::ResourceRef;

/// The `manifest.json` written at the root of an export/backup.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    pub cluster_id: String,
    pub created_at: String,
    /// Source cluster's platform-stack version (M1) — `restore --reprovision`
    /// bootstraps the target at THIS version so the PlatformStack apply is a
    /// no-op (no mid-restore component re-render).
    pub platform_version: String,
    pub namespaces: Vec<String>,
    pub resources: Vec<ResourceRef>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::ResourceRef;

    #[test]
    fn manifest_carries_scope_resources_and_platform_version() {
        let m = BackupManifest {
            cluster_id: "k3d-demo".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            platform_version: "0.2.37".into(),
            namespaces: vec!["demo".into()],
            resources: vec![ResourceRef {
                namespace: "demo".into(),
                kind: "Application".into(),
                name: "alpha".into(),
                claim_type: None,
            }],
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["clusterId"], "k3d-demo");
        assert_eq!(v["platformVersion"], "0.2.37");
        assert_eq!(v["namespaces"][0], "demo");
        assert_eq!(v["resources"][0]["kind"], "Application");
        let back: BackupManifest = serde_json::from_value(v).unwrap();
        assert_eq!(back.namespaces, vec!["demo".to_string()]);
        assert_eq!(back.platform_version, "0.2.37");
    }
}
