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

    /// Was the Cloudflare origin firewall on for the captured cluster (A4)?
    ///
    /// The toggle lives in the operator's LOCAL target store, never in the
    /// cluster, so a restore onto a new target had no way to know the source
    /// restricted its 80/443 — and re-provisioned the node wide open.
    /// `backup create` runs against a resolved target and records the intent
    /// here; `restore --reprovision` carries it to the destination target.
    ///
    /// Three-valued on purpose. `Some(true)`/`Some(false)` are what the source
    /// had; `None` is UNKNOWN — a manifest written before this field existed,
    /// or one written by the in-cluster runner, which has no target store to
    /// read. Restore must treat `None` as "say nothing", never as "the source
    /// had it off".
    ///
    /// `skip_serializing_if` keeps the key out of a manifest that has nothing
    /// to say, so an unknown reads as an absent key in the JSON too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_firewall: Option<bool>,

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

    /// A4: a manifest written before `originFirewall` existed reads as
    /// UNKNOWN. Defaulting it to `false` would let a restore tell an operator
    /// the source cluster had its origin firewall off — a claim about a
    /// cluster the snapshot never recorded anything about.
    #[test]
    fn a_manifest_without_the_origin_firewall_key_reads_as_unknown() {
        let json = r#"{"clusterId":"c","createdAt":"t","platformVersion":"0.2.31","namespaces":[],"resources":[]}"#;
        let m: BackupManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.origin_firewall, None);

        let off: BackupManifest = serde_json::from_str(
            r#"{"clusterId":"c","createdAt":"t","platformVersion":"0.2.31","namespaces":[],
                "originFirewall":false,"resources":[]}"#,
        )
        .unwrap();
        assert_eq!(
            off.origin_firewall,
            Some(false),
            "recorded OFF is not absent"
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
            origin_firewall: Some(true),
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
