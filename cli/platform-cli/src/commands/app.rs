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

use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::Command;

use cli_core::{CliError, Result};
use serde_json::{json, Value};
use tabled::{Table, Tabled};

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_get_json_by_selector, kubectl_merge_patch,
};

const ARGOCD_NAMESPACE: &str = "argocd";
const APPRAFTER_MANAGED_LABEL: &str = "apprafter.io/managed-by=apprafter";
const APPRAFTER_SOURCE_ANNOTATION: &str = "apprafter.io/source";
/// Argo CD cascade-deletion finalizer (background variant). EXACT string
/// per the Argo CD docs — note `resources-finalizer` (NOT
/// `resources-finalization`); a typo here makes Argo ignore it and the
/// Application hangs in `Terminating` forever. `/background` deletes the
/// managed resources without blocking the delete call on child cleanup.
const ARGOCD_CASCADE_FINALIZER: &str = "resources-finalizer.argocd.argoproj.io/background";

#[derive(Tabled)]
struct AppRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "ENV")]
    env: String,
    #[tabled(rename = "PROJECT")]
    project: String,
    #[tabled(rename = "REPO")]
    repo: String,
    #[tabled(rename = "REV")]
    revision: String,
    #[tabled(rename = "SYNC")]
    sync: String,
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
                if !envs.iter().any(|d| d == e) {
                    let declared = if envs.is_empty() {
                        "(none declared)".to_string()
                    } else {
                        envs.join(", ")
                    };
                    return Err(CliError::Other(format!(
                        "environment '{e}' is not declared in this app's manifest. \
                         Declared environments: {declared}. Add a \
                         `spec.environments.{e}` block to apprafter/Application.cue, \
                         or pass one of the declared environments to `--env`."
                    )));
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
        // Post-2.9 UX: `app status` aggregates ALL env-deployments of one
        // logical app, and `app remove --env` targets a single one — so
        // suggest the LOGICAL name (+ env), not the `<name>-<env>` Argo
        // name. A base-only collision (env: None) drops the `--env` hint.
        let remove_hint = match env.as_deref() {
            Some(e) => format!("apprafter app remove {derived_name} --env {e}"),
            None => format!("apprafter app remove {derived_name}"),
        };
        return Err(CliError::Other(format!(
            "Application '{argo_app_name}' is already registered in namespace \
             {ARGOCD_NAMESPACE}. Run `apprafter app status {derived_name}` to inspect all \
             environment deployments of this app, `{remove_hint}` to cascade-delete this \
             one, or pass a different `--name` / `--env`."
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

    println!("✓ Application '{argo_app_name}' registered in AppProject '{project}'.");
    println!("  Repo:        {repo_url}");
    println!("  Revision:    {target_revision}");
    println!("  Path:        {path}");
    println!("  Destination: {effective_namespace} (created if missing)");
    match env.as_deref() {
        Some(e) => println!("  Environment: {e}"),
        None => {
            // Surface the cluster's soft default so the operator knows
            // which env a base-only deploy renders against. Best-effort —
            // a missing PlatformStack simply omits the line.
            if let Ok(Some(default_env)) = fetch_platformstack_default_env(kc.path()) {
                println!(
                    "  Environment: (base — cluster default is '{default_env}'; \
                     pass `--env {default_env}` to pin it)"
                );
            }
        }
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
    if crate::commands::repo_creds::valid_credential_covers(&creds, repo_url) {
        return Ok(());
    }
    let detail = if crate::commands::repo_creds::any_credential_covers(&creds, repo_url) {
        "a SourceCredential covers it but its status is not GitValid=True \
         (Unverified or Invalid). Validate the credential, or rerun with \
         `--coverage-gate present` (the default) if the cluster has no egress to validate"
    } else {
        "no SourceCredential covers it. Register one with `apprafter repo creds add`, \
         or rerun with `--coverage-gate present` (the default) if the repo is public"
    };
    Err(CliError::Other(format!(
        "coverage gate (confirmed): {repo_url} not admitted — {detail}."
    )))
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

    let suggestion = derive_creds_suggestion(repo_url);

    println!();
    println!("ℹ {repo_url} is private (no anonymous Git access) and has no");
    println!("  matching credentials in Argo CD — register one so the repo-server");
    println!("  can clone it:");

    if let Some(ref s) = suggestion {
        if let Some(ref pat_url) = s.pat_creation_url {
            println!("    1. Generate a PAT here:");
            println!("       {pat_url}");
            println!("       Required scopes: `repo` for code; add `read:packages`");
            println!("       if your CI publishes container images to the same provider.");
            println!("    2. Register it with AppRafter:");
            println!(
                "       apprafter repo creds add {} --url-prefix {} --token <paste-the-pat>",
                s.suggested_name, s.url_prefix
            );
        } else {
            println!(
                "    apprafter repo creds add {} --url-prefix {} --token <pat>",
                s.suggested_name, s.url_prefix
            );
        }
    } else {
        println!("    apprafter repo creds add <name> --url-prefix <prefix> --token <pat>");
    }
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
    // `Ok(vec![])` = manifest declares no environments (base-only — no
    // picker, no warning). An `Err` = the manifest couldn't be rendered;
    // surface it instead of silently hiding the env picker (the cause is
    // usually an incomplete/invalid manifest, not a base-only one).
    let declared_envs = match get_manifest_environments(&cwd) {
        Ok(envs) => envs,
        Err(e) => {
            eprintln!(
                "⚠ Could not read declared environments from apprafter/Application.cue \
                 ({e}); the environment picker is hidden. Pass `--env <env>` to pin one."
            );
            Vec::new()
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

    let mut cmd = Command::new("kubectl");
    cmd.arg("get")
        .arg("application.argoproj.io")
        .arg("-n")
        .arg(ARGOCD_NAMESPACE)
        .arg("-o")
        .arg("json")
        .env("KUBECONFIG", kc.path());
    if !all_managed {
        cmd.arg("-l").arg(APPRAFTER_MANAGED_LABEL);
    }

    let out = cmd
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

    let items: Vec<Value> = parsed
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let filtered: Vec<Value> = if all_projects {
        items
    } else {
        items
            .into_iter()
            .filter(|app| {
                app.pointer("/spec/project")
                    .and_then(Value::as_str)
                    .map(|p| p == project)
                    .unwrap_or(false)
            })
            .collect()
    };

    if filtered.is_empty() {
        if all_projects {
            println!("No apprafter-managed Applications in the cluster.");
        } else {
            println!("No apprafter-managed Applications in AppProject '{project}'.");
        }
        if !all_managed {
            println!(
                "Hint: try `--all-managed` to list Applications that were not registered \
                 through `apprafter app add`."
            );
        }
        return Ok(());
    }

    let rows: Vec<AppRow> = filtered.iter().map(app_row).collect();
    println!("{}", Table::new(&rows));
    Ok(())
}

pub fn status(name: &str, show_resources: bool) -> Result<()> {
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
    let summaries = summarize_deployments(&apps);
    if summaries.len() > 1 {
        println!(
            "Application '{name}' — {} environment deployments:",
            summaries.len()
        );
        for s in &summaries {
            println!("  • {} ({})", s.argo_name, s.environment);
        }
        println!();
    }

    let mut sorted: Vec<&Value> = apps.iter().collect();
    sorted.sort_by_key(|a| deployment_sort_key(a));

    let last = sorted.len().saturating_sub(1);
    for (idx, app) in sorted.iter().enumerate() {
        print_app_detail(app, show_resources, kc.path());
        if idx != last {
            println!();
            println!("{}", "─".repeat(40));
            println!();
        }
    }

    Ok(())
}

/// Render ONE Argo CD Application's full status block — the Argo CD
/// summary (`print_status`) plus the inner AppRafter CR phase, pods,
/// services, and resource claims (each a best-effort kubectl read).
/// Factored out of `status` so the per-environment aggregation loop
/// reuses the identical single-app rendering for every `<name>-<env>`.
fn print_app_detail(app: &Value, show_resources: bool, kubeconfig_path: &Path) {
    print_status(app);

    // Resolve the INNER workload name + destination namespace
    // once — all four default-path fetches key off them. The
    // inner name is the operator's `app.kubernetes.io/name`
    // label value (the AppRafter Application CR's metadata.name,
    // which can differ from the Argo CD parent); dest_ns is
    // where Argo CD lays down children. Both come from
    // `status.resources[]` / `spec.destination.namespace`, so a
    // not-yet-synced app yields `None` for either — in which
    // case the workload detail is simply unavailable yet.
    let inner = crate::commands::app_open::find_apprafter_app_name(app);
    let dest_ns = app
        .pointer("/spec/destination/namespace")
        .and_then(Value::as_str);

    match (inner.as_deref(), dest_ns) {
        (Some(inner_name), Some(dest_ns)) => {
            // 1. AppRafter Application phase (group-qualified
            //    apprafter.io read). Non-fatal — a missing CR /
            //    absent phase simply skips the line.
            match kubectl_get_json(
                "application.apprafter.io",
                Some(inner_name),
                Some(dest_ns),
                kubeconfig_path,
            ) {
                Ok(Some(cr)) => {
                    if let Some(phase) = cr.pointer("/status/phase").and_then(Value::as_str) {
                        println!("AppRafter phase: {phase}");
                    }
                    // ADR 0040: surface the running image digest the
                    // operator resolved from base.image's tag. Omitted
                    // when status.image is absent (resolve: off, or
                    // pre-first-resolve).
                    if let Some(image_line) = format_image_line(&cr, &chrono::Utc::now()) {
                        println!("{image_line}");
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!(
                        "⚠ Could not fetch AppRafter Application phase ({e}). \
                         Argo CD's view (above) is still authoritative."
                    );
                }
            }

            // 2. Pods (moved out of --resources). Non-fatal.
            match list_pods_for_apprafter_app(inner_name, dest_ns, kubeconfig_path) {
                Ok(pods) => print_pod_summaries(&pods, inner_name, dest_ns),
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
        }
        _ => {
            println!();
            println!("(workload detail unavailable — app not synced yet)");
        }
    }

    if show_resources {
        print_argocd_resources(app);
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
    let selector = format!("app.kubernetes.io/name={apprafter_app_name}");
    let out = Command::new("kubectl")
        .arg("get")
        .arg("pods")
        .arg("-n")
        .arg(namespace)
        .arg("-l")
        .arg(&selector)
        .arg("-o")
        .arg("json")
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

    PodSummary {
        name,
        ready: format!("{ready_count}/{total_count}"),
        status,
        restarts,
        age,
    }
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

fn print_argocd_resources(app: &Value) {
    let resources = extract_tracked_resources(app);
    println!();
    println!("Argo CD tracked resources:");
    if resources.is_empty() {
        println!("  (none — Application has not yet reported `status.resources[]`)");
        return;
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
    println!(
        "  {:<name_w$}  {:<kind_w$}  {:<ns_w$}  {:<status_w$}  HEALTH",
        "NAME",
        "KIND",
        "NAMESPACE",
        "STATUS",
        name_w = name_w,
        kind_w = kind_w,
        ns_w = ns_w,
        status_w = status_w,
    );
    for r in &resources {
        println!(
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
        );
    }
}

fn print_pod_summaries(pods: &[PodSummary], inner_name: &str, namespace: &str) {
    println!();
    println!("Workload pods ({namespace}, app.kubernetes.io/name={inner_name}):");
    if pods.is_empty() {
        println!(
            "  (none — operator may not have rendered the Deployment yet, or \
             the AppRafter Application's `spec.expose` is omitted so no \
             workload runs)"
        );
        return;
    }
    let name_w = pods.iter().map(|p| p.name.len()).max().unwrap_or(4).max(4);
    let status_w = pods
        .iter()
        .map(|p| p.status.len())
        .max()
        .unwrap_or(6)
        .max(6);
    println!(
        "  {:<name_w$}  READY  {:<status_w$}  RESTARTS  AGE",
        "NAME",
        "STATUS",
        name_w = name_w,
        status_w = status_w,
    );
    for p in pods {
        println!(
            "  {:<name_w$}  {:<5}  {:<status_w$}  {:<8}  {}",
            p.name,
            p.ready,
            p.status,
            p.restarts,
            p.age,
            name_w = name_w,
            status_w = status_w,
        );
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
    let selector = format!("app.kubernetes.io/name={apprafter_app_name}");
    let out = Command::new("kubectl")
        .arg("get")
        .arg("services")
        .arg("-n")
        .arg(namespace)
        .arg("-l")
        .arg(&selector)
        .arg("-o")
        .arg("json")
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

fn print_service_summaries(services: &[ServiceSummary], inner_name: &str, namespace: &str) {
    println!();
    println!("Workload services ({namespace}, app.kubernetes.io/name={inner_name}):");
    if services.is_empty() {
        println!(
            "  (none — the AppRafter Application's `spec.expose` may be omitted \
             so the operator renders no Service)"
        );
        return;
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
    println!(
        "  {:<name_w$}  {:<type_w$}  {:<ip_w$}  PORTS",
        "NAME",
        "TYPE",
        "CLUSTER-IP",
        name_w = name_w,
        type_w = type_w,
        ip_w = ip_w,
    );
    for s in services {
        println!(
            "  {:<name_w$}  {:<type_w$}  {:<ip_w$}  {}",
            s.name,
            s.type_,
            s.cluster_ip,
            s.ports,
            name_w = name_w,
            type_w = type_w,
            ip_w = ip_w,
        );
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
    let out = Command::new("kubectl")
        .arg("get")
        .arg("resourceclaim.apprafter.io")
        .arg("-n")
        .arg(namespace)
        .arg("-o")
        .arg("json")
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
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
    Ok(parse_resource_claim_summaries(&parsed, owner))
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
    }
}

fn print_resource_claims(claims: &[ResourceClaimSummary], namespace: &str) {
    println!();
    println!("Resource provisioning ({namespace}):");
    if claims.is_empty() {
        println!("  (none — the AppRafter Application declares no `needs.*` resources)");
        return;
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
    println!(
        "  {:<name_w$}  {:<provider_w$}  READY  SCHEDULED  SECRET",
        "NAME",
        "PROVIDER",
        name_w = name_w,
        provider_w = provider_w,
    );
    for c in claims {
        let secret = c.secret_ref.as_deref().unwrap_or("-");
        println!(
            "  {:<name_w$}  {:<provider_w$}  {:<5}  {:<9}  {}",
            c.name,
            c.provider,
            if c.ready { "true" } else { "false" },
            if c.scheduled { "true" } else { "false" },
            secret,
            name_w = name_w,
            provider_w = provider_w,
        );
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

fn resolve_app_for_command(name: &str, env: Option<&str>, kc: &Path) -> Result<(Value, String)> {
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
            let envs: Vec<String> = grouped
                .iter()
                .filter_map(|a| {
                    a.pointer("/metadata/labels/apprafter.io~1environment")
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .collect();
            return Err(CliError::Other(per_env_guidance_message(name, &envs)));
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
pub fn logs(
    name: &str,
    env: Option<String>,
    follow: bool,
    tail: i64,
    container: Option<String>,
    pod: Option<String>,
) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    let (app, argo_name) = resolve_app_for_command(name, env.as_deref(), kc.path())?;

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

    // The workload pods carry the INNER AppRafter app name
    // (operator's `app.kubernetes.io/name`), which is the
    // `Application.cue` metadata.name and can differ from the Argo
    // CD parent name typed at `app add`. Resolve it from
    // status.resources[]; fall back to the resolved Argo name
    // (`<name>-<env>` for an env deploy) for raw-YAML apps that carry
    // no AppRafter Application CR.
    let inner = crate::commands::app_open::find_apprafter_app_name(&app)
        .unwrap_or_else(|| argo_name.clone());
    let target = build_kubectl_logs_target(&inner, pod.as_deref());
    let args = build_kubectl_logs_args(&target, workload_ns, follow, tail, container.as_deref());

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

/// `apprafter app rollback <name> [--to <rev>]` — patches
/// `spec.source.targetRevision` to the requested revision (or
/// the previous entry in `status.history` when `--to` is
/// omitted). Argo CD's automated sync picks up the change on
/// the next reconcile and rolls back the workload.
pub fn rollback(name: &str, env: Option<String>, to: Option<String>, yes: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let (app, argo_name) = resolve_app_for_command(name, env.as_deref(), kc.path())?;

    let current_revision = app
        .pointer("/spec/source/targetRevision")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();

    let target_revision = match to {
        Some(explicit) => explicit,
        None => pick_previous_revision(&app)?.to_string(),
    };

    if target_revision == current_revision {
        return Err(CliError::Other(format!(
            "Target revision '{target_revision}' matches the current \
             `spec.source.targetRevision` — rollback would be a no-op."
        )));
    }

    if !yes {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!(
            "Roll back Application '{argo_name}' from revision '{current_revision}' to \
             '{target_revision}'?"
        );
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let body = format!(r#"{{"spec":{{"source":{{"targetRevision":"{target_revision}"}}}}}}"#);
    // Patch the RESOLVED Argo CD object (`<name>-<env>` for an env
    // deploy), NOT the bare logical name — otherwise a per-env rollback
    // would target a non-existent `<name>` Application.
    kubectl_merge_patch(
        "application.argoproj.io",
        &argo_name,
        Some(ARGOCD_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;

    println!(
        "✓ Application '{argo_name}' rolled back to revision '{target_revision}'. Argo CD will \
         sync the workload within a reconcile cycle."
    );
    Ok(())
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

    // Backward-compat: a pre-2.9 / base-only app carries no
    // `apprafter.io/application` label, so the selector matches nothing.
    // Fall back to deleting the single `<name>` Application. Exactly one
    // labelled match also routes here (its own Argo name is `<name>` or
    // `<name>-<env>` — use the actual metadata.name).
    if grouped.len() <= 1 {
        let argo_app_name = grouped
            .first()
            .and_then(|a| a.pointer("/metadata/name").and_then(Value::as_str))
            .map(String::from)
            .unwrap_or_else(|| name.to_string());
        return remove_single_app(&argo_app_name, yes, keep_data, kc.path());
    }

    // Multiple env-deployments. Collect their Argo names for the
    // confirmation prompt + the delete loop.
    let argo_names: Vec<String> = grouped
        .iter()
        .filter_map(|a| {
            a.pointer("/metadata/name")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .collect();

    if !yes {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!(
            "Delete ALL {} environment deployments of '{name}'?",
            argo_names.len()
        );
        for an in &argo_names {
            println!("  • {an}");
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

/// Delete ONE Argo CD Application by its exact `metadata.name`. Carries
/// the confirm / finalizer-strip / kubectl-delete / report flow that
/// both `remove` paths reuse. `--keep-data` strips the cascade finalizer
/// so the synced AppRafter CR (and its workload) is preserved.
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
    let app = existing.ok_or_else(|| {
        CliError::Other(format!(
            "Application '{argo_app_name}' not found in namespace {ARGOCD_NAMESPACE}."
        ))
    })?;

    if !yes {
        // Interactive confirm: refuse silently without a TTY
        // when `--yes` was not passed. Symmetric with the
        // `apprafter target remove` ergonomics.
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        let project = app
            .pointer("/spec/project")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let repo = app
            .pointer("/spec/source/repoURL")
            .and_then(Value::as_str)
            .unwrap_or("?");
        println!("Delete Application '{argo_app_name}' (project: {project}, repo: {repo})?");
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
        .arg("delete")
        .arg("application.argoproj.io")
        .arg(argo_app_name)
        .arg("-n")
        .arg(ARGOCD_NAMESPACE)
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

    if keep_data {
        println!(
            "✓ Application '{argo_app_name}' deleted (Argo CD object only). The workload and its \
             AppRafter Application CR are preserved — re-register to re-adopt."
        );
    } else {
        println!(
            "✓ Application '{argo_app_name}' deleted. Argo CD cascade-prunes the synced AppRafter \
             resources; the operator then removes the workload."
        );
    }
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
    let raw_url = String::from_utf8_lossy(&remote_out.stdout)
        .trim()
        .to_string();
    if raw_url.is_empty() {
        return Err(CliError::Other(format!(
            "git remote {remote} returned an empty URL"
        )));
    }
    let url = normalise_git_url(&raw_url);

    let branch_out = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .output();
    let branch = match branch_out {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => None,
    };
    Ok((url, branch))
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
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lower = stderr.to_lowercase();
    if lower.contains("authentication") || lower.contains("permission denied") {
        return Err(CliError::Other(format!(
            "git ls-remote refused access to {repo_url}.\n\
             {stderr}\n\
             Register creds via `apprafter repo creds add` and retry `apprafter app add`."
        )));
    }
    Err(CliError::Other(format!(
        "git ls-remote {repo_url} failed (exit {:?}): {stderr}",
        out.status.code()
    )))
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
        KubectlLogsTarget::Selector(selector) => {
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
    if matches!(target, KubectlLogsTarget::Selector(_)) {
        args.push("--prefix=true".into());
        // On a large scale-out the stream from many pods could
        // overwhelm the terminal; --max-log-requests=N caps
        // kubectl's parallel streaming in selector mode. 10 is
        // kubectl's documented default ceiling; we pass it
        // explicitly for predictability.
        args.push("--max-log-requests=10".into());
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
    Selector(String),
}

/// Resolve the `kubectl logs` target. Without `--pod` — a label
/// selector via the AppRafter operator's `app.kubernetes.io/name:
/// <inner workload name>` label (the same label `app status`
/// selects workload pods by). `app_name` is the already-resolved
/// INNER name, not the Argo CD parent.
pub(crate) fn build_kubectl_logs_target(app_name: &str, pod: Option<&str>) -> KubectlLogsTarget {
    match pod {
        Some(name) => KubectlLogsTarget::Pod(name.to_string()),
        None => KubectlLogsTarget::Selector(format!("app.kubernetes.io/name={app_name}")),
    }
}

/// Pick the "previous" revision from `status.history`. History
/// is ordered chronologically (oldest first, newest last) with
/// monotonically increasing `id`. The previous entry is the
/// second-to-last; roll back to it.
///
/// Returns a string with a lifetime tied to `app` through the
/// `Value` borrow.
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
    let project = app
        .pointer("/spec/project")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let repo = app
        .pointer("/spec/source/repoURL")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let revision = app
        .pointer("/spec/source/targetRevision")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let sync = app
        .pointer("/status/sync/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    let health = app
        .pointer("/status/health/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    // ADR 0044 (1.83j): which environment this deployment targets. Reuses
    // the same label → status → `(base)` resolution as the `app status`
    // per-env aggregation so the two surfaces never disagree.
    let env = deployment_environment(app);
    AppRow {
        name,
        env,
        project,
        repo,
        revision,
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

    vec![
        format!("Application {ARGOCD_NAMESPACE}/{name}"),
        format!("  project:       {project}"),
        format!("  repo:          {repo}"),
        format!("  revision:      {revision}"),
        format!("  path:          {path}"),
        format!("  destination:   {dest_ns}"),
        format!("  environment:   {environment}"),
        format!("  sync state:    {sync}"),
        format!("  health:        {health}"),
    ]
}

fn print_status(app: &Value) {
    for line in status_detail_lines(app) {
        println!("{line}");
    }

    if let Some(revs) = app.pointer("/status/history").and_then(Value::as_array) {
        if !revs.is_empty() {
            println!();
            println!("Recent revisions (last {}):", revs.len().min(3));
            for rev in revs.iter().rev().take(3) {
                let id = rev.get("id").and_then(Value::as_u64).unwrap_or(0);
                let rev_str = rev.get("revision").and_then(Value::as_str).unwrap_or("?");
                let deployed_at = rev.get("deployedAt").and_then(Value::as_str).unwrap_or("?");
                println!("  #{id:>3} {rev_str:<10} {deployed_at}");
            }
        }
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
        let target = build_kubectl_logs_target("payments", None);
        assert_eq!(
            target,
            KubectlLogsTarget::Selector("app.kubernetes.io/name=payments".to_string())
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
            "status": { "resources": [
                { "group": "apprafter.io", "kind": "Application", "name": "landing-cms" }
            ] }
        });
        let inner = crate::commands::app_open::find_apprafter_app_name(&app)
            .unwrap_or_else(|| "cms".to_string());
        assert_eq!(inner, "landing-cms");
        let target = build_kubectl_logs_target(&inner, None);
        assert_eq!(
            target,
            KubectlLogsTarget::Selector("app.kubernetes.io/name=landing-cms".to_string())
        );
    }

    #[test]
    fn build_kubectl_logs_target_uses_pod_name_when_provided() {
        let target = build_kubectl_logs_target("payments", Some("payments-7f9c-xyz"));
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
            &KubectlLogsTarget::Selector("app.kubernetes.io/name=payments".to_string()),
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
