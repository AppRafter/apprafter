// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter secret seal` — seal literal key/value pairs into a bitnami
//! `SealedSecret` using the in-cluster controller's public cert (1.79c S0 /
//! ADR 0039). The CLI never holds the cluster private key, so sealing is a
//! one-way operation: the output is safe to print, commit, or apply.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use cli_core::{CliError, Result};
use cli_providers::k8s::kubectl::KubectlCli;
use cli_providers::k8s::sealing::{build_sealed_secret, fetch_controller_public_key};
use serde_json::Value;

use crate::commands::k8s_helpers::{ensure_kubeconfig_tempfile, kubectl_get_json};

/// What the caller should do when a secret with the same name already exists.
/// Pure (no I/O) — the interactive prompt + cluster check are wired in
/// `run_seal`; this type is factored out so all three branches are unit-testable.
#[derive(Debug, PartialEq, Eq)]
pub enum OverwriteDecision {
    /// Proceed silently (--yes was set, or the resource is absent).
    Proceed,
    /// Show an interactive TTY confirmation prompt.
    Prompt,
    /// Return an error — non-interactive shell without --yes.
    ErrorNonInteractive,
}

/// Decide what to do when asked to re-seal over an existing resource.
/// Pure: takes `exists`, `yes`, `is_tty` and returns the action.
pub fn overwrite_decision(exists: bool, yes: bool, is_tty: bool) -> OverwriteDecision {
    if !exists {
        return OverwriteDecision::Proceed;
    }
    if yes {
        return OverwriteDecision::Proceed;
    }
    if is_tty {
        OverwriteDecision::Prompt
    } else {
        OverwriteDecision::ErrorNonInteractive
    }
}

/// Parse repeatable `KEY=VALUE` literals into a byte map. The value may
/// contain `=` (only the first splits the pair) and may be empty.
pub fn parse_literals(items: &[String]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    for item in items {
        let (key, value) = item.split_once('=').ok_or_else(|| {
            CliError::Other(format!("--from-literal expects KEY=VALUE, got `{item}`"))
        })?;
        if key.is_empty() {
            return Err(CliError::Other(format!(
                "--from-literal key is empty in `{item}`"
            )));
        }
        out.insert(key.to_string(), value.as_bytes().to_vec());
    }
    Ok(out)
}

/// Seal `--from-literal` pairs and either print the `SealedSecret` YAML
/// (`--stdout`) or `kubectl apply` it.
///
/// When applying to the cluster (not `--stdout`), checks whether a
/// `SealedSecret` or `Secret` named `name` already exists in `namespace`
/// and gates overwrite on `yes`/TTY — sealing REPLACES keys, it does not
/// merge, so silent overwrites can destroy data.
pub fn run_seal(
    name: &str,
    namespace: &str,
    from_literal: &[String],
    secret_type: &str,
    stdout: bool,
    yes: bool,
) -> Result<()> {
    let data = parse_literals(from_literal)?;
    if data.is_empty() {
        return Err(CliError::Other(
            "no data to seal — pass at least one --from-literal KEY=VALUE".to_string(),
        ));
    }

    let kc = ensure_kubeconfig_tempfile()?;

    // Existence check — only relevant when we are about to apply to the
    // cluster. `--stdout` is a local rendering path; skip the network check.
    if !stdout {
        let exists = secret_exists(name, namespace, kc.path())?;
        match overwrite_decision(exists, yes, std::io::stdin().is_terminal()) {
            OverwriteDecision::Proceed => {
                if exists {
                    println!(
                        "note: replacing existing secret '{name}' in '{namespace}' \
                         (all keys will be replaced)"
                    );
                }
            }
            OverwriteDecision::Prompt => {
                println!(
                    "A secret named '{name}' already exists in '{namespace}'. \
                     Sealing REPLACES its keys (it does NOT merge — keys not in \
                     this command are dropped). Continue? [y/N]"
                );
                let confirmed = inquire::Confirm::new("Continue?")
                    .with_default(false)
                    .prompt()
                    .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
                if !confirmed {
                    println!("Aborted — no changes made.");
                    return Ok(());
                }
            }
            OverwriteDecision::ErrorNonInteractive => {
                return Err(CliError::Other(format!(
                    "secret '{name}' already exists in '{namespace}' — \
                     re-run with --yes to replace it (sealing does NOT merge keys), \
                     or choose a different name"
                )));
            }
        }
    }

    let pub_key = fetch_controller_public_key(&KubectlCli, kc.path())?;
    let cr = build_sealed_secret(&pub_key, namespace, name, &data, secret_type)?;

    if stdout {
        let yaml = serde_yaml::to_string(&cr)
            .map_err(|e| CliError::Other(format!("render SealedSecret yaml: {e}")))?;
        print!("{yaml}");
    } else {
        apply_manifest(&cr, kc.path())?;
        println!("sealedsecret/{name} applied to namespace {namespace}");
    }
    Ok(())
}

