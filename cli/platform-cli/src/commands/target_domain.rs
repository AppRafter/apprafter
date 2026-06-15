// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter target domain {add,list,remove}` (1.83f) — register apex zones in
//! `PlatformStack.spec.values.gateway.allowedDomains[]` so the platform Gateway
//! serves them (apex + wildcard :443 listeners) from an imported cert. The pure
//! helpers are unit-tested; the live merge-patch + Gateway generation ride the
//! track-end manual walk.

use std::path::Path;
use std::process::Command;

use cli_core::{CliError, Result};
use serde_json::{json, Value};
use tabled::{settings::Style, Table, Tabled};

use crate::cli::TargetDomainCommand;
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_merge_patch,
};

const PLATFORMSTACK_NAME: &str = "default";
const PLATFORMSTACK_NAMESPACE: &str = "apprafter-system";
const CERT_NAMESPACE: &str = "apprafter-system";

pub fn run(action: TargetDomainCommand) -> Result<()> {
    match action {
        TargetDomainCommand::Add {
            domain,
            cert,
            added_by,
        } => run_domain_add(&domain, &cert, added_by),
        TargetDomainCommand::List => run_domain_list(),
        TargetDomainCommand::Remove { domain, force } => run_domain_remove(&domain, force),
    }
}

// ---- pure helpers (unit-tested) -------------------------------------------

/// Validate an apex domain: lower-case RFC-1123 hostname, NO "*." prefix (the
/// chart generates the wildcard listener from the apex). Slice — apex only.
pub fn validate_apex_domain(domain: &str) -> Result<()> {
    let bad = |msg: &str| Err(CliError::Other(format!("invalid domain '{domain}': {msg}")));
    if domain.is_empty() {
        return bad("must not be empty");
    }
    if domain.starts_with("*.") {
        return bad("apex only — drop the '*.' (the wildcard listener is generated from the apex)");
    }
    if domain.starts_with('.') || domain.ends_with('.') {
        return bad("no leading or trailing dot");
    }
    if domain.len() > 253 {
        return bad("too long (max 253 chars)");
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return bad("must be a registrable domain (at least two labels)");
    }
    for label in &labels {
        if label.is_empty() || label.len() > 63 {
            return bad("each label must be 1-63 chars");
        }
        let ok = label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-');
        if !ok {
            return bad("labels are lower-case [a-z0-9-], no leading/trailing dash");
        }
    }
    Ok(())
}

/// Build one `allowedDomains` entry (the 4.1b-locked shape).
pub fn build_domain_entry(domain: &str, cert_ref: &str, added_at: &str, added_by: &str) -> Value {
    json!({
        "domain": domain,
        "certMode": "imported",
        "importedCertRef": cert_ref,
        "addedAt": added_at,
        "addedBy": added_by,
    })
}

/// Append `entry` to `current`, rejecting a duplicate `domain`.
pub fn allowed_domains_after_add(current: &[Value], entry: Value) -> Result<Vec<Value>> {
    let new_domain = entry
        .get("domain")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if current
        .iter()
        .any(|e| e.get("domain").and_then(Value::as_str) == Some(new_domain))
    {
        return Err(CliError::Other(format!(
            "Domain already registered: {new_domain}. \
             Use apprafter target domain remove {new_domain} to swap."
        )));
    }
    let mut out = current.to_vec();
    out.push(entry);
    Ok(out)
}

/// Drop the entry whose `domain == domain`; returns (new_array, removed?).
pub fn allowed_domains_after_remove(current: &[Value], domain: &str) -> (Vec<Value>, bool) {
    let mut removed = false;
    let out: Vec<Value> = current
        .iter()
        .filter(|e| {
            let keep = e.get("domain").and_then(Value::as_str) != Some(domain);
            if !keep {
                removed = true;
            }
            keep
        })
        .cloned()
        .collect();
    (out, removed)
}

/// Does `hostname` fall under `domain`? True if `hostname == domain` (apex) or a
/// SINGLE-label subdomain `<label>.<domain>`. `a.b.<domain>` (two labels) false.
pub fn hostname_matches_domain(hostname: &str, domain: &str) -> bool {
    if hostname == domain {
        return true;
    }
    match hostname.strip_suffix(domain) {
        Some(prefix) => match prefix.strip_suffix('.') {
            Some(label) => !label.is_empty() && !label.contains('.'),
            None => false,
        },
        None => false,
    }
}

