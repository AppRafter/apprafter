// SPDX-License-Identifier: FSL-1.1-MIT
//! Typed Rust mirror of the CUE `Infrastructure` document, plus a
//! `parse_infrastructure(workdir, path)` helper.
//!
//! The parser shells out to `cue export` (via [`crate::cue::export_in`])
//! and deserialises the resulting JSON into [`InfrastructureManifest`].
//! Defaults are intentionally **not** applied here: callers
//! (currently `apprafter::commands::apply`) compose manifest
//! values with their own constants so the parser stays a pure
//! shape-translator.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::{cue, CliError, Result};

/// Top-level CUE-exported document for a `kind: Infrastructure`
/// manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct InfrastructureManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: InfrastructureSpec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InfrastructureSpec {
    pub provider: String,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub network: Option<NetworkBlock>,
    #[serde(default)]
    pub firewall: Option<FirewallBlock>,
    #[serde(rename = "sshKeys", default)]
    pub ssh_keys: Option<Vec<SshKeyBlock>>,
    #[serde(rename = "osImage", default)]
    pub os_image: Option<String>,
    #[serde(default)]
    pub argocd: Option<ArgocdBlock>,
    #[serde(default)]
    pub backstage: Option<BackstageBlock>,
    #[serde(default)]
    pub operator: Option<OperatorBlock>,
    #[serde(default, rename = "admissionWebhook")]
    pub admission_webhook: Option<AdmissionWebhookBlock>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ArgocdBlock {
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(rename = "bootstrapRepo", default)]
    pub bootstrap_repo: Option<String>,
    #[serde(rename = "bootstrapPath", default)]
    pub bootstrap_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BackstageBlock {
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AdmissionWebhookBlock {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OperatorBlock {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeSpec {
    pub role: String,
    /// Hetzner-style server type, e.g. "cpx22". `type` is a
    /// keyword in Rust, so the field is renamed.
    #[serde(rename = "type")]
    pub kind: String,
    pub count: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkBlock {
    #[serde(default)]
    pub ip_range: Option<String>,
    #[serde(default)]
    pub subnet: Option<SubnetBlock>,
    #[serde(rename = "floatingIPs", default)]
    pub floating_ips: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubnetBlock {
    #[serde(default)]
    pub ip_range: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FirewallBlock {
    #[serde(default)]
    pub ingress: Option<Vec<FirewallIngressRule>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FirewallIngressRule {
    pub port: String,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub source_ips: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SshKeyBlock {
    #[serde(default)]
    pub name: Option<String>,
    pub public_key: String,
}

/// Run `cue export <path> --out json` from `workdir` and parse
/// the result as an [`InfrastructureManifest`].
///
/// The CUE document is expected to expose a single top-level
/// field whose value is the manifest. The example fixture uses
/// `infra: v1alpha1.#Infrastructure & { … }`; the parser walks
/// the exported object and picks the first value that looks like
/// an `Infrastructure`.
pub fn parse_infrastructure(workdir: &Path, path: &Path) -> Result<InfrastructureManifest> {
    let value = cue::export_in(workdir, path)?;
    parse_infrastructure_from_value(&value)
}

fn parse_infrastructure_from_value(value: &Value) -> Result<InfrastructureManifest> {
    // CUE export of `package examples` containing
    // `infra: v1alpha1.#Infrastructure & { … }` yields:
    //   { "infra": { "apiVersion": "...", "kind": "...", ... } }
    // Walk the top-level object and return the first value whose
    // `kind == "Infrastructure"`.
    let obj = value
        .as_object()
        .ok_or_else(|| CliError::Other("cue export did not yield a JSON object".to_string()))?;

    for (_, candidate) in obj {
        if candidate
            .get("kind")
            .and_then(Value::as_str)
            .map(|k| k == "Infrastructure")
            .unwrap_or(false)
        {
            return serde_json::from_value(candidate.clone()).map_err(CliError::from);
        }
    }
    Err(CliError::Other(
        "cue export did not contain an Infrastructure document".to_string(),
    ))
}

/// Top-level CUE-exported document for a `kind: Application` manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct ApplicationManifest {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    #[serde(default)]
    pub spec: ApplicationOuterSpec,
}

/// `spec` block of an Application — wraps the base config and the
/// per-environment overrides.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApplicationOuterSpec {
    #[serde(default)]
    pub base: Option<ApplicationSpec>,
    #[serde(default)]
    pub environments: Option<BTreeMap<String, ApplicationSpec>>,
}

/// Mirror of CUE `#ApplicationSpec` — used both as the `base` block
/// and as the value of every `environments[name]` entry.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApplicationSpec {
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub replicas: Option<u32>,
    #[serde(default)]
    pub expose: Option<ApplicationExpose>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApplicationExpose {
    pub port: u16,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default)]
    pub network: Option<String>,
}

/// Run `cue export <path> --out json` from `workdir` and parse the
/// result as an [`ApplicationManifest`]. Walks the top-level object
/// the same way `parse_infrastructure` does, picking the first
/// value whose `kind == "Application"`.
pub fn parse_application(workdir: &Path, path: &Path) -> Result<ApplicationManifest> {
    let value = cue::export_in(workdir, path)?;
    parse_application_from_value(&value)
}

fn parse_application_from_value(value: &Value) -> Result<ApplicationManifest> {
    let obj = value
        .as_object()
        .ok_or_else(|| CliError::Other("cue export did not yield a JSON object".to_string()))?;

    for (_, candidate) in obj {
        if candidate
            .get("kind")
            .and_then(Value::as_str)
            .map(|k| k == "Application")
            .unwrap_or(false)
        {
            return serde_json::from_value(candidate.clone()).map_err(CliError::from);
        }
    }
    Err(CliError::Other(
        "cue export did not contain an Application document".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn operator_block_parses_all_three_fields() {
        let v = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "operator": {
                    "enabled": false,
                    "image": "ghcr.io/x/op",
                    "tag": "v0.1.65"
                }
            }
        });
        let parsed: InfrastructureManifest = serde_json::from_value(v).unwrap();
        let op = parsed.spec.operator.unwrap();
        assert_eq!(op.enabled, Some(false));
        assert_eq!(op.image.as_deref(), Some("ghcr.io/x/op"));
        assert_eq!(op.tag.as_deref(), Some("v0.1.65"));
    }

    #[test]
    fn operator_block_absent_yields_none() {
        let v = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {"provider": "hetzner-cloud"}
        });
        let parsed: InfrastructureManifest = serde_json::from_value(v).unwrap();
        assert!(parsed.spec.operator.is_none());
        assert!(parsed.spec.admission_webhook.is_none());
    }

    #[test]
    fn admission_webhook_block_parses_all_fields() {
        let v = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "admissionWebhook": {
                    "enabled": true,
                    "image": "ghcr.io/x/aw",
                    "tag": "v0.1.65"
                }
            }
        });
        let parsed: InfrastructureManifest = serde_json::from_value(v).unwrap();
        let aw = parsed.spec.admission_webhook.unwrap();
        assert_eq!(aw.enabled, Some(true));
        assert_eq!(aw.image.as_deref(), Some("ghcr.io/x/aw"));
        assert_eq!(aw.tag.as_deref(), Some("v0.1.65"));
    }

    #[test]
    fn admission_webhook_block_partial_only_image() {
        // Backward-compat shape: just `image`, no enabled / tag.
        let v = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Infrastructure",
            "metadata": {"name": "p"},
            "spec": {
                "provider": "hetzner-cloud",
                "admissionWebhook": {"image": "ghcr.io/x/aw"}
            }
        });
        let parsed: InfrastructureManifest = serde_json::from_value(v).unwrap();
        let aw = parsed.spec.admission_webhook.unwrap();
        assert_eq!(aw.image.as_deref(), Some("ghcr.io/x/aw"));
        assert_eq!(aw.enabled, None);
        assert_eq!(aw.tag, None);
    }
}
