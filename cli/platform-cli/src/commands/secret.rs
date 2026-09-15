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

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, namespace_confirmed_missing,
};

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

/// What to do when the namespace a seal targets does not exist yet.
///
/// Sealing into a fresh namespace is a normal step, not an anomaly: the
/// order that avoids a new application crash-looping on startup is to seal
/// its environment first and deploy second, and at that point nothing has
/// created the namespace. Refusing would push the reader to a bare
/// `kubectl create namespace` for a step the platform owns.
///
/// **But it is never done silently, with or without `--yes`.** A
/// `SealedSecret` is encrypted under its namespace and name, so the blob
/// this command is about to write can only ever be decrypted in the
/// namespace it names — a mistyped `-n` does not produce a recoverable
/// mistake to be fixed by moving something, it produces a namespace nobody
/// wanted holding ciphertext nobody can read, and the application still
/// waiting for the secret it never got. Creating that namespace without
/// saying so removes the last place the typo was visible.
#[derive(Debug, PartialEq, Eq)]
pub enum NamespaceDecision {
    /// It is already there — nothing to do, nothing to say.
    Exists,
    /// Create it, and print that it was created (`--yes`).
    CreateAnnounced,
    /// Ask on a TTY before creating.
    Prompt,
    /// Non-interactive and unauthorised — refuse and name both ways out.
    ErrorNonInteractive,
}

/// Decide how to handle a missing target namespace. Pure: takes `exists`,
/// `yes`, `is_tty`. Deliberately shaped like [`overwrite_decision`] — both
/// gate a write the reader may not have meant, and one shape means one thing
/// to learn.
pub fn namespace_decision(exists: bool, yes: bool, is_tty: bool) -> NamespaceDecision {
    if exists {
        return NamespaceDecision::Exists;
    }
    if yes {
        return NamespaceDecision::CreateAnnounced;
    }
    if is_tty {
        NamespaceDecision::Prompt
    } else {
        NamespaceDecision::ErrorNonInteractive
    }
}

/// `kubectl create namespace <ns>`. Only reached once a
/// [`NamespaceDecision`] has authorised it.
fn create_namespace(namespace: &str, kubeconfig_path: &Path) -> Result<()> {
    let out = Command::new("kubectl")
        .args(["create", "namespace", namespace])
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl create namespace: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // A namespace that appeared between the probe and here is the
        // outcome we wanted, not a failure.
        if stderr.contains("AlreadyExists") || stderr.contains("already exists") {
            return Ok(());
        }
        return Err(CliError::Other(format!(
            "create namespace '{namespace}' failed (exit {:?}): {stderr}",
            out.status.code()
        )));
    }
    Ok(())
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
        // The namespace comes first: the overwrite check below asks whether
        // a secret is in it, and in a namespace that does not exist the
        // answer is a truthful "no" to a question the reader is not asking.
        match namespace_decision(
            !namespace_confirmed_missing(namespace, kc.path()),
            yes,
            std::io::stdin().is_terminal(),
        ) {
            NamespaceDecision::Exists => {}
            NamespaceDecision::CreateAnnounced => {
                create_namespace(namespace, kc.path())?;
                println!("created namespace '{namespace}'");
            }
            NamespaceDecision::Prompt => {
                println!(
                    "Namespace '{namespace}' does not exist. A SealedSecret is \
                     encrypted under its namespace, so this blob will only ever \
                     decrypt in '{namespace}' — if that is a typo, the secret is \
                     unreadable and the application never receives it."
                );
                let confirmed = inquire::Confirm::new(&format!("Create namespace '{namespace}'?"))
                    .with_default(false)
                    .prompt()
                    .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
                if !confirmed {
                    println!("Aborted — no namespace created, nothing sealed.");
                    return Ok(());
                }
                create_namespace(namespace, kc.path())?;
                println!("created namespace '{namespace}'");
            }
            NamespaceDecision::ErrorNonInteractive => {
                return Err(CliError::Other(format!(
                    "namespace '{namespace}' does not exist. A SealedSecret is \
                     encrypted under its namespace and decrypts nowhere else, so \
                     this is not created silently. Re-run with --yes to create it, \
                     or create it yourself first — and check the spelling either way"
                )));
            }
        }
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
    let mut cr = build_sealed_secret(&pub_key, namespace, name, &data, secret_type)?;
    stamp_provenance(&mut cr);

    if stdout {
        let yaml = serde_yaml::to_string(&cr)
            .map_err(|e| CliError::Other(format!("render SealedSecret yaml: {e}")))?;
        print!("{yaml}");
    } else {
        apply_manifest(&cr, kc.path())?;
        println!("sealedsecret/{name} applied to namespace {namespace}");
        report_consumers(name, namespace, kc.path());
    }
    Ok(())
}

