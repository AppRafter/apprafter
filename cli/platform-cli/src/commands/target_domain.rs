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
    resolve_added_by_from(flag, std::env::var("USER").ok())
}

/// Attribution for a new entry: the explicit flag, else the shell's `$USER`,
/// else a literal placeholder. Never empty — the column is an audit trail and a
/// blank cell is indistinguishable from a rendering bug.
pub(crate) fn resolve_added_by_from(flag: Option<String>, env_user: Option<String>) -> String {
    flag.or(env_user).unwrap_or_else(|| "unknown".to_string())
}

/// Is this the Secret `target cert import` wrote?
///
/// The check is on the `apprafter.io/cert-mode: imported` label, not mere
/// existence: pointing a domain at an arbitrary Secret (a cert-manager
/// intermediate, say) would make the Gateway serve a listener that never gets
/// renewed. `~1` is the JSON-Pointer escape for the `/` inside the label key.
pub(crate) fn cert_is_imported(secret: Option<&Value>) -> bool {
    secret
        .and_then(|s| s.pointer("/metadata/labels/apprafter.io~1cert-mode"))
        .and_then(Value::as_str)
        == Some("imported")
}

/// Error for a cert that is absent or not an imported one.
pub(crate) fn cert_not_imported_error(cert: &str) -> String {
    format!(
        "Cert '{cert}' not found. \
         Run apprafter target cert import {cert} --cert ... --key ... first."
    )
}

/// Error for `remove` on a zone that was never registered.
pub(crate) fn not_registered_error(domain: &str) -> String {
    format!("Domain not registered: {domain}")
}

/// Gate a removal against the apps still pointing at the zone.
///
/// Without `--force` this is a hard refusal listing the apps — removing the
/// zone strips their external access. With `--force` it downgrades to one
/// warning per app so the operator still sees exactly what they broke.
pub(crate) fn check_remove_allowed(
    using: &[String],
    domain: &str,
    force: bool,
) -> Result<Vec<String>> {
    if using.is_empty() {
        return Ok(Vec::new());
    }
    if !force {
        return Err(CliError::Other(format!(
            "{} application(s) using {domain}: [{}]. \
             Remove apps first or use --force (apps will lose external access).",
            using.len(),
            using.join(", ")
        )));
    }
    Ok(using
        .iter()
        .map(|app| format!("app '{app}' uses {domain} — it will lose external access"))
        .collect())
}

/// Lines printed after a successful `domain add`.
pub(crate) fn domain_added_lines(domain: &str, cert: &str) -> Vec<String> {
    vec![
        format!("✓ Domain '{domain}' registered (cert '{cert}')."),
        "  Point your domain's DNS at the node — run `apprafter target ip` for the A/AAAA values — \
         proxied through Cloudflare (orange-cloud, SSL/TLS Full (strict))."
            .to_string(),
        "See your zones: apprafter target domain list".to_string(),
    ]
}

/// Line printed after a successful `domain remove`. It has to say the Secret
/// survived, or an operator re-importing the cert hits "already exists".
pub(crate) fn domain_removed_line(domain: &str) -> String {
    format!(
        "✓ Domain '{domain}' unregistered. (The cert Secret is kept — remove it separately if unused.)"
    )
}

/// Shown by `domain list` when nothing is registered yet.
const NO_DOMAINS_HINT: &str =
    "No domains registered. Run apprafter target domain add <zone> --cert <name>.";

// ---- command plumbing ------------------------------------------------------

fn read_platformstack(kc: &Path) -> Result<Value> {
    kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc,
    )?
    .ok_or_else(|| CliError::Other(platformstack_missing_error()))
}

/// Error when the cluster has no PlatformStack at all.
///
/// Every `domain` verb reads the stack first, so this is the message an
/// operator sees when they point the CLI at a cluster that was never
/// bootstrapped — it has to name the bootstrap command, not just the missing
/// object.
pub(crate) fn platformstack_missing_error() -> String {
    format!(
        "PlatformStack '{PLATFORMSTACK_NAME}' not found in {PLATFORMSTACK_NAMESPACE}. \
         Bootstrap the cluster first (apprafter cluster-bootstrap)."
    )
}

