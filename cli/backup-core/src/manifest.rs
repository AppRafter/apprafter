// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `manifest.json` builder for 2.6d backup/restore.

use crate::ResourceRef;

/// Manifest format version this build can read and write.
///
/// v1 = initial (2.6d-4). `restore` rejects any backup whose `manifestVersion`
/// exceeds this constant so that a future format bump surfaces a clear error
/// instead of silent misparse (spec §Manifest / m8).
pub const MANIFEST_VERSION_CURRENT: u32 = 1;

fn default_manifest_version() -> u32 {
    MANIFEST_VERSION_CURRENT
}

/// The `manifest.json` written at the root of an export/backup.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    /// Manifest format version. Absent in shipped v1 backups → defaults to 1.
    /// `restore` rejects `> MANIFEST_VERSION_CURRENT` (m8).
    #[serde(rename = "manifestVersion", default = "default_manifest_version")]
    pub manifest_version: u32,
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
    use crate::ResourceRef;

    #[test]
    fn manifest_defaults_to_current_version_for_shipped_v1_json() {
        // old manifest without manifestVersion -> reads as MANIFEST_VERSION_CURRENT (1)
        let json = r#"{"clusterId":"c","createdAt":"t","platformVersion":"0.2.31","namespaces":[],"resources":[]}"#;
        let m: BackupManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.manifest_version, MANIFEST_VERSION_CURRENT);
        assert_eq!(MANIFEST_VERSION_CURRENT, 1);
    }

    #[test]
    fn manifest_carries_scope_resources_and_platform_version() {
        let m = BackupManifest {
            manifest_version: MANIFEST_VERSION_CURRENT,
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