/// Name the applications this seal just affected, and what has NOT happened
/// to them yet (2.22c / D6 + D14).
///
/// Best-effort: a failure to read the Applications must never turn a
/// successful seal into an error, so this reports nothing and stays quiet.
/// The seal already happened; a decorative lookup that fails the command
/// afterwards would be strictly worse than silence — the operator-RBAC
/// lesson from ADR 0048, applied to the CLI.
///
/// Why it prints at all: a secret is not owned by one application. Nothing
/// stops three Applications in a namespace resolving the same one, and until
/// now the person sealing had no way to know that — `seal` said nothing and
/// the operator holds no reverse index. That unknowable blast radius is the
/// reason the automatic roll was rejected as a Tier-1 default (D6), which
/// makes showing it the thing that has to exist instead.
fn report_consumers(name: &str, namespace: &str, kubeconfig_path: &Path) {
    let Ok(Some(apps)) = crate::commands::k8s_helpers::kubectl_get_json_cluster_wide(
        "applications.apprafter.io",
        None,
        kubeconfig_path,
    ) else {
        return;
    };
    let consumers = apps_consuming(&parse_secret_bindings(&apps), namespace, name);
    if consumers.is_empty() {
        return;
    }
    let n = consumers.len();
    let plural = if n == 1 {
        "application"
    } else {
        "applications"
    };
    println!();
    println!(
        "  {n} {plural} in '{namespace}' resolve this secret: {}",
        consumers.join(", ")
    );
    // This used to stop short of naming a command, because none shipped and
    // inventing one in a message is the defect D3 recorded. `app restart`
    // (ADR 0064) is that command, and this is the exact moment a reader
    // needs it: they have just rotated a credential and the pods are still
    // serving the old one.
    println!(
        "  Their running pods keep the PREVIOUS value: an environment variable \n           from a secret is resolved once at pod start and never re-read."
    );
    println!("  Roll them when the rotation is complete:");
    for app in &consumers {
        println!("      apprafter app restart {app}");
    }
}

