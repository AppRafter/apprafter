// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter app …` user-application management. Track
//! B.1.79a part 3.
//!
//! Surface:
//!
//! * `apprafter app add` — register a user Application. Detects
//!   git origin from cwd or accepts explicit URL, normalises to
//!   HTTPS, reachability-checks via `git ls-remote` (skippable),
//!   writes an Argo CD `Application` CR labeled
//!   `apprafter.io/managed-by: apprafter`.
//!
//! * `apprafter app list` — table of Applications scoped to the
//!   `apps` AppProject (default), filtered to apprafter-managed
//!   ones unless `--all-managed` flips.
//!
//! * `apprafter app status` — detail view (sync + health + source
//!   + destinations + pending MigrationPlans + recent history).
//!
//! * `apprafter app remove` — delete the Argo CD CR; Argo CD
//!   tears down child resources for us via owner-ref cascade.
//!   `--keep-data` strips destructive child prune.
//!
//! All paths shell out to `kubectl` through
//! `commands::k8s_helpers` — keeps the wire format consistent
//! with the `platform` / `migration` wrappers.

use std::collections::BTreeMap;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::Command;

use cli_core::style;
use cli_core::{CliError, Result};
use serde_json::{json, Value};
use tabled::{Table, Tabled};

use cli_providers::k8s::kubectl::APPRAFTER_CLI_PIN_FIELD_MANAGER;

use crate::commands::app_index::{AppIndex, Resolution};
use crate::commands::app_open;
use crate::commands::app_open::CrRef;
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_apply_server_side, kubectl_get_json,
    kubectl_get_json_by_selector, kubectl_get_json_showing_managed_fields, kubectl_merge_patch,
};

pub(crate) const ARGOCD_NAMESPACE: &str = "argocd";
const APPRAFTER_MANAGED_LABEL: &str = "apprafter.io/managed-by=apprafter";
const APPRAFTER_SOURCE_ANNOTATION: &str = "apprafter.io/source";
/// Argo CD cascade-deletion finalizer (background variant). EXACT string
/// per the Argo CD docs — note `resources-finalizer` (NOT
/// `resources-finalization`); a typo here makes Argo ignore it and the
/// Application hangs in `Terminating` forever. `/background` deletes the
/// managed resources without blocking the delete call on child cleanup.
const ARGOCD_CASCADE_FINALIZER: &str = "resources-finalizer.argocd.argoproj.io/background";
/// GROUP-QUALIFIED on purpose — a bare `resourceclaim` collides with the
/// Kubernetes 1.32+ DRA `resourceclaims.resource.k8s.io`, and kubectl
/// resolves the collision to the built-in, so `app status` would list
/// somebody else's objects (or none).
const RESOURCECLAIM_RESOURCE: &str = "resourceclaim.apprafter.io";

/// Pure helper — the `kubectl get … -o json` arg vector every read in
/// this module shares. Extracted from the four `Command::new("kubectl")`
/// sites so the resource name, namespace scoping and `-o json` shape are
/// pinned in one place rather than re-typed per call.
pub(crate) fn kubectl_list_args(
    resource: &str,
    namespace: &str,
    selector: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "get".to_string(),
        resource.to_string(),
        "-n".to_string(),
        namespace.to_string(),
    ];
    if let Some(sel) = selector {
        args.push("-l".to_string());
        args.push(sel.to_string());
    }
    args.push("-o".to_string());
    args.push("json".to_string());
    args
}

/// Pure helper — the label selector the operator stamps on a workload's
/// pods and services.
pub(crate) fn operator_workload_selector(apprafter_app_name: &str) -> String {
    format!("app.kubernetes.io/name={apprafter_app_name}")
}

/// Pure helper — the `kubectl delete` arg vector `app remove` runs.
/// Extracted from [`remove_single_app`].
pub(crate) fn kubectl_delete_argo_app_args(argo_app_name: &str) -> Vec<String> {
    vec![
        "delete".to_string(),
        "application.argoproj.io".to_string(),
        argo_app_name.to_string(),
        "-n".to_string(),
        ARGOCD_NAMESPACE.to_string(),
    ]
}

/// One `app list` row = one REGISTRATION (one Argo CD `Application`),
/// which ADR 0062 defines as a bundle of 1..N workloads. `tabled` emits
/// the columns in declaration order.
///
/// `PROJECT` and `REV` are deliberately absent: `project` is near-always
/// `apps` and `targetRevision` near-always the default branch, so both
/// cost width without informing. They are removed from the *list*, not
/// from the product — `app status` still prints them
/// ([`status_detail_lines`]'s `project:` and `revision:` lines).
///
/// There is no time column, by the same reasoning.
#[derive(Tabled)]
struct AppRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "ENV")]
    env: String,
    /// `spec.destination.namespace` — the namespace Argo CD creates
    /// (`CreateNamespace=true`) and applies every namespaced child into.
    /// The same field [`status_detail_lines`] prints as `destination:`,
    /// down to the `?` fallback, so the two surfaces cannot disagree.
    /// ADR 0062 makes this a property of the whole registration.
    #[tabled(rename = "NAMESPACE")]
    namespace: String,
    /// How many `apprafter.io/Application` CRs this registration
    /// deploys. Not decoration: with one row per bundle, a 3-workload
    /// manifest and a single app are otherwise indistinguishable, and
    /// every `apprafter app <verb> <name>` hint the CLI prints is
    /// silently ambiguous for half the table.
    #[tabled(rename = "WORKLOADS")]
    workloads: String,
    #[tabled(rename = "REPO")]
    repo: String,
    #[tabled(rename = "SYNC")]
    sync: String,
    /// Argo CD's health verdict, folded over the bundle's workloads by
    /// [`workload_health_cell`] rather than read off the registration —
    /// see that function for why a single word is not enough at N > 1.
    /// It is a verdict on the `Application` CRs and does NOT see pod
    /// state; the footer [`HEALTH_COLUMN_NOTE`] says so under the table.
    #[tabled(rename = "HEALTH")]
    health: String,
}

/// Credential coverage gate for `app add` (1.79c S5 / ADR 0039).
/// Mirrors the `--no-ping` philosophy: advisory by default, strict
/// on opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum CoverageGate {
    /// Only warn (post-registration) when no `SourceCredential`
    /// declares a covering prefix. The default — egress-restricted
    /// clusters keep this since validity stays `Unverified`.
    #[default]
    Present,
    /// Pre-flight-block registration of a private (`https`) repo
    /// unless a `SourceCredential` whose status reports
    /// `GitValid=True` covers it. Fail-fast for clusters whose
    /// operator has egress to validate.
    Confirmed,
}

/// Read `PlatformStack/default.spec.defaultEnvironment` (ADR 0044) —
/// the cluster's soft default environment. `Ok(None)` when no
/// PlatformStack is present or the field is unset. Used to surface a
/// hint when an operator runs `app add` without `--env`.
fn fetch_platformstack_default_env(kubeconfig_path: &std::path::Path) -> Result<Option<String>> {
    match kubectl_get_json(
        "platformstack",
        Some("default"),
        Some("apprafter-system"),
        kubeconfig_path,
    )? {
        Some(ps) => Ok(ps
            .pointer("/spec/defaultEnvironment")
            .and_then(serde_json::Value::as_str)
            .map(String::from)),
        None => Ok(None),
    }
}

/// Render the cwd's `apprafter/Application.cue` and return its
/// declared `spec.environments` keys (ADR 0044). Used to validate
/// `--env <e>` against the manifest's declared environments before
/// registering the Argo CD Application.
///
/// `cue export` of the scaffolded package yields a top-level object
/// keyed by the binding name (e.g. `{ "web": { kind: "Application",
/// … } }`), NOT a single `out:` doc — so the Application doc is
/// selected by `kind == "Application"` via
/// `cli_core::manifest::parse_application` (the same doc-selection the
/// manifest parser + its integration tests use), then its
/// `spec.environments` map keys are returned. An empty vec means the
/// manifest declares no environments (base-only app).
fn get_manifest_environments(cwd: &std::path::Path) -> Result<Vec<String>> {
    // Inject the shipped schema (post-2.12 manifests don't vendor it —
    // a bare `cue export` would fail "no cue.mod/module.cue"), the same
    // way `apprafter app validate` + the cue-cmp sidecar do.
    let manifest =
        crate::commands::app_validate::parse_application_injected(&cwd.join("apprafter"))?;
    Ok(manifest
        .spec
        .environments
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default())
}

/// List the cluster's namespace names via `kubectl get namespaces`
/// (ADR 0044 / 2.9). Feeds the wizard's destination-namespace picker.
/// `Ok(vec![])` when no namespaces are returned; the caller treats a
/// listing error (no cluster / no kubeconfig) as "no list" and falls
/// back to a plain text namespace prompt.
fn list_namespace_names(kubeconfig_path: &std::path::Path) -> Result<Vec<String>> {
    match kubectl_get_json("namespaces", None, None, kubeconfig_path)? {
        Some(v) => Ok(v
            .pointer("/items")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|it| {
                        it.pointer("/metadata/name")
                            .and_then(serde_json::Value::as_str)
                            .map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default()),
        None => Ok(Vec::new()),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn add(
    git_url: Option<String>,
    name: Option<String>,
    branch: Option<String>,
    path: &str,
    project: &str,
    namespace: &str,
    remote: &str,
    no_ping: bool,
    coverage_gate: CoverageGate,
    env: Option<String>,
    no_interactive: bool,
    scaffold_flag: bool,
) -> Result<()> {
    let stdin_is_tty = io::stdin().is_terminal();
    let stdout_is_tty = std::io::stdout().is_terminal();
    let use_wizard =
        crate::commands::app_wizard::should_use_wizard(no_interactive, stdin_is_tty, stdout_is_tty);

    // Step 0 (Track B.1.79b Part 3b) — bridge from a fresh
    // repo to a registered app. Check `<cwd>/apprafter/
    // Application.cue`; missing → scaffold interactively
    // (TTY) or per `--scaffold` flag (non-TTY), or refuse
    // with a pointer to standalone `apprafter app scaffold`.
    // After this block, `<cwd>/apprafter/Application.cue` is
    // guaranteed to exist for the wizard / non-interactive
    // flow below.
    let cwd = std::env::current_dir().map_err(|e| CliError::Other(format!("cwd: {e}")))?;
    let scaffold_target = cwd.join("apprafter").join("Application.cue");
    let decision = crate::commands::scaffold_wizard::decide_scaffold_step(
        scaffold_target.exists(),
        use_wizard,
        scaffold_flag,
        git_url.is_some(),
    );
    // Step 0 may settle a namespace ≠ clap's `--namespace`
    // default. Carry it forward to the outer wizard so the
    // operator doesn't double-enter and end up with the scaffold's
    // `metadata.namespace` mismatched against Argo CD's
    // `destination.namespace` (walk-fix post-Part-3b — the
    // procvue/apprafter mismatch operator reported).
    let mut effective_namespace = namespace.to_string();
    match decision {
        crate::commands::scaffold_wizard::ScaffoldDecision::Skip => {}
        crate::commands::scaffold_wizard::ScaffoldDecision::Interactive => {
            let suggested = cwd
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .map(|raw| sanitise_for_dns_1123(&raw))
                .unwrap_or_else(|| "app".into());
            let out = crate::commands::scaffold_wizard::run_step_zero(&cwd, &suggested)?;
            effective_namespace = out.namespace;
        }
        crate::commands::scaffold_wizard::ScaffoldDecision::NonInteractive => {
            // `--no-interactive --scaffold`: auto-detect and
            // generate. Pass the clap `--namespace` value
            // through to scaffold so both layers agree (walk-fix
            // post-Part-3b — was hard-defaulting to "apprafter"
            // regardless of operator's flag).
            crate::commands::app_scaffold::scaffold(crate::commands::app_scaffold::ScaffoldOpts {
                runtime: None,
                name: None,
                namespace: Some(effective_namespace.clone()),
                path: cwd.clone(),
                force: false,
                needs: Vec::new(),
            })?;
        }
        crate::commands::scaffold_wizard::ScaffoldDecision::Refuse => {
            return Err(CliError::Other(format!(
                "apprafter/Application.cue not found in {}. Run \
                 `apprafter app scaffold` first to generate one, or rerun \
                 with `--scaffold` to chain scaffolding into this command.",
                cwd.display()
            )));
        }
    }

    if use_wizard {
        return add_via_wizard(
            git_url,
            name,
            branch,
            path,
            project,
            &effective_namespace,
            remote,
            no_ping,
            coverage_gate,
            env,
        );
    }

    let (repo_url, derived_branch) = match git_url {
        Some(explicit) => (normalise_git_url(&explicit), None),
        None => detect_git_repo_for_cwd(remote)?,
    };

    let derived_name = name.unwrap_or_else(|| derive_app_name(&repo_url));
    validate_dns_1123(&derived_name)?;

    // ADR 0044 (2.9): one `app add --env <env>` produces ONE Argo CD
    // Application named `<name>-<env>` (or `<name>` for a base-only
    // deploy). The env (when set) must be one of the manifest's
    // declared `spec.environments` keys.
    let argo_app_name = match &env {
        Some(e) => format!("{derived_name}-{e}"),
        None => derived_name.clone(),
    };
    if let Some(e) = env.as_deref() {
        // `<name>-<e>` becomes the Argo CD Application `metadata.name`, so the
        // env value has to satisfy DNS-1123 too — validate it FIRST so an
        // invalid `--env foo_bar` fails fast with a clear message regardless of
        // whether the manifest renders (on the manifest-unrenderable degrade
        // path below an invalid env would otherwise reach `kubectl apply` and
        // surface a cryptic RFC-1123 error).
        validate_dns_1123(e)?;
        // Validate against the cwd manifest's declared environments.
        // Degrade gracefully when the manifest can't be rendered (e.g.
        // a remote-only `app add <git-url>` from outside the repo) —
        // warn rather than hard-fail, so a no-cwd-manifest add still
        // works (the operator vouches the env exists upstream).
        match get_manifest_environments(&cwd) {
            Ok(envs) => {
                if let Some(msg) = undeclared_env_error(e, &envs) {
                    return Err(CliError::Other(msg));
                }
            }
            Err(err) => {
                eprintln!(
                    "⚠ Could not render apprafter/Application.cue to validate `--env {e}` \
                     ({err}). Proceeding — ensure `spec.environments.{e}` exists upstream."
                );
            }
        }
    }

    let target_revision = branch.or(derived_branch).unwrap_or_else(|| "main".into());

    if !no_ping {
        ensure_repo_reachable(&repo_url)?;
    }

    let kc = ensure_kubeconfig_tempfile()?;

    // Pre-flight: refuse if an Application with this name already
    // exists in the `argocd` namespace. Argo CD's apiserver
    // wouldn't allow a duplicate `metadata.name` anyway, but the
    // kubectl 409 message is cryptic for new users — give a
    // cleaner hint and an explicit pointer to `app status` /
    // `app remove` instead. Keyed on the env-suffixed Argo name so
    // `<name>-dev` and `<name>-prod` coexist for one logical app.
    let existing = kubectl_get_json(
        "application.argoproj.io",
        Some(&argo_app_name),
        Some(ARGOCD_NAMESPACE),
        kc.path(),
    )?;
    if existing.is_some() {
        return Err(CliError::Other(already_registered_error(
            &argo_app_name,
            &derived_name,
            env.as_deref(),
        )));
    }

    // Coverage gate (confirmed): fail-fast before registering when no
    // validated credential covers a private repo. `present` (default)
    // only warns post-registration, below.
    if matches!(coverage_gate, CoverageGate::Confirmed) {
        enforce_confirmed_coverage(&repo_url, kc.path())?;
    }

    let manifest = build_application_manifest(
        &derived_name,
        &argo_app_name,
        &repo_url,
        &target_revision,
        path,
        project,
        &effective_namespace,
        env.as_deref(),
    );
    apply_application_manifest(&manifest, kc.path())?;

    // Best-effort — a missing PlatformStack simply omits the environment
    // line for a base-only deploy.
    let cluster_default_env = match env {
        Some(_) => None,
        None => fetch_platformstack_default_env(kc.path()).ok().flatten(),
    };
    for line in registration_summary_lines(
        &argo_app_name,
        project,
        &repo_url,
        &target_revision,
        path,
        &effective_namespace,
        env.as_deref(),
        cluster_default_env.as_deref(),
    ) {
        println!("{line}");
    }
    println!();
    warn_if_no_matching_repo_creds(&repo_url, kc.path());
    println!("Argo CD will sync the workload within a reconcile cycle. State:");
    // `app status` takes the LOGICAL name (it aggregates every
    // `<name>-<env>` deployment via the apprafter.io/application label),
    // NOT the per-env Argo app name — mirror the collision-path hint above.
    println!("  apprafter app status {derived_name}");
    Ok(())
}

/// Pure helper — reject an `--env` the cwd manifest does not declare.
/// Extracted from [`add`]; `None` means the env is declared and `add`
/// proceeds.
///
/// INVARIANT: a manifest that declares NO environments still produces a
/// rejection (with `(none declared)` in place of the list), because the
/// alternative — treating "nothing declared" as "everything allowed" —
/// registers an Argo CD Application whose environment renders to nothing.
pub(crate) fn undeclared_env_error(env: &str, declared: &[String]) -> Option<String> {
    if declared.iter().any(|d| d == env) {
        return None;
    }
    let list = if declared.is_empty() {
        "(none declared)".to_string()
    } else {
        declared.join(", ")
    };
    Some(format!(
        "environment '{env}' is not declared in this app's manifest. \
         Declared environments: {list}. Add a \
         `spec.environments.{env}` block to apprafter/Application.cue, \
         or pass one of the declared environments to `--env`."
    ))
}

/// Pure helper — the name-collision refusal `add` raises when the Argo CD
/// Application already exists. Extracted from [`add`].
///
/// INVARIANT (the logical-name UX rule): every suggested command names the
/// LOGICAL app plus `--env`, never the `<name>-<env>` Argo CD identity —
/// `app status <name>` aggregates the environments and `app remove
/// <name> --env <e>` targets one, so echoing the Argo name back would
/// hand the reader a name none of our verbs accept.
pub(crate) fn already_registered_error(
    argo_app_name: &str,
    logical_name: &str,
    env: Option<&str>,
) -> String {
    let remove_hint = match env {
        Some(e) => format!("apprafter app remove {logical_name} --env {e}"),
        None => format!("apprafter app remove {logical_name}"),
    };
    format!(
        "Application '{argo_app_name}' is already registered in namespace \
         {ARGOCD_NAMESPACE}. Run `apprafter app status {logical_name}` to inspect all \
         environment deployments of this app, `{remove_hint}` to cascade-delete this \
         one, or pass a different `--name` / `--env`."
    )
}

/// Pure helper — the post-registration summary block. Extracted from
/// [`add`], with the PlatformStack lookup hoisted to the caller so the
/// rendering is a function of its arguments.
///
/// INVARIANT: a `--env` deploy states its environment flatly; a base-only
/// deploy either names the cluster's soft default (so the reader learns
/// which environment their manifest actually renders against) or, when no
/// PlatformStack answered, omits the line rather than claiming `(base)`
/// means nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn registration_summary_lines(
    argo_app_name: &str,
    project: &str,
    repo_url: &str,
    target_revision: &str,
    path: &str,
    namespace: &str,
    env: Option<&str>,
    cluster_default_env: Option<&str>,
) -> Vec<String> {
    let mut out = vec![
        format!("✓ Application '{argo_app_name}' registered in AppProject '{project}'."),
        format!("  Repo:        {repo_url}"),
        format!("  Revision:    {target_revision}"),
        format!("  Path:        {path}"),
        format!("  Destination: {namespace} (created if missing)"),
    ];
    match env {
        Some(e) => out.push(format!("  Environment: {e}")),
        None => {
            if let Some(default_env) = cluster_default_env {
                out.push(format!(
                    "  Environment: (base — cluster default is '{default_env}'; \
                     pass `--env {default_env}` to pin it)"
                ));
            }
        }
    }
    out
}

/// Confirmed-mode coverage gate (1.79c S5). Refuse to register a
/// private (`https`) repo unless a `SourceCredential` whose status
/// reports `GitValid=True` covers its URL. Non-`https` repos (SSH)
/// are not gated — Argo CD uses a different cred shape we don't
/// probe. Unlike the `present`-mode warn (best-effort), a failure to
/// fetch credentials surfaces as an error here: confirmed mode is an
/// explicit "I want this verified before proceeding".
fn enforce_confirmed_coverage(repo_url: &str, kubeconfig_path: &Path) -> Result<()> {
    if !repo_url.starts_with("https://") {
        return Ok(());
    }
    let creds = crate::commands::repo_creds::fetch_source_credentials_public(kubeconfig_path)?;
    match confirmed_coverage_error(
        repo_url,
        crate::commands::repo_creds::valid_credential_covers(&creds, repo_url),
        crate::commands::repo_creds::any_credential_covers(&creds, repo_url),
    ) {
        Some(msg) => Err(CliError::Other(msg)),
        None => Ok(()),
    }
}

/// Pure helper — the confirmed-gate verdict, given what the cluster's
/// `SourceCredential`s cover. Extracted from
/// [`enforce_confirmed_coverage`]; `None` admits the repo.
///
/// INVARIANT: "covered but unvalidated" and "not covered at all" are
/// DIFFERENT refusals. Collapsing them would send an operator who already
/// registered the right credential off to register it a second time,
/// when what they need is to validate the one they have.
pub(crate) fn confirmed_coverage_error(
    repo_url: &str,
    valid_credential_covers: bool,
    any_credential_covers: bool,
) -> Option<String> {
    if valid_credential_covers {
        return None;
    }
    let detail = if any_credential_covers {
        "a SourceCredential covers it but its status is not GitValid=True \
         (Unverified or Invalid). Validate the credential, or rerun with \
         `--coverage-gate present` (the default) if the cluster has no egress to validate"
    } else {
        "no SourceCredential covers it. Register one with `apprafter repo creds add`, \
         or rerun with `--coverage-gate present` (the default) if the repo is public"
    };
    Some(format!(
        "coverage gate (confirmed): {repo_url} not admitted — {detail}."
    ))
}

/// Post-register cred check (walk-fix #1 + #2 post-Part-3b).
/// `git ls-remote` succeeds locally on private repos when the
/// operator's user-side git is authenticated, but Argo CD's
/// repo-server has its own credential store; an unmatched
/// private repo lands in a sync failure later. Walk-fix #2
/// adds auto-derived defaults (cred name, URL prefix at org
/// level) and a PAT-creation URL for GitHub / GitLab so the
/// operator doesn't hunt for it.
///
/// Gated on anonymous publicness: a credential-less probe of the
/// repo's smart-HTTP advert decides whether Argo CD's repo-server
/// could clone it without creds — public repos stay quiet, only
/// genuinely private ones get the notice. Best-effort — failure to
/// fetch secrets or probe prints nothing, does not fail the command.
fn warn_if_no_matching_repo_creds(repo_url: &str, kubeconfig_path: &Path) {
    if !repo_url.starts_with("https://") {
        // Only HTTPS triggers the warning — git@ / ssh://
        // shapes typically use SSH keys which Argo CD picks
        // up via a different cred entry shape we don't probe.
        return;
    }
    let creds = match crate::commands::repo_creds::fetch_source_credentials_public(kubeconfig_path)
    {
        Ok(c) => c,
        Err(_) => return,
    };
    if crate::commands::repo_creds::any_credential_covers(&creds, repo_url) {
        return;
    }

    // No matching creds — but a credential-less Argo CD repo-server
    // can clone a PUBLIC repo just fine, so the PAT notice would be
    // noise there. The creds check above is cheap and already done;
    // only NOW (the notice-eligible case) pay for the network probe.
    // A 200 from the anonymous smart-HTTP advert → public → suppress.
    if is_git_repo_anonymously_public(repo_url) {
        return;
    }

    for line in creds_notice_lines(repo_url, derive_creds_suggestion(repo_url).as_ref()) {
        println!("{line}");
    }
}

/// Pure helper — the "this repo is private and has no credentials"
/// notice. Extracted from [`warn_if_no_matching_repo_creds`], with the
/// suggestion derivation hoisted to the caller.
///
/// INVARIANT: the notice degrades in two steps rather than one. A
/// provider we know gets a numbered PAT-creation walkthrough; a provider
/// we merely parsed gets a one-line `repo creds add` with the name and
/// prefix pre-filled; an unparseable URL still gets the command shape
/// with placeholders. The operator is never left with only "register a
/// credential" and no command.
pub(crate) fn creds_notice_lines(
    repo_url: &str,
    suggestion: Option<&CredsSuggestion>,
) -> Vec<String> {
    let mut out = vec![
        String::new(),
        format!("ℹ {repo_url} is private (no anonymous Git access) and has no"),
        "  matching credentials in Argo CD — register one so the repo-server".to_string(),
        "  can clone it:".to_string(),
    ];
    match suggestion {
        Some(s) => match s.pat_creation_url.as_deref() {
            Some(pat_url) => {
                out.push("    1. Generate a PAT here:".to_string());
                out.push(format!("       {pat_url}"));
                out.push(
                    "       Required scopes: `repo` for code; add `read:packages`".to_string(),
                );
                out.push(
                    "       if your CI publishes container images to the same provider."
                        .to_string(),
                );
                out.push("    2. Register it with AppRafter:".to_string());
                out.push(format!(
                    "       apprafter repo creds add {} --url-prefix {} --token <paste-the-pat>",
                    s.suggested_name, s.url_prefix
                ));
            }
            None => out.push(format!(
                "    apprafter repo creds add {} --url-prefix {} --token <pat>",
                s.suggested_name, s.url_prefix
            )),
        },
        None => out.push(
            "    apprafter repo creds add <name> --url-prefix <prefix> --token <pat>".to_string(),
        ),
    }
    out
}

/// Map a git smart-HTTP probe status to a publicness verdict.
/// `true` = anonymously public (suppress the cred notice): the
/// credential-less probe returned 200, so a credential-less client
/// like Argo CD's repo-server can clone it too. 401/403 = auth
/// required → private. Anything else, or a transport error
/// (`None`), is treated conservatively as NOT public, so the
/// operator still sees the credential guidance.
fn git_probe_verdict(status: Option<u16>) -> bool {
    matches!(status, Some(200))
}

/// Anonymously probe a Git HTTPS repo's smart-HTTP advertisement
/// (`<url>/info/refs?service=git-upload-pack`) with NO auth header
/// — the exact view a credential-less Argo CD repo-server has. A
/// 200 means the repo is publicly cloneable; 401/403 means private.
/// 5s timeout; any transport error is treated as not-public.
fn is_git_repo_anonymously_public(repo_url: &str) -> bool {
    let probe = format!("{repo_url}/info/refs?service=git-upload-pack");
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let status = match agent.get(&probe).call() {
        Ok(resp) => Some(resp.status()),
        Err(ureq::Error::Status(code, _)) => Some(code),
        Err(ureq::Error::Transport(_)) => None,
    };
    git_probe_verdict(status)
}

/// Surface for the walk-fix #2 hint — auto-derived defaults
/// from a Git HTTPS URL. Pure-fn tests cover every supported
/// provider shape, plus the generic fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CredsSuggestion {
    /// Suggested cred-secret name — DNS-1123. e.g.
    /// `github-procvue`, `gitlab-acme`.
    pub suggested_name: String,
    /// URL prefix the operator should pass to `--url-prefix`.
    /// Org-level so one PAT covers every repo in the org.
    pub url_prefix: String,
    /// Pre-filled PAT-creation URL (provider-specific). None
    /// for unknown providers — operator still gets the
    /// `repo creds add` template, just without the link.
    pub pat_creation_url: Option<String>,
}

/// Derive cred-add hint values from a repo URL. Recognises
/// `github.com` and `gitlab.com`; everything else falls
/// through to a generic "host-org" name + org-level prefix
/// heuristic.
pub(crate) fn derive_creds_suggestion(repo_url: &str) -> Option<CredsSuggestion> {
    let rest = repo_url.strip_prefix("https://")?;
    let mut iter = rest.split('/');
    let host = iter.next()?;
    let org = iter.next()?;
    if host.is_empty() || org.is_empty() {
        return None;
    }

    let url_prefix = format!("https://{host}/{org}");
    let host_slug = host.split('.').next().unwrap_or(host).to_ascii_lowercase();
    let org_slug = org.to_ascii_lowercase();
    let suggested_name = format!("{host_slug}-{org_slug}");

    let pat_creation_url = match host {
        "github.com" => Some(format!(
            "https://github.com/settings/tokens/new\
             ?scopes=repo,read:packages\
             &description=AppRafter%20{org_slug}",
        )),
        "gitlab.com" => Some(format!(
            "https://gitlab.com/-/user_settings/personal_access_tokens\
             ?name=AppRafter+{org_slug}\
             &scopes=read_repository,read_registry",
        )),
        _ => None,
    };

    Some(CredsSuggestion {
        suggested_name,
        url_prefix,
        pat_creation_url,
    })
}

/// Pure helper — the two wizard pickers a parsed manifest feeds:
/// `(declared environments, manifest namespace)`. Extracted from
/// [`add_via_wizard`].
///
/// INVARIANT: a manifest with NO `spec.environments` yields an EMPTY
/// list, which the wizard reads as "base-only, hide the env picker" —
/// distinct from the parse-failure path, which warns. Conflating the two
/// is what silently hid the picker for every post-2.12 app.
pub(crate) fn wizard_manifest_pickers(
    manifest: &cli_core::manifest::ApplicationManifest,
) -> (Vec<String>, Option<String>) {
    let envs = manifest
        .spec
        .environments
        .as_ref()
        .map(|e| e.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    (envs, manifest.metadata.namespace.clone())
}

/// Wizard entry point — gathers any missing field via inquire
/// prompts and re-dispatches to the non-interactive `add` with
/// `no_interactive=true` to avoid recursion. The flag values
/// above are passed through verbatim; cwd detection pre-fills
/// the wizard's Git URL and branch suggestions.
#[allow(clippy::too_many_arguments)]
fn add_via_wizard(
    git_url: Option<String>,
    name: Option<String>,
    branch: Option<String>,
    path: &str,
    project: &str,
    namespace: &str,
    remote: &str,
    no_ping: bool,
    coverage_gate: CoverageGate,
    env: Option<String>,
) -> Result<()> {
    let detected_origin = crate::commands::app_wizard::detect_git_origin(remote);
    let detected_branch = crate::commands::app_wizard::detect_git_branch();
    let detected_path = crate::commands::app_wizard::detect_path_relative_to_repo_root();

    // ADR 0044 (2.9): gather the wizard's cluster-/manifest-derived
    // pickers up front, all best-effort so an offline / no-cluster
    // `app add` still runs the wizard (env picker hidden, namespace
    // picker degrades to plain text):
    //   * declared envs — from the cwd manifest's `spec.environments`.
    //   * platform default env — `PlatformStack.spec.defaultEnvironment`.
    //   * existing namespaces — `kubectl get namespaces`.
    // The kubeconfig tempfile is resolved once and reused for both
    // cluster reads.
    let cwd = std::env::current_dir().map_err(|e| CliError::Other(format!("cwd: {e}")))?;
    // Parse the manifest ONCE (schema injected — post-2.12 apps don't vendor
    // it) for BOTH the env picker's declared environments AND the namespace
    // picker's preselect (`metadata.namespace`). `Ok` with no environments =
    // base-only (no env picker, no warning); `Err` = couldn't render, so warn
    // rather than silently hiding the pickers.
    let (declared_envs, manifest_namespace) =
        match crate::commands::app_validate::parse_application_injected(&cwd.join("apprafter")) {
            Ok(m) => wizard_manifest_pickers(&m),
            Err(e) => {
                eprintln!(
                    "⚠ Could not read apprafter/Application.cue ({e}); the environment \
                     picker is hidden and the namespace isn't preselected. Pass \
                     `--env <env>` / `--namespace <ns>` to set them."
                );
                (Vec::new(), None)
            }
        };
    let (platform_default, existing_namespaces) = match ensure_kubeconfig_tempfile() {
        Ok(kc) => (
            fetch_platformstack_default_env(kc.path()).ok().flatten(),
            list_namespace_names(kc.path()).unwrap_or_default(),
        ),
        Err(_) => (None, Vec::new()),
    };

    let inputs = crate::commands::app_wizard::WizardInputs {
        git_url,
        name,
        branch,
        path: Some(path.to_string()),
        project: Some(project.to_string()),
        namespace: Some(namespace.to_string()),
        detected_origin,
        detected_branch,
        detected_path,
        // A `--env` supplied in a TTY run pre-fills the picker (the wizard
        // prefers a non-empty `inputs.env`); None ⇒ prompt.
        env,
    };
    let out = crate::commands::app_wizard::run(
        inputs,
        &declared_envs,
        platform_default.as_deref(),
        manifest_namespace.as_deref(),
        &existing_namespaces,
    )?;
    add(
        Some(out.git_url),
        Some(out.name),
        Some(out.branch),
        &out.path,
        &out.project,
        &out.namespace,
        remote,
        no_ping,
        coverage_gate,
        out.env, // ADR 0044 (2.9): the wizard's chosen environment
        // (None for a base-only deploy when the manifest
        // declares no environments).
        true, // no_interactive — prevent recursion into the wizard.
        false, // scaffold_flag — step 0 already ran in the outer call;
              // by here `<cwd>/apprafter/Application.cue` exists and
              // `decide_scaffold_step` will return Skip on re-entry.
    )
}

/// Cwd-basename → DNS-1123 lowercase. `MyProject` → `myproject`,
/// `acme_app` → `acme-app`, leading/trailing non-`[a-z0-9]`
/// characters trimmed; empty result falls back to `"app"`.
/// Used for step 0's suggested name default; operators see this
/// in the wizard's "name" prompt and can override.
fn sanitise_for_dns_1123(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Collapse repeated dashes ⇒ single.
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    // Trim leading / trailing dashes.
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "app".to_string()
    } else if trimmed.len() > 63 {
        trimmed[..63].trim_end_matches('-').to_string()
    } else {
        trimmed
    }
}

pub fn list(project: &str, all_projects: bool, all_managed: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    let out = Command::new("kubectl")
        .args(kubectl_list_args(
            "application.argoproj.io",
            ARGOCD_NAMESPACE,
            list_label_selector(all_managed),
        ))
        .env("KUBECONFIG", kc.path())
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl get applications failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;

    let filtered = filter_apps_for_list(&parsed, project, all_projects);

    if filtered.is_empty() {
        for line in empty_list_lines(project, all_projects, all_managed) {
            println!("{line}");
        }
        return Ok(());
    }

    let rows: Vec<AppRow> = filtered.iter().map(app_row).collect();
    println!("{}", Table::new(&rows));
    // Printed once, under the table, and only when there is a table —
    // the early return above owns the empty case. `app_rollup`'s
    // single trailing `run \`apprafter app status <name>\`` line is the
    // house shape for this.
    println!("{HEALTH_COLUMN_NOTE}");
    Ok(())
}

/// The one-line footer under `app list`'s table.
///
/// The `HEALTH` column reads like pod health and is not: the chart's
/// `apprafter.io_Application` health script keys on `status.phase`,
/// which the operator sets to `Ready` the moment it applies the
/// Deployment, so a CrashLooping application renders `Healthy` here.
/// This change does not fix that — it is older and lives in the chart —
/// but a column that quietly implies a guarantee it does not give is
/// worse than one that names its own limit and points at the command
/// that does look at pods.
pub(crate) const HEALTH_COLUMN_NOTE: &str =
    "HEALTH is Argo CD's verdict on the Application CRs; it does not see pod state — \
     run `apprafter app status <name>`.";

/// Pure helper — the label selector `app list` reads with. Extracted
/// from [`list`].
///
/// INVARIANT: the DEFAULT is filtered. Argo CD's `argocd` namespace holds
/// the platform's own root Application and every component under it;
/// listing those alongside a developer's apps would bury the answer.
/// `--all-managed` is the opt-in that drops the filter.
pub(crate) fn list_label_selector(all_managed: bool) -> Option<&'static str> {
    (!all_managed).then_some(APPRAFTER_MANAGED_LABEL)
}

/// Pure helper — narrow a `kubectl get applications -o json` payload to the
/// rows `app list` shows. Extracted from [`list`] so the AppProject filter
/// is testable without a cluster.
///
/// INVARIANT: without `--all-projects` an Application whose
/// `spec.project` is absent is DROPPED, not kept — the filter is an
/// allow-list on an exact match, so an unparseable CR cannot leak into a
/// project-scoped listing.
pub(crate) fn filter_apps_for_list(
    payload: &Value,
    project: &str,
    all_projects: bool,
) -> Vec<Value> {
    let items: Vec<Value> = payload
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if all_projects {
        return items;
    }
    items
        .into_iter()
        .filter(|app| {
            app.pointer("/spec/project")
                .and_then(Value::as_str)
                .map(|p| p == project)
                .unwrap_or(false)
        })
        .collect()
}

/// Pure helper — what `app list` says when nothing matched. Extracted
/// from [`list`]. The `--all-managed` hint is suppressed when that flag
/// is already set, because suggesting a flag the reader just passed is
/// how a CLI teaches people to stop reading its hints.
pub(crate) fn empty_list_lines(
    project: &str,
    all_projects: bool,
    all_managed: bool,
) -> Vec<String> {
    let mut out = vec![if all_projects {
        "No apprafter-managed Applications in the cluster.".to_string()
    } else {
        format!("No apprafter-managed Applications in AppProject '{project}'.")
    }];
    if !all_managed {
        out.push(
            "Hint: try `--all-managed` to list Applications that were not registered \
             through `apprafter app add`."
                .to_string(),
        );
    }
    out
}

pub fn status(name: &str, show_resources: bool, workload: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // ADR 0044 (2.9): `name` is the LOGICAL app name. The same app is
    // deployed per environment as SEPARATE Argo CD Applications named
    // `<name>-<env>`, grouped by the `apprafter.io/application=<name>`
    // label the operator + `app add` both stamp. Aggregate across them.
    let mut apps = kubectl_get_json_by_selector(
        "application.argoproj.io",
        &format!("apprafter.io/application={name}"),
        Some(ARGOCD_NAMESPACE),
        kc.path(),
    )?;

    // Backward-compat: a pre-2.9 app was registered as a single Argo CD
    // Application named exactly `<name>` with NO `apprafter.io/application`
    // label, so the selector matches nothing. Fall back to the old
    // single-name lookup; if that's also absent, the not-found error.
    if apps.is_empty() {
        let app = kubectl_get_json(
            "application.argoproj.io",
            Some(name),
            Some(ARGOCD_NAMESPACE),
            kc.path(),
        )?
        .ok_or_else(|| {
            CliError::Other(format!(
                "Application '{name}' not found in namespace {ARGOCD_NAMESPACE}. Check \
                 `apprafter app list` for the registered applications."
            ))
        })?;
        apps.push(app);
    }

    // Deterministic ordering — by environment then Argo name — so the
    // per-env sections render the same way every run.
    for line in env_deployment_index_lines(name, &summarize_deployments(&apps)) {
        println!("{line}");
    }

    let mut sorted: Vec<&Value> = apps.iter().collect();
    sorted.sort_by_key(|a| deployment_sort_key(a));

    let last = sorted.len().saturating_sub(1);
    // ADR 0062: `--workload` narrows INSIDE each registration, and the
    // registrations here are the per-environment deployments of one
    // application. A workload that exists in `prod` but not in `dev` must
    // not abort the whole command — each section says what it holds, and
    // only a selector that matched NOWHERE is an error.
    let mut matched_any = false;
    for (idx, app) in sorted.iter().enumerate() {
        matched_any |= print_app_detail(name, app, show_resources, workload, kc.path());
        if idx != last {
            println!();
            println!("{}", "─".repeat(40));
            println!();
        }
    }

    if let Some(asked) = workload {
        if !matched_any {
            let mut available: Vec<String> = sorted
                .iter()
                .flat_map(|a| crate::commands::app_open::apprafter_app_refs(a))
                .map(|r| r.name)
                .collect();
            available.sort();
            available.dedup();
            return Err(CliError::Other(unknown_workload_message(
                name, asked, &available,
            )));
        }
    }

    Ok(())
}

/// Pure helper — what `app status --workload <name>` says when no
/// registration of the application deploys that workload.
///
/// It names what DOES exist, because every way to reach this message is
/// a wrong `--workload` VALUE against a positional that already
/// resolved, and the candidate list is what corrects it. A positional
/// that is a workload name never gets here: it fails earlier in
/// [`status`], at the `Application '<name>' not found in namespace
/// argocd` error, which is where that courtesy belongs (ADR 0062
/// §Addressing — "the courtesy lives on the error path").
///
/// The addressing sentence stays anyway, for the one reachable cause
/// that is not a plain typo: an application whose workload is named
/// something else, which this repository's own tests pin as supported
/// (`app.rs`: Argo CD app `cms` rendering an AppRafter Application
/// `landing-cms`). A reader who types `--workload cms` there guessed the
/// application's name for the workload's, and needs to be told which
/// argument holds which — with the real answer listed one line above.
///
/// An EMPTY `available` is reachable — every registration of the app is
/// registered but unsynced — and must not render as "It deploys: .".
pub(crate) fn unknown_workload_message(
    application: &str,
    asked: &str,
    available: &[String],
) -> String {
    let what_exists = if available.is_empty() {
        "It deploys no workload yet — Argo CD has not synced it.".to_string()
    } else {
        format!("It deploys: {}.", available.join(", "))
    };
    format!(
        "Application '{application}' deploys no workload '{asked}'. {what_exists}\n\
         The positional argument names the APPLICATION; a workload inside it is \
         addressed only by `--workload` (ADR 0062)."
    )
}

/// Pure helper — the index `app status` prints ahead of the per-env
/// detail sections. Extracted from [`status`].
///
/// INVARIANT: a SINGLE deployment renders NO index. The header would
/// otherwise announce "1 environment deployments" above the very block it
/// summarises, and the base-only case — the overwhelming majority — would
/// grow a section that says nothing.
pub(crate) fn env_deployment_index_lines(
    name: &str,
    summaries: &[DeploymentSummary],
) -> Vec<String> {
    if summaries.len() <= 1 {
        return vec![];
    }
    let mut out = vec![format!(
        "Application '{name}' — {} environment deployments:",
        summaries.len()
    )];
    out.extend(
        summaries
            .iter()
            .map(|s| format!("  • {} ({})", s.argo_name, s.environment)),
    );
    out.push(String::new());
    out
}

/// Render ONE Argo CD Application's full status block — the Argo CD
/// summary (`print_status`) plus, for the workloads that registration
/// deploys, either one full detail block or the bundle summary.
/// Factored out of `status` so the per-environment aggregation loop
/// reuses the identical single-app rendering for every `<name>-<env>`.
///
/// `application` is the LOGICAL name the user typed — the thing the
/// positional argument of every `app` verb names (ADR 0062) — so the
/// summary's `--workload` hint quotes a command that works, not the
/// `<name>-<env>` Argo object name which the positional does not take.
///
/// Returns whether this registration rendered a workload the caller
/// asked for: `status` turns "no registration matched `--workload`" into
/// an error, and cannot tell that from "one env has it and the other
/// does not" without this.
fn print_app_detail(
    application: &str,
    app: &Value,
    show_resources: bool,
    workload: Option<&str>,
    kubeconfig_path: &Path,
) -> bool {
    print_status(app);

    // ADR 0062: one registration deploys 1..N workloads. Until 2.27 this
    // took the FIRST and scoped all five downstream reads to it, so a
    // sibling in CrashLoopBackOff printed as a clean healthy block.
    // `apprafter_app_refs` is that list; `status_render_plan` is the
    // decision over it, pure and table-tested.
    let refs = crate::commands::app_open::apprafter_app_refs(app);
    let plan = status_render_plan(&refs, workload);
    // Answered by the PLAN, before any rendering: whether the selector
    // named something here is a question about addressing, and whether
    // the block can be drawn is a question about renderability. Deriving
    // the first from the second made an unknown namespace report
    // "deploys no workload 'web'. It deploys: web." and exit non-zero —
    // on a state `apprafter_app_refs` documents as real.
    let matched = plan_matched_selector(&plan);

    match plan {
        StatusPlan::Detail(r) => {
            // The namespace is the ref's own, which already falls back to
            // the registration's `spec.destination.namespace` — see
            // `apprafter_app_refs`. `None` is UNKNOWN and must never reach
            // `kubectl -n`, so it renders the same "not synced yet" line
            // the pre-2.27 `(_, None)` arm printed.
            match r.namespace.as_deref() {
                Some(ns) => print_workload_detail(&r.name, ns, kubeconfig_path),
                None => {
                    println!();
                    println!("(workload detail unavailable — app not synced yet)");
                }
            }
        }
        StatusPlan::Summary(workloads) => {
            print_workload_summary(application, &workloads, kubeconfig_path);
        }
        StatusPlan::NoWorkloads => {
            println!();
            println!("(workload detail unavailable — app not synced yet)");
        }
        StatusPlan::UnknownWorkload { asked, available } => {
            // Per-registration and NOT fatal: with two env deployments the
            // workload may live in one of them. `status` raises the error
            // only when no registration matched at all.
            println!();
            println!(
                "(no workload '{asked}' here — this deployment has: {})",
                if available.is_empty() {
                    "none yet".to_string()
                } else {
                    available.join(", ")
                }
            );
        }
    }

    if show_resources {
        print_argocd_resources(app);
    }
    matched
}

/// Render ONE workload's detail block — the inner AppRafter CR phase,
/// pods, services, resource claims and secret bindings (each a
/// best-effort kubectl read).
///
/// Extracted VERBATIM from the `(Some, Some)` arm of the pre-2.27
/// `print_app_detail`, which is what makes a single-workload bundle
/// byte-identical to today by construction rather than by copying: the
/// same function runs with the same arguments. The extracted arm never
/// referenced the Argo CD `Application` — every read keys off the inner
/// name and the namespace — so it is not a parameter here.
///
/// UNGUARDED, and this comment is the only record of it: **nothing
/// inside this body is covered by a test.** The golden test pins the
/// TEXT each section renders by composing the same pure line helpers
/// itself, so it is blind to what this function does with them. Three
/// mutations here were confirmed to leave the whole suite green —
/// swapping the Pods and Services sections, DELETING the Services
/// section outright, and transposing `inner_name`/`dest_ns` at a call
/// site. Ordering, omission and argument swaps are all invisible.
///
/// Closing it means threading the four read RESULTS into a pure
/// assembler, which moves every failure warning from stderr to stdout
/// and changes the interleaving those warnings were written for; that
/// was judged not worth the change. Two things bound the risk. Nothing
/// inside this body moved in the extraction, so this commit adds none of
/// it. And the argument-swap class is now structurally narrower than it
/// was: the extraction dropped the Argo CD `Application` from this
/// function's scope, so the historical bug it warns about below —
/// passing the registration where the AppRafter CR belonged, which reads
/// `spec.base.env` and silently finds nothing — is no longer
/// representable here.
fn print_workload_detail(inner_name: &str, dest_ns: &str, kubeconfig_path: &Path) {
    // 1. AppRafter Application phase (group-qualified
    //    apprafter.io read). Non-fatal — a missing CR /
    //    absent phase simply skips the line.
    // Hoisted: the AppRafter CR is read once and reused below for the
    // secret bindings (2.22c / D7) and the config-drift boundary
    // (D6). Both are properties of THIS CR — an earlier draft passed
    // the Argo CD Application to the bindings parser by mistake,
    // which reads `spec.base.env` and would simply have found
    // nothing, every time, silently.
    let mut apprafter_cr: Option<Value> = None;
    match kubectl_get_json(
        "application.apprafter.io",
        Some(inner_name),
        Some(dest_ns),
        kubeconfig_path,
    ) {
        Ok(Some(cr)) => {
            for line in apprafter_cr_advisory_lines(&cr, &chrono::Utc::now()) {
                if line.warn {
                    println!("{}", style::warn(&line.text));
                } else {
                    println!("{}", line.text);
                }
            }
            apprafter_cr = Some(cr);
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!(
                "⚠ Could not fetch AppRafter Application phase ({e}). \
                 Argo CD's view (above) is still authoritative."
            );
        }
    }

    let config_changed_at = apprafter_cr
        .as_ref()
        .and_then(|cr| cr.pointer("/status/envConfig/changedAt"))
        .and_then(Value::as_str);

    // 2. Pods (moved out of --resources). Non-fatal.
    match list_pods_for_apprafter_app(inner_name, dest_ns, kubeconfig_path) {
        Ok(pods) => print_pod_summaries(&pods, inner_name, dest_ns, config_changed_at),
        Err(e) => {
            eprintln!();
            eprintln!(
                "⚠ Could not fetch workload pod state ({e}). \
                 Argo CD's view (above) is still authoritative \
                 for sync/health from the apiserver perspective."
            );
        }
    }

    // 3. Services. Non-fatal.
    match list_services_for_apprafter_app(inner_name, dest_ns, kubeconfig_path) {
        Ok(services) => print_service_summaries(&services, inner_name, dest_ns),
        Err(e) => {
            eprintln!();
            eprintln!("⚠ Could not fetch workload service state ({e}).");
        }
    }

    // 4. Resource provisioning (ResourceClaims). Non-fatal.
    match list_resource_claims_for_app(inner_name, dest_ns, kubeconfig_path) {
        Ok(claims) => print_resource_claims(&claims, dest_ns),
        Err(e) => {
            eprintln!();
            eprintln!("⚠ Could not fetch resource-claim state ({e}).");
        }
    }

    // 5. Secrets this app resolves (2.22c / D7). The app -> secrets
    // half of the same index `secret seal` reads the other way for
    // its blast radius. Non-fatal: this is a read that adds context,
    // and failing `app status` over it would be the wrong trade.
    if let Some(cr) = apprafter_cr.as_ref() {
        print_secret_bindings_for_app(cr, inner_name, dest_ns);
    }
}

/// What `app status` renders for ONE registration, once the workloads it
/// deploys are known (ADR 0062).
///
/// The decision is separated from the rendering because it is the whole
/// of the bug: the pre-2.27 code took the first workload of however many
/// and scoped five cluster reads to it, and no test could observe that
/// choice because it was fused to the IO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusPlan {
    /// Render the full per-workload block for this one — the single
    /// workload of a single-workload bundle, or the one `--workload`
    /// named.
    Detail(CrRef),
    /// Render one row per workload, with a pointer at `--workload`.
    /// NEVER N full blocks: a bundle of six would bury the one that is
    /// broken under five that are not.
    Summary(Vec<CrRef>),
    /// `--workload` named something this registration does not deploy.
    /// Carries what it DOES deploy, because the likeliest cause is a
    /// typo and the second is the addressing rule not being known yet.
    UnknownWorkload {
        asked: String,
        available: Vec<String>,
    },
    /// The registration tracks no workload at all.
    ///
    /// Distinct from `Summary(vec![])` on purpose. `status.resources[]`
    /// is empty until Argo CD's first sync, and an empty summary table
    /// would tell an operator their registration deploys nothing when
    /// the truth is that nothing is known yet. This carries the
    /// pre-2.27 `(workload detail unavailable — app not synced yet)`
    /// line, unchanged.
    NoWorkloads,
}

/// Pure helper — did this plan ANSWER the caller's `--workload`?
///
/// Purely about addressing: did the name the caller typed name something
/// this registration deploys. It says nothing about whether the block
/// can then be drawn — `Detail` over a workload whose namespace is
/// UNKNOWN is a match that renders nothing, and reading "no" out of that
/// is what made `status` report `deploys no workload 'web'. It deploys:
/// web.` and exit non-zero on a state
/// `apprafter_app_refs_reports_an_unknown_namespace_as_none` documents
/// as real.
///
/// `Summary` counts as matched because a summary is only ever reached
/// with no selector at all, so there is no question outstanding.
pub(crate) fn plan_matched_selector(plan: &StatusPlan) -> bool {
    matches!(plan, StatusPlan::Detail(_) | StatusPlan::Summary(_))
}

/// Pure helper — decide what [`print_app_detail`] renders for one
/// registration. No IO, no clock: the whole addressing rule is
/// table-tested without a cluster.
///
/// INVARIANT: `--workload` is validated even at N=1. Accepting any name
/// when there is only one workload would let a typo silently render a
/// different application's block than the one the reader asked for —
/// and at N=1 the registration name and the workload name are usually
/// the same string, so the typo is easy to make and invisible to catch.
pub(crate) fn status_render_plan(refs: &[CrRef], workload: Option<&str>) -> StatusPlan {
    if refs.is_empty() {
        // Before ANY other rule: with nothing synced there is neither a
        // summary to render nor a set of names to correct `--workload`
        // against, and "there is no workload `api`" would be a narrower
        // and wronger claim than "nothing has synced yet".
        return StatusPlan::NoWorkloads;
    }
    match workload {
        Some(asked) => match refs.iter().find(|r| r.name == asked) {
            Some(r) => StatusPlan::Detail(r.clone()),
            None => StatusPlan::UnknownWorkload {
                asked: asked.to_string(),
                available: refs.iter().map(|r| r.name.clone()).collect(),
            },
        },
        None if refs.len() == 1 => StatusPlan::Detail(refs[0].clone()),
        None => StatusPlan::Summary(refs.to_vec()),
    }
}

/// What a verb does with a bundle of several workloads when the caller
/// named none (ADR 0062 §Write surfaces).
///
/// The three arms are the whole of this subphase. They are carried as
/// one enum, and every rule below is table-tested across all three,
/// because what has to stay true is not any single verb's behaviour but
/// the DIFFERENCE between them — a difference that would otherwise drift
/// one verb at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkloadDemand {
    /// `app rollback --to <digest>` and `app unpin` — a write against
    /// ONE workload's CR. Refuses ambiguity: picking the first is how a
    /// pin lands on a workload nobody named, and nothing downstream can
    /// tell that from a pin the operator meant.
    Write,
    /// `app open` — one workload's Service. Port-forwarding an arbitrary
    /// one is wrong too (it puts a different application on
    /// localhost:8080 than the reader named), just not destructive, so
    /// this asks instead of refusing.
    ReadOne,
    /// `app logs` — every workload at once.
    ///
    /// Defensible for this verb ALONE. `kubectl logs -l` is already a
    /// multiplexer over pods; before 2.27 it just happened to be scoped
    /// to one workload's pod set by the first-wins shim. A bundle is
    /// deployed, synced and removed together, so its workloads' lines
    /// interleave into one story — the API 500 and the worker exception
    /// that caused it. Showing more than asked costs a reader nothing
    /// they cannot filter; the other three verbs each ACT on their
    /// resolution, so for them the same generosity is a wrong write or a
    /// wrong forward.
    ReadEvery,
}

/// A workload that can be handed to `kubectl -n`.
///
/// `namespace` is a plain `String`, not [`CrRef`]'s `Option<String>`,
/// and that is the point: the only constructor is [`workload_for`],
/// which answers [`WorkloadChoice::Unplaceable`] rather than defaulting
/// an unknown namespace. `kubectl -n ""` does not error — it falls
/// through to the kubeconfig's default namespace — so a defaulted write
/// would land, successfully and silently, on whatever object shares the
/// name there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlacedWorkload {
    pub name: String,
    pub namespace: String,
}

/// Which workload(s) of a bundle a verb acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkloadChoice {
    /// Act on exactly this one — the sole workload of a bundle, or the
    /// one `--workload` named.
    One(PlacedWorkload),
    /// Act on all of these at once. Only [`WorkloadDemand::ReadEvery`]
    /// produces it, and it always carries 2+: at N = 1 the answer is
    /// [`WorkloadChoice::One`], so a caller cannot accidentally render
    /// the multi-workload shape for today's fleet.
    Every(Vec<PlacedWorkload>),
    /// N > 1, no `--workload`, and the verb WRITES. Carries every
    /// candidate, because a refusal that does not name them leaves the
    /// reader with no next command — see [`ambiguous_write_lines`].
    Refuse(Vec<String>),
    /// N > 1, no `--workload`, and the verb reads ONE. Carries every
    /// candidate for the same reason; the caller prompts with them in a
    /// TTY and prints them otherwise.
    Ask(Vec<String>),
    /// `--workload` named something this bundle does not deploy. The
    /// likeliest cause is a typo and the candidate list is what corrects
    /// it — rendered by [`unknown_workload_message`], shared verbatim
    /// with `app status`.
    Unknown {
        asked: String,
        available: Vec<String>,
    },
    /// The registration tracks no workload at all — `status.resources[]`
    /// is empty until Argo CD's first sync. NOT an error by itself:
    /// `logs` falls back to the raw-YAML selector and `rollback` falls
    /// through to the Git-revision branch, both exactly as before 2.27.
    NoWorkloads,
    /// The chosen workload cannot be placed in a namespace. A refusal
    /// for every verb, including the multiplexing read: a stream missing
    /// one workload reads exactly like a workload that logged nothing.
    Unplaceable(String),
}

/// Pure — the addressing decision `app logs`, `app open`, `app rollback`
/// and `app unpin` share. No IO, no clock.
///
/// ADR 0062 §Addressing: the positional argument is ALWAYS the
/// registration, and `selector` — `--workload <name>` — is the only way
/// to address one workload inside it, a disambiguator in the same sense
/// as `--env`. `refs` is what that registration deploys, in Argo CD's
/// own order.
///
/// Rule order, and why:
///
/// 1. **Nothing synced beats everything else.** With no workloads there
///    is neither a target to resolve nor a set of names to correct
///    `--workload` against, and "there is no workload `api`" would be a
///    narrower and wronger claim than "nothing has synced yet". Same
///    first rule as [`status_render_plan`].
/// 2. **An explicit selector is checked even at N = 1.** Accepting any
///    name when there is only one workload would let a typo act on a
///    different object than the reader asked for — and at N = 1 the
///    application name and the workload name are usually the same
///    string, so the typo is both easy to make and invisible.
/// 3. **N = 1 resolves outright, for every demand.** That is today's
///    entire fleet, and it must not grow a prompt, a refusal or a
///    multiplexed selector.
/// 4. **Only then does the demand matter** — and it is the only place it
///    matters, which is what keeps the read/write asymmetry to one
///    `match` instead of scattering it across four commands.
pub(crate) fn workload_for(
    refs: &[CrRef],
    selector: Option<&str>,
    demand: WorkloadDemand,
) -> WorkloadChoice {
    if refs.is_empty() {
        return WorkloadChoice::NoWorkloads;
    }
    if let Some(asked) = selector {
        return match refs.iter().find(|r| r.name == asked) {
            Some(r) => place(r),
            None => WorkloadChoice::Unknown {
                asked: asked.to_string(),
                available: refs.iter().map(|r| r.name.clone()).collect(),
            },
        };
    }
    if refs.len() == 1 {
        return place(&refs[0]);
    }
    let names: Vec<String> = refs.iter().map(|r| r.name.clone()).collect();
    match demand {
        WorkloadDemand::Write => WorkloadChoice::Refuse(names),
        WorkloadDemand::ReadOne => WorkloadChoice::Ask(names),
        WorkloadDemand::ReadEvery => {
            let mut placed = Vec::with_capacity(refs.len());
            for r in refs {
                match r.namespace.as_deref() {
                    Some(ns) => placed.push(PlacedWorkload {
                        name: r.name.clone(),
                        namespace: ns.to_string(),
                    }),
                    None => return WorkloadChoice::Unplaceable(r.name.clone()),
                }
            }
            WorkloadChoice::Every(placed)
        }
    }
}

/// The one [`CrRef`] → [`PlacedWorkload`] conversion, so the refusal on
/// an unknown namespace exists in exactly one place.
fn place(r: &CrRef) -> WorkloadChoice {
    match r.namespace.as_deref() {
        Some(ns) => WorkloadChoice::One(PlacedWorkload {
            name: r.name.clone(),
            namespace: ns.to_string(),
        }),
        None => WorkloadChoice::Unplaceable(r.name.clone()),
    }
}

/// Pure — the `--env` the caller typed, echoed back into a quoted
/// command, or the empty string.
///
/// Every command this module prints for a reader to run is re-entered
/// through `resolve_app_for_command`, which on a registration with two
/// or more environments needs the flag to pick one — without it the
/// retry errors on `per_env_guidance_message`. Echoing what was typed
/// (rather than inferring one) is what makes the quoted command
/// re-resolve to the SAME deployment the caller was just looking at.
pub(crate) fn env_echo(env: Option<&str>) -> String {
    env.map(|e| format!(" --env {e}")).unwrap_or_default()
}

/// Pure — what a WRITE verb says when it will not choose for the caller
/// (ADR 0062 §Write surfaces).
///
/// `suffix` is the flags the caller already typed, appended verbatim to
/// each quoted command — without it the retry silently loses the `--to`
/// the operator was in the middle of, which turns a refusal into a
/// second mistake.
///
/// One line per candidate rather than a comma list, because the point is
/// that the next command is COPY-PASTEABLE: a reader under rollback
/// pressure should not have to assemble it. The positional stays the
/// application in every one of them — quoting `apprafter app rollback
/// api` would teach the exact collapse ADR 0062 §Addressing exists to
/// prevent.
pub(crate) fn ambiguous_write_lines(
    verb: &str,
    application: &str,
    candidates: &[String],
    suffix: &str,
) -> Vec<String> {
    let mut out = vec![
        format!(
            "Application '{application}' deploys {} workloads, and `app {verb}` writes to one \
             of them.",
            candidates.len()
        ),
        "Name it with `--workload` — this command will not choose for you:".to_string(),
    ];
    out.extend(
        candidates
            .iter()
            .map(|w| format!("  apprafter app {verb} {application} --workload {w}{suffix}")),
    );
    out
}

/// Pure — `rollback`'s ambiguity refusal, which is
/// [`ambiguous_write_lines`] plus the branch that is NOT ambiguous.
///
/// A bare refusal would leave the reader believing `rollback` is
/// unavailable on a bundle, and it is not: the Git-revision branch moves
/// every workload at once and therefore takes no `--workload` and
/// refuses nothing. Printing both routes is the same courtesy
/// [`refuse_workload_lines`] already pays, and here it doubles as the
/// place a reader learns the two branches differ in cardinality at all.
pub(crate) fn rollback_refusal_lines(
    application: &str,
    candidates: &[String],
    suffix: &str,
) -> Vec<String> {
    let mut out = ambiguous_write_lines("rollback", application, candidates, suffix);
    out.push(String::new());
    out.push(format!(
        "To roll ALL {} workloads back to a Git revision instead — one revision for the whole \
         application, no `--workload`:",
        candidates.len()
    ));
    out.push(format!(
        "  apprafter app rollback {application} --to <revision>"
    ));
    out
}

/// Pure — what `app open` says when it cannot ask (no TTY) and will not
/// pick.
///
/// A read, so the wording is a question rather than a refusal — but the
/// candidate list and the copy-pasteable command are the same courtesy a
/// write owes, for the same reason: forwarding an arbitrary Service puts
/// a different application on localhost than the reader named, and they
/// would have no way to tell.
pub(crate) fn ambiguous_read_message(
    application: &str,
    candidates: &[String],
    env: Option<&str>,
) -> String {
    format!(
        "Application '{application}' deploys {} workloads ({}), and `app open` forwards one \
         of them. Re-run in a TTY to choose, or pass `apprafter app open {application} \
         --workload <name>{}`.",
        candidates.len(),
        candidates.join(", "),
        env_echo(env)
    )
}

/// Pure — what every verb says about a workload it cannot place.
///
/// Deliberately NOT "not synced yet", which is what the pre-2.27 code
/// degraded this state to: the registration HAS synced (it tracks the
/// workload), we simply cannot say where the workload lives, and telling
/// an operator to wait for a sync that already finished sends them to
/// watch the wrong thing.
pub(crate) fn unplaceable_workload_message(application: &str, workload: &str) -> String {
    format!(
        "Workload '{workload}' of application '{application}' names no namespace — neither \
         its own `status.resources[]` entry nor the application's \
         `spec.destination.namespace` places it, so there is nothing safe to point kubectl \
         at. Inspect with `apprafter app status {application}`."
    )
}

/// One row of the bundle summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkloadSummary {
    pub name: String,
    /// The AppRafter CR's `status.phase`, or `—` when it could not be
    /// read. Never blank — a blank cell reads as healthy.
    pub phase: String,
    /// `<ready>/<total>` pods, or `—` when the pod read failed.
    pub pods: String,
    /// See [`workload_image_cell`].
    pub image: String,
}

/// Build one [`WorkloadSummary`] row.
pub(crate) fn workload_summary(
    name: &str,
    phase: &str,
    pods: &str,
    image: &str,
) -> WorkloadSummary {
    WorkloadSummary {
        name: name.to_string(),
        phase: phase.to_string(),
        pods: pods.to_string(),
        image: image.to_string(),
    }
}

/// The one phase that counts as fine. Anything else — including the
/// em-dash of a read that failed — is in the roll-up.
const READY_PHASE: &str = "Ready";

/// Pure helper — is this row known to be serving?
///
/// TWO conditions, and the pod one is not redundant with the phase. The
/// operator writes `status.phase = Ready` the moment it APPLIES the
/// Deployment (`operator-controllers/application/src/lib.rs:1072`), so a
/// workload in `CrashLoopBackOff` carries `phase: Ready` — the exact
/// shape this subphase exists for would otherwise leave the roll-up
/// silent and point the hint at the healthy sibling. The count is
/// already on the row, read by this same command, so folding it in costs
/// nothing.
///
/// ADR 0062 §Accepted does NOT license skipping this. It scopes the
/// no-pod-visibility concession to `app list`'s HEALTH cell, which never
/// reads pods. This summary has.
///
/// A `<ready>/<total>` that does not parse is the em-dash of a failed
/// read and counts as NOT ready, the same rule the phase follows: this
/// table never lets something unobserved render as fine. `0/0` does not
/// — `ready == total` there, and a deliberately scaled-to-zero workload
/// must not nag forever.
fn workload_is_ready(row: &WorkloadSummary) -> bool {
    if row.phase != READY_PHASE {
        return false;
    }
    match row.pods.split_once('/') {
        Some((ready, total)) => match (ready.parse::<usize>(), total.parse::<usize>()) {
            (Ok(ready), Ok(total)) => ready >= total,
            _ => false,
        },
        None => false,
    }
}

/// Pure helper — the bundle summary `app status` prints for a
/// registration that deploys more than one workload (ADR 0062).
///
/// `application` is the LOGICAL name, so the hint quotes a command that
/// works: the positional argument is never a workload name.
///
/// INVARIANT: the roll-up line appears ONLY when something is not Ready,
/// so its presence is the signal. The table above it already proves the
/// command ran and saw every workload, which makes an "all fine" line
/// pure noise — the inverse of `apprafter status`, whose silence would
/// be ambiguous. Same reasoning as [`env_deployment_index_lines`].
///
/// "Not Ready" is [`workload_is_ready`], which reads the PODS cell as
/// well as the phase — a `phase: Ready` workload whose pods are `0/1` is
/// in the roll-up, and is what the hint points at. A workload whose
/// phase or pod count did not read (`—`) counts as NOT Ready too: the
/// failed-read warning sits beside this table, and excluding an
/// unobserved workload from the count would recreate, one level down,
/// the exact defect this subphase exists to fix — something unseen
/// rendering as a clean bill of health.
pub(crate) fn workload_summary_lines(
    application: &str,
    namespace: &str,
    rows: &[WorkloadSummary],
) -> Vec<String> {
    // `str`'s Display padding counts CHARS, so the widths must too —
    // otherwise every em-dash cell gains two spaces and the columns
    // stagger, which is a normal state here rather than an exotic one.
    let width = |f: fn(&WorkloadSummary) -> &String, header: usize| {
        rows.iter()
            .map(|r| f(r).chars().count())
            .max()
            .unwrap_or(header)
            .max(header)
    };
    let name_w = width(|r| &r.name, "NAME".len());
    let phase_w = width(|r| &r.phase, "PHASE".len());
    let pods_w = width(|r| &r.pods, "PODS".len());

    let mut out = vec![
        String::new(),
        format!("Workloads ({}, namespace {namespace}):", rows.len()),
        format!(
            "  {:<name_w$}  {:<phase_w$}  {:<pods_w$}  IMAGE",
            "NAME",
            "PHASE",
            "PODS",
            name_w = name_w,
            phase_w = phase_w,
            pods_w = pods_w,
        ),
    ];
    for r in rows {
        out.push(format!(
            "  {:<name_w$}  {:<phase_w$}  {:<pods_w$}  {}",
            r.name,
            r.phase,
            r.pods,
            r.image,
            name_w = name_w,
            phase_w = phase_w,
            pods_w = pods_w,
        ));
    }

    let not_ready: Vec<&WorkloadSummary> = rows.iter().filter(|r| !workload_is_ready(r)).collect();
    out.push(String::new());
    if !not_ready.is_empty() {
        out.push(format!(
            "  {} of {} workloads {} not Ready.",
            not_ready.len(),
            rows.len(),
            if not_ready.len() == 1 { "is" } else { "are" },
        ));
    }
    // Point at a workload worth looking at — the first not-Ready one, so
    // the hint is a next step rather than an example.
    if let Some(pick) = not_ready.first().copied().or_else(|| rows.first()) {
        out.push(format!(
            "  Full detail for one: apprafter app status {application} --workload {}",
            pick.name
        ));
    }
    out
}

/// Pure helper — the ONE namespace a bundle lives in (ADR 0062: one
/// package = one registration = one namespace).
///
/// `None` when the refs disagree or any of them is UNKNOWN. Neither is
/// a thing to average or to drop: the summary's two reads are
/// namespace-scoped, and guessing would read someone else's namespace
/// or silently lose a workload.
pub(crate) fn bundle_namespace(refs: &[CrRef]) -> Option<String> {
    let mut it = refs.iter();
    let first = it.next()?.namespace.clone()?;
    it.all(|r| r.namespace.as_deref() == Some(first.as_str()))
        .then_some(first)
}

/// The label selector that matches every operator-rendered pod in a
/// namespace, whatever workload it belongs to.
///
/// `operator-rendering::make_labels` stamps `apprafter: "true"` and
/// `app.kubernetes.io/name: <cr>` on the SAME label map, and that map
/// goes on the Deployment's pod template — so one read covers the whole
/// bundle and [`bucket_pod_readiness`] splits it by workload. That is
/// what keeps the summary at two reads for N workloads rather than the
/// four-per-workload of the detail path.
pub(crate) const BUNDLE_POD_SELECTOR: &str = "apprafter=true";

/// Pure helper — split one namespace-wide pod list into
/// `workload -> (ready, total)`.
///
/// A pod with no `app.kubernetes.io/name` label belongs to no workload
/// this command speaks for and is dropped rather than bucketed under an
/// empty key.
pub(crate) fn bucket_pod_readiness(pods: &[Value]) -> BTreeMap<String, (usize, usize)> {
    let mut out: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for pod in pods {
        let Some(name) = pod
            .pointer("/metadata/labels/app.kubernetes.io~1name")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let ready = pod
            .pointer("/status/conditions")
            .and_then(Value::as_array)
            .is_some_and(|cs| {
                cs.iter().any(|c| {
                    c.get("type").and_then(Value::as_str) == Some("Ready")
                        && c.get("status").and_then(Value::as_str) == Some("True")
                })
            });
        let bucket = out.entry(name.to_string()).or_insert((0, 0));
        bucket.1 += 1;
        if ready {
            bucket.0 += 1;
        }
    }
    out
}

/// Pure helper — the summary's `IMAGE` cell for one workload CR.
///
/// `status.image.tag` first: that is what the operator actually
/// deployed, with the per-environment merge already applied, and it is
/// the same field the detail block's `image:` line reads. It is absent
/// under `imagePolicy.resolve: off`, and then the DECLARED image is the
/// honest answer — the effective one, so the env override wins over
/// `base` exactly as the operator resolves it.
///
/// Unknown renders the em-dash, never a blank.
pub(crate) fn workload_image_cell(cr: &Value) -> String {
    if let Some(tag) = cr.pointer("/status/image/tag").and_then(Value::as_str) {
        return tag.to_string();
    }
    cr.pointer("/spec/environment")
        .and_then(Value::as_str)
        .and_then(|env| {
            cr.pointer("/spec/environments")
                .and_then(|envs| envs.get(env))
                .and_then(|e| e.get("image"))
                .and_then(Value::as_str)
        })
        .or_else(|| cr.pointer("/spec/base/image").and_then(Value::as_str))
        .unwrap_or("—")
        .to_string()
}

/// Print the bundle summary: two namespace-scoped reads for the whole
/// registration, against four per workload on the detail path.
///
/// Either read may fail, and then the affected columns render `—` and a
/// warning names the command that failed. Never a blank: a blank cell
/// reads as healthy, which is the failure mode this subphase exists to
/// close.
fn print_workload_summary(application: &str, refs: &[CrRef], kubeconfig_path: &Path) {
    let Some(namespace) = bundle_namespace(refs) else {
        let rows: Vec<WorkloadSummary> = refs
            .iter()
            .map(|r| workload_summary(&r.name, "—", "—", "—"))
            .collect();
        for line in workload_summary_lines(application, "—", &rows) {
            println!("{line}");
        }
        eprintln!(
            "⚠ The workloads of this application do not report one shared namespace, \
             so their state could not be read. Run `apprafter app status {application} \
             --workload <name>` for one of them."
        );
        return;
    };

    // Read 1 — every AppRafter CR in the namespace: phase + image.
    let crs: Option<BTreeMap<String, Value>> = match kubectl_get_json(
        "application.apprafter.io",
        None,
        Some(&namespace),
        kubeconfig_path,
    ) {
        Ok(list) => Some(
            list.as_ref()
                .and_then(|l| l.get("items"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|cr| {
                            Some((
                                cr.pointer("/metadata/name")
                                    .and_then(Value::as_str)?
                                    .to_string(),
                                cr.clone(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        Err(e) => {
            eprintln!(
                "⚠ Could not fetch workload phases ({e}) — PHASE and IMAGE show —. \
                 Failed: kubectl get application.apprafter.io -n {namespace}"
            );
            None
        }
    };

    // Read 2 — every operator-rendered pod in the namespace, bucketed.
    let pods: Option<BTreeMap<String, (usize, usize)>> = match kubectl_get_json_by_selector(
        "pods",
        BUNDLE_POD_SELECTOR,
        Some(&namespace),
        kubeconfig_path,
    ) {
        Ok(items) => Some(bucket_pod_readiness(&items)),
        Err(e) => {
            eprintln!(
                "⚠ Could not fetch workload pod state ({e}) — PODS shows —. \
                 Failed: kubectl get pods -n {namespace} -l {BUNDLE_POD_SELECTOR}"
            );
            None
        }
    };

    let rows: Vec<WorkloadSummary> = refs
        .iter()
        .map(|r| {
            let cr = crs.as_ref().and_then(|m| m.get(&r.name));
            let phase = match &crs {
                // The CR list read, but THIS workload is not in it: Argo CD
                // tracks a resource the apiserver does not have. That is a
                // real state (mid-prune, or a failed apply) and `—` is what
                // this table says about anything it could not observe.
                Some(_) => cr
                    .and_then(|c| c.pointer("/status/phase"))
                    .and_then(Value::as_str)
                    .unwrap_or("—")
                    .to_string(),
                None => "—".to_string(),
            };
            let image = cr
                .map(workload_image_cell)
                .unwrap_or_else(|| "—".to_string());
            let pod_cell = match &pods {
                Some(buckets) => {
                    let (ready, total) = buckets.get(&r.name).copied().unwrap_or((0, 0));
                    format!("{ready}/{total}")
                }
                None => "—".to_string(),
            };
            workload_summary(&r.name, &phase, &pod_cell, &image)
        })
        .collect();

    for line in workload_summary_lines(application, &namespace, &rows) {
        println!("{line}");
    }
}

/// One per-environment deployment row, summarised from an Argo CD
/// `Application` JSON. Pure — the formatter / aggregation loop drives
/// it without a cluster. `environment` resolves from the
/// `apprafter.io/environment` label first, then `status.environment`,
/// then `(base)` for a pre-2.9 / base-only deploy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeploymentSummary {
    /// Argo CD `metadata.name` — `<logical>-<env>` for an env deploy,
    /// or `<logical>` for a base-only one.
    pub argo_name: String,
    /// Resolved environment label (or `(base)` when none).
    pub environment: String,
    pub destination_namespace: String,
    pub sync: String,
    pub health: String,
}

/// Pure helper — the AppRafter CR's advisory block inside `app status`.
/// Extracted from [`print_app_detail`], with the clock injected so the
/// age-bearing lines are deterministic.
///
/// INVARIANT: the ORDER is the message. The phase names the failure, the
/// not-ready reason explains it directly underneath, the image and pin
/// lines qualify each other (a pin is invisible in Git, so the pin line
/// must sit against the digest it holds), and undesigned reconcile
/// problems precede the recommendation because a failing reconcile
/// changes what a sizing advisory means. Every line is independently
/// omitted when its field is absent, so a healthy app renders none.
pub(crate) fn apprafter_cr_advisory_lines(
    cr: &Value,
    now: &chrono::DateTime<chrono::Utc>,
) -> Vec<RenderedLine> {
    let mut out = Vec::new();
    if let Some(phase) = cr.pointer("/status/phase").and_then(Value::as_str) {
        out.push(RenderedLine::plain(format!("AppRafter phase: {phase}")));
    }
    if let Some(why) = format_not_ready_line(cr) {
        out.push(RenderedLine::warned(why));
    }
    if let Some(image_line) = format_image_line(cr, now) {
        out.push(RenderedLine::plain(image_line));
    }
    if let Some(pin_line) = format_pin_line(cr) {
        out.push(RenderedLine::warned(pin_line));
    }
    for line in format_problem_lines(cr, now) {
        out.push(RenderedLine::warned(line));
    }
    if let Some(reco_line) = format_recommendation_line(cr) {
        out.push(RenderedLine::plain(reco_line));
    }
    out
}

/// Pure helper — resolve an Argo CD Application's environment from
/// `metadata.labels."apprafter.io/environment"`, falling back to
/// `status.environment`, then `(base)`.
pub(crate) fn deployment_environment(app: &Value) -> String {
    app.pointer("/metadata/labels/apprafter.io~1environment")
        .and_then(Value::as_str)
        .or_else(|| app.pointer("/status/environment").and_then(Value::as_str))
        .unwrap_or("(base)")
        .to_string()
}

/// Sort key for deterministic per-env section ordering: environment
/// name then Argo `metadata.name`.
fn deployment_sort_key(app: &Value) -> (String, String) {
    let env = deployment_environment(app);
    let name = app
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    (env, name)
}

/// Pure helper — summarise a slice of Argo CD `Application` JSON values
/// into per-environment rows, sorted by (environment, argo name) for a
/// stable render. Tests drive it from fake Argo-app Values.
pub(crate) fn summarize_deployments(apps: &[Value]) -> Vec<DeploymentSummary> {
    let mut rows: Vec<DeploymentSummary> = apps
        .iter()
        .map(|app| DeploymentSummary {
            argo_name: app
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            environment: deployment_environment(app),
            destination_namespace: app
                .pointer("/spec/destination/namespace")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            sync: app
                .pointer("/status/sync/status")
                .and_then(Value::as_str)
                .unwrap_or("Unknown")
                .to_string(),
            health: app
                .pointer("/status/health/status")
                .and_then(Value::as_str)
                .unwrap_or("Unknown")
                .to_string(),
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.environment.as_str(), a.argo_name.as_str())
            .cmp(&(b.environment.as_str(), b.argo_name.as_str()))
    });
    rows
}

/// Pod state snapshot rendered into the `--resources` table.
/// Public(crate) so tests can drive `print_pod_summaries`
/// without spawning kubectl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PodSummary {
    pub name: String,
    /// `<ready>/<total>` — matches `kubectl get pods` shape.
    pub ready: String,
    /// Container `waiting.reason` if any (e.g.
    /// `ImagePullBackOff`, `CrashLoopBackOff`, `ContainerCreating`);
    /// otherwise pod phase (Pending / Running / Succeeded /
    /// Failed). Matches kubectl's STATUS column heuristic.
    pub status: String,
    pub restarts: i64,
    /// Human-readable age computed at print time. Format
    /// mirrors `kubectl get pods` (`s`, `m`, `h`, `d` units).
    pub age: String,
    /// RFC3339 `status.startTime` — when the kubelet started this pod, which
    /// is the moment it resolved its env from Secrets and never re-read them.
    /// Compared against `status.envConfig.changedAt` to decide whether the
    /// pod is running an older configuration (2.22c / D6). Falls back to
    /// `metadata.creationTimestamp` when the pod has not started yet.
    pub started_at: Option<String>,
}

/// Service surface rendered into the default `app status`
/// services table. Mirrors `PodSummary` — public(crate) so
/// tests can drive `print_service_summaries` without spawning
/// kubectl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceSummary {
    pub name: String,
    /// `spec.type` (e.g. `ClusterIP`, `LoadBalancer`); default
    /// `ClusterIP` when omitted, matching the k8s API default.
    pub type_: String,
    /// `spec.clusterIP`; `-` when omitted (e.g. headless or
    /// not-yet-assigned).
    pub cluster_ip: String,
    /// Compact `<port>/<protocol>` join over `spec.ports[]`
    /// (e.g. `3000/TCP`, comma-separated for multi-port).
    pub ports: String,
}

/// ResourceClaim provisioning snapshot rendered into the
/// default `app status` claims table. public(crate) so tests
/// drive `print_resource_claims` without a cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceClaimSummary {
    pub name: String,
    /// `status.provider`; `—` when the provisioner has not yet
    /// bound a provider.
    pub provider: String,
    /// `status.ready`; false when absent (fresh claim).
    pub ready: bool,
    /// `status.connectionSecretRef` — the Secret holding the
    /// connection material. None until the claim is fulfilled.
    pub secret_ref: Option<String>,
    /// The concrete backend resource serving this claim, when the
    /// provisioner has named one: the pooled instance and logical DB for
    /// redis, the standalone PVC for a disk, the observed streams for
    /// jetstream (2.5 / ADR 0061), `—` otherwise. Answers "what actually got
    /// created", which the ready/scheduled pair does not.
    ///
    /// `—` is still the honest answer for a `pg` claim: CNPG writes neither
    /// an instance nor a volumeClaimRef (`reconcile.rs`: "CNPG owns no
    /// instance/dbnum"), so there is nothing on the claim to name. Filling
    /// that in needs a provisioner-side status write, not a CLI change.
    pub backing: String,
    /// How much data the claim holds, per-backend (2.22d / D8): used/total
    /// for a disk, bytes for pg, keys for redis, `—` when unmeasured.
    pub size: String,
    /// Whether `status.conditions[]` carries
    /// `{type: "Scheduled", status: "True"}` — the provisioner
    /// has placed the claim on a backing cluster.
    pub scheduled: bool,
}

/// Argo CD resource entry rendered into the tracked-
/// resources table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackedResource {
    pub name: String,
    pub kind: String,
    pub namespace: String,
    pub status: String,
    pub health: String,
}

/// Shell out to `kubectl get pods` filtered to the AppRafter
/// operator's label. Mirrors `app_open::list_services_for_
/// apprafter_app` shape.
fn list_pods_for_apprafter_app(
    apprafter_app_name: &str,
    namespace: &str,
    kubeconfig: &Path,
) -> Result<Vec<PodSummary>> {
    let out = Command::new("kubectl")
        .args(kubectl_list_args(
            "pods",
            namespace,
            Some(&operator_workload_selector(apprafter_app_name)),
        ))
        .env("KUBECONFIG", kubeconfig)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl get pods: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl get pods failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
    Ok(parse_pod_summaries(&parsed, &chrono::Utc::now()))
}

/// Pure helper — parse `kubectl get pods -o json` items into
/// `PodSummary` rows. `now` injected so tests can pin a
/// deterministic clock when computing ages.
pub(crate) fn parse_pod_summaries(
    payload: &Value,
    now: &chrono::DateTime<chrono::Utc>,
) -> Vec<PodSummary> {
    let items = payload.get("items").and_then(Value::as_array);
    let Some(items) = items else { return vec![] };
    items.iter().map(|p| summarise_pod(p, now)).collect()
}

fn summarise_pod(pod: &Value, now: &chrono::DateTime<chrono::Utc>) -> PodSummary {
    let name = pod
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();

    let mut ready_count = 0usize;
    let mut total_count = 0usize;
    let mut restarts = 0i64;
    let mut waiting_reason: Option<String> = None;

    if let Some(arr) = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
    {
        for cs in arr {
            total_count += 1;
            if cs.get("ready").and_then(Value::as_bool).unwrap_or(false) {
                ready_count += 1;
            }
            restarts += cs.get("restartCount").and_then(Value::as_i64).unwrap_or(0);
            if waiting_reason.is_none() {
                if let Some(reason) = cs.pointer("/state/waiting/reason").and_then(Value::as_str) {
                    waiting_reason = Some(reason.to_string());
                }
            }
        }
    }

    if total_count == 0 {
        // Pod has not been admitted yet, or has init containers
        // only. Use spec.containers length as the denominator
        // so the column reads sensibly ("0/N" — none yet).
        total_count = pod
            .pointer("/spec/containers")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(1);
    }

    let phase = pod
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    let status = waiting_reason.unwrap_or(phase);

    let age = pod
        .pointer("/metadata/creationTimestamp")
        .and_then(Value::as_str)
        .map(|ts| format_pod_age(ts, now))
        .unwrap_or_else(|| "?".to_string());

    let started_at = pod
        .pointer("/status/startTime")
        .or_else(|| pod.pointer("/metadata/creationTimestamp"))
        .and_then(Value::as_str)
        .map(str::to_string);

    PodSummary {
        name,
        ready: format!("{ready_count}/{total_count}"),
        status,
        restarts,
        age,
        started_at,
    }
}

/// Whether a pod started BEFORE the application's resolved configuration
/// last changed — i.e. it is running an older set of secret values
/// (2.22c / D6).
///
/// This is the whole drift mechanism, and it needs no marker on the pod.
/// An environment variable sourced from a Secret is resolved once when the
/// kubelet starts the container and is never re-read, so "started before the
/// config changed" is exactly "running the previous configuration".
///
/// Deliberately conservative: an unparseable or absent timestamp on either
/// side reports NOT stale. A false "your pods are stale" teaches the reader
/// to ignore the column, which costs more than the occasional miss.
///
/// # The two sides do not have the same resolution
///
/// COMPARED AT WHOLE SECONDS, ON PURPOSE. `pod.status.startTime` is a
/// Kubernetes `metav1.Time`, which serialises RFC3339 TRUNCATED TO SECONDS —
/// `2026-09-03T01:18:29Z`. The operator's `changedAt` is
/// `Utc::now().to_rfc3339()`, which carries nanoseconds —
/// `2026-09-03T01:18:29.943066898+00:00`. A plain `<` between them therefore
/// says "the pod is older" for every pod that started in the SAME SECOND as
/// the change, because the pod's fractional part was thrown away by the
/// apiserver and reads as `.000000000`.
///
/// That is not a corner case. On a first deployment the operator applies the
/// Deployment and stamps the digest in the same reconcile, so the pod is
/// created in that same second — and every freshly deployed application
/// displayed `← old config` and the "still serving the previous values" note
/// immediately, having changed nothing. Measured: startTime
/// `01:18:29Z` against changedAt `01:18:29.943066898+00:00`, on a pod seven
/// seconds old.
///
/// Truncating both sides to the resolution the apiserver actually provides
/// costs at most one second of sensitivity — a change and a pod start inside
/// the same second read as not-stale — and that is the direction this function
/// already commits to above.
///
/// Found by the negative control in `e2e/env-and-secrets-walk.sh`, which
/// exists only because two independent reviewers refused to accept a
/// positive-only assertion for this flag.
pub(crate) fn pod_is_stale(started_at: Option<&str>, changed_at: Option<&str>) -> bool {
    let (Some(started), Some(changed)) = (started_at, changed_at) else {
        return false;
    };
    let (Ok(started), Ok(changed)) = (
        chrono::DateTime::parse_from_rfc3339(started),
        chrono::DateTime::parse_from_rfc3339(changed),
    ) else {
        return false;
    };
    // `timestamp()` is whole seconds since the epoch — the truncation the
    // apiserver has already applied to one side, applied to both.
    started.timestamp() < changed.timestamp()
}

/// Pure helper — format a duration from `since` to `now` using
/// kubectl-style shorthand: `s` under a minute, `m` under an
/// hour, `<h>h<m>m` under a day, `<d>d<h>h` beyond. Returns
/// the original timestamp string when parsing fails (defensive
/// against Argo CD shape drift).
pub(crate) fn format_pod_age(timestamp: &str, now: &chrono::DateTime<chrono::Utc>) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return timestamp.to_string();
    };
    let secs = now
        .signed_duration_since(t.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h{m}m")
        }
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        if h == 0 {
            format!("{d}d")
        } else {
            format!("{d}d{h}h")
        }
    }
}

/// Pure helper — extract `status.resources[]` entries from the
/// Argo CD Application CR JSON.
pub(crate) fn extract_tracked_resources(app: &Value) -> Vec<TrackedResource> {
    let arr = app.pointer("/status/resources").and_then(Value::as_array);
    let Some(arr) = arr else { return vec![] };
    arr.iter()
        .filter_map(|r| {
            let name = r.get("name").and_then(Value::as_str)?.to_string();
            let kind = r
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let namespace = r
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string();
            let status = r
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let health = r
                .pointer("/health/status")
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string();
            Some(TrackedResource {
                name,
                kind,
                namespace,
                status,
                health,
            })
        })
        .collect()
}

/// Pure helper — the `--resources` table, assembled into lines.
/// Extracted from [`print_argocd_resources`], which is now a `println!`
/// loop over this: the column-width arithmetic is the part worth
/// pinning, and it cannot be pinned through stdout.
pub(crate) fn render_tracked_resource_lines(resources: &[TrackedResource]) -> Vec<String> {
    let mut out = vec![String::new(), "Argo CD tracked resources:".to_string()];
    if resources.is_empty() {
        out.push("  (none — Application has not yet reported `status.resources[]`)".to_string());
        return out;
    }
    let name_w = resources
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let kind_w = resources
        .iter()
        .map(|r| r.kind.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let ns_w = resources
        .iter()
        .map(|r| r.namespace.len())
        .max()
        .unwrap_or(9)
        .max(9);
    let status_w = resources
        .iter()
        .map(|r| r.status.len())
        .max()
        .unwrap_or(6)
        .max(6);
    out.push(format!(
        "  {:<name_w$}  {:<kind_w$}  {:<ns_w$}  {:<status_w$}  HEALTH",
        "NAME",
        "KIND",
        "NAMESPACE",
        "STATUS",
        name_w = name_w,
        kind_w = kind_w,
        ns_w = ns_w,
        status_w = status_w,
    ));
    for r in resources {
        out.push(format!(
            "  {:<name_w$}  {:<kind_w$}  {:<ns_w$}  {:<status_w$}  {}",
            r.name,
            r.kind,
            r.namespace,
            r.status,
            r.health,
            name_w = name_w,
            kind_w = kind_w,
            ns_w = ns_w,
            status_w = status_w,
        ));
    }
    out
}

fn print_argocd_resources(app: &Value) {
    for line in render_tracked_resource_lines(&extract_tracked_resources(app)) {
        println!("{line}");
    }
}

/// One rendered output line plus whether the caller paints it as a
/// warning. Lets the table renderers stay pure: `style::warn` inspects the
/// terminal, the line assembly must not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderedLine {
    pub text: String,
    pub warn: bool,
}

impl RenderedLine {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            warn: false,
        }
    }
    fn warned(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            warn: true,
        }
    }
}

/// Pure helper — the workload-pod table, assembled into lines.
/// Extracted from [`print_pod_summaries`] so the drift marking (which
/// pods get flagged `← old config`, and whether the trailing explanation
/// appears at all) is testable without a cluster or a terminal.
pub(crate) fn render_pod_summary_lines(
    pods: &[PodSummary],
    inner_name: &str,
    namespace: &str,
    config_changed_at: Option<&str>,
) -> Vec<RenderedLine> {
    let mut out = vec![
        RenderedLine::plain(String::new()),
        RenderedLine::plain(format!(
            "Workload pods ({namespace}, app.kubernetes.io/name={inner_name}):"
        )),
    ];
    if pods.is_empty() {
        out.push(RenderedLine::plain(
            "  (none — operator may not have rendered the Deployment yet, or \
             the AppRafter Application's `spec.expose` is omitted so no \
             workload runs)",
        ));
        return out;
    }
    let name_w = pods.iter().map(|p| p.name.len()).max().unwrap_or(4).max(4);
    let status_w = pods
        .iter()
        .map(|p| p.status.len())
        .max()
        .unwrap_or(6)
        .max(6);
    out.push(RenderedLine::plain(format!(
        "  {:<name_w$}  READY  {:<status_w$}  RESTARTS  AGE",
        "NAME",
        "STATUS",
        name_w = name_w,
        status_w = status_w,
    )));
    let mut any_stale = false;
    for p in pods {
        let stale = pod_is_stale(p.started_at.as_deref(), config_changed_at);
        any_stale |= stale;
        let flag = if stale { "  ← old config" } else { "" };
        let row = format!(
            "  {:<name_w$}  {:<5}  {:<status_w$}  {:<8}  {}{flag}",
            p.name,
            p.ready,
            p.status,
            p.restarts,
            p.age,
            name_w = name_w,
            status_w = status_w,
        );
        // Yellow, not red: the pod is healthy and serving. It is serving the
        // PREVIOUS secret values, which is a thing to know rather than an
        // outage — and colouring it as a failure would train the reader to
        // ignore it.
        out.push(if stale {
            RenderedLine::warned(row)
        } else {
            RenderedLine::plain(row)
        });
    }
    if any_stale {
        out.push(RenderedLine::plain(String::new()));
        out.push(RenderedLine::warned(
            "  Some pods started before this application's secrets last changed, \n                   so they are still serving the previous values. An environment \n                   variable from a secret is resolved once at pod start and never \n                   re-read; restarting the workload is what picks up the new one.",
        ));
    }
    out
}

fn print_pod_summaries(
    pods: &[PodSummary],
    inner_name: &str,
    namespace: &str,
    config_changed_at: Option<&str>,
) {
    for line in render_pod_summary_lines(pods, inner_name, namespace, config_changed_at) {
        if line.warn {
            println!("{}", style::warn(&line.text));
        } else {
            println!("{}", line.text);
        }
    }
}

/// Shell out to `kubectl get services` filtered to the
/// AppRafter operator's `app.kubernetes.io/name` label.
/// Mirrors `list_pods_for_apprafter_app` / `app_open::
/// list_services_for_apprafter_app` shape.
fn list_services_for_apprafter_app(
    apprafter_app_name: &str,
    namespace: &str,
    kubeconfig: &Path,
) -> Result<Vec<ServiceSummary>> {
    let out = Command::new("kubectl")
        .args(kubectl_list_args(
            "services",
            namespace,
            Some(&operator_workload_selector(apprafter_app_name)),
        ))
        .env("KUBECONFIG", kubeconfig)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl get services: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl get services failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
    Ok(parse_service_summaries(&parsed))
}

/// Pure helper — parse `kubectl get services -o json` items
/// into `ServiceSummary` rows. Defensive against shape drift:
/// missing fields fall back to sensible defaults.
pub(crate) fn parse_service_summaries(payload: &Value) -> Vec<ServiceSummary> {
    let items = payload.get("items").and_then(Value::as_array);
    let Some(items) = items else { return vec![] };
    items
        .iter()
        .filter_map(|svc| {
            let name = svc
                .pointer("/metadata/name")
                .and_then(Value::as_str)?
                .to_string();
            let type_ = svc
                .pointer("/spec/type")
                .and_then(Value::as_str)
                .unwrap_or("ClusterIP")
                .to_string();
            let cluster_ip = svc
                .pointer("/spec/clusterIP")
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string();
            let ports = svc
                .pointer("/spec/ports")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| {
                            let port = p.get("port").and_then(Value::as_i64)?;
                            let proto = p.get("protocol").and_then(Value::as_str).unwrap_or("TCP");
                            Some(format!("{port}/{proto}"))
                        })
                        .collect::<Vec<String>>()
                        .join(",")
                })
                .unwrap_or_default();
            Some(ServiceSummary {
                name,
                type_,
                cluster_ip,
                ports,
            })
        })
        .collect()
}

/// Pure helper — the workload-service table, assembled into lines.
/// Extracted from [`print_service_summaries`] for the same reason as
/// [`render_tracked_resource_lines`]: the widths are the behaviour.
pub(crate) fn render_service_lines(
    services: &[ServiceSummary],
    inner_name: &str,
    namespace: &str,
) -> Vec<String> {
    let mut out = vec![
        String::new(),
        format!("Workload services ({namespace}, app.kubernetes.io/name={inner_name}):"),
    ];
    if services.is_empty() {
        out.push(
            "  (none — the AppRafter Application's `spec.expose` may be omitted \
             so the operator renders no Service)"
                .to_string(),
        );
        return out;
    }
    let name_w = services
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let type_w = services
        .iter()
        .map(|s| s.type_.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let ip_w = services
        .iter()
        .map(|s| s.cluster_ip.len())
        .max()
        .unwrap_or(10)
        .max(10);
    out.push(format!(
        "  {:<name_w$}  {:<type_w$}  {:<ip_w$}  PORTS",
        "NAME",
        "TYPE",
        "CLUSTER-IP",
        name_w = name_w,
        type_w = type_w,
        ip_w = ip_w,
    ));
    for s in services {
        out.push(format!(
            "  {:<name_w$}  {:<type_w$}  {:<ip_w$}  {}",
            s.name,
            s.type_,
            s.cluster_ip,
            s.ports,
            name_w = name_w,
            type_w = type_w,
            ip_w = ip_w,
        ));
    }
    out
}

fn print_service_summaries(services: &[ServiceSummary], inner_name: &str, namespace: &str) {
    for line in render_service_lines(services, inner_name, namespace) {
        println!("{line}");
    }
}

/// Shell out to `kubectl get resourceclaim.apprafter.io`
/// (GROUP-QUALIFIED — bare `resourceclaim` collides with the
/// k8s 1.32+ DRA `resourceclaims.resource.k8s.io`). Returns
/// only the claims owned by `owner` (the inner AppRafter
/// Application), filtered in `parse_resource_claim_summaries`.
fn list_resource_claims_for_app(
    owner: &str,
    namespace: &str,
    kubeconfig: &Path,
) -> Result<Vec<ResourceClaimSummary>> {
    let parsed = list_resource_claim_payload(namespace, kubeconfig)?;
    Ok(parse_resource_claim_summaries(&parsed, owner))
}

/// The raw `kubectl get resourceclaim.apprafter.io -n <ns> -o json`
/// payload. Split out of [`list_resource_claims_for_app`] so a caller
/// that needs a DIFFERENT filter over the same namespace-wide list —
/// `app remove`'s blast-radius enumeration, which is owned by any
/// workload of the bundle rather than by one — spends one read rather
/// than one per workload.
fn list_resource_claim_payload(namespace: &str, kubeconfig: &Path) -> Result<Value> {
    let out = Command::new("kubectl")
        .args(kubectl_list_args(RESOURCECLAIM_RESOURCE, namespace, None))
        .env("KUBECONFIG", kubeconfig)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl get resourceclaim: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl get resourceclaim.apprafter.io failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))
}

/// Pure helper — parse `kubectl get resourceclaim.apprafter.io
/// -o json` items into `ResourceClaimSummary` rows, FILTERED to
/// claims whose `metadata.ownerReferences[]` carries an entry
/// with `kind == "Application"` and `name == owner`. The inner
/// app owns the claims its `needs.*` block generates, so a
/// namespace-wide list is narrowed to just this app's claims.
pub(crate) fn parse_resource_claim_summaries(
    payload: &Value,
    owner: &str,
) -> Vec<ResourceClaimSummary> {
    let items = payload.get("items").and_then(Value::as_array);
    let Some(items) = items else { return vec![] };
    items
        .iter()
        .filter(|claim| claim_owned_by(claim, owner))
        .map(summarise_resource_claim)
        .collect()
}

/// Does the claim's `metadata.ownerReferences[]` carry an
/// `Application`-kind entry naming `owner`?
fn claim_owned_by(claim: &Value, owner: &str) -> bool {
    claim
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .map(|refs| {
            refs.iter().any(|r| {
                r.get("kind").and_then(Value::as_str) == Some("Application")
                    && r.get("name").and_then(Value::as_str) == Some(owner)
            })
        })
        .unwrap_or(false)
}

fn summarise_resource_claim(claim: &Value) -> ResourceClaimSummary {
    let name = claim
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let provider = claim
        .pointer("/status/provider")
        .and_then(Value::as_str)
        .unwrap_or("—")
        .to_string();
    let ready = claim
        .pointer("/status/ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // `connectionSecretRef` is a plain string in this CRD; fall
    // back to `.name` if a future shape makes it an object.
    let secret_ref = claim.pointer("/status/connectionSecretRef").and_then(|v| {
        v.as_str()
            .map(String::from)
            .or_else(|| v.get("name").and_then(Value::as_str).map(String::from))
    });
    let scheduled = claim
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(|conds| {
            conds.iter().any(|c| {
                c.get("type").and_then(Value::as_str) == Some("Scheduled")
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
        .unwrap_or(false);
    ResourceClaimSummary {
        name,
        provider,
        ready,
        secret_ref,
        scheduled,
        backing: backing_resource(claim),
        size: claim_size_cell(claim),
    }
}

/// Human bytes, at the precision a size column wants.
fn human_bytes(n: i64) -> String {
    const U: [(&str, i64); 4] = [("GB", 1 << 30), ("MB", 1 << 20), ("KB", 1 << 10), ("B", 1)];
    for (unit, div) in U {
        if n >= div {
            let v = n as f64 / div as f64;
            return if *unit == *"B" {
                format!("{n} B")
            } else if v >= 10.0 {
                format!("{v:.0} {unit}")
            } else {
                format!("{v:.1} {unit}")
            };
        }
    }
    "0 B".to_string()
}

/// How much data this claim holds, rendered per backend (2.22d / D8).
///
/// Three shapes, because three backends can honestly answer different
/// questions and forcing one unit would mean inventing a number:
///
///  * a **disk** has its own PVC, so used/total and a percentage;
///  * **pg** reports on-disk BYTES, read from CNPG's own exporter;
///  * **redis** reports a KEY COUNT and says so, because that is the whole
///    of what Dragonfly will tell us about one logical DB: the per-DB byte
///    figures exist inside the server but are summed across databases at
///    every point they could reach a client, the `db`-labelled metrics are
///    all counters, and the DB-scoped `DEBUG` subcommands report slot
///    capacity or logical lengths rather than bytes.
///
/// `—` when nothing has been measured. Never a zero: "not sampled" and
/// "empty" are different, and rendering the first as the second tells a
/// tenant their database is empty when it is merely unmeasured.
pub(crate) fn claim_size_cell(claim: &Value) -> String {
    let used = claim
        .pointer("/status/capacity/usedBytes")
        .and_then(Value::as_i64);
    let cap = claim
        .pointer("/status/capacity/capacityBytes")
        .and_then(Value::as_i64);
    if let (Some(used), Some(cap)) = (used, cap) {
        if cap > 0 {
            let pct = (used as f64 / cap as f64 * 100.0).round() as i64;
            // D29: when the operator measured the HOST DISK rather than this
            // volume — which is what the kubelet reports for a local-path PV,
            // because a directory on a shared filesystem has no quota — say
            // so, and lead with the size the claim actually asked for.
            //
            // The alternative of printing the pair unlabelled told an operator
            // their 1Gi claim held 80GB. The alternative of printing the
            // requested size alone was rejected when D8 shipped, for answering
            // the easy half of the question and reading as if it had answered
            // both. This does neither: two true facts, each named.
            if claim
                .pointer("/status/capacity/scope")
                .and_then(Value::as_str)
                == Some("host")
            {
                let requested = claim
                    .pointer("/spec/size")
                    .and_then(Value::as_str)
                    .unwrap_or("—");
                return format!("{requested} · host disk {pct}% full");
            }
            return format!("{} / {} ({pct}%)", human_bytes(used), human_bytes(cap));
        }
    }
    if let Some(bytes) = claim.pointer("/status/size/bytes").and_then(Value::as_i64) {
        return human_bytes(bytes);
    }
    if let Some(keys) = claim.pointer("/status/size/keys").and_then(Value::as_i64) {
        return format!("{keys} keys");
    }
    "—".to_string()
}

/// The concrete backend resource serving a claim (2.22d / D8).
///
/// The table has always answered "was it provisioned" and never "what
/// exists now". A pooled Dragonfly claim lives as a numbered DB on a named
/// instance; a disk claim is a standalone PVC. Both are in the claim's own
/// status already, so this costs nothing and turns a row that said `true
/// true` into one an operator can act on.
///
/// Deliberately NOT a size or a fullness. Neither is on the ResourceClaim,
/// and neither is reachable from the CLI: a PVC carries its provisioned
/// size but not its usage, and usage comes from the kubelet Summary API,
/// which only the operator samples. Printing provisioned size alone would
/// answer the easy half of the question the column exists for, and read as
/// if it had answered both. Recorded as follow-on in D8 instead.
pub(crate) fn backing_resource(claim: &Value) -> String {
    let instance = claim.pointer("/status/instance").and_then(Value::as_str);
    let dbnum = claim.pointer("/status/dbnum").and_then(Value::as_i64);
    if let (Some(i), Some(n)) = (instance, dbnum) {
        return format!("{i} (db {n})");
    }
    if let Some(i) = instance {
        return i.to_string();
    }
    if let Some(pvc) = claim
        .pointer("/status/volumeClaimRef")
        .and_then(Value::as_str)
    {
        return format!("pvc/{pvc}");
    }
    // 2.5 (ADR 0061): a jetstream claim names neither an instance nor a PVC.
    // What the provisioner created for it on the shared NATS server is the
    // set of STREAMS, and it records them in `status.streams` — the
    // inventory it observed on the server, not the declaration it sent
    // (`jetstream_status_body` in resourceclaim-provisioner). So the cell
    // answers this column's question ("what actually got created", not "what
    // was asked for") from the claim's own status, the same standing as the
    // instance/PVC arms above and with no extra API call.
    //
    // `unattributed` is deliberately NOT counted: those are streams sitting
    // in the account that no declaration accounts for, so they are not this
    // claim's backing — they surface as a `ForeignSubjectCapture` condition
    // instead.
    //
    // An empty inventory prints "no streams" rather than falling through to
    // "—": for a consume-only application it is the permanent, correct
    // answer, and it is a MEASURED one (the provisioner wrote `observedAt`),
    // which is exactly the distinction "—" would erase.
    if let Some(streams) = claim.pointer("/status/streams") {
        let count = |k: &str| {
            streams
                .get(k)
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0)
        };
        let (declared, dynamic) = (count("declared"), count("dynamic"));
        let total = declared + dynamic;
        if total == 0 {
            return "no streams".to_string();
        }
        let plural = if total == 1 { "stream" } else { "streams" };
        if dynamic > 0 {
            return format!("{total} {plural} ({dynamic} dynamic)");
        }
        return format!("{total} {plural}");
    }
    "—".to_string()
}

/// The `secret:` bindings this Application declares, read off the CR the
/// caller already fetched (2.22c / D7).
///
/// No extra API call: `app status` holds the Application JSON, and the
/// bindings live in its own spec. That is why this direction is nearly free
/// once the parser exists — the other direction (`secret seal`'s blast
/// radius) needs a cluster-wide list, this one needs nothing.
///
/// Prints the SECRET and KEY names only. A key name is not secret material;
/// the value is, and nothing here reads one.
/// Pure helper — the `Secrets (<ns>/<app>)` block, assembled into lines.
/// Extracted from [`print_secret_bindings_for_app`]. An app that binds
/// nothing renders NOTHING (not an empty header), which is the invariant
/// worth holding: the block is context, and a header with no rows would
/// read as "your secrets are missing".
pub(crate) fn render_secret_binding_lines(app: &Value, name: &str, namespace: &str) -> Vec<String> {
    // parse_secret_bindings takes a LIST shape; wrap the single CR so the
    // one parser serves both callers rather than growing a near-copy.
    let wrapped = serde_json::json!({ "items": [app] });
    let bindings = crate::commands::secret::parse_secret_bindings(&wrapped);
    if bindings.is_empty() {
        return vec![];
    }
    let var_w = bindings
        .iter()
        .map(|b| b.env_var.len())
        .max()
        .unwrap_or(3)
        .max(3);
    let mut out = vec![
        String::new(),
        format!("Secrets ({namespace}/{name}):"),
        format!("  {:<var_w$}  SECRET/KEY  (SCOPE)", "ENV", var_w = var_w),
    ];
    for b in &bindings {
        out.push(format!(
            "  {:<var_w$}  {}/{}  ({})",
            b.env_var,
            b.secret,
            b.key,
            b.scope,
            var_w = var_w
        ));
    }
    out
}

fn print_secret_bindings_for_app(app: &Value, name: &str, namespace: &str) {
    for line in render_secret_binding_lines(app, name, namespace) {
        println!("{line}");
    }
}

/// Pure helper — the `Resource provisioning (<ns>)` claims table,
/// assembled into lines. Extracted from [`print_resource_claims`].
pub(crate) fn render_resource_claim_lines(
    claims: &[ResourceClaimSummary],
    namespace: &str,
) -> Vec<String> {
    let mut out = vec![
        String::new(),
        format!("Resource provisioning ({namespace}):"),
    ];
    if claims.is_empty() {
        out.push(
            "  (none — the AppRafter Application declares no `needs.*` resources)".to_string(),
        );
        return out;
    }
    let name_w = claims
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let provider_w = claims
        .iter()
        .map(|c| c.provider.len())
        .max()
        .unwrap_or(8)
        .max(8);
    let backing_w = claims
        .iter()
        .map(|c| c.backing.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let size_w = claims
        .iter()
        .map(|c| c.size.len())
        .max()
        .unwrap_or(4)
        .max(4);
    out.push(format!(
        "  {:<name_w$}  {:<provider_w$}  READY  SCHEDULED  {:<backing_w$}  {:<size_w$}  SECRET",
        "NAME",
        "PROVIDER",
        "BACKING",
        "SIZE",
        name_w = name_w,
        provider_w = provider_w,
        backing_w = backing_w,
        size_w = size_w,
    ));
    for c in claims {
        let secret = c.secret_ref.as_deref().unwrap_or("-");
        out.push(format!(
            "  {:<name_w$}  {:<provider_w$}  {:<5}  {:<9}  {:<backing_w$}  {:<size_w$}  {}",
            c.name,
            c.provider,
            if c.ready { "true" } else { "false" },
            if c.scheduled { "true" } else { "false" },
            c.backing,
            c.size,
            secret,
            backing_w = backing_w,
            size_w = size_w,
            name_w = name_w,
            provider_w = provider_w,
        ));
    }
    out
}

fn print_resource_claims(claims: &[ResourceClaimSummary], namespace: &str) {
    for line in render_resource_claim_lines(claims, namespace) {
        println!("{line}");
    }
}

/// Resolve the Argo CD Application for a per-env-aware command
/// (`logs`, `rollback`): `<name>-<env>` when `--env` is given, else the
/// bare `<name>`. On a not-found where `--env` was omitted, if the app
/// is actually deployed per-env (apps match the
/// `apprafter.io/application=<name>` label) return a helpful error
/// listing the env-deployments and pointing at `--env` — mirroring the
/// per-env aggregation `status` / `remove` already do.
///
/// Returns `(application_json, argo_name)` — pass `argo_name` to any
/// later kubectl call that targets the Argo app's `metadata.name`
/// (e.g. rollback's patch target), NOT the bare `name`.
/// Pure helper — the Argo CD `metadata.name` a per-env-aware command
/// targets: `<name>-<env>` when `--env` is given, else the bare
/// `<name>`. The same `<name>-<env>` shape `add` / `remove` stamp.
fn argo_app_name_for(name: &str, env: Option<&str>) -> String {
    match env {
        Some(e) => format!("{name}-{e}"),
        None => name.to_string(),
    }
}

/// Pure helper — the "deployed per environment" guidance message shown
/// when `--env` was omitted but the app only exists as `<name>-<env>`
/// deployments. `envs` is the list of resolved environment labels;
/// empty ⇒ a generic "multiple".
fn per_env_guidance_message(name: &str, envs: &[String]) -> String {
    format!(
        "'{name}' is deployed per environment ({}). Pass `--env <env>` to target one.",
        if envs.is_empty() {
            "multiple".to_string()
        } else {
            envs.join(", ")
        }
    )
}

/// Given the per-env deployments matching `apprafter.io/application=<name>`
/// (looked up when `--env` was omitted), either auto-resolve the single one
/// or guide to `--env`. EXACTLY ONE ⇒ the logical name is unambiguous, so
/// resolve it (the user interacts by the logical name — the same one in
/// `app list` — and shouldn't need `--env` for a single-env app). TWO OR
/// MORE ⇒ ambiguous, return the `--env` guidance. Pure + testable; callers
/// pass a non-empty `grouped`.
fn single_deployment_or_guidance(name: &str, grouped: Vec<Value>) -> Result<(Value, String)> {
    if grouped.len() == 1 {
        let app = grouped.into_iter().next().expect("len == 1 checked");
        let argo_name = app
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_string();
        return Ok((app, argo_name));
    }
    let envs: Vec<String> = grouped
        .iter()
        .filter_map(|a| {
            a.pointer("/metadata/labels/apprafter.io~1environment")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .collect();
    Err(CliError::Other(per_env_guidance_message(name, &envs)))
}

pub(crate) fn resolve_app_for_command(
    name: &str,
    env: Option<&str>,
    kc: &Path,
) -> Result<(Value, String)> {
    let argo_name = argo_app_name_for(name, env);
    if let Some(app) = kubectl_get_json(
        "application.argoproj.io",
        Some(&argo_name),
        Some(ARGOCD_NAMESPACE),
        kc,
    )? {
        return Ok((app, argo_name));
    }
    // Not found directly. If no `--env` and the app is deployed
    // per-env, guide the user to pass `--env <env>`.
    if env.is_none() {
        let grouped = kubectl_get_json_by_selector(
            "application.argoproj.io",
            &format!("apprafter.io/application={name}"),
            Some(ARGOCD_NAMESPACE),
            kc,
        )
        .unwrap_or_default();
        if !grouped.is_empty() {
            return single_deployment_or_guidance(name, grouped);
        }
    }
    Err(CliError::Other(format!(
        "Application '{argo_name}' not found in namespace {ARGOCD_NAMESPACE}. Run \
         `apprafter app list`."
    )))
}

/// `apprafter app logs <name>` — stream logs from the
/// workload pods of an apprafter-managed Application. Pure
/// shell-out to `kubectl logs`, scoped to the app's destination
/// namespace (read from the Application CR's
/// `spec.destination.namespace`). ADR 0044 (2.9): `--env <env>`
/// selects the per-env deployment `<name>-<env>`; omit for a
/// base/single-env app.
///
/// Without `--pod`: aggregate via `-l <selector>` — the
/// AppRafter operator stamps `app.kubernetes.io/name: <inner
/// workload name>` on the Deployment it renders (the same label
/// `app status` selects workload pods by). That inner name comes
/// from the repo's `Application.cue` metadata.name and can differ
/// from the Argo CD parent name typed at `app add`, so we resolve
/// it from `status.resources[]` before building the selector.
/// `--pod` overrides the selector with a direct pod name.
///
/// ADR 0062: `name` is the REGISTRATION, which deploys 1..N workloads,
/// and this verb multiplexes across every one of them — see
/// [`WorkloadDemand::ReadEvery`] for why that is the right default here
/// and nowhere else. `--workload` narrows to one.
pub fn logs(
    name: &str,
    env: Option<String>,
    follow: bool,
    tail: i64,
    container: Option<String>,
    pod: Option<String>,
    workload: Option<String>,
) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    let (app, argo_name) = resolve_app_for_command(name, env.as_deref(), kc.path())?;

    let (workload_ns, inner) = resolve_logs_workload(&app, &argo_name, workload.as_deref())?;
    if let Some(banner) = multiplex_banner(name, &inner, pod.as_deref()) {
        eprintln!("{banner}");
    }
    let target = build_kubectl_logs_target(&inner, pod.as_deref());
    let args = build_kubectl_logs_args(&target, &workload_ns, follow, tail, container.as_deref());

    let status = Command::new("kubectl")
        .args(&args)
        .env("KUBECONFIG", kc.path())
        .status()
        .map_err(|e| CliError::Other(format!("spawn kubectl logs: {e}")))?;
    if !status.success() {
        return Err(CliError::Other(format!(
            "kubectl logs failed (exit {:?})",
            status.code()
        )));
    }
    Ok(())
}

/// Pure — the line `app logs` prints before an interleaved stream, or
/// `None` when the stream is not interleaved.
///
/// An interleaved stream is only readable if the reader knows it is one.
/// `--prefix=true` names the pod on every line and the operator names
/// each Deployment after its workload, but that answers "which workload
/// is this line", not "which workloads am I watching".
///
/// It is a CLAIM about the command that follows, so it has to be true of
/// it. `--pod` makes [`build_kubectl_logs_target`] return
/// [`KubectlLogsTarget::Pod`] regardless of how many workloads resolved,
/// so announcing three and then streaming one pod would be a false
/// statement — the same distinction this module keeps everywhere else
/// between what was looked at and what was found. Hence the predicate is
/// `--pod` absent AND more than one workload, not the workload count
/// alone.
pub(crate) fn multiplex_banner(
    application: &str,
    workloads: &[String],
    pod: Option<&str>,
) -> Option<String> {
    if pod.is_some() || workloads.len() <= 1 {
        return None;
    }
    Some(format!(
        "ℹ Streaming all {} workloads of '{application}' ({}). Narrow with \
         `--workload <name>`.",
        workloads.len(),
        workloads.join(", ")
    ))
}

/// Pure helper — resolve `(workload namespace, pod label values)` for
/// `app logs` from the Argo CD Application. Extracted from [`logs`].
///
/// INVARIANT: the pod labels are the INNER AppRafter app names from
/// `status.resources[]`, which are `Application.cue`'s `metadata.name`
/// and need not equal the Argo CD parent typed at `app add`. Only when
/// Argo tracks no AppRafter CR (a raw-YAML app) does it fall back to the
/// RESOLVED Argo name — `<name>-<env>` for an env deploy, never the bare
/// logical name.
///
/// Since 2.27b the return is a LIST: a registration deploys 1..N
/// workloads and this verb streams all of them ([`WorkloadDemand::ReadEvery`]).
/// One name is the pre-2.27b answer exactly, so today's fleet is
/// unchanged down to the argv.
///
/// The namespace still comes from the registration's
/// `spec.destination.namespace` rather than from the chosen workloads —
/// a bundle is one namespace (ADR 0062 §Decision), and this is the read
/// that has always refused when there is none rather than defaulting.
pub(crate) fn resolve_logs_workload(
    app: &Value,
    argo_name: &str,
    workload: Option<&str>,
) -> Result<(String, Vec<String>)> {
    let workload_ns = app
        .pointer("/spec/destination/namespace")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::Other(format!(
                "Application '{argo_name}' does not carry `spec.destination.namespace` — no \
                 namespace to point kubectl logs at. The CR may have been created outside \
                 `apprafter app add`."
            ))
        })?;
    let refs = crate::commands::app_open::apprafter_app_refs(app);
    let inner = match workload_for(&refs, workload, WorkloadDemand::ReadEvery) {
        WorkloadChoice::One(w) => vec![w.name],
        WorkloadChoice::Every(ws) => ws.into_iter().map(|w| w.name).collect(),
        // The raw-YAML fallback, unchanged: Argo CD tracks no AppRafter
        // CR at all, so the Argo object's own name is the best selector
        // value available.
        WorkloadChoice::NoWorkloads => vec![argo_name.to_string()],
        WorkloadChoice::Unknown { asked, available } => {
            return Err(CliError::Other(unknown_workload_message(
                argo_name, &asked, &available,
            )));
        }
        WorkloadChoice::Unplaceable(w) => {
            return Err(CliError::Other(unplaceable_workload_message(argo_name, &w)));
        }
        // `ReadEvery` never asks and never refuses — that is the whole
        // of the `logs` decision.
        WorkloadChoice::Ask(_) | WorkloadChoice::Refuse(_) => {
            unreachable!("WorkloadDemand::ReadEvery resolves every workload; see `workload_for`")
        }
    };
    Ok((workload_ns.to_string(), inner))
}

/// `apprafter app rollback <name> [--to <revision|sha256:digest>]`.
///
/// Two operations behind one verb, because a developer who has shipped a bad
/// build should not have to know which of them their situation calls for:
///
///  * an **image digest** pins the AppRafter CR to that digest (ADR 0059).
///    This is a MODE change — the application stops following its tag —
///    because merely setting the workload back would be undone by the next
///    reconcile re-resolving the tag and finding the bad build again.
///  * a **Git revision** patches the Argo CD Application's `targetRevision`,
///    which is what this command has always done.
///
/// Bare `rollback` prefers the retained digest when the CR has one; see
/// [`classify_rollback_target`].
///
/// **The two branches have OPPOSITE cardinality** (ADR 0062 §Write
/// surfaces), and that is what shapes the function below:
///
/// * `--to <git-rev>` patches the REGISTRATION's `targetRevision`, which
///   Argo CD re-renders the whole package from — every workload in the
///   bundle moves. It therefore needs no workload selector and must not
///   refuse on ambiguity: there is nothing ambiguous about it.
/// * every other form writes ONE workload's CR, so it goes through
///   [`WorkloadDemand::Write`] and refuses rather than picking.
///
/// Bare `rollback` is on the second side even though it may END on the
/// first: whether it resolves to the retained digest is a fact of a
/// workload's CR, and at N > 1 choosing whose CR to read is exactly the
/// choice this verb will not make for the caller.
pub fn rollback(
    name: &str,
    env: Option<String>,
    to: Option<String>,
    yes: bool,
    workload: Option<String>,
) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let (app, argo_name) = resolve_app_for_command(name, env.as_deref(), kc.path())?;
    let refs = app_open::apprafter_app_refs(&app);

    // Which branch an EXPLICIT `--to` takes is decided syntactically:
    // with `to = Some(_)`, `classify_rollback_target` returns before it
    // reads either CR, so passing `None` for the AppRafter one here
    // reaches the identical verdict — including its refusals for a
    // malformed digest or a refname carrying a colon. Deciding it this
    // early is what lets the Git branch skip the workload resolution
    // entirely, which it must: it moves the whole bundle, so there is
    // nothing for `--workload` to select and nothing to refuse over.
    if to.is_some() {
        let target = classify_rollback_target(to.as_deref(), None, &app)?;
        if let RollbackTarget::GitRevision(rev) = &target {
            // Site (a): the caller may have named a workload this branch
            // cannot honour. Refuse BEFORE the write rather than
            // disclosing it in a prompt `--yes` skips.
            vet_rollback_scope(
                name,
                workload.as_deref(),
                to.as_deref(),
                &target,
                refs.len(),
            )?;
            return rollback_to_revision(&app, &argo_name, rev, refs.len(), yes, kc.path());
        }
    }

    // The AppRafter CR is where a pin lives and where the retained digest is
    // recorded. Absent on an application Argo CD has not synced yet, which
    // is a refusal rather than a fallback: a pin write cannot create the CR.
    let scope = BundleScope {
        application: name.to_string(),
        size: refs.len(),
        env: env.clone(),
    };
    // Every flag the caller typed that the retry needs, in the order a
    // reader would have typed them: `--env` selects the deployment,
    // `--to` selects what to roll back to.
    let suffix = format!(
        "{}{}",
        env_echo(env.as_deref()),
        to.as_deref()
            .map(|t| format!(" --to {t}"))
            .unwrap_or_default()
    );
    let cr = match workload_for(&refs, workload.as_deref(), WorkloadDemand::Write) {
        WorkloadChoice::One(w) => read_apprafter_cr(&w, kc.path()),
        // Nothing synced. With no `--workload` this is the pre-2.27b
        // behaviour exactly — no CR, so a bare rollback falls through to
        // the Git-revision branch and an explicit digest is refused
        // below for want of a CR. WITH one, the named workload does not
        // exist yet, and saying that is more use than the downstream
        // "no image to roll back to", which would blame the workload for
        // the registration's state.
        WorkloadChoice::NoWorkloads => {
            if let Some(w) = workload.as_deref() {
                return Err(CliError::Other(format!(
                    "Application '{name}' has not synced yet, so it deploys no workload \
                     '{w}' to roll back. Wait for the first sync, then retry."
                )));
            }
            None
        }
        WorkloadChoice::Refuse(candidates) => {
            return Err(CliError::Other(
                rollback_refusal_lines(name, &candidates, &suffix).join("\n"),
            ));
        }
        WorkloadChoice::Unknown { asked, available } => {
            return Err(CliError::Other(unknown_workload_message(
                name, &asked, &available,
            )));
        }
        WorkloadChoice::Unplaceable(w) => {
            return Err(CliError::Other(unplaceable_workload_message(name, &w)));
        }
        WorkloadChoice::Ask(_) | WorkloadChoice::Every(_) => {
            unreachable!("WorkloadDemand::Write neither asks nor multiplexes")
        }
    };

    let target = classify_rollback_target(to.as_deref(), cr.as_ref(), &app)?;
    // Site (b): a BARE `rollback --workload api` whose workload has no
    // retained digest falls through to a Git revision here, and that
    // moves every workload. Nothing the caller typed hints at it — this
    // is the shape reached by accident.
    vet_rollback_scope(
        name,
        workload.as_deref(),
        to.as_deref(),
        &target,
        refs.len(),
    )?;
    match target {
        RollbackTarget::Digest(digest) => {
            let cr = cr.ok_or_else(|| {
                CliError::Other(format!(
                    "Application '{argo_name}' has not synced yet, so it has no resolved \
                     image to pin. Wait for the first sync, then retry."
                ))
            })?;
            rollback_to_digest(&app, &argo_name, &cr, &digest, scope, yes, kc.path())
        }
        RollbackTarget::GitRevision(rev) => {
            rollback_to_revision(&app, &argo_name, &rev, refs.len(), yes, kc.path())
        }
    }
}

/// Pin the application to `digest` (ADR 0059).
fn rollback_to_digest(
    argo_app: &Value,
    argo_name: &str,
    cr: &Value,
    digest: &str,
    scope: BundleScope,
    yes: bool,
    kubeconfig: &Path,
) -> Result<()> {
    let plan = plan_pin(argo_app, cr, digest, scope)?;

    if !yes {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        for line in pin_prompt_lines(&plan) {
            println!("{line}");
        }
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let at = chrono::Utc::now().to_rfc3339();
    let body = pin_manifest(&plan.cr_name, &plan.cr_ns, Some((&plan.reference, &at)));
    kubectl_apply_server_side(
        &serde_json::to_string(&body).unwrap_or_default(),
        APPRAFTER_CLI_PIN_FIELD_MANAGER,
        kubeconfig,
    )?;

    println!("{}", cli_core::style::warn(&pin_success_line(&plan)));
    let _ = argo_name;
    Ok(())
}

/// The bundle a write verb is acting INSIDE (ADR 0062).
///
/// Carried into the plans rather than passed to the renderers, because
/// the cardinality is part of what the plan resolved: a pin moves one
/// workload of `size`, and both the prompt and the success line have to
/// say so — and both have to quote a way back that is addressable, which
/// needs `application` as well as the workload's own name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BundleScope {
    /// The positional the caller typed — the registration, never a
    /// workload name.
    pub application: String,
    /// How many workloads that registration deploys. `1` is today's
    /// entire fleet and renders exactly as before 2.27b.
    pub size: usize,
    /// The `--env` the caller typed, if any, echoed back into every
    /// command this scope quotes.
    ///
    /// Not decoration: on a registration with two or more environments,
    /// a quoted command without it re-enters `resolve_app_for_command`
    /// with `env: None`, matches several deployments and errors on
    /// `per_env_guidance_message`. Echoing exactly what the caller
    /// passed is both sufficient and precise — their invocation
    /// resolved, so the same flags resolve again — where inferring one
    /// would be guessing at which environment they meant.
    pub env: Option<String>,
}

impl BundleScope {
    /// The command that addresses this plan's workload, at any bundle
    /// size.
    ///
    /// **The positional is the APPLICATION on both branches** — ADR 0062
    /// §Addressing, which admits no other form. Only the `--workload`
    /// disambiguator is conditional, and only because at `size <= 1`
    /// there is nothing to disambiguate.
    ///
    /// The `size <= 1` branch used to quote the WORKLOAD's own name,
    /// which is the pre-2.27b spelling and is a defect wherever the two
    /// names differ: `apprafter app unpin <workload>` re-enters
    /// `resolve_app_for_command`, finds no Argo object of that name and
    /// no registration carrying it as an `apprafter.io/application`
    /// label, and fails with `not found`. That is not a hypothetical
    /// configuration — it is the one this module's own tests pin as
    /// supported (registration `cms-prod`, grouping label `cms`,
    /// rendering a CR called `landing-cms`), and a pin is precisely the
    /// operation whose way back must work, since it keeps acting until
    /// somebody runs it.
    ///
    /// The 2.27b byte-identity rule is not weakened by this: the two
    /// strings coincide for every bundle whose application and workload
    /// share a name — the scaffolded default, and so nearly the whole
    /// fleet — so the output changes only where the old one was already
    /// broken or resolving by coincidence.
    fn verb_for(&self, verb: &str, workload: &str) -> String {
        let env = env_echo(self.env.as_deref());
        if self.size <= 1 {
            format!("apprafter app {verb} {}{env}", self.application)
        } else {
            format!(
                "apprafter app {verb} {} --workload {workload}{env}",
                self.application
            )
        }
    }

    /// "This moves one of N, and leaves the other N-1 alone" — or
    /// nothing at all when there is no other.
    fn one_of_many_line(&self, workload: &str) -> Option<String> {
        (self.size > 1).then(|| {
            format!(
                "  '{workload}' is 1 of the {} workloads application '{}' deploys; the other \
                 {} are not touched.",
                self.size,
                self.application,
                self.size - 1
            )
        })
    }
}

/// Everything [`rollback_to_digest`] needs to write a pin, resolved from
/// the two CRs before any prompting or apply happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinPlan {
    /// AppRafter `Application` CR `metadata.name` — the pin's target.
    pub cr_name: String,
    /// Namespace to write into.
    pub cr_ns: String,
    /// Fully-qualified `repo@sha256:…` reference the pin holds.
    pub reference: String,
    /// What the app is running now, `?` when it has resolved nothing.
    pub current: String,
    /// Which bundle this one workload belongs to.
    pub scope: BundleScope,
}

/// Pure helper — resolve and vet a pin before anything is written.
/// Extracted from [`rollback_to_digest`]: every refusal below is a
/// judgement about the two CRs, and none of them needs a cluster.
///
/// INVARIANTS:
/// * a CR whose namespace is absent borrows the Argo CD Application's
///   `spec.destination.namespace` — the pin must land where the workload
///   is, and a namespace-less apply would silently target `default`;
/// * a Git-managed pin is REFUSED rather than written, because the next
///   Argo sync reverts it and the operator would be left believing a
///   rollback happened;
/// * pinning to what is already running is refused as a no-op, so the
///   reader is not told a rollback succeeded when nothing moved.
pub(crate) fn plan_pin(
    argo_app: &Value,
    cr: &Value,
    digest: &str,
    scope: BundleScope,
) -> Result<PinPlan> {
    let reference = compose_pin_reference(digest, cr)?;
    let cr_name = cr
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Other("AppRafter Application CR has no name".into()))?;
    let cr_ns = cr
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .or_else(|| {
            argo_app
                .pointer("/spec/destination/namespace")
                .and_then(Value::as_str)
        })
        .ok_or_else(|| CliError::Other("cannot determine the application's namespace".into()))?;

    if pin_appears_git_managed(cr) {
        return Err(CliError::Other(format!(
            "'{cr_name}' declares `apprafter.io/image-pin` in its own manifest, so Git owns \
             it and the next sync would revert this pin. Change the manifest instead."
        )));
    }

    let current = cr
        .pointer("/status/image/resolved")
        .and_then(Value::as_str)
        .unwrap_or("?");
    if current == reference {
        return Err(CliError::Other(format!(
            "'{cr_name}' is already running {reference} — rollback would be a no-op."
        )));
    }

    Ok(PinPlan {
        cr_name: cr_name.to_string(),
        cr_ns: cr_ns.to_string(),
        reference,
        current: current.to_string(),
        scope,
    })
}

/// Pure helper — the pin confirmation preamble. Extracted from
/// [`rollback_to_digest`]. It names the mode change explicitly (and the
/// verb that undoes it) because a pin is the one rollback that keeps
/// acting after it returns.
///
/// Since 2.27b it also names the CARDINALITY, which is the sharpest
/// thing about this verb: `--to <digest>` pins ONE workload and
/// `--to <git-rev>` moves EVERY workload of the bundle, and a prompt
/// that read the same for both would be consent for the wrong
/// operation. See [`revision_prompt_lines`] for the other half — each
/// points at the other, because the reader reaching for one may have
/// wanted the other and nothing in the flag itself says which is which.
///
/// At `scope.size <= 1` the lines are exactly the pre-2.27b three: there
/// is no cardinality to disclose, and today's entire fleet is that case.
pub(crate) fn pin_prompt_lines(plan: &PinPlan) -> Vec<String> {
    let PinPlan {
        cr_name,
        reference,
        current,
        scope,
        ..
    } = plan;
    let mut out = vec![format!(
        "Roll back '{cr_name}' from {current} to {reference}?"
    )];
    out.extend(scope.one_of_many_line(cr_name));
    // "the application" is the pre-2.27b wording and is right at N = 1,
    // where the bundle and the workload are the same thing. At N > 1 it
    // is wrong in the direction that matters — a pin is per workload, and
    // saying "the application" over a three-workload bundle claims a
    // blast radius three times the real one.
    out.push(if scope.size > 1 {
        format!(
            "  This PINS it: '{cr_name}' stops following its tag until you run `{}`.",
            scope.verb_for("unpin", cr_name)
        )
    } else {
        format!(
            "  This PINS the application: it stops following its tag until you run `{}`.",
            scope.verb_for("unpin", cr_name)
        )
    });
    out.push(if scope.size > 1 {
        format!(
            "  To roll back a Git revision instead, pass `--to <revision>` — that moves ALL \
             {} workloads together.",
            scope.size
        )
    } else {
        "  To roll back a Git revision instead, pass `--to <revision>`.".to_string()
    });
    out
}

/// Pure helper — what a completed pin reports. Extracted from
/// [`rollback_to_digest`]; carries the un-pin verb so the mode change is
/// never a one-way door the reader has to go looking for.
///
/// The quoted verb goes through [`BundleScope::verb_for`], so on a
/// bundle it is a command that actually resolves — `apprafter app unpin
/// <workload>` would not, the positional being the application.
pub(crate) fn pin_success_line(plan: &PinPlan) -> String {
    let PinPlan {
        cr_name,
        reference,
        scope,
        ..
    } = plan;
    format!(
        "✓ '{cr_name}' pinned to {reference}. It is no longer following its tag — \
         resume with `{}`.",
        scope.verb_for("unpin", cr_name)
    )
}

/// Pure helper — the Argo CD Application's current
/// `spec.source.targetRevision`, or `?` when absent. Extracted from
/// [`rollback_to_revision`].
pub(crate) fn current_target_revision(app: &Value) -> String {
    app.pointer("/spec/source/targetRevision")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string()
}

/// Pure helper — refuse a Git rollback that would change nothing.
/// Extracted from [`rollback_to_revision`]; `None` lets it proceed.
///
/// INVARIANT: a no-op is an ERROR, not a silent success. Argo CD would
/// accept the patch and report a sync, so "rolled back" would be printed
/// over a workload that never moved — the exact failure a developer
/// running rollback under pressure cannot afford to be told.
pub(crate) fn noop_revision_error(target_revision: &str, current_revision: &str) -> Option<String> {
    if target_revision != current_revision {
        return None;
    }
    Some(format!(
        "Target revision '{target_revision}' matches the current \
         `spec.source.targetRevision` — rollback would be a no-op."
    ))
}

/// Pure — refuse a `--workload` that the resolved rollback branch would
/// silently discard (ADR 0062 §Write surfaces).
///
/// The exact inverse of [`workload_for`]'s refusal, and the same defect
/// with the sign flipped. That one stops a write acting on a workload
/// nobody named; this one stops a write acting on MORE than the workload
/// the caller did name — which is what `--to <git-rev> --workload api`
/// was doing, and what a bare `rollback --workload api` was doing
/// whenever that workload had no image to roll back to.
///
/// Two shapes reach here and they must not render alike:
///
/// * an EXPLICIT `--to <git-rev>`: the caller named the revision, so the
///   message names their flag;
/// * a BARE `rollback` that fell through for want of a retained image:
///   the caller named no revision at all, so blaming a `--to` they never
///   typed would be a false statement about their own command. This is
///   the shape reached by accident, and the one `--help` never described.
///
/// **No size exception.** `--workload` means "act on this workload", and
/// a Git-revision rollback does not act on a workload at any N — it
/// patches the registration's `targetRevision`. Accepting it at N = 1
/// because the blast radius coincides would teach a mental model that
/// becomes a silent N-fold write the day the bundle grows a second
/// workload.
///
/// The prompt DOES disclose the cardinality, but `--yes` skips the
/// prompt, and a disclosure that arrives after the write is not one.
pub(crate) fn vet_rollback_scope(
    application: &str,
    workload: Option<&str>,
    to: Option<&str>,
    target: &RollbackTarget,
    workloads: usize,
) -> Result<()> {
    let RollbackTarget::GitRevision(revision) = target else {
        return Ok(());
    };
    let Some(w) = workload else {
        return Ok(());
    };
    let scope = if workloads > 1 {
        format!("all {workloads} workloads of '{application}'")
    } else {
        format!("the whole application '{application}'")
    };
    let cause = match to {
        Some(raw) => format!(
            "`--to {}` is a Git revision, which moves the application's `targetRevision` — \
             {scope} roll back together, not just '{w}'.",
            raw.trim()
        ),
        None => format!(
            "'{w}' has no image to roll back to, so `rollback` falls through to Git revision \
             '{revision}' — which moves the application's `targetRevision`, and {scope} with \
             it, not just '{w}'."
        ),
    };
    Err(CliError::Other(format!(
        "{cause}\n\
         Either drop `--workload {w}` to roll the whole application back, or pass \
         `--to <sha256:digest>` to pin just '{w}'."
    )))
}

/// Pure helper — the Git-revision confirmation preamble (ADR 0062).
///
/// The twin of [`pin_prompt_lines`], and the reason both exist as pure
/// functions: the two branches of one verb have OPPOSITE cardinality.
/// This one patches the registration's `targetRevision`, which Argo CD
/// then re-renders the whole package from — so every workload in the
/// bundle moves, whether or not the reader was thinking about more than
/// one of them.
///
/// Line 0 is byte-identical to the pre-2.27b `println!`, at every bundle
/// size; the disclosure is an ADDED line, so a single-workload bundle —
/// today's entire fleet — reads exactly as it did.
pub(crate) fn revision_prompt_lines(
    argo_name: &str,
    current_revision: &str,
    target_revision: &str,
    workloads: usize,
) -> Vec<String> {
    let mut out = vec![format!(
        "Roll back Application '{argo_name}' from revision '{current_revision}' to \
         '{target_revision}'?"
    )];
    if workloads > 1 {
        out.push(format!(
            "  This moves the application's Git revision, so ALL {workloads} workloads it \
             deploys roll back together. To roll back ONE workload's image instead, pass \
             `--to <sha256:digest> --workload <name>`."
        ));
    }
    out
}

/// Pure helper — what a completed Git-revision rollback reports.
///
/// "the workload", singular, is what this line said before 2.27b — the
/// same defect `delete_success_line` already carries a fix for. After
/// moving three of them it is simply false, and the reader has no other
/// signal that three moved.
pub(crate) fn revision_success_line(
    argo_name: &str,
    target_revision: &str,
    workloads: usize,
) -> String {
    let what = if workloads > 1 {
        format!("all {workloads} workloads")
    } else {
        "the workload".to_string()
    };
    format!(
        "✓ Application '{argo_name}' rolled back to revision '{target_revision}'. Argo CD \
         will sync {what} within a reconcile cycle."
    )
}

/// Patch the Argo CD Application's `targetRevision` — the original behaviour.
fn rollback_to_revision(
    app: &Value,
    argo_name: &str,
    target_revision: &str,
    workloads: usize,
    yes: bool,
    kubeconfig: &Path,
) -> Result<()> {
    let current_revision = current_target_revision(app);
    if let Some(msg) = noop_revision_error(target_revision, &current_revision) {
        return Err(CliError::Other(msg));
    }

    if !yes {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        for line in revision_prompt_lines(argo_name, &current_revision, target_revision, workloads)
        {
            println!("{line}");
        }
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let body = rollback_patch_body(target_revision);
    // Patch the RESOLVED Argo CD object (`<name>-<env>` for an env
    // deploy), NOT the bare logical name — otherwise a per-env rollback
    // would target a non-existent `<name>` Application.
    kubectl_merge_patch(
        "application.argoproj.io",
        argo_name,
        Some(ARGOCD_NAMESPACE),
        None,
        &body,
        kubeconfig,
    )?;

    println!(
        "{}",
        revision_success_line(argo_name, target_revision, workloads)
    );
    Ok(())
}

/// `apprafter app unpin <name>` — resume following the tag (ADR 0059).
///
/// Mandatory rather than a convenience: without it `rollback` is a one-way
/// door out of the platform's auto-deploy feature, and the only way back
/// would be hand-editing the manifest — the same trap the defect records,
/// entered from the other side.
///
/// Removes the pin by re-applying the SAME body under the SAME field manager
/// with the annotations omitted, so server-side apply prunes exactly the two
/// keys that manager owns.
/// ADR 0062: `name` is the REGISTRATION. A pin lives on ONE workload's
/// CR, so at N > 1 this refuses rather than un-pinning whichever sorts
/// first — the mirror of [`rollback`]'s digest branch, and refused for
/// the same reason: nothing downstream can tell an un-pin nobody asked
/// for from one they did.
pub fn unpin(name: &str, env: Option<String>, yes: bool, workload: Option<String>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let (app, argo_name) = resolve_app_for_command(name, env.as_deref(), kc.path())?;
    let refs = app_open::apprafter_app_refs(&app);
    let scope = BundleScope {
        application: name.to_string(),
        size: refs.len(),
        env: env.clone(),
    };

    let cr = match workload_for(&refs, workload.as_deref(), WorkloadDemand::Write) {
        WorkloadChoice::One(w) => read_apprafter_cr(&w, kc.path()),
        WorkloadChoice::NoWorkloads => None,
        WorkloadChoice::Refuse(candidates) => {
            return Err(CliError::Other(
                ambiguous_write_lines("unpin", name, &candidates, &env_echo(env.as_deref()))
                    .join("\n"),
            ));
        }
        WorkloadChoice::Unknown { asked, available } => {
            return Err(CliError::Other(unknown_workload_message(
                name, &asked, &available,
            )));
        }
        WorkloadChoice::Unplaceable(w) => {
            return Err(CliError::Other(unplaceable_workload_message(name, &w)));
        }
        WorkloadChoice::Ask(_) | WorkloadChoice::Every(_) => {
            unreachable!("WorkloadDemand::Write neither asks nor multiplexes")
        }
    };
    let cr = cr.ok_or_else(|| {
        CliError::Other(format!(
            "Application '{argo_name}' has not synced yet — there is nothing pinned."
        ))
    })?;

    let plan = plan_unpin(&app, &cr, scope)?;
    if plan.pinned.is_none() {
        println!("'{}' is not pinned — nothing to do.", plan.cr_name);
        return Ok(());
    }

    if !yes {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        for line in unpin_prompt_lines(&plan) {
            println!("{line}");
        }
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let body = pin_manifest(&plan.cr_name, &plan.cr_ns, None);
    kubectl_apply_server_side(
        &serde_json::to_string(&body).unwrap_or_default(),
        APPRAFTER_CLI_PIN_FIELD_MANAGER,
        kc.path(),
    )?;

    println!(
        "✓ '{}' un-pinned — following {} again.",
        plan.cr_name, plan.tag
    );
    Ok(())
}

/// Everything [`unpin`] needs, resolved from the two CRs before anything
/// is prompted or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnpinPlan {
    pub cr_name: String,
    pub cr_ns: String,
    /// The reference the pin currently holds; `None` when there is no pin
    /// and `unpin` is a no-op.
    pub pinned: Option<String>,
    /// The tag the app resumes following, or the placeholder `its tag`
    /// when the CR has resolved none.
    pub tag: String,
    /// Which bundle this one workload belongs to.
    pub scope: BundleScope,
}

/// Pure helper — resolve the un-pin target. Extracted from [`unpin`].
///
/// INVARIANT: the namespace fallback matches [`plan_pin`]'s exactly — the
/// un-pin re-applies the SAME body under the SAME field manager, so a
/// namespace that differed by one step would prune nothing and leave the
/// application pinned while reporting success.
pub(crate) fn plan_unpin(argo_app: &Value, cr: &Value, scope: BundleScope) -> Result<UnpinPlan> {
    let cr_name = cr
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Other("AppRafter Application CR has no name".into()))?;
    let cr_ns = cr
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .or_else(|| {
            argo_app
                .pointer("/spec/destination/namespace")
                .and_then(Value::as_str)
        })
        .ok_or_else(|| CliError::Other("cannot determine the application's namespace".into()))?;
    Ok(UnpinPlan {
        cr_name: cr_name.to_string(),
        cr_ns: cr_ns.to_string(),
        pinned: cr
            .pointer("/metadata/annotations/apprafter.io~1image-pin")
            .and_then(Value::as_str)
            .map(String::from),
        tag: cr
            .pointer("/status/image/tag")
            .and_then(Value::as_str)
            .unwrap_or("its tag")
            .to_string(),
        scope,
    })
}

/// Pure helper — the un-pin confirmation preamble. Extracted from
/// [`unpin`]: un-pinning can roll the workload FORWARD to whatever the
/// tag now points at, which is the thing the reader is actually agreeing
/// to and is not implied by the word "un-pin".
pub(crate) fn unpin_prompt_lines(plan: &UnpinPlan) -> Vec<String> {
    let UnpinPlan {
        cr_name,
        pinned,
        tag,
        scope,
        ..
    } = plan;
    let held = pinned.as_deref().unwrap_or("nothing");
    let mut out = vec![format!("Un-pin '{cr_name}' (currently held at {held})?")];
    // Same disclosure as the pin, for the same reason: this is a write
    // against one workload of a bundle, and the reader is agreeing to it
    // without having named the other N-1 anywhere.
    out.extend(scope.one_of_many_line(cr_name));
    out.push(format!(
        "  It will resume following {tag} and may roll forward to whatever that now points \
         at, within one reconcile."
    ));
    out
}

/// Fetch ONE workload's AppRafter `Application` CR.
///
/// `None` when the object is simply not there. Best-effort on the read
/// itself; the CALLERS decide whether absence is fatal, and for both pin
/// verbs it is.
///
/// Takes a [`PlacedWorkload`] rather than the registration, which is the
/// 2.27b change: WHICH workload is a decision [`workload_for`] makes and
/// refuses to guess at, not one this read may make by taking the first
/// entry of `status.resources[]`. The namespace arrives as a plain
/// `String` for the same reason — `kubectl -n ""` silently means the
/// kubeconfig's default namespace.
fn read_apprafter_cr(workload: &PlacedWorkload, kubeconfig: &Path) -> Option<Value> {
    // The managed-fields variant: `pin_appears_git_managed` reads
    // `metadata.managedFields`, and kubectl STRIPS that from `get -o json`
    // unless asked. Without the flag the guard sees an empty list, concludes
    // nobody owns the annotation, and can never fire. It is also why the
    // pin verbs cannot be served by `AppIndex`'s cached CRs — that read
    // does not pass the flag.
    kubectl_get_json_showing_managed_fields(
        "application.apprafter.io",
        Some(&workload.name),
        Some(&workload.namespace),
        kubeconfig,
    )
    .ok()
    .flatten()
}

pub fn remove(name: &str, yes: bool, keep_data: bool, env: Option<String>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // ADR 0044 (2.9): `--env <env>` targets the ONE per-environment
    // deployment registered as `<name>-<env>`. Without `--env`, remove
    // is logical — it tears down EVERY env-deployment grouped under the
    // `apprafter.io/application=<name>` label.
    if let Some(e) = &env {
        let argo_app_name = format!("{name}-{e}");
        return remove_single_app(&argo_app_name, yes, keep_data, kc.path());
    }

    // No `--env`: list all env-deployments grouped by the logical-app
    // label. Multiple matches → confirm once, then delete each.
    let grouped = kubectl_get_json_by_selector(
        "application.argoproj.io",
        &format!("apprafter.io/application={name}"),
        Some(ARGOCD_NAMESPACE),
        kc.path(),
    )?;

    let argo_names = match plan_remove(name, &grouped) {
        RemovePlan::Single(argo_app_name) => {
            return remove_single_app(&argo_app_name, yes, keep_data, kc.path());
        }
        RemovePlan::Batch(names) => names,
    };

    if !yes {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        for line in batch_remove_prompt_lines(name, &argo_names) {
            println!("{line}");
        }
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Loop-delete each env-deployment. `--keep-data` (finalizer strip) is
    // applied to every one. `yes=true` skips per-app re-prompting — the
    // operator already confirmed the batch above.
    for an in &argo_names {
        remove_single_app(an, true, keep_data, kc.path())?;
    }
    Ok(())
}

/// What `apprafter app remove <name>` (no `--env`) resolves to once the
/// logical-app label selector has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemovePlan {
    /// Delete exactly this Argo CD `metadata.name`, confirming as usual.
    Single(String),
    /// Delete every one of these, behind ONE batch confirmation.
    Batch(Vec<String>),
}

/// Pure helper — decide what a no-`--env` remove deletes. Extracted from
/// [`remove`] so the pre-2.9 fallback is testable without a cluster.
///
/// INVARIANT: an EMPTY selector result falls back to the bare logical
/// name — a pre-2.9 app carries no `apprafter.io/application` label, so
/// "the selector matched nothing" must not be read as "there is nothing
/// to delete". A single match uses that object's OWN `metadata.name`
/// (which may be `<name>-<env>`), never the logical name, or the delete
/// would target a non-existent Application.
pub(crate) fn plan_remove(name: &str, grouped: &[Value]) -> RemovePlan {
    if grouped.len() <= 1 {
        return RemovePlan::Single(
            grouped
                .first()
                .and_then(|a| a.pointer("/metadata/name").and_then(Value::as_str))
                .map(String::from)
                .unwrap_or_else(|| name.to_string()),
        );
    }
    RemovePlan::Batch(
        grouped
            .iter()
            .filter_map(|a| {
                a.pointer("/metadata/name")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .collect(),
    )
}

/// Pure helper — the batch-delete confirmation preamble. Extracted from
/// [`remove`]: the reader is about to destroy several environments at
/// once, so every Argo name is listed rather than counted.
pub(crate) fn batch_remove_prompt_lines(name: &str, argo_names: &[String]) -> Vec<String> {
    let mut out = vec![format!(
        "Delete ALL {} environment deployments of '{name}'?",
        argo_names.len()
    )];
    out.extend(argo_names.iter().map(|an| format!("  • {an}")));
    out
}

/// What `app remove <name>` does with a string that names no Argo CD
/// `Application` — once [`AppIndex`] has said what it DID name (ADR 0062
/// §Write surfaces).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoveTarget {
    /// Tear down this registration: the bundle and every workload in it.
    ///
    /// The ONLY variant that deletes anything, and it is reachable from
    /// exactly one place — a string that is a registration's own
    /// `metadata.name`, which the pre-flight read happened to miss. No
    /// workload name produces it at any bundle size; see
    /// [`plan_remove_target`].
    Bundle(String),
    /// The string named ONE workload of a `bundle_size`-workload (> 1)
    /// bundle that `registration` deploys. Refused — see
    /// [`refuse_workload_lines`].
    RefuseWorkload {
        workload: String,
        registration: String,
        bundle_size: usize,
    },
    /// The string named the SOLE workload of `registration`. Also
    /// refused, but for a different reason and with a different message:
    /// nothing about it is unsupported, the caller simply named the
    /// wrong object. See [`name_the_application_lines`].
    NameTheApplication {
        workload: String,
        registration: String,
    },
    /// The index cannot attribute the string to any registration. Keep
    /// the plain "not found" — `remove` operates on registrations, and
    /// for this string there is none.
    Unknown,
}

/// Pure — the whole `app remove` targeting decision, so the rule that
/// decides whether a destructive verb runs is table-tested without a
/// cluster.
///
/// `bundle_size` is how many workloads the registration deploys IN TOTAL
/// (the resolved workload included, so 1 means "it is the only one"),
/// and is consulted on the workload arm ONLY.
///
/// **A workload name never deletes anything, at any bundle size.** ADR
/// 0062 §Addressing: the positional argument of every `app` verb is the
/// registration name, and when it fails to resolve, the error resolves
/// it as a workload and names the command that would have worked. So the
/// two workload arms differ only in what they SAY:
///
/// - `bundle_size > 1` — the git steps, because removing one workload of
///   several is genuinely not something the CLI can do. `app add` writes
///   `syncPolicy.automated.selfHeal: true`, so deleting that CR here is
///   undone on the next reconcile and the command would report a success
///   that does not survive a sync.
/// - `bundle_size <= 1` — just the application's name, because there is
///   nothing unsupported about the intent; the caller named the workload
///   when the verb takes the application.
///
/// The second case USED to proceed, on the reasoning that at N = 1
/// removing the registration is removing the workload. That was wrong in
/// the direction that matters: `apprafter app remove <workload> --yes`
/// previously failed safe, and making it destroy a registration is a
/// silent escalation on a positional the ADR says is never a workload
/// name — and the caller who typed a workload name did not know N was 1.
///
/// `typed` is compared against the resolved registration on the
/// `Registration` arm and nowhere else. That arm is reachable only as a
/// race (the pre-flight read missed an object the index then saw), and
/// the retry is allowed ONLY when the index resolved the very string the
/// caller typed — so the object deleted is always the one they named.
pub(crate) fn plan_remove_target(
    typed: &str,
    resolution: &Resolution,
    bundle_size: usize,
) -> RemoveTarget {
    match resolution {
        Resolution::Workload(w) => match w.registration.as_deref() {
            Some(registration) if bundle_size > 1 => RemoveTarget::RefuseWorkload {
                workload: w.name.clone(),
                registration: registration.to_string(),
                bundle_size,
            },
            Some(registration) => RemoveTarget::NameTheApplication {
                workload: w.name.clone(),
                registration: registration.to_string(),
            },
            // Claimed by nobody: a CR applied by hand, or one left behind
            // by a removed registration. There is no registration to tear
            // down, and inventing one would delete somebody else's.
            None => RemoveTarget::Unknown,
        },
        // A race, and only a race: retry the read against the name the
        // caller typed. A registration the index reached by some OTHER
        // string (its grouping label) is deliberately not actioned here
        // — `remove`'s own label selector already covers that path on the
        // happy side, and honouring it here would re-open the escalation
        // by a rarer door.
        Resolution::Registration(registration, _) if registration == typed => {
            RemoveTarget::Bundle(registration.clone())
        }
        // Every remaining shape is a question `remove` must not answer by
        // guessing: two namespaces, two environments, or nothing at all.
        Resolution::Registration(_, _)
        | Resolution::AmbiguousWorkload(_)
        | Resolution::AmbiguousRegistration(_)
        | Resolution::PendingRegistration(_)
        | Resolution::NotFound => RemoveTarget::Unknown,
    }
}

/// Pure — what `app remove` says when the string named the SOLE workload
/// of a registration.
///
/// Distinct from [`refuse_workload_lines`] on purpose. Nothing here is
/// unsupported: the caller wants exactly what `app remove` does, they
/// just named the workload instead of the application that deploys it.
/// So the message states the distinction once and hands over the command
/// — no git steps, because git is not the route to what they asked for.
///
/// It is an ERROR, not a redirect. `--yes` skips the confirmation, so
/// acting on the caller's behalf here would turn an invocation that
/// failed safe into one that destroys a registration.
pub(crate) fn name_the_application_lines(workload: &str, registration: &str) -> Vec<String> {
    vec![
        format!(
            "'{workload}' is not an application; it is the only workload of application \
             '{registration}'."
        ),
        "To remove that application (and this workload with it):".to_string(),
        format!("  apprafter app remove {registration}"),
    ]
}

/// Pure — the refusal for ONE workload of a multi-workload bundle.
///
/// This **will** be reported as a missing feature, so the message has to
/// say plainly that it is GitOps and not an unimplemented verb: the CR
/// delete is not refused because the code cannot do it, it is refused
/// because Argo CD self-heal restores the object within one reconcile
/// and the command would report a success that does not survive a sync.
///
/// A bare refusal leaves the reader stuck, so both routes are printed —
/// the git steps that really remove one workload, and the command that
/// removes the whole bundle, which is the other thing they may have
/// meant.
pub(crate) fn refuse_workload_lines(
    workload: &str,
    registration: &str,
    bundle_size: usize,
) -> Vec<String> {
    vec![
        format!(
            "'{workload}' is 1 of {bundle_size} workloads deployed by application \
             '{registration}'."
        ),
        "Deleting its Application CR from here is undone by Argo CD self-heal on the next \
         sync, so `app remove` does not do it."
            .to_string(),
        String::new(),
        "To remove just this workload:".to_string(),
        format!(
            "  1. delete the `{workload}:` block from the bundle's manifest \
             (apprafter/Application.cue)"
        ),
        "  2. commit and push — Argo CD prunes it on the next sync".to_string(),
        String::new(),
        format!("To remove the application and ALL {bundle_size} workloads:"),
        format!("  apprafter app remove {registration}"),
    ]
}

/// What the confirmation knows about the data a cascade-prune destroys.
///
/// Two states, and the distinction is the whole point: "we looked and
/// there is none" and "we could not look" must never render the same
/// way. The same discipline as `app status`'s `—`-never-blank rule — an
/// unmeasured cell says so rather than printing a value it does not
/// have. On a destructive confirmation the stakes are higher: an
/// unavailable enumeration rendering as an empty one reads as "no data
/// is at risk", which is the one thing this section exists to prevent
/// anybody concluding by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaimInventory {
    /// The read succeeded. Every entry is a rendered bullet line; an
    /// empty vector means the bundle really holds nothing.
    Known(Vec<String>),
    /// The read failed, or was never made. Carries the command to run by
    /// hand so the reader can check before consenting.
    Unavailable(String),
}

/// `apprafter.io` kinds in `status.resources[]` whose prune destroys
/// DATA rather than a re-creatable object.
///
/// `RetainedClaim` is excluded on purpose: it is the object whose entire
/// purpose is that the data SURVIVES its claim, so naming it here would
/// invert what it means.
const DATA_BEARING_KINDS: &[&str] = &["ResourceClaim", "SharedVolume"];

/// The data-bearing resources a registration's own `status.resources[]`
/// declares, as prompt bullet lines.
///
/// This is the MANIFEST-declared half only — a `ResourceClaim` or
/// `SharedVolume` the user wrote in CUE, which the cue-cmp sidecar
/// renders and Argo CD therefore tracks. The claims the operator
/// generates from `spec.needs` are its own owner-ref'd children, never
/// synced by Argo CD, and so never appear here. Those are the
/// databases, which is why the other half of the enumeration
/// ([`read_claim_inventory`]) spends a cluster read rather than
/// settling for this list.
fn data_bearing_lines(app: &Value) -> Vec<String> {
    let dest = app
        .pointer("/spec/destination/namespace")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    app.pointer("/status/resources")
        .and_then(Value::as_array)
        .map(|resources| {
            resources
                .iter()
                .filter_map(|r| {
                    let kind = r.get("kind").and_then(Value::as_str)?;
                    if r.get("group").and_then(Value::as_str) != Some("apprafter.io")
                        || !DATA_BEARING_KINDS.contains(&kind)
                    {
                        return None;
                    }
                    let name = r.get("name").and_then(Value::as_str)?;
                    match r
                        .get("namespace")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .or(dest)
                    {
                        Some(ns) => Some(format!("    • {kind} {name} (namespace: {ns})")),
                        None => Some(format!("    • {kind} {name}")),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pure helper — the bullet lines for every `ResourceClaim` in a
/// namespace-wide payload that ANY workload of the bundle owns.
///
/// The owner set is the bundle's, not one workload's, which is why this
/// cannot reuse `parse_resource_claim_summaries`: that one filters to a
/// single owner, and `app remove` is tearing down all of them at once.
/// A namespace may hold a second registration's apps — the
/// shared-volumes guide registers `writer` and `reader` into one
/// namespace — so filtering by namespace alone would name another
/// application's database in this one's blast radius.
///
/// Ownership is the same `ownerReferences[] {kind: Application, name}`
/// test `claim_owned_by` applies, reused verbatim so the two surfaces
/// cannot disagree about what a workload owns.
pub(crate) fn owned_claim_lines(
    payload: &Value,
    owners: &[String],
    namespace: &str,
) -> Vec<String> {
    payload
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|c| owners.iter().any(|o| claim_owned_by(c, o)))
                .filter_map(|c| c.pointer("/metadata/name").and_then(Value::as_str))
                .map(|name| format!("    • ResourceClaim {name} (namespace: {namespace})"))
                .collect()
        })
        .unwrap_or_default()
}

/// Enumerate the data a cascade-prune of this bundle destroys.
///
/// ONE extra cluster read, scoped to the bundle's namespace — ADR 0062
/// makes a bundle one namespace, so a namespace-wide list is both
/// sufficient and bounded, and it is filtered back to this bundle's own
/// workloads by [`owned_claim_lines`]. It is spent only on a path that
/// was already stopping to prompt a human, which is what makes it
/// affordable where `app list`'s doubled read had to be argued for.
///
/// It covers what `status.resources[]` structurally cannot: the claims
/// the operator generated from `spec.needs`, i.e. the databases. A
/// blast-radius line that omits the databases is not doing the job the
/// confirmation exists for.
///
/// Every failure path returns [`ClaimInventory::Unavailable`], never an
/// empty [`ClaimInventory::Known`] — including a workload whose
/// namespace could not be resolved, because an enumeration that silently
/// skipped one workload is indistinguishable from a complete one.
fn read_claim_inventory(
    app: &Value,
    workloads: &[CrRef],
    kubeconfig_path: &Path,
) -> ClaimInventory {
    let dest = app
        .pointer("/spec/destination/namespace")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let probe = |ns: &str| format!("kubectl get {RESOURCECLAIM_RESOURCE} -n {ns}");

    // One namespace by ADR 0062, but derived rather than assumed: if the
    // workloads disagree, or any of them cannot be placed, we cannot
    // claim to have enumerated the bundle.
    let mut namespaces: Vec<&str> = Vec::new();
    for w in workloads {
        let Some(ns) = w.namespace.as_deref().or(dest) else {
            return ClaimInventory::Unavailable(probe(dest.unwrap_or("<namespace>")));
        };
        if !namespaces.contains(&ns) {
            namespaces.push(ns);
        }
    }

    let owners: Vec<String> = workloads.iter().map(|w| w.name.clone()).collect();
    let mut lines: Vec<String> = data_bearing_lines(app);
    for ns in &namespaces {
        let Ok(payload) = list_resource_claim_payload(ns, kubeconfig_path) else {
            return ClaimInventory::Unavailable(probe(ns));
        };
        lines.extend(owned_claim_lines(&payload, &owners, ns));
    }
    // A manifest-declared claim is ALSO owner-ref'd once the operator
    // adopts it, so the two halves overlap. Sort then dedup — the order
    // is the reader's, not Argo CD's.
    lines.sort();
    lines.dedup();
    ClaimInventory::Known(lines)
}

/// Pure helper — the delete confirmation for ONE registration.
///
/// ADR 0062: a registration is a BUNDLE of 1..N workloads. Before 2.27b
/// this surface printed [`single_remove_prompt_line`] and nothing else —
/// one line, in the singular, naming the Argo CD object, as the entire
/// consent for tearing down however many production workloads it
/// deployed and whatever data they held. At N > 1 the block below names
/// every workload and every data-bearing resource the cascade prunes; a
/// count is not consent, which is the rule
/// [`batch_remove_prompt_lines`] already follows for environments.
///
/// At N <= 1 it returns exactly [`single_remove_prompt_line`], by
/// CALLING it rather than by re-deriving its wording — so today's fleet,
/// every registration of which deploys one workload, is byte-identical
/// by construction. A registration that has never synced tracks no
/// workload at all and lands in the same arm: a list it cannot fill, or
/// a count of zero, would read as "broken" for an application that is
/// merely new.
///
/// INVARIANT: `keep_data` and a plain remove say OPPOSITE things about
/// survival, exactly as [`remove_success_line`] does. The keep-data path
/// strips the cascade finalizer before the delete, so nothing is pruned
/// — naming the data there would threaten what is not at risk.
///
/// `claims` is [`read_claim_inventory`]'s answer. `None` means the
/// caller did not consult it, which is correct on every path that
/// renders no data section (N <= 1, or `--keep-data`) and a bug
/// anywhere else — so the data section treats `None` as
/// [`ClaimInventory::Unavailable`] rather than as an empty list. A
/// forgotten read then reads as "could not check", never as "nothing to
/// lose".
pub(crate) fn remove_prompt_lines(
    argo_app_name: &str,
    app: &Value,
    keep_data: bool,
    claims: Option<&ClaimInventory>,
) -> Vec<String> {
    let workloads = app_open::apprafter_app_refs(app);
    if workloads.len() <= 1 {
        return vec![single_remove_prompt_line(argo_app_name, app)];
    }
    let n = workloads.len();
    let project = app
        .pointer("/spec/project")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let repo = app
        .pointer("/spec/source/repoURL")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let mut out = if keep_data {
        vec![
            format!("Delete application '{argo_app_name}' (Argo CD object only)?"),
            format!("  project: {project}, repo: {repo}"),
            format!("  All {n} workloads and their data are preserved — re-register to re-adopt:"),
        ]
    } else {
        vec![
            format!("Delete application '{argo_app_name}' and ALL {n} workloads it deploys?"),
            format!("  project: {project}, repo: {repo}"),
            "  Workloads:".to_string(),
        ]
    };
    out.extend(workloads.iter().map(|w| match &w.namespace {
        Some(ns) => format!("    • {} (namespace: {ns})", w.name),
        // `apprafter_app_refs` reports an unresolvable namespace as
        // `None` rather than dropping the entry. A workload we cannot
        // place is still a workload about to be destroyed, so it is
        // listed without one — never under a guessed namespace.
        None => format!("    • {}", w.name),
    }));
    if !keep_data {
        // An unconsulted inventory fails SAFE. See the `claims` note
        // above: "we could not look" and "there is nothing" must never
        // render the same way.
        let unconsulted = ClaimInventory::Unavailable(format!(
            "kubectl get {RESOURCECLAIM_RESOURCE} -n {}",
            app.pointer("/spec/destination/namespace")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("<namespace>")
        ));
        match claims.unwrap_or(&unconsulted) {
            ClaimInventory::Known(data) if data.is_empty() => out.push(
                "  Data destroyed: none — these workloads hold no ResourceClaim or SharedVolume."
                    .to_string(),
            ),
            ClaimInventory::Known(data) => {
                out.push(
                    "  Data destroyed — every ResourceClaim and SharedVolume these workloads \
                     hold, and it does not come back:"
                        .to_string(),
                );
                out.extend(data.iter().cloned());
            }
            ClaimInventory::Unavailable(command) => {
                out.push(
                    "  Data destroyed: every ResourceClaim and SharedVolume these workloads \
                     hold. The list could NOT be read — check it by hand before confirming:"
                        .to_string(),
                );
                out.push(format!("    {command}"));
            }
        }
    }
    out
}

/// Pure helper — the single-app delete confirmation line. Extracted from
/// [`remove_single_app`]; project and repo are quoted back so the reader
/// can tell two similarly named apps apart before saying yes.
///
/// Since 2.27b this is the N <= 1 arm of [`remove_prompt_lines`], kept as
/// its own function so the byte-identity of the single-workload case is
/// guaranteed by the call rather than by a copied format string.
pub(crate) fn single_remove_prompt_line(argo_app_name: &str, app: &Value) -> String {
    let project = app
        .pointer("/spec/project")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let repo = app
        .pointer("/spec/source/repoURL")
        .and_then(Value::as_str)
        .unwrap_or("?");
    format!("Delete Application '{argo_app_name}' (project: {project}, repo: {repo})?")
}

/// Pure helper — what `remove` reports after the delete succeeded.
/// Extracted from [`remove_single_app`].
///
/// INVARIANT: `--keep-data` and a plain remove say OPPOSITE things about
/// the workload. The finalizer was stripped in the keep-data path, so the
/// AppRafter CR and its pods survive; reporting the cascade wording there
/// would tell an operator their data is gone when it is still running.
///
/// `workloads` is how many the registration deployed (ADR 0062). At
/// `<= 1` both arms are the pre-2.27b wording verbatim; above it they
/// stop saying "the workload", which after removing three is as wrong
/// about what happened as the keep-data arm would be about survival.
pub(crate) fn remove_success_line(
    argo_app_name: &str,
    keep_data: bool,
    workloads: usize,
) -> String {
    match (keep_data, workloads) {
        (true, n) if n > 1 => format!(
            "✓ Application '{argo_app_name}' deleted (Argo CD object only). All {n} workloads and \
             their AppRafter Application CRs are preserved — re-register to re-adopt."
        ),
        (true, _) => format!(
            "✓ Application '{argo_app_name}' deleted (Argo CD object only). The workload and its \
             AppRafter Application CR are preserved — re-register to re-adopt."
        ),
        (false, n) if n > 1 => format!(
            "✓ Application '{argo_app_name}' deleted. Argo CD cascade-prunes the synced AppRafter \
             resources; the operator then removes all {n} workloads."
        ),
        (false, _) => format!(
            "✓ Application '{argo_app_name}' deleted. Argo CD cascade-prunes the synced AppRafter \
             resources; the operator then removes the workload."
        ),
    }
}

/// Delete ONE Argo CD Application — one registration, the whole bundle
/// it deploys (ADR 0062) — by its exact `metadata.name`. Carries the
/// confirm / finalizer-strip / kubectl-delete / report flow that both
/// `remove` paths reuse. `--keep-data` strips the cascade finalizer so
/// the synced AppRafter CRs (and their workloads) are preserved.
///
/// When the name matches nothing, [`resolve_missing_registration`]
/// decides what to say — which is where a workload name gets the ADR
/// 0062 refusal instead of a bare "not found".
fn remove_single_app(
    argo_app_name: &str,
    yes: bool,
    keep_data: bool,
    kubeconfig_path: &Path,
) -> Result<()> {
    // Pre-flight: ensure the Application exists. Otherwise
    // kubectl delete reports `applications.argoproj.io
    // "<name>" not found` with exit 1 — we can surface this as
    // a cleaner CLI message than raw kubectl output.
    let existing = kubectl_get_json(
        "application.argoproj.io",
        Some(argo_app_name),
        Some(ARGOCD_NAMESPACE),
        kubeconfig_path,
    )?;
    let (argo_app_name, app) = match existing {
        Some(app) => (argo_app_name.to_string(), app),
        None => resolve_missing_registration(argo_app_name, kubeconfig_path)?,
    };
    delete_registration(&argo_app_name, &app, yes, keep_data, kubeconfig_path)
}

/// The string named no Argo CD `Application`. Before reporting that, ask
/// the index what it DID name.
///
/// ADR 0062 §Addressing: *"when the positional fails to resolve, the
/// error resolves it as a workload and names the command that would have
/// worked — the courtesy lives on the error path, not in the grammar."*
/// Doing the index read HERE and nowhere else is what keeps that true:
/// the happy path pays nothing, and the two cluster-wide reads are spent
/// only on an invocation that was already going to fail.
///
/// Best-effort on the read itself. A failed index read must not REPLACE
/// the real answer with a read error — the Application genuinely is not
/// there, and that is what the user needs to be told.
fn resolve_missing_registration(typed: &str, kubeconfig_path: &Path) -> Result<(String, Value)> {
    let not_found = || {
        CliError::Other(format!(
            "Application '{typed}' not found in namespace {ARGOCD_NAMESPACE}."
        ))
    };
    let Ok(index) = AppIndex::read(kubeconfig_path) else {
        return Err(not_found());
    };
    let resolution = index.resolve(typed, None);
    let bundle_size = match &resolution {
        Resolution::Workload(w) => w
            .registration
            .as_deref()
            .map_or(0, |r| index.workloads_of(r).len()),
        _ => 0,
    };
    match plan_remove_target(typed, &resolution, bundle_size) {
        RemoveTarget::RefuseWorkload {
            workload,
            registration,
            bundle_size,
        } => Err(CliError::Other(
            refuse_workload_lines(&workload, &registration, bundle_size).join("\n"),
        )),
        // An ERROR, not a redirect — see [`plan_remove_target`]. Acting
        // here would make `app remove <workload> --yes`, which fails
        // safe today, destroy a registration.
        RemoveTarget::NameTheApplication {
            workload,
            registration,
        } => Err(CliError::Other(
            name_the_application_lines(&workload, &registration).join("\n"),
        )),
        // The race arm, and the only one that acts: the index resolved
        // the very string the caller typed, so the object re-read here
        // is the one they named.
        RemoveTarget::Bundle(registration) => {
            let app = kubectl_get_json(
                "application.argoproj.io",
                Some(&registration),
                Some(ARGOCD_NAMESPACE),
                kubeconfig_path,
            )?
            .ok_or_else(not_found)?;
            Ok((registration, app))
        }
        RemoveTarget::Unknown => Err(not_found()),
    }
}

/// Confirm, then delete ONE registration whose object is already in hand.
/// Split out of [`remove_single_app`] so the pre-flight read has exactly
/// one place to redirect to, and so the delete flow is written once.
fn delete_registration(
    argo_app_name: &str,
    app: &Value,
    yes: bool,
    keep_data: bool,
    kubeconfig_path: &Path,
) -> Result<()> {
    // ADR 0062: which workloads this registration deploys. No extra
    // cluster read — the pre-flight `status.resources[]` already says.
    let workloads = app_open::apprafter_app_refs(app);

    if !yes {
        // Interactive confirm: refuse silently without a TTY
        // when `--yes` was not passed. Symmetric with the
        // `apprafter target remove` ergonomics.
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        // The blast-radius read, scoped as tightly as it can be: only an
        // interactive removal of a multi-workload bundle that will
        // actually prune reaches it. Every other path renders no data
        // section, so consulting the cluster for one would be a read
        // nobody reads.
        let claims = (workloads.len() > 1 && !keep_data)
            .then(|| read_claim_inventory(app, &workloads, kubeconfig_path));
        for line in remove_prompt_lines(argo_app_name, app, keep_data, claims.as_ref()) {
            println!("{line}");
        }
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // `app remove` manages ONLY the Argo CD Application; Argo CD owns the
    // cascade-deletion of the resource it synced (the AppRafter Application
    // CR), whose ownerReference then GC's the operator's Deployment → RS →
    // pods. The CLI does not touch the CR.
    if keep_data {
        // Strip the cascade finalizer so deleting the Argo Application does
        // NOT prune the synced AppRafter CR — the workload is preserved.
        kubectl_merge_patch(
            "application.argoproj.io",
            argo_app_name,
            Some(ARGOCD_NAMESPACE),
            None,
            r#"{"metadata":{"finalizers":[]}}"#,
            kubeconfig_path,
        )?;
    }

    // Delete the Argo CD Application. It carries the cascade finalizer
    // (set at `app add`), so Argo cascade-prunes the synced AppRafter CR.
    let out = Command::new("kubectl")
        .args(kubectl_delete_argo_app_args(argo_app_name))
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl delete: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl delete failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }

    println!(
        "{}",
        remove_success_line(argo_app_name, keep_data, workloads.len())
    );
    Ok(())
}

// =========================================================================
// PURE HELPERS — testable without kube::Client / git binary / network.
// =========================================================================

/// Normalise a git URL to the HTTPS form Argo CD prefers:
///
/// - `git@host:org/repo.git` → `https://host/org/repo`
/// - `ssh://git@host/org/repo.git` → `https://host/org/repo`
/// - `https://host/org/repo.git` → `https://host/org/repo`
/// - anything else returned verbatim (caller's responsibility)
///
/// Strips trailing `.git`. Keeps the URL human-readable since
/// it surfaces in `apprafter app list` output.
pub(crate) fn normalise_git_url(url: &str) -> String {
    let url = url.trim();
    let url = url.strip_suffix(".git").unwrap_or(url);
    // SCP-style `git@host:org/repo` — convert to HTTPS.
    if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("https://{host}/{path}");
        }
    }
    // `ssh://git@host/path` → strip `git@` userinfo + flip
    // scheme to https.
    if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        return format!("https://{rest}");
    }
    url.to_string()
}

/// The repo URL as `app list` shows it: the `https://` (or `http://`)
/// scheme stripped for column width, everything else verbatim.
///
/// The contrast with [`normalise_git_url`] directly above is the point.
/// That one REWRITES — `ssh://` and SCP-style become `https://` — which
/// is correct on the WRITE path, because Argo CD's repo-server wants the
/// HTTPS form. Here it would be a lie: `--all-managed` surfaces
/// registrations this CLI never wrote, and rendering an `ssh://` remote
/// as `https://` tells the reader their Argo CD clones over a protocol
/// it does not. So anything that is not an HTTP(S) scheme passes
/// through untouched, `.git` suffix included.
pub(crate) fn display_repo_url(repo: &str) -> String {
    repo.strip_prefix("https://")
        .or_else(|| repo.strip_prefix("http://"))
        .unwrap_or(repo)
        .to_string()
}

/// The `WORKLOADS` cell — how many `apprafter.io/Application` CRs this
/// registration deploys, out of Argo CD's `status.resources[]`.
///
/// Counted here rather than via `app_open::apprafter_app_refs`: that
/// helper DROPS an entry whose `name` is missing and resolves a
/// namespace per entry, neither of which a count needs, and wiring a
/// display cell to a resolution helper would couple this column to
/// changes made for `app open`'s reasons.
///
/// `status.resources` is absent until Argo CD has synced once, and the
/// answer there is the em-dash — the unmeasured marker this repo already
/// uses — never `0`, which would read as "broken" for an application
/// that is merely new.
pub(crate) fn workload_count_cell(app: &Value) -> String {
    let Some(resources) = app.pointer("/status/resources").and_then(Value::as_array) else {
        return "—".to_string();
    };
    resources
        .iter()
        .filter(|r| app_open::is_apprafter_workload(r))
        .count()
        .to_string()
}

/// Argo CD's health codes, **healthiest first**.
///
/// Two sources, and they agree. gitops-engine's `pkg/health/health.go`
/// declares `healthOrder = [Healthy, Suspended, Progressing, Missing,
/// Degraded, Unknown]` and its `IsWorse` compares positions in exactly
/// that slice. `platform-stack/cue/component_argocd.cue` (the
/// `resource.customizations.health.apprafter.io_Application` block)
/// depends on position 1 by name:
///
/// > `Suspended` is the SECOND-healthiest code, so it overrides
/// > `Healthy` and nothing else — the pin is invisible on the tile
/// > whenever a sibling managed resource is `Progressing`, including a
/// > repository that renders several apps into one Argo Application.
///
/// That masking note is why [`workload_health_cell`] exists. Anyone
/// reordering this slice is changing which workload a multi-workload row
/// speaks for, and must read that chart block first.
const HEALTH_ORDER: &[&str] = &[
    "Healthy",
    "Suspended",
    "Progressing",
    "Missing",
    "Degraded",
    "Unknown",
];

/// Position in [`HEALTH_ORDER`]; higher is worse.
///
/// An unrecognised code sorts **worst of all** — one past the end of the
/// slice. This is a deliberate divergence from gitops-engine's
/// `IsWorse`, which leaves an unmatched code at index `0` and so treats
/// a status it has never heard of as the HEALTHIEST thing in the set.
/// Here that would be the exact failure this cell was written to end: a
/// fold that silently renders a word it does recognise while a workload
/// sits in a state this table cannot name. Sorting it worst makes it the
/// word the cell prints, verbatim, so the reader learns of it.
fn health_rank(status: &str) -> usize {
    HEALTH_ORDER
        .iter()
        .position(|h| *h == status)
        .unwrap_or(HEALTH_ORDER.len())
}

/// The `HEALTH` cell folded over every workload the registration
/// deploys, or `None` when it tracks no AppRafter workload at all.
///
/// `None` — not `"Unknown"`, not `"—"` — so the caller falls back to the
/// registration's own `/status/health/status`. Two real cases land
/// there: a registration Argo CD has not synced yet, and a raw
/// YAML/Helm/Kustomize app surfaced by `--all-managed`, which deploys no
/// AppRafter CR and whose only health verdict IS the registration's.
///
/// When every workload agrees the cell is that word **verbatim**, which
/// is what keeps today's entire (N=1) fleet byte-identical to the
/// pre-fold table. Disagreement renders `"<worst> <k>/<n>"`, because
/// `Degraded` alone cannot distinguish 1-of-3 from 3-of-3, and the
/// difference is the difference between a bad deploy and an outage.
///
/// The `· <k> pinned` suffix exists because of the masking note on
/// [`HEALTH_ORDER`]: `Suspended` outranks `Healthy` and nothing else, so
/// a single `Progressing` sibling hides a rollback pin on the Argo CD
/// tile — and `app list` is the only listing in the product where a pin
/// surfaces at all. The suffix is omitted when the aggregate is itself
/// `Suspended`, which already says it.
///
/// NOT what this fixes: `health.status` here is Argo CD's verdict on the
/// **CRs**, and the chart's health script reads only `status.phase`,
/// which the operator sets to `Ready` as soon as it applies the
/// Deployment. A CrashLooping app reads `Healthy` in every one of these
/// cells. That is older and separate; the footer under the table says so
/// rather than letting the column imply pod visibility.
pub(crate) fn workload_health_cell(app: &Value) -> Option<String> {
    let resources = app.pointer("/status/resources").and_then(Value::as_array)?;
    let healths: Vec<&str> = resources
        .iter()
        .filter(|r| app_open::is_apprafter_workload(r))
        // An entry Argo CD has not yet health-checked has no `health`
        // key at all; `Unknown` is what the rest of this file calls that.
        .map(|r| {
            r.pointer("/health/status")
                .and_then(Value::as_str)
                .unwrap_or("Unknown")
        })
        .collect();
    let (first, rest) = healths.split_first()?;

    if rest.iter().all(|h| h == first) {
        return Some((*first).to_string());
    }

    // `max_by_key` would keep the LAST maximum, and two *different*
    // unrecognised codes tie at the bottom rank — naming the later one
    // would be arbitrary. Strictly-greater keeps the first, i.e. the
    // order Argo CD records the resources in.
    let worst = healths
        .iter()
        .copied()
        .reduce(|acc, h| {
            if health_rank(h) > health_rank(acc) {
                h
            } else {
                acc
            }
        })
        .unwrap_or("Unknown");
    let worst_count = healths.iter().filter(|h| **h == worst).count();
    let mut cell = format!("{worst} {worst_count}/{}", healths.len());

    let pinned = healths.iter().filter(|h| **h == "Suspended").count();
    if pinned > 0 && worst != "Suspended" {
        cell.push_str(&format!(" · {pinned} pinned"));
    }
    Some(cell)
}

/// Derive a sane Application name from a normalised repo URL.
/// `https://github.com/foo/my-app` → `my-app`. Strips trailing
/// `.git` defensively (in case the URL slipped through
/// normalisation).
pub(crate) fn derive_app_name(repo_url: &str) -> String {
    let stripped = repo_url.strip_suffix(".git").unwrap_or(repo_url);
    let last_segment = stripped.rsplit('/').next().unwrap_or("app");
    let cleaned: String = last_segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

/// Validate a derived name matches Argo CD's DNS-1123 label
/// constraint. Argo CD enforces this server-side; we catch it
/// client-side with a friendlier error.
fn validate_dns_1123(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(CliError::Other(format!(
            "Application name '{name}' must be 1..63 DNS-1123 characters (lowercase, [a-z0-9-])."
        )));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !ok {
        return Err(CliError::Other(format!(
            "Application name '{name}' contains invalid characters; expected DNS-1123 \
             (lowercase [a-z0-9-], does not start or end with '-')."
        )));
    }
    Ok(())
}

/// Detect the git remote URL + current branch for the cwd.
/// Returns `(repo_url, Some(branch))`, or an error mapped to a
/// CLI-friendly message when the cwd is not a git repo or the
/// named remote does not exist.
fn detect_git_repo_for_cwd(remote: &str) -> Result<(String, Option<String>)> {
    let remote_out = Command::new("git")
        .args(["remote", "get-url", remote])
        .output()
        .map_err(|e| {
            CliError::Other(format!(
                "failed to run git remote get-url {remote}: {e}. Pass the git URL as an \
                 argument: `apprafter app add <git-url>`"
            ))
        })?;
    if !remote_out.status.success() {
        let stderr = String::from_utf8_lossy(&remote_out.stderr);
        return Err(CliError::Other(format!(
            "git remote get-url {remote} failed (exit {:?}): {stderr}\n\
             Run from a git repository or pass the URL explicitly via `apprafter app add <git-url>`.",
            remote_out.status.code()
        )));
    }
    let url = remote_url_or_error(remote, &String::from_utf8_lossy(&remote_out.stdout))?;

    let branch_out = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .output();
    let branch = match branch_out {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => None,
    };
    Ok((url, branch))
}

/// Pure helper — turn `git remote get-url <remote>`'s stdout into the
/// Argo-CD-shaped URL. Extracted from [`detect_git_repo_for_cwd`].
///
/// INVARIANT: whitespace-only stdout is an ERROR, not an empty URL. `git`
/// exits 0 for a remote configured with a blank URL, and letting that
/// through would register an Application pointing at nothing — a failure
/// that surfaces much later, in Argo CD, as somebody else's problem.
pub(crate) fn remote_url_or_error(remote: &str, stdout: &str) -> Result<String> {
    let raw_url = stdout.trim();
    if raw_url.is_empty() {
        return Err(CliError::Other(format!(
            "git remote {remote} returned an empty URL"
        )));
    }
    Ok(normalise_git_url(raw_url))
}

/// `git ls-remote` reachability check. Returns `Ok(())` when
/// the remote responds (HEAD listed), `Err` with an auth-hint
/// when `git` reports authentication failure. Fail-quiet on a
/// missing `git` binary — but errors out so the caller can
/// suggest `--no-ping`.
fn ensure_repo_reachable(repo_url: &str) -> Result<()> {
    let out = Command::new("git")
        .args(["ls-remote", "--exit-code", repo_url, "HEAD"])
        .output()
        .map_err(|e| {
            CliError::Other(format!(
                "failed to run `git ls-remote {repo_url}`: {e}. Pass `--no-ping` to skip \
                 the reachability check."
            ))
        })?;
    if out.status.success() {
        return Ok(());
    }
    Err(CliError::Other(ls_remote_failure_message(
        repo_url,
        &String::from_utf8_lossy(&out.stderr),
        out.status.code(),
    )))
}

/// Pure helper — turn a failed `git ls-remote` into the message `add`
/// raises. Extracted from [`ensure_repo_reachable`].
///
/// INVARIANT: an AUTH failure is routed to `repo creds add` rather than
/// dumped as a generic exit code, and the match is case-insensitive
/// because git's wording varies by transport ("Authentication failed",
/// "fatal: … Permission denied"). Misclassifying it sends the operator
/// looking for a typo in a URL that is perfectly correct.
pub(crate) fn ls_remote_failure_message(
    repo_url: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> String {
    let lower = stderr.to_lowercase();
    if lower.contains("authentication") || lower.contains("permission denied") {
        return format!(
            "git ls-remote refused access to {repo_url}.\n\
             {stderr}\n\
             Register creds via `apprafter repo creds add` and retry `apprafter app add`."
        );
    }
    format!("git ls-remote {repo_url} failed (exit {exit_code:?}): {stderr}")
}

/// Build `kubectl logs` arg vector. Pure fn — tests cover
/// flag combinations exhaustively.
pub(crate) fn build_kubectl_logs_args(
    target: &KubectlLogsTarget,
    namespace: &str,
    follow: bool,
    tail: i64,
    container: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["logs".to_string()];
    match target {
        KubectlLogsTarget::Pod(name) => args.push(name.clone()),
        KubectlLogsTarget::Selector { selector, .. } => {
            args.push("-l".into());
            args.push(selector.clone());
        }
    }
    args.push("-n".into());
    args.push(namespace.to_string());
    if follow {
        args.push("-f".into());
    }
    if tail >= 0 {
        args.push(format!("--tail={tail}"));
    }
    if let Some(c) = container {
        args.push("-c".into());
        args.push(c.to_string());
    }
    // In selector mode there's no single-container guarantee,
    // so explicitly prefix lines with the pod name for the
    // multi-pod case. The single-pod target stays prefix-free
    // — lines already arrive in natural order there.
    if let KubectlLogsTarget::Selector { workloads, .. } = target {
        args.push("--prefix=true".into());
        // On a large scale-out the stream from many pods could
        // overwhelm the terminal; --max-log-requests=N caps
        // kubectl's parallel streaming in selector mode. 10 is
        // kubectl's documented default ceiling; we pass it
        // explicitly for predictability.
        //
        // Since 2.27b the selector may span several WORKLOADS
        // (ADR 0062), so the budget is per workload rather than
        // per command: a three-workload bundle with four replicas
        // each is twelve streams, and a shared ceiling of 10
        // would fail `-f` with an error naming a flag this CLI
        // does not expose. One workload still gets exactly 10.
        args.push(format!("--max-log-requests={}", 10 * (*workloads).max(1)));
    }
    args
}

/// What `kubectl logs` targets — a direct pod name or a label
/// selector. The two forms emit incompatible CLI flags
/// (positional pod name vs `-l` selector), so we model the
/// branch up-front.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KubectlLogsTarget {
    Pod(String),
    /// A label selector plus how many WORKLOADS it spans (ADR 0062).
    ///
    /// The count is carried rather than re-derived from the selector
    /// string, because it decides `--max-log-requests` and parsing it
    /// back out of `in (…)` would be a second source of truth for a fact
    /// the caller already had.
    Selector {
        selector: String,
        workloads: usize,
    },
}

/// Resolve the `kubectl logs` target. Without `--pod` — a label
/// selector via the AppRafter operator's `app.kubernetes.io/name:
/// <inner workload name>` label (the same label `app status`
/// selects workload pods by). `workloads` are the already-resolved
/// INNER names, not the Argo CD parent.
///
/// ADR 0062: one registration deploys 1..N workloads, and `app logs`
/// multiplexes across all of them (see [`WorkloadDemand::ReadEvery`]).
/// One name keeps the pre-2.27b equality selector byte-for-byte —
/// today's entire fleet — and several use a set-based requirement,
/// which `kubectl logs` parses with the same `labels.Parse` every other
/// `-l` goes through.
pub(crate) fn build_kubectl_logs_target(
    workloads: &[String],
    pod: Option<&str>,
) -> KubectlLogsTarget {
    if let Some(name) = pod {
        return KubectlLogsTarget::Pod(name.to_string());
    }
    let selector = match workloads {
        [one] => operator_workload_selector(one),
        many => format!("app.kubernetes.io/name in ({})", many.join(",")),
    };
    KubectlLogsTarget::Selector {
        selector,
        workloads: workloads.len(),
    }
}

/// Pick the "previous" revision from `status.history`. History
/// is ordered chronologically (oldest first, newest last) with
/// monotonically increasing `id`. The previous entry is the
/// second-to-last; roll back to it.
///
/// Returns a string with a lifetime tied to `app` through the
/// `Value` borrow.
/// What `apprafter app rollback` was asked to roll back to (ADR 0059).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RollbackTarget {
    /// An image digest — pin the AppRafter CR to `repo@sha256:…`.
    Digest(String),
    /// A Git revision — patch the Argo CD Application's `targetRevision`.
    GitRevision(String),
}

/// Decide whether `--to` names an image digest or a Git revision, and what a
/// bare `rollback` means (ADR 0059).
///
/// `sha256:<64 lowercase hex>` is a digest; anything else is a Git revision.
/// A value that carries a colon but is not a well-formed digest is REJECTED
/// rather than passed through, because Git refnames forbid `:` — letting it
/// fall through would only defer the same failure to Argo CD, with a worse
/// message and a reconcile cycle in between.
///
/// With no `--to`, the retained digest wins when the CR has one. For a
/// tag-following application a `targetRevision` rollback provably does not
/// roll the workload back — that is the whole of D9 — so preferring Git there
/// would be preferring the known-wrong answer. The presence of
/// `status.image.previous` is exactly the evidence that the digest has moved.
///
/// Pure: `cr` is the AppRafter Application CR, `argo` the Argo CD one.
pub(crate) fn classify_rollback_target(
    to: Option<&str>,
    cr: Option<&Value>,
    argo: &Value,
) -> Result<RollbackTarget> {
    if let Some(raw) = to {
        let raw = raw.trim();
        if let Some(hex) = raw.strip_prefix("sha256:") {
            if !is_canonical_sha256_hex(hex) {
                return Err(CliError::Other(format!(
                    "`--to {raw}` is not a canonical digest — expected \
                     `sha256:` followed by 64 lowercase hex characters."
                )));
            }
            return Ok(RollbackTarget::Digest(raw.to_string()));
        }
        if raw.contains(':') {
            return Err(CliError::Other(format!(
                "`--to {raw}` is neither an image digest nor a Git revision. \
                 Image digests are `sha256:<64 hex>`; Git refnames cannot \
                 contain `:`."
            )));
        }
        return Ok(RollbackTarget::GitRevision(raw.to_string()));
    }

    if let Some(prev) = cr
        .and_then(|c| c.pointer("/status/image/previous/resolved"))
        .and_then(Value::as_str)
    {
        return Ok(RollbackTarget::Digest(prev.to_string()));
    }
    Ok(RollbackTarget::GitRevision(
        pick_previous_revision(argo)?.to_string(),
    ))
}

/// `true` for exactly 64 lowercase hex characters.
///
/// Case matters: OCI digests are canonically lowercase, and an uppercase
/// variant would be a different string to every registry and to the
/// operator's own validator, so accepting it here would produce a pin the
/// operator then rejects.
fn is_canonical_sha256_hex(hex: &str) -> bool {
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Turn whatever the user typed into the full `repo@sha256:…` the operator's
/// repository-identity guard expects (ADR 0059).
///
/// A bare `sha256:…` carries no repository, and the annotation must, so the
/// repository is recovered from a reference the CR itself already recorded —
/// `status.image.previous.resolved`, then `status.image.resolved`, then
/// `status.image.tag`. If the digest matches none of them we error rather
/// than guess: a guessed repository is how a pin ends up pointing at somebody
/// else's image.
pub(crate) fn compose_pin_reference(digest: &str, cr: &Value) -> Result<String> {
    if digest.contains('@') || !digest.starts_with("sha256:") {
        // Already a full reference (or something we should not touch).
        return Ok(digest.to_string());
    }
    let image = cr.pointer("/status/image");
    let candidates = [
        image
            .and_then(|i| i.pointer("/previous/resolved"))
            .and_then(Value::as_str),
        image
            .and_then(|i| i.get("resolved"))
            .and_then(Value::as_str),
        image.and_then(|i| i.get("tag")).and_then(Value::as_str),
    ];
    for c in candidates.into_iter().flatten() {
        if let Some(repo) = reference_repository(c) {
            return Ok(format!("{repo}@{digest}"));
        }
    }
    Err(CliError::Other(format!(
        "cannot tell which repository `{digest}` belongs to — the application \
         has recorded no resolved image yet. Pass the full reference \
         (`--to <repo>@{digest}`)."
    )))
}

/// The `repo` part of `repo@sha256:…` or `repo:tag`.
fn reference_repository(reference: &str) -> Option<&str> {
    if let Some((repo, _)) = reference.split_once('@') {
        return Some(repo);
    }
    // `:` is only a tag separator when it comes after the last `/` — a
    // registry host may carry a port (`localhost:5000/x`).
    let last_slash = reference.rfind('/').map_or(0, |i| i + 1);
    match reference[last_slash..].rfind(':') {
        Some(i) => Some(&reference[..last_slash + i]),
        None => Some(reference),
    }
}

/// The SSA body that places (or, with `pin: None`, removes) the pin.
///
/// Carries NOTHING but the two annotations, which is what makes un-pinning
/// work: server-side apply prunes the keys this manager owns and no longer
/// lists. A body that also named a spec field would make `unpin` delete that
/// field too — the 2.10 egress defect at a new address.
pub(crate) fn pin_manifest(
    name: &str,
    namespace: &str,
    pin: Option<(&str, &str)>,
) -> serde_json::Value {
    let mut annotations = serde_json::Map::new();
    if let Some((reference, at)) = pin {
        annotations.insert("apprafter.io/image-pin".into(), json!(reference));
        annotations.insert("apprafter.io/image-pinned-at".into(), json!(at));
    }
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "Application",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "annotations": Value::Object(annotations),
        },
    })
}

/// `true` when an Argo-CD-shaped field manager already owns the pin
/// annotation on this CR (ADR 0059).
///
/// If the user's own manifest declares the annotation then Git owns it, the
/// next sync reverts our write, and reporting success would be a lie. Refuse
/// instead. Same shape as `platform::egress_field_appears_git_managed`, and
/// for the same reason.
pub(crate) fn pin_appears_git_managed(cr: &Value) -> bool {
    let Some(entries) = cr
        .pointer("/metadata/managedFields")
        .and_then(Value::as_array)
    else {
        return false;
    };
    entries.iter().any(|e| {
        let manager = e.get("manager").and_then(Value::as_str).unwrap_or("");
        let is_argo = manager.contains("argocd")
            || manager.contains("argo-cd")
            || manager.contains("application-controller");
        is_argo
            && e.pointer("/fieldsV1/f:metadata/f:annotations/f:apprafter.io~1image-pin")
                .is_some()
    })
}

/// The Git-revision rollback patch body.
///
/// Built with `json!` rather than `format!`: the old hand-spliced string
/// produced malformed JSON for any revision containing a quote or a
/// backslash, and a patch body is not a place to discover that.
pub(crate) fn rollback_patch_body(revision: &str) -> String {
    json!({ "spec": { "source": { "targetRevision": revision } } }).to_string()
}

pub(crate) fn pick_previous_revision(app: &Value) -> Result<&str> {
    let history = app
        .pointer("/status/history")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CliError::Other(
                "Application status.history is empty — no previous revision to roll back to. \
                 Pass `--to <rev>` explicitly."
                    .into(),
            )
        })?;
    if history.len() < 2 {
        return Err(CliError::Other(format!(
            "Application status.history contains {} entry — not enough to roll back to a \
             previous revision. Pass `--to <rev>` explicitly.",
            history.len()
        )));
    }
    history[history.len() - 2]
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::Other(
                "Previous status.history entry does not carry a `revision` field — corrupt CR?"
                    .into(),
            )
        })
}

/// Build the Argo CD `Application` CR manifest YAML. Pure fn
/// — tests cover the shape exhaustively without a cluster.
///
/// `destination_namespace` controls Argo CD's
/// `spec.destination.namespace` — the namespace whose existence
/// `CreateNamespace=true` in syncOptions guarantees, AND the
/// namespace Argo CD uses for namespaced resources that don't
/// declare one in their own metadata. Walk-fix #12 (v0.1.160)
/// detached this from the app name (which created an orphan
/// destination namespace mismatched with the manifest's
/// `metadata.namespace`); operators now pass `--namespace` (or
/// accept the `apprafter` default) when registering user apps.
///
/// `logical_name` is the user-facing app name (the cwd basename /
/// `--name`); it is stamped as the `apprafter.io/application` label
/// so every per-environment deployment of one app shares a common
/// selector. `argo_app_name` is the Argo CD `Application`'s
/// `metadata.name` — `<logical_name>-<env>` for an env deploy, or
/// `<logical_name>` for a base-only deploy (ADR 0044 / 2.9).
///
/// When `env` is `Some(e)`, the manifest carries the
/// `apprafter.io/environment` label and a `spec.source.plugin.env`
/// entry `{ name: APPRAFTER_APP_ENV, value: e }` — the cue-cmp
/// sidecar reads `APPRAFTER_APP_ENV` and injects `spec.environment`
/// into the rendered CR, so the operator unifies the chosen
/// `spec.environments.<env>` override onto base. A base-only deploy
/// (`env: None`) omits both, leaving the CMP at its prod-base
/// default.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_application_manifest(
    logical_name: &str,
    argo_app_name: &str,
    repo_url: &str,
    target_revision: &str,
    path: &str,
    project: &str,
    destination_namespace: &str,
    env: Option<&str>,
) -> Value {
    let normalised_path = normalise_argocd_source_path(path);
    let mut manifest = json!({
        "apiVersion": "argoproj.io/v1alpha1",
        "kind": "Application",
        "metadata": {
            "name": argo_app_name,
            "namespace": ARGOCD_NAMESPACE,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
                "apprafter.io/application": logical_name,
            },
            "annotations": {
                APPRAFTER_SOURCE_ANNOTATION: "cli",
            },
            // Argo CD owns cascade deletion of the resources it syncs (the
            // AppRafter Application CR), whose ownerReference then GC's the
            // operator's Deployment. Without this finalizer, deleting the
            // Argo Application orphans the synced CR + workload.
            "finalizers": [ARGOCD_CASCADE_FINALIZER],
        },
        "spec": {
            "project": project,
            "source": {
                "repoURL": repo_url,
                "path": normalised_path,
                "targetRevision": target_revision,
            },
            "destination": {
                "server": "https://kubernetes.default.svc",
                "namespace": destination_namespace,
            },
            "syncPolicy": {
                "automated": {
                    "prune": true,
                    "selfHeal": true,
                },
                "syncOptions": [
                    "CreateNamespace=true",
                    "ServerSideApply=true",
                ],
            },
        },
    });

    // Per-environment deploy (ADR 0044): label the env + hand the CMP
    // the env name via a config-management-plugin env var. The cue-cmp
    // sidecar reads `APPRAFTER_APP_ENV` and stamps `spec.environment`
    // onto the rendered CR so the operator unifies the matching
    // `spec.environments.<env>` override onto base.
    if let Some(e) = env {
        manifest["metadata"]["labels"]["apprafter.io/environment"] = json!(e);
        manifest["spec"]["source"]["plugin"] = json!({
            "env": [
                { "name": "APPRAFTER_APP_ENV", "value": e },
            ],
        });
    }

    manifest
}

/// Pure helper — translate AppRafter-side path conventions
/// (`/`, empty, leading `/`) to Argo CD's `spec.source.path`
/// shape (relative-to-repo-root). Walk-fix #2 post-Part-3b:
/// our wizard documented `/` as the "whole repo" idiom, but
/// Argo CD's apiserver validates `spec.source.path` as a
/// relative path and rejects absolute values with `app path is
/// absolute`. Map at the boundary so the wizard UX stays
/// familiar and the stored CR remains valid.
pub(crate) fn normalise_argocd_source_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return ".".to_string();
    }
    let stripped = trimmed.trim_start_matches('/');
    if stripped.is_empty() {
        ".".to_string()
    } else {
        stripped.to_string()
    }
}

fn apply_application_manifest(manifest: &Value, kubeconfig_path: &Path) -> Result<()> {
    use std::io::Write as _;
    let mut file = tempfile::Builder::new()
        .prefix("apprafter-app-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| CliError::Other(format!("create app manifest tempfile: {e}")))?;
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialise app manifest: {e}")))?;
    file.write_all(&body)
        .map_err(|e| CliError::Other(format!("write app manifest tempfile: {e}")))?;
    file.flush()
        .map_err(|e| CliError::Other(format!("flush app manifest tempfile: {e}")))?;

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

fn app_row(app: &Value) -> AppRow {
    let name = app
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let namespace = app
        .pointer("/spec/destination/namespace")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let repo = display_repo_url(
        app.pointer("/spec/source/repoURL")
            .and_then(Value::as_str)
            .unwrap_or("?"),
    );
    let sync = app
        .pointer("/status/sync/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    // ADR 0062: fold the verdict over every workload in the bundle, so a
    // minority `Degraded` cannot hide behind a majority `Healthy` and a
    // rollback pin cannot be masked by a `Progressing` sibling. Falls
    // back to the registration's own health when it tracks no AppRafter
    // workload — an unsynced registration, or a raw-YAML app under
    // `--all-managed`.
    let health = workload_health_cell(app).unwrap_or_else(|| {
        app.pointer("/status/health/status")
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
            .to_string()
    });
    // ADR 0044 (1.83j): which environment this deployment targets. Reuses
    // the same label → status → `(base)` resolution as the `app status`
    // per-env aggregation so the two surfaces never disagree.
    let env = deployment_environment(app);
    // ADR 0062: the row stands for a BUNDLE of 1..N workloads, so it has
    // to say how many. No extra cluster read — `list` already fetched
    // `status.resources[]`.
    let workloads = workload_count_cell(app);
    AppRow {
        name,
        env,
        namespace,
        workloads,
        repo,
        sync,
        health,
    }
}

/// Pure formatter — tests drive it with a fixture JSON.
/// The Argo CD summary lines for one Application — name, project, repo,
/// revision, path, destination, ENVIRONMENT, sync, health. Pure +
/// testable; the `environment:` line closes a 2.9 gap where `app status`
/// surfaced the env only in the multi-deployment header, never in the
/// single-deployment detail block.
fn status_detail_lines(app: &Value) -> Vec<String> {
    let name = app
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let project = app
        .pointer("/spec/project")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let repo = app
        .pointer("/spec/source/repoURL")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let revision = app
        .pointer("/spec/source/targetRevision")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let path = app
        .pointer("/spec/source/path")
        .and_then(Value::as_str)
        .unwrap_or("/");
    let dest_ns = app
        .pointer("/spec/destination/namespace")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let sync = app
        .pointer("/status/sync/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let health = app
        .pointer("/status/health/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let environment = deployment_environment(app);

    let mut out = vec![
        format!("Application {ARGOCD_NAMESPACE}/{name}"),
        format!("  project:       {project}"),
        format!("  repo:          {repo}"),
        format!("  revision:      {revision}"),
        format!("  path:          {path}"),
        format!("  destination:   {dest_ns}"),
        format!("  environment:   {environment}"),
        format!("  sync state:    {sync}"),
        format!("  health:        {health}"),
    ];
    // Directly under `health:`, because a condition is the REASON the two
    // lines above read the way they do — a 2.27a bundle refusal leaves the
    // sync `Unknown` and says why only here.
    out.extend(condition_lines(app));
    out
}

/// Pure helper — `status.conditions[]` of one Argo CD `Application`,
/// rendered as the tail of [`status_detail_lines`].
///
/// Argo CD writes a condition whenever it cannot do what the registration
/// asked: a manifest-generation failure becomes `ComparisonError` carrying
/// whatever the generate command put on stderr. The 2.27a cue-cmp refusals
/// (contradicting namespaces, a duplicate `(namespace, name)`, a
/// package-scope manifest mixed with named wrappers, divergent
/// `spec.environment`, two packages under one path) land here verbatim —
/// and *only* here, since the sync itself just goes `Unknown`.
///
/// **Folding rule: the first line rides the `<Type>:` line, every
/// remaining line is re-indented underneath it, and nothing is dropped
/// or counted.** The sidecar's first line is a self-contained summary
/// because Argo CD truncates it onto a UI tile, but a terminal has no
/// tile: what the reader must act on — *which* workloads disagree, what
/// each declared, and the remedy — lives in the detail block below that
/// summary. Printing the summary alone, or the summary plus an "(+12
/// more lines)" count, would re-create the trip to the Argo CD UI that
/// surfacing the condition here exists to remove.
///
/// The one thing dropped is the transport framing ahead of our own
/// [`CUE_CMP_SENTINEL`] — see [`strip_transport_prefix`].
///
/// Two details the fold does not take liberties with. The message is
/// never re-wrapped — `bundle_refuse` writes a column-aligned table of
/// offending workloads, and wrapping would destroy it. And interior
/// blank lines are kept (runs collapsed to one, leading/trailing
/// dropped) — measured on a real namespace-divergence refusal, deleting
/// them fuses the offending-workload table, the "why" paragraph and the
/// "how to fix it" paragraph into one wall, which is exactly the part a
/// red-deploy reader skims.
///
/// Every condition is rendered, not just the first: Argo CD reports them
/// as a set, and a `ComparisonError` sitting next to an
/// `OrphanedResourceWarning` is two different things to fix.
///
/// Returns an empty `Vec` when there are no conditions — a healthy app
/// pays nothing for this block, not even a header (pinned by the N=1
/// golden detail block).
///
/// Deliberately unstyled: [`status_detail_lines`] colours nothing today,
/// and `RenderedLine`'s doctrine is that line assembly must not inspect
/// the terminal. Painting these `warn` means moving that whole function
/// to `Vec<RenderedLine>` — its callers and the N=1 golden included —
/// which is a bigger change than surfacing the data, so it is not made
/// here.
fn condition_lines(app: &Value) -> Vec<String> {
    let conditions = app
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut out = Vec::new();
    for cond in conditions {
        let kind = cond
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");
        let message = cond.get("message").and_then(Value::as_str).unwrap_or("");
        let body = fold_condition_message(strip_transport_prefix(message));
        if out.is_empty() {
            out.push(String::new());
            out.push("Argo CD conditions:".to_string());
        }
        let mut body = body.into_iter();
        match body.next() {
            Some(first) => out.push(format!("  {kind}: {first}")),
            // A condition with no message is still worth naming: the type
            // alone tells the reader which surface to look at.
            None => out.push(format!("  {kind}")),
        }
        out.extend(body.map(|l| {
            // An interior blank stays blank rather than becoming four
            // spaces of trailing whitespace.
            if l.is_empty() {
                l
            } else {
                format!("    {l}")
            }
        }));
    }
    out
}

/// The marker `argocd-cue-cmp/entrypoint.sh` opens every one of its own
/// stderr lines with. Ours, not Argo CD's — which is the whole reason
/// [`strip_transport_prefix`] is safe.
const CUE_CMP_SENTINEL: &str = "::cue-cmp::";

/// Drop whatever precedes the first [`CUE_CMP_SENTINEL`] in a condition
/// message; return the message untouched when there is none.
///
/// Argo CD delivers a CMP failure as a Go error chain with our sidecar's
/// stderr appended to the end of it, so the rendered first line opens
/// with ~180 characters of `Failed to load target state: failed to
/// generate manifest for source 1 of 1: rpc error: code = Unknown desc =
/// … exit status 1:` before the one sentence `bundle_refuse` wrote to BE
/// the whole finding. First line is what a reader sees first, and that
/// made it the least useful line in the block.
///
/// **This deliberately does not parse Argo CD's wrapper.** It searches
/// for a marker WE emit, so Argo CD may reshape, translate or extend its
/// error chain freely and this keeps working — there is no shape here to
/// break. And a condition Argo CD raises on its own (`SyncError`,
/// `OrphanedResourceWarning`, `InvalidSpecError`, …) carries no sentinel
/// and so falls through byte-for-byte, which is the behaviour that must
/// not regress: those messages have no marker and no transport framing,
/// and are entirely their own content.
///
/// The cut is the FIRST occurrence in the whole message rather than a
/// first-line-only match, because the sidecar can write several sentinel
/// lines before the fatal one (the nested-manifest-directory notices at
/// `entrypoint.sh:248`) and every one of them is content worth keeping;
/// anything ahead of the earliest of them is framing by construction,
/// since a sentinel is the first thing the sidecar writes.
///
/// The sentinel stays in the output. It names which layer refused — the
/// manifest renderer, not the apiserver, not the operator — and it is
/// greppable in a pasted terminal scrollback.
fn strip_transport_prefix(message: &str) -> &str {
    match message.find(CUE_CMP_SENTINEL) {
        Some(at) => &message[at..],
        None => message,
    }
}

/// The line-level half of [`condition_lines`]' folding rule: trailing
/// whitespace stripped, leading and trailing blank lines dropped, runs of
/// interior blanks collapsed to one. Returns the message's own lines,
/// un-indented — the caller owns the indent, since the first one is
/// spliced onto the `<Type>:` line and the rest sit under it.
fn fold_condition_message(message: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in message.lines().map(str::trim_end) {
        let blank = line.is_empty();
        // Leading blanks never open the block; a run never widens it.
        if blank && out.last().is_none_or(String::is_empty) {
            continue;
        }
        out.push(line.to_string());
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// Pure helper — the `Recent revisions` tail of the status block.
/// Extracted from [`print_status`]. Argo CD appends to
/// `status.history`, so the NEWEST entries are last: the render reverses
/// and caps at three, and an app with no history renders no header at
/// all rather than an empty one.
pub(crate) fn recent_revision_lines(app: &Value) -> Vec<String> {
    let Some(revs) = app.pointer("/status/history").and_then(Value::as_array) else {
        return vec![];
    };
    if revs.is_empty() {
        return vec![];
    }
    let mut out = vec![
        String::new(),
        format!("Recent revisions (last {}):", revs.len().min(3)),
    ];
    for rev in revs.iter().rev().take(3) {
        let id = rev.get("id").and_then(Value::as_u64).unwrap_or(0);
        let rev_str = rev.get("revision").and_then(Value::as_str).unwrap_or("?");
        let deployed_at = rev.get("deployedAt").and_then(Value::as_str).unwrap_or("?");
        out.push(format!("  #{id:>3} {rev_str:<10} {deployed_at}"));
    }
    out
}

fn print_status(app: &Value) {
    for line in status_detail_lines(app) {
        println!("{line}");
    }
    for line in recent_revision_lines(app) {
        println!("{line}");
    }
}

/// Pure helper — render the AppRafter Application CR's
/// `status.image.{tag,resolved,resolvedAt}` (ADR 0040 image
/// tag→digest resolution) into a single status line, e.g.:
///
/// ```text
///   image:         ghcr.io/acme/web:latest -> @sha256:abc (resolved 5m ago)
/// ```
///
/// Returns `None` when `status.image` is absent (resolution
/// opted out via `imagePolicy.resolve: off`, or the operator
/// has not yet completed a first resolution) — the caller then
/// omits the line entirely. `now` is injected so the relative
/// "resolved <age> ago" suffix is deterministic in tests,
/// mirroring `format_pod_age`'s clock seam. The age suffix is
/// dropped when `resolvedAt` is missing or unparseable.
/// One problem entry that survived the render filter, with its age resolved.
///
/// Borrowed from the CR rather than copied: every consumer only formats it.
pub(crate) struct LiveProblem<'a> {
    pub(crate) reason: &'a str,
    pub(crate) message: &'a str,
    pub(crate) last_seen: &'a str,
    /// Seconds since `last_seen`, already known to be within the horizon.
    pub(crate) age: i64,
    pub(crate) count: i64,
}

impl LiveProblem<'_> {
    /// `now` while the entry is fresh enough that the operator would have
    /// refreshed it, else `<age> ago`.
    pub(crate) fn when(&self, now: &chrono::DateTime<chrono::Utc>) -> String {
        if self.age <= PROBLEM_LIVE_SECS {
            "now".to_string()
        } else {
            format!("{} ago", format_pod_age(self.last_seen, now))
        }
    }

    /// `, 7x` for a repeated failure, empty for a single sighting.
    pub(crate) fn times(&self) -> String {
        if self.count > 1 {
            format!(", {}x", self.count)
        } else {
            String::new()
        }
    }
}

/// THE render filter for `status.recentProblems`, and the only one.
///
/// Extracted so `app status` (per application) and `platform status` (the
/// cluster roll-up) cannot drift apart. If the roll-up re-derived these three
/// rules, the first retune of the horizon would make it name an application
/// whose own `app status` prints nothing — which is the worst failure available
/// to a surface whose entire job is to send you to that command.
///
/// The three rules, and why each is a rule rather than a detail:
///  * absent or non-array `status.recentProblems` — nothing to say;
///  * an unparseable `lastSeen` is SKIPPED. Note this is deliberately the
///    opposite of `operator_core::problems::age_out`, which KEEPS one: the
///    operator must not silently drop state it cannot date, but a reader must
///    not be shown an undateable claim;
///  * older than [`PROBLEM_RENDER_HORIZON_SECS`] is dropped even though the
///    operator still carries it — a surface listing what was ONCE broken is one
///    people learn not to read.
pub(crate) fn live_problems<'a>(
    cr: &'a Value,
    now: &chrono::DateTime<chrono::Utc>,
) -> Vec<LiveProblem<'a>> {
    let Some(rows) = cr
        .pointer("/status/recentProblems")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for r in rows {
        let last = r.get("lastSeen").and_then(Value::as_str).unwrap_or("");
        let Some(age) = age_secs(last, now) else {
            continue;
        };
        if age > PROBLEM_RENDER_HORIZON_SECS {
            continue;
        }
        out.push(LiveProblem {
            reason: r.get("reason").and_then(Value::as_str).unwrap_or("Problem"),
            message: r.get("message").and_then(Value::as_str).unwrap_or(""),
            last_seen: last,
            age,
            count: r.get("count").and_then(Value::as_i64).unwrap_or(1),
        });
    }
    out
}

/// How the render horizon reads to a human, derived from the constant so the
/// number lives in exactly one place.
pub(crate) fn problem_window_label() -> String {
    format!("last {}h", PROBLEM_RENDER_HORIZON_SECS / 3600)
}

/// Undesigned reconcile failures, as `app status` prints them (2.22h / D16).
///
/// Empty when there is nothing to report — deliberately, and not a
/// `Problems: none` line. A section that is always present is one people
/// learn to skip, and then it is worse than absent. (The cluster roll-up in
/// `platform status` makes the OPPOSITE call, for a reason stated there: it is
/// asked "is anything wrong?", and to that question silence is not an answer.)
///
/// Entries older than [`PROBLEM_RENDER_HORIZON_SECS`] are not printed at all,
/// even though the operator may still be carrying them. That is the guard
/// against the failure mode the defect named: a surface listing what was once
/// broken is one nobody reads.
pub(crate) fn format_problem_lines(cr: &Value, now: &chrono::DateTime<chrono::Utc>) -> Vec<String> {
    let mut out: Vec<String> = live_problems(cr, now)
        .iter()
        .map(|p| {
            format!(
                "  problem:       {} ({}{}): {}",
                p.reason,
                p.when(now),
                p.times(),
                p.message
            )
        })
        .collect();
    if !out.is_empty() {
        out.push(
            "                 these are reconcile failures the platform could not act on; \
             they clear on their own once they stop"
                .to_string(),
        );
    }
    out
}

/// How recent a problem must be to read as happening NOW rather than as a
/// timestamp. Must exceed the operator's refresh floor, or a live failure
/// would spend most of every window reading as stale.
const PROBLEM_LIVE_SECS: i64 = 20 * 60;

/// Beyond this, a problem is not printed even if the object still carries it.
const PROBLEM_RENDER_HORIZON_SECS: i64 = 24 * 60 * 60;

/// Seconds since an RFC3339 stamp, or `None` if it will not parse.
fn age_secs(stamp: &str, now: &chrono::DateTime<chrono::Utc>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|t| (*now - t.with_timezone(&chrono::Utc)).num_seconds())
}

/// The "this application is held at a digest" line (ADR 0059).
///
/// `None` when the application is not pinned. Reads `status.image.pinned`,
/// which the operator writes only when the pin is HONOURED — a pin the
/// operator rejected must not be reported here, because a status line saying
/// "held at X" while the workload follows the tag is worse than silence.
///
/// This exists because the pin is invisible to Git, permanently: a reader of
/// the repository sees `:latest` and cannot know the cluster is deliberately
/// holding an older digest. So this is not decoration — it is the only place
/// the truth exists, which is also why it names the way out.
/// The `Ready=False` reason and its message, for an application that is not
/// Ready — `None` while it is.
///
/// # Why this exists
///
/// Ledger entry D7 is titled "the CLI cannot answer the question its own error
/// asks", and 2.22c answered only half of it. The operator's diagnostic became
/// genuinely good — `env STRIPE_KEY → secret "appsecret/token": Secret
/// "appsecret" exists in namespace "demo" but carries no key "token" (it
/// carries: api_key, url)` names the cause, the namespace, and the keys that
/// ARE there, which is what separates "no Secret", "wrong namespace" and
/// "wrong spelling" without a second command.
///
/// It then went nowhere a user looks. `app status` printed
/// `AppRafter phase: EnvSecretMissing` and stopped; the message lived in
/// `status.conditions[type=Ready].message`, and no CLI surface read it — not
/// `app status`, not `app list`, not `platform status`. `secret list` cannot
/// stand in for it either: it renders key names out of the SealedSecret's own
/// `spec.encryptedData` without decrypting anything or reading the resulting
/// Secret, so it answers what was DECLARED, never whether it materialised.
///
/// So the fix that made the answer available left the reader exactly where
/// they started — at `kubectl get application -o yaml`. This prints it.
///
/// Deliberately not restricted to `EnvSecretMissing`: every reason the
/// operator sets carries a message written to be read, and a status command
/// that shows a failure's NAME while withholding its EXPLANATION is the defect
/// D7 describes, whatever the reason happens to be.
pub(crate) fn format_not_ready_line(cr: &Value) -> Option<String> {
    let conds = cr.pointer("/status/conditions")?.as_array()?;
    let ready = conds
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some("Ready"))?;
    if ready.get("status").and_then(Value::as_str) == Some("True") {
        return None;
    }
    let message = ready.get("message").and_then(Value::as_str)?;
    if message.is_empty() {
        return None;
    }
    // The operator joins per-ref diagnostics with "; " into one line, which is
    // unreadable at three refs. Split it back out, one per line, under a
    // heading that names the reason.
    let reason = ready
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("NotReady");
    let mut line = format!("  {reason}:");
    let parts: Vec<&str> = message.split("; ").filter(|p| !p.is_empty()).collect();
    if parts.len() <= 1 {
        line.push_str(&format!(" {message}"));
    } else {
        for p in parts {
            line.push_str(&format!("\n                   {p}"));
        }
    }
    Some(line)
}

pub(crate) fn format_pin_line(cr: &Value) -> Option<String> {
    let pinned = cr.pointer("/status/image/pinned")?;
    let reference = pinned.get("resolved").and_then(Value::as_str)?;
    let name = cr
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("<app>");
    let tag = cr
        .pointer("/status/image/tag")
        .and_then(Value::as_str)
        .unwrap_or("its tag");
    let mut line = format!(
        "  pinned:        held at {reference} — NOT following {tag}\n\
         \x20                resume with `apprafter app unpin {name}`"
    );
    if let Some(at) = pinned.get("at").and_then(Value::as_str) {
        line.push_str(&format!("\n                 pinned at {at}"));
    }
    Some(line)
}

pub(crate) fn format_image_line(cr: &Value, now: &chrono::DateTime<chrono::Utc>) -> Option<String> {
    let image = cr.pointer("/status/image")?;
    let tag = image.get("tag").and_then(Value::as_str).unwrap_or("?");
    let resolved = image.get("resolved").and_then(Value::as_str);
    let resolved_at = image.get("resolvedAt").and_then(Value::as_str);

    // Show only the `@sha256:…` digest portion of the resolved
    // reference — the repo prefix duplicates the tag's, so the
    // line reads `tag -> @sha256:…`.
    let digest_suffix = resolved.map(|r| match r.split_once('@') {
        Some((_, digest)) => format!("@{digest}"),
        None => r.to_string(),
    });

    let mut line = match digest_suffix {
        Some(digest) => format!("  image:         {tag} -> {digest}"),
        None => format!("  image:         {tag}"),
    };

    if let Some(ts) = resolved_at {
        // `format_pod_age` echoes the input verbatim on a parse
        // failure; only append the "(resolved … ago)" suffix when
        // the timestamp actually parsed to an age.
        if chrono::DateTime::parse_from_rfc3339(ts).is_ok() {
            let age = format_pod_age(ts, now);
            line.push_str(&format!(" (resolved {age} ago)"));
        }
    }

    Some(line)
}

/// Pure helper — format the VPA recommendation line from
/// `Application.status.recommendedResources` for display in `app status`.
///
/// Returns `None` when the field is absent (VPA not installed, first
/// reconcile not yet done, or autoscale.mode == off with no prior run).
/// When `uncappedTarget` differs from `target` on a resource (i.e. VPA
/// would recommend higher but a VPA `ResourcePolicy.maxAllowed` cap is
/// in effect), the line appends a `· uncapped <v> — raise limits.memory`
/// hint. When `notApplied` is present (recommendation recorded but the
/// updater was told not to apply it), the reason is appended.
pub(crate) fn format_recommendation_line(cr: &Value) -> Option<String> {
    let reco = cr.pointer("/status/recommendedResources")?;
    let target = reco.pointer("/recommendation/target")?;

    // Collect resource entries from the `target` object.
    let target_obj = target.as_object()?;

    let uncapped = reco.pointer("/recommendation/uncappedTarget");
    let not_applied = reco.get("notApplied").and_then(Value::as_str);

    let mut parts: Vec<String> = Vec::new();
    for (resource, value) in target_obj {
        let target_val = value.as_str().unwrap_or("?");

        // Build the base entry: "limits.memory: 512Mi"
        let display_key = match resource.as_str() {
            "memory" => "limits.memory".to_string(),
            "cpu" => "limits.cpu".to_string(),
            other => other.to_string(),
        };
        let mut entry = format!("{display_key}: {target_val}");

        // Append uncapped hint when it differs from the capped target.
        if let Some(uncapped_val) = uncapped
            .and_then(|u| u.get(resource))
            .and_then(Value::as_str)
        {
            if uncapped_val != target_val {
                entry.push_str(&format!(
                    " · uncapped {uncapped_val} — raise `resources.limits.memory`"
                ));
            }
        }

        parts.push(entry);
    }

    if parts.is_empty() {
        return None;
    }

    let mut line = format!("  VPA reco:      {}", parts.join(", "));

    if let Some(reason) = not_applied {
        line.push_str(&format!(" · not applied — {reason}"));
    }

    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_git_url_strips_dotgit_suffix() {
        assert_eq!(
            normalise_git_url("https://github.com/foo/bar.git"),
            "https://github.com/foo/bar"
        );
    }

    #[test]
    fn normalise_git_url_converts_scp_style_to_https() {
        // SCP-style (git@host:org/repo) — the form `git clone`
        // accepts but Argo CD does NOT (the repo-server's git
        // backend wants a scheme prefix). Conversion makes the
        // URL Argo-CD-friendly without operator gymnastics.
        assert_eq!(
            normalise_git_url("git@github.com:foo/bar.git"),
            "https://github.com/foo/bar"
        );
        assert_eq!(
            normalise_git_url("git@gitlab.com:acme/app"),
            "https://gitlab.com/acme/app"
        );
    }

    #[test]
    fn normalise_git_url_strips_ssh_scheme() {
        assert_eq!(
            normalise_git_url("ssh://git@github.com/foo/bar.git"),
            "https://github.com/foo/bar"
        );
    }

    #[test]
    fn normalise_git_url_passes_through_https() {
        // The common Argo-CD-native shape — must round-trip
        // identically except for the optional `.git` strip.
        assert_eq!(
            normalise_git_url("https://gitlab.com/acme/platform"),
            "https://gitlab.com/acme/platform"
        );
    }

    #[test]
    fn derive_app_name_takes_last_path_segment() {
        assert_eq!(derive_app_name("https://github.com/foo/bar-baz"), "bar-baz");
        assert_eq!(derive_app_name("https://gitlab.com/acme/My_App"), "my-app");
    }

    #[test]
    fn derive_app_name_strips_invalid_chars() {
        // Underscores / dots → dashes; whole thing lowercased.
        // Edge case: a URL ending in a dot or only non-alnum
        // chars defaults to "app".
        assert_eq!(
            derive_app_name("https://x.com/MIXED.case_v2"),
            "mixed-case-v2"
        );
        assert_eq!(derive_app_name("https://x.com/..."), "app");
    }

    #[test]
    fn validate_dns_1123_accepts_well_formed_names() {
        assert!(validate_dns_1123("my-app").is_ok());
        assert!(validate_dns_1123("app1").is_ok());
        assert!(validate_dns_1123("a").is_ok());
    }

    #[test]
    fn validate_dns_1123_rejects_uppercase_underscore_leading_dash() {
        assert!(validate_dns_1123("MyApp").is_err());
        assert!(validate_dns_1123("my_app").is_err());
        assert!(validate_dns_1123("-leading").is_err());
        assert!(validate_dns_1123("trailing-").is_err());
        assert!(validate_dns_1123("").is_err());
    }

    #[test]
    fn rejects_env_with_underscore() {
        // ADR 0044 (2.9): `app add --env <e>` folds the env into the Argo CD
        // Application name `<name>-<e>`, so `add()` runs `validate_dns_1123(e)`
        // before that name reaches `kubectl apply`. Guard the env-value path:
        // an underscore env must be rejected client-side (clear message)
        // rather than degrading into a cryptic RFC-1123 apiserver error.
        assert!(validate_dns_1123("foo_bar").is_err());
        assert!(validate_dns_1123("dev").is_ok());
        assert!(validate_dns_1123("prod").is_ok());
    }

    #[test]
    fn argo_app_name_for_resolves_per_env_vs_base() {
        // ADR 0044 (2.9): per-env-aware `logs` / `rollback` target
        // `<name>-<env>` with `--env`, else the bare `<name>` — matching
        // the `<name>-<env>` shape `add` / `remove` stamp.
        assert_eq!(argo_app_name_for("web", Some("dev")), "web-dev");
        assert_eq!(argo_app_name_for("web", Some("prod")), "web-prod");
        assert_eq!(argo_app_name_for("web", None), "web");
    }

    #[test]
    fn per_env_guidance_message_lists_envs_and_points_at_flag() {
        // The no-`--env`-but-deployed-per-env guidance lists the available
        // environments and points the user at `--env`.
        let msg = per_env_guidance_message("web", &["dev".into(), "prod".into()]);
        assert!(msg.contains("'web' is deployed per environment (dev, prod)"));
        assert!(msg.contains("Pass `--env <env>` to target one."));
        // Empty env list (labels absent) falls back to "multiple".
        let generic = per_env_guidance_message("web", &[]);
        assert!(generic.contains("(multiple)"));
    }

    #[test]
    fn single_deployment_auto_resolves_logical_name() {
        // ONE env deployment ⇒ the logical name is unambiguous, so commands
        // (status/logs/rollback/open) resolve it WITHOUT requiring `--env`.
        let one = vec![serde_json::json!({
            "metadata": { "name": "web-prod", "labels": { "apprafter.io/environment": "prod" } }
        })];
        let (app, argo_name) = single_deployment_or_guidance("web", one).unwrap();
        assert_eq!(argo_name, "web-prod");
        assert_eq!(
            app.pointer("/metadata/name").and_then(Value::as_str),
            Some("web-prod")
        );
    }

    #[test]
    fn multiple_deployments_require_env_flag() {
        // TWO+ deployments ⇒ ambiguous, guide the user to `--env`.
        let two = vec![
            serde_json::json!({
                "metadata": { "name": "web-dev", "labels": { "apprafter.io/environment": "dev" } }
            }),
            serde_json::json!({
                "metadata": { "name": "web-prod", "labels": { "apprafter.io/environment": "prod" } }
            }),
        ];
        let err = single_deployment_or_guidance("web", two).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--env"), "must guide to --env; got {msg:?}");
        assert!(
            msg.contains("dev") && msg.contains("prod"),
            "must list envs; got {msg:?}"
        );
    }

    #[test]
    fn sanitise_for_dns_1123_lowercases_and_replaces_specials() {
        // Walk-fix Part 3b — cwd basename → DNS-1123-safe
        // suggestion for step 0's "name" default. Operators
        // see this in the wizard's name prompt; should be
        // valid DNS-1123 already so the wizard's prefill
        // doesn't fail validation.
        assert_eq!(sanitise_for_dns_1123("MyProject"), "myproject");
        assert_eq!(sanitise_for_dns_1123("acme_app"), "acme-app");
        assert_eq!(sanitise_for_dns_1123("Hello World"), "hello-world");
        assert_eq!(sanitise_for_dns_1123("payments"), "payments");
    }

    #[test]
    fn sanitise_for_dns_1123_collapses_repeated_dashes() {
        // `foo__bar` → `foo-bar`, NOT `foo--bar` (which
        // CUE / kube accept but is ugly).
        assert_eq!(sanitise_for_dns_1123("foo__bar"), "foo-bar");
        assert_eq!(sanitise_for_dns_1123("a  b  c"), "a-b-c");
        assert_eq!(sanitise_for_dns_1123("x--y--z"), "x-y-z");
    }

    #[test]
    fn sanitise_for_dns_1123_trims_leading_trailing_dashes() {
        assert_eq!(sanitise_for_dns_1123("-leading"), "leading");
        assert_eq!(sanitise_for_dns_1123("trailing-"), "trailing");
        assert_eq!(sanitise_for_dns_1123("__bracketed__"), "bracketed");
    }

    #[test]
    fn sanitise_for_dns_1123_empty_input_falls_back_to_app() {
        // operator's cwd is `/` or a garbage-only name —
        // suggestion must still be valid DNS-1123 so the
        // wizard prefill doesn't fail.
        assert_eq!(sanitise_for_dns_1123(""), "app");
        assert_eq!(sanitise_for_dns_1123("___"), "app");
        assert_eq!(sanitise_for_dns_1123("..."), "app");
    }

    #[test]
    fn sanitise_for_dns_1123_truncates_at_63_characters() {
        // DNS-1123 length limit. Trim trailing dash after
        // cut to avoid landing on an invalid suffix.
        let long = "a".repeat(70);
        let out = sanitise_for_dns_1123(&long);
        assert_eq!(out.len(), 63);
        assert!(!out.ends_with('-'));
    }

    #[test]
    fn normalise_argocd_source_path_translates_slash_to_dot() {
        // Walk-fix #2 post-Part-3b — Argo CD rejects "/"
        // (absolute) but the wizard's documented default is
        // "/". Map at the manifest-build boundary.
        assert_eq!(normalise_argocd_source_path("/"), ".");
        assert_eq!(normalise_argocd_source_path(""), ".");
        assert_eq!(normalise_argocd_source_path("   "), ".");
    }

    #[test]
    fn normalise_argocd_source_path_strips_leading_slash() {
        // Operator typing `--path /landing/web` is a common
        // muscle-memory shape; translate to relative-form
        // automatically.
        assert_eq!(normalise_argocd_source_path("/landing/web"), "landing/web");
        assert_eq!(normalise_argocd_source_path("landing/web"), "landing/web");
    }

    #[test]
    fn normalise_argocd_source_path_preserves_dot_and_relative_paths() {
        assert_eq!(normalise_argocd_source_path("."), ".");
        assert_eq!(normalise_argocd_source_path("./apps/x"), "./apps/x");
        assert_eq!(normalise_argocd_source_path("apps/x"), "apps/x");
    }

    #[test]
    fn build_application_manifest_normalises_slash_path_to_dot() {
        // Regression guard for the walk-fix — the operator's
        // procvue-landing CR had `path: "/"` and Argo CD
        // surfaced "app path is absolute". build_application_
        // manifest must emit "." whenever the wizard hands us
        // "/".
        let m = build_application_manifest(
            "test-app",
            "test-app",
            "https://github.com/foo/bar",
            "main",
            "/",
            "apps",
            "apprafter",
            None,
        );
        assert_eq!(
            m.pointer("/spec/source/path").and_then(Value::as_str),
            Some(".")
        );
    }

    #[test]
    fn derive_creds_suggestion_github_full_shape() {
        // Walk-fix #2: GitHub URL → org-level prefix, kebab
        // cred name, and pre-filled PAT-creation URL with the
        // required scopes.
        let s = derive_creds_suggestion("https://github.com/ProcVue/landing")
            .expect("github URL must yield a suggestion");
        assert_eq!(s.suggested_name, "github-procvue");
        assert_eq!(s.url_prefix, "https://github.com/ProcVue");
        let pat_url = s.pat_creation_url.expect("github PAT URL");
        assert!(pat_url.starts_with("https://github.com/settings/tokens/new"));
        assert!(pat_url.contains("scopes=repo,read:packages"));
        assert!(pat_url.contains("AppRafter%20procvue"));
    }

    #[test]
    fn derive_creds_suggestion_gitlab_full_shape() {
        let s = derive_creds_suggestion("https://gitlab.com/acme/app")
            .expect("gitlab URL must yield a suggestion");
        assert_eq!(s.suggested_name, "gitlab-acme");
        assert_eq!(s.url_prefix, "https://gitlab.com/acme");
        let pat_url = s.pat_creation_url.expect("gitlab PAT URL");
        assert!(
            pat_url.starts_with("https://gitlab.com/-/user_settings/personal_access_tokens"),
            "{pat_url}"
        );
        assert!(pat_url.contains("read_repository"));
        assert!(pat_url.contains("read_registry"));
    }

    #[test]
    fn derive_creds_suggestion_unknown_provider_omits_pat_url() {
        // Self-hosted Gitea / Forgejo / etc. — we don't know
        // their PAT pages. Suggestion still carries name +
        // prefix; pat_creation_url is None so hint falls
        // back to the one-liner.
        let s = derive_creds_suggestion("https://gitea.example.com/team/app")
            .expect("known shape must yield a suggestion");
        assert_eq!(s.suggested_name, "gitea-team");
        assert_eq!(s.url_prefix, "https://gitea.example.com/team");
        assert!(s.pat_creation_url.is_none());
    }

    #[test]
    fn derive_creds_suggestion_rejects_non_https_urls() {
        // SSH / git@ shapes use SSH keys; our cred-suggestion
        // surface doesn't apply.
        assert!(derive_creds_suggestion("git@github.com:foo/bar").is_none());
        assert!(derive_creds_suggestion("ssh://git@github.com/foo/bar").is_none());
    }

    #[test]
    fn derive_creds_suggestion_rejects_malformed_urls() {
        // No org segment, or empty fields after the scheme.
        assert!(derive_creds_suggestion("https://github.com").is_none());
        assert!(derive_creds_suggestion("https://github.com/").is_none());
        assert!(derive_creds_suggestion("https://").is_none());
    }

    #[test]
    fn git_probe_verdict_only_200_is_public() {
        // 2.4g: the anonymous smart-HTTP probe gates the PAT notice.
        // 200 = credential-less clone works → public → suppress the
        // notice. 401/403 = auth required → private. 404 / other /
        // transport error (None) → conservatively NOT public, so the
        // operator still gets the credential guidance.
        assert!(git_probe_verdict(Some(200)));
        assert!(!git_probe_verdict(Some(401)));
        assert!(!git_probe_verdict(Some(403)));
        assert!(!git_probe_verdict(Some(404)));
        assert!(!git_probe_verdict(Some(500)));
        assert!(!git_probe_verdict(None));
    }

    #[test]
    fn manifest_injects_plugin_env_and_labels_when_env_set() {
        let m = build_application_manifest(
            "web",
            "web-dev",
            "https://x/r.git",
            "main",
            "/",
            "apps",
            "web-dev",
            Some("dev"),
        );
        assert_eq!(m.pointer("/metadata/name").unwrap(), "web-dev");
        assert_eq!(
            m.pointer("/metadata/labels/apprafter.io~1application")
                .unwrap(),
            "web"
        );
        assert_eq!(
            m.pointer("/metadata/labels/apprafter.io~1environment")
                .unwrap(),
            "dev"
        );
        let env0 = &m.pointer("/spec/source/plugin/env/0").unwrap();
        assert_eq!(env0.get("name").unwrap(), "APPRAFTER_APP_ENV");
        assert_eq!(env0.get("value").unwrap(), "dev");
    }

    #[test]
    fn manifest_base_only_has_no_plugin_env_or_environment_label() {
        let m = build_application_manifest(
            "web",
            "web",
            "https://x/r.git",
            "main",
            "/",
            "apps",
            "apprafter",
            None,
        );
        assert_eq!(m.pointer("/metadata/name").unwrap(), "web");
        assert_eq!(
            m.pointer("/metadata/labels/apprafter.io~1application")
                .unwrap(),
            "web"
        );
        assert!(m
            .pointer("/metadata/labels/apprafter.io~1environment")
            .is_none());
        assert!(m.pointer("/spec/source/plugin").is_none());
    }

    #[test]
    fn build_application_manifest_includes_managed_by_label() {
        // Load-bearing — `app list` filters by this label.
        // If a future refactor drops it, `list` shows nothing.
        let m = build_application_manifest(
            "my-app",
            "my-app",
            "https://github.com/foo/bar",
            "main",
            "/",
            "apps",
            "apprafter",
            None,
        );
        assert_eq!(
            m.pointer("/metadata/labels/apprafter.io~1managed-by")
                .and_then(Value::as_str),
            Some("apprafter")
        );
        assert_eq!(
            m.pointer("/metadata/annotations/apprafter.io~1source")
                .and_then(Value::as_str),
            Some("cli")
        );
    }

    #[test]
    fn build_application_manifest_carries_correct_cascade_finalizer() {
        // Load-bearing — Argo CD owns cascade deletion of the synced
        // AppRafter CR via THIS finalizer. The exact string matters:
        // `resources-finalizer` (NOT `resources-finalization` — walk-fix
        // #3's typo, which Argo ignored, hanging deletion in Terminating).
        let m = build_application_manifest(
            "my-app",
            "my-app",
            "https://github.com/foo/bar",
            "main",
            "/",
            "apps",
            "apprafter",
            None,
        );
        let finalizers = m
            .pointer("/metadata/finalizers")
            .and_then(Value::as_array)
            .expect("finalizers array present");
        assert_eq!(
            finalizers
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            vec!["resources-finalizer.argocd.argoproj.io/background"]
        );
    }

    #[test]
    fn build_application_manifest_routes_argocd_cr_to_argocd_ns_destination_to_param() {
        // The CR itself lives in the `argocd` namespace by
        // convention. `spec.destination.namespace` is now an
        // explicit caller parameter (walk-fix #12 / v0.1.160)
        // — must reflect what's passed, not the app name.
        let m = build_application_manifest(
            "payments",
            "payments",
            "https://x/y",
            "v1.0",
            "/",
            "apps",
            "apprafter",
            None,
        );
        assert_eq!(
            m.pointer("/metadata/namespace").and_then(Value::as_str),
            Some("argocd")
        );
        assert_eq!(
            m.pointer("/spec/destination/namespace")
                .and_then(Value::as_str),
            Some("apprafter")
        );
    }

    #[test]
    fn build_application_manifest_destination_namespace_honours_explicit_caller_value() {
        // Walk-fix #12 regression guard: passing an explicit
        // namespace (e.g. an operator who knows their manifest
        // lives in `tenant-x`) must NOT be silently overridden
        // by the prior `<app-name>` default.
        let m = build_application_manifest(
            "payments",
            "payments",
            "https://x/y",
            "v1.0",
            "/",
            "apps",
            "tenant-x",
            None,
        );
        assert_eq!(
            m.pointer("/spec/destination/namespace")
                .and_then(Value::as_str),
            Some("tenant-x")
        );
        // Make sure the name doesn't accidentally leak into
        // destination.namespace anywhere else.
        assert_ne!(
            m.pointer("/spec/destination/namespace")
                .and_then(Value::as_str),
            Some("payments")
        );
    }

    #[test]
    fn build_application_manifest_carries_project_and_revision() {
        let m = build_application_manifest("a", "a", "u", "v", "/p", "apps", "apprafter", None);
        assert_eq!(
            m.pointer("/spec/project").and_then(Value::as_str),
            Some("apps")
        );
        assert_eq!(
            m.pointer("/spec/source/targetRevision")
                .and_then(Value::as_str),
            Some("v")
        );
        // Walk-fix #2: `build_application_manifest` strips
        // leading `/` to keep Argo CD's `spec.source.path`
        // relative; "/p" → "p".
        assert_eq!(
            m.pointer("/spec/source/path").and_then(Value::as_str),
            Some("p")
        );
    }

    #[test]
    fn build_kubectl_logs_target_defaults_to_selector() {
        let target = build_kubectl_logs_target(&["payments".to_string()], None);
        assert_eq!(
            target,
            KubectlLogsTarget::Selector {
                selector: "app.kubernetes.io/name=payments".to_string(),
                workloads: 1,
            }
        );
    }

    #[test]
    fn logs_target_resolves_inner_workload_name_from_status_resources() {
        // 2.4g walk bug: Argo CD app "cms" renders an AppRafter
        // Application "landing-cms"; the workload pods carry the
        // operator's `app.kubernetes.io/name=landing-cms` label, NOT
        // the Argo parent name. `app logs cms` must select by the
        // resolved inner name, or it finds no pods.
        let app = serde_json::json!({
            "metadata": { "name": "cms" },
            "spec": { "destination": { "namespace": "landing" } },
            "status": { "resources": [
                { "group": "apprafter.io", "kind": "Application", "name": "landing-cms" }
            ] }
        });
        let (_, inner) = resolve_logs_workload(&app, "cms", None).unwrap();
        assert_eq!(inner, vec!["landing-cms".to_string()]);
        let target = build_kubectl_logs_target(&inner, None);
        assert_eq!(
            target,
            KubectlLogsTarget::Selector {
                selector: "app.kubernetes.io/name=landing-cms".to_string(),
                workloads: 1,
            }
        );
    }

    #[test]
    fn build_kubectl_logs_target_uses_pod_name_when_provided() {
        let target =
            build_kubectl_logs_target(&["payments".to_string()], Some("payments-7f9c-xyz"));
        assert_eq!(
            target,
            KubectlLogsTarget::Pod("payments-7f9c-xyz".to_string())
        );
    }

    #[test]
    fn build_kubectl_logs_args_selector_form_includes_prefix_and_max_requests() {
        // Selector mode aggregates across multiple pods;
        // kubectl benefits from --prefix to distinguish lines,
        // and --max-log-requests to cap fan-out. Both are
        // load-bearing for real usability on multi-pod apps.
        let args = build_kubectl_logs_args(
            &KubectlLogsTarget::Selector {
                selector: "app.kubernetes.io/name=payments".to_string(),
                workloads: 1,
            },
            "payments",
            false,
            -1,
            None,
        );
        assert!(args.contains(&"-l".to_string()));
        assert!(args.contains(&"app.kubernetes.io/name=payments".to_string()));
        assert!(args.contains(&"-n".to_string()));
        assert!(args.contains(&"payments".to_string()));
        assert!(args.contains(&"--prefix=true".to_string()));
        assert!(args.contains(&"--max-log-requests=10".to_string()));
        assert!(!args.contains(&"-f".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--tail=")));
    }

    #[test]
    fn build_kubectl_logs_args_pod_form_drops_prefix() {
        // Single-pod target — no need for a line prefix;
        // kubectl's default output is already clean. Defensive:
        // ensure we don't accidentally emit --prefix or
        // --max-log-requests when it would just clutter the
        // output.
        let args = build_kubectl_logs_args(
            &KubectlLogsTarget::Pod("payments-7f9c-xyz".to_string()),
            "payments",
            true,
            100,
            Some("api"),
        );
        assert_eq!(args[0], "logs");
        assert_eq!(args[1], "payments-7f9c-xyz");
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&"--tail=100".to_string()));
        assert!(args.contains(&"-c".to_string()));
        assert!(args.contains(&"api".to_string()));
        assert!(!args.contains(&"--prefix=true".to_string()));
        assert!(!args.iter().any(|a| a.contains("--max-log-requests")));
    }

    #[test]
    fn pick_previous_revision_returns_second_to_last() {
        // Argo CD's status.history is chronological — oldest
        // first, newest last. Previous = second-to-last.
        let app = serde_json::json!({
            "status": {
                "history": [
                    {"id": 1, "revision": "abc123"},
                    {"id": 2, "revision": "def456"},
                    {"id": 3, "revision": "ghi789"}
                ]
            }
        });
        assert_eq!(pick_previous_revision(&app).unwrap(), "def456");
    }

    #[test]
    fn pick_previous_revision_errors_when_history_too_short() {
        // Fresh app — one or zero history entries — has no
        // "previous" to roll back to. The operator must pass
        // --to explicitly. Tests both edge cases.
        let one = serde_json::json!({
            "status": { "history": [ {"id": 1, "revision": "abc"} ] }
        });
        let zero = serde_json::json!({ "status": { "history": [] } });
        let missing = serde_json::json!({ "status": {} });
        assert!(pick_previous_revision(&one).is_err());
        assert!(pick_previous_revision(&zero).is_err());
        assert!(pick_previous_revision(&missing).is_err());
    }

    #[test]
    fn print_status_handles_app_without_status_block() {
        // Freshly-created Argo CD Application has no status
        // yet — must not panic, must default to Unknown for
        // sync + health.
        let app = serde_json::json!({
            "metadata": { "name": "x" },
            "spec": {
                "project": "apps",
                "source": { "repoURL": "https://r", "targetRevision": "main" },
                "destination": { "namespace": "x" }
            }
        });
        print_status(&app);
    }

    #[test]
    fn status_detail_lines_shows_environment() {
        // An env deployment (`<name>-<env>`) carries the
        // apprafter.io/environment label — the detail view must show it
        // (the 2.9 gap: env was only printed in the multi-deploy header).
        let prod = serde_json::json!({
            "metadata": { "name": "web-prod", "labels": { "apprafter.io/environment": "prod" } },
            "spec": {
                "project": "apps",
                "source": { "repoURL": "https://r", "targetRevision": "main" },
                "destination": { "namespace": "web" }
            }
        });
        let env_line = status_detail_lines(&prod)
            .into_iter()
            .find(|l| l.contains("environment:"))
            .expect("detail view must include an environment: line");
        assert!(
            env_line.contains("prod"),
            "labelled deployment must show its env; got {env_line:?}"
        );

        // A base-only app (no env label) shows `(base)`.
        let base = serde_json::json!({
            "metadata": { "name": "web" },
            "spec": {
                "project": "apps",
                "source": { "repoURL": "https://r", "targetRevision": "main" },
                "destination": { "namespace": "web" }
            }
        });
        let env_line = status_detail_lines(&base)
            .into_iter()
            .find(|l| l.contains("environment:"))
            .expect("detail view must include an environment: line");
        assert!(
            env_line.contains("(base)"),
            "base-only deployment must show (base); got {env_line:?}"
        );
    }

    #[test]
    fn status_detail_lines_surface_an_argo_condition() {
        // 2.27a taught the cue-cmp sidecar to REFUSE a self-contradicting
        // bundle. Argo CD lands that refusal on
        // `status.conditions[].message` and leaves the sync Unknown, so a
        // detail block that reads only `status.sync` tells the operator
        // their deploy is broken and nothing about why.
        let app = serde_json::json!({
            "metadata": {"name": "shop"},
            "status": {
                "sync": {"status": "Unknown"},
                "conditions": [{
                    "type": "ComparisonError",
                    "message": "::cue-cmp:: bundle is inconsistent: workloads declare 2 different namespaces — api -> \"one\", web -> \"two\""
                }]
            }
        });
        let joined = status_detail_lines(&app).join("\n");
        assert!(joined.contains("ComparisonError"), "{joined}");
        assert!(joined.contains("2 different namespaces"), "{joined}");
    }

    #[test]
    fn status_detail_lines_say_nothing_when_there_are_no_conditions() {
        // A healthy app must not pay for the refusing one: no header, no
        // empty section. The N=1 golden block pins this too.
        let app = serde_json::json!({
            "metadata": {"name": "ok"},
            "status": {"sync": {"status": "Synced"}}
        });
        let joined = status_detail_lines(&app).join("\n");
        assert!(!joined.to_lowercase().contains("condition"), "{joined}");
    }

    #[test]
    fn status_detail_lines_surface_every_condition_not_just_the_first() {
        let app = serde_json::json!({"metadata": {"name": "shop"}, "status": {"conditions": [
            {"type": "ComparisonError", "message": "first"},
            {"type": "OrphanedResourceWarning", "message": "second"}
        ]}});
        let joined = status_detail_lines(&app).join("\n");
        assert!(
            joined.contains("first") && joined.contains("second"),
            "{joined}"
        );
    }

    #[test]
    fn status_detail_lines_fold_a_multi_line_condition_message() {
        // Argo stores what the generate command put on stderr. The cue-cmp
        // refusals are multi-line by design: a one-line tile summary, then a
        // detail block. A raw embed would break the aligned block this
        // function renders.
        let app = serde_json::json!({"metadata": {"name": "shop"}, "status": {"conditions": [
            {"type": "ComparisonError", "message": "summary line\n\n--- apprafter bundle check ---\n  api  -> \"one\"\n  web  -> \"two\""}
        ]}});
        let lines = status_detail_lines(&app);
        // The folding rule: the FIRST line rides the `<Type>:` line and
        // every remaining line is re-indented underneath it, paragraph
        // breaks and all. See `condition_lines` for why the tail is kept
        // rather than counted or dropped.
        assert!(
            lines.iter().any(|l| l.contains("summary line")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("api  -> \"one\"")),
            "the detail block names WHICH workloads disagree — that is the \
             fix instruction, it must survive the fold; got {lines:?}"
        );
        assert!(
            lines.iter().all(|l| !l.contains('\n')),
            "a folded message must not smuggle a raw newline back into a \
             single Vec element; got {lines:?}"
        );
        assert!(
            !lines.last().is_some_and(|l| l.trim().is_empty()),
            "a message's trailing blank must not dangle into whatever \
             section follows; got {lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.trim().is_empty()).count(),
            2,
            "exactly two blanks: the one opening the conditions section, \
             and the message's own paragraph break — no run, no dangle; \
             got {lines:?}"
        );
    }

    /// Exactly what Argo CD v2.13 wraps a failed CMP generate in before
    /// it stores the plugin's stderr on the condition: a Go error chain,
    /// our sidecar's output appended to the tail of it.
    fn argo_cmp_wrapped(stderr: &str) -> String {
        format!(
            "Failed to load target state: failed to generate manifest for \
             source 1 of 1: rpc error: code = Unknown desc = error generating \
             manifests in cmp: rpc error: code = Unknown desc = error \
             generating manifests: exit status 1: {stderr}"
        )
    }

    #[test]
    fn condition_lines_render_from_our_own_sentinel_not_argos_wrapper() {
        // The finding the sidecar wrote to BE the whole finding must be
        // the first thing on the first line — not ~180 characters of RPC
        // plumbing followed by it.
        let app = serde_json::json!({
            "metadata": {"name": "shop"},
            "status": {"sync": {"status": "Unknown"}, "conditions": [{
                "type": "ComparisonError",
                "message": argo_cmp_wrapped(
                    "::cue-cmp:: bundle is inconsistent: workloads declare 2 \
                     different namespaces — api -> \"one\", web -> \"two\"\n\n\
                     --- apprafter bundle check ---\n  \
                     api                      metadata.namespace: one\n  \
                     web                      metadata.namespace: two"
                )
            }]}
        });
        let lines = status_detail_lines(&app);
        let first = lines
            .iter()
            .find(|l| l.contains("ComparisonError"))
            .expect("the condition must render at all");
        assert_eq!(
            first,
            "  ComparisonError: ::cue-cmp:: bundle is inconsistent: workloads \
             declare 2 different namespaces — api -> \"one\", web -> \"two\"",
            "the first line must open at OUR sentinel"
        );
        let joined = lines.join("\n");
        for framing in ["Failed to load target state", "rpc error", "exit status 1"] {
            assert!(
                !joined.contains(framing),
                "transport framing {framing:?} survived the cut; got {joined}"
            );
        }
        // The cut removes the framing only — the detail block that names
        // which workloads disagree is still the point of the block.
        assert!(
            joined.contains("api                      metadata.namespace: one"),
            "{joined}"
        );
    }

    #[test]
    fn condition_lines_leave_a_condition_without_our_sentinel_whole() {
        // Argo CD raises plenty of conditions on its own. Those carry no
        // sentinel, have no transport framing wrapped around them, and are
        // entirely their own content — the cut must not touch them.
        let message = "one or more objects failed to apply, reason: \
                       Operation cannot be fulfilled on deployments.apps \
                       \"web\": the object has been modified; please apply \
                       your changes to the latest version and try again";
        let app = serde_json::json!({
            "metadata": {"name": "shop"},
            "status": {"conditions": [{"type": "SyncError", "message": message}]}
        });
        let lines = status_detail_lines(&app);
        assert!(
            lines.contains(&format!("  SyncError: {message}")),
            "a sentinel-free condition must render byte-for-byte; got {lines:?}"
        );
        // And the same at the helper, where a stray cut would be silent.
        assert_eq!(strip_transport_prefix(message), message);
    }

    #[test]
    fn fold_condition_message_collapses_blank_runs_and_trims_the_ends() {
        assert_eq!(
            fold_condition_message("\n\n  a  \n\n\n\nb\n  \n\n"),
            vec!["  a", "", "b"],
            "leading and trailing blanks dropped, an interior run collapsed \
             to one, trailing whitespace stripped, indentation preserved"
        );
        assert!(fold_condition_message("").is_empty());
        assert!(fold_condition_message("\n  \n\n").is_empty());
    }

    #[test]
    fn parse_pod_summaries_running_pod() {
        // Happy path: container is ready, no waiting state,
        // pod phase Running. STATUS column reflects phase.
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let payload = serde_json::json!({
            "items": [{
                "metadata": {
                    "name": "landing-web-abc",
                    "creationTimestamp": "2026-05-25T11:30:00Z"
                },
                "spec": { "containers": [{ "name": "landing-web" }] },
                "status": {
                    "phase": "Running",
                    "containerStatuses": [{
                        "ready": true,
                        "restartCount": 0,
                        "state": { "running": { "startedAt": "2026-05-25T11:31:00Z" } }
                    }]
                }
            }]
        });
        let pods = parse_pod_summaries(&payload, &now);
        assert_eq!(pods.len(), 1);
        assert_eq!(pods[0].name, "landing-web-abc");
        assert_eq!(pods[0].ready, "1/1");
        assert_eq!(pods[0].status, "Running");
        assert_eq!(pods[0].restarts, 0);
        assert_eq!(pods[0].age, "30m");
    }

    #[test]
    fn parse_pod_summaries_image_pull_back_off() {
        // The walk-fix-#2 regression scenario. Pod phase
        // Pending, container waiting.reason=ImagePullBackOff;
        // status column reflects the waiting reason — that's
        // what's actionable for the operator.
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let payload = serde_json::json!({
            "items": [{
                "metadata": {
                    "name": "landing-web-mrpfx",
                    "creationTimestamp": "2026-05-25T09:10:00Z"
                },
                "spec": { "containers": [{ "name": "landing-web" }] },
                "status": {
                    "phase": "Pending",
                    "containerStatuses": [{
                        "ready": false,
                        "restartCount": 0,
                        "state": {
                            "waiting": {
                                "reason": "ImagePullBackOff",
                                "message": "Back-off pulling image ..."
                            }
                        }
                    }]
                }
            }]
        });
        let pods = parse_pod_summaries(&payload, &now);
        assert_eq!(pods[0].ready, "0/1");
        assert_eq!(pods[0].status, "ImagePullBackOff");
        assert_eq!(pods[0].age, "2h50m");
    }

    #[test]
    fn parse_pod_summaries_crash_loop_back_off_carries_restart_count() {
        // CMS scenario — container exists but crashes. Restart
        // count surfaces; STATUS column shows CrashLoopBackOff.
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let payload = serde_json::json!({
            "items": [{
                "metadata": {
                    "name": "landing-cms-b8tsv",
                    "creationTimestamp": "2026-05-25T09:10:00Z"
                },
                "spec": { "containers": [{ "name": "landing-cms" }] },
                "status": {
                    "phase": "Running",
                    "containerStatuses": [{
                        "ready": false,
                        "restartCount": 38,
                        "state": {
                            "waiting": {
                                "reason": "CrashLoopBackOff",
                                "message": "back-off restarting failed container"
                            }
                        }
                    }]
                }
            }]
        });
        let pods = parse_pod_summaries(&payload, &now);
        assert_eq!(pods[0].status, "CrashLoopBackOff");
        assert_eq!(pods[0].restarts, 38);
    }

    #[test]
    fn parse_pod_summaries_missing_container_statuses_falls_back_to_spec() {
        // Brand-new pod that's been admitted to scheduling
        // but not yet had any containers materialised. Must
        // not panic; column denominator comes from spec.
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let payload = serde_json::json!({
            "items": [{
                "metadata": {
                    "name": "fresh-pod",
                    "creationTimestamp": "2026-05-25T11:59:30Z"
                },
                "spec": {
                    "containers": [
                        { "name": "first" },
                        { "name": "sidecar" }
                    ]
                },
                "status": { "phase": "Pending" }
            }]
        });
        let pods = parse_pod_summaries(&payload, &now);
        assert_eq!(pods[0].ready, "0/2");
        assert_eq!(pods[0].status, "Pending");
        assert_eq!(pods[0].age, "30s");
    }

    #[test]
    fn parse_pod_summaries_handles_empty_payload() {
        let now = chrono::Utc::now();
        assert!(parse_pod_summaries(&serde_json::json!({}), &now).is_empty());
        assert!(parse_pod_summaries(&serde_json::json!({ "items": [] }), &now).is_empty());
    }

    #[test]
    fn format_pod_age_seconds_minutes_hours_days() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(format_pod_age("2026-05-25T11:59:55Z", &now), "5s");
        assert_eq!(format_pod_age("2026-05-25T11:55:00Z", &now), "5m");
        assert_eq!(format_pod_age("2026-05-25T09:30:00Z", &now), "2h30m");
        assert_eq!(format_pod_age("2026-05-25T11:00:00Z", &now), "1h");
        assert_eq!(format_pod_age("2026-05-22T12:00:00Z", &now), "3d");
        assert_eq!(format_pod_age("2026-05-22T09:00:00Z", &now), "3d3h");
    }

    #[test]
    fn format_pod_age_returns_input_on_unparseable_timestamp() {
        // Defensive against Argo CD shape drift — must not
        // panic, just echo whatever was passed.
        let now = chrono::Utc::now();
        assert_eq!(format_pod_age("not-a-timestamp", &now), "not-a-timestamp");
    }

    // ---- ADR 0059: rollback target classification + pin write ----

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn cr_with_previous(prev: &str) -> Value {
        json!({ "status": { "image": {
            "tag": "ghcr.io/acme/web:latest",
            "resolved": "ghcr.io/acme/web@sha256:bbb",
            "previous": { "resolved": prev, "tag": "ghcr.io/acme/web:latest" }
        }}})
    }

    fn argo_with_history() -> Value {
        json!({ "status": { "history": [
            { "revision": "aaaaaaa" },
            { "revision": "bbbbbbb" },
            { "revision": "ccccccc" }
        ]}})
    }

    #[test]
    fn a_sha256_target_is_an_image_digest() {
        let to = format!("sha256:{HEX}");
        assert_eq!(
            classify_rollback_target(Some(&to), None, &json!({})).unwrap(),
            RollbackTarget::Digest(to.clone())
        );
    }

    #[test]
    fn a_branch_name_is_still_a_git_revision() {
        assert_eq!(
            classify_rollback_target(Some("main"), None, &json!({})).unwrap(),
            RollbackTarget::GitRevision("main".into())
        );
        assert_eq!(
            classify_rollback_target(Some("v1.2.3"), None, &json!({})).unwrap(),
            RollbackTarget::GitRevision("v1.2.3".into())
        );
    }

    #[test]
    fn a_colon_bearing_value_that_is_not_a_digest_is_rejected_not_passed_through() {
        // Git refnames forbid `:`, so falling through would only defer the
        // same failure to Argo CD — with a worse message and a reconcile
        // cycle in between.
        let err = classify_rollback_target(Some("sha256:deadbeef"), None, &json!({})).unwrap_err();
        assert!(format!("{err}").contains("64 lowercase hex"), "{err}");
        let err = classify_rollback_target(Some("weird:thing"), None, &json!({})).unwrap_err();
        assert!(format!("{err}").contains("cannot contain"), "{err}");
    }

    #[test]
    fn an_uppercase_digest_is_rejected() {
        // OCI digests are canonically lowercase. Accepting an uppercase
        // variant here would produce a pin the operator's own validator
        // then rejects — a failure two components away from the typo.
        let to = format!("sha256:{}", HEX.to_uppercase());
        assert!(classify_rollback_target(Some(&to), None, &json!({})).is_err());
    }

    #[test]
    fn bare_rollback_prefers_the_retained_digest_over_the_git_revision() {
        // For a tag-following app the git path provably does not roll the
        // workload back — that is the defect. Preferring it would be
        // preferring the known-wrong answer.
        let cr = cr_with_previous("ghcr.io/acme/web@sha256:aaa");
        assert_eq!(
            classify_rollback_target(None, Some(&cr), &argo_with_history()).unwrap(),
            RollbackTarget::Digest("ghcr.io/acme/web@sha256:aaa".into())
        );
    }

    #[test]
    fn bare_rollback_falls_back_to_git_when_no_digest_was_retained() {
        let cr = json!({ "status": { "image": { "tag": "ghcr.io/acme/web:latest" }}});
        assert_eq!(
            classify_rollback_target(None, Some(&cr), &argo_with_history()).unwrap(),
            RollbackTarget::GitRevision("bbbbbbb".into())
        );
        assert_eq!(
            classify_rollback_target(None, None, &argo_with_history()).unwrap(),
            RollbackTarget::GitRevision("bbbbbbb".into())
        );
    }

    // ---- compose_pin_reference ----

    #[test]
    fn a_bare_digest_gains_the_repository_the_app_already_recorded() {
        let cr = cr_with_previous("ghcr.io/acme/web@sha256:aaa");
        let composed = compose_pin_reference(&format!("sha256:{HEX}"), &cr).unwrap();
        assert_eq!(composed, format!("ghcr.io/acme/web@sha256:{HEX}"));
    }

    #[test]
    fn a_repository_is_never_guessed() {
        // A guessed repository is how a pin ends up pointing at somebody
        // else's image, so an app with no recorded image is an error rather
        // than a default.
        let err = compose_pin_reference(&format!("sha256:{HEX}"), &json!({})).unwrap_err();
        assert!(format!("{err}").contains("which repository"), "{err}");
    }

    #[test]
    fn a_full_reference_passes_through_untouched() {
        let full = format!("ghcr.io/acme/web@sha256:{HEX}");
        assert_eq!(compose_pin_reference(&full, &json!({})).unwrap(), full);
    }

    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag() {
        // `localhost:5000/web:latest` — the `:` in the host must not be
        // read as the tag separator, or the composed pin would name the
        // repository `localhost`.
        let cr = json!({ "status": { "image": { "tag": "localhost:5000/web:latest" }}});
        let composed = compose_pin_reference(&format!("sha256:{HEX}"), &cr).unwrap();
        assert_eq!(composed, format!("localhost:5000/web@sha256:{HEX}"));
    }

    // ---- pin_manifest ----

    #[test]
    fn the_pin_body_carries_nothing_but_the_two_annotations() {
        // Load-bearing: un-pinning re-applies this body with the keys
        // omitted, and SSA prunes whatever the manager owns and no longer
        // lists. A body naming a spec field would make `unpin` delete that
        // field too — the 2.10 egress defect at a new address.
        let body = pin_manifest(
            "web",
            "demo",
            Some(("repo@sha256:aaa", "2026-08-31T10:00:00Z")),
        );
        let meta = body.get("metadata").unwrap().as_object().unwrap();
        let mut keys: Vec<&str> = meta.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["annotations", "name", "namespace"]);
        assert!(body.get("spec").is_none());
        let anns = body
            .pointer("/metadata/annotations")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(anns.len(), 2);
    }

    #[test]
    fn the_unpin_body_is_the_same_body_with_the_keys_omitted() {
        let body = pin_manifest("web", "demo", None);
        assert_eq!(
            body.pointer("/metadata/annotations")
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            body.pointer("/metadata/name").and_then(Value::as_str),
            Some("web")
        );
    }

    #[test]
    fn the_pin_manager_is_distinct_from_every_other_cli_manager() {
        // Same guard the egress manager carries: sharing a manager with
        // another write would make one command's apply prune the other's
        // fields.
        use cli_providers::k8s::kubectl::{
            APPRAFTER_CLI_EGRESS_FIELD_MANAGER, APPRAFTER_CLI_FIELD_MANAGER,
            APPRAFTER_CLI_PIN_FIELD_MANAGER,
        };
        assert_ne!(APPRAFTER_CLI_PIN_FIELD_MANAGER, APPRAFTER_CLI_FIELD_MANAGER);
        assert_ne!(
            APPRAFTER_CLI_PIN_FIELD_MANAGER,
            APPRAFTER_CLI_EGRESS_FIELD_MANAGER
        );
        // And not Argo-CD-shaped, or our own write would trip the
        // git-ownership guard on the next invocation.
        for needle in ["argocd", "argo-cd", "application-controller"] {
            assert!(!APPRAFTER_CLI_PIN_FIELD_MANAGER.contains(needle));
        }
    }

    // ---- pin_appears_git_managed ----

    #[test]
    fn a_manifest_declared_pin_is_detected_as_git_owned() {
        let cr = json!({ "metadata": { "managedFields": [
            { "manager": "argocd-application-controller",
              "fieldsV1": { "f:metadata": { "f:annotations": { "f:apprafter.io/image-pin": {} }}}}
        ]}});
        assert!(pin_appears_git_managed(&cr));
    }

    #[test]
    fn our_own_pin_write_is_not_mistaken_for_a_git_owner() {
        let cr = json!({ "metadata": { "managedFields": [
            { "manager": "apprafter-cli-pin",
              "fieldsV1": { "f:metadata": { "f:annotations": { "f:apprafter.io/image-pin": {} }}}}
        ]}});
        assert!(!pin_appears_git_managed(&cr));
        assert!(!pin_appears_git_managed(&json!({})));
    }

    #[test]
    fn argo_owning_a_different_annotation_is_not_a_git_owned_pin() {
        // Argo stamps its own tracking annotation on every managed object.
        // Treating that as ownership of OUR key would refuse every pin.
        let cr = json!({ "metadata": { "managedFields": [
            { "manager": "argocd-application-controller",
              "fieldsV1": { "f:metadata": { "f:annotations": {
                  "f:argocd.argoproj.io/tracking-id": {} }}}}
        ]}});
        assert!(!pin_appears_git_managed(&cr));
    }

    // ---- rollback_patch_body ----

    #[test]
    fn the_rollback_patch_body_survives_a_quote_in_the_revision() {
        // The old hand-spliced `format!` produced malformed JSON here, and
        // a patch body is not a place to discover that.
        let body = rollback_patch_body(r#"we"ird"#);
        let parsed: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(
            parsed
                .pointer("/spec/source/targetRevision")
                .and_then(Value::as_str),
            Some(r#"we"ird"#)
        );
    }

    // ---- format_pin_line ----

    // ---- 2.22h / D16: reconcile failures visible without a log read ----

    fn problem_cr(reason: &str, last_seen: &str, count: i64) -> Value {
        json!({ "status": { "recentProblems": [
            { "reason": reason,
              "message": "could not delete claim web-pg: forbidden",
              "firstSeen": "2026-09-01T10:00:00+00:00",
              "lastSeen": last_seen,
              "count": count }
        ]}})
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn a_live_problem_names_the_reason_and_the_message() {
        // The whole point of D16: the operator log said "forbidden ... cannot
        // delete resourceclaims" every thirty seconds for three walk runs, and
        // `app status` said Ready. This line is what should have been there.
        let cr = problem_cr("ClaimPruneFailed", "2026-09-01T10:05:00+00:00", 9);
        let lines = format_problem_lines(&cr, &at("2026-09-01T10:06:00+00:00"));
        assert!(lines[0].contains("ClaimPruneFailed"), "{lines:?}");
        assert!(lines[0].contains("forbidden"), "{lines:?}");
        assert!(lines[0].contains("(now, 9x)"), "{lines:?}");
    }

    #[test]
    fn an_older_problem_is_dated_rather_than_reported_as_current() {
        let cr = problem_cr("ClaimPruneFailed", "2026-09-01T10:00:00+00:00", 3);
        let lines = format_problem_lines(&cr, &at("2026-09-01T12:00:00+00:00"));
        assert!(!lines[0].contains("(now"), "{lines:?}");
        assert!(lines[0].contains("ago"), "{lines:?}");
    }

    #[test]
    fn a_problem_past_the_render_horizon_is_not_printed() {
        // The guard against the failure mode the defect named: a surface
        // listing what was ONCE broken is one people learn not to read, and
        // then it is worse than not having it.
        let cr = problem_cr("ClaimPruneFailed", "2026-08-30T10:00:00+00:00", 3);
        assert!(format_problem_lines(&cr, &at("2026-09-01T12:00:00+00:00")).is_empty());
    }

    #[test]
    fn a_healthy_application_prints_nothing_at_all() {
        // NOT "Problems: none". A section that is always there is a section
        // people skip, which would defeat the whole surface.
        assert!(format_problem_lines(&json!({}), &at("2026-09-01T12:00:00+00:00")).is_empty());
        let empty = json!({ "status": { "recentProblems": [] }});
        assert!(format_problem_lines(&empty, &at("2026-09-01T12:00:00+00:00")).is_empty());
    }

    #[test]
    fn a_problem_refreshed_at_the_operators_floor_still_reads_as_live() {
        // The operator only rewrites `lastSeen` every 900s (its write
        // deadband). If the CLI's "now" window were shorter, a still-failing
        // application would spend most of every window reading as stale —
        // telling an operator a live problem is old is a specific way of
        // being wrong. Asserted through the RENDERING, not on the constants,
        // so it survives either number being retuned.
        let cr = problem_cr("ClaimPruneFailed", "2026-09-01T10:00:00+00:00", 30);
        let just_before_refresh = at("2026-09-01T10:14:59+00:00");
        let lines = format_problem_lines(&cr, &just_before_refresh);
        assert!(
            lines[0].contains("(now"),
            "a problem the operator has not yet refreshed must still read as current: {lines:?}"
        );
    }

    #[test]
    fn an_unparseable_timestamp_is_skipped_rather_than_rendered_as_now() {
        // Rendering a garbage stamp as "now" would report a problem that may
        // be ancient as current.
        let cr = problem_cr("ClaimPruneFailed", "not-a-date", 1);
        assert!(format_problem_lines(&cr, &at("2026-09-01T12:00:00+00:00")).is_empty());
    }

    #[test]
    fn the_pin_line_names_the_digest_the_tag_and_the_way_out() {
        let cr = json!({
            "metadata": { "name": "web" },
            "status": { "image": {
                "tag": "ghcr.io/acme/web:latest",
                "pinned": { "resolved": "ghcr.io/acme/web@sha256:aaa", "at": "2026-08-31T10:00:00Z" }
            }}
        });
        let line = format_pin_line(&cr).expect("pinned");
        assert!(line.contains("ghcr.io/acme/web@sha256:aaa"), "{line}");
        assert!(
            line.contains("NOT following ghcr.io/acme/web:latest"),
            "{line}"
        );
        assert!(line.contains("apprafter app unpin web"), "{line}");
    }

    // -----------------------------------------------------------------
    // D7 — `app status` explains a failure, it does not merely name it
    // -----------------------------------------------------------------

    fn env_secret_missing_cr(message: &str) -> Value {
        json!({
            "status": {
                "phase": "EnvSecretMissing",
                "conditions": [
                    { "type": "Ready", "status": "False",
                      "reason": "EnvSecretMissing", "message": message }
                ]
            }
        })
    }

    #[test]
    fn a_not_ready_app_shows_the_operators_diagnostic_not_just_the_phase() {
        // The whole of D7: this message names the cause, the namespace and
        // the keys the Secret DOES carry, and until now the only way to read
        // it was `kubectl get application -o yaml`.
        let msg = "env STRIPE_KEY → secret \"appsecret/token\": Secret \"appsecret\" \
                   exists in namespace \"demo\" but carries no key \"token\" \
                   (it carries: api_key, url)";
        let line = format_not_ready_line(&env_secret_missing_cr(msg)).expect("a reason line");
        assert!(
            line.contains("EnvSecretMissing"),
            "names the reason: {line}"
        );
        assert!(
            line.contains("carries no key"),
            "carries the diagnostic: {line}"
        );
        assert!(
            line.contains("namespace \"demo\""),
            "names the namespace: {line}"
        );
        assert!(line.contains("api_key"), "lists the available keys: {line}");
    }

    #[test]
    fn several_unresolved_refs_are_split_one_per_line() {
        // The operator joins them with "; " into a single line, which at three
        // refs is a wall. One per line, or the reader skims past the one that
        // matters.
        let msg = "env A → secret \"s/a\": missing; env B → secret \"s/b\": missing; \
                   env C → secret \"s/c\": missing";
        let line = format_not_ready_line(&env_secret_missing_cr(msg)).expect("a reason line");
        assert_eq!(
            line.lines().count(),
            4,
            "heading + one line per ref: {line}"
        );
        assert!(
            !line.contains("; "),
            "the join is undone, not reprinted: {line}"
        );
    }

    #[test]
    fn a_ready_app_prints_no_reason_line() {
        let ready = json!({
            "status": { "phase": "Ready", "conditions": [
                { "type": "Ready", "status": "True", "reason": "Reconciled",
                  "message": "Reconcile completed; child Deployment applied." }
            ]}
        });
        assert!(
            format_not_ready_line(&ready).is_none(),
            "a healthy app must not carry a yellow line explaining its health"
        );
        // And the degenerate shapes: no status, no conditions, empty message.
        assert!(format_not_ready_line(&json!({})).is_none());
        assert!(format_not_ready_line(&env_secret_missing_cr("")).is_none());
    }

    #[test]
    fn an_unpinned_app_prints_no_pin_line() {
        assert!(format_pin_line(&json!({})).is_none());
        // And a REJECTED pin must not print one either: the operator writes
        // `status.image.pinned` only when the pin is honoured, so a status
        // line saying "held at X" while the workload follows the tag is
        // worse than silence.
        let rejected = json!({ "metadata": { "annotations": { "apprafter.io/image-pin": "bad" }},
                               "status": { "image": { "tag": "app:latest" }}});
        assert!(format_pin_line(&rejected).is_none());
    }

    #[test]
    fn format_image_line_shows_resolved_digest_and_age() {
        // 2.4h-e (ADR 0040). status.image carries the tag, the
        // resolved repo@sha256:… digest, and the resolution time.
        // The line shows the tag, the bare @sha256:… suffix, and a
        // deterministic relative age via the injected `now` seam.
        let cr = serde_json::json!({
            "status": {
                "image": {
                    "tag": "ghcr.io/acme/web:latest",
                    "resolved": "ghcr.io/acme/web@sha256:abc",
                    "resolvedAt": "2026-06-05T00:00:00Z"
                }
            }
        });
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-05T00:05:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let line = format_image_line(&cr, &now).expect("status.image present");
        assert!(line.contains("ghcr.io/acme/web:latest"));
        assert!(line.contains("@sha256:abc"));
        assert!(line.contains("5m"));
        assert!(line.contains("resolved"));
    }

    #[test]
    fn format_image_line_absent_when_no_status_image() {
        let now = chrono::Utc::now();
        assert_eq!(format_image_line(&serde_json::json!({}), &now), None);
        assert_eq!(
            format_image_line(&serde_json::json!({ "status": {} }), &now),
            None
        );
    }

    #[test]
    fn format_image_line_without_resolved_at_drops_age_suffix() {
        // Pre-first-resolve / unparseable timestamp: the line still
        // renders the tag + digest but omits the "(resolved … ago)"
        // suffix rather than echoing a bad timestamp.
        let now = chrono::Utc::now();
        let cr = serde_json::json!({
            "status": {
                "image": {
                    "tag": "ghcr.io/acme/web:1.0",
                    "resolved": "ghcr.io/acme/web@sha256:def"
                }
            }
        });
        let line = format_image_line(&cr, &now).expect("status.image present");
        assert!(line.contains("ghcr.io/acme/web:1.0"));
        assert!(line.contains("@sha256:def"));
        assert!(!line.contains("resolved"));
    }

    #[test]
    fn format_image_line_tag_only_on_resolve_failure() {
        // 2.4h Fix 1(B) seam: when the controller fails to resolve a
        // tag to a digest it now writes status.image = { tag } with
        // resolved/resolvedAt left absent (None). The line then shows
        // ONLY the tag — no "-> @sha256:…" digest, no "(resolved …
        // ago)" age suffix. This guards the now-reachable branch.
        let now = chrono::Utc::now();
        let cr = serde_json::json!({
            "status": {
                "image": {
                    "tag": "ghcr.io/acme/web:latest"
                }
            }
        });
        let line = format_image_line(&cr, &now).expect("status.image present");
        assert!(line.contains("ghcr.io/acme/web:latest"));
        assert!(!line.contains("@sha256"));
        assert!(!line.contains("->"));
        assert!(!line.contains("resolved"));
    }

    // ── format_recommendation_line (T10 / 2.16e) ──────────────────────

    #[test]
    fn format_reco_line_shows_capped_and_uncapped() {
        // When uncappedTarget differs from target the line must surface
        // both values + the "raise limits.memory" hint so operators know
        // to loosen the VPA ResourcePolicy cap.
        let cr = serde_json::json!({ "status": { "recommendedResources": {
            "recommendation": {
                "target": { "memory": "512Mi" },
                "uncappedTarget": { "memory": "900Mi" }
            }
        }}});
        let line = format_recommendation_line(&cr).unwrap();
        assert!(line.contains("512Mi"), "must show capped target: {line}");
        assert!(line.contains("900Mi"), "must show uncapped target: {line}");
        assert!(
            line.to_lowercase().contains("limits.memory"),
            "must reference limits.memory: {line}"
        );
    }

    #[test]
    fn format_reco_line_not_applied() {
        // When notApplied is set the reason must appear in the line
        // (e.g. operator chose not to apply due to node-capacity safety).
        let cr = serde_json::json!({ "status": { "recommendedResources": {
            "recommendation": { "target": { "memory": "400Mi" } },
            "notApplied": "recommendation not applied — node capacity"
        }}});
        let line = format_recommendation_line(&cr).unwrap();
        assert!(
            line.contains("not applied — node capacity"),
            "must surface notApplied reason: {line}"
        );
    }

    #[test]
    fn format_reco_line_none_when_absent() {
        // No recommendedResources field → None (VPA not installed or
        // first reconcile not yet done). Must not panic.
        assert!(format_recommendation_line(&serde_json::json!({ "status": {} })).is_none());
        assert!(format_recommendation_line(&serde_json::json!({})).is_none());
    }

    #[test]
    fn format_reco_line_no_uncapped_suffix_when_equal() {
        // When uncappedTarget matches target (no cap in effect) the
        // "uncapped … — raise limits.memory" hint must NOT appear.
        let cr = serde_json::json!({ "status": { "recommendedResources": {
            "recommendation": {
                "target": { "memory": "256Mi" },
                "uncappedTarget": { "memory": "256Mi" }
            }
        }}});
        let line = format_recommendation_line(&cr).unwrap();
        assert!(line.contains("256Mi"), "must show target: {line}");
        assert!(
            !line.contains("uncapped"),
            "must NOT show uncapped hint when values match: {line}"
        );
    }

    #[test]
    fn extract_tracked_resources_happy_multi_entry() {
        // Walk-fix #3 regression guard. status.resources[]
        // entries come back with the fields kubectl + Argo CD
        // both populate; health.status sits one level deep.
        let app = serde_json::json!({
            "status": {
                "resources": [
                    {
                        "group": "",
                        "kind": "Namespace",
                        "name": "apprafter-landing-web",
                        "status": "Synced",
                        "health": { "status": "Healthy" },
                        "version": "v1"
                    },
                    {
                        "group": "apprafter.io",
                        "kind": "Application",
                        "name": "landing-web",
                        "namespace": "apprafter",
                        "status": "Synced",
                        "health": { "status": "Healthy" },
                        "version": "v1alpha1"
                    }
                ]
            }
        });
        let rs = extract_tracked_resources(&app);
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].kind, "Namespace");
        assert_eq!(rs[0].namespace, "-"); // cluster-scoped
        assert_eq!(rs[1].name, "landing-web");
        assert_eq!(rs[1].namespace, "apprafter");
        assert_eq!(rs[1].health, "Healthy");
    }

    #[test]
    fn extract_tracked_resources_missing_status_returns_empty() {
        let app = serde_json::json!({ "spec": {} });
        assert!(extract_tracked_resources(&app).is_empty());
    }

    #[test]
    fn extract_tracked_resources_skips_entries_without_name() {
        let app = serde_json::json!({
            "status": {
                "resources": [
                    { "kind": "Service", "status": "Synced" }, // no name
                    { "name": "ok", "kind": "Service", "status": "Synced" }
                ]
            }
        });
        let rs = extract_tracked_resources(&app);
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].name, "ok");
    }

    #[test]
    fn parse_service_summaries_happy_two_service_list() {
        // 2.4g default-path services table. One ClusterIP with a
        // single port, one LoadBalancer with two ports — exercise
        // type/clusterIP defaults absent and the compact
        // <port>/<protocol> join (multi-port comma-separated).
        let payload = serde_json::json!({
            "items": [
                {
                    "metadata": { "name": "landing-web" },
                    "spec": {
                        "type": "ClusterIP",
                        "clusterIP": "10.43.0.10",
                        "ports": [{ "port": 3000, "protocol": "TCP" }]
                    }
                },
                {
                    "metadata": { "name": "landing-lb" },
                    "spec": {
                        "type": "LoadBalancer",
                        "clusterIP": "10.43.0.20",
                        "ports": [
                            { "port": 80, "protocol": "TCP" },
                            { "port": 443, "protocol": "TCP" }
                        ]
                    }
                }
            ]
        });
        let svcs = parse_service_summaries(&payload);
        assert_eq!(svcs.len(), 2);
        assert_eq!(svcs[0].name, "landing-web");
        assert_eq!(svcs[0].type_, "ClusterIP");
        assert_eq!(svcs[0].cluster_ip, "10.43.0.10");
        assert_eq!(svcs[0].ports, "3000/TCP");
        assert_eq!(svcs[1].type_, "LoadBalancer");
        assert_eq!(svcs[1].ports, "80/TCP,443/TCP");
    }

    #[test]
    fn parse_service_summaries_defaults_type_and_clusterip() {
        // Missing `spec.type` defaults to ClusterIP (k8s API
        // default); missing `spec.clusterIP` → `-`; missing
        // protocol on a port defaults to TCP.
        let payload = serde_json::json!({
            "items": [{
                "metadata": { "name": "bare" },
                "spec": { "ports": [{ "port": 8080 }] }
            }]
        });
        let svcs = parse_service_summaries(&payload);
        assert_eq!(svcs.len(), 1);
        assert_eq!(svcs[0].type_, "ClusterIP");
        assert_eq!(svcs[0].cluster_ip, "-");
        assert_eq!(svcs[0].ports, "8080/TCP");
    }

    #[test]
    fn parse_service_summaries_empty_or_missing_items_returns_empty() {
        assert!(parse_service_summaries(&serde_json::json!({ "items": [] })).is_empty());
        assert!(parse_service_summaries(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn parse_resource_claim_summaries_owned_claim_full_status() {
        // A claim owned by the target Application, fully
        // provisioned: provider bound, ready=true, secretRef set,
        // and a Scheduled=True condition.
        let payload = serde_json::json!({
            "items": [{
                "metadata": {
                    "name": "payments-pg",
                    "ownerReferences": [
                        { "kind": "Application", "name": "payments" }
                    ]
                },
                "status": {
                    "provider": "cnpg",
                    "ready": true,
                    "connectionSecretRef": "payments-pg-conn",
                    "conditions": [
                        { "type": "Scheduled", "status": "True" }
                    ]
                }
            }]
        });
        let claims = parse_resource_claim_summaries(&payload, "payments");
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].name, "payments-pg");
        assert_eq!(claims[0].provider, "cnpg");
        assert!(claims[0].ready);
        assert_eq!(claims[0].secret_ref.as_deref(), Some("payments-pg-conn"));
        assert!(claims[0].scheduled);
    }

    #[test]
    fn parse_resource_claim_summaries_filters_out_other_owners() {
        // A claim owned by a DIFFERENT Application must not leak
        // into this app's status — the namespace-wide list is
        // narrowed by ownerReference.
        let payload = serde_json::json!({
            "items": [{
                "metadata": {
                    "name": "other-pg",
                    "ownerReferences": [
                        { "kind": "Application", "name": "billing" }
                    ]
                },
                "status": { "provider": "cnpg", "ready": true }
            }]
        });
        let claims = parse_resource_claim_summaries(&payload, "payments");
        assert!(claims.is_empty());
    }

    #[test]
    fn parse_resource_claim_summaries_fresh_claim_no_status() {
        // A just-created claim owned by the target with NO
        // `.status` block yet — defaults: ready=false,
        // provider=`—`, scheduled=false, secret_ref=None.
        let payload = serde_json::json!({
            "items": [{
                "metadata": {
                    "name": "payments-pg",
                    "ownerReferences": [
                        { "kind": "Application", "name": "payments" }
                    ]
                }
            }]
        });
        let claims = parse_resource_claim_summaries(&payload, "payments");
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].name, "payments-pg");
        assert_eq!(claims[0].provider, "—");
        assert!(!claims[0].ready);
        assert!(!claims[0].scheduled);
        assert!(claims[0].secret_ref.is_none());
    }

    #[test]
    fn parse_resource_claim_summaries_empty_or_missing_items_returns_empty() {
        assert!(
            parse_resource_claim_summaries(&serde_json::json!({ "items": [] }), "x").is_empty()
        );
        assert!(parse_resource_claim_summaries(&serde_json::json!({}), "x").is_empty());
    }

    #[test]
    fn summarize_deployments_produces_a_row_per_environment() {
        // ADR 0044 (2.9): `app status web` aggregates `web-dev` and
        // `web-prod`, two SEPARATE Argo CD Applications grouped by the
        // `apprafter.io/application=web` label. Both env rows render,
        // each resolving its env from the `apprafter.io/environment`
        // label.
        let dev = serde_json::json!({
            "metadata": {
                "name": "web-dev",
                "labels": {
                    "apprafter.io/application": "web",
                    "apprafter.io/environment": "dev"
                }
            },
            "spec": { "destination": { "namespace": "web-dev" } },
            "status": {
                "sync": { "status": "Synced" },
                "health": { "status": "Healthy" }
            }
        });
        let prod = serde_json::json!({
            "metadata": {
                "name": "web-prod",
                "labels": {
                    "apprafter.io/application": "web",
                    "apprafter.io/environment": "prod"
                }
            },
            "spec": { "destination": { "namespace": "web-prod" } },
            "status": {
                "sync": { "status": "OutOfSync" },
                "health": { "status": "Progressing" }
            }
        });
        // Pass prod first to prove the (env, name) sort makes the order
        // deterministic regardless of input order.
        let rows = summarize_deployments(&[prod, dev]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].environment, "dev");
        assert_eq!(rows[0].argo_name, "web-dev");
        assert_eq!(rows[0].destination_namespace, "web-dev");
        assert_eq!(rows[0].sync, "Synced");
        assert_eq!(rows[0].health, "Healthy");
        assert_eq!(rows[1].environment, "prod");
        assert_eq!(rows[1].argo_name, "web-prod");
        assert_eq!(rows[1].sync, "OutOfSync");
        assert_eq!(rows[1].health, "Progressing");
    }

    #[test]
    fn summarize_deployments_falls_back_to_status_environment_then_base() {
        // No `apprafter.io/environment` label: resolve from
        // `status.environment`, then `(base)` for a pre-2.9 / base-only
        // app. Also exercises the Unknown sync/health defaults.
        let from_status = serde_json::json!({
            "metadata": { "name": "api-staging" },
            "status": { "environment": "staging" }
        });
        let base_only = serde_json::json!({
            "metadata": { "name": "legacy" },
            "spec": { "destination": { "namespace": "apprafter" } }
        });
        let rows = summarize_deployments(&[from_status, base_only]);
        assert_eq!(rows.len(), 2);
        // Sorted by (env, name): "(base)" sorts before "staging".
        assert_eq!(rows[0].environment, "(base)");
        assert_eq!(rows[0].argo_name, "legacy");
        assert_eq!(rows[0].sync, "Unknown");
        assert_eq!(rows[0].health, "Unknown");
        assert_eq!(rows[1].environment, "staging");
        assert_eq!(rows[1].argo_name, "api-staging");
    }

    #[test]
    fn app_row_populates_env_column_from_label_then_status_then_base() {
        // ADR 0044 (1.83j PART 2): `app list` surfaces an ENV column so a
        // per-environment deploy (`<name>-<env>`) is distinguishable at a
        // glance from a base-only one. Resolution mirrors
        // `deployment_environment`: label → status → `(base)`.
        let labelled = serde_json::json!({
            "metadata": {
                "name": "web-staging",
                "labels": { "apprafter.io/environment": "staging" }
            },
            "spec": { "project": "apps", "source": { "repoURL": "r", "targetRevision": "main" } }
        });
        assert_eq!(app_row(&labelled).env, "staging");

        let from_status = serde_json::json!({
            "metadata": { "name": "web-prod" },
            "status": { "environment": "prod" }
        });
        assert_eq!(app_row(&from_status).env, "prod");

        let base_only = serde_json::json!({
            "metadata": { "name": "legacy" },
            "spec": { "project": "apps" }
        });
        assert_eq!(app_row(&base_only).env, "(base)");
    }

    #[test]
    fn app_row_counts_the_workloads_a_registration_deploys() {
        let app = serde_json::json!({
            "metadata": {"name": "shop"},
            "spec": {"source": {"repoURL": "https://github.com/acme/shop"},
                     "destination": {"namespace": "shop"}},
            "status": {"resources": [
                {"group": "", "kind": "Namespace", "name": "shop"},
                {"group": "apprafter.io", "kind": "Application", "name": "api", "namespace": "shop"},
                {"group": "apprafter.io", "kind": "Application", "name": "web", "namespace": "shop"}
            ]}
        });
        let row = app_row(&app);
        assert_eq!(row.workloads, "2", "the Namespace is not a workload");
        assert_eq!(row.namespace, "shop");
        assert_eq!(
            row.repo, "github.com/acme/shop",
            "the scheme is stripped for width"
        );
    }

    #[test]
    fn app_row_reports_an_unsynced_registration_as_unmeasured() {
        // status.resources is absent until the first sync. `0` would read as
        // "broken" for an application that is merely new; the em-dash is this
        // repo's unmeasured marker.
        let app = serde_json::json!({"metadata": {"name": "fresh"},
                         "spec": {"source": {"repoURL": "https://x/y"}}});
        assert_eq!(app_row(&app).workloads, "\u{2014}");
    }

    #[test]
    fn app_row_keeps_a_non_https_repo_url_verbatim() {
        // DELIBERATELY not normalise_git_url: that REWRITES ssh:// to
        // https://, which is right on the write path and a lie here —
        // `--all-managed` surfaces registrations this CLI never wrote, and
        // showing an ssh remote as https tells the reader their Argo CD
        // clones over a protocol it does not.
        let app = serde_json::json!({"metadata": {"name": "x"},
                         "spec": {"source": {"repoURL": "ssh://git@github.com/acme/x"}}});
        assert_eq!(app_row(&app).repo, "ssh://git@github.com/acme/x");
    }

    #[test]
    fn app_row_counts_only_apprafter_applications() {
        // An app-of-apps child is an `argoproj.io` Application. The group is
        // recorded separately from the version in status.resources[], so the
        // group alone is the right discriminator here.
        let app = serde_json::json!({"metadata": {"name": "x"},
                         "spec": {"source": {"repoURL": "https://x/y"}},
                         "status": {"resources": [
            {"group": "apprafter.io", "kind": "Application", "name": "ours"},
            {"group": "argoproj.io", "kind": "Application", "name": "theirs"},
            {"group": "apprafter.io", "kind": "ServiceProvider", "name": "pg"}
        ]}});
        assert_eq!(app_row(&app).workloads, "1");
    }

    /// A registration deploying one workload per entry in `healths`.
    ///
    /// The top-level `status.health.status` is set to a value no fold can
    /// produce (`REGISTRATION-LEVEL`), so every assertion below proves the
    /// cell came from the WORKLOADS rather than coincidentally agreeing
    /// with the registration Argo CD would have reported.
    fn argo_with_workload_health(healths: &[&str]) -> Value {
        let resources: Vec<Value> = healths
            .iter()
            .enumerate()
            .map(|(i, h)| {
                json!({"group": "apprafter.io", "kind": "Application",
                       "name": format!("w{i}"), "namespace": "shop",
                       "health": {"status": h}})
            })
            .collect();
        json!({
            "metadata": {"name": "shop"},
            "spec": {"source": {"repoURL": "https://github.com/acme/shop"},
                     "destination": {"namespace": "shop"}},
            "status": {"health": {"status": "REGISTRATION-LEVEL"},
                       "resources": resources}
        })
    }

    #[test]
    fn health_cell_is_unchanged_when_every_workload_agrees() {
        // N=1 is today's entire fleet. It must render exactly as before, or
        // this is a cosmetic change to every existing user's table.
        for h in ["Healthy", "Degraded", "Progressing", "Missing"] {
            assert_eq!(app_row(&argo_with_workload_health(&[h])).health, h);
        }
        let three_agreeing = argo_with_workload_health(&["Healthy", "Healthy", "Healthy"]);
        assert_eq!(app_row(&three_agreeing).health, "Healthy");
    }

    #[test]
    fn health_cell_is_cardinal_when_workloads_disagree() {
        let app = argo_with_workload_health(&["Healthy", "Degraded", "Healthy"]);
        assert_eq!(app_row(&app).health, "Degraded 1/3");
    }

    #[test]
    fn health_cell_surfaces_a_pin_a_sibling_would_otherwise_mask() {
        // component_argocd.cue — Suspended overrides Healthy and nothing
        // else, so a Progressing sibling hides the pin on the Argo tile. The
        // cell must say it anyway; this listing is the only place a pin
        // shows up at all.
        let cell = app_row(&argo_with_workload_health(&["Suspended", "Progressing"])).health;
        assert!(cell.contains("1 pinned"), "got {cell}");
        // …and NOT when the aggregate is already the word `Suspended`
        // (it outranks Healthy, so it wins that pair): the suffix would
        // only repeat what the cell already says.
        assert_eq!(
            app_row(&argo_with_workload_health(&["Suspended", "Healthy"])).health,
            "Suspended 1/2"
        );
    }

    #[test]
    fn health_cell_falls_back_to_the_registration_when_no_workload_is_tracked() {
        // Before the first sync, and for a raw-YAML app surfaced by
        // --all-managed, there is no apprafter.io workload to fold.
        let app = json!({"metadata": {"name": "x"},
                         "spec": {"source": {"repoURL": "https://x/y"}},
                         "status": {"health": {"status": "Progressing"}}});
        assert_eq!(app_row(&app).health, "Progressing");
    }

    #[test]
    fn health_cell_names_the_worst_by_the_charts_ordering_not_by_count() {
        // gitops-engine's healthOrder, which component_argocd.cue's
        // "SECOND-healthiest" note is keyed to: Healthy < Suspended <
        // Progressing < Missing < Degraded < Unknown. The MINORITY status
        // is the one that leads the cell whenever it is the worse one —
        // "2/3 fine" is not the sentence a reader needs.
        // Deliberately no `Suspended` anywhere in here: the pin suffix is
        // the previous test's subject, and duplicating it would make both
        // tests flip on one defect.
        let one_degraded = argo_with_workload_health(&["Degraded", "Missing", "Missing"]);
        assert_eq!(app_row(&one_degraded).health, "Degraded 1/3");
        let one_missing = argo_with_workload_health(&["Missing", "Progressing", "Progressing"]);
        assert_eq!(app_row(&one_missing).health, "Missing 1/3");
        assert_eq!(
            app_row(&argo_with_workload_health(&["Progressing", "Healthy"])).health,
            "Progressing 1/2"
        );
    }

    #[test]
    fn health_cell_sorts_an_unrecognised_status_worst_not_healthiest() {
        // gitops-engine's own IsWorse leaves a code it cannot find at index
        // 0 — the HEALTHIEST slot — so an unheard-of status would vanish
        // behind the majority word. This fold puts it last instead, so the
        // reader is told a workload is in a state the table cannot name.
        assert_eq!(
            app_row(&argo_with_workload_health(&["Healthy", "Quiesced"])).health,
            "Quiesced 1/2"
        );
        // Including against Degraded, the worst code that is recognised.
        assert_eq!(
            app_row(&argo_with_workload_health(&["Degraded", "Quiesced"])).health,
            "Quiesced 1/2"
        );
    }

    #[test]
    fn health_cell_calls_an_unchecked_workload_unknown() {
        // An entry Argo CD has recorded but not yet health-checked carries
        // no `health` key. It is still a tracked workload, so the fold must
        // not drop it (that would misreport the denominator) — `Unknown` is
        // what the rest of this file calls an absent health.
        let app = json!({
            "metadata": {"name": "shop"},
            "spec": {"source": {"repoURL": "https://x/y"}},
            "status": {"health": {"status": "REGISTRATION-LEVEL"}, "resources": [
                {"group": "apprafter.io", "kind": "Application", "name": "a",
                 "health": {"status": "Healthy"}},
                {"group": "apprafter.io", "kind": "Application", "name": "b"}
            ]}
        });
        assert_eq!(app_row(&app).health, "Unknown 1/2");
    }

    #[test]
    fn health_cell_ignores_non_workload_resources() {
        // A Degraded Namespace or an app-of-apps child is not a workload of
        // this bundle and must not colour its verdict — the same GROUP
        // discriminator `WORKLOADS` counts by.
        let app = json!({
            "metadata": {"name": "shop"},
            "spec": {"source": {"repoURL": "https://x/y"}},
            "status": {"health": {"status": "REGISTRATION-LEVEL"}, "resources": [
                {"group": "", "kind": "Namespace", "name": "shop",
                 "health": {"status": "Degraded"}},
                {"group": "argoproj.io", "kind": "Application", "name": "theirs",
                 "health": {"status": "Missing"}},
                {"group": "apprafter.io", "kind": "Application", "name": "ours",
                 "health": {"status": "Healthy"}}
            ]}
        });
        let row = app_row(&app);
        assert_eq!(row.workloads, "1");
        // `starts_with`, not equality: whether one agreeing workload
        // renders verbatim is the subject of
        // `health_cell_is_unchanged_when_every_workload_agrees`, and
        // asserting it here too would make both flip on one defect. What
        // this test owns is that neither non-workload's status leaked in.
        assert!(row.health.starts_with("Healthy"), "got {}", row.health);
    }

    #[test]
    fn health_column_note_does_not_promise_pod_visibility() {
        // The footer is the whole apology for a column that reads like pod
        // health and is not. If it stops naming the command that DOES look
        // at pods, the column is back to implying something it cannot give.
        assert!(HEALTH_COLUMN_NOTE.contains("does not see pod state"));
        assert!(HEALTH_COLUMN_NOTE.contains("apprafter app status"));
    }

    #[test]
    fn deployment_environment_prefers_label_over_status() {
        let app = serde_json::json!({
            "metadata": { "labels": { "apprafter.io/environment": "dev" } },
            "status": { "environment": "prod" }
        });
        assert_eq!(deployment_environment(&app), "dev");
        let app2 = serde_json::json!({ "status": { "environment": "prod" } });
        assert_eq!(deployment_environment(&app2), "prod");
        let app3 = serde_json::json!({ "metadata": { "name": "x" } });
        assert_eq!(deployment_environment(&app3), "(base)");
    }
}

#[cfg(test)]
mod drift_tests {
    use super::*;

    #[test]
    fn a_pod_started_before_the_config_changed_is_stale() {
        assert!(pod_is_stale(
            Some("2026-08-31T10:00:00Z"),
            Some("2026-08-31T11:00:00Z")
        ));
    }

    #[test]
    fn a_pod_started_after_the_config_changed_is_current() {
        assert!(!pod_is_stale(
            Some("2026-08-31T12:00:00Z"),
            Some("2026-08-31T11:00:00Z")
        ));
    }

    #[test]
    fn a_missing_timestamp_never_reports_stale() {
        // Conservative on purpose: a false "your pods are stale" teaches the
        // reader to ignore the column, which costs more than a miss. An app
        // that binds no secrets has no changedAt at all and must stay quiet.
        assert!(!pod_is_stale(None, Some("2026-08-31T11:00:00Z")));
        assert!(!pod_is_stale(Some("2026-08-31T10:00:00Z"), None));
        assert!(!pod_is_stale(None, None));
    }

    #[test]
    fn an_unparseable_timestamp_never_reports_stale() {
        assert!(!pod_is_stale(
            Some("not-a-time"),
            Some("2026-08-31T11:00:00Z")
        ));
        assert!(!pod_is_stale(Some("2026-08-31T10:00:00Z"), Some("nope")));
    }

    #[test]
    fn offsets_are_compared_as_instants_not_as_strings() {
        // The operator writes RFC3339 with an offset (Utc::now().to_rfc3339()
        // yields +00:00) while the kubelet writes Z. A string comparison would
        // get this wrong; these are the same instant, so neither is stale.
        assert!(!pod_is_stale(
            Some("2026-08-31T11:00:00Z"),
            Some("2026-08-31T11:00:00+00:00")
        ));
        // And an offset that IS earlier must still register.
        assert!(pod_is_stale(
            Some("2026-08-31T10:00:00Z"),
            Some("2026-08-31T13:00:00+01:00")
        ));
    }

    #[test]
    fn a_pod_started_in_the_same_second_as_the_change_is_not_stale() {
        // THE FRESH-DEPLOY FALSE POSITIVE. `pod.status.startTime` is a
        // metav1.Time and arrives TRUNCATED TO SECONDS; the operator's
        // changedAt carries nanoseconds. A plain `<` therefore flagged every
        // pod created in the same second as the stamp — which, on a first
        // deployment, is every pod, because the Deployment apply and the digest
        // stamp happen in one reconcile.
        //
        // These are the exact values measured by e2e/env-and-secrets-walk.sh
        // on a seven-second-old pod that had never seen a rotation.
        assert!(
            !pod_is_stale(
                Some("2026-09-03T01:18:29Z"),
                Some("2026-09-03T01:18:29.943066898+00:00")
            ),
            "a pod whose truncated startTime shares the second with changedAt must not be flagged"
        );
    }

    #[test]
    fn a_genuinely_earlier_pod_is_still_stale_despite_the_truncation() {
        // The fix must not cost the signal. One second earlier is a different
        // whole second, so it still registers — as does anything older.
        assert!(pod_is_stale(
            Some("2026-09-03T01:18:28Z"),
            Some("2026-09-03T01:18:29.943066898+00:00")
        ));
        assert!(pod_is_stale(
            Some("2026-09-03T01:10:00Z"),
            Some("2026-09-03T01:18:29.943066898+00:00")
        ));
        // And a pod started in a LATER second is not stale, sub-seconds or not.
        assert!(!pod_is_stale(
            Some("2026-09-03T01:18:30Z"),
            Some("2026-09-03T01:18:29.943066898+00:00")
        ));
    }
}

#[cfg(test)]
mod backing_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_pooled_claim_names_its_instance_and_logical_db() {
        // "ready true, scheduled true" told an operator nothing about WHERE
        // the data went. A Dragonfly claim is a numbered DB on a shared
        // instance, and both halves matter: two apps on the same instance
        // are isolated only by that number.
        let c = json!({ "status": { "instance": "platform-redis-ephemeral-000", "dbnum": 7 }});
        assert_eq!(backing_resource(&c), "platform-redis-ephemeral-000 (db 7)");
    }

    #[test]
    fn an_instance_without_a_db_number_still_names_the_instance() {
        let c = json!({ "status": { "instance": "platform-redis-persistent-000" }});
        assert_eq!(backing_resource(&c), "platform-redis-persistent-000");
    }

    #[test]
    fn a_disk_claim_names_its_pvc() {
        let c = json!({ "status": { "volumeClaimRef": "web-disk-data" }});
        assert_eq!(backing_resource(&c), "pvc/web-disk-data");
    }

    #[test]
    fn an_unprovisioned_claim_shows_a_dash_rather_than_an_empty_cell() {
        assert_eq!(backing_resource(&json!({ "status": {} })), "—");
        assert_eq!(backing_resource(&json!({})), "—");
    }

    // ---- 2.5 (ADR 0061): jetstream ----

    #[test]
    fn a_jetstream_claim_names_the_streams_it_owns() {
        // The shape `jetstream_status_body` (resourceclaim-provisioner)
        // actually writes, observedAt and all.
        let c = json!({ "status": { "streams": {
            "declared": ["streamapp_orders", "streamapp_events"],
            "dynamic": [],
            "unattributed": [],
            "observedAt": "2026-09-11T10:00:00Z"
        }}});
        assert_eq!(backing_resource(&c), "2 streams");
    }

    #[test]
    fn one_stream_is_singular() {
        let c = json!({ "status": { "streams": {
            "declared": ["streamapp_orders"], "dynamic": [], "unattributed": []
        }}});
        assert_eq!(backing_resource(&c), "1 stream");
    }

    #[test]
    fn the_shape_a_live_cluster_actually_writes() {
        // Copied verbatim off `resourceclaim/walkapp-jetstream` in the
        // needs-jetstream walk's kind cluster (2026-09-12), a dynamicStreams
        // application that had created two streams at runtime. Fixtures
        // elsewhere in this module are hand-written; this one is evidence,
        // and it is what pins the key names and the timestamp format against
        // the provisioner rather than against my memory of it.
        let c = json!({ "status": { "streams": {
            "declared": [],
            "dynamic": ["negstream", "walkstream"],
            "observedAt": "2026-09-12T06:39:28.622117080+00:00",
            "unattributed": []
        }}});
        assert_eq!(backing_resource(&c), "2 streams (2 dynamic)");
    }

    #[test]
    fn dynamic_streams_are_counted_in_and_called_out() {
        // A dynamicStreams app creates streams the manifest never named.
        // They back the claim just as much, but an operator reading the row
        // needs to know which half of the number they cannot find in git.
        let c = json!({ "status": { "streams": {
            "declared": ["feeder_orders"],
            "dynamic": ["feeder_adhoc", "feeder_tmp"],
            "unattributed": []
        }}});
        assert_eq!(backing_resource(&c), "3 streams (2 dynamic)");
    }

    #[test]
    fn unattributed_streams_do_not_count_as_backing() {
        // Foreign/captured streams sitting in the account are reported as a
        // condition, not as this claim's backing — counting them would make
        // a capture look like capacity.
        let c = json!({ "status": { "streams": {
            "declared": ["streamapp_orders"],
            "dynamic": [],
            "unattributed": ["someone_elses", "and_another"]
        }}});
        assert_eq!(backing_resource(&c), "1 stream");
    }

    #[test]
    fn a_consume_only_claim_says_no_streams_rather_than_dash() {
        // Permanent and correct for a consume-only application, and it is a
        // MEASURED answer (the provisioner observed the account) — which is
        // precisely what "—" would erase.
        let c = json!({ "status": { "streams": {
            "declared": [], "dynamic": [], "unattributed": [],
            "observedAt": "2026-09-11T10:00:00Z"
        }}});
        assert_eq!(backing_resource(&c), "no streams");
    }

    #[test]
    fn a_jetstream_claim_before_its_first_observation_still_dashes() {
        // No `status.streams` yet → the provisioner has not looked at the
        // server. That is genuinely unknown, so it keeps the dash.
        let c = json!({ "status": { "provider": "jetstream-integrated", "ready": false }});
        assert_eq!(backing_resource(&c), "—");
    }

    #[test]
    fn an_instance_still_wins_over_a_stream_inventory() {
        // Ordering guard: the instance/PVC arms are checked first, so a
        // future backend that carried both would not have its instance
        // silently replaced by a stream count.
        let c = json!({ "status": {
            "instance": "platform-redis-ephemeral-000",
            "dbnum": 3,
            "streams": { "declared": ["x"], "dynamic": [], "unattributed": [] }
        }});
        assert_eq!(backing_resource(&c), "platform-redis-ephemeral-000 (db 3)");
    }
}

#[cfg(test)]
mod size_cell_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_disk_shows_used_total_and_a_percentage() {
        // The one claim shape with its own denominator, so a percentage is
        // a judgement rather than a number floating free.
        let c = json!({ "status": { "capacity": {
            "usedBytes": 4_294_967_296i64, "capacityBytes": 8_589_934_592i64 }}});
        assert_eq!(claim_size_cell(&c), "4.0 GB / 8.0 GB (50%)");
    }

    #[test]
    fn postgres_shows_bytes() {
        let c = json!({ "status": { "size": { "bytes": 12_582_912 }}});
        assert_eq!(claim_size_cell(&c), "12 MB");
    }

    #[test]
    fn redis_shows_keys_and_says_keys() {
        // Dragonfly cannot give bytes for a logical DB — per-DB memory is
        // accounted internally but summed across databases at every
        // emission point. Labelling a key count as a size would be the
        // invented number this column exists to avoid.
        let c = json!({ "status": { "size": { "keys": 1234 }}});
        assert_eq!(claim_size_cell(&c), "1234 keys");
    }

    #[test]
    fn an_unmeasured_claim_shows_a_dash_and_never_a_zero() {
        // "Not sampled" and "empty" are different facts. Rendering the first
        // as the second tells a tenant their database is empty when it is
        // merely unmeasured.
        assert_eq!(claim_size_cell(&json!({ "status": {} })), "—");
        assert_eq!(claim_size_cell(&json!({})), "—");
    }

    #[test]
    fn a_host_scoped_figure_names_the_host_and_leads_with_the_request() {
        // D29, measured on real hardware: a 1Gi claim whose kubelet figures are
        // the node's whole 74.8 GiB root filesystem. Rendering the pair
        // unlabelled told the reader their 1Gi volume holds 80GB.
        let c = json!({
            "spec": { "size": "1Gi" },
            "status": { "capacity": {
                "usedBytes": 9_616_949_248i64,
                "capacityBytes": 80_279_486_464i64,
                "scope": "host" }}});
        let cell = claim_size_cell(&c);
        assert!(
            cell.starts_with("1Gi"),
            "leads with what was asked for: {cell}"
        );
        assert!(cell.contains("host disk"), "names whose disk it is: {cell}");
        assert!(cell.contains("12%"), "keeps the useful fact: {cell}");
        assert!(
            !cell.contains("80.3") && !cell.contains(" / "),
            "must not present the node's capacity as the claim's: {cell}"
        );
    }

    #[test]
    fn a_volume_scoped_figure_is_unchanged_by_d29() {
        // A backend with a real quota reports its own numbers, and this
        // rendering must not move — the fix is about mislabelling, not about
        // the pair itself.
        let c = json!({ "spec": { "size": "8Gi" }, "status": { "capacity": {
            "usedBytes": 4_294_967_296i64, "capacityBytes": 8_589_934_592i64,
            "scope": "volume" }}});
        assert_eq!(claim_size_cell(&c), "4.0 GB / 8.0 GB (50%)");
    }

    #[test]
    fn a_figure_with_no_scope_keeps_the_pre_d29_rendering() {
        // Written by an operator that predates the field. Absence is unknown,
        // not "host": guessing would relabel every correct figure in the fleet
        // during a rolling upgrade.
        let c = json!({ "status": { "capacity": {
            "usedBytes": 4_294_967_296i64, "capacityBytes": 8_589_934_592i64 }}});
        assert_eq!(claim_size_cell(&c), "4.0 GB / 8.0 GB (50%)");
    }

    #[test]
    fn a_zero_capacity_sample_does_not_become_a_percentage() {
        // An unusable sample, not an empty disk — and dividing by it would
        // be a nonsense rather than a crash, which is worse.
        let c = json!({ "status": { "capacity": { "usedBytes": 5, "capacityBytes": 0 }}});
        assert_eq!(claim_size_cell(&c), "—");
    }

    #[test]
    fn human_bytes_switches_precision_so_columns_stay_narrow() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(15 * 1024 * 1024), "15 MB");
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use serde_json::json;

    fn tracked(name: &str, kind: &str, ns: &str, status: &str, health: &str) -> TrackedResource {
        TrackedResource {
            name: name.into(),
            kind: kind.into(),
            namespace: ns.into(),
            status: status.into(),
            health: health.into(),
        }
    }

    #[test]
    fn the_resource_table_widens_every_column_to_its_longest_cell() {
        // The whole point of a hand-rolled table: a name longer than its
        // header must not push the next column out of alignment, and the
        // header row must move with it.
        let rows = [
            tracked("web", "Service", "apps", "Synced", "Healthy"),
            tracked(
                "web-deployment-long",
                "Deployment",
                "production",
                "OutOfSync",
                "Progressing",
            ),
        ];
        let lines = render_tracked_resource_lines(&rows);
        let header = &lines[2];
        let first = &lines[3];
        let second = &lines[4];
        for (head, short, long) in [
            ("KIND", "Service", "Deployment"),
            ("NAMESPACE", "apps", "production"),
            ("STATUS", "Synced", "OutOfSync"),
            ("HEALTH", "Healthy", "Progressing"),
        ] {
            assert_eq!(
                header.find(head),
                first.find(short),
                "{head} must start where its header does: {header:?} / {first:?}"
            );
            assert_eq!(
                first.find(short),
                second.find(long),
                "both rows must start {head} at the same offset: {first:?} / {second:?}"
            );
        }
        assert!(
            first.ends_with("Healthy"),
            "health is the last, unpadded column: {first:?}"
        );
    }

    #[test]
    fn a_resource_table_of_short_values_still_clears_its_own_headers() {
        // The width floors: `NAME` is 4, `NAMESPACE` 9, `STATUS` 6, and a
        // one-row table of short cells would otherwise print HEALTH on top
        // of the NAMESPACE header.
        let rows = [tracked("ui", "Pod", "ns", "OK", "Healthy")];
        let lines = render_tracked_resource_lines(&rows);
        let header = &lines[2];
        let row = &lines[3];
        assert_eq!(
            header.find("NAMESPACE"),
            row.find("ns"),
            "{header:?} / {row:?}"
        );
        assert_eq!(
            header.find("STATUS"),
            row.find("OK"),
            "{header:?} / {row:?}"
        );
        assert_eq!(
            header.find("HEALTH"),
            row.find("Healthy"),
            "{header:?} / {row:?}"
        );
    }

    #[test]
    fn a_never_synced_application_says_so_instead_of_printing_an_empty_table() {
        // An empty table under a header reads as "Argo tracks nothing";
        // the sentence says it is Argo that has not reported yet.
        let lines = render_tracked_resource_lines(&[]);
        assert_eq!(lines.len(), 3, "blank, header, sentence: {lines:?}");
        assert!(lines[2].contains("status.resources[]"), "{:?}", lines[2]);
    }

    fn pod(name: &str, started_at: Option<&str>) -> PodSummary {
        PodSummary {
            name: name.into(),
            ready: "1/1".into(),
            status: "Running".into(),
            restarts: 0,
            age: "5m".into(),
            started_at: started_at.map(String::from),
        }
    }

    #[test]
    fn only_the_pods_older_than_the_config_carry_the_drift_marker() {
        // INVARIANT (2.22c / D6): the marker is PER POD. During a rollout
        // the old and new replicas coexist, and flagging both would tell an
        // operator the restart did nothing.
        let pods = [
            pod("web-old", Some("2026-08-31T10:00:00Z")),
            pod("web-new", Some("2026-08-31T12:00:00Z")),
        ];
        let lines = render_pod_summary_lines(&pods, "web", "apps", Some("2026-08-31T11:00:00Z"));
        let old = lines.iter().find(|l| l.text.contains("web-old")).unwrap();
        let new = lines.iter().find(|l| l.text.contains("web-new")).unwrap();
        assert!(old.warn && old.text.contains("← old config"), "{old:?}");
        assert!(!new.warn && !new.text.contains("← old config"), "{new:?}");
    }

    #[test]
    fn the_drift_explanation_appears_only_when_some_pod_actually_drifted() {
        // The paragraph explains a marker. Printing it with no marked row
        // is an unexplained warning on a healthy app.
        let pods = [pod("web-new", Some("2026-08-31T12:00:00Z"))];
        let clean = render_pod_summary_lines(&pods, "web", "apps", Some("2026-08-31T11:00:00Z"));
        assert!(
            !clean
                .iter()
                .any(|l| l.text.contains("resolved once at pod start")),
            "{clean:?}"
        );
        let pods = [pod("web-old", Some("2026-08-31T10:00:00Z"))];
        let drifted = render_pod_summary_lines(&pods, "web", "apps", Some("2026-08-31T11:00:00Z"));
        assert!(
            drifted
                .iter()
                .any(|l| l.warn && l.text.contains("resolved once at pod start")),
            "{drifted:?}"
        );
    }

    #[test]
    fn a_workload_with_no_pods_names_the_two_reasons_rather_than_showing_a_bare_table() {
        let lines = render_pod_summary_lines(&[], "web", "apps", None);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines[1].text.contains("app.kubernetes.io/name=web"),
            "{:?}",
            lines[1]
        );
        assert!(lines[2].text.contains("spec.expose"), "{:?}", lines[2]);
        assert!(!lines.iter().any(|l| l.warn), "an absence is not a warning");
    }

    fn svc(name: &str, type_: &str, ip: &str, ports: &str) -> ServiceSummary {
        ServiceSummary {
            name: name.into(),
            type_: type_.into(),
            cluster_ip: ip.into(),
            ports: ports.into(),
        }
    }

    #[test]
    fn the_service_table_pads_cluster_ip_to_its_header_even_when_every_value_is_shorter() {
        // `CLUSTER-IP` is 10 chars and an IP can be 8 ("10.0.0.1"). Without
        // the floor the PORTS column would slide under the IP header.
        let rows = [svc("web", "ClusterIP", "10.0.0.1", "80/TCP")];
        let lines = render_service_lines(&rows, "web", "apps");
        assert_eq!(
            lines[2].find("PORTS"),
            lines[3].find("80/TCP"),
            "{:?} / {:?}",
            lines[2],
            lines[3]
        );
    }

    #[test]
    fn a_service_less_app_is_told_expose_may_simply_be_omitted() {
        let lines = render_service_lines(&[], "web", "apps");
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[2].contains("spec.expose"), "{:?}", lines[2]);
    }

    fn claim(
        name: &str,
        provider: &str,
        ready: bool,
        secret: Option<&str>,
    ) -> ResourceClaimSummary {
        ResourceClaimSummary {
            name: name.into(),
            provider: provider.into(),
            ready,
            secret_ref: secret.map(String::from),
            backing: "pvc/data".into(),
            size: "1Gi".into(),
            scheduled: true,
        }
    }

    #[test]
    fn a_claim_row_spells_its_booleans_and_dashes_a_missing_secret() {
        // A blank READY cell and a blank SECRET cell look identical, and
        // one of them means "not ready" while the other means "not yet
        // bound" — both need a token the reader can see.
        let rows = [claim("web-db", "postgres", false, None)];
        let lines = render_resource_claim_lines(&rows, "apps");
        let row = &lines[3];
        assert!(row.contains("false"), "{row:?}");
        assert!(row.trim_end().ends_with('-'), "{row:?}");
        let bound = render_resource_claim_lines(
            &[claim("web-db", "postgres", true, Some("web-db-conn"))],
            "apps",
        );
        assert!(
            bound[3].trim_end().ends_with("web-db-conn"),
            "{:?}",
            bound[3]
        );
        assert!(bound[3].contains("true"), "{:?}", bound[3]);
    }

    #[test]
    fn an_app_with_no_needs_is_told_so_rather_than_shown_an_empty_claims_table() {
        let lines = render_resource_claim_lines(&[], "apps");
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[1].contains("apps"), "{:?}", lines[1]);
        assert!(lines[2].contains("needs.*"), "{:?}", lines[2]);
    }

    #[test]
    fn the_secrets_block_lists_every_binding_with_its_scope() {
        let cr = json!({
            "metadata": { "name": "web", "namespace": "apps" },
            "spec": {
                "base": { "env": { "DATABASE_URL": { "secret": "db-creds/url" } } },
                "environments": { "prod": { "env": { "API_KEY": { "secret": "vault/api" } } } }
            }
        });
        let lines = render_secret_binding_lines(&cr, "web", "apps");
        assert!(lines[1].contains("Secrets (apps/web)"), "{:?}", lines[1]);
        let body = lines[3..].join("\n");
        assert!(body.contains("DATABASE_URL"), "{body}");
        assert!(body.contains("db-creds/url"), "{body}");
        assert!(body.contains("(base)"), "{body}");
        assert!(body.contains("API_KEY"), "{body}");
        assert!(body.contains("vault/api"), "{body}");
        assert!(body.contains("(prod)"), "{body}");
    }

    #[test]
    fn an_app_that_binds_no_secrets_renders_no_secrets_block_at_all() {
        // INVARIANT: not an empty header. A `Secrets:` heading with nothing
        // under it reads as "your secrets went missing".
        let cr = json!({
            "metadata": { "name": "web", "namespace": "apps" },
            "spec": { "base": { "env": { "PORT": "8080" } } }
        });
        assert!(render_secret_binding_lines(&cr, "web", "apps").is_empty());
    }

    #[test]
    fn recent_revisions_shows_the_newest_three_newest_first() {
        // INVARIANT: Argo CD APPENDS to status.history, so the newest entry
        // is LAST. Rendering the array as-is would show the three OLDEST
        // deploys under a heading that says "recent".
        let app = json!({ "status": { "history": [
            { "id": 1, "revision": "aaa", "deployedAt": "d1" },
            { "id": 2, "revision": "bbb", "deployedAt": "d2" },
            { "id": 3, "revision": "ccc", "deployedAt": "d3" },
            { "id": 4, "revision": "ddd", "deployedAt": "d4" }
        ]}});
        let lines = recent_revision_lines(&app);
        assert_eq!(lines.len(), 5, "blank + header + 3 rows: {lines:?}");
        assert!(lines[1].contains("last 3"), "{:?}", lines[1]);
        assert!(
            lines[2].contains("ddd") && lines[2].contains("#  4"),
            "{:?}",
            lines[2]
        );
        assert!(lines[3].contains("ccc"), "{:?}", lines[3]);
        assert!(lines[4].contains("bbb"), "{:?}", lines[4]);
        assert!(!lines.iter().any(|l| l.contains("aaa")), "{lines:?}");
    }

    #[test]
    fn a_shorter_history_reports_its_own_length_not_three() {
        let app = json!({ "status": { "history": [
            { "id": 7, "revision": "aaa", "deployedAt": "d1" }
        ]}});
        let lines = recent_revision_lines(&app);
        assert!(lines[1].contains("last 1"), "{:?}", lines[1]);
        assert_eq!(lines.len(), 3, "{lines:?}");
    }

    #[test]
    fn an_app_with_no_history_renders_no_revision_heading() {
        assert!(recent_revision_lines(&json!({})).is_empty());
        assert!(recent_revision_lines(&json!({ "status": { "history": [] } })).is_empty());
    }

    fn summary(argo_name: &str, env: &str) -> DeploymentSummary {
        DeploymentSummary {
            argo_name: argo_name.into(),
            environment: env.into(),
            destination_namespace: "apps".into(),
            sync: "Synced".into(),
            health: "Healthy".into(),
        }
    }

    #[test]
    fn a_multi_environment_app_gets_an_index_naming_each_deployment() {
        let lines = env_deployment_index_lines(
            "web",
            &[summary("web-dev", "dev"), summary("web-prod", "prod")],
        );
        assert!(
            lines[0].contains("2 environment deployments"),
            "{:?}",
            lines[0]
        );
        assert!(
            lines[1].contains("web-dev") && lines[1].contains("(dev)"),
            "{:?}",
            lines[1]
        );
        assert!(lines[2].contains("web-prod"), "{:?}", lines[2]);
        assert_eq!(lines[3], "", "trailing blank separates index from detail");
    }

    #[test]
    fn a_single_deployment_gets_no_index_header() {
        // INVARIANT: the index summarises a choice. With one deployment
        // there is none, and "1 environment deployments" above the block it
        // describes is noise on the common case.
        assert!(env_deployment_index_lines("web", &[summary("web", "(base)")]).is_empty());
        assert!(env_deployment_index_lines("web", &[]).is_empty());
    }

    #[test]
    fn the_advisory_block_orders_phase_reason_image_pin_and_marks_the_right_ones_yellow() {
        // INVARIANT: order is the message — the reason must sit directly
        // under the phase it explains, and the pin under the digest it
        // qualifies. Colour is a claim about severity: the pin and the
        // failure reason are advisories, the image line is a fact.
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-05T00:05:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let cr = json!({
            "metadata": { "name": "web" },
            "status": {
                "phase": "EnvSecretMissing",
                "conditions": [
                    { "type": "Ready", "status": "False",
                      "reason": "EnvSecretMissing", "message": "secret apps/db-creds not found" }
                ],
                "image": {
                    "tag": "ghcr.io/acme/web:latest",
                    "resolved": "ghcr.io/acme/web@sha256:abc",
                    "resolvedAt": "2026-06-05T00:00:00Z",
                    "pinned": { "resolved": "ghcr.io/acme/web@sha256:abc",
                                "at": "2026-06-04T00:00:00Z" }
                }
            }
        });
        let lines = apprafter_cr_advisory_lines(&cr, &now);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(lines[0].text, "AppRafter phase: EnvSecretMissing");
        assert!(!lines[0].warn);
        assert!(
            lines[1].warn && lines[1].text.contains("db-creds"),
            "{:?}",
            lines[1]
        );
        assert!(
            !lines[2].warn && lines[2].text.contains("@sha256:abc"),
            "{:?}",
            lines[2]
        );
        assert!(
            lines[3].warn && lines[3].text.contains("unpin"),
            "{:?}",
            lines[3]
        );
    }

    #[test]
    fn a_healthy_app_produces_no_advisory_lines_beyond_its_phase() {
        let now = chrono::Utc::now();
        let cr = json!({ "status": { "phase": "Ready", "conditions": [
            { "type": "Ready", "status": "True" }
        ]}});
        let lines = apprafter_cr_advisory_lines(&cr, &now);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].text, "AppRafter phase: Ready");
        assert!(apprafter_cr_advisory_lines(&json!({}), &now).is_empty());
    }
}

#[cfg(test)]
mod add_path_tests {
    use super::*;

    #[test]
    fn an_env_the_manifest_declares_is_admitted() {
        // Both the sole-declaration and the one-among-several case: the
        // check is membership, not "some other environment also exists".
        assert_eq!(undeclared_env_error("prod", &["prod".to_string()]), None);
        assert_eq!(
            undeclared_env_error("prod", &["dev".to_string(), "prod".to_string()]),
            None
        );
    }

    #[test]
    fn an_undeclared_env_is_refused_and_the_declared_ones_are_listed() {
        let msg = undeclared_env_error("stage", &["dev".to_string(), "prod".to_string()])
            .expect("stage is not declared");
        assert!(msg.contains("'stage'"), "{msg}");
        assert!(msg.contains("dev, prod"), "{msg}");
        assert!(msg.contains("spec.environments.stage"), "{msg}");
    }

    #[test]
    fn a_manifest_declaring_no_environments_still_refuses_rather_than_allowing_anything() {
        // INVARIANT: "nothing declared" must not read as "everything
        // allowed". Admitting it registers an Argo CD Application whose
        // environment renders to nothing, and the failure lands later in
        // Argo CD rather than here.
        let msg = undeclared_env_error("dev", &[]).expect("nothing is declared");
        assert!(msg.contains("(none declared)"), "{msg}");
    }

    #[test]
    fn the_collision_refusal_suggests_the_logical_name_never_the_argo_identity() {
        // The logical-name UX rule: `app status` / `app remove` take the
        // LOGICAL name; echoing back `web-prod` would hand the reader a
        // name none of our verbs accept.
        let msg = already_registered_error("web-prod", "web", Some("prod"));
        assert!(msg.contains("'web-prod' is already registered"), "{msg}");
        assert!(msg.contains("apprafter app status web"), "{msg}");
        assert!(
            !msg.contains("apprafter app status web-prod"),
            "must not suggest the Argo identity: {msg}"
        );
        assert!(msg.contains("apprafter app remove web --env prod"), "{msg}");
    }

    #[test]
    fn a_base_only_collision_drops_the_env_flag_from_its_remove_hint() {
        let msg = already_registered_error("web", "web", None);
        assert!(msg.contains("`apprafter app remove web`"), "{msg}");
        assert!(
            !msg.contains("apprafter app remove web --env"),
            "there is no environment to name: {msg}"
        );
    }

    #[test]
    fn the_registration_summary_states_an_explicit_environment_flatly() {
        let lines = registration_summary_lines(
            "web-prod",
            "apps",
            "https://github.com/acme/web",
            "main",
            ".",
            "production",
            Some("prod"),
            Some("dev"),
        );
        assert!(
            lines[0].contains("'web-prod'") && lines[0].contains("'apps'"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("https://github.com/acme/web"),
            "{lines:?}"
        );
        assert!(lines[2].contains("main"), "{lines:?}");
        assert!(lines[4].contains("production"), "{lines:?}");
        assert_eq!(lines[5], "  Environment: prod");
        assert!(
            !lines[5].contains("dev"),
            "an explicit --env must not mention the cluster default: {:?}",
            lines[5]
        );
    }

    #[test]
    fn a_base_only_registration_names_the_cluster_default_and_how_to_pin_it() {
        let lines = registration_summary_lines(
            "web",
            "apps",
            "https://github.com/acme/web",
            "main",
            ".",
            "production",
            None,
            Some("dev"),
        );
        assert!(
            lines[5].contains("cluster default is 'dev'"),
            "{:?}",
            lines[5]
        );
        assert!(lines[5].contains("--env dev"), "{:?}", lines[5]);
    }

    #[test]
    fn a_base_only_registration_with_no_platformstack_omits_the_environment_line() {
        // INVARIANT: silence rather than a claim. Printing `(base)` with no
        // default would assert the cluster has none, which we did not learn.
        let lines = registration_summary_lines(
            "web",
            "apps",
            "https://github.com/acme/web",
            "main",
            ".",
            "production",
            None,
            None,
        );
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.contains("Environment")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_validated_credential_admits_the_repo() {
        assert_eq!(
            confirmed_coverage_error("https://github.com/acme/web", true, true),
            None
        );
    }

    #[test]
    fn covered_but_unvalidated_and_not_covered_at_all_are_different_refusals() {
        // INVARIANT: collapsing them sends an operator who already
        // registered the right credential off to register a second one,
        // when what they need is to validate the one they have.
        let unvalidated =
            confirmed_coverage_error("https://github.com/acme/web", false, true).expect("refused");
        assert!(unvalidated.contains("GitValid=True"), "{unvalidated}");
        assert!(!unvalidated.contains("repo creds add"), "{unvalidated}");

        let uncovered =
            confirmed_coverage_error("https://github.com/acme/web", false, false).expect("refused");
        assert!(uncovered.contains("repo creds add"), "{uncovered}");
        assert!(!uncovered.contains("GitValid=True"), "{uncovered}");
    }

    #[test]
    fn a_known_provider_gets_a_numbered_pat_walkthrough_with_the_command_prefilled() {
        let s = CredsSuggestion {
            suggested_name: "github-acme".into(),
            url_prefix: "https://github.com/acme".into(),
            pat_creation_url: Some("https://github.com/settings/tokens/new".into()),
        };
        let lines = creds_notice_lines("https://github.com/acme/web", Some(&s));
        let body = lines.join("\n");
        assert!(
            body.contains("https://github.com/settings/tokens/new"),
            "{body}"
        );
        assert!(body.contains("1. Generate a PAT here"), "{body}");
        assert!(body.contains("2. Register it with AppRafter"), "{body}");
        assert!(
            body.contains(
                "apprafter repo creds add github-acme --url-prefix https://github.com/acme"
            ),
            "{body}"
        );
    }

    #[test]
    fn an_unknown_provider_still_gets_a_command_with_its_name_and_prefix_filled_in() {
        // INVARIANT: the notice degrades in TWO steps. Without a PAT URL
        // the operator still gets a runnable command, not just advice.
        let s = CredsSuggestion {
            suggested_name: "gitea-acme".into(),
            url_prefix: "https://git.acme.dev/acme".into(),
            pat_creation_url: None,
        };
        let body = creds_notice_lines("https://git.acme.dev/acme/web", Some(&s)).join("\n");
        assert!(
            body.contains(
                "apprafter repo creds add gitea-acme --url-prefix https://git.acme.dev/acme"
            ),
            "{body}"
        );
        assert!(!body.contains("Generate a PAT here"), "{body}");
    }

    #[test]
    fn an_unparseable_url_still_gets_the_command_shape_with_placeholders() {
        let body = creds_notice_lines("https://weird", None).join("\n");
        assert!(
            body.contains("apprafter repo creds add <name> --url-prefix <prefix> --token <pat>"),
            "{body}"
        );
        assert!(body.contains("https://weird is private"), "{body}");
    }

    #[test]
    fn a_remote_url_is_trimmed_and_normalised_to_the_https_shape_argo_accepts() {
        assert_eq!(
            remote_url_or_error("origin", "git@github.com:acme/web.git\n").unwrap(),
            "https://github.com/acme/web"
        );
    }

    #[test]
    fn a_blank_remote_url_is_an_error_rather_than_an_empty_repo_url() {
        // INVARIANT: `git remote get-url` exits 0 for a remote configured
        // with a blank URL. Letting that through registers an Application
        // pointing at nothing, and the failure surfaces later in Argo CD.
        let err = remote_url_or_error("origin", "  \n ").unwrap_err();
        assert!(
            format!("{err}").contains("git remote origin returned an empty URL"),
            "{err}"
        );
    }

    #[test]
    fn an_auth_failure_from_ls_remote_is_routed_to_repo_creds_add() {
        // INVARIANT: git's wording varies by transport, so the match is
        // case-insensitive. Misclassifying it sends the operator hunting
        // for a typo in a URL that is perfectly correct.
        for stderr in [
            "remote: Authentication failed for 'https://github.com/acme/web'",
            "git@github.com: Permission denied (publickey).",
            "fatal: PERMISSION DENIED",
        ] {
            let msg = ls_remote_failure_message("https://github.com/acme/web", stderr, Some(128));
            assert!(msg.contains("refused access"), "{stderr} -> {msg}");
            assert!(
                msg.contains("apprafter repo creds add"),
                "{stderr} -> {msg}"
            );
        }
    }

    #[test]
    fn a_non_auth_failure_reports_the_exit_code_and_stays_out_of_the_creds_advice() {
        let msg = ls_remote_failure_message(
            "https://github.com/acme/web",
            "fatal: repository not found",
            Some(128),
        );
        assert!(msg.contains("exit Some(128)"), "{msg}");
        assert!(msg.contains("repository not found"), "{msg}");
        assert!(
            !msg.contains("apprafter repo creds add"),
            "a missing repo is not a credentials problem: {msg}"
        );
    }
}

#[cfg(test)]
mod list_filter_tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({ "items": [
            { "metadata": { "name": "web" },   "spec": { "project": "apps" } },
            { "metadata": { "name": "infra" }, "spec": { "project": "platform" } },
            { "metadata": { "name": "orphan" } }
        ]})
    }

    #[test]
    fn a_project_scoped_listing_keeps_only_that_projects_applications() {
        let kept = filter_apps_for_list(&payload(), "apps", false);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].pointer("/metadata/name").unwrap(), "web");
    }

    #[test]
    fn an_application_with_no_project_is_dropped_from_a_scoped_listing() {
        // INVARIANT: the filter is an allow-list on an exact match, so an
        // unparseable CR cannot leak into a project-scoped listing.
        let kept = filter_apps_for_list(&payload(), "apps", false);
        assert!(
            !kept
                .iter()
                .any(|a| a.pointer("/metadata/name").unwrap() == "orphan"),
            "{kept:?}"
        );
    }

    #[test]
    fn all_projects_keeps_every_item_including_the_project_less_one() {
        let kept = filter_apps_for_list(&payload(), "apps", true);
        assert_eq!(kept.len(), 3, "{kept:?}");
    }

    #[test]
    fn a_payload_without_items_yields_no_rows_rather_than_panicking() {
        assert!(filter_apps_for_list(&json!({}), "apps", false).is_empty());
        assert!(filter_apps_for_list(&json!({ "items": null }), "apps", true).is_empty());
    }

    #[test]
    fn the_default_listing_is_filtered_to_apprafter_managed_applications() {
        // INVARIANT: `argocd` also holds the platform's own root
        // Application and every component under it. Listing those next to a
        // developer's apps buries the answer; `--all-managed` is the opt-in.
        assert_eq!(
            list_label_selector(false),
            Some("apprafter.io/managed-by=apprafter")
        );
        assert_eq!(list_label_selector(true), None);
    }

    #[test]
    fn the_empty_listing_names_the_project_it_searched() {
        let lines = empty_list_lines("apps", false, false);
        assert!(lines[0].contains("AppProject 'apps'"), "{lines:?}");
        let cluster = empty_list_lines("apps", true, false);
        assert!(cluster[0].contains("in the cluster"), "{cluster:?}");
        assert!(!cluster[0].contains("apps"), "{cluster:?}");
    }

    #[test]
    fn the_all_managed_hint_is_suppressed_once_that_flag_is_already_set() {
        // INVARIANT: suggesting a flag the reader just passed is how a CLI
        // teaches people to stop reading its hints.
        assert_eq!(empty_list_lines("apps", false, true).len(), 1);
        let unset = empty_list_lines("apps", false, false);
        assert_eq!(unset.len(), 2, "{unset:?}");
        assert!(unset[1].contains("--all-managed"), "{unset:?}");
    }
}

#[cfg(test)]
mod remove_plan_tests {
    use super::*;
    use crate::commands::app_index::{Resolution, WorkloadEntry};
    use serde_json::json;

    fn argo(name: &str) -> Value {
        json!({ "metadata": { "name": name } })
    }

    /// One registration whose `status.resources[]` lists `workloads` as
    /// `apprafter.io` `Application` CRs in `shop`, plus any `extra`
    /// entries verbatim — the exact JSON `remove` already holds after its
    /// pre-flight read.
    fn bundle(workloads: &[&str], extra: Vec<Value>) -> Value {
        let mut resources: Vec<Value> = workloads
            .iter()
            .map(|w| {
                json!({ "group": "apprafter.io", "kind": "Application",
                        "version": "v1alpha1", "name": w, "namespace": "shop" })
            })
            .collect();
        resources.extend(extra);
        json!({
            "metadata": { "name": "shop-reg" },
            "spec": {
                "project": "apps",
                "source": { "repoURL": "https://github.com/acme/shop" },
                "destination": { "namespace": "shop" },
            },
            "status": { "resources": resources },
        })
    }

    /// One indexed workload, as `AppIndex::resolve` hands it back.
    fn workload(name: &str, namespace: &str, registration: Option<&str>) -> WorkloadEntry {
        WorkloadEntry {
            cr: json!({ "apiVersion": "apprafter.io/v1alpha1", "kind": "Application",
                        "metadata": { "name": name, "namespace": namespace } }),
            name: name.to_string(),
            namespace: namespace.to_string(),
            registration: registration.map(str::to_string),
        }
    }

    #[test]
    fn removing_one_workload_of_a_bundle_is_refused_with_the_git_steps() {
        // ADR 0062 §Write surfaces. `app add` writes
        // `syncPolicy.automated.selfHeal: true`, so deleting ONE CR from
        // the CLI is undone within a reconcile. Performing that delete
        // would report success for a change that does not survive the
        // next sync, so the only honest plan is a refusal.
        assert_eq!(
            plan_remove_target(
                "api",
                &Resolution::Workload(workload("api", "shop", Some("shop-reg"))),
                3,
            ),
            RemoveTarget::RefuseWorkload {
                workload: "api".into(),
                registration: "shop-reg".into(),
                bundle_size: 3,
            },
        );
    }

    #[test]
    fn removing_the_sole_workload_of_a_registration_names_the_application_instead() {
        // At N = 1 nothing about the intent is unsupported — the caller
        // named the workload when the verb takes the application — so
        // this is not the git-steps refusal. It is still an ERROR, not a
        // redirect: ADR 0062 §Addressing makes the positional always the
        // registration, and the error path's job is to name the command
        // that would have worked. The shape that reaches here is the
        // pre-2.9 name mismatch this module's own test already asserts is
        // supported — registration `cms-prod` renders a CR `landing-cms`.
        assert_eq!(
            plan_remove_target(
                "landing-cms",
                &Resolution::Workload(workload("landing-cms", "cms", Some("cms-prod"))),
                1,
            ),
            RemoveTarget::NameTheApplication {
                workload: "landing-cms".into(),
                registration: "cms-prod".into(),
            },
        );
        let msg = name_the_application_lines("landing-cms", "cms-prod").join("\n");
        assert!(msg.contains("'landing-cms' is not an application"), "{msg}");
        assert!(
            msg.contains("only workload of application 'cms-prod'"),
            "{msg}"
        );
        assert!(msg.contains("apprafter app remove cms-prod"), "{msg}");
        // NOT the git steps: deleting the block is not the route to what
        // this caller asked for, and offering it would be noise.
        assert!(!msg.contains("commit and push"), "{msg}");
    }

    #[test]
    fn yes_cannot_make_a_workload_name_reach_the_delete_path() {
        // `--yes` skips the CONFIRMATION, and the confirmation is the
        // only thing standing between a `Bundle` plan and a `kubectl
        // delete`. So the guarantee that `--yes` cannot turn a workload
        // name destructive is exactly this: a workload resolution never
        // yields `Bundle`, at ANY bundle size. `plan_remove_target` takes
        // no `yes` argument for the same reason — the flag cannot reach
        // this decision at all.
        //
        // This is the regression guard for a real escalation: an earlier
        // cut of 2.27b proceeded at N = 1, which turned `apprafter app
        // remove <workload> --yes` from a safe failure into a registration
        // delete.
        for size in [0, 1, 2, 3, 17] {
            let plan = plan_remove_target(
                "landing-cms",
                &Resolution::Workload(workload("landing-cms", "cms", Some("cms-prod"))),
                size,
            );
            assert!(
                !matches!(plan, RemoveTarget::Bundle(_)),
                "bundle_size {size} reached the delete path: {plan:?}"
            );
        }
        // The one arm that DOES act only ever names the string the caller
        // typed — never a registration reached by some other name.
        assert_eq!(
            plan_remove_target("blog", &Resolution::Registration("blog".into(), vec![]), 0),
            RemoveTarget::Bundle("blog".into()),
        );
        assert_eq!(
            plan_remove_target(
                "cms",
                &Resolution::Registration("cms-prod".into(), vec![]),
                0
            ),
            RemoveTarget::Unknown,
        );
    }

    #[test]
    fn a_workload_no_registration_claims_gets_the_plain_not_found() {
        // A CR applied by hand, or one left behind by a removed
        // registration. There is no registration to tear down and no
        // self-heal to undo a delete, so neither the refusal nor the
        // bundle plan is true of it — `remove` operates on registrations,
        // and for this string there is none.
        assert_eq!(
            plan_remove_target(
                "legacy",
                &Resolution::Workload(workload("legacy", "legacy-ns", None)),
                1
            ),
            RemoveTarget::Unknown,
        );
        assert_eq!(
            plan_remove_target("nope", &Resolution::NotFound, 0),
            RemoveTarget::Unknown,
        );
    }

    #[test]
    fn the_refusal_names_the_git_steps_and_the_whole_bundle_alternative() {
        let msg = refuse_workload_lines("api", "shop-reg", 3).join("\n");
        // WHAT was typed and what it turned out to be.
        assert!(msg.contains("'api' is 1 of 3 workloads"), "{msg}");
        assert!(msg.contains("'shop-reg'"), "{msg}");
        // WHY it is refused. This will be reported as a missing feature;
        // the message has to say it is GitOps, not an unimplemented verb.
        assert!(msg.contains("self-heal"), "{msg}");
        // The route that actually works.
        assert!(msg.contains("Application.cue"), "{msg}");
        assert!(msg.contains("commit and push"), "{msg}");
        // And the other thing they might have meant, as a command.
        assert!(msg.contains("ALL 3 workloads"), "{msg}");
        assert!(msg.contains("apprafter app remove shop-reg"), "{msg}");
    }

    #[test]
    fn the_whole_bundle_prompt_names_every_workload_not_a_count() {
        // A count is not consent — the same rule
        // `batch_remove_prompt_lines` already follows for environments.
        // Pre-2.27b this rendered ONE line, in the singular, before
        // tearing down three production workloads.
        let app = bundle(&["api", "web", "worker"], vec![]);
        let lines = remove_prompt_lines(
            "shop-reg",
            &app,
            false,
            Some(&ClaimInventory::Known(Vec::new())),
        );
        let joined = lines.join("\n");
        for w in ["api", "web", "worker"] {
            assert!(
                lines.iter().any(|l| l.contains(&format!("• {w}"))),
                "{joined}"
            );
        }
        assert!(joined.contains("'shop-reg'"), "{joined}");
        assert!(joined.contains("3 workloads"), "{joined}");
        // The source is still quoted back — it is how a reader tells two
        // similarly named applications apart before saying yes.
        assert!(joined.contains("https://github.com/acme/shop"), "{joined}");
    }

    #[test]
    fn the_whole_bundle_prompt_names_a_claim_whose_data_dies() {
        // The claim that matters is the one the OPERATOR generated from
        // `needs.pg` — the database. It is an owner-ref'd child of the
        // workload CR, never synced by Argo CD, so it is absent from
        // `status.resources[]` and only the scoped cluster read finds it.
        // A blast-radius line that omits the databases is not doing the
        // job the confirmation exists for.
        let app = bundle(&["api", "web"], vec![]);
        let inventory = ClaimInventory::Known(vec![
            "    • ResourceClaim api-pg (namespace: shop)".to_string()
        ]);
        let joined = remove_prompt_lines("shop-reg", &app, false, Some(&inventory)).join("\n");
        assert!(joined.contains("ResourceClaim api-pg"), "{joined}");
        assert!(joined.contains("does not come back"), "{joined}");
        // A claim is not a workload — it must not inflate the count the
        // reader is consenting to.
        assert!(joined.contains("2 workloads"), "{joined}");
        // INVARIANT: `--keep-data` strips the cascade finalizer, so
        // NOTHING is pruned. Threatening data that is not at risk is the
        // same defect as the success line's keep-data arm.
        let kept = remove_prompt_lines("shop-reg", &app, true, Some(&inventory)).join("\n");
        assert!(!kept.contains("api-pg"), "{kept}");
        assert!(kept.contains("preserved"), "{kept}");
    }

    #[test]
    fn the_enumeration_covers_operator_generated_claims_and_only_this_bundle_s() {
        // The namespace-wide read is filtered by ownerReference to the
        // bundle's OWN workloads. A namespace may hold a second
        // registration — the shared-volumes guide puts `writer` and
        // `reader` in one — and naming another application's database in
        // this one's blast radius is its own kind of wrong.
        let payload = json!({ "items": [
            { "metadata": { "name": "api-pg", "ownerReferences": [
                { "kind": "Application", "name": "api" }]}},
            { "metadata": { "name": "web-redis", "ownerReferences": [
                { "kind": "Application", "name": "web" }]}},
            { "metadata": { "name": "other-pg", "ownerReferences": [
                { "kind": "Application", "name": "somebody-else" }]}},
            // No owner at all — cannot be attributed, so not claimed.
            { "metadata": { "name": "orphan" }},
        ]});
        let owners = vec!["api".to_string(), "web".to_string()];
        let lines = owned_claim_lines(&payload, &owners, "shop");
        assert_eq!(
            lines,
            vec![
                "    • ResourceClaim api-pg (namespace: shop)",
                "    • ResourceClaim web-redis (namespace: shop)",
            ],
        );
    }

    #[test]
    fn a_failed_claim_read_never_renders_as_no_data_at_risk() {
        // The `—`-never-blank rule, on a destructive path. "We looked and
        // there is none" and "we could not look" must never render the
        // same way; collapsing them would let a failed read read as
        // consent to destroy a database.
        let app = bundle(&["api", "web"], vec![]);
        let cmd = "kubectl get resourceclaim.apprafter.io -n shop";
        let unavailable = remove_prompt_lines(
            "shop-reg",
            &app,
            false,
            Some(&ClaimInventory::Unavailable(cmd.to_string())),
        )
        .join("\n");
        assert!(unavailable.contains("could NOT be read"), "{unavailable}");
        assert!(unavailable.contains(cmd), "{unavailable}");
        assert!(!unavailable.contains("none —"), "{unavailable}");

        // An inventory the caller never consulted fails the SAME way, so
        // a forgotten read cannot silently become a clean bill of health.
        let unconsulted = remove_prompt_lines("shop-reg", &app, false, None).join("\n");
        assert!(unconsulted.contains("could NOT be read"), "{unconsulted}");
        assert!(unconsulted.contains(cmd), "{unconsulted}");

        // And a read that genuinely found nothing says so positively —
        // it is a different sentence, backed by an actual look.
        let empty = remove_prompt_lines(
            "shop-reg",
            &app,
            false,
            Some(&ClaimInventory::Known(vec![])),
        )
        .join("\n");
        assert!(empty.contains("none —"), "{empty}");
        assert!(!empty.contains("could NOT be read"), "{empty}");
    }

    #[test]
    fn removing_a_registration_by_name_is_unchanged_at_one_workload() {
        // The regression guard for today's entire fleet: every existing
        // registration deploys exactly one workload. Prompt and BOTH
        // success arms must render what they rendered before 2.27b, and
        // the `--env` axis must be untouched.
        let app = json!({
            "metadata": { "name": "web-prod" },
            "spec": {
                "project": "apps",
                "source": { "repoURL": "https://github.com/acme/web" },
                "destination": { "namespace": "web" },
            },
            "status": { "resources": [
                { "group": "apprafter.io", "kind": "Application", "version": "v1alpha1",
                  "name": "web", "namespace": "web" }
            ]},
        });
        // Byte-identical by CALLING the old function, not by copying its
        // wording — the technique ADR 0062 mandates for `app status`.
        // `None` for the inventory is not an omission: at N <= 1 no data
        // section is rendered, and the caller correspondingly spends no
        // cluster read.
        assert_eq!(
            remove_prompt_lines("web-prod", &app, false, None),
            vec![single_remove_prompt_line("web-prod", &app)]
        );
        assert_eq!(
            remove_prompt_lines("web-prod", &app, true, None),
            vec![single_remove_prompt_line("web-prod", &app)]
        );
        assert_eq!(
            single_remove_prompt_line("web-prod", &app),
            "Delete Application 'web-prod' (project: apps, repo: https://github.com/acme/web)?"
        );
        // A registration that has never synced tracks no workload at all.
        // It must keep the one-line prompt, not grow a list it cannot
        // fill or a count of zero that reads as "broken".
        let unsynced = json!({ "metadata": { "name": "web-prod" }, "spec": {} });
        assert_eq!(
            remove_prompt_lines("web-prod", &unsynced, false, None),
            vec![single_remove_prompt_line("web-prod", &unsynced)]
        );
        // Both success arms, verbatim.
        assert_eq!(
            remove_success_line("web-prod", false, 1),
            "✓ Application 'web-prod' deleted. Argo CD cascade-prunes the synced AppRafter \
             resources; the operator then removes the workload."
        );
        assert_eq!(
            remove_success_line("web-prod", true, 1),
            "✓ Application 'web-prod' deleted (Argo CD object only). The workload and its \
             AppRafter Application CR are preserved — re-register to re-adopt."
        );
        // The `--env` batch axis is orthogonal to bundles (ADR 0062) and
        // resolves exactly as before.
        assert_eq!(
            plan_remove("web", &[argo("web-prod")]),
            RemovePlan::Single("web-prod".into())
        );
        assert_eq!(
            plan_remove("web", &[argo("web-dev"), argo("web-prod")]),
            RemovePlan::Batch(vec!["web-dev".into(), "web-prod".into()])
        );
        assert_eq!(
            batch_remove_prompt_lines("web", &["web-dev".into(), "web-prod".into()])[0],
            "Delete ALL 2 environment deployments of 'web'?"
        );
    }

    #[test]
    fn an_unlabelled_pre_2_9_app_falls_back_to_the_bare_logical_name() {
        // INVARIANT: a pre-2.9 app carries no `apprafter.io/application`
        // label, so "the selector matched nothing" must not be read as
        // "there is nothing to delete".
        assert_eq!(plan_remove("web", &[]), RemovePlan::Single("web".into()));
    }

    #[test]
    fn a_single_labelled_match_targets_its_own_argo_name_not_the_logical_one() {
        // INVARIANT: the object's own metadata.name may be `<name>-<env>`.
        // Deleting `<name>` would target an Application that does not exist.
        assert_eq!(
            plan_remove("web", &[argo("web-prod")]),
            RemovePlan::Single("web-prod".into())
        );
    }

    #[test]
    fn several_labelled_matches_become_a_batch_of_every_argo_name() {
        assert_eq!(
            plan_remove("web", &[argo("web-dev"), argo("web-prod")]),
            RemovePlan::Batch(vec!["web-dev".into(), "web-prod".into()])
        );
    }

    #[test]
    fn an_entry_without_a_name_is_skipped_rather_than_deleted_blind() {
        let plan = plan_remove("web", &[argo("web-dev"), json!({ "metadata": {} })]);
        assert_eq!(plan, RemovePlan::Batch(vec!["web-dev".into()]));
    }

    #[test]
    fn the_batch_prompt_names_every_deployment_rather_than_counting_them() {
        // A count is not consent. The reader is about to destroy several
        // environments and must see which.
        let lines = batch_remove_prompt_lines("web", &["web-dev".into(), "web-prod".into()]);
        assert!(
            lines[0].contains("Delete ALL 2 environment deployments of 'web'?"),
            "{lines:?}"
        );
        assert_eq!(lines[1], "  • web-dev");
        assert_eq!(lines[2], "  • web-prod");
    }

    #[test]
    fn the_single_prompt_quotes_the_project_and_repo_back() {
        let app = json!({
            "spec": { "project": "apps", "source": { "repoURL": "https://github.com/acme/web" } }
        });
        let line = single_remove_prompt_line("web-prod", &app);
        assert!(line.contains("'web-prod'"), "{line}");
        assert!(line.contains("project: apps"), "{line}");
        assert!(line.contains("repo: https://github.com/acme/web"), "{line}");
    }

    #[test]
    fn a_prompt_for_an_app_missing_those_fields_shows_a_question_mark_not_an_empty_pair() {
        let line = single_remove_prompt_line("web", &json!({}));
        assert!(line.contains("project: ?"), "{line}");
        assert!(line.contains("repo: ?"), "{line}");
    }

    #[test]
    fn keep_data_and_a_plain_remove_say_opposite_things_about_the_workload() {
        // INVARIANT: the finalizer was stripped on the keep-data path, so
        // the CR and its pods survive. Reporting the cascade wording there
        // tells an operator their data is gone while it is still running.
        //
        // It has to keep holding at N > 1, where BOTH arms are reworded:
        // a line that says "the workload" after removing three is just as
        // wrong about what happened.
        for n in [1, 3] {
            let kept = remove_success_line("web", true, n);
            assert!(kept.contains("preserved"), "{kept}");
            assert!(!kept.contains("cascade-prunes"), "{kept}");

            let cascaded = remove_success_line("web", false, n);
            assert!(cascaded.contains("cascade-prunes"), "{cascaded}");
            assert!(!cascaded.contains("preserved"), "{cascaded}");
        }
        // And at N > 1 neither arm speaks in the singular.
        let many = remove_success_line("shop-reg", false, 3);
        assert!(many.contains("all 3 workloads"), "{many}");
        assert!(!many.contains("the workload"), "{many}");
        let many_kept = remove_success_line("shop-reg", true, 3);
        assert!(many_kept.contains("All 3 workloads"), "{many_kept}");
        assert!(!many_kept.contains("The workload"), "{many_kept}");
    }
}

#[cfg(test)]
mod pin_plan_tests {
    use super::*;
    use serde_json::json;

    fn cr_with(ns: Option<&str>) -> Value {
        let mut meta = json!({ "name": "web" });
        if let Some(ns) = ns {
            meta["namespace"] = json!(ns);
        }
        json!({
            "metadata": meta,
            "status": { "image": {
                "tag": "ghcr.io/acme/web:latest",
                "resolved": "ghcr.io/acme/web@sha256:current"
            }}
        })
    }

    fn argo_app() -> Value {
        json!({ "spec": { "destination": { "namespace": "apps" } } })
    }

    /// The single-workload bundle every pre-2.27b assertion in this
    /// module was written against — application `web`, one workload.
    fn solo() -> BundleScope {
        BundleScope {
            application: "web".into(),
            size: 1,
            env: None,
        }
    }

    #[test]
    fn a_pin_plan_resolves_the_reference_from_the_crs_own_repository() {
        let plan = plan_pin(&argo_app(), &cr_with(Some("prod")), "sha256:older", solo()).unwrap();
        assert_eq!(plan.cr_name, "web");
        assert_eq!(plan.cr_ns, "prod");
        assert_eq!(plan.reference, "ghcr.io/acme/web@sha256:older");
        assert_eq!(plan.current, "ghcr.io/acme/web@sha256:current");
    }

    #[test]
    fn a_namespaceless_cr_borrows_the_argo_applications_destination() {
        // INVARIANT: the pin must land where the workload is. A
        // namespace-less server-side apply would silently target `default`.
        let plan = plan_pin(&argo_app(), &cr_with(None), "sha256:older", solo()).unwrap();
        assert_eq!(plan.cr_ns, "apps");
    }

    #[test]
    fn a_pin_with_no_namespace_anywhere_is_refused_rather_than_defaulted() {
        let err = plan_pin(&json!({}), &cr_with(None), "sha256:older", solo()).unwrap_err();
        assert!(
            format!("{err}").contains("cannot determine the application's namespace"),
            "{err}"
        );
    }

    #[test]
    fn a_git_managed_pin_is_refused_because_the_next_sync_would_revert_it() {
        // INVARIANT: writing it would report a rollback the operator never
        // got — Argo owns the annotation and re-syncs it away.
        let mut cr = cr_with(Some("apps"));
        cr["metadata"]["managedFields"] = json!([{
            "manager": "argocd-application-controller",
            "fieldsV1": { "f:metadata": { "f:annotations": { "f:apprafter.io/image-pin": {} } } }
        }]);
        let err = plan_pin(&argo_app(), &cr, "sha256:older", solo()).unwrap_err();
        assert!(format!("{err}").contains("Git owns"), "{err}");
    }

    #[test]
    fn pinning_to_what_is_already_running_is_refused_as_a_no_op() {
        let err = plan_pin(
            &argo_app(),
            &cr_with(Some("apps")),
            "sha256:current",
            solo(),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("already running"), "{err}");
    }

    #[test]
    fn the_pin_prompt_names_the_mode_change_and_the_verb_that_undoes_it() {
        // A pin is the one rollback that keeps acting after it returns, so
        // the way back has to be on screen before the reader agrees.
        let plan = plan_pin(&argo_app(), &cr_with(Some("apps")), "sha256:older", solo()).unwrap();
        let lines = pin_prompt_lines(&plan);
        assert!(
            lines[0].contains("from ghcr.io/acme/web@sha256:current"),
            "{lines:?}"
        );
        assert!(
            lines[0].contains("to ghcr.io/acme/web@sha256:older"),
            "{lines:?}"
        );
        assert!(lines[1].contains("stops following its tag"), "{lines:?}");
        assert!(lines[1].contains("apprafter app unpin web"), "{lines:?}");
        assert!(lines[2].contains("--to <revision>"), "{lines:?}");
    }

    #[test]
    fn the_pin_success_line_carries_the_un_pin_verb() {
        let plan = plan_pin(&argo_app(), &cr_with(Some("apps")), "sha256:older", solo()).unwrap();
        let line = pin_success_line(&plan);
        assert!(
            line.contains("pinned to ghcr.io/acme/web@sha256:older"),
            "{line}"
        );
        assert!(line.contains("apprafter app unpin web"), "{line}");
    }

    #[test]
    fn the_unpin_plan_uses_the_same_namespace_fallback_as_the_pin_plan() {
        // INVARIANT: un-pin re-applies the SAME body under the SAME field
        // manager. A namespace that differed by one step would prune
        // nothing and leave the application pinned while reporting success.
        let cr = cr_with(None);
        let pin = plan_pin(&argo_app(), &cr, "sha256:older", solo()).unwrap();
        let unpin = plan_unpin(&argo_app(), &cr, solo()).unwrap();
        assert_eq!(unpin.cr_ns, pin.cr_ns);
        assert_eq!(unpin.cr_name, pin.cr_name);
    }

    #[test]
    fn an_unpinned_cr_reports_no_held_reference() {
        assert_eq!(
            plan_unpin(&argo_app(), &cr_with(Some("apps")), solo())
                .unwrap()
                .pinned,
            None
        );
    }

    #[test]
    fn a_pinned_cr_reports_the_reference_the_annotation_holds() {
        let mut cr = cr_with(Some("apps"));
        cr["metadata"]["annotations"] =
            json!({ "apprafter.io/image-pin": "ghcr.io/acme/web@sha256:held" });
        let plan = plan_unpin(&argo_app(), &cr, solo()).unwrap();
        assert_eq!(plan.pinned.as_deref(), Some("ghcr.io/acme/web@sha256:held"));
        assert_eq!(plan.tag, "ghcr.io/acme/web:latest");
    }

    #[test]
    fn a_cr_with_no_resolved_tag_falls_back_to_a_phrase_that_still_reads() {
        let cr = json!({ "metadata": { "name": "web", "namespace": "apps" } });
        assert_eq!(plan_unpin(&argo_app(), &cr, solo()).unwrap().tag, "its tag");
    }

    #[test]
    fn the_unpin_prompt_warns_that_the_workload_may_roll_forward() {
        // INVARIANT: "un-pin" does not imply "may deploy something new",
        // and that is exactly what the reader is agreeing to.
        let mut cr = cr_with(Some("apps"));
        cr["metadata"]["annotations"] =
            json!({ "apprafter.io/image-pin": "ghcr.io/acme/web@sha256:held" });
        let lines = unpin_prompt_lines(&plan_unpin(&argo_app(), &cr, solo()).unwrap());
        assert!(
            lines[0].contains("currently held at ghcr.io/acme/web@sha256:held"),
            "{lines:?}"
        );
        assert!(lines[1].contains("roll forward"), "{lines:?}");
        assert!(lines[1].contains("ghcr.io/acme/web:latest"), "{lines:?}");
    }

    #[test]
    fn a_revision_rollback_to_the_current_revision_is_an_error_not_a_silent_success() {
        // INVARIANT: Argo CD would accept the patch and report a sync, so
        // "rolled back" would be printed over a workload that never moved.
        let app = json!({ "spec": { "source": { "targetRevision": "main" } } });
        assert_eq!(current_target_revision(&app), "main");
        let msg = noop_revision_error("main", &current_target_revision(&app)).expect("refused");
        assert!(msg.contains("no-op"), "{msg}");
        assert_eq!(noop_revision_error("v1.2.3", "main"), None);
    }

    #[test]
    fn an_application_without_a_target_revision_reads_as_a_question_mark() {
        assert_eq!(current_target_revision(&json!({})), "?");
    }

    /// The three-workload bundle `shop`, whose second workload is the
    /// `web` CR every fixture in this module already builds.
    fn trio() -> BundleScope {
        BundleScope {
            application: "shop".into(),
            size: 3,
            env: None,
        }
    }

    #[test]
    fn the_two_rollback_branches_do_not_read_the_same_on_a_bundle() {
        // THE defect this subphase is about. `--to <digest>` pins ONE
        // workload; `--to <git-rev>` moves the registration's
        // targetRevision and with it EVERY workload. A prompt that reads
        // identically for "pin this one image" and "roll all three
        // workloads back to last Tuesday" is consent for the wrong
        // operation.
        let pin = pin_prompt_lines(
            &plan_pin(&argo_app(), &cr_with(Some("apps")), "sha256:older", trio()).unwrap(),
        )
        .join("\n");
        let rev = revision_prompt_lines("shop", "main", "v1.2.3", 3).join("\n");

        // The pin says one of three, and which two it leaves alone —
        // and stops calling its target "the application", which over a
        // three-workload bundle claims triple the real blast radius.
        assert!(pin.contains("1 of the 3 workloads"), "{pin}");
        assert!(pin.contains("other 2 are not touched"), "{pin}");
        assert!(!pin.contains("PINS the application"), "{pin}");
        assert!(pin.contains("This PINS it: 'web'"), "{pin}");
        // The revision says all three, in the same breath as the verb.
        assert!(rev.contains("ALL 3 workloads"), "{rev}");
        assert!(
            !rev.contains("not touched"),
            "the revision branch touches every workload: {rev}"
        );

        // Each points at the other, because the reader reaching for one
        // may have wanted the other — and the difference is cardinality,
        // which is invisible in the flag itself.
        assert!(pin.contains("--to <revision>"), "{pin}");
        assert!(rev.contains("--to <sha256:digest>"), "{rev}");

        // The un-pin route quoted on a bundle must be addressable: the
        // positional is the APPLICATION and the workload rides
        // `--workload` (ADR 0062 §Addressing). `apprafter app unpin web`
        // would be the collapse the whole rule prevents.
        assert!(
            pin.contains("apprafter app unpin shop --workload web"),
            "{pin}"
        );
    }

    #[test]
    fn a_single_workload_bundle_renders_both_branches_exactly_as_before() {
        // N = 1 is today's entire fleet. Neither prompt grows a line —
        // the cardinality clause is added ONLY where cardinality exists.
        //
        // `solo()` is the coincident case, where the application and its
        // one workload are both called `web`, which is what the scaffold
        // produces and so what nearly every real bundle is. There the
        // quoted un-pin is byte-identical to pre-2.27b. The ONE N = 1
        // output that deliberately changed is the divergent case, pinned
        // by `the_un_pin_route_names_the_application_when_the_names_diverge`.
        let pin = pin_prompt_lines(
            &plan_pin(&argo_app(), &cr_with(Some("apps")), "sha256:older", solo()).unwrap(),
        );
        assert_eq!(pin.len(), 3, "{pin:?}");
        assert!(pin[1].contains("apprafter app unpin web"), "{pin:?}");
        assert!(!pin.join("\n").contains("workloads"), "{pin:?}");

        assert_eq!(
            revision_prompt_lines("web", "main", "v1.2.3", 1),
            vec!["Roll back Application 'web' from revision 'main' to 'v1.2.3'?".to_string()],
        );
        assert_eq!(
            revision_success_line("web", "v1.2.3", 1),
            "✓ Application 'web' rolled back to revision 'v1.2.3'. Argo CD will sync the \
             workload within a reconcile cycle."
        );
    }

    #[test]
    fn the_un_pin_route_names_the_application_when_the_names_diverge() {
        // A pin is the one rollback that keeps acting after the command
        // returns, so the way back is not decoration — it is the whole
        // reason the prompt names a verb at all. Handing over a command
        // that errors leaves the reader pinned with no printed route out.
        //
        // The shape is the one this repository already pins as supported
        // (`remove_plan_tests`, and `app.rs`'s own Argo-CD-app-`cms`
        // assertion): registration `cms-prod`, grouping label `cms`,
        // rendering ONE workload called `landing-cms`. N = 1 — so before
        // this fix the pre-2.27b spelling applied and quoted
        // `apprafter app unpin landing-cms`, which re-enters
        // `resolve_app_for_command`, matches no Argo object and no
        // `apprafter.io/application` label, and fails `not found`.
        //
        // ADR 0062 §Addressing has one rule and no size exception: the
        // positional is the application. At N = 1 that is the whole
        // command; `--workload` joins only when there is something to
        // disambiguate.
        let scope = BundleScope {
            application: "cms".into(),
            size: 1,
            env: None,
        };
        let mut cr = cr_with(Some("cms"));
        cr["metadata"]["name"] = json!("landing-cms");
        let plan = plan_pin(&argo_app(), &cr, "sha256:older", scope).unwrap();

        for line in [pin_prompt_lines(&plan)[1].clone(), pin_success_line(&plan)] {
            assert!(
                line.contains("apprafter app unpin cms"),
                "the route out must name the application the caller typed: {line}"
            );
            assert!(
                !line.contains("apprafter app unpin landing-cms"),
                "quoting the workload name is the command that fails `not found`: {line}"
            );
            // No `--workload` at N = 1: there is nothing to
            // disambiguate, and the flag would imply there is.
            assert!(!line.contains("--workload"), "{line}");
        }

        // The block still names the WORKLOAD as the thing being pinned —
        // the application is the address, not the target.
        assert!(pin_prompt_lines(&plan)[0].contains("'landing-cms'"));
    }

    #[test]
    fn every_quoted_command_carries_the_env_the_caller_typed() {
        // A printed next-command is re-entered through
        // `resolve_app_for_command`. On a registration with two or more
        // environments, dropping `--env` makes that resolve to several
        // deployments and error on `per_env_guidance_message` — so the
        // handed-over command fails for a reason the reader did not
        // cause. Not a safety bug (it fails loudly rather than acting
        // wrongly), but a route out that does not work is not a route.
        let scope = BundleScope {
            application: "shop".into(),
            size: 3,
            env: Some("prod".into()),
        };
        let mut cr = cr_with(Some("shop"));
        cr["metadata"]["name"] = json!("api");
        let plan = plan_pin(&argo_app(), &cr, "sha256:older", scope).unwrap();
        for line in [pin_prompt_lines(&plan)[2].clone(), pin_success_line(&plan)] {
            assert!(
                line.contains("apprafter app unpin shop --workload api --env prod"),
                "{line}"
            );
        }

        // The write refusal and the read prompt quote commands too, and
        // they re-enter the same resolver.
        let refusal = ambiguous_write_lines(
            "rollback",
            "shop",
            &["api".to_string()],
            &format!("{} --to sha256:beef", env_echo(Some("prod"))),
        )
        .join("\n");
        assert!(
            refusal
                .contains("apprafter app rollback shop --workload api --env prod --to sha256:beef"),
            "{refusal}"
        );
        assert!(
            ambiguous_read_message("shop", &["api".to_string()], Some("prod"))
                .contains("--workload <name> --env prod"),
        );

        // …and nothing is appended when the caller passed none, so the
        // single-environment case — nearly every one — is untouched.
        assert_eq!(env_echo(None), "");
    }

    #[test]
    fn the_revision_success_line_stops_saying_the_workload_on_a_bundle() {
        // The same defect `delete_success_line` already fixed: "the
        // workload", after moving three of them, is simply false.
        let line = revision_success_line("shop", "v1.2.3", 3);
        assert!(line.contains("all 3 workloads"), "{line}");
    }

    #[test]
    fn the_pin_success_line_quotes_an_addressable_un_pin_on_a_bundle() {
        // A pin keeps acting after the command returns, so the way back
        // has to be a command that works — at N > 1 that means the
        // application plus `--workload`, not the workload alone.
        let plan = plan_pin(&argo_app(), &cr_with(Some("apps")), "sha256:older", trio()).unwrap();
        let line = pin_success_line(&plan);
        assert!(
            line.contains("apprafter app unpin shop --workload web"),
            "{line}"
        );
    }

    #[test]
    fn the_unpin_prompt_says_which_workload_of_how_many_it_moves() {
        let mut cr = cr_with(Some("apps"));
        cr["metadata"]["annotations"] =
            json!({ "apprafter.io/image-pin": "ghcr.io/acme/web@sha256:held" });
        let lines = unpin_prompt_lines(&plan_unpin(&argo_app(), &cr, trio()).unwrap()).join("\n");
        assert!(lines.contains("1 of the 3 workloads"), "{lines}");
        // …and adds nothing at N = 1.
        let solo_lines = unpin_prompt_lines(&plan_unpin(&argo_app(), &cr, solo()).unwrap());
        assert_eq!(solo_lines.len(), 2, "{solo_lines:?}");
    }
}

#[cfg(test)]
mod logs_target_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_pod_label_is_the_inner_apprafter_name_not_the_argo_parent() {
        // INVARIANT: `Application.cue`'s metadata.name need not equal the
        // name typed at `app add`; the operator labels pods with the former.
        let app = json!({
            "spec": { "destination": { "namespace": "apps" } },
            "status": { "resources": [
                { "group": "apprafter.io", "kind": "Application",
                  "name": "storefront", "namespace": "apps" }
            ]}
        });
        let (ns, inner) = resolve_logs_workload(&app, "web-prod", None).unwrap();
        assert_eq!(ns, "apps");
        assert_eq!(inner, vec!["storefront".to_string()]);
    }

    #[test]
    fn a_raw_yaml_app_falls_back_to_the_resolved_argo_name_not_the_logical_one() {
        // INVARIANT: the fallback is `<name>-<env>` — the RESOLVED name the
        // per-env lookup already produced. Falling back to the logical name
        // would select pods of no deployment at all.
        let app = json!({ "spec": { "destination": { "namespace": "apps" } } });
        let (_, inner) = resolve_logs_workload(&app, "web-prod", None).unwrap();
        assert_eq!(inner, vec!["web-prod".to_string()]);
    }

    #[test]
    fn an_application_without_a_destination_namespace_is_refused_with_a_reason() {
        let err = resolve_logs_workload(&json!({}), "web", None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("spec.destination.namespace"), "{msg}");
        assert!(msg.contains("'web'"), "{msg}");
    }

    #[test]
    fn one_workload_still_selects_by_equality() {
        // N = 1 must be byte-identical to pre-2.27b, argv included: the
        // set form would be a gratuitous change to the command every
        // existing user runs, and `--max-log-requests` must not move
        // either.
        assert_eq!(
            build_kubectl_logs_target(&["storefront".to_string()], None),
            KubectlLogsTarget::Selector {
                selector: "app.kubernetes.io/name=storefront".to_string(),
                workloads: 1,
            }
        );
        let args = build_kubectl_logs_args(
            &build_kubectl_logs_target(&["storefront".to_string()], None),
            "apps",
            true,
            -1,
            None,
        );
        assert_eq!(
            args,
            vec![
                "logs",
                "-l",
                "app.kubernetes.io/name=storefront",
                "-n",
                "apps",
                "-f",
                "--prefix=true",
                "--max-log-requests=10",
            ]
        );
    }

    #[test]
    fn a_bundle_multiplexes_every_workload_into_one_stream() {
        // The `logs` decision (ADR 0062): a bundle is deployed together,
        // so its workloads' lines interleave into one story. A set-based
        // selector is what turns `kubectl logs -l` — already a
        // multiplexer over pods — into one over workloads too.
        let target = build_kubectl_logs_target(
            &["api".to_string(), "web".to_string(), "worker".to_string()],
            None,
        );
        assert_eq!(
            target,
            KubectlLogsTarget::Selector {
                selector: "app.kubernetes.io/name in (api,web,worker)".to_string(),
                workloads: 3,
            }
        );

        let args = build_kubectl_logs_args(&target, "shop", true, -1, None);
        // `--prefix=true` is what makes a multiplexed stream readable:
        // every line arrives as `[pod/<pod>/<container>]`, and the
        // operator names each Deployment after its workload, so the
        // prefix carries the workload name.
        assert!(args.contains(&"--prefix=true".to_string()), "{args:?}");
        // The per-workload stream budget stays what it was; three
        // workloads get three times the ceiling rather than sharing one,
        // or `-f` on a replicated bundle fails with a flag this CLI does
        // not expose.
        assert!(
            args.contains(&"--max-log-requests=30".to_string()),
            "{args:?}"
        );
    }

    #[test]
    fn the_multiplex_banner_is_silent_whenever_the_stream_is_not_multiplexed() {
        // The banner is an output CLAIM, and this file elsewhere keeps
        // "we looked and found none" distinct from "we could not look"
        // to the letter. `--pod` makes `build_kubectl_logs_target`
        // return `Pod(..)` two lines later, so announcing three
        // workloads and then streaming one pod is simply a false
        // statement about what the reader is watching.
        let trio = ["api".to_string(), "web".to_string(), "worker".to_string()];
        assert_eq!(
            multiplex_banner("shop", &trio, Some("api-7f9c-xyz")),
            None,
            "a --pod stream is one pod, whatever the bundle holds"
        );
        assert_eq!(
            multiplex_banner("blog", &["blog".to_string()], None),
            None,
            "one workload is not a multiplex and needs no announcement"
        );

        let banner = multiplex_banner("shop", &trio, None).expect("three workloads, no --pod");
        assert!(banner.contains("all 3 workloads of 'shop'"), "{banner}");
        assert!(banner.contains("api, web, worker"), "{banner}");
        assert!(banner.contains("--workload"), "{banner}");
    }

    #[test]
    fn an_explicit_pod_still_overrides_the_selector_at_any_bundle_size() {
        // `--pod` is strictly narrower than `--workload`, so it wins —
        // and a direct pod name is not a multiplexed stream, so it keeps
        // its prefix-free, ceiling-free argv.
        let target = build_kubectl_logs_target(
            &["api".to_string(), "web".to_string()],
            Some("api-7f9c-xyz"),
        );
        assert_eq!(target, KubectlLogsTarget::Pod("api-7f9c-xyz".to_string()));
        let args = build_kubectl_logs_args(&target, "shop", false, -1, None);
        assert!(!args.iter().any(|a| a.starts_with("--prefix")), "{args:?}");
        assert!(
            !args.iter().any(|a| a.starts_with("--max-log-requests")),
            "{args:?}"
        );
    }
}

/// The addressing decision the four remaining `app` verbs share (ADR
/// 0062 §Write surfaces) — `logs`, `open`, `rollback`, `unpin`.
///
/// Every test here runs the SAME fixture through all three
/// [`WorkloadDemand`]s, because the thing being pinned is not any one
/// verb's behaviour but the *difference* between them: at N > 1 with no
/// `--workload`, a write refuses, a single-target read asks, and a
/// multiplexing read takes every workload. A test per verb would let
/// that difference drift one verb at a time.
#[cfg(test)]
mod workload_for_tests {
    use super::*;

    /// Every demand, so a rule that must hold for all of them is written
    /// once and cannot be updated for two verbs out of three.
    const EVERY_DEMAND: [WorkloadDemand; 3] = [
        WorkloadDemand::Write,
        WorkloadDemand::ReadOne,
        WorkloadDemand::ReadEvery,
    ];

    fn placed(name: &str, namespace: &str) -> CrRef {
        CrRef {
            namespace: Some(namespace.to_string()),
            name: name.to_string(),
        }
    }

    /// A workload Argo CD tracks but that neither its own
    /// `status.resources[]` entry nor the registration's
    /// `spec.destination.namespace` places — the state
    /// `apprafter_app_refs_reports_an_unknown_namespace_as_none`
    /// documents as real.
    fn unplaced(name: &str) -> CrRef {
        CrRef {
            namespace: None,
            name: name.to_string(),
        }
    }

    /// The three-workload bundle: one registration, one namespace.
    fn bundle() -> Vec<CrRef> {
        vec![
            placed("api", "shop"),
            placed("web", "shop"),
            placed("worker", "shop"),
        ]
    }

    #[test]
    fn every_verb_is_unchanged_on_a_single_workload_bundle() {
        // N = 1 is today's entire fleet, so this is the regression guard
        // for the whole change: with one workload and no selector there
        // is nothing to disambiguate, and every verb must resolve it
        // outright — no prompt, no refusal, no multiplexing. Asserted
        // across all three demands because "unchanged at N = 1" is a
        // claim about the verbs collectively.
        for demand in EVERY_DEMAND {
            assert_eq!(
                workload_for(&[placed("blog", "blog")], None, demand),
                WorkloadChoice::One(PlacedWorkload {
                    name: "blog".into(),
                    namespace: "blog".into(),
                }),
                "{demand:?} changed the single-workload path",
            );
        }
    }

    #[test]
    fn a_write_verb_refuses_ambiguity_rather_than_picking_one() {
        // The defect this subphase exists to end. Picking the first is
        // how a pin lands on a workload nobody named — a silent mutation
        // of the wrong object, which no later command can distinguish
        // from a pin the operator meant.
        let choice = workload_for(&bundle(), None, WorkloadDemand::Write);
        assert_eq!(
            choice,
            WorkloadChoice::Refuse(vec!["api".into(), "web".into(), "worker".into()]),
            "a write must refuse, and must name every candidate it refused to choose between",
        );

        // The refusal is only useful if the next command is
        // copy-pasteable, so it quotes one per candidate — carrying the
        // flags the caller already typed, or the retry loses them.
        let lines = ambiguous_write_lines(
            "rollback",
            "shop",
            &["api".to_string(), "web".to_string(), "worker".to_string()],
            " --to sha256:beef",
        );
        let msg = lines.join("\n");
        for w in ["api", "web", "worker"] {
            assert!(
                msg.contains(&format!(
                    "apprafter app rollback shop --workload {w} --to sha256:beef"
                )),
                "{msg}",
            );
        }
        // The positional stays the APPLICATION in every quoted command —
        // ADR 0062 §Addressing. `apprafter app rollback api` would be
        // the collapse this whole rule exists to prevent.
        assert!(!msg.contains("app rollback api"), "{msg}");

        // …and `rollback`'s refusal also names the branch that is not
        // ambiguous, or the reader concludes the verb is unavailable on
        // a bundle when in fact one of its two branches is bundle-wide
        // by construction.
        let full = rollback_refusal_lines(
            "shop",
            &["api".to_string(), "web".to_string(), "worker".to_string()],
            " --to sha256:beef",
        )
        .join("\n");
        assert!(full.contains("To roll ALL 3 workloads back"), "{full}");
        assert!(
            full.contains("apprafter app rollback shop --to <revision>"),
            "{full}"
        );
        // `unpin` gets no such tail — it has no bundle-wide branch, and
        // inventing one would be a route that does not exist.
        let unpin = ambiguous_write_lines("unpin", "shop", &["api".to_string()], "").join("\n");
        assert!(!unpin.contains("To roll ALL"), "{unpin}");
    }

    #[test]
    fn a_read_verb_on_a_multi_workload_bundle_does_not_silently_pick_the_first() {
        // Port-forwarding an arbitrary Service is wrong too — it puts a
        // different application on localhost:8080 than the one the
        // reader named — it is just not destructive, so `open` ASKS
        // where a write refuses.
        assert_eq!(
            workload_for(&bundle(), None, WorkloadDemand::ReadOne),
            WorkloadChoice::Ask(vec!["api".into(), "web".into(), "worker".into()]),
        );

        // `logs` multiplexes instead: `kubectl logs -l` already fans out
        // across pods, a bundle is deployed together, and its workloads'
        // lines interleave into one story. EVERY workload, not the first.
        assert_eq!(
            workload_for(&bundle(), None, WorkloadDemand::ReadEvery),
            WorkloadChoice::Every(vec![
                PlacedWorkload {
                    name: "api".into(),
                    namespace: "shop".into()
                },
                PlacedWorkload {
                    name: "web".into(),
                    namespace: "shop".into()
                },
                PlacedWorkload {
                    name: "worker".into(),
                    namespace: "shop".into()
                },
            ]),
        );

        // Stated as its own assertion because it is the actual
        // regression: the pre-2.27 shim returned `refs[0].name` and
        // every one of these verbs acted on it.
        for demand in [WorkloadDemand::ReadOne, WorkloadDemand::ReadEvery] {
            assert!(
                !matches!(
                    workload_for(&bundle(), None, demand),
                    WorkloadChoice::One(_)
                ),
                "{demand:?} silently resolved one workload of three",
            );
        }
    }

    #[test]
    fn an_explicit_selector_wins_for_every_verb() {
        // `--workload` is the disambiguator, in the same sense `--env`
        // is: it answers the question the verb would otherwise have to
        // refuse or ask. It answers it identically for all three, and it
        // never resolves to the first.
        for demand in EVERY_DEMAND {
            assert_eq!(
                workload_for(&bundle(), Some("web"), demand),
                WorkloadChoice::One(PlacedWorkload {
                    name: "web".into(),
                    namespace: "shop".into(),
                }),
                "{demand:?} ignored an explicit --workload",
            );
        }
    }

    #[test]
    fn an_unknown_selector_names_the_workloads_that_exist() {
        // Validated even at N = 1: accepting any name when there is only
        // one workload would let a typo act on a different object than
        // the reader asked for, and at N = 1 the application name and
        // the workload name are usually the same string — so the typo is
        // easy to make and invisible to catch. Same invariant
        // `status_render_plan` already carries.
        for refs in [bundle(), vec![placed("blog", "blog")]] {
            for demand in EVERY_DEMAND {
                let available: Vec<String> = refs.iter().map(|r| r.name.clone()).collect();
                assert_eq!(
                    workload_for(&refs, Some("wbe"), demand),
                    WorkloadChoice::Unknown {
                        asked: "wbe".into(),
                        available: available.clone(),
                    },
                    "{demand:?} accepted a workload the bundle does not deploy",
                );
            }
        }

        // The candidate list is what corrects the typo, so it has to
        // reach the reader.
        let msg = unknown_workload_message("shop", "wbe", &["api".into(), "web".into()]);
        assert!(msg.contains("api, web"), "{msg}");
        assert!(msg.contains("'wbe'"), "{msg}");
    }

    #[test]
    fn a_workload_whose_namespace_is_unknown_is_refused_not_passed_to_kubectl() {
        // `CrRef::namespace` is `Option<String>` precisely so this case
        // cannot be defaulted. `kubectl -n ""` does NOT error: it falls
        // through to the kubeconfig's default namespace, so a pin would
        // land — successfully, silently — on whatever object happens to
        // share the name there. [`PlacedWorkload`] carries a plain
        // `String`, so the only way out of this arm is a refusal.
        for demand in EVERY_DEMAND {
            assert_eq!(
                workload_for(&[unplaced("api")], None, demand),
                WorkloadChoice::Unplaceable("api".into()),
                "{demand:?} accepted a workload it cannot place",
            );
            // …and by an explicit selector, which is the likelier route:
            // a reader who typed `--workload api` gets the same refusal
            // rather than a write into the default namespace.
            assert_eq!(
                workload_for(
                    &[placed("web", "shop"), unplaced("api")],
                    Some("api"),
                    demand
                ),
                WorkloadChoice::Unplaceable("api".into()),
            );
        }

        // The multiplexing read cannot quietly drop the one it could not
        // place either: a stream missing a workload reads exactly like a
        // workload that logged nothing.
        assert_eq!(
            workload_for(
                &[placed("web", "shop"), unplaced("api")],
                None,
                WorkloadDemand::ReadEvery
            ),
            WorkloadChoice::Unplaceable("api".into()),
        );
    }
}

/// `--workload` must never be accepted and then thrown away.
///
/// The mirror image of [`workload_for_tests`], and the same defect with
/// the sign flipped: those tests stop a write acting on a workload
/// nobody named, these stop one acting on MORE than the workload the
/// caller did name. Both are about the gap between what the operator
/// addressed and what the command moves.
#[cfg(test)]
mod rollback_scope_tests {
    use super::*;

    #[test]
    fn an_explicit_git_revision_refuses_a_workload_it_would_discard() {
        // Shape (a): `apprafter app rollback shop --to v1.2.3 --workload api`.
        // `--to <git-rev>` patches the registration's `targetRevision`,
        // so Argo CD re-renders the whole package — all three workloads
        // move. Accepting the flag and ignoring it hands the caller 3x
        // the blast radius they addressed, and under `--yes` the prompt
        // that would have disclosed it never prints.
        let err = vet_rollback_scope(
            "shop",
            Some("api"),
            Some("v1.2.3"),
            &RollbackTarget::GitRevision("v1.2.3".into()),
            3,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`--to v1.2.3` is a Git revision"), "{err}");
        assert!(err.contains("all 3 workloads of 'shop'"), "{err}");
        // Both ways out, because either may be what they meant.
        assert!(err.contains("--to <sha256:digest>"), "{err}");
        assert!(err.contains("drop `--workload api`"), "{err}");
    }

    #[test]
    fn a_bare_rollback_that_falls_through_to_git_refuses_a_named_workload() {
        // Shape (b), and the one a user reaches BY ACCIDENT: no `--to` at
        // all. `apprafter app rollback shop --workload api` where `api`'s
        // CR carries no `status.image.previous.resolved` —
        // `classify_rollback_target` falls through to the previous
        // `status.history` revision, and `rollback_to_revision` moves all
        // three. Nothing the caller typed hints at that, and `--help`
        // documents only the explicit-`--to` case.
        let err = vet_rollback_scope(
            "shop",
            Some("api"),
            None,
            &RollbackTarget::GitRevision("abc123".into()),
            3,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("'api' has no image to roll back to"), "{err}");
        assert!(err.contains("Git revision 'abc123'"), "{err}");
        assert!(err.contains("all 3 workloads of 'shop'"), "{err}");
        assert!(err.contains("--to <sha256:digest>"), "{err}");
        // The two shapes must not render the same: this caller never
        // asked for a Git revision, so blaming their `--to` would be a
        // false statement about what they typed.
        assert!(!err.contains("is a Git revision"), "{err}");
    }

    #[test]
    fn the_refusal_holds_at_every_bundle_size() {
        // No size exception, for the same reason `BundleScope::verb_for`
        // has none: `--workload` means "act on this workload", and a
        // Git-revision rollback does not act on a workload at any N — it
        // acts on the registration. Accepting it at N = 1 because the
        // blast radius happens to coincide teaches a mental model that
        // silently becomes a 3x write the day somebody adds a second
        // workload to the bundle.
        for size in [0, 1, 2, 3, 17] {
            assert!(
                vet_rollback_scope(
                    "shop",
                    Some("api"),
                    None,
                    &RollbackTarget::GitRevision("abc123".into()),
                    size,
                )
                .is_err(),
                "bundle_size {size} discarded the named workload",
            );
        }
        // …and at N = 1 it does not claim a plurality it does not have.
        let err = vet_rollback_scope(
            "web",
            Some("web"),
            None,
            &RollbackTarget::GitRevision("abc123".into()),
            1,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("the whole application 'web'"), "{err}");
        assert!(!err.contains("workloads"), "{err}");
    }

    #[test]
    fn everything_that_is_not_a_discarded_workload_proceeds() {
        // The guard must not become a refusal machine: a digest target
        // is per-workload and is exactly what `--workload` addresses, and
        // a Git revision with no `--workload` is the bundle-wide branch
        // working as designed.
        let digest = RollbackTarget::Digest("sha256:beef".into());
        let git = RollbackTarget::GitRevision("v1.2.3".into());
        assert!(vet_rollback_scope("shop", Some("api"), None, &digest, 3).is_ok());
        assert!(vet_rollback_scope("shop", Some("api"), Some("sha256:beef"), &digest, 3).is_ok());
        assert!(vet_rollback_scope("shop", None, Some("v1.2.3"), &git, 3).is_ok());
        assert!(vet_rollback_scope("shop", None, None, &git, 3).is_ok());
    }
}

#[cfg(test)]
mod sort_tests {
    use super::*;
    use serde_json::json;

    fn app(name: &str, env: Option<&str>) -> Value {
        match env {
            Some(e) => json!({
                "metadata": { "name": name, "labels": { "apprafter.io/environment": e } }
            }),
            None => json!({ "metadata": { "name": name } }),
        }
    }

    #[test]
    fn deployments_sort_by_environment_first_then_argo_name() {
        // INVARIANT: `app status` renders one section per deployment, and an
        // unstable order would reshuffle the sections between two runs of
        // the same command.
        let mut apps = [
            app("z-prod", Some("prod")),
            app("a-prod", Some("prod")),
            app("m-dev", Some("dev")),
        ];
        apps.sort_by_key(deployment_sort_key);
        let names: Vec<&str> = apps
            .iter()
            .map(|a| a.pointer("/metadata/name").unwrap().as_str().unwrap())
            .collect();
        assert_eq!(names, ["m-dev", "a-prod", "z-prod"]);
    }

    #[test]
    fn an_unlabelled_deployment_sorts_under_the_base_key() {
        assert_eq!(
            deployment_sort_key(&app("web", None)),
            ("(base)".to_string(), "web".to_string())
        );
    }
}

#[cfg(test)]
mod kubectl_arg_tests {
    use super::*;

    #[test]
    fn claims_are_read_group_qualified_so_dra_cannot_shadow_them() {
        // INVARIANT: a bare `resourceclaim` resolves to the Kubernetes
        // 1.32+ DRA `resourceclaims.resource.k8s.io`, so `app status`
        // would list somebody else's objects — or none — with no error.
        let args = kubectl_list_args(RESOURCECLAIM_RESOURCE, "apps", None);
        assert_eq!(
            args,
            [
                "get",
                "resourceclaim.apprafter.io",
                "-n",
                "apps",
                "-o",
                "json"
            ]
        );
    }

    #[test]
    fn a_selector_is_passed_as_its_own_argument_not_glued_to_the_flag() {
        // `-lfoo=bar` and `-l foo=bar` both work for kubectl, but only the
        // two-argument form survives a label value containing a space.
        let args = kubectl_list_args("pods", "apps", Some("app.kubernetes.io/name=web"));
        assert_eq!(
            args,
            [
                "get",
                "pods",
                "-n",
                "apps",
                "-l",
                "app.kubernetes.io/name=web",
                "-o",
                "json"
            ]
        );
    }

    #[test]
    fn the_output_flag_is_always_last_and_always_json() {
        for args in [
            kubectl_list_args("pods", "apps", None),
            kubectl_list_args("services", "apps", Some("a=b")),
        ] {
            assert_eq!(&args[args.len() - 2..], ["-o", "json"], "{args:?}");
        }
    }

    #[test]
    fn the_workload_selector_uses_the_operators_own_label_key() {
        assert_eq!(
            operator_workload_selector("storefront"),
            "app.kubernetes.io/name=storefront"
        );
    }

    #[test]
    fn the_delete_targets_the_argo_cd_application_in_the_argocd_namespace() {
        assert_eq!(
            kubectl_delete_argo_app_args("web-prod"),
            [
                "delete",
                "application.argoproj.io",
                "web-prod",
                "-n",
                "argocd"
            ]
        );
    }
}

#[cfg(test)]
mod wizard_picker_tests {
    use super::*;

    fn manifest(json: serde_json::Value) -> cli_core::manifest::ApplicationManifest {
        serde_json::from_value(json).expect("manifest fixture")
    }

    #[test]
    fn a_manifest_with_environments_offers_them_all_plus_its_namespace() {
        let m = manifest(serde_json::json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "web", "namespace": "storefront" },
            "spec": { "environments": {
                "prod": { "image": "ghcr.io/acme/web:v1" },
                "dev":  { "image": "ghcr.io/acme/web:dev" }
            }}
        }));
        let (envs, ns) = wizard_manifest_pickers(&m);
        // BTreeMap keys — deterministic, alphabetical.
        assert_eq!(envs, ["dev", "prod"]);
        assert_eq!(ns.as_deref(), Some("storefront"));
    }

    #[test]
    fn a_base_only_manifest_offers_no_environments_rather_than_a_placeholder() {
        // INVARIANT: an empty list is how the wizard learns to HIDE the env
        // picker. Anything else puts an environment on the screen that the
        // manifest cannot render.
        let m = manifest(serde_json::json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "web" },
            "spec": { "base": { "image": "ghcr.io/acme/web:v1" } }
        }));
        let (envs, ns) = wizard_manifest_pickers(&m);
        assert!(envs.is_empty(), "{envs:?}");
        assert_eq!(ns, None);
    }
}

#[cfg(test)]
mod status_render_plan_tests {
    use super::*;
    use crate::commands::app_open::{apprafter_app_refs, CrRef};
    use serde_json::json;

    fn cr_ref(name: &str, namespace: &str) -> CrRef {
        CrRef {
            namespace: Some(namespace.to_string()),
            name: name.to_string(),
        }
    }

    #[test]
    fn a_single_workload_registration_renders_the_full_detail_block() {
        assert!(matches!(
            status_render_plan(&[cr_ref("solo", "ns")], None),
            StatusPlan::Detail(_)
        ));
    }

    #[test]
    fn a_multi_workload_registration_renders_the_summary() {
        assert!(matches!(
            status_render_plan(&[cr_ref("api", "shop"), cr_ref("web", "shop")], None),
            StatusPlan::Summary(_)
        ));
    }

    #[test]
    fn an_explicit_workload_renders_its_detail_block() {
        match status_render_plan(&[cr_ref("api", "shop"), cr_ref("web", "shop")], Some("web")) {
            StatusPlan::Detail(r) => assert_eq!(r.name, "web"),
            other => panic!("expected Detail, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_workload_names_the_ones_that_exist() {
        match status_render_plan(&[cr_ref("api", "shop")], Some("nope")) {
            StatusPlan::UnknownWorkload { asked, available } => {
                assert_eq!(asked, "nope");
                assert_eq!(available, vec!["api".to_string()]);
            }
            other => panic!("expected UnknownWorkload, got {other:?}"),
        }
    }

    #[test]
    fn workload_selector_on_a_single_workload_registration_still_validates() {
        // `--workload` naming the one workload is fine; naming a different one
        // is an error even at N=1, or a typo silently shows the wrong app.
        assert!(matches!(
            status_render_plan(&[cr_ref("solo", "ns")], Some("solo")),
            StatusPlan::Detail(_)
        ));
        assert!(matches!(
            status_render_plan(&[cr_ref("solo", "ns")], Some("other")),
            StatusPlan::UnknownWorkload { .. }
        ));
    }

    #[test]
    fn a_registration_with_no_tracked_workload_is_not_a_summary() {
        // Before the first sync `status.resources[]` is empty, and that is
        // NOT "this registration deploys nothing" — it is "nothing is known
        // yet". An empty summary table would assert the first. `NoWorkloads`
        // carries today's `(workload detail unavailable — app not synced
        // yet)` line, unchanged, which is what the (None, _) arm of
        // `print_app_detail` printed before this change.
        assert_eq!(status_render_plan(&[], None), StatusPlan::NoWorkloads);
        // With a selector too: "there is no workload `api`" would be a
        // narrower and WRONGER claim than "nothing has synced yet", and
        // `available: []` names nothing for the reader to correct towards.
        assert_eq!(
            status_render_plan(&[], Some("api")),
            StatusPlan::NoWorkloads
        );
    }

    #[test]
    fn a_crashlooping_workload_is_in_the_roll_up_even_though_its_phase_says_ready() {
        // THE case this subphase exists for. The operator writes
        // `status.phase = Ready` the moment it applies the Deployment
        // (`operator-controllers/application/src/lib.rs:1072`), so a
        // workload in CrashLoopBackOff carries `phase: Ready`. On phase
        // alone the roll-up stays silent and the hint points at the
        // HEALTHY sibling — the summary's one designated mitigation (ADR
        // 0062 §Risks: "its presence is the signal") failing on exactly
        // the shape it mitigates. The pod count is already on the row.
        let lines = workload_summary_lines(
            "shop",
            "shop",
            &[
                workload_summary("api", "Ready", "2/2", "ghcr.io/acme/api:1.9"),
                workload_summary("worker", "Ready", "0/1", "ghcr.io/acme/worker:1.9"),
            ],
        );
        let joined = lines.join("\n");
        assert!(joined.contains("1 of 2 workloads is not Ready"), "{joined}");
        assert!(
            joined.contains("apprafter app status shop --workload worker"),
            "the hint must name the BROKEN workload, not the healthy one: {joined}"
        );
    }

    #[test]
    fn a_partially_rolled_workload_is_not_ready_but_a_scaled_to_zero_one_is_left_alone() {
        // `ready < total` is the rule, so 1/3 is in the roll-up …
        let rolling = workload_summary_lines(
            "shop",
            "shop",
            &[
                workload_summary("api", "Ready", "1/3", "x"),
                workload_summary("web", "Ready", "1/1", "x"),
            ],
        );
        assert!(
            rolling.join("\n").contains("1 of 2 workloads is not Ready"),
            "{rolling:?}"
        );

        // … and `0/0` is NOT: ready == total, and a deliberately
        // scaled-to-zero workload must not nag on every run.
        let scaled_to_zero = workload_summary_lines(
            "shop",
            "shop",
            &[
                workload_summary("api", "Ready", "0/0", "x"),
                workload_summary("web", "Ready", "1/1", "x"),
            ],
        );
        assert!(
            !scaled_to_zero.join("\n").contains("not Ready"),
            "{scaled_to_zero:?}"
        );
    }

    #[test]
    fn a_pod_count_that_did_not_read_is_not_ready() {
        // Same rule as the phase: this table never lets something
        // unobserved render as fine. The failed-read warning that put the
        // em-dash there is printed beside the table.
        let lines = workload_summary_lines(
            "shop",
            "shop",
            &[
                workload_summary("api", "Ready", "1/1", "x"),
                workload_summary("web", "Ready", "—", "x"),
            ],
        );
        assert!(
            lines.join("\n").contains("1 of 2 workloads is not Ready"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_selector_is_matched_by_the_plan_not_by_whether_the_block_can_render() {
        // A workload whose namespace is UNKNOWN is a real state —
        // `apprafter_app_refs_reports_an_unknown_namespace_as_none` — and
        // naming it in `--workload` IS a match: the name was found, only
        // the rendering is impossible. Deriving "matched" from "rendered"
        // made `status` answer `deploys no workload 'web'. It deploys:
        // web.` and exit non-zero on a state that is not a user error.
        let unknown_ns = [CrRef {
            namespace: None,
            name: "web".to_string(),
        }];
        let plan = status_render_plan(&unknown_ns, Some("web"));
        assert!(matches!(plan, StatusPlan::Detail(_)), "{plan:?}");
        assert!(plan_matched_selector(&plan), "{plan:?}");

        // The three arms that are NOT an answer to the selector, and the
        // one that is vacuously one.
        assert!(!plan_matched_selector(&status_render_plan(
            &[],
            Some("web")
        )));
        assert!(!plan_matched_selector(&status_render_plan(
            &[cr_ref("api", "shop")],
            Some("nope")
        )));
        assert!(plan_matched_selector(&status_render_plan(
            &[cr_ref("api", "shop"), cr_ref("web", "shop")],
            None
        )));
    }

    #[test]
    fn the_unknown_workload_error_names_what_exists_and_the_addressing_rule() {
        let msg = unknown_workload_message("shop", "nope", &["api".into(), "web".into()]);
        assert!(msg.contains("'nope'"), "{msg}");
        assert!(msg.contains("It deploys: api, web."), "{msg}");
        assert!(msg.contains("--workload"), "{msg}");

        // Registered but unsynced: there is nothing to list, and "It
        // deploys: ." would read as a rendering bug.
        let unsynced = unknown_workload_message("shop", "api", &[]);
        assert!(!unsynced.contains("It deploys:"), "{unsynced}");
        assert!(unsynced.contains("has not synced"), "{unsynced}");
    }

    #[test]
    fn summary_lines_name_every_workload_and_point_at_the_selector() {
        let lines = workload_summary_lines(
            "shop",
            "shop",
            &[
                workload_summary("api", "Ready", "2/2", "ghcr.io/acme/api:1.9"),
                workload_summary(
                    "worker",
                    "EnvSecretMissing",
                    "0/1",
                    "ghcr.io/acme/worker:1.9",
                ),
            ],
        );
        let joined = lines.join("\n");
        assert!(joined.contains("Workloads (2, namespace shop)"), "{joined}");
        assert!(joined.contains("worker"), "{joined}");
        assert!(joined.contains("1 of 2 workloads is not Ready"), "{joined}");
        assert!(
            joined.contains("apprafter app status shop --workload worker"),
            "{joined}"
        );
    }

    #[test]
    fn summary_omits_the_roll_up_when_every_workload_is_ready() {
        // The table above it already proves the command ran, so an "all fine"
        // line here is noise — the inverse of `apprafter status`, whose silence
        // WOULD be ambiguous. Same reasoning as env_deployment_index_lines.
        let lines = workload_summary_lines(
            "shop",
            "shop",
            &[
                workload_summary("api", "Ready", "2/2", "x"),
                workload_summary("web", "Ready", "1/1", "x"),
            ],
        );
        assert!(!lines.join("\n").contains("not Ready"));
    }

    #[test]
    fn the_roll_up_counts_a_workload_whose_phase_did_not_read_as_not_ready() {
        // The em-dash is "unmeasured", and the one thing this subphase exists
        // to stop is an unobserved workload rendering as a clean bill of
        // health. The failed-read warning sits beside the table; the roll-up
        // must not quietly exclude the row it covers.
        let lines = workload_summary_lines(
            "shop",
            "shop",
            &[
                workload_summary("api", "Ready", "2/2", "x"),
                workload_summary("web", "—", "—", "—"),
            ],
        );
        assert!(
            lines.join("\n").contains("1 of 2 workloads is not Ready"),
            "{lines:?}"
        );
    }

    #[test]
    fn the_roll_up_is_plural_past_one() {
        let lines = workload_summary_lines(
            "shop",
            "shop",
            &[
                workload_summary("api", "Pending", "0/1", "x"),
                workload_summary("web", "Failed", "0/1", "x"),
            ],
        );
        assert!(
            lines.join("\n").contains("2 of 2 workloads are not Ready"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_bundle_namespace_is_the_one_every_workload_agrees_on() {
        assert_eq!(
            bundle_namespace(&[cr_ref("api", "shop"), cr_ref("web", "shop")]).as_deref(),
            Some("shop")
        );
        // Disagreement is not a thing to average: ADR 0062 says one bundle
        // is one namespace, so two answers means the caller must not read
        // either one as "the" namespace.
        assert_eq!(
            bundle_namespace(&[cr_ref("api", "shop"), cr_ref("web", "other")]),
            None
        );
        // UNKNOWN (CrRef::namespace == None) is never silently dropped.
        assert_eq!(
            bundle_namespace(&[
                cr_ref("api", "shop"),
                CrRef {
                    namespace: None,
                    name: "web".to_string()
                }
            ]),
            None
        );
        assert_eq!(bundle_namespace(&[]), None);
    }

    #[test]
    fn the_summary_reads_pods_off_the_label_the_operator_stamps_on_every_pod() {
        // `operator-rendering::make_labels` puts BOTH `apprafter=true` and
        // `app.kubernetes.io/name=<cr>` on the pod template, so one read
        // covers the whole bundle and the bucket key is the workload name.
        let items = vec![
            json!({
                "metadata": { "labels": { "apprafter": "true", "app.kubernetes.io/name": "api" } },
                "status": { "conditions": [{ "type": "Ready", "status": "True" }] }
            }),
            json!({
                "metadata": { "labels": { "apprafter": "true", "app.kubernetes.io/name": "api" } },
                "status": { "conditions": [{ "type": "Ready", "status": "False" }] }
            }),
            json!({
                "metadata": { "labels": { "apprafter": "true", "app.kubernetes.io/name": "web" } },
                "status": { "conditions": [{ "type": "Ready", "status": "True" }] }
            }),
            // A pod with no workload label belongs to nothing this command
            // speaks for and must not inflate a bucket.
            json!({ "metadata": { "labels": { "apprafter": "true" } }, "status": {} }),
        ];
        let buckets = bucket_pod_readiness(&items);
        assert_eq!(buckets.get("api"), Some(&(1, 2)));
        assert_eq!(buckets.get("web"), Some(&(1, 1)));
        assert_eq!(buckets.len(), 2);
    }

    #[test]
    fn the_image_cell_prefers_what_the_operator_resolved() {
        // status.image.tag is what the operator actually deployed (env merge
        // already applied). It is absent under `imagePolicy.resolve: off`,
        // and then the declared image is the honest answer.
        let resolved = json!({
            "spec": { "base": { "image": "ghcr.io/acme/web:declared" } },
            "status": { "image": { "tag": "ghcr.io/acme/web:1.9" } }
        });
        assert_eq!(workload_image_cell(&resolved), "ghcr.io/acme/web:1.9");

        let per_env = json!({
            "spec": {
                "environment": "prod",
                "base": { "image": "ghcr.io/acme/web:base" },
                "environments": { "prod": { "image": "ghcr.io/acme/web:prod" } }
            }
        });
        assert_eq!(workload_image_cell(&per_env), "ghcr.io/acme/web:prod");

        let base_only = json!({ "spec": { "base": { "image": "ghcr.io/acme/web:base" } } });
        assert_eq!(workload_image_cell(&base_only), "ghcr.io/acme/web:base");

        // Unknown is the em-dash, never a blank: a blank reads as healthy.
        assert_eq!(workload_image_cell(&json!({})), "—");
    }

    // ── N=1 byte-identity (Step 3) ────────────────────────────────────

    /// The detail block a single-workload registration renders, assembled
    /// from the same pure renderers `print_workload_detail` calls, in the
    /// same order. `print_workload_detail` itself interleaves four kubectl
    /// reads and writes its failures to stderr, so it cannot be called from
    /// a test; what is pinned here is every line of TEXT it emits on the
    /// happy path, which is what a fleet-wide regression would show.
    ///
    /// The ORDER below is this test's own, not an observation of the
    /// function's — which means this test cannot see what the function
    /// does with these helpers at all. Confirmed by mutation: swapping
    /// the Pods and Services sections there, deleting the Services
    /// section there, and transposing `inner_name`/`dest_ns` at a call
    /// site there each leave the suite green. Omission and argument
    /// swaps, not only ordering. `print_workload_detail`'s own doc
    /// comment carries the full statement of the gap and why it is left
    /// open.
    fn assembled_detail_lines(
        argo: &Value,
        cr: &Value,
        pods: &Value,
        services: &Value,
        claims: &Value,
        workload: &CrRef,
        now: &chrono::DateTime<chrono::Utc>,
    ) -> Vec<String> {
        let inner = workload.name.as_str();
        // UNKNOWN never reaches a namespaced read — the same refusal
        // `print_app_detail` makes before calling `print_workload_detail`.
        let ns = workload.namespace.as_deref().expect("a known namespace");
        let mut out = status_detail_lines(argo);
        out.extend(recent_revision_lines(argo));
        out.extend(
            apprafter_cr_advisory_lines(cr, now)
                .into_iter()
                .map(|l| l.text),
        );
        let config_changed_at = cr
            .pointer("/status/envConfig/changedAt")
            .and_then(Value::as_str);
        out.extend(
            render_pod_summary_lines(
                &parse_pod_summaries(pods, now),
                inner,
                ns,
                config_changed_at,
            )
            .into_iter()
            .map(|l| l.text),
        );
        out.extend(render_service_lines(
            &parse_service_summaries(services),
            inner,
            ns,
        ));
        out.extend(render_resource_claim_lines(
            &parse_resource_claim_summaries(claims, inner),
            ns,
        ));
        out.extend(render_secret_binding_lines(cr, inner, ns));
        out
    }

    #[test]
    fn a_single_workload_bundle_still_renders_todays_detail_block_verbatim() {
        let argo = json!({
            "metadata": { "name": "landing-web" },
            "spec": {
                "project": "apps",
                "source": {
                    "repoURL": "https://github.com/acme/landing-web",
                    "targetRevision": "master",
                    "path": "apprafter"
                },
                "destination": { "namespace": "landing" }
            },
            "status": {
                "sync": { "status": "Synced" },
                "health": { "status": "Healthy" },
                "history": [
                    { "id": 6, "revision": "a1b2c3d", "deployedAt": "2026-09-14T10:00:00Z" }
                ],
                "resources": [{
                    "group": "apprafter.io",
                    "version": "v1alpha1",
                    "kind": "Application",
                    "name": "landing-web",
                    "namespace": "landing",
                    "health": { "status": "Healthy" }
                }]
            }
        });
        let cr = json!({
            "metadata": { "name": "landing-web", "namespace": "landing" },
            "spec": { "base": {
                "image": "ghcr.io/acme/landing-web:1.9",
                "env": { "SESSION_KEY": { "secret": "landing-web-session/key" } }
            }},
            "status": {
                "phase": "Ready",
                "image": {
                    "tag": "ghcr.io/acme/landing-web:1.9",
                    "resolved": "ghcr.io/acme/landing-web@sha256:beef",
                    "resolvedAt": "2026-09-14T11:55:00Z"
                }
            }
        });
        let pods = json!({ "items": [{
            "metadata": { "name": "landing-web-7d4f-abc", "creationTimestamp": "2026-09-14T11:00:00Z" },
            "spec": { "containers": [{ "name": "landing-web" }] },
            "status": {
                "phase": "Running",
                "startTime": "2026-09-14T11:00:00Z",
                "containerStatuses": [{
                    "ready": true,
                    "restartCount": 0,
                    "state": { "running": { "startedAt": "2026-09-14T11:00:05Z" } }
                }]
            }
        }]});
        let services = json!({ "items": [{
            "metadata": { "name": "landing-web" },
            "spec": {
                "type": "ClusterIP",
                "clusterIP": "10.43.7.21",
                "ports": [{ "port": 3000, "protocol": "TCP" }]
            }
        }]});
        let claims = json!({ "items": [] });
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-14T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        // The decision comes first: at N=1 the plan MUST be Detail, and the
        // block below is what Detail renders. Fold the two and a change that
        // turned N=1 into a summary would fail here as well as above.
        let refs = apprafter_app_refs(&argo);
        let StatusPlan::Detail(r) = status_render_plan(&refs, None) else {
            panic!("a single-workload registration must render the detail block");
        };
        assert_eq!(r.name, "landing-web");
        assert_eq!(r.namespace.as_deref(), Some("landing"));

        let rendered =
            assembled_detail_lines(&argo, &cr, &pods, &services, &claims, &r, &now).join("\n");

        assert_eq!(rendered, GOLDEN_SINGLE_WORKLOAD_DETAIL, "\n{rendered}");
    }

    const GOLDEN_SINGLE_WORKLOAD_DETAIL: &str = "\
Application argocd/landing-web
  project:       apps
  repo:          https://github.com/acme/landing-web
  revision:      master
  path:          apprafter
  destination:   landing
  environment:   (base)
  sync state:    Synced
  health:        Healthy

Recent revisions (last 1):
  #  6 a1b2c3d    2026-09-14T10:00:00Z
AppRafter phase: Ready
  image:         ghcr.io/acme/landing-web:1.9 -> @sha256:beef (resolved 5m ago)

Workload pods (landing, app.kubernetes.io/name=landing-web):
  NAME                  READY  STATUS   RESTARTS  AGE
  landing-web-7d4f-abc  1/1    Running  0         1h

Workload services (landing, app.kubernetes.io/name=landing-web):
  NAME         TYPE       CLUSTER-IP  PORTS
  landing-web  ClusterIP  10.43.7.21  3000/TCP

Resource provisioning (landing):
  (none — the AppRafter Application declares no `needs.*` resources)

Secrets (landing/landing-web):
  ENV          SECRET/KEY  (SCOPE)
  SESSION_KEY  landing-web-session/key  (base)";
}
