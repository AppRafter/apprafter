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

use crate::commands::k8s_helpers::ensure_kubeconfig_tempfile;

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
pub fn run_seal(
    name: &str,
    namespace: &str,
    from_literal: &[String],
    secret_type: &str,
    stdout: bool,
) -> Result<()> {
    let data = parse_literals(from_literal)?;
    if data.is_empty() {
        return Err(CliError::Other(
            "no data to seal — pass at least one --from-literal KEY=VALUE".to_string(),
        ));
    }

    let kc = ensure_kubeconfig_tempfile()?;
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
}