/// App names (from `kubectl get applications.apprafter.io -A -o json`) whose base
/// or per-env `expose.hostname` (a JSON string OR array) matches `domain`.
/// Deduped + sorted.
pub fn apps_using_domain(apps_json: &Value, domain: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let items = apps_json
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for app in &items {
        let app_name = app
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if app_name.is_empty() || names.contains(&app_name) {
            continue;
        }
        let mut hostnames: Vec<String> = Vec::new();
        collect_hostnames(app.pointer("/spec/base/expose/hostname"), &mut hostnames);
        if let Some(envs) = app.pointer("/spec/environments").and_then(Value::as_object) {
            for env in envs.values() {
                collect_hostnames(env.pointer("/expose/hostname"), &mut hostnames);
            }
        }
        if hostnames.iter().any(|h| hostname_matches_domain(h, domain)) {
            names.push(app_name);
        }
    }
    names.sort();
    names
}

fn collect_hostnames(node: Option<&Value>, out: &mut Vec<String>) {
    match node {
        Some(Value::String(s)) => out.push(s.clone()),
        Some(Value::Array(arr)) => {
            for v in arr {
                if let Some(s) = v.as_str() {
                    out.push(s.to_string());
                }
            }
        }
        _ => {}
    }
}

fn resolve_added_by(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string())
}

// ---- command plumbing ------------------------------------------------------

fn read_platformstack(kc: &Path) -> Result<Value> {
    kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc,
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "PlatformStack '{PLATFORMSTACK_NAME}' not found in {PLATFORMSTACK_NAMESPACE}. \
             Bootstrap the cluster first (apprafter cluster-bootstrap)."
        ))
    })
}

fn read_allowed_domains(stack: &Value) -> Vec<Value> {
    stack
        .pointer("/spec/values/gateway/allowedDomains")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn merge_patch_allowed_domains(domains: &[Value], kc: &Path) -> Result<()> {
    let patch = json!({ "spec": { "values": { "gateway": { "allowedDomains": domains } } } });
    let body = serde_json::to_string(&patch)
        .map_err(|e| CliError::Other(format!("serialize patch: {e}")))?;
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc,
    )
}

/// `kubectl get applications.apprafter.io -A -o json` → parsed list JSON.
fn list_applications(kc: &Path) -> Result<Value> {
    let out = Command::new("kubectl")
        .arg("get")
        .arg("applications.apprafter.io")
        .arg("-A")
        .arg("-o")
        .arg("json")
        .env("KUBECONFIG", kc)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl get applications.apprafter.io failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))
}

fn run_domain_add(domain: &str, cert: &str, added_by: Option<String>) -> Result<()> {
    validate_apex_domain(domain)?;
    let kc = ensure_kubeconfig_tempfile()?;

    let secret = kubectl_get_json("secret", Some(cert), Some(CERT_NAMESPACE), kc.path())?;
    let cert_ok = secret
        .as_ref()
        .and_then(|s| s.pointer("/metadata/labels/apprafter.io~1cert-mode"))
        .and_then(Value::as_str)
        == Some("imported");
    if !cert_ok {
        return Err(CliError::Other(format!(
            "Cert '{cert}' not found. \
             Run apprafter target cert import {cert} --cert ... --key ... first."
        )));
    }

    let stack = read_platformstack(kc.path())?;
    let current = read_allowed_domains(&stack);
    let entry = build_domain_entry(
        domain,
        cert,
        &chrono::Utc::now().to_rfc3339(),
        &resolve_added_by(added_by),
    );
    let updated = allowed_domains_after_add(&current, entry)?;
    merge_patch_allowed_domains(&updated, kc.path())?;

    println!("✓ Domain '{domain}' registered (cert '{cert}').");
    println!(
        "  Point your domain's DNS at the node — run `apprafter target ip` for the A/AAAA values — \
         proxied through Cloudflare (orange-cloud, SSL/TLS Full (strict))."
    );
    println!("See your zones: apprafter target domain list");
    Ok(())
}

#[derive(Tabled)]
struct DomainListRow {
    #[tabled(rename = "Domain")]
    domain: String,
    #[tabled(rename = "Cert")]
    cert: String,
    #[tabled(rename = "Apps")]
    apps: String,
    #[tabled(rename = "Added At")]
    added_at: String,
    #[tabled(rename = "Added By")]
    added_by: String,
}

fn run_domain_list() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let stack = read_platformstack(kc.path())?;
    let domains = read_allowed_domains(&stack);
    if domains.is_empty() {
        println!("No domains registered. Run apprafter target domain add <zone> --cert <name>.");
        return Ok(());
    }
    let apps_json = list_applications(kc.path())?;
    let field = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).unwrap_or("-").to_string();
    let rows: Vec<DomainListRow> = domains
        .iter()
        .map(|e| {
            let domain = field(e, "domain");
            let apps = apps_using_domain(&apps_json, &domain).len().to_string();
            DomainListRow {
                cert: field(e, "importedCertRef"),
                apps,
                added_at: field(e, "addedAt"),
                added_by: field(e, "addedBy"),
                domain,
            }
        })
        .collect();
    let mut table = Table::new(&rows);
    table.with(Style::sharp());
    println!("{table}");
    Ok(())
}

