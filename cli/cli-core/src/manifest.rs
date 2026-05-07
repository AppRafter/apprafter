// SPDX-License-Identifier: FSL-1.1-MIT
//! Typed Rust mirror of the CUE `Infrastructure` document, plus a
//! `parse_infrastructure(workdir, path)` helper.
//!
//! The parser shells out to `cue export` (via [`crate::cue::export_in`])
//! and deserialises the resulting JSON into [`InfrastructureManifest`].
//! Defaults are intentionally **not** applied here: callers
//! (currently `platform-cli::commands::apply`) compose manifest
//! values with their own constants so the parser stays a pure
//! shape-translator.

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
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArgocdBlock {
    #[serde(default)]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeSpec {
    pub role: String,
    /// Hetzner-style server type, e.g. "cx22". `type` is a
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