fn read_allowed_domains(stack: &Value) -> Vec<Value> {
    stack
        .pointer("/spec/values/gateway/allowedDomains")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The merge-patch body for an `allowedDomains` rewrite.
///
/// The envelope is deliberately the exact path `spec.values.gateway
/// .allowedDomains` and nothing else. `spec.values` is a
/// preserve-unknown-fields blob that also carries required siblings such as
/// `tier`; a merge patch scoped to this one path leaves them untouched, whereas
/// a whole-object write (or a server-side apply that claims `spec.values`)
/// could prune them.
pub(crate) fn allowed_domains_patch(domains: &[Value]) -> Value {
    json!({ "spec": { "values": { "gateway": { "allowedDomains": domains } } } })
}

fn merge_patch_allowed_domains(domains: &[Value], kc: &Path) -> Result<()> {
    let patch = allowed_domains_patch(domains);
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
    if !cert_is_imported(secret.as_ref()) {
        return Err(CliError::Other(cert_not_imported_error(cert)));
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

    for line in domain_added_lines(domain, cert) {
        println!("{line}");
    }
    Ok(())
}

#[derive(Tabled)]
pub(crate) struct DomainListRow {
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

/// Build the `domain list` rows.
///
/// Missing fields render as `-` rather than an empty cell — an entry written by
/// an older CLI simply has no `addedBy`, and a blank column reads like a
/// rendering bug. The `Apps` count comes from the live Application list, so an
/// operator can see at a glance what a `remove` would break.
pub(crate) fn domain_list_rows(domains: &[Value], apps_json: &Value) -> Vec<DomainListRow> {
    let field = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).unwrap_or("-").to_string();
    domains
        .iter()
        .map(|e| {
            let domain = field(e, "domain");
            let apps = apps_using_domain(apps_json, &domain).len().to_string();
            DomainListRow {
                cert: field(e, "importedCertRef"),
                apps,
                added_at: field(e, "addedAt"),
                added_by: field(e, "addedBy"),
                domain,
            }
        })
        .collect()
}

fn run_domain_list() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let stack = read_platformstack(kc.path())?;
    let domains = read_allowed_domains(&stack);
    if domains.is_empty() {
        println!("{NO_DOMAINS_HINT}");
        return Ok(());
    }
    let rows = domain_list_rows(&domains, &list_applications(kc.path())?);
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
        return Err(CliError::Other(not_registered_error(domain)));
    }

    let using = apps_using_domain(&list_applications(kc.path())?, domain);
    for warning in check_remove_allowed(&using, domain, force)? {
        eprintln!("{}", cli_core::style::warn(&warning));
    }

    merge_patch_allowed_domains(&trimmed, kc.path())?;
    println!("{}", domain_removed_line(domain));
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

    // ── allowed_domains_patch ────────────────────────────────────────────

    /// The write MUST stay scoped to `spec.values.gateway.allowedDomains`.
    /// `spec.values` is a preserve-unknown-fields blob that also carries
    /// required siblings (`tier`); a patch that reached any wider path could
    /// prune them and leave the PlatformStack invalid.
    #[test]
    fn the_patch_envelope_touches_only_the_allowed_domains_path() {
        let entry = build_domain_entry("apprafter.dev", "cf-cert", "t", "u");
        let patch = allowed_domains_patch(std::slice::from_ref(&entry));

        // Exactly one key at every level down to the leaf.
        let spec = patch.get("spec").and_then(Value::as_object).expect("spec");
        assert_eq!(spec.keys().collect::<Vec<_>>(), vec!["values"]);
        let values = spec["values"].as_object().expect("spec.values");
        assert_eq!(values.keys().collect::<Vec<_>>(), vec!["gateway"]);
        let gateway = values["gateway"].as_object().expect("gateway");
        assert_eq!(gateway.keys().collect::<Vec<_>>(), vec!["allowedDomains"]);
        assert_eq!(gateway["allowedDomains"], Value::Array(vec![entry]));
        assert_eq!(
            patch.as_object().map(|o| o.len()),
            Some(1),
            "the envelope must not carry a sibling of `spec`"
        );
    }

    /// Clearing the last domain has to send an empty ARRAY, not `null` and not
    /// an omitted key — under JSON merge-patch semantics `null` deletes the
    /// field and an omitted key is a no-op, so either would silently leave the
    /// removed zone live on the Gateway.
    #[test]
    fn removing_the_last_domain_patches_an_empty_array_not_null() {
        let patch = allowed_domains_patch(&[]);
        assert_eq!(
            patch.pointer("/spec/values/gateway/allowedDomains"),
            Some(&Value::Array(vec![]))
        );
    }

    /// The patch carries entries verbatim and in order — the chart renders one
    /// listener pair per entry, so a reordered or lossy round-trip changes what
    /// the Gateway serves.
    #[test]
    fn the_patch_carries_every_entry_in_order() {
        let a = build_domain_entry("a.dev", "c1", "t1", "u1");
        let b = build_domain_entry("b.dev", "c2", "t2", "u2");
        let patch = allowed_domains_patch(&[a.clone(), b.clone()]);
        let arr = patch
            .pointer("/spec/values/gateway/allowedDomains")
            .and_then(Value::as_array)
            .expect("array");
        assert_eq!(arr, &vec![a, b]);
    }

    // ── read_allowed_domains ─────────────────────────────────────────────

    /// A stack that has never had a domain registered has no
    /// `gateway.allowedDomains` key at all. That must read as "empty", because
    /// the alternative — erroring — would make the very first `domain add`
    /// impossible.
    #[test]
    fn a_stack_without_the_key_reads_as_no_domains() {
        assert!(read_allowed_domains(&serde_json::json!({})).is_empty());
        assert!(read_allowed_domains(&serde_json::json!({"spec": {"values": {}}})).is_empty());
    }

    /// A non-array at that path is corrupt input, not a domain list. Reading it
    /// as empty is the safe direction: `add` then re-writes a well-formed
    /// array instead of appending onto garbage.
    #[test]
    fn a_non_array_at_the_domains_path_reads_as_empty() {
        let stack = serde_json::json!({
            "spec": {"values": {"gateway": {"allowedDomains": "apprafter.dev"}}}
        });
        assert!(read_allowed_domains(&stack).is_empty());
    }

    #[test]
    fn registered_domains_are_read_back_in_order() {
        let stack = serde_json::json!({
            "spec": {"values": {"gateway": {"allowedDomains": [
                {"domain": "a.dev"}, {"domain": "b.dev"}
            ]}}}
        });
        let got = read_allowed_domains(&stack);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["domain"], "a.dev");
        assert_eq!(got[1]["domain"], "b.dev");
    }

    /// A read-then-write round trip must be lossless: `add`/`remove` rewrite
    /// the WHOLE array, so anything dropped on the way in is deleted from the
    /// cluster on the way out.
    #[test]
    fn a_read_then_patch_round_trip_loses_nothing() {
        let entries = vec![
            build_domain_entry("a.dev", "c1", "t1", "u1"),
            build_domain_entry("b.dev", "c2", "t2", "u2"),
        ];
        let stack = serde_json::json!({
            "spec": {"values": {"tier": 1, "gateway": {"allowedDomains": entries}}}
        });
        let read_back = read_allowed_domains(&stack);
        assert_eq!(
            allowed_domains_patch(&read_back).pointer("/spec/values/gateway/allowedDomains"),
            stack.pointer("/spec/values/gateway/allowedDomains")
        );
    }

    // ── cert_is_imported ─────────────────────────────────────────────────

    /// Only a Secret carrying `apprafter.io/cert-mode: imported` may back a
    /// zone. Accepting any Secret would let the Gateway serve a listener from,
    /// say, a cert-manager intermediate that nothing renews.
    #[test]
    fn only_an_imported_cert_secret_is_accepted() {
        let imported = serde_json::json!({
            "metadata": {"labels": {"apprafter.io/cert-mode": "imported"}}
        });
        assert!(cert_is_imported(Some(&imported)));

        let other_mode = serde_json::json!({
            "metadata": {"labels": {"apprafter.io/cert-mode": "acme"}}
        });
        assert!(!cert_is_imported(Some(&other_mode)));

        let unlabelled = serde_json::json!({"metadata": {"name": "cf-cert"}});
        assert!(!cert_is_imported(Some(&unlabelled)));

        // `kubectl get` found nothing at all.
        assert!(!cert_is_imported(None));
    }

    // ── check_remove_allowed ─────────────────────────────────────────────

    /// Without `--force`, removing a zone that apps still use is refused, and
    /// the refusal names every app so the operator knows what to migrate.
    #[test]
    fn removing_a_zone_in_use_is_refused_and_names_the_apps() {
        let using = vec!["web".to_string(), "api".to_string()];
        let err = check_remove_allowed(&using, "apprafter.dev", false)
            .expect_err("a zone in use must not be removed without --force");
        let msg = format!("{err}");
        assert!(msg.contains("web"), "{msg}");
        assert!(msg.contains("api"), "{msg}");
        assert!(msg.contains("--force"), "{msg}");
    }

    /// `--force` downgrades the refusal to one warning PER APP — the operator
    /// asked for it, but must still see exactly which apps lose access.
    #[test]
    fn force_downgrades_the_refusal_to_one_warning_per_app() {
        let using = vec!["web".to_string(), "api".to_string()];
        let warnings =
            check_remove_allowed(&using, "apprafter.dev", true).expect("--force must not refuse");
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|w| w.contains("web")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("api")), "{warnings:?}");
        assert!(
            warnings.iter().all(|w| w.contains("apprafter.dev")),
            "{warnings:?}"
        );
    }

    /// An unused zone removes silently under either flag — no warning to
    /// desensitise the operator to the ones that matter.
    #[test]
    fn an_unused_zone_removes_without_a_word() {
        assert_eq!(
            check_remove_allowed(&[], "apprafter.dev", false).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            check_remove_allowed(&[], "apprafter.dev", true).unwrap(),
            Vec::<String>::new()
        );
    }

    // ── resolve_added_by_from ────────────────────────────────────────────

    /// The flag outranks `$USER`, `$USER` outranks the placeholder, and the
    /// result is never empty — this column is the audit trail for who opened a
    /// zone to the internet.
    #[test]
    fn attribution_prefers_the_flag_then_the_shell_user_then_a_placeholder() {
        assert_eq!(
            resolve_added_by_from(Some("ci-bot".into()), Some("rem".into())),
            "ci-bot"
        );
        assert_eq!(resolve_added_by_from(None, Some("rem".into())), "rem");
        assert_eq!(resolve_added_by_from(None, None), "unknown");
    }

    // ── domain_list_rows ─────────────────────────────────────────────────

    /// Every column falls back to `-` rather than an empty cell: entries
    /// written by older CLIs legitimately lack `addedBy`, and a blank column
    /// is indistinguishable from a rendering bug.
    #[test]
    fn missing_entry_fields_render_as_a_dash() {
        let rows = domain_list_rows(
            &[serde_json::json!({"domain": "apprafter.dev"})],
            &serde_json::json!({"items": []}),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].domain, "apprafter.dev");
        assert_eq!(rows[0].cert, "-");
        assert_eq!(rows[0].added_at, "-");
        assert_eq!(rows[0].added_by, "-");
    }

    /// The `Apps` cell counts the apps actually serving on that zone — it is
    /// what tells the operator whether a `remove` is safe, so it must be
    /// per-row and not, say, the total app count.
    #[test]
    fn the_apps_column_counts_only_the_apps_on_that_zone() {
        let apps = serde_json::json!({
            "items": [
                { "metadata": {"name": "web"}, "spec": {"base": {"expose": {"hostname": "apprafter.dev"}}} },
                { "metadata": {"name": "api"}, "spec": {"base": {"expose": {"hostname": "api.apprafter.dev"}}} },
                { "metadata": {"name": "shop"}, "spec": {"base": {"expose": {"hostname": "shop.other.dev"}}} },
            ]
        });
        let rows = domain_list_rows(
            &[
                build_domain_entry("apprafter.dev", "cf-cert", "2026-06-15", "rem"),
                build_domain_entry("other.dev", "other-cert", "2026-06-16", "ci"),
                build_domain_entry("unused.dev", "u-cert", "2026-06-17", "ci"),
            ],
            &apps,
        );
        let counts: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.domain.as_str(), r.apps.as_str()))
            .collect();
        assert_eq!(
            counts,
            vec![
                ("apprafter.dev", "2"),
                ("other.dev", "1"),
                ("unused.dev", "0")
            ]
        );
        assert_eq!(rows[0].cert, "cf-cert");
        assert_eq!(rows[0].added_by, "rem");
    }

    // ── operator-facing messages ─────────────────────────────────────────

    /// The add confirmation has to carry the DNS next step; a bare "registered"
    /// leaves the operator with a zone that resolves nowhere.
    #[test]
    fn the_add_confirmation_names_the_zone_the_cert_and_the_dns_next_step() {
        let text = domain_added_lines("apprafter.dev", "cf-cert").join("\n");
        assert!(text.contains("apprafter.dev"), "{text}");
        assert!(text.contains("cf-cert"), "{text}");
        assert!(text.contains("apprafter target ip"), "{text}");
    }

    /// The remove confirmation must say the Secret survived — otherwise the
    /// operator re-imports it and hits "already exists".
    #[test]
    fn the_remove_confirmation_says_the_cert_secret_survives() {
        let line = domain_removed_line("apprafter.dev");
        assert!(line.contains("apprafter.dev"), "{line}");
        assert!(line.contains("cert Secret is kept"), "{line}");
    }

    /// The cert-missing error has to hand over the exact command that fixes
    /// it, including the cert name the operator just used.
    #[test]
    fn the_missing_cert_error_hands_over_the_import_command() {
        let msg = cert_not_imported_error("cf-cert");
        assert!(msg.contains("cf-cert"), "{msg}");
        assert!(msg.contains("apprafter target cert import"), "{msg}");
    }

    /// The bootstrap error has to name the command that fixes it AND the
    /// namespace it looked in — pointing the CLI at the wrong cluster looks
    /// identical otherwise.
    #[test]
    fn the_missing_platformstack_error_points_at_cluster_bootstrap() {
        let msg = platformstack_missing_error();
        assert!(msg.contains("apprafter cluster-bootstrap"), "{msg}");
        assert!(msg.contains(PLATFORMSTACK_NAMESPACE), "{msg}");
        assert!(msg.contains(PLATFORMSTACK_NAME), "{msg}");
    }

    #[test]
    fn the_unregistered_domain_error_names_the_zone() {
        assert!(not_registered_error("apprafter.dev").contains("apprafter.dev"));
    }
}