/// Returns `true` when a `SealedSecret` OR a `Secret` named `name` exists
/// in `namespace`. Any non-404 kubectl error is propagated.
fn secret_exists(name: &str, namespace: &str, kubeconfig_path: &Path) -> Result<bool> {
    // Check SealedSecret first (the source of truth), then plain Secret
    // (a user may have created one directly without sealing).
    for kind in ["sealedsecret", "secret"] {
        if kubectl_get_json(kind, Some(name), Some(namespace), kubeconfig_path)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn apply_manifest(manifest: &Value, kubeconfig_path: &Path) -> Result<()> {
    let mut file = tempfile::Builder::new()
        .prefix("apprafter-sealed-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| CliError::Other(format!("create SealedSecret tempfile: {e}")))?;
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialise SealedSecret: {e}")))?;
    file.write_all(&body)
        .map_err(|e| CliError::Other(format!("write SealedSecret tempfile: {e}")))?;
    file.flush()
        .map_err(|e| CliError::Other(format!("flush SealedSecret tempfile: {e}")))?;

    let out = Command::new("kubectl")
        .arg("apply")
        .arg("-f")
        .arg(file.path())
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl apply: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl apply failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Pure: the `kubectl delete` argv for one resource. `--ignore-not-found`
/// makes it idempotent (an absent object is a no-op, not an error).
/// Extracted so the command shape is unit-testable without a cluster.
fn delete_args<'a>(kind: &'a str, name: &'a str, namespace: &'a str) -> [&'a str; 6] {
    ["delete", kind, name, "-n", namespace, "--ignore-not-found"]
}

/// `apprafter secret remove <name>` — delete the `SealedSecret` and the
/// `Secret` the controller unsealed from it, in `namespace`. Saves a manual
/// `kubectl delete sealedsecret,secret`. The SealedSecret is the source of
/// truth (the controller re-creates the Secret from it), so it is deleted
/// FIRST — cascade-removing its owned Secret — and the Secret is then deleted
/// explicitly to also cover a plain Secret that has no SealedSecret.
/// Idempotent via `--ignore-not-found`.
pub fn run_remove(name: &str, namespace: &str, yes: bool) -> Result<()> {
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!("Delete SealedSecret + Secret '{name}' in namespace '{namespace}'?");
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let kc = ensure_kubeconfig_tempfile()?;
    // SealedSecret first (source of truth — its deletion cascade-removes the
    // owned Secret), then the Secret explicitly for a plain/un-owned one.
    for kind in ["sealedsecret", "secret"] {
        let out = Command::new("kubectl")
            .args(delete_args(kind, name, namespace))
            .env("KUBECONFIG", kc.path())
            .output()
            .map_err(|e| CliError::Other(format!("spawn kubectl delete {kind}: {e}")))?;
        if !out.status.success() {
            return Err(CliError::Other(format!(
                "kubectl delete {kind}/{name} failed (exit {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let trimmed = stdout.trim();
        if !trimmed.is_empty() {
            println!("  {trimmed}");
        }
    }
    println!("✓ Removed '{name}' (SealedSecret + Secret) from namespace '{namespace}'.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_args_are_idempotent_and_namespaced() {
        assert_eq!(
            delete_args("sealedsecret", "my-secret", "apprafter"),
            [
                "delete",
                "sealedsecret",
                "my-secret",
                "-n",
                "apprafter",
                "--ignore-not-found"
            ]
        );
    }

    #[test]
    fn parses_key_value_pairs() {
        let m = parse_literals(&["DB=secret".to_string(), "TOKEN=ghp_x".to_string()]).unwrap();
        assert_eq!(m.get("DB").unwrap(), b"secret");
        assert_eq!(m.get("TOKEN").unwrap(), b"ghp_x");
    }

    #[test]
    fn value_may_contain_equals() {
        let m = parse_literals(&["URL=a=b=c".to_string()]).unwrap();
        assert_eq!(m.get("URL").unwrap(), b"a=b=c");
    }

    #[test]
    fn empty_value_is_allowed() {
        let m = parse_literals(&["EMPTY=".to_string()]).unwrap();
        assert_eq!(m.get("EMPTY").unwrap(), b"");
    }

    #[test]
    fn rejects_missing_equals() {
        let err = parse_literals(&["NOEQ".to_string()]).unwrap_err();
        assert!(format!("{err}").contains("KEY=VALUE"));
    }

    #[test]
    fn rejects_empty_key() {
        let err = parse_literals(&["=value".to_string()]).unwrap_err();
        assert!(format!("{err}").contains("key is empty"));
    }

    // --- overwrite_decision pure branch coverage ---

    #[test]
    fn no_existing_secret_always_proceeds() {
        // exists=false → Proceed regardless of yes/tty
        assert_eq!(
            overwrite_decision(false, false, false),
            OverwriteDecision::Proceed
        );
        assert_eq!(
            overwrite_decision(false, false, true),
            OverwriteDecision::Proceed
        );
        assert_eq!(
            overwrite_decision(false, true, false),
            OverwriteDecision::Proceed
        );
        assert_eq!(
            overwrite_decision(false, true, true),
            OverwriteDecision::Proceed
        );
    }

    #[test]
    fn existing_secret_with_yes_proceeds() {
        assert_eq!(
            overwrite_decision(true, true, false),
            OverwriteDecision::Proceed
        );
        assert_eq!(
            overwrite_decision(true, true, true),
            OverwriteDecision::Proceed
        );
    }

    #[test]
    fn existing_secret_no_yes_tty_prompts() {
        assert_eq!(
            overwrite_decision(true, false, true),
            OverwriteDecision::Prompt
        );
    }

    #[test]
    fn existing_secret_no_yes_non_tty_errors() {
        assert_eq!(
            overwrite_decision(true, false, false),
            OverwriteDecision::ErrorNonInteractive
        );
    }
}

/// One sealed secret as `secret list` renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSecretSummary {
    pub namespace: String,
    pub name: String,
    /// Key NAMES, sorted. Read from the `SealedSecret`'s own
    /// `spec.encryptedData` map, so nothing is decrypted and no `Secret`
    /// is read for its contents — the names answer both questions
    /// `EnvSecretMissing` raises without touching the material.
    pub keys: Vec<String>,
}

/// Parse `kubectl get sealedsecrets -o json` into summaries.
///
/// Pure, so the shape is tested without a cluster. Tolerant by design: a
/// SealedSecret with no `encryptedData` is a real state (sealed empty, or
/// a hand-written CR) and renders with no keys rather than being dropped —
/// a listing that silently omits an object is worse than one that shows an
/// odd row, because the reader is here to find out where something is.
pub fn parse_sealed_secret_summaries(v: &Value) -> Vec<SealedSecretSummary> {
    let mut out: Vec<SealedSecretSummary> = v
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            let name = item.pointer("/metadata/name")?.as_str()?.to_string();
            let namespace = item
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut keys: Vec<String> = item
                .pointer("/spec/encryptedData")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            keys.sort();
            Some(SealedSecretSummary {
                namespace,
                name,
                keys,
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    out
}

/// Render the table. Pure so the column maths is tested directly.
pub fn render_secret_list(rows: &[SealedSecretSummary], scope: &str) -> String {
    if rows.is_empty() {
        return format!("No sealed secrets found in {scope}.\n");
    }
    let ns_w = rows
        .iter()
        .map(|r| r.namespace.len())
        .max()
        .unwrap_or(9)
        .max(9);
    let name_w = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    let mut s = format!("Sealed secrets in {scope}:\n\n");
    s.push_str(&format!(
        "  {:<ns_w$}  {:<name_w$}  KEYS\n",
        "NAMESPACE",
        "NAME",
        ns_w = ns_w,
        name_w = name_w
    ));
    for r in rows {
        let keys = if r.keys.is_empty() {
            "(none)".to_string()
        } else {
            r.keys.join(", ")
        };
        s.push_str(&format!(
            "  {:<ns_w$}  {:<name_w$}  {}\n",
            r.namespace,
            r.name,
            keys,
            ns_w = ns_w,
            name_w = name_w
        ));
    }
    s
}

/// `apprafter secret list` — where each sealed secret lives and what keys
/// it carries.
///
/// D7: the operator's `EnvSecretMissing` used to ask two questions — is it
/// in the wrong namespace, or is the key spelled differently — and the CLI
/// could answer neither, so the guide reached for `kubectl` three times on
/// a page whose whole subject is a first-class task. The condition message
/// now names the cause; this command answers the same questions BEFORE an
/// error rather than after.
pub fn run_list(namespace: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let json = match namespace {
        Some(ns) => kubectl_get_json("sealedsecrets", None, Some(ns), kc.path())?,
        None => crate::commands::k8s_helpers::kubectl_get_json_cluster_wide(
            "sealedsecrets",
            None,
            kc.path(),
        )?,
    };
    let rows = json
        .as_ref()
        .map(parse_sealed_secret_summaries)
        .unwrap_or_default();
    let scope = match namespace {
        Some(ns) => format!("namespace '{ns}'"),
        None => "every namespace".to_string(),
    };
    print!("{}", render_secret_list(&rows, &scope));
    Ok(())
}

#[cfg(test)]
mod list_tests {
    use super::*;
    use serde_json::json;

    fn sealed(ns: &str, name: &str, keys: &[&str]) -> Value {
        let mut enc = serde_json::Map::new();
        for k in keys {
            enc.insert((*k).to_string(), json!("AgB1c2VsZXNz"));
        }
        json!({
            "metadata": { "name": name, "namespace": ns },
            "spec": { "encryptedData": Value::Object(enc) }
        })
    }

    #[test]
    fn it_reads_key_names_without_touching_any_value() {
        // The property that makes this command safe to run anywhere: the
        // names come from the SealedSecret's OWN encryptedData map, so
        // nothing decrypts and no Secret is read. The values in the
        // fixture are ciphertext and never appear in the output.
        let v = json!({ "items": [sealed("shop", "checkout", &["stripe-api-key", "webhook"])] });
        let rows = parse_sealed_secret_summaries(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].keys, vec!["stripe-api-key", "webhook"]);

        let out = render_secret_list(&rows, "every namespace");
        assert!(out.contains("stripe-api-key"), "{out}");
        assert!(!out.contains("AgB1c2VsZXNz"), "ciphertext leaked: {out}");
    }

    #[test]
    fn keys_are_sorted_so_the_output_is_stable() {
        let v = json!({ "items": [sealed("shop", "c", &["zeta", "alpha", "mid"])] });
        assert_eq!(
            parse_sealed_secret_summaries(&v)[0].keys,
            vec!["alpha", "mid", "zeta"]
        );
    }

    #[test]
    fn rows_are_sorted_by_namespace_then_name() {
        let v = json!({ "items": [
            sealed("shop", "b", &["k"]),
            sealed("apprafter-system", "z", &["k"]),
            sealed("shop", "a", &["k"]),
        ]});
        let rows = parse_sealed_secret_summaries(&v);
        let got: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.namespace.as_str(), r.name.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![("apprafter-system", "z"), ("shop", "a"), ("shop", "b")]
        );
    }

    #[test]
    fn a_sealed_secret_with_no_keys_is_shown_not_dropped() {
        // A listing that silently omits an object is worse than one that
        // shows an odd row: the reader is here to find out WHERE something
        // is, and an omission answers "nowhere".
        let v = json!({ "items": [{ "metadata": { "name": "empty", "namespace": "shop" } }] });
        let rows = parse_sealed_secret_summaries(&v);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].keys.is_empty());
        assert!(render_secret_list(&rows, "x").contains("(none)"));
    }

    #[test]
    fn an_empty_result_says_where_it_looked() {
        // "No sealed secrets found." with no scope leaves the reader
        // unsure whether they searched the right place — which is the
        // exact confusion this command exists to end.
        let out = render_secret_list(&[], "namespace 'shop'");
        assert!(out.contains("namespace 'shop'"), "{out}");
    }
}
