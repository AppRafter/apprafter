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

    /// Namespaces the run captured SECRETS from.
    ///
    /// A superset of `namespaces`: a sealed secret is captured wherever it
    /// lives, including a namespace with no `Application` yet — someone
    /// preparing to deploy seals the credentials first, and losing them on
    /// a substrate migration would mean re-creating them by hand for a
    /// deployment that was already half done.
    ///
    /// `#[serde(default)]` so a manifest written before this field reads as
    /// an empty list rather than failing — restore falls back to
    /// `namespaces` there, which is exactly what those backups captured.
    #[serde(default)]
    pub secret_namespaces: Vec<String>,

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

    /// A4: the origin-firewall intent is NOT a manifest field. It briefly was
    /// — an unreleased tree recorded it here from the operator's target store —
    /// and that could only ever work for a hand-run `backup create`: the
    /// scheduled in-cluster runner, which is the default and the recommended
    /// mode, is a CronJob with no target store to read and wrote nothing. The
    /// intent lives in `PlatformStack.spec.firewall.cloudflareOrigin` instead,
    /// which every backup mode captures as a CR.
    ///
    /// A snapshot from that intervening tree still carries the key. Reading one
    /// must not error — a stray field in the manifest is not a reason to refuse
    /// a restore of the data underneath it.
    #[test]
    fn a_manifest_carrying_the_retired_origin_firewall_key_still_parses() {
        let m: BackupManifest = serde_json::from_str(
            r#"{"clusterId":"c","createdAt":"t","platformVersion":"0.2.31","namespaces":[],
                "originFirewall":true,"resources":[]}"#,
        )
        .expect("a retired key must be ignored, not rejected");
        assert_eq!(m.cluster_id, "c");
        assert!(m.resources.is_empty());
    }

    /// The key is not written back either: a round trip through the struct
    /// must not resurrect it into the JSON under a field that no longer exists.
    #[test]
    fn the_retired_origin_firewall_key_is_never_written() {
        let m = BackupManifest {
            manifest_version: MANIFEST_VERSION_CURRENT,
            cluster_id: "c".into(),
            created_at: "t".into(),
            platform_version: "0.2.31".into(),
            namespaces: vec![],
            secret_namespaces: vec![],
            resources: vec![],
        };
        let v = serde_json::to_value(&m).unwrap();
        assert!(
            v.get("originFirewall").is_none(),
            "the manifest must not carry a second source of truth for the \
             origin-firewall intent: {v}"
        );
    }

    #[test]
    fn manifest_carries_scope_resources_and_platform_version() {
        let m = BackupManifest {
            manifest_version: MANIFEST_VERSION_CURRENT,
            cluster_id: "k3d-demo".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            platform_version: "0.2.37".into(),
            namespaces: vec!["demo".into()],
            secret_namespaces: vec!["demo".into(), "staged".into()],
            resources: vec![ResourceRef {
                namespace: "demo".into(),
                kind: "Application".into(),
                name: "alpha".into(),
                claim_type: None,
                no_data: false,
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