fn run_domain_remove(domain: &str, force: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let stack = read_platformstack(kc.path())?;
    let current = read_allowed_domains(&stack);

    let (trimmed, removed) = allowed_domains_after_remove(&current, domain);
    if !removed {
        return Err(CliError::Other(format!("Domain not registered: {domain}")));
    }

    let using = apps_using_domain(&list_applications(kc.path())?, domain);
    if !using.is_empty() {
        if !force {
            return Err(CliError::Other(format!(
                "{} application(s) using {domain}: [{}]. \
                 Remove apps first or use --force (apps will lose external access).",
                using.len(),
                using.join(", ")
            )));
        }
        for app in &using {
            eprintln!(
                "{}",
                cli_core::style::warn(&format!(
                    "app '{app}' uses {domain} — it will lose external access"
                ))
            );
        }
    }

    merge_patch_allowed_domains(&trimmed, kc.path())?;
    println!(
        "✓ Domain '{domain}' unregistered. (The cert Secret is kept — remove it separately if unused.)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_apex_accepts_and_rejects() {
        assert!(validate_apex_domain("apprafter.dev").is_ok());
        assert!(validate_apex_domain("sub.apprafter.dev").is_ok());
        assert!(validate_apex_domain("*.apprafter.dev").is_err());
        assert!(validate_apex_domain("").is_err());
        assert!(validate_apex_domain("Apprafter.dev").is_err());
        assert!(validate_apex_domain("apprafter.dev.").is_err());
        assert!(validate_apex_domain("nodash").is_err());
    }

    #[test]
    fn build_entry_has_locked_shape() {
        let e = build_domain_entry(
            "apprafter.dev",
            "cf-cert",
            "2026-06-15T00:00:00+00:00",
            "rem",
        );
        assert_eq!(e["domain"], "apprafter.dev");
        assert_eq!(e["certMode"], "imported");
        assert_eq!(e["importedCertRef"], "cf-cert");
        assert_eq!(e["addedAt"], "2026-06-15T00:00:00+00:00");
        assert_eq!(e["addedBy"], "rem");
    }

    #[test]
    fn add_appends_and_rejects_dup() {
        let cur = vec![build_domain_entry("a.dev", "c", "t", "u")];
        let added =
            allowed_domains_after_add(&cur, build_domain_entry("b.dev", "c2", "t", "u")).unwrap();
        assert_eq!(added.len(), 2);
        let err = allowed_domains_after_add(&cur, build_domain_entry("a.dev", "x", "t", "u"))
            .unwrap_err();
        assert!(format!("{err}").contains("already registered"));
    }

    #[test]
    fn remove_filters_and_flags() {
        let cur = vec![
            build_domain_entry("a.dev", "c", "t", "u"),
            build_domain_entry("b.dev", "c", "t", "u"),
        ];
        let (out, removed) = allowed_domains_after_remove(&cur, "a.dev");
        assert!(removed);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["domain"], "b.dev");
        let (out2, removed2) = allowed_domains_after_remove(&cur, "missing.dev");
        assert!(!removed2);
        assert_eq!(out2.len(), 2);
    }

    #[test]
    fn hostname_match_apex_and_single_wildcard() {
        assert!(hostname_matches_domain("apprafter.dev", "apprafter.dev"));
        assert!(hostname_matches_domain(
            "www.apprafter.dev",
            "apprafter.dev"
        ));
        assert!(!hostname_matches_domain(
            "a.b.apprafter.dev",
            "apprafter.dev"
        ));
        assert!(!hostname_matches_domain("apprafter.dev", "other.dev"));
        assert!(!hostname_matches_domain("xapprafter.dev", "apprafter.dev"));
    }

    #[test]
    fn apps_using_scans_base_and_env_string_and_array() {
        let apps = serde_json::json!({
            "items": [
                { "metadata": {"name": "web"}, "spec": {"base": {"expose": {"hostname": "apprafter.dev"}}} },
                { "metadata": {"name": "api"}, "spec": {"base": {"expose": {"hostname": ["x.other.dev", "api.apprafter.dev"]}}} },
                { "metadata": {"name": "preview"}, "spec": {"base": {"expose": {}}, "environments": {"prod": {"expose": {"hostname": "preview.apprafter.dev"}}}} },
                { "metadata": {"name": "internal"}, "spec": {"base": {"expose": {"hostname": "nope.other.dev"}}} },
            ]
        });
        assert_eq!(
            apps_using_domain(&apps, "apprafter.dev"),
            vec!["api".to_string(), "preview".to_string(), "web".to_string()]
        );
    }
}