/// Record who sealed this and when (2.22c / D14).
///
/// **This is provenance, not attestation.** The value is self-reported by
/// the machine running the command and anyone who can seal can also edit
/// it, so it authenticates nothing. It is here for the two cases D14 says
/// are worth serving — an insider mistake and a forensic reconstruction —
/// where the question is "when did this last change, and roughly by whom",
/// and today the only answer is a resourceVersion.
///
/// Stamped by the COMMAND, not by `build_sealed_secret`: that builder is
/// shared with the restore path, which re-seals every captured secret for
/// the target cluster, and attributing a machine restore to whoever ran it
/// would be a lie in the record rather than a gap in it.
fn stamp_provenance(cr: &mut Value) {
    let who = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    let Some(meta) = cr.pointer_mut("/metadata").and_then(Value::as_object_mut) else {
        return;
    };
    let ann = meta
        .entry("annotations")
        .or_insert_with(|| Value::Object(Default::default()));
    if let Some(ann) = ann.as_object_mut() {
        ann.insert("apprafter.io/sealed-by".to_string(), Value::String(who));
        ann.insert(
            "apprafter.io/sealed-at".to_string(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
    }
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

/// What `secret remove` found at the address it was given, decided BEFORE
/// anything is deleted.
///
/// The check has to happen first for two reasons. The confirmation prompt
/// must not ask the reader to authorise destroying an object that is not
/// there — a `y` typed at that prompt teaches them the address was right.
/// And `kubectl delete --ignore-not-found` exits 0 whether or not it deleted
/// anything, so after the fact the two outcomes are indistinguishable
/// without parsing kubectl's prose.
#[derive(Debug, PartialEq, Eq)]
pub enum RemoveTarget {
    /// A `SealedSecret` and/or a `Secret` by that name is in that namespace.
    Present,
    /// The namespace exists; nothing by that name is in it.
    SecretAbsent,
    /// The namespace itself does not exist — so the address is wrong,
    /// rather than the work being already done.
    NamespaceAbsent,
}

/// What to say when there was nothing to remove, and whether that is an
/// error. Pure, so both branches are asserted without a cluster.
///
/// The two absences are NOT the same event and do not get the same exit
/// code. A missing secret in a real namespace means the delete already
/// happened — `remove` documents itself as idempotent and a script that
/// runs it twice must not fail the second time. A missing namespace means
/// the reader is addressing somewhere that does not exist, which is a typo
/// often enough that reporting "nothing to remove" would be the same lie in
/// a quieter voice.
pub fn render_nothing_to_remove(name: &str, namespace: &str, target: &RemoveTarget) -> String {
    match target {
        RemoveTarget::Present => String::new(),
        RemoveTarget::SecretAbsent => format!(
            "Nothing to remove — no SealedSecret or Secret named '{name}' in \
             namespace '{namespace}'.\n\
             \n  `apprafter secret list` shows every sealed secret and the \
             namespace each one lives in.\n"
        ),
        RemoveTarget::NamespaceAbsent => format!(
            "namespace '{namespace}' does not exist, so nothing named '{name}' \
             can be removed from it. Check the namespace — \
             `apprafter secret list` shows every sealed secret and where each \
             one lives."
        ),
    }
}

/// `apprafter secret remove <name>` — delete the `SealedSecret` and the
/// `Secret` the controller unsealed from it, in `namespace`. Saves a manual
/// `kubectl delete sealedsecret,secret`. The SealedSecret is the source of
/// truth (the controller re-creates the Secret from it), so it is deleted
/// FIRST — cascade-removing its owned Secret — and the Secret is then deleted
/// explicitly to also cover a plain Secret that has no SealedSecret.
/// Idempotent via `--ignore-not-found`.
///
/// Reports what it actually did. It used to print `✓ Removed '<name>' …`
/// unconditionally, because `--ignore-not-found` makes the delete succeed
/// against an empty namespace exactly as it does against a real one — so a
/// mistyped `-n` produced a tick and a sentence naming a namespace the
/// reader had never populated, and the secret they meant to delete was
/// still live somewhere else.
pub fn run_remove(name: &str, namespace: &str, yes: bool) -> Result<()> {
    let probe = ensure_kubeconfig_tempfile()?;
    let target = if namespace_confirmed_missing(namespace, probe.path()) {
        RemoveTarget::NamespaceAbsent
    } else if secret_exists(name, namespace, probe.path())? {
        RemoveTarget::Present
    } else {
        RemoveTarget::SecretAbsent
    };
    match target {
        RemoveTarget::NamespaceAbsent => {
            return Err(CliError::Other(render_nothing_to_remove(
                name, namespace, &target,
            )));
        }
        RemoveTarget::SecretAbsent => {
            print!("{}", render_nothing_to_remove(name, namespace, &target));
            return Ok(());
        }
        RemoveTarget::Present => {}
    }

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

    // SealedSecret first (source of truth — its deletion cascade-removes the
    // owned Secret), then the Secret explicitly for a plain/un-owned one.
    let mut deleted_any = false;
    for kind in ["sealedsecret", "secret"] {
        let out = Command::new("kubectl")
            .args(delete_args(kind, name, namespace))
            .env("KUBECONFIG", probe.path())
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
            deleted_any = true;
            println!("  {trimmed}");
        }
    }
    // The pre-check said something was there, so an empty stdout here means
    // it went away between the two calls — a concurrent delete, or the
    // controller reaping the Secret as the SealedSecret's owner. Claiming a
    // removal this command did not perform would be the same false tick in a
    // narrower window.
    if deleted_any {
        println!("✓ Removed '{name}' (SealedSecret + Secret) from namespace '{namespace}'.");
    } else {
        println!(
            "Nothing was removed — '{name}' disappeared from namespace \
             '{namespace}' between the check and the delete."
        );
    }
    Ok(())
}

#[cfg(test)]
mod namespace_and_absence_tests {
    use super::*;

    // ── namespace_decision ────────────────────────────────────────

    #[test]
    fn an_existing_namespace_is_not_touched_or_mentioned() {
        for yes in [false, true] {
            for tty in [false, true] {
                assert_eq!(
                    namespace_decision(true, yes, tty),
                    NamespaceDecision::Exists,
                    "exists=true must short-circuit regardless of yes={yes} tty={tty}"
                );
            }
        }
    }

    #[test]
    fn yes_authorises_creation_without_making_it_silent() {
        // `--yes` is the only non-interactive way to create, and it lands on
        // the arm whose name is the requirement. The silence half is
        // structural rather than assertable here: the enum has no arm that
        // creates without printing, so `run_seal` has none to fall into.
        assert_eq!(
            namespace_decision(false, true, false),
            NamespaceDecision::CreateAnnounced
        );
        assert_eq!(
            namespace_decision(false, true, true),
            NamespaceDecision::CreateAnnounced,
            "a TTY with --yes must not downgrade to a prompt"
        );
    }

    #[test]
    fn a_tty_is_asked_and_a_pipe_is_refused() {
        assert_eq!(
            namespace_decision(false, false, true),
            NamespaceDecision::Prompt
        );
        assert_eq!(
            namespace_decision(false, false, false),
            NamespaceDecision::ErrorNonInteractive
        );
    }

    #[test]
    fn it_gates_the_same_way_the_overwrite_check_does() {
        // Two guards on the same command, both standing between the reader
        // and a write they may not have meant. A reader who has learned one
        // has learned the other; a divergence here is a second thing to learn
        // for no reason.
        for (exists, yes, tty) in [
            (true, false, true),
            (false, true, false),
            (false, false, true),
            (false, false, false),
        ] {
            // The two take `exists` with opposite polarity — a namespace
            // gates when it is ABSENT, a secret when it is PRESENT — so the
            // comparable call negates it.
            let ns = namespace_decision(exists, yes, tty);
            let ow = overwrite_decision(!exists, yes, tty);
            let ns_blocks = matches!(ns, NamespaceDecision::ErrorNonInteractive);
            let ow_blocks = matches!(ow, OverwriteDecision::ErrorNonInteractive);
            assert_eq!(
                ns_blocks, ow_blocks,
                "exists={exists} yes={yes} tty={tty}: the two guards refuse in \
                 different circumstances"
            );
        }
    }

    // ── render_nothing_to_remove ──────────────────────────────────

    #[test]
    fn an_absent_secret_says_nothing_was_removed() {
        let msg = render_nothing_to_remove("stripe", "shop", &RemoveTarget::SecretAbsent);
        assert!(msg.contains("Nothing to remove"), "{msg}");
        assert!(msg.contains("stripe") && msg.contains("shop"), "{msg}");
        assert!(
            !msg.contains('✓'),
            "the tick is what made this a false claim: {msg}"
        );
        assert!(
            msg.contains("secret list"),
            "the reader needs the next command: {msg}"
        );
    }

    #[test]
    fn an_absent_namespace_blames_the_namespace_not_the_secret() {
        // The reported case was a mistyped `-n`. A message that leads with
        // the secret sends the reader hunting for an object that was never
        // misplaced.
        let msg = render_nothing_to_remove("stripe", "shopp", &RemoveTarget::NamespaceAbsent);
        assert!(msg.starts_with("namespace 'shopp' does not exist"), "{msg}");
        assert!(msg.contains("Check the namespace"), "{msg}");
    }

    #[test]
    fn the_two_absences_do_not_read_alike() {
        let secret_gone = render_nothing_to_remove("a", "ns", &RemoveTarget::SecretAbsent);
        let ns_gone = render_nothing_to_remove("a", "ns", &RemoveTarget::NamespaceAbsent);
        assert_ne!(secret_gone, ns_gone);
        assert!(render_nothing_to_remove("a", "ns", &RemoveTarget::Present).is_empty());
    }
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
    /// `apprafter.io/sealed-at` when the seal was stamped by this CLI.
    /// Absent for anything sealed before 2.22c, or applied by other means —
    /// shown as `-` rather than guessed at.
    pub sealed_at: Option<String>,
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
            // Rendered here rather than in `render_secret_list`, because
            // the column width is computed from this string and a width
            // measured on the raw form would leave the table padded for
            // characters it no longer prints.
            let sealed_at = item
                .pointer("/metadata/annotations/apprafter.io~1sealed-at")
                .and_then(Value::as_str)
                .map(cli_core::timefmt::format_timestamp);
            Some(SealedSecretSummary {
                namespace,
                name,
                keys,
                sealed_at,
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
    let sealed_w = rows
        .iter()
        .map(|r| r.sealed_at.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(6)
        .max(6);
    let mut s = format!("Sealed secrets in {scope}:\n\n");
    s.push_str(&format!(
        "  {:<ns_w$}  {:<name_w$}  {:<sealed_w$}  KEYS\n",
        "NAMESPACE",
        "NAME",
        "SEALED",
        ns_w = ns_w,
        name_w = name_w,
        sealed_w = sealed_w
    ));
    for r in rows {
        let keys = if r.keys.is_empty() {
            "(none)".to_string()
        } else {
            r.keys.join(", ")
        };
        s.push_str(&format!(
            "  {:<ns_w$}  {:<name_w$}  {:<sealed_w$}  {}\n",
            r.namespace,
            r.name,
            r.sealed_at.as_deref().unwrap_or("-"),
            keys,
            ns_w = ns_w,
            name_w = name_w,
            sealed_w = sealed_w
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
    // A LIST in a namespace that does not exist comes back empty and exit 0,
    // so without this the answer to `-n prdo` is the same "no sealed secrets"
    // the reader would get from a correctly spelled, genuinely empty one —
    // and this command's whole job is to answer "is it in the namespace I
    // think it is?".
    if let Some(ns) = namespace {
        if namespace_confirmed_missing(ns, kc.path()) {
            return Err(CliError::Other(format!(
                "namespace '{ns}' does not exist — so this is not an empty list, \
                 it is the wrong address. Run `apprafter secret list` with no \
                 `-n` to see every sealed secret and the namespace each lives in"
            )));
        }
    }
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
    fn an_unstamped_seal_shows_a_dash_rather_than_a_guess() {
        // Anything sealed before 2.22c, or applied by other means, carries no
        // provenance annotation. Inventing one would be worse than admitting
        // it is unknown — the whole value of the field is that it is a record.
        let v = json!({ "items": [sealed("shop", "old", &["k"])] });
        let rows = parse_sealed_secret_summaries(&v);
        assert_eq!(rows[0].sealed_at, None);
        assert!(
            render_secret_list(&rows, "x").contains("  -  "),
            "{}",
            render_secret_list(&rows, "x")
        );
    }

    #[test]
    fn a_stamped_seal_shows_its_timestamp_rendered_not_raw() {
        // The annotation is stored RFC3339 and used to reach the SEALED
        // column that way. "When was this last rotated?" is the question
        // the column exists for, and the stored form is the one that hides
        // the answer.
        let mut item = sealed("shop", "new", &["k"]);
        item["metadata"]["annotations"] =
            json!({ "apprafter.io/sealed-at": "2026-08-31T09:00:00+00:00" });
        let rows = parse_sealed_secret_summaries(&json!({ "items": [item] }));
        assert_eq!(rows[0].sealed_at.as_deref(), Some("2026-08-31 09:00 UTC"));
    }

    #[test]
    fn a_seal_stamped_by_something_else_keeps_whatever_it_wrote() {
        // Provenance this cannot parse is still the only record of what
        // stamped it, and the annotation is user-editable by design.
        let mut item = sealed("shop", "odd", &["k"]);
        item["metadata"]["annotations"] = json!({ "apprafter.io/sealed-at": "yesterday" });
        let rows = parse_sealed_secret_summaries(&json!({ "items": [item] }));
        assert_eq!(rows[0].sealed_at.as_deref(), Some("yesterday"));
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

/// One `secret:"<name>/<key>"` binding, resolved to the Application that
/// declares it (2.22c / D6 + D7 + D14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretBinding {
    pub namespace: String,
    pub app: String,
    /// `base`, or the environment key the override came from.
    pub scope: String,
    pub env_var: String,
    pub secret: String,
    pub key: String,
}

/// Every `secret:` binding declared by the Applications in `apps_json`.
///
/// ONE pass, read in BOTH directions — which is the point. Filtered by app
/// it answers "which secrets does this consume" (D7, diagnosis); filtered by
/// secret it answers "which applications would this change touch" (D6's
/// blast radius, D14's disclosure). Building one direction and not the other
/// would be the waste, since the scan is identical.
///
/// Pure: takes the JSON `kubectl get applications -o json` returns, so the
/// shape is tested without a cluster.
pub fn parse_secret_bindings(apps_json: &Value) -> Vec<SecretBinding> {
    let mut out = Vec::new();
    for item in apps_json
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(app) = item.pointer("/metadata/name").and_then(Value::as_str) else {
            continue;
        };
        let namespace = item
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .unwrap_or_default();

        collect_env_bindings(
            item.pointer("/spec/base/env"),
            namespace,
            app,
            "base",
            &mut out,
        );
        // Per-environment overrides declare their own bindings (2.9), and a
        // secret referenced only by one environment is just as real as a base
        // one — omitting them would under-report a blast radius, which is the
        // direction that costs.
        if let Some(envs) = item
            .pointer("/spec/environments")
            .and_then(Value::as_object)
        {
            for (env_name, env_body) in envs {
                collect_env_bindings(env_body.get("env"), namespace, app, env_name, &mut out);
            }
        }
    }
    out.sort_by(|a, b| {
        (&a.namespace, &a.secret, &a.app, &a.env_var).cmp(&(
            &b.namespace,
            &b.secret,
            &b.app,
            &b.env_var,
        ))
    });
    out
}

fn collect_env_bindings(
    env: Option<&Value>,
    namespace: &str,
    app: &str,
    scope: &str,
    out: &mut Vec<SecretBinding>,
) {
    let Some(map) = env.and_then(Value::as_object) else {
        return;
    };
    for (var, value) in map {
        // The CR carries the resolved marker `{secret: "<name>/<key>"}`
        // (ADR 0046). A literal is a plain string and a claim ref is
        // `{claim: ...}`; neither is a Secret dependency.
        let Some(path) = value.get("secret").and_then(Value::as_str) else {
            continue;
        };
        let Some((name, key)) = path.split_once('/') else {
            // The webhook rejects a malformed ref on the way in, so this
            // is defensive — but skipping silently is right: a listing is
            // not the place to re-litigate admission.
            continue;
        };
        out.push(SecretBinding {
            namespace: namespace.to_string(),
            app: app.to_string(),
            scope: scope.to_string(),
            env_var: var.clone(),
            secret: name.to_string(),
            key: key.to_string(),
        });
    }
}

/// The applications that resolve `secret` in `namespace`, deduplicated and
/// sorted — the blast radius of re-sealing it.
pub fn apps_consuming(bindings: &[SecretBinding], namespace: &str, secret: &str) -> Vec<String> {
    let mut apps: Vec<String> = bindings
        .iter()
        .filter(|b| b.namespace == namespace && b.secret == secret)
        .map(|b| b.app.clone())
        .collect();
    apps.sort();
    apps.dedup();
    apps
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use serde_json::json;

    fn app(ns: &str, name: &str, base_env: Value, envs: Value) -> Value {
        json!({
            "metadata": { "name": name, "namespace": ns },
            "spec": { "base": { "env": base_env }, "environments": envs }
        })
    }

    #[test]
    fn it_reads_secret_markers_and_ignores_the_other_env_shapes() {
        // Three value shapes share the env map (ADR 0046): a literal is a
        // plain string, a claim ref is {claim: ...}, and only {secret: ...}
        // is a Secret dependency. Counting a claim ref would inflate every
        // blast radius the D6 disclosure prints.
        let v = json!({ "items": [app("shop", "web", json!({
            "STRIPE_KEY": { "secret": "checkout/stripe-api-key" },
            "DATABASE_URL": { "claim": "pg.url" },
            "LOG_LEVEL": "info"
        }), json!({}))]});
        let b = parse_secret_bindings(&v);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].secret, "checkout");
        assert_eq!(b[0].key, "stripe-api-key");
        assert_eq!(b[0].env_var, "STRIPE_KEY");
        assert_eq!(b[0].scope, "base");
    }

    #[test]
    fn a_binding_declared_only_by_an_environment_still_counts() {
        // Under-reporting is the direction that costs: an operator told
        // "this touches one app" who then breaks three has been misled by
        // the surface that was supposed to prevent exactly that.
        let v = json!({ "items": [app("shop", "web", json!({}), json!({
            "prod": { "env": { "SENTRY_DSN": { "secret": "obs/dsn" } } }
        }))]});
        let b = parse_secret_bindings(&v);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].scope, "prod");
        assert_eq!(b[0].secret, "obs");
    }

    #[test]
    fn the_same_index_answers_both_directions() {
        // The property that made building one direction alone wasteful.
        let v = json!({ "items": [
            app("shop", "web", json!({ "A": { "secret": "shared/k" } }), json!({})),
            app("shop", "api", json!({ "B": { "secret": "shared/k" } }), json!({})),
            app("shop", "api", json!({ "C": { "secret": "other/k" } }), json!({})),
        ]});
        let b = parse_secret_bindings(&v);

        // secret -> apps (D6 blast radius, D14 disclosure)
        assert_eq!(apps_consuming(&b, "shop", "shared"), vec!["api", "web"]);
        // app -> secrets (D7 diagnosis)
        let webs: Vec<&str> = b
            .iter()
            .filter(|x| x.app == "web")
            .map(|x| x.secret.as_str())
            .collect();
        assert_eq!(webs, vec!["shared"]);
    }

    #[test]
    fn a_secret_of_the_same_name_in_another_namespace_is_a_different_secret() {
        // Sealed secrets are namespace-bound by construction, so a blast
        // radius that ignored the namespace would name innocent apps.
        let v = json!({ "items": [
            app("shop", "web", json!({ "A": { "secret": "creds/k" } }), json!({})),
            app("blog", "site", json!({ "A": { "secret": "creds/k" } }), json!({})),
        ]});
        let b = parse_secret_bindings(&v);
        assert_eq!(apps_consuming(&b, "shop", "creds"), vec!["web"]);
        assert_eq!(apps_consuming(&b, "blog", "creds"), vec!["site"]);
    }

    #[test]
    fn an_app_consuming_a_secret_twice_is_listed_once() {
        let v = json!({ "items": [app("shop", "web", json!({
            "A": { "secret": "creds/one" },
            "B": { "secret": "creds/two" }
        }), json!({}))]});
        let b = parse_secret_bindings(&v);
        assert_eq!(b.len(), 2);
        assert_eq!(apps_consuming(&b, "shop", "creds"), vec!["web"]);
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn it_stamps_who_and_when_without_disturbing_the_sealed_payload() {
        // The annotation must never touch spec: a builder that reshaped the
        // ciphertext would break the seal, and that failure would surface
        // only at unseal time on the cluster.
        let mut cr = json!({
            "metadata": { "name": "checkout", "namespace": "shop" },
            "spec": { "encryptedData": { "k": "AgB1" } }
        });
        let before = cr["spec"].clone();
        stamp_provenance(&mut cr);
        assert_eq!(cr["spec"], before, "the sealed payload was modified");

        let ann = &cr["metadata"]["annotations"];
        assert!(ann["apprafter.io/sealed-by"].is_string());
        let at = ann["apprafter.io/sealed-at"].as_str().unwrap();
        assert!(
            chrono::DateTime::parse_from_rfc3339(at).is_ok(),
            "sealed-at is not RFC3339: {at}"
        );
    }

    #[test]
    fn it_preserves_annotations_that_are_already_there() {
        let mut cr = json!({
            "metadata": {
                "name": "c", "namespace": "s",
                "annotations": { "kept": "yes" }
            }
        });
        stamp_provenance(&mut cr);
        assert_eq!(cr["metadata"]["annotations"]["kept"], "yes");
        assert!(cr["metadata"]["annotations"]["apprafter.io/sealed-by"].is_string());
    }

    #[test]
    fn a_shapeless_object_is_left_alone_rather_than_panicking() {
        // Defensive: the builder is shared and could change shape. Losing
        // provenance is acceptable; panicking mid-seal is not.
        let mut cr = json!("not an object");
        stamp_provenance(&mut cr);
        assert_eq!(cr, json!("not an object"));
    }
}
