// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Fetch + parse `compatibility.yaml` from a published
//! platform-stack chart tarball. Used by the reconciler to
//! classify version diffs (safe / requires-restart /
//! data-migration / breaking) before deciding whether to
//! auto-bump.
//!
//! The chart's `compatibility.yaml` is rendered from
//! `platform-stack/cue/compatibility.cue` at publish time and
//! ships inside the chart tarball as one of the OCI manifest's
//! layers. We pull the tarball via `oci-distribution`, extract
//! the file from the gzipped tar, parse with serde_yaml.

use std::collections::BTreeMap;
use std::io::Read;

use oci_distribution::client::ClientConfig;
use oci_distribution::manifest::OciManifest;
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client, Reference};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeClass {
    Safe,
    RequiresRestart,
    DataMigration,
    Breaking,
}

#[derive(Debug, Error)]
pub enum CompatError {
    #[error("invalid reference {0:?}: {1}")]
    InvalidReference(String, String),
    #[error("registry IO: {0}")]
    Registry(String),
    #[error("chart tarball missing compatibility.yaml")]
    MissingFile,
    #[error("parse compatibility.yaml: {0}")]
    Parse(String),
    #[error("version {0:?} not declared in compatibility.yaml")]
    UnknownVersion(String),
}

/// The rendered `compatibility.yaml` shape that
/// `platform-stack/cue/render_tool.cue` produces is a
/// top-level map keyed by version string (NOT wrapped in a
/// `compatibility:` field). Walk-found bug v0.1.117 → v0.1.118:
/// the old `CompatibilityDoc { compatibility: BTreeMap<..> }`
/// expected an outer wrapper that doesn't exist, so every
/// reconcile that hit `fetch_change_class` errored on
/// `missing field "compatibility"`. Switch to a direct map.
pub type CompatibilityDoc = BTreeMap<String, VersionRecord>;

#[derive(Debug, Clone, Deserialize)]
pub struct VersionRecord {
    #[serde(default)]
    pub change: Option<String>,
    /// Yank marker added in B.1.74a. Defaults to `false` when
    /// the field is absent (older `compatibility.yaml` tarballs
    /// published before the schema bump).
    #[serde(default)]
    pub yanked: bool,
    /// Free-form yank reason; mandatory when `yanked == true`,
    /// surfaced verbatim in the `YankedVersion` condition
    /// message. CI guards the conditional requirement at chart
    /// publish time (see
    /// `.github/workflows/platform-stack-publish.yml`).
    #[serde(default, rename = "yankedReason")]
    pub yanked_reason: Option<String>,
}

/// Pull the chart tarball at `<repo>:<version_tag>` from
/// `upstream_url` and return the full parsed compatibility
/// document. The `version_tag` is the OCI tag to query (not
/// the version-being-classified); typically the
/// channel-latest tag, whose `compatibility.yaml` contains
/// records for itself and every prior published version.
///
/// Callers reuse the returned map for: change-class lookup
/// (target version's `change` field), yank filtering (walk
/// candidates desc, skip entries with `yanked == true`), and
/// `YankedVersion` condition messaging (read
/// `yanked_reason`).
///
/// One OCI manifest pull + one blob pull per call.
pub async fn fetch_compatibility_doc(
    upstream_url: &str,
    version_tag: &str,
) -> Result<CompatibilityDoc, CompatError> {
    let bare = upstream_url.strip_prefix("oci://").unwrap_or(upstream_url);
    let with_tag = format!("{bare}:{version_tag}");
    let reference: Reference = with_tag
        .parse()
        .map_err(|e: oci_distribution::ParseError| {
            CompatError::InvalidReference(with_tag.clone(), e.to_string())
        })?;

    let client = Client::new(ClientConfig::default());
    let (manifest, _digest) = client
        .pull_manifest(&reference, &RegistryAuth::Anonymous)
        .await
        .map_err(|e| CompatError::Registry(e.to_string()))?;
    let manifest = match manifest {
        OciManifest::Image(m) => m,
        OciManifest::ImageIndex(_) => {
            return Err(CompatError::Registry(
                "expected image manifest, got image index".into(),
            ))
        }
    };

    // Helm charts publish a single layer of mediaType
    // `application/vnd.cncf.helm.chart.content.v1.tar+gzip`
    // containing a gzipped tarball with files under
    // `<chartname>/`. Tolerate >1 layer just in case future Helm
    // versions split content; iterate and pick the first that
    // yields a compatibility.yaml.
    for layer in manifest.layers {
        let mut blob = Vec::new();
        client
            .pull_blob(&reference, &layer, &mut blob)
            .await
            .map_err(|e| CompatError::Registry(e.to_string()))?;
        if let Ok(yaml_bytes) = extract_compatibility_yaml(&blob) {
            let doc: CompatibilityDoc = serde_yaml::from_slice(&yaml_bytes)
                .map_err(|e| CompatError::Parse(e.to_string()))?;
            return Ok(doc);
        }
    }
    Err(CompatError::MissingFile)
}

