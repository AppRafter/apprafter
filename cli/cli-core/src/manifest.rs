// SPDX-License-Identifier: FSL-1.1-Apache-2.0
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
    /// Destination namespace declared in the manifest. Application
    /// manifests set it (`metadata.namespace`); Infrastructure manifests
    /// omit it. The `app add` wizard reads it to preselect the namespace
    /// picker. `#[serde(default)]` ⇒ absent in JSON deserializes to `None`.
    #[serde(default)]
    pub namespace: Option<String>,
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
    /// 1.83d: restrict `80`/`443` inbound to Cloudflare's published IP ranges
    /// (origin firewall). Opt-in — absent/false leaves them open to all, so a
    /// non-Cloudflare-fronted cluster is unaffected.
    #[serde(rename = "cloudflareOrigin", default)]
    pub cloudflare_origin: Option<bool>,
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
    pub env: Option<BTreeMap<String, EnvValue>>,
}

/// An `Application.spec.*.env` value (ADR 0046 / 2.12): a literal string,
/// OR a structured reference the cue-cmp renders to a marker object
/// (`{claim: "<type>.<field>"}` / `{secret: "<name>/<key>"}`). The CLI does
/// not consume env values (it parses the manifest only for the env +
/// namespace pickers), so this only has to DESERIALIZE every shape the
/// schema permits — the untagged `Reference(Value)` fallback accepts any
/// non-string marker, current or future, without erroring. (Before 2.12
/// env was `[string]: string`, which now fails to parse a claim/secret
/// reference with "invalid type: map, expected a string".)
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum EnvValue {
    Literal(String),
    Reference(Value),
}

/// An `Application.spec.*.expose` block.
///
/// **Every field is optional, `port` included**, because this type only has to
/// DESERIALISE every shape the schema permits — the CLI never produces one.
/// A per-environment override that changes visibility and nothing else,
///
/// ```cue
/// environments: dev: expose: network: "internal"
/// ```
///
/// is legal at every other layer: `cue vet` accepts it, `apprafter app
/// validate` reports valid, the admission webhook returns no errors, and the
/// operator merges `port` from `base`. While `port` was required here the CLI
/// was the only layer that refused it, and it refused the WHOLE manifest —
/// `app add`'s picker setup (`commands/app.rs`) is the sole caller, and on a
/// parse error it warns, hides the environment picker and skips the namespace
/// preselect. So a manifest the rest of the platform accepts lost two pieces
/// of the wizard.
///
/// Requiring a field here can only ever cost reach, never buy safety: the
/// schema, the CRD and the webhook are where `expose.port`'s presence is
/// actually enforced, on the manifests that reach a cluster.
#[derive(Debug, Clone, Deserialize)]
pub struct ApplicationExpose {
    #[serde(default)]
    pub port: Option<u16>,
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
    fn application_env_accepts_literal_claim_and_secret_refs() {
        // ADR 0046 / 2.12: env values are literal strings OR structured
        // reference markers (`{claim:…}` / `{secret:…}`). The CLI parses the
        // manifest only for the env + namespace pickers and never consumes
        // the values, so it must DESERIALIZE all three without erroring.
        // Regression for "invalid type: map, expected a string" (cli v0.2.26).
        let v = json!({
            "landingCms": {
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "Application",
                "metadata": { "name": "landing-cms", "namespace": "apprafter" },
                "spec": {
                    "base": {
                        "image": "ghcr.io/apprafter/landing-cms:latest",
                        "env": {
                            "LANDING_CMS_PORT": "3000",
                            "DATABASE_URL": { "claim": "pg.url" },
                            "PAYLOAD_SECRET": { "secret": "ns-secrets/PAYLOAD_SECRET" }
                        }
                    },
                    "environments": { "dev": { "replicas": 1 }, "prod": { "replicas": 1 } }
                }
            }
        });
        let parsed =
            parse_application_from_value(&v).expect("mixed literal/claim/secret env must parse");
        assert_eq!(parsed.metadata.namespace.as_deref(), Some("apprafter"));
        let envs: Vec<&String> = parsed.spec.environments.as_ref().unwrap().keys().collect();
        assert_eq!(envs, vec!["dev", "prod"]);
        let base_env = parsed.spec.base.as_ref().unwrap().env.as_ref().unwrap();
        assert!(matches!(
            base_env.get("LANDING_CMS_PORT"),
            Some(EnvValue::Literal(_))
        ));
        assert!(matches!(
            base_env.get("DATABASE_URL"),
            Some(EnvValue::Reference(_))
        ));
        assert!(matches!(
            base_env.get("PAYLOAD_SECRET"),
            Some(EnvValue::Reference(_))
        ));
    }

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
    fn firewall_block_decodes_cloudflare_origin() {
        let fw: FirewallBlock = serde_json::from_value(serde_json::json!({
            "ingress": [{ "port": "443" }],
            "cloudflareOrigin": true
        }))
        .unwrap();
        assert_eq!(fw.cloudflare_origin, Some(true));
        // Absent → None (defaults to off at the apply layer).
        let bare: FirewallBlock = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(bare.cloudflare_origin, None);
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
