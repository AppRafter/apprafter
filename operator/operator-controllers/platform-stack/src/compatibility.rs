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

#[derive(Debug, Deserialize)]
struct CompatibilityDoc {
    compatibility: BTreeMap<String, VersionRecord>,
}

#[derive(Debug, Deserialize)]
struct VersionRecord {
    #[serde(default)]
    change: Option<String>,
}

/// Pull the chart tarball at `<repo>:<version>` from `upstream_url`
/// and return the change class for `version`. The upstream URL
/// follows the same `oci://...` shape passed to
/// `oci::latest_in_channel`.
pub async fn fetch_change_class(
    upstream_url: &str,
    version: &str,
) -> Result<ChangeClass, CompatError> {
    let bare = upstream_url.strip_prefix("oci://").unwrap_or(upstream_url);
    let with_tag = format!("{bare}:{version}");
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
            let record = doc
                .compatibility
                .get(version)
                .ok_or_else(|| CompatError::UnknownVersion(version.to_string()))?;
            return Ok(parse_change_class(record.change.as_deref()));
        }
    }
    Err(CompatError::MissingFile)
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
            let content = b"compatibility:\n  \"0.1.15\":\n    change: safe\n";
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
        assert!(s.contains("0.1.15"));
        assert!(s.contains("change: safe"));
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
}