/// Convenience wrapper kept for callers that still need just
/// the change class. Internally fetches the full
/// `compatibility.yaml` map then extracts the requested
/// version's `change` field.
pub async fn fetch_change_class(
    upstream_url: &str,
    version: &str,
) -> Result<ChangeClass, CompatError> {
    let doc = fetch_compatibility_doc(upstream_url, version).await?;
    let record = doc
        .get(version)
        .ok_or_else(|| CompatError::UnknownVersion(version.to_string()))?;
    Ok(parse_change_class(record.change.as_deref()))
}

/// Classify a transition path from `from_version` к
/// `to_version` based on the MOST DESTRUCTIVE classification
/// encountered in the (from, to] range. Pulls the
/// compatibility doc at `to_version`'s tarball (which contains
/// records for every prior published version up к to_version)
/// и walks each record whose semver lies in the half-open
/// range.
///
/// Why path-max и not just the target's class — walk-fix #8
/// post-B.1.78: with single-target-class semantics, jumping
/// from 0.1.A directly к 0.1.C silently bypasses any
/// destructive transitions in 0.1.B (or any version strictly
/// between A и C). Operator-approved/rejected decisions on
/// those intermediate versions don't carry forward к the
/// new pair, but the destructive content does. Path-max
/// guarantees the strictest classification along the way
/// governs the transition.
///
/// Downgrade (`from > to`) returns Safe — no destructive
/// transitions on the way back unless каждый интервалс
/// `compatibility.yaml` separately records destructive
/// downgrade semantics (which they don't today). spec.md
/// doesn't address downgrade direction; conservative
/// behaviour deferred.
pub async fn fetch_path_max_change_class(
    upstream_url: &str,
    from_version: &str,
    to_version: &str,
) -> Result<ChangeClass, CompatError> {
    let doc = fetch_compatibility_doc(upstream_url, to_version).await?;
    Ok(path_max_change_class(&doc, from_version, to_version))
}

/// Pure path-max computation. Pulled out так что unit tests
/// can exercise the range walk без a kube cluster.
pub fn path_max_change_class(
    doc: &CompatibilityDoc,
    from_version: &str,
    to_version: &str,
) -> ChangeClass {
    let from = semver::Version::parse(from_version).ok();
    let to = semver::Version::parse(to_version).ok();
    let (from, to) = match (from, to) {
        (Some(f), Some(t)) => (f, t),
        _ => return ChangeClass::Breaking, // unparseable — fail-closed
    };
    // Downgrade or no-op transition — no path к gate.
    if from >= to {
        return ChangeClass::Safe;
    }
    let mut max = ChangeClass::Safe;
    for (version_key, record) in doc {
        let v = match semver::Version::parse(version_key) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v > from && v <= to {
            let class = parse_change_class(record.change.as_deref());
            if class_order(class) > class_order(max) {
                max = class;
            }
        }
    }
    max
}

/// Strict ordering on `ChangeClass` (Safe < RequiresRestart <
/// DataMigration < Breaking) so `path_max_change_class` can
/// reduce the range to a single value. Not exposed as `Ord`
/// on the enum itself к keep the type's public surface
/// minimal — only this module needs the ordering.
fn class_order(c: ChangeClass) -> u8 {
    match c {
        ChangeClass::Safe => 0,
        ChangeClass::RequiresRestart => 1,
        ChangeClass::DataMigration => 2,
        ChangeClass::Breaking => 3,
    }
}

/// Walk a gzipped chart tarball blob and return the contents of
/// the `*/compatibility.yaml` file. `*` is the chart's top-level
/// directory inside the tar (Helm convention).
fn extract_compatibility_yaml(gzipped: &[u8]) -> Result<Vec<u8>, CompatError> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(gzipped);
    let mut archive = Archive::new(decoder);
    for entry in archive
        .entries()
        .map_err(|e| CompatError::Parse(e.to_string()))?
    {
        let mut entry = entry.map_err(|e| CompatError::Parse(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| CompatError::Parse(e.to_string()))?
            .into_owned();
        if path.file_name().and_then(|n| n.to_str()) == Some("compatibility.yaml") {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| CompatError::Parse(e.to_string()))?;
            return Ok(buf);
        }
    }
    Err(CompatError::MissingFile)
}

fn parse_change_class(s: Option<&str>) -> ChangeClass {
    match s {
        Some("safe") => ChangeClass::Safe,
        Some("requires-restart") => ChangeClass::RequiresRestart,
        Some("data-migration") => ChangeClass::DataMigration,
        Some("breaking") => ChangeClass::Breaking,
        // Default to breaking when unspecified — fail-closed.
        _ => ChangeClass::Breaking,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_change_class_known_values() {
        assert_eq!(parse_change_class(Some("safe")), ChangeClass::Safe);
        assert_eq!(
            parse_change_class(Some("requires-restart")),
            ChangeClass::RequiresRestart
        );
        assert_eq!(
            parse_change_class(Some("data-migration")),
            ChangeClass::DataMigration
        );
        assert_eq!(parse_change_class(Some("breaking")), ChangeClass::Breaking);
    }

    #[test]
    fn parse_change_class_defaults_to_breaking_when_unset() {
        // Fail-closed: an undeclared change class is the most
        // conservative interpretation.
        assert_eq!(parse_change_class(None), ChangeClass::Breaking);
        assert_eq!(
            parse_change_class(Some("unknown-future-class")),
            ChangeClass::Breaking
        );
    }

    // extract_compatibility_yaml smoke test: build a tiny in-memory
    // gzipped tar with a `chart/compatibility.yaml` file and confirm
    // the extractor returns the bytes.
    #[test]
    fn extract_finds_compatibility_yaml_in_synthetic_tarball() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tar::Builder;

        let mut tar_bytes = Vec::new();
        {
            let enc = GzEncoder::new(&mut tar_bytes, Compression::default());
            let mut builder = Builder::new(enc);
            // Matches the shape `platform-stack/cue/render_tool.cue`
            // emits: top-level map keyed by version string. NO
            // wrapping `compatibility:` field — walk-found bug
            // v0.1.117 → v0.1.118.
            let content = b"\"0.1.19\":\n  version: \"0.1.19\"\n  change: safe\n  operatorVersion: v0.1.118\n";
            let mut header = tar::Header::new_gnu();
            header
                .set_path("platform-stack/compatibility.yaml")
                .unwrap();
            header.set_size(content.len() as u64);
            header.set_cksum();
            builder.append(&header, &content[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let extracted = extract_compatibility_yaml(&tar_bytes).unwrap();
        let s = std::str::from_utf8(&extracted).unwrap();
        assert!(s.contains("0.1.19"));
        assert!(s.contains("change: safe"));
    }

    #[test]
    fn compatibility_doc_parses_top_level_version_map() {
        // Regression guard for walk-fix v0.1.117 → v0.1.118. The
        // chart renderer emits compatibility.yaml as a
        // top-level map keyed by semver string — NOT wrapped in
        // a `compatibility:` object. Parser must accept this
        // shape directly.
        //
        // Raw byte literal (`br#"..."#`) — escape-free so the
        // YAML indentation survives literal newlines. The naive
        // `b"...\n  ...\n"` form with `\<newline>` line
        // continuations eats the leading whitespace of the next
        // line and corrupts indentation.
        let yaml: &[u8] = br#""0.1.18":
  version: "0.1.18"
  change: safe
  operatorVersion: v0.1.116
"0.1.19":
  version: "0.1.19"
  change: safe
  operatorVersion: v0.1.117
"0.2.0":
  version: "0.2.0"
  change: breaking
  operatorVersion: v0.2.0
"#;
        let doc: CompatibilityDoc =
            serde_yaml::from_slice(yaml).expect("top-level version map parses");
        assert_eq!(doc.len(), 3);
        assert_eq!(
            doc.get("0.1.19").and_then(|r| r.change.as_deref()),
            Some("safe")
        );
        assert_eq!(
            doc.get("0.2.0").and_then(|r| r.change.as_deref()),
            Some("breaking")
        );
    }

    #[test]
    fn compatibility_doc_tolerates_extra_fields_per_record() {
        // The rendered version record carries `version`,
        // `change`, `operatorVersion`, `notes`, `references[]`,
        // plus the B.1.74a `yanked` + `yankedReason` fields.
        // serde_yaml's deserializer ignores unknown fields by
        // default. Pin this so a future field bump on the chart
        // side doesn't break PlatformController.
        let yaml: &[u8] = br#""0.1.19":
  version: "0.1.19"
  change: safe
  operatorVersion: v0.1.117
  notes: "multi-line notes here"
  references:
    - docs/...
    - another
  futureField: 42
"#;
        let doc: CompatibilityDoc = serde_yaml::from_slice(yaml).expect("extra fields tolerated");
        assert_eq!(
            doc.get("0.1.19").and_then(|r| r.change.as_deref()),
            Some("safe")
        );
    }

    #[test]
    fn version_record_yanked_defaults_to_false_when_field_absent() {
        // B.1.74a contract: older `compatibility.yaml` tarballs
        // published before the schema bump have no `yanked`
        // field. PlatformController must treat the absence as
        // "not yanked" — never panic, never error.
        let yaml: &[u8] = br#""0.1.10":
  version: "0.1.10"
  change: safe
  operatorVersion: v0.1.91
"#;
        let doc: CompatibilityDoc = serde_yaml::from_slice(yaml).expect("missing yanked tolerated");
        let rec = doc.get("0.1.10").expect("entry present");
        assert!(!rec.yanked);
        assert!(rec.yanked_reason.is_none());
    }

    #[test]
    fn version_record_yanked_true_with_camelcase_reason_parses() {
        // Regression guard for B.1.74a. The CUE schema emits
        // `yankedReason` (camelCase, matching the rest of the
        // chart's value names); the Rust deserializer maps via
        // `#[serde(rename = "yankedReason")]` onto the
        // `yanked_reason` field. Forgetting the rename would
        // silently drop the reason and leave the
        // `YankedVersion` condition message empty in the
        // cluster.
        let yaml: &[u8] = br#""0.1.21":
  version: "0.1.21"
  change: safe
  operatorVersion: v0.1.118
  yanked: true
  yankedReason: "regression in PlatformController OCI poll"
"#;
        let doc: CompatibilityDoc = serde_yaml::from_slice(yaml).expect("yanked entry parses");
        let rec = doc.get("0.1.21").expect("entry present");
        assert!(rec.yanked);
        assert_eq!(
            rec.yanked_reason.as_deref(),
            Some("regression in PlatformController OCI poll")
        );
    }

    #[test]
    fn extract_returns_missing_file_when_tarball_has_no_match() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tar::Builder;

        let mut tar_bytes = Vec::new();
        {
            let enc = GzEncoder::new(&mut tar_bytes, Compression::default());
            let mut builder = Builder::new(enc);
            let content = b"not the right file\n";
            let mut header = tar::Header::new_gnu();
            header.set_path("platform-stack/values.yaml").unwrap();
            header.set_size(content.len() as u64);
            header.set_cksum();
            builder.append(&header, &content[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let err = extract_compatibility_yaml(&tar_bytes).unwrap_err();
        assert!(matches!(err, CompatError::MissingFile));
    }

    fn record(change: &str) -> VersionRecord {
        VersionRecord {
            change: Some(change.to_string()),
            yanked: false,
            yanked_reason: None,
        }
    }

    #[test]
    fn path_max_change_class_picks_strictest_in_range() {
        // Walk-fix #8 contract: bump 0.1.35 → 0.1.37 must
        // surface 0.1.36's breaking class even though 0.1.37
        // alone is `safe`. The strictest in (from, to] wins.
        let mut doc = BTreeMap::new();
        doc.insert("0.1.34".to_string(), record("safe"));
        doc.insert("0.1.35".to_string(), record("safe"));
        doc.insert("0.1.36".to_string(), record("breaking"));
        doc.insert("0.1.37".to_string(), record("safe"));
        let class = path_max_change_class(&doc, "0.1.35", "0.1.37");
        assert!(matches!(class, ChangeClass::Breaking));
    }

    #[test]
    fn path_max_change_class_excludes_from_version() {
        // Half-open range (from, to] — `from`'s own class
        // doesn't count (operator already accepted it, by
        // definition: it's the current state). Only versions
        // strictly greater than `from`, up к и including
        // `to`, participate в the max.
        let mut doc = BTreeMap::new();
        doc.insert("0.1.35".to_string(), record("breaking")); // <- excluded
        doc.insert("0.1.36".to_string(), record("safe"));
        doc.insert("0.1.37".to_string(), record("safe"));
        let class = path_max_change_class(&doc, "0.1.35", "0.1.37");
        assert!(matches!(class, ChangeClass::Safe));
    }

    #[test]
    fn path_max_change_class_returns_safe_for_no_op_transition() {
        // from == to → empty range → Safe.
        let mut doc = BTreeMap::new();
        doc.insert("0.1.37".to_string(), record("breaking"));
        let class = path_max_change_class(&doc, "0.1.37", "0.1.37");
        assert!(matches!(class, ChangeClass::Safe));
    }

    #[test]
    fn path_max_change_class_returns_safe_for_downgrade() {
        // spec.md doesn't address downgrade direction;
        // conservative default — Safe. Future work: scan
        // (to, from] also for cumulative reverse-direction
        // destructiveness.
        let mut doc = BTreeMap::new();
        doc.insert("0.1.35".to_string(), record("safe"));
        doc.insert("0.1.36".to_string(), record("breaking"));
        doc.insert("0.1.37".to_string(), record("safe"));
        let class = path_max_change_class(&doc, "0.1.37", "0.1.35");
        assert!(matches!(class, ChangeClass::Safe));
    }

    #[test]
    fn path_max_change_class_picks_requires_restart_over_safe() {
        // RequiresRestart strictly more destructive than Safe.
        let mut doc = BTreeMap::new();
        doc.insert("0.1.36".to_string(), record("requires-restart"));
        doc.insert("0.1.37".to_string(), record("safe"));
        let class = path_max_change_class(&doc, "0.1.35", "0.1.37");
        assert!(matches!(class, ChangeClass::RequiresRestart));
    }

    #[test]
    fn path_max_change_class_picks_data_migration_over_requires_restart() {
        // DataMigration ranks higher than RequiresRestart.
        let mut doc = BTreeMap::new();
        doc.insert("0.1.36".to_string(), record("requires-restart"));
        doc.insert("0.1.37".to_string(), record("data-migration"));
        let class = path_max_change_class(&doc, "0.1.35", "0.1.37");
        assert!(matches!(class, ChangeClass::DataMigration));
    }

    #[test]
    fn path_max_change_class_picks_breaking_over_data_migration() {
        // Breaking is the strictest class.
        let mut doc = BTreeMap::new();
        doc.insert("0.1.36".to_string(), record("data-migration"));
        doc.insert("0.1.37".to_string(), record("breaking"));
        let class = path_max_change_class(&doc, "0.1.35", "0.1.37");
        assert!(matches!(class, ChangeClass::Breaking));
    }

    #[test]
    fn path_max_change_class_skips_unparseable_version_keys() {
        // Garbage version strings in the doc don't break the
        // walk — they're ignored, и valid entries still get
        // classified.
        let mut doc = BTreeMap::new();
        doc.insert("0.1.36".to_string(), record("breaking"));
        doc.insert("not-a-version".to_string(), record("data-migration"));
        let class = path_max_change_class(&doc, "0.1.35", "0.1.37");
        assert!(matches!(class, ChangeClass::Breaking));
    }
}
