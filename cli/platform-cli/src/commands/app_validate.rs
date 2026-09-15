// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter app validate [manifest]` — local pre-commit
//! validation of an AppRafter `Application.cue` manifest
//! (subphase 2.12g, ADR 0046 Decisions #6 + #7).
//!
//! Why a dedicated command (and not a plain `cue vet`): the
//! manifest's `env` block uses BARE `claim.<type>.<field>`
//! selectors (and the named `claim.<type>.<name>.<field>`
//! form). `claim` is a CUE lexical reference that resolves
//! against a top-level `claim` binding the user does NOT
//! vendor — it is generated at RENDER time by the cue-cmp
//! (`argocd-cue-cmp/entrypoint.sh`) into Argo CD's ephemeral
//! checkout, never committed. A raw `cue vet` on the user's
//! repo therefore fails "reference claim not found".
//!
//! `apprafter app validate` reproduces EXACTLY the cue-cmp's
//! render-time pipeline in a temp dir so local validation
//! matches the cluster verdict byte-for-byte:
//!
//!   1. lay the CURRENT schema this CLI ships with into
//!      `cue.mod/pkg/apprafter.io/schemas/v1alpha1/`
//!      (the same `include_str!`'d sources scaffold used to
//!      vendor — inject-wins, no version drift),
//!   2. write a `cue.mod/module.cue` render-workspace module,
//!   3. copy the user's manifest in,
//!   4. TWO-PASS generate the `apprafter_claim_gen.cue`
//!      sibling (pass-1 permissive stub → extract the
//!      env-agnostic union of `needs` from base + every
//!      `environments[*]` → pass-2 emit the concrete `claim`
//!      binding running the comprehension IN the manifest's
//!      package against the cross-package `#ClaimFieldsFor`
//!      table),
//!   5. run `cue vet ./...` and map a non-zero exit to a
//!      list of error messages.
//!
//! This is the SAME mechanism as `entrypoint.sh` (T6 /
//! commit c0abc78) — the bash cue-cmp and this Rust CLI feed
//! `cue` identical inputs so the two NEVER disagree.

use std::path::{Path, PathBuf};
use std::process::Command;

use cli_core::{CliError, Result};

/// Every AppRafter `v1alpha1` schema file, embedded at compile
/// time. The validate path lays these into the temp workspace's
/// `cue.mod/pkg/` (inject-wins) so the manifest's
/// `import "apprafter.io/schemas/v1alpha1"` resolves against
/// the schema THIS CLI ships with (no vendored-copy drift —
/// ADR 0046 Decision #7).
///
/// The WHOLE package, not just `types` + `application`: the
/// import is of the package, so a manifest naming any other kind
/// in it (`#SharedVolume`, `#SourceCredential`, …) previously hit
/// "undefined field" from a *partial* injection — an error about
/// our workspace, not about the user's file. Every file here is
/// `package v1alpha1` with no imports of its own, so laying all
/// fourteen unifies exactly as the real `schemas/v1alpha1/`
/// directory does; measured cost is ~2 ms.
macro_rules! workspace_schemas {
    ($($file:literal),+ $(,)?) => {
        &[$(($file, include_str!(concat!("../../../../schemas/v1alpha1/", $file)))),+]
    };
}

/// Schema files written into the temp workspace's
/// `cue.mod/pkg/apprafter.io/schemas/v1alpha1/`, mirroring the
/// repository directory of the same name one-for-one.
///
/// `pub` because `docsgen`'s CUE-document check lays the SAME
/// bundle (through `docs_api`) to vet the manifests printed in the
/// documentation. Two embeddings of the same fourteen files would
/// be two things to keep in step; one is one.
pub const WORKSPACE_SCHEMAS: &[(&str, &str)] = workspace_schemas![
    "accessgrant.cue",
    "application.cue",
    "externalsurface.cue",
    "infrastructure.cue",
    "infrastructureproviderplugin.cue",
    "migrationplan.cue",
    "platformstack.cue",
    "resourceclaim.cue",
    "retainedclaim.cue",
    "serviceprovider.cue",
    "serviceproviderplugin.cue",
    "sharedvolume.cue",
    "sourcecredential.cue",
    "types.cue",
];

/// Minimal render-workspace module file. Mirrors the
/// `entrypoint.sh` `inject_schema_and_claim` module: the module
/// path is irrelevant to resolving the bundled `pkg/`, but cue
/// requires the file to exist. A cwd-local `cue.mod` shadows any
/// parent module so the import resolves unambiguously through
/// our injected bundle.
///
/// `language.version` is the load-bearing field, and `pub` for the
/// same reason `WORKSPACE_SCHEMAS` is: it pins which CUE language
/// semantics the evaluation runs under, so a workspace built by
/// `docsgen` reaches the same verdict as this one even when the two
/// run against different `cue` binaries.
pub const WORKSPACE_MODULE_CUE: &str = "module: \"apprafter.io/render-workspace\"

language: {
\tversion: \"v0.10.0\"
}
";

/// The generated claim-binding filename. NO leading underscore:
/// cue IGNORES files whose name begins with `_` (or `.`), so
/// `_apprafter_claim_gen.cue` would silently never load. Same
/// name the cue-cmp's `entrypoint.sh` writes (`CLAIM_GEN`).
const CLAIM_GEN_FILE: &str = "apprafter_claim_gen.cue";

/// What the injected filesystem probe reports about one path — the
/// whole of what the discovery rules need to know about it.
///
/// A plain `exists` predicate cannot express the rule below, because
/// "`<cwd>/apprafter` is there" and "`<cwd>/apprafter` is a directory"
/// are different questions and only the second one resolves a BUNDLE.
/// Answering the first for the second is how a package gets half-read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// Nothing at this path.
    Missing,
    /// Something that is not a directory.
    File,
    /// A directory — for `apprafter/`, the whole bundle.
    Dir,
}

/// The real filesystem probe, and the one [`run_validate`] passes.
///
/// Both `is_dir` and `exists` follow symlinks, which is what
/// `std::fs::read_dir` and `std::fs::copy` will do to the same path
/// further down [`lay_out_workspace`] — so a symlinked bundle directory
/// is classified the way it is later treated.
pub fn fs_path_kind(p: &Path) -> PathKind {
    if p.is_dir() {
        PathKind::Dir
    } else if p.exists() {
        PathKind::File
    } else {
        PathKind::Missing
    }
}

/// Resolve which manifest directory (or file) to validate, given the
/// optional CLI argument and the cwd. Pure — filesystem
/// presence is injected via `probe` so the discovery rules are
/// unit-testable without touching disk.
///
/// Rules (ADR 0046 Decision #6, corrected for ADR 0062's bundle):
///   1. explicit `arg` always wins — error if it doesn't exist.
///   2. else the bundle DIRECTORY `<cwd>/apprafter` when it is one
///      (the `apprafter app scaffold` convention).
///   3. else `cwd` itself when it holds any `*.cue` — cwd IS the
///      package directory, which is what `cd apprafter` produces.
///   4. else error, telling the user to pass a manifest path.
///
/// **Rules 2 and 3 resolve a DIRECTORY, and that is the whole point.**
/// A CUE package is a directory; a manifest package is a bundle (ADR
/// 0062); and the render layer this command is the local twin of
/// evaluates the package instance in a directory (`cue export .`,
/// `entrypoint.sh`). Resolving one FILE of it instead — which rule 2
/// did until 2.27b, by naming `apprafter/Application.cue` — makes
/// [`lay_out_workspace`] copy that file alone, so every sibling
/// disappears before [`check_bundle_consistency`] runs, all four
/// cross-workload checks see N=1, and none of them can fire. The local
/// twin then returns the OPPOSITE verdict from the layer it twins, on
/// the command's default invocation. This repository's own
/// `landing/web/apprafter/` is exactly such a bundle.
///
/// Neither rule 3 nor the error below tells the reader to "pass the
/// manifest explicitly", and that is deliberate: at N files, naming one
/// of them reproduces the half-read this function exists to stop.
///
/// A `<cwd>/apprafter` that is a directory wins even when it holds no
/// `*.cue` — [`lay_out_workspace`] then names it and says it is empty,
/// which is the accurate answer. Falling through to a stray `.cue` in
/// cwd would validate something the reader never pointed at.
pub fn resolve_manifest_path(
    arg: Option<&Path>,
    cwd: &Path,
    probe: &dyn Fn(&Path) -> PathKind,
) -> Result<PathBuf> {
    if let Some(arg) = arg {
        let p = if arg.is_absolute() {
            arg.to_path_buf()
        } else {
            cwd.join(arg)
        };
        if probe(&p) != PathKind::Missing {
            return Ok(p);
        }
        return Err(CliError::Other(format!(
            "manifest path '{}' does not exist.",
            arg.display()
        )));
    }

    let bundle_dir = cwd.join("apprafter");
    if probe(&bundle_dir) == PathKind::Dir {
        return Ok(bundle_dir);
    }

    if !list_cue_files(cwd).is_empty() {
        return Ok(cwd.to_path_buf());
    }

    Err(CliError::Other(format!(
        "no manifest found. Looked for the bundle directory '{}' and for `*.cue` files \
         in '{}'. Pass the directory holding the manifests: \
         `apprafter app validate <dir>`.",
        bundle_dir.display(),
        cwd.display()
    )))
}

/// Enumerate top-level `*.cue` files directly under `dir`
/// (non-recursive), sorted for determinism. Returns an empty
/// vec when the directory can't be read.
fn list_cue_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().map(|x| x == "cue").unwrap_or(false))
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// Validate the manifest at `manifest` (a file path or a
/// directory holding `*.cue`). Reproduces the cue-cmp's
/// render-time injection in a fresh temp dir, runs `cue vet`, and
/// then applies the four intra-bundle consistency checks the
/// render layer refuses on (ADR 0063 §Decision 5).
///
/// On success → `Ok(())`. On a non-zero `cue` exit → `Err` with
/// the captured stderr split into lines (each a diagnostic). If
/// `cue` is absent → `Err` carrying [`CliError::CueNotFound`]'s
/// message so the caller can surface the install hint.
pub fn validate_manifest(manifest: &Path) -> std::result::Result<(), Vec<String>> {
    validate_manifest_workloads(manifest).map(|_| ())
}

/// [`validate_manifest`] plus the bundle's workload roster.
///
/// Split out rather than folded in because `validate_manifest` is a
/// `docs_api` export with a `Result<(), Vec<String>>` contract that
/// `lib_surface_test.rs` pins, and because the roster is a *presentation*
/// need of `run_validate` alone — nothing else should have to skip past
/// it.
///
/// Returned in the order the SIDECAR reads, taken from the exported
/// JSON's own text ([`json_top_level_keys`]) — the same document, in the
/// same sequence, that `entrypoint.sh` hands `jq to_entries`. Printing a
/// different sequence from the Argo CD tile makes one finding read as
/// two different problems, which is precisely what this subphase exists
/// to remove.
///
/// `cue def` — which [`top_level_names`] reads, for the claim-binding
/// scopes — is NOT that order and cannot be substituted for it. Measured
/// on a two-FILE package: `cue export` emits the keys sorted while
/// `cue def` follows file order, and `Application-preview.cue` sorts
/// before `Application.cue`, so the two disagree on every bundle shaped
/// like this repository's own `landing/web/apprafter/`. Reading the text
/// sidesteps having to know either rule.
fn validate_manifest_workloads(
    manifest: &Path,
) -> std::result::Result<Vec<BundleWorkload>, Vec<String>> {
    let workdir =
        tempfile::tempdir().map_err(|e| vec![format!("could not create temp workspace: {e}")])?;
    // CUE's `./...` SKIPS any directory whose name begins with `.`
    // (the same rule that hides `_`/`.` files). `tempfile::tempdir()`
    // names its dir with a `.tmp…` prefix, so running cue from there
    // matches NO packages. Run inside a non-dot subdirectory instead.
    let root = workdir.path().join("workspace");
    std::fs::create_dir_all(&root).map_err(|e| vec![format!("create workspace dir: {e}")])?;
    let root = root.as_path();

    lay_out_workspace(root, manifest).map_err(|e| vec![e.to_string()])?;

    // The two-pass `claim` binding generation (mirrors
    // entrypoint.sh's inject_schema_and_claim). A failure to
    // detect the package / read needs is non-fatal here — we
    // still run `cue vet`, which surfaces the real error.
    generate_claim_binding(root);

    // `cue vet` FIRST, and the ordering is load-bearing: a package that
    // does not compile has no workloads to compare, and its real
    // diagnostic is the compile error, not a derived complaint about a
    // table we could not build.
    run_cue_vet(root)?;

    // The whole package as ONE JSON document — the same shape
    // `entrypoint.sh:644` hands its `jq` programs, and the only point in
    // either pipeline where every workload of a bundle is visible at
    // once. No extra `cue` concept: `cue export .` evaluates exactly the
    // package instance in cwd, which is what the sidecar renders.
    //
    // The key ORDER comes off the same call, read from the raw text
    // before `serde_json` sorts it into a `BTreeMap` — see
    // `json_top_level_keys`.
    let (doc, order) = cue_export_package(root)?;

    if let Some(refusal) = check_bundle_consistency(&doc, &order) {
        return Err(vec![refusal]);
    }
    Ok(bundle_rows(&doc, &order))
}

/// Parse the manifest's Application doc with the CURRENT shipped schema
/// injected — the `cue export` counterpart of [`validate_manifest`].
///
/// A post-2.12 manifest does NOT vendor the schema (`apprafter app
/// scaffold` stopped vendoring — ADR 0046 Decision #7), so a bare
/// `cue export` of the manifest directory fails with "imports are
/// unavailable because there is no cue.mod/module.cue file". Laying the
/// embedded schema into a temp workspace's `cue.mod/pkg/` lets the
/// manifest's `import "apprafter.io/schemas/v1alpha1"` resolve without a
/// vendored copy (mirrors the cue-cmp sidecar + `validate_manifest`).
///
/// Used by the `app add` wizard's environment picker and `--env`
/// validation (via `get_manifest_environments`): both need the
/// manifest's declared `spec.environments`, which a bare parse could no
/// longer read. `manifest` is the `apprafter/` directory (or a single
/// `.cue` file).
pub(crate) fn parse_application_injected(
    manifest: &Path,
) -> Result<cli_core::manifest::ApplicationManifest> {
    let workdir = tempfile::tempdir()
        .map_err(|e| CliError::Other(format!("could not create temp workspace: {e}")))?;
    let root = workdir.path().join("workspace");
    std::fs::create_dir_all(&root)
        .map_err(|e| CliError::Other(format!("create workspace dir: {e}")))?;
    let root = root.as_path();
    lay_out_workspace(root, manifest)?;
    // Mirror validate's two-pass claim binding so a manifest using bare
    // `claim.<type>.<field>` env refs (2.12) still exports.
    generate_claim_binding(root);
    cli_core::manifest::parse_application(root, std::path::Path::new("."))
}

/// Copy the schema bundle, module file, and the user manifest
/// into the temp workspace. The manifest may be a single `.cue`
/// FILE (copied verbatim, keeping its filename) or a DIRECTORY
/// (all its top-level `.cue` files copied in).
fn lay_out_workspace(root: &Path, manifest: &Path) -> Result<()> {
    // 1. cue.mod/module.cue + injected schema bundle.
    let mod_dir = root.join("cue.mod");
    let pkg_dir = mod_dir
        .join("pkg")
        .join("apprafter.io")
        .join("schemas")
        .join("v1alpha1");
    std::fs::create_dir_all(&pkg_dir)
        .map_err(|e| CliError::Other(format!("create {}: {e}", pkg_dir.display())))?;
    std::fs::write(mod_dir.join("module.cue"), WORKSPACE_MODULE_CUE)
        .map_err(|e| CliError::Other(format!("write module.cue: {e}")))?;
    for (name, content) in WORKSPACE_SCHEMAS {
        std::fs::write(pkg_dir.join(name), content)
            .map_err(|e| CliError::Other(format!("write schema {name}: {e}")))?;
    }

    // 2. The user manifest(s).
    if manifest.is_dir() {
        let mut copied = 0usize;
        for entry in std::fs::read_dir(manifest)
            .map_err(|e| CliError::Other(format!("read {}: {e}", manifest.display())))?
        {
            let entry = entry.map_err(CliError::from)?;
            let path = entry.path();
            if path.is_file() && path.extension().map(|x| x == "cue").unwrap_or(false) {
                let name = path.file_name().expect("file has a name");
                std::fs::copy(&path, root.join(name))
                    .map_err(|e| CliError::Other(format!("copy {}: {e}", path.display())))?;
                copied += 1;
            }
        }
        if copied == 0 {
            return Err(CliError::Other(format!(
                "no `*.cue` files in '{}' to validate.",
                manifest.display()
            )));
        }
    } else {
        let name = manifest.file_name().ok_or_else(|| {
            CliError::Other(format!("invalid manifest path '{}'", manifest.display()))
        })?;
        std::fs::copy(manifest, root.join(name))
            .map_err(|e| CliError::Other(format!("copy {}: {e}", manifest.display())))?;
    }

    Ok(())
}

/// Two-pass `claim` binding generation, faithfully mirroring
/// `entrypoint.sh`'s `inject_schema_and_claim`:
///
///   * PASS 1 — write a permissive recursive stub under which
///     any `claim.*` selector resolves, so the manifest
///     evaluates and its `needs` can be read.
///   * collect the per-type `{unnamed, names[]}` union across
///     every manifest's `base.needs` AND every
///     `environments[*].needs` (env-agnostic on purpose: a
///     `cue vet ./...` evaluates every env block, so a claim
///     ref in a non-active env must resolve too).
///   * PASS 2 — overwrite the stub with the concrete `claim`
///     binding whose comprehension runs in the manifest's
///     package, referencing the cross-package `#ClaimFieldsFor`
///     table.
///
/// Best-effort: any cue/jq failure leaves the pass-1 stub (or
/// no binding) in place; `cue vet` then surfaces the real
/// error. Never panics.
fn generate_claim_binding(root: &Path) {
    let Some(pkg) = detect_package(root) else {
        // No detectable package — a stub with an unresolved
        // package name would itself break the vet, so bail
        // (matches entrypoint.sh's `pkg=$(detect_package) ||
        // return 0`). cue vet will report the real issue.
        return;
    };

    // PASS 1 — permissive stub.
    let stub = format!(
        "package {pkg}\n\
         // 2.12g pass-1 extraction stub (overwritten in pass 2).\n\
         _N: {{claim: string}} & {{[!=\"claim\"]: _N}}\n\
         claim: _N\n"
    );
    if std::fs::write(root.join(CLAIM_GEN_FILE), &stub).is_err() {
        return;
    }

    // Build the list of spec SCOPES (bare `spec` for Style A
    // unwrapped manifests, `<name>.spec` for Style B named
    // wrappers). Mirrors entrypoint.sh step 3.
    let mut scopes: Vec<String> = vec!["spec".to_string()];
    for name in top_level_names(root) {
        scopes.push(format!("{name}.spec"));
    }

    // Collect the per-type union across base.needs + every
    // environments[*].needs of every scope (entrypoint.sh
    // step 4).
    let mut state = serde_json::json!({});
    for scope in &scopes {
        let base = cue_export_json(root, &format!("{scope}.base.needs"))
            .unwrap_or_else(|| serde_json::json!({}));
        union_needs(&mut state, &base);

        // Env keys — list lazily via a comprehension (does not
        // force the env values concrete, whose `claim.*` refs
        // are still the pass-1 stub).
        if let Some(keys) =
            cue_export_json(root, &format!("[for k, _ in {scope}.environments {{k}}]"))
        {
            if let Some(arr) = keys.as_array() {
                for k in arr {
                    if let Some(envkey) = k.as_str() {
                        let en =
                            cue_export_json(root, &format!("{scope}.environments.{envkey}.needs"))
                                .unwrap_or_else(|| serde_json::json!({}));
                        union_needs(&mut state, &en);
                    }
                }
            }
        }
    }

    // PASS 2 — emit the concrete binding. `state` is embedded
    // as `_apprafterClaimState` and the comprehension runs over
    // it, referencing the cross-package `#ClaimFieldsFor` table.
    let state_cue = serde_json::to_string(&state).unwrap_or_else(|_| "{}".to_string());
    let binding = format!(
        "package {pkg}\n\
         \n\
         import v1alpha1 \"apprafter.io/schemas/v1alpha1\"\n\
         \n\
         // 2.12g generated claim binding (ADR 0046) — runtime artifact.\n\
         _apprafterClaimState: {state_cue}\n\
         \n\
         claim: {{\n\
         \tfor type, st in _apprafterClaimState if (v1alpha1.#ClaimFieldsFor[type] != _|_) {{\n\
         \t\t(type): {{\n\
         \t\t\tif st.unnamed {{\n\
         \t\t\t\tfor f in v1alpha1.#ClaimFieldsFor[type] {{(f): {{claim: \"\\(type).\\(f)\"}}}}\n\
         \t\t\t}}\n\
         \t\t\tfor nm in st.names {{\n\
         \t\t\t\t(nm): {{for f in v1alpha1.#ClaimFieldsFor[type] {{(f): {{claim: \"\\(type).\\(nm).\\(f)\"}}}}}}\n\
         \t\t\t}}\n\
         \t\t}}\n\
         \t}}\n\
         }}\n"
    );
    let _ = std::fs::write(root.join(CLAIM_GEN_FILE), binding);
}

/// Detect the `package <name>` clause from the first user
/// `.cue` file in `root` (skipping our generated sibling and the
/// `_`/`.` files cue ignores). The generated sibling MUST match
/// it or cue treats them as different packages and the lexical
/// `claim` binding is invisible.
fn detect_package(root: &Path) -> Option<String> {
    let mut files = list_cue_files(root);
    files.sort();
    for path in files {
        let name = path.file_name()?.to_string_lossy().into_owned();
        if name == CLAIM_GEN_FILE || name.starts_with('_') || name.starts_with('.') {
            continue;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        for line in content.lines() {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix("package ") {
                let pkg: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !pkg.is_empty() {
                    return Some(pkg);
                }
            }
        }
    }
    None
}

/// List top-level field names via `cue def ./...`, stripping our
/// own helpers and the literal scalars apiVersion/kind/etc.
/// Mirrors entrypoint.sh's `cue def | sed | grep -v`.
fn top_level_names(root: &Path) -> Vec<String> {
    let out = match Command::new(cue_bin())
        .current_dir(root)
        .args(["def", "./..."])
        .output()
    {
        Ok(out) if out.status.success() => out.stdout,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out);
    let skip = ["_N", "claim", "apiVersion", "kind", "metadata", "spec"];
    let mut names = Vec::new();
    for line in text.lines() {
        // Top-level field declarations look like `name: …` with
        // no leading whitespace.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((ident, _)) = line.split_once(':') else {
            continue;
        };
        let ident = ident.trim();
        if ident.is_empty()
            || skip.contains(&ident)
            || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || ident.starts_with(|c: char| c.is_ascii_digit())
        {
            continue;
        }
        if !names.contains(&ident.to_string()) {
            names.push(ident.to_string());
        }
    }
    names
}

/// Run `cue export ./... -e <expr> --out json` in `root`,
/// returning the parsed JSON value or `None` on any failure
/// (the caller treats `None` as `{}`, matching entrypoint.sh's
/// `|| echo '{}'`).
fn cue_export_json(root: &Path, expr: &str) -> Option<serde_json::Value> {
    let out = Command::new(cue_bin())
        .current_dir(root)
        .args(["export", "./...", "-e", expr, "--out", "json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Fold one `needs` JSON object into the running `{type:
/// {unnamed, names[]}}` state. Pure — operates on serde_json.
/// Mirrors entrypoint.sh's `norm1` + `union` jq programs.
fn union_needs(state: &mut serde_json::Value, incoming: &serde_json::Value) {
    let Some(obj) = incoming.as_object() else {
        return;
    };
    let map = state.as_object_mut().expect("state is an object");
    for (ty, val) in obj {
        let (mut unnamed, mut names): (bool, Vec<String>) = match map.get(ty) {
            Some(existing) => (
                existing
                    .get("unnamed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                existing
                    .get("names")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|n| n.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            None => (false, Vec::new()),
        };

        if let Some(arr) = val.as_array() {
            // Array of named/unnamed entries.
            for entry in arr {
                match entry.get("name").and_then(|n| n.as_str()) {
                    Some(nm) => {
                        if !names.contains(&nm.to_string()) {
                            names.push(nm.to_string());
                        }
                    }
                    None => unnamed = true,
                }
            }
        } else {
            // Scalar struct → the unnamed default claim.
            unnamed = true;
        }

        names.sort();
        names.dedup();
        map.insert(
            ty.clone(),
            serde_json::json!({ "unnamed": unnamed, "names": names }),
        );
    }
}

/// Run `cue vet -c ./...` in `root`, mapping a non-zero exit to
/// the captured stderr (split into non-empty lines). A missing
/// `cue` binary surfaces the install hint.
///
/// `-c` (concrete) is load-bearing: without it `cue vet` reports
/// only the unhelpful "some instances are incomplete; use the -c
/// flag…" on an unresolved `claim.*` selector. `-c` surfaces the
/// precise diagnostic the user needs — e.g.
/// `app.spec.base.env.REDIS_URL: undefined field: redis:
/// ./Application.cue:5:67` — which is exactly the render-time
/// error the cue-cmp would hit.
fn run_cue_vet(root: &Path) -> std::result::Result<(), Vec<String>> {
    let out = match Command::new(cue_bin())
        .current_dir(root)
        .args(["vet", "-c", "./..."])
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(vec![CliError::CueNotFound.to_string()]);
        }
        Err(e) => return Err(vec![format!("running cue: {e}")]),
    };
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let msgs: Vec<String> = stderr
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    Err(if msgs.is_empty() {
        vec![format!(
            "cue vet failed (exit {})",
            out.status.code().unwrap_or(-1)
        )]
    } else {
        msgs
    })
}

/// `cue` binary path — honours `CUE_BIN` (as cli-core's
/// `cue::export` does) for custom installs / tests.
fn cue_bin() -> String {
    std::env::var("CUE_BIN").unwrap_or_else(|_| "cue".to_string())
}

// ─────────────────────────────────────────────────────────────
// Intra-bundle consistency — the LOCAL TWIN of the render layer
// (ADR 0063 §Decision 5, subphase 2.27b)
// ─────────────────────────────────────────────────────────────
//
// ADR 0063 §Decision 5's enforcement table puts four inconsistencies on
// one row each: "cue-cmp `exit 1` + `validate`". 2.27a shipped the
// cue-cmp half (`argocd-cue-cmp/entrypoint.sh`, the block above the
// Style-A/Style-B dispatch and the `bundle_refuse` helper); this is the
// `validate` half, and it is a MIRROR rather than a reimplementation.
//
// Mirror down to the wording, deliberately. An operator meets these
// findings twice — once here, on a laptop, before committing, and once
// on an Argo CD Application tile if they commit anyway — and the second
// encounter has to read as the SAME finding. A paraphrase reads as a
// second, unrelated problem, and the reader then has two mysteries
// instead of one. So the summary lines, the detail prose, and even the
// column widths below are the entrypoint's, verbatim. The one
// deliberate difference is the closing paragraph: the sidecar says
// nothing was applied (true of a sync), this says the sidecar would
// refuse it too (true of a laptop).
//
// The promise this restores, stated plainly: before 2.27b a manifest
// the sidecar refuses validated clean locally, and the operator learned
// about it from a red tile. `validate` answering `✓ valid` for exactly
// what the cluster refuses inverts the point of having a local
// validator at all — which is why `bundle_refuse`'s own comment
// refuses to name this command until it is true.
//
// Every check here is pure over the exported JSON plus a declaration
// order: no second `cue` invocation, no cluster access. That is the
// same property ADR 0063 §Decision 5 claims for the render layer.

/// The key the checks use for an unwrapped (Style A) package, whose
/// manifest IS the package scope and so has no top-level name of its
/// own. Byte-identical to the entrypoint's, because it is printed.
const PACKAGE_SCOPE_KEY: &str = "(package scope)";

/// One row of the intra-bundle table — the CLI's mirror of
/// `entrypoint.sh`'s tab-separated `$bundle_rows`.
///
/// `environments` has no counterpart there: the sidecar never prints a
/// roster, so it never needs `spec.environments`. It rides along here
/// because the same filtered row set is exactly what the success roster
/// should list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BundleWorkload {
    /// Top-level CUE key, or [`PACKAGE_SCOPE_KEY`] when unwrapped.
    key: String,
    /// `metadata.namespace` — empty string when not declared.
    namespace: String,
    /// `metadata.name` — empty string when not declared.
    name: String,
    /// `spec.environment` — empty string when not declared.
    environment: String,
    /// `spec.environments` keys, sorted; empty when none are declared.
    environments: Vec<String>,
}

/// Top-level keys of a JSON object, in the order its TEXT declares
/// them.
///
/// The sidecar's `jq to_entries` walks the exported document in exactly
/// this sequence, so the text is what the two layers have to agree on.
/// Nothing downstream of `serde_json` can supply it: objects parse into
/// a `BTreeMap` unless the `preserve_order` feature is enabled, and it
/// deliberately is not — [ADR 0062](../../../../docs/adr/0062-manifest-package-is-a-bundle.md)
/// records the alphabetical "first wins" defects that property caused,
/// and turning it on globally would also reorder every map this CLI
/// serialises, `.apprafter/state.json` included.
///
/// Reading the text also means not having to model `cue`'s own rule,
/// which is not one rule: measured on cue v0.16.0, a single-FILE package
/// exports its keys in declaration order while a multi-FILE package
/// exports them sorted, and `cue def` follows file order in both cases.
///
/// A key is a string token at depth 1 immediately followed by `:`.
/// Non-object roots, and any input malformed enough to run off the end,
/// yield an empty vec — which [`ordered_keys`] treats as "sequence
/// nothing", falling back to the parsed object's own key set rather than
/// dropping a workload.
fn json_top_level_keys(raw: &str) -> Vec<String> {
    let b = raw.as_bytes();
    let mut keys: Vec<String> = Vec::new();
    let mut depth: i32 = 0;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'"' => {
                // Scan to the closing quote. A backslash escapes the
                // next byte, and every byte JSON allows after one is
                // ASCII, so stepping two never lands mid-character.
                let start = i;
                let mut j = i + 1;
                while j < b.len() {
                    match b[j] {
                        b'\\' => j += 2,
                        b'"' => break,
                        _ => j += 1,
                    }
                }
                if j >= b.len() {
                    break; // unterminated string — give up, do not guess
                }
                let token = &raw[start..=j];
                i = j + 1;
                // A KEY is a string followed by `:`; a string VALUE is
                // not. Both occur at depth 1 (Style A's `apiVersion` is
                // a top-level value).
                let mut k = i;
                while k < b.len() && b[k].is_ascii_whitespace() {
                    k += 1;
                }
                if depth == 1 && k < b.len() && b[k] == b':' {
                    if let Ok(name) = serde_json::from_str::<String>(token) {
                        if !keys.contains(&name) {
                            keys.push(name);
                        }
                    }
                }
            }
            _ => i += 1,
        }
    }
    keys
}

/// Export the WHOLE package as one JSON document, plus its top-level
/// keys in the document's own textual order.
///
/// `.`, not `./...`, mirroring `entrypoint.sh:644` — `./...` matches
/// every package instance BELOW cwd as well, which would emit two
/// concatenated JSON documents for a package holding a helper
/// sub-package and leave this parse failing on trailing input. The temp
/// workspace is flat today (`lay_out_workspace` copies only top-level
/// `.cue` files), so the two are equivalent here; `.` is the request
/// that stays correct if that ever changes.
///
/// The order rides along with the value rather than being recovered by a
/// second call, because it is a property of THIS invocation's output —
/// re-deriving it from a separate `cue` run would be a second opinion
/// about a document nobody re-exported.
///
/// A failure is an ERROR, never a silent skip. `cue vet -c ./...` has
/// already passed by the time this runs, so an export that then fails is
/// a genuine anomaly — and swallowing it would turn the four checks into
/// a guard that quietly stops guarding, which is the failure mode the
/// whole subphase is about.
fn cue_export_package(
    root: &Path,
) -> std::result::Result<(serde_json::Value, Vec<String>), Vec<String>> {
    let out = match Command::new(cue_bin())
        .current_dir(root)
        .args(["export", ".", "--out", "json"])
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(vec![CliError::CueNotFound.to_string()]);
        }
        Err(e) => return Err(vec![format!("running cue export: {e}")]),
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut msgs: Vec<String> = stderr
            .lines()
            .map(|l| l.trim_end())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        if msgs.is_empty() {
            msgs.push(format!(
                "cue export failed (exit {})",
                out.status.code().unwrap_or(-1)
            ));
        }
        return Err(msgs);
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| vec![format!("could not read the exported package as JSON: {e}")])?;
    let order = json_top_level_keys(&raw);
    Ok((doc, order))
}

/// jq's `has("apiVersion") and has("kind")` on an object — the shape
/// probe both layers dispatch on. `has` is true for a declared-but-null
/// field, and `.get(..).is_some()` matches that.
fn is_k8s_shaped(v: &serde_json::Value) -> bool {
    v.is_object() && v.get("apiVersion").is_some() && v.get("kind").is_some()
}

/// jq's `tostring`: a string passes through unquoted, anything else
/// becomes its JSON text.
fn jq_tostring(v: &serde_json::Value) -> String {
    match v.as_str() {
        Some(s) => s.to_string(),
        None => v.to_string(),
    }
}

/// Follow `path` and render the leaf as jq's `… // ""` would: absent,
/// `null` and `false` all read as the empty string.
fn field_or_empty(v: &serde_json::Value, path: &[&str]) -> String {
    let mut cur = v;
    for seg in path {
        match cur.get(seg) {
            Some(next) => cur = next,
            None => return String::new(),
        }
    }
    match cur {
        serde_json::Value::Null | serde_json::Value::Bool(false) => String::new(),
        other => jq_tostring(other),
    }
}

/// Top-level keys of `doc` in the order the sidecar reads them.
///
/// `order` comes from [`json_top_level_keys`] — the exported document's
/// own text, which is the sequence `entrypoint.sh`'s `jq to_entries`
/// walks. The key SET is taken from the parsed JSON, which is
/// authoritative; `order` only sequences it. Anything the parse carries
/// that the scan did not name — including the case where the scan
/// returns nothing at all — still appears, appended in the parsed
/// object's own (sorted) order. A check that silently sees zero
/// workloads because a helper returned an empty vec is a guard that
/// stopped guarding.
fn ordered_keys(doc: &serde_json::Value, order: &[String]) -> Vec<String> {
    let Some(obj) = doc.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for k in order {
        if obj.contains_key(k) && !out.contains(k) {
            out.push(k.clone());
        }
    }
    for k in obj.keys() {
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
}

/// Build one row, or `None` when the value is not a workload of THIS
/// bundle.
///
/// The filter is `apiVersion` AND `kind`, never `kind` alone — the
/// entrypoint's row-table comment argues this at length and
/// `testdata/bundle-foreign-kind/` pins it. Argo CD's own CRD is
/// `argoproj.io/v1alpha1, kind: Application`, the one foreign apiVersion
/// that collides exactly with ours, and a package may legitimately ship
/// one beside the workload it registers. Keyed on `kind` alone, such a
/// package reads as two namespaces (`argocd` is where Argo CD's
/// Applications must live) and two environments, and is refused though
/// nothing about it is inconsistent.
fn workload_row(key: &str, v: &serde_json::Value) -> Option<BundleWorkload> {
    if v.get("kind").and_then(|k| k.as_str()) != Some("Application") {
        return None;
    }
    if !v
        .get("apiVersion")
        .map(jq_tostring)
        .unwrap_or_default()
        .starts_with("apprafter.io/")
    {
        return None;
    }
    let mut environments: Vec<String> = v
        .get("spec")
        .and_then(|s| s.get("environments"))
        .and_then(|e| e.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    environments.sort();
    Some(BundleWorkload {
        key: key.to_string(),
        namespace: field_or_empty(v, &["metadata", "namespace"]),
        name: field_or_empty(v, &["metadata", "name"]),
        environment: field_or_empty(v, &["spec", "environment"]),
        environments,
    })
}

/// The bundle's workloads, in declaration order.
///
/// An unwrapped (Style A) package yields exactly ONE row by
/// construction, which is what keeps every single-manifest layout —
/// i.e. every manifest written before bundles existed — behaving
/// identically: all three cross-workload checks are no-ops on one row.
fn bundle_rows(doc: &serde_json::Value, order: &[String]) -> Vec<BundleWorkload> {
    if is_k8s_shaped(doc) {
        return workload_row(PACKAGE_SCOPE_KEY, doc).into_iter().collect();
    }
    ordered_keys(doc, order)
        .iter()
        .filter_map(|k| {
            let v = doc.get(k)?;
            if !is_k8s_shaped(v) {
                return None;
            }
            workload_row(k, v)
        })
        .collect()
}

/// The named children of a package-scope manifest — check (1)'s subject.
///
/// Deliberately withOUT the `apprafter.io/` predicate `workload_row`
/// carries, and the asymmetry is the point: a foreign apiVersion+kind
/// CHILD really would ride out as a stray top-level key and be pruned in
/// silence, whoever's API group it belongs to. There the question is
/// "will this document survive the render", which is group-agnostic;
/// in the row table it is "is this a workload of this bundle", which is
/// not.
fn mixed_style_wrappers(doc: &serde_json::Value, order: &[String]) -> Vec<String> {
    if !is_k8s_shaped(doc) {
        return Vec::new();
    }
    ordered_keys(doc, order)
        .into_iter()
        .filter(|k| doc.get(k).map(is_k8s_shaped).unwrap_or(false))
        .collect()
}

/// One shape for every intra-bundle refusal — the CLI's `bundle_refuse`.
///
/// The FIRST line carries the whole finding (which workloads, which
/// values they disagree on), exactly as the sidecar's does, because on
/// the Argo CD side that line is the tile and is usually all anyone
/// reads. Keeping it identical here is what lets a reader recognise the
/// tile they see later.
fn bundle_refusal(summary: &str, detail: &str) -> String {
    format!(
        "bundle is inconsistent: {summary}\n\
         \n\
         --- apprafter bundle check ---\n\
         {detail}\n\
         \n\
         The cue-cmp render sidecar runs this same check at sync time, so a\n\
         commit of this bundle would be refused there too — with nothing\n\
         applied and the resources already running left untouched."
    )
}

/// The four checks, in the entrypoint's order, first hit wins.
///
/// Order is load-bearing for (1): the Style-A/Style-B dispatch DISCARDS
/// a mixed package's named wrappers, so the row table below it cannot
/// see past a package-scope manifest — for a mixed package the table
/// takes the package-scope branch and the wrappers are invisible to it.
fn check_bundle_consistency(doc: &serde_json::Value, order: &[String]) -> Option<String> {
    // (1) Style A mixed with Style B. The one inconsistency the render
    // layer is the ONLY possible place to catch: the dispatch takes the
    // Style-A branch, the named wrapper rides out as a stray top-level
    // key of the emitted document, and the apiserver PRUNES an unknown
    // top-level key without an error. The discarded manifest never
    // becomes an API object at all, so there is nothing downstream left
    // to inspect it.
    let mixed = mixed_style_wrappers(doc, order);
    if !mixed.is_empty() {
        let mixed = mixed.join(", ");
        return Some(bundle_refusal(
            &format!(
                "package-scope manifest mixed with named wrapper(s) {mixed} \
                 — only one layout renders, the rest are silently dropped"
            ),
            &format!(
                "This package declares a manifest at PACKAGE SCOPE (bare apiVersion /
kind / metadata / spec) and ALSO these named wrappers:

  {mixed}

Only one of the two layouts is rendered. The package-scope manifest
wins, and each named wrapper rides out as an extra top-level key inside
it — which the apiserver removes without reporting anything. The
wrapped workload would simply never appear, and nothing would say why.

Pick one layout for the whole package: either move the package-scope
apiVersion/kind/metadata/spec into a named wrapper of its own, or fold
the wrapped manifests into the package scope (only one manifest fits
there, so several wrappers means the first option)."
            ),
        ));
    }

    let rows = bundle_rows(doc, order);

    // (2) Divergent `metadata.namespace`.
    //
    // Divergent means TWO OR MORE DISTINCT NON-EMPTY values. An absent
    // namespace is deliberately NOT counted as a third value: Argo CD
    // fills it in from the registration's `destination.namespace`, which
    // neither layer can see, so an absent one may well resolve to the
    // same namespace its sibling declares and refusing would be a guess.
    // Check (4) reasons the opposite way, for the reason stated there —
    // the two are not inconsistent, they differ because one field has a
    // registration-level default and the other does not.
    let namespaces = distinct(
        rows.iter()
            .filter(|r| !r.namespace.is_empty())
            .map(|r| r.namespace.clone()),
    );
    if namespaces.len() > 1 {
        let pairs = join_pairs(
            rows.iter()
                .filter(|r| !r.namespace.is_empty())
                .map(|r| format!("{} -> \"{}\"", r.key, r.namespace)),
            ", ",
        );
        let lines = rows
            .iter()
            .map(|r| {
                let v = if r.namespace.is_empty() {
                    "(not declared)"
                } else {
                    &r.namespace
                };
                format!("  {:<24} metadata.namespace: {v}", r.key)
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Some(bundle_refusal(
            &format!(
                "workloads declare {} different namespaces — {pairs}",
                namespaces.len()
            ),
            &format!(
                "{lines}

One manifest package is one bundle: one registration, one namespace.
Argo CD applies every document this render emits into the single
destination namespace of the registration, so a second namespace
declared here cannot be honoured — the workload would land somewhere
nobody registered, or not at all.

Give every workload in this package the same metadata.namespace. If they
genuinely belong to different namespaces they are different bundles: put
them in separate directories and register each with its own
`apprafter app add --path`."
            ),
        ));
    }

    // (3) Duplicate identity — `(namespace, name)` TOGETHER, never
    // either alone. Two workloads may share a name in different
    // namespaces, and must share a namespace to be a bundle at all; it
    // is the pair that names one object. An absent namespace
    // participates in the key as its own value: two workloads that both
    // omit it and share a name resolve to one object under whatever
    // destination namespace the registration carries, the same
    // collision.
    let mut ids: Vec<(String, Vec<String>)> = Vec::new();
    for r in &rows {
        let ns = if r.namespace.is_empty() {
            "(no namespace)"
        } else {
            &r.namespace
        };
        let id = format!("{ns}/{}", r.name);
        match ids.iter_mut().find(|(k, _)| *k == id) {
            Some((_, who)) => who.push(r.key.clone()),
            None => ids.push((id, vec![r.key.clone()])),
        }
    }
    let dups: Vec<&(String, Vec<String>)> = ids.iter().filter(|(_, who)| who.len() > 1).collect();
    if !dups.is_empty() {
        let lines = dups
            .iter()
            .map(|(id, who)| format!("  {:<28} declared by: {}", id, who.join(", ")))
            .collect::<Vec<_>>()
            .join("\n");
        // The entrypoint derives this by stripping the leading indent
        // off `$dup_lines` and collapsing its runs of spaces; building
        // it directly from the same parts is the same string.
        let pairs = join_pairs(
            dups.iter()
                .map(|(id, who)| format!("{id} declared by: {}", who.join(", "))),
            "; ",
        );
        return Some(bundle_refusal(
            &format!("two workloads share one (namespace, name) — {pairs}"),
            &format!(
                "{lines}

Two CUE values, two rendered documents — but ONE object. Kubernetes
identifies an Application by (namespace, name), so whichever document is
applied last overwrites the other in place. One of these workloads would
never run, and neither the render nor the sync would report that a
choice had been made.

Rename one of them (metadata.name), or move it to a namespace of its own
— in which case it is a separate bundle and wants its own directory and
its own `apprafter app add --path`."
            ),
        ));
    }

    // (4) Divergent `spec.environment`.
    //
    // Here an ABSENT value IS counted, as its own distinct value, and
    // that is a deliberate choice rather than an inconsistency with (2).
    // An absent `spec.environment` is not "unspecified pending a
    // default", it is the BASE-ONLY deploy — a different deployment
    // semantic from `environment: "dev"`, documented as such in
    // `schemas/v1alpha1/application.cue`. Unlike a namespace there is no
    // `destination.environment` on the registration to fill it in, so
    // "declared on one workload, absent on its sibling" is a real
    // divergence the reader can act on, not a guess. Every workload
    // absent is therefore ONE distinct value, which is the normal
    // single-environment bundle and stays silent.
    let env_label = |r: &BundleWorkload| -> String {
        if r.environment.is_empty() {
            "(not declared)".to_string()
        } else {
            r.environment.clone()
        }
    };
    let environments = distinct(rows.iter().map(env_label));
    if environments.len() > 1 {
        let pairs = join_pairs(
            rows.iter().map(|r| {
                if r.environment.is_empty() {
                    format!("{} -> (not declared)", r.key)
                } else {
                    format!("{} -> \"{}\"", r.key, r.environment)
                }
            }),
            ", ",
        );
        let lines = rows
            .iter()
            .map(|r| {
                let v = if r.environment.is_empty() {
                    "(not declared — base-only deploy)"
                } else {
                    &r.environment
                };
                format!("  {:<24} spec.environment: {v}", r.key)
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Some(bundle_refusal(
            &format!(
                "workloads declare {} different environments — {pairs}",
                environments.len()
            ),
            &format!(
                "{lines}

One manifest package is one bundle: one registration, one environment.
The environment belongs to the REGISTRATION (`apprafter app add --env`),
which stamps the same value onto every document of the package — so a
per-workload spec.environment that disagrees with its siblings either
splits one bundle across two environments, or is quietly overwritten and
never takes effect.

A workload with no spec.environment deploys its base only, which is its
own environment as far as this check is concerned — so \"declared on one,
absent on the other\" counts too.

Either give every workload in this package the same spec.environment (or
drop it from all of them and select the environment at registration
time), or split them into separate directories and register each with
its own `--env`."
            ),
        ));
    }

    None
}

/// Distinct values in first-seen order — `sort -u | grep -c .` without
/// the sort, since only the COUNT is ever read from it.
fn distinct<I: IntoIterator<Item = String>>(values: I) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// Join an iterator of already-rendered pairs with `sep`.
fn join_pairs<I: IntoIterator<Item = String>>(items: I, sep: &str) -> String {
    items.into_iter().collect::<Vec<_>>().join(sep)
}

/// The success output: `✓ valid` plus the bundle's roster.
///
/// A bare `✓ valid` cannot answer "how many workloads is this?", which
/// is the first question a bundle raises and the one that makes "my
/// workload never appeared" answerable — a wrapper whose `apiVersion` is
/// mistyped is not a workload, it is an inert struct, and the only
/// visible symptom is a roster one line shorter than expected.
///
/// The roster DOES appear at N=1, and that is the decision: one workload
/// prints two lines, which is still terse, and the count is exactly as
/// load-bearing there — a two-workload package whose second workload
/// silently failed to parse as one reports `1 workload`, and printing
/// nothing in that case would hide the only evidence. N=0 stays the bare
/// `✓ valid`: a package with no workloads at all is supporting CUE, and
/// an empty bulleted list under a "0 workloads" header says less than
/// the line above it.
fn format_roster(workloads: &[BundleWorkload]) -> Vec<String> {
    if workloads.is_empty() {
        return vec!["✓ valid".to_string()];
    }
    let mut out = vec![format!(
        "✓ valid — {} workload{}",
        workloads.len(),
        if workloads.len() == 1 { "" } else { "s" }
    )];
    for w in workloads {
        let name = if w.name.is_empty() {
            "(no metadata.name)"
        } else {
            &w.name
        };
        let ns = if w.namespace.is_empty() {
            "(not declared)"
        } else {
            &w.namespace
        };
        let mut line = format!("  • {name}  namespace {ns}");
        if !w.environments.is_empty() {
            line.push_str(&format!("  envs: {}", w.environments.join(", ")));
        }
        out.push(line);
    }
    out
}

/// Entry point for `apprafter app validate [manifest]`. Resolves
/// the manifest path, runs the cue-cmp-equivalent validation,
/// and prints the roster or the diagnostics. Returns `Err` (so
/// the process exits non-zero) on validation failure.
pub fn run_validate(arg: Option<PathBuf>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let manifest = resolve_manifest_path(arg.as_deref(), &cwd, &fs_path_kind)?;

    println!("Validating {} …", manifest.display());
    match validate_manifest_workloads(&manifest) {
        Ok(workloads) => {
            for line in format_roster(&workloads) {
                println!("{line}");
            }
            Ok(())
        }
        Err(msgs) => {
            eprintln!("✗ validation failed:");
            // One element is ONE finding, and a bundle refusal is a
            // multi-line one — so indent per LINE rather than per
            // element, and leave blank lines bare rather than emitting
            // two trailing spaces. The count below then reports findings
            // (a refusal is 1), not the lines it happens to occupy.
            for m in &msgs {
                for line in m.lines() {
                    if line.is_empty() {
                        eprintln!();
                    } else {
                        eprintln!("  {line}");
                    }
                }
            }
            Err(CliError::Other(format!(
                "manifest {} failed validation ({} error{}).",
                manifest.display(),
                msgs.len(),
                if msgs.len() == 1 { "" } else { "s" }
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::tempdir;

    // ── resolve_manifest_path — pure discovery rules ──────────

    /// A probe that reports every listed path as a FILE and everything
    /// else as missing.
    fn files(paths: &[&str]) -> impl Fn(&Path) -> PathKind {
        let present: HashSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |p: &Path| {
            if present.contains(p) {
                PathKind::File
            } else {
                PathKind::Missing
            }
        }
    }

    /// A probe that reports every listed path as a DIRECTORY.
    fn dirs(paths: &[&str]) -> impl Fn(&Path) -> PathKind {
        let present: HashSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |p: &Path| {
            if present.contains(p) {
                PathKind::Dir
            } else {
                PathKind::Missing
            }
        }
    }

    #[test]
    fn resolve_explicit_arg_wins_when_present() {
        let cwd = Path::new("/work");
        let arg = Path::new("custom/My.cue");
        let got = resolve_manifest_path(Some(arg), cwd, &files(&["/work/custom/My.cue"])).unwrap();
        assert_eq!(got, PathBuf::from("/work/custom/My.cue"));
    }

    #[test]
    fn resolve_explicit_absolute_arg_used_verbatim() {
        let cwd = Path::new("/work");
        let arg = Path::new("/elsewhere/App.cue");
        let got = resolve_manifest_path(Some(arg), cwd, &files(&["/elsewhere/App.cue"])).unwrap();
        assert_eq!(got, PathBuf::from("/elsewhere/App.cue"));
    }

    #[test]
    fn resolve_explicit_directory_arg_is_the_bundle() {
        // `apprafter app validate .` and `… apprafter/` are the forms
        // that always read the WHOLE package; a directory argument must
        // survive rule 1 unchanged.
        let cwd = Path::new("/work");
        let got = resolve_manifest_path(
            Some(Path::new("apprafter")),
            cwd,
            &dirs(&["/work/apprafter"]),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/work/apprafter"));
    }

    #[test]
    fn resolve_explicit_missing_arg_errors() {
        let cwd = Path::new("/work");
        let arg = Path::new("nope.cue");
        let err = resolve_manifest_path(Some(arg), cwd, &|_| PathKind::Missing).unwrap_err();
        assert!(
            err.to_string().contains("does not exist"),
            "missing explicit arg must error with 'does not exist'; got: {err}"
        );
    }

    // THE BUG THIS FILE SHIPPED: rule 2 resolved the FILE
    // `apprafter/Application.cue`, so `lay_out_workspace` copied that
    // one file and every sibling of a multi-file bundle was dropped
    // before any check could see it. A bundle is a PACKAGE — the
    // directory is the answer.
    #[test]
    fn resolve_defaults_to_the_apprafter_bundle_directory() {
        let cwd = Path::new("/work");
        let got = resolve_manifest_path(None, cwd, &dirs(&["/work/apprafter"])).unwrap();
        assert_eq!(
            got,
            PathBuf::from("/work/apprafter"),
            "the default must be the bundle DIRECTORY; resolving one file of it makes every \
             cross-workload check see N=1 and silently pass"
        );
    }

    #[test]
    fn resolve_single_cue_in_cwd_when_no_bundle_directory() {
        // No apprafter/ directory, but cwd itself holds *.cue — cwd IS
        // the package.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("App.cue"), "package x\n").unwrap();
        let got = resolve_manifest_path(None, dir.path(), &fs_path_kind).unwrap();
        assert_eq!(got, dir.path());
    }

    #[test]
    fn resolve_errors_when_no_manifest_found() {
        // Empty dir, no bundle directory, no *.cue.
        let dir = tempdir().unwrap();
        let err = resolve_manifest_path(None, dir.path(), &fs_path_kind).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no manifest found") && msg.contains("apprafter app validate"),
            "empty cwd must error pointing at an explicit path; got: {msg}"
        );
    }

    // Running from INSIDE the bundle directory used to be the second
    // half of the same defect: N `*.cue` read as "ambiguous, pass the
    // manifest explicitly", and following that advice literally named
    // one file — i.e. the error's own remedy reproduced the
    // half-validation. cwd holding `*.cue` means cwd is the package.
    #[test]
    fn resolve_from_inside_the_bundle_directory_takes_the_whole_package() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Application.cue"), "package x\n").unwrap();
        fs::write(dir.path().join("Application-preview.cue"), "package x\n").unwrap();
        let got = resolve_manifest_path(None, dir.path(), &fs_path_kind).unwrap();
        assert_eq!(
            got,
            dir.path(),
            "two `*.cue` in cwd is a two-file package, not an ambiguity to push back to the user"
        );
    }

    #[test]
    fn resolve_bundle_directory_preferred_over_a_cwd_cue() {
        // Both an apprafter/ directory AND a cwd *.cue exist — the
        // convention directory must win.
        let cwd = Path::new("/work");
        let got = resolve_manifest_path(None, cwd, &dirs(&["/work/apprafter"])).unwrap();
        assert_eq!(got, PathBuf::from("/work/apprafter"));
    }

    // ── WORKSPACE_SCHEMAS — the injected bundle is the whole package ──

    #[test]
    fn injected_bundle_mirrors_the_schema_directory() {
        // The manifest imports the PACKAGE, so a partial bundle makes a
        // manifest naming a kind we forgot to list fail with "undefined
        // field" — an error about our workspace, not the user's file.
        // Adding a file to `schemas/v1alpha1/` must therefore add it here;
        // this is the machine gate that makes forgetting loud.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("cli/platform-cli is two levels below the repo root")
            .join("schemas/v1alpha1");
        let mut on_disk: Vec<String> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .map(|e| {
                e.expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|n| n.ends_with(".cue"))
            .collect();
        on_disk.sort();
        let mut injected: Vec<String> = WORKSPACE_SCHEMAS
            .iter()
            .map(|(n, _)| n.to_string())
            .collect();
        injected.sort();
        assert_eq!(
            injected, on_disk,
            "WORKSPACE_SCHEMAS must list every schemas/v1alpha1/*.cue file"
        );
    }

    // ── union_needs — pure needs union (no cue) ───────────────

    #[test]
    fn union_needs_scalar_marks_unnamed() {
        let mut state = serde_json::json!({});
        union_needs(&mut state, &serde_json::json!({ "pg": {} }));
        assert_eq!(state["pg"]["unnamed"], serde_json::json!(true));
        assert_eq!(state["pg"]["names"], serde_json::json!([]));
    }

    #[test]
    fn union_needs_array_collects_names_and_unnamed() {
        let mut state = serde_json::json!({});
        union_needs(
            &mut state,
            &serde_json::json!({ "pg": [{}, { "name": "main" }] }),
        );
        assert_eq!(state["pg"]["unnamed"], serde_json::json!(true));
        assert_eq!(state["pg"]["names"], serde_json::json!(["main"]));
    }

    // ── json_top_level_keys — the sidecar's key order, from the text ──

    #[test]
    fn json_top_level_keys_reads_declaration_order_not_sorted_order() {
        // The property the parsed `Value` cannot supply: `serde_json`
        // builds objects on a `BTreeMap`, so by the time anything can
        // read them `zeta` has moved behind `alpha`.
        let raw = r#"{
    "zeta": {"apiVersion": "apprafter.io/v1alpha1"},
    "alpha": {"apiVersion": "apprafter.io/v1alpha1"}
}"#;
        assert_eq!(json_top_level_keys(raw), vec!["zeta", "alpha"]);
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            parsed
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"],
            "if this ever matches the text order, `preserve_order` was turned on and the \
             ordering comment above is stale"
        );
    }

    #[test]
    fn json_top_level_keys_ignores_nested_keys_and_string_values() {
        let raw = r#"{
    "apiVersion": "apprafter.io/v1alpha1",
    "kind": "Application",
    "metadata": {"name": "demo", "namespace": "apprafter"},
    "spec": {"base": {"image": "x"}}
}"#;
        assert_eq!(
            json_top_level_keys(raw),
            vec!["apiVersion", "kind", "metadata", "spec"],
            "only depth-1 strings followed by `:` are keys — `\"Application\"` is a value and \
             `name` is nested"
        );
    }

    #[test]
    fn json_top_level_keys_survives_escapes_and_colons_in_values() {
        // A `:` inside a string value, and an escaped quote, must not be
        // mistaken for structure by a hand-rolled scanner.
        let raw = r#"{
    "one": {"note": "a \"quoted\" url: https://x/y"},
    "two": {}
}"#;
        assert_eq!(json_top_level_keys(raw), vec!["one", "two"]);
    }

    #[test]
    fn json_top_level_keys_of_a_non_object_is_empty() {
        // `ordered_keys` then sequences nothing and falls back to the
        // parsed object's own keys — it never drops a workload.
        assert!(json_top_level_keys("[1, 2, 3]").is_empty());
        assert!(json_top_level_keys("").is_empty());
        assert!(json_top_level_keys(r#"{"unterminated: 1"#).is_empty());
    }

    #[test]
    fn union_needs_merges_across_calls() {
        let mut state = serde_json::json!({});
        union_needs(&mut state, &serde_json::json!({ "pg": {} }));
        union_needs(&mut state, &serde_json::json!({ "redis": {} }));
        assert_eq!(state["pg"]["unnamed"], serde_json::json!(true));
        assert_eq!(state["redis"]["unnamed"], serde_json::json!(true));
    }

    // ── validate_manifest — real cue accept/reject ────────────
    //
    // These exercise the full cue-cmp-equivalent pipeline against
    // the `cue` binary. `cue` is provided by the repo's `nix
    // develop` shell (or `~/bin/cue` → `nix run nixpkgs#cue`), so
    // the gate command runs them with `cue` on PATH. If `cue` is
    // genuinely unavailable the test FAILS LOUDLY (no silent skip)
    // — local validation parity is load-bearing and an absent cue
    // must be visible, not swept under a green run.

    fn cue_available() -> bool {
        Command::new(cue_bin())
            .arg("version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Write a manifest into `<dir>/apprafter/Application.cue`
    /// (the scaffold convention) and return that file path.
    fn write_manifest(dir: &Path, body: &str) -> PathBuf {
        let app_dir = dir.join("apprafter");
        fs::create_dir_all(&app_dir).unwrap();
        let f = app_dir.join("Application.cue");
        fs::write(&f, body).unwrap();
        f
    }

    const VALID_MANIFEST: &str = r#"package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
	metadata: {
		name:      "demo"
		namespace: "apprafter"
	}
	spec: base: {
		image: "nginxdemos/hello:plain-text"
		needs: {
			pg: {}
		}
		env: {
			DATABASE_URL: claim.pg.url
			STRIPE_KEY: secret: "stripe/api-key"
		}
	}
}
"#;

    // Undeclared need: references claim.redis.url but only pg is
    // declared → the generated `claim` binding has no `redis` key
    // → bare selector fails to resolve → cue error.
    const UNDECLARED_NEED_MANIFEST: &str = r#"package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
	metadata: {
		name:      "demo"
		namespace: "apprafter"
	}
	spec: base: {
		image: "nginxdemos/hello:plain-text"
		needs: {
			pg: {}
		}
		env: {
			REDIS_URL: claim.redis.url
		}
	}
}
"#;

    #[test]
    fn validate_accepts_valid_claim_and_secret_manifest() {
        assert!(
            cue_available(),
            "`cue` not runnable — local validation parity is load-bearing; \
             run under `nix develop` (or ensure ~/bin/cue shim is present). \
             Refusing to silently pass."
        );
        let dir = tempdir().unwrap();
        let manifest = write_manifest(dir.path(), VALID_MANIFEST);
        let res = validate_manifest(&manifest);
        assert!(
            res.is_ok(),
            "valid manifest (needs.pg + claim.pg.url + secret) must pass; got: {:?}",
            res.unwrap_err()
        );
    }

    #[test]
    fn validate_rejects_undeclared_claim_reference() {
        assert!(
            cue_available(),
            "`cue` not runnable — refusing to silently pass (see sibling test)."
        );
        let dir = tempdir().unwrap();
        let manifest = write_manifest(dir.path(), UNDECLARED_NEED_MANIFEST);
        let res = validate_manifest(&manifest);
        assert!(
            res.is_err(),
            "claim.redis.url with only needs.pg declared must FAIL validation"
        );
    }

    #[test]
    fn validate_accepts_manifest_passed_as_directory() {
        // The CLI may resolve to either the file or the holding dir;
        // validate_manifest accepts both. Directory form copies every
        // *.cue in.
        assert!(
            cue_available(),
            "`cue` not runnable — refusing to silently pass."
        );
        let dir = tempdir().unwrap();
        write_manifest(dir.path(), VALID_MANIFEST);
        let app_dir = dir.path().join("apprafter");
        let res = validate_manifest(&app_dir);
        assert!(
            res.is_ok(),
            "directory form must validate; got: {:?}",
            res.err()
        );
    }

    // Manifest with declared `spec.environments` and NO vendored
    // `cue.mod` (write_manifest writes only Application.cue) — exactly
    // the post-2.12 layout that silently emptied the wizard env picker
    // because a bare `cue export` can't resolve the schema import.
    const ENV_MANIFEST: &str = r#"package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

landing: v1alpha1.#Application & {
	metadata: {
		name:      "landing"
		namespace: "procvue"
	}
	spec: base: {
		image: "ghcr.io/procvue/landing:latest"
		expose: {
			port:     8080
			network:  "public"
			hostname: "procvue.com"
		}
	}
	spec: environments: {
		dev: expose: {
			port:    8080
			network: "internal"
		}
		prod: replicas: 2
	}
}
"#;

    #[test]
    fn parse_application_injected_reads_environments_without_vendored_schema() {
        assert!(
            cue_available(),
            "`cue` not runnable — local schema injection is load-bearing; \
             run under `nix develop` (or ensure ~/bin/cue shim is present). \
             Refusing to silently pass."
        );
        let dir = tempdir().unwrap();
        write_manifest(dir.path(), ENV_MANIFEST);
        let app_dir = dir.path().join("apprafter");
        // No cue.mod was written — the injected workspace must supply
        // the schema so `import "apprafter.io/schemas/v1alpha1"` resolves.
        let manifest = parse_application_injected(&app_dir)
            .expect("injected parse must resolve the schema import without a vendored cue.mod");
        let mut envs: Vec<String> = manifest
            .spec
            .environments
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        envs.sort();
        assert_eq!(
            envs,
            vec!["dev".to_string(), "prod".to_string()],
            "wizard env picker (via get_manifest_environments) must see the declared envs"
        );
    }

    // ── Intra-bundle consistency: the LOCAL TWIN of the render-layer
    //    guards (ADR 0063 §Decision 5, subphase 2.27b) ──────────────
    //
    // 2.27a taught `argocd-cue-cmp/entrypoint.sh` to REFUSE four ways a
    // manifest package can contradict itself. ADR 0063 §Decision 5 puts
    // `apprafter app validate` on the same row of its enforcement table
    // ("cue-cmp `exit 1` + `validate`"), so a bundle the sidecar refuses
    // at sync must be refused here, on the laptop, first.
    //
    // These tests drive `validate_manifest` from the SIDECAR'S OWN
    // FIXTURES — `argocd-cue-cmp/testdata/bundle-*`, built and
    // mutation-tested in 2.27a — rather than from private copies. That is
    // the whole point: two layers asserting the same rule against two
    // sets of fixtures drift silently, and the drift shows up as a
    // manifest that validates clean and then reddens an Argo CD tile,
    // which is exactly the inverted promise 2.27b exists to restore.
    // The sidecar half of the pair asserts the same seven directories in
    // `argocd-cue-cmp/test-inject.sh` §5; sharing the fixtures is what
    // makes the two halves one gate rather than two.

    /// Repository root, derived from this crate's manifest directory.
    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("cli/platform-cli is two levels below the repo root")
            .to_path_buf()
    }

    /// Copy `argocd-cue-cmp/testdata/<name>/apprafter/` into a fresh
    /// temp dir and return the guard plus the copied directory.
    ///
    /// COPIED, never validated in place. `validate_manifest` itself only
    /// reads, but its sibling layer — the real `entrypoint.sh` the parity
    /// script runs over these same directories — writes `cue.mod/` and
    /// `apprafter_claim_gen.cue` into whatever it renders, and these
    /// fixtures are committed. One rule for both layers is cheaper than
    /// remembering which one is safe.
    ///
    /// Only top-level files are copied, which is what `lay_out_workspace`
    /// would have taken anyway — `inject-fixture-multi/` ships a
    /// `cue.mod/` of its own and the temp workspace supplies that.
    fn sidecar_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
        let src = repo_root()
            .join("argocd-cue-cmp")
            .join("testdata")
            .join(name)
            .join("apprafter");
        assert!(
            src.is_dir(),
            "cue-cmp fixture '{name}' is missing at {} — the CLI checks and the \
             sidecar checks share these fixtures on purpose; do not fork a copy",
            src.display()
        );
        let tmp = tempdir().unwrap();
        let dst = tmp.path().join("apprafter");
        fs::create_dir_all(&dst).unwrap();
        for entry in fs::read_dir(&src).unwrap() {
            let p = entry.unwrap().path();
            if p.is_file() {
                fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
            }
        }
        (tmp, dst)
    }

    const CUE_REQUIRED: &str = "`cue` not runnable — the intra-bundle checks ARE the local twin \
         of the render layer and a skipped run is a silently missing guard; \
         run under `nix develop` (or ensure the ~/bin/cue shim is present).";

    /// Validate a sidecar fixture and return the refusal, joined.
    fn refuse(name: &str) -> String {
        assert!(cue_available(), "{CUE_REQUIRED}");
        let (_guard, dir) = sidecar_fixture(name);
        let Err(err) = validate_manifest(&dir) else {
            panic!(
                "ADR 0063 §5: the cue-cmp sidecar REFUSES fixture '{name}' at render; \
                 `app validate` is its local twin and must refuse it too, not answer `✓ valid`"
            )
        };
        err.join("\n")
    }

    /// Validate a sidecar fixture that must stay VALID.
    fn accept(name: &str) {
        assert!(cue_available(), "{CUE_REQUIRED}");
        let (_guard, dir) = sidecar_fixture(name);
        if let Err(msgs) = validate_manifest(&dir) {
            panic!(
                "ADR 0063 §5: the cue-cmp sidecar renders fixture '{name}' at rc=0; \
                 `app validate` must not refuse what the cluster accepts. Got:\n{}",
                msgs.join("\n")
            );
        }
    }

    // (1) Style A mixed with Style B.
    #[test]
    fn validate_refuses_a_bundle_mixing_package_scope_with_named_wrappers() {
        let msg = refuse("bundle-mixed-style");
        assert!(
            msg.contains("bundle is inconsistent"),
            "the refusal must carry the sidecar's own marker so one finding reads as \
             one finding in both places; got:\n{msg}"
        );
        assert!(
            msg.contains("package-scope manifest mixed with named wrapper"),
            "mixed style — the summary must name the finding in the sidecar's words; got:\n{msg}"
        );
        assert!(
            msg.contains("wrapped"),
            "mixed style — the summary must name the wrapper that would be dropped; got:\n{msg}"
        );
    }

    // (2) Divergent metadata.namespace.
    #[test]
    fn validate_refuses_a_bundle_whose_workloads_declare_two_namespaces() {
        let msg = refuse("bundle-split-ns");
        assert!(
            msg.contains("2 different namespaces"),
            "namespaces — the summary must name the divergence; got:\n{msg}"
        );
        assert!(
            msg.contains(r#"nsOne -> "one""#) && msg.contains(r#"nsTwo -> "two""#),
            "namespaces — the summary must name BOTH workloads and their values; got:\n{msg}"
        );
        assert!(
            !msg.contains("spec.environment:"),
            "namespaces — the environment check must not also fire; got:\n{msg}"
        );
    }

    // (3) Duplicate (namespace, name).
    #[test]
    fn validate_refuses_a_bundle_with_a_duplicate_namespace_name_pair() {
        let msg = refuse("bundle-dup-name");
        assert!(
            msg.contains("share one (namespace, name)"),
            "identity — the summary must name the rule; got:\n{msg}"
        );
        assert!(
            msg.contains("dup-demo/dup-app"),
            "identity — the summary must name the colliding identity; got:\n{msg}"
        );
        assert!(
            msg.contains("dupOne, dupTwo"),
            "identity — the summary must name both workloads that declared it; got:\n{msg}"
        );
    }

    // (4) Divergent spec.environment — both declared.
    #[test]
    fn validate_refuses_a_bundle_whose_workloads_declare_two_environments() {
        let msg = refuse("bundle-split-env");
        assert!(
            msg.contains("2 different environments"),
            "environments — the summary must name the divergence; got:\n{msg}"
        );
        assert!(
            msg.contains(r#"envOne -> "dev""#) && msg.contains(r#"envTwo -> "prod""#),
            "environments — the summary must name BOTH workloads and their values; got:\n{msg}"
        );
    }

    // (4) Divergent spec.environment — the PARTIAL form. The sidecar
    // counts an ABSENT `spec.environment` as its own distinct value
    // (`entrypoint.sh` check (4)): absent is not "unspecified pending a
    // default", it is the BASE-ONLY deploy, and unlike a namespace there
    // is no `destination.environment` on the registration to fill it in.
    // The two layers must agree on that or the local twin is a different
    // rule wearing the same name.
    #[test]
    fn validate_counts_an_absent_environment_as_its_own_value() {
        let msg = refuse("bundle-env-partial");
        assert!(
            msg.contains("2 different environments"),
            "declared-vs-absent — absent must count as its own value; got:\n{msg}"
        );
        assert!(
            msg.contains("(not declared)"),
            "declared-vs-absent — the summary must spell out the absent side; got:\n{msg}"
        );
        assert!(
            msg.contains("partialDeclared") && msg.contains("partialAbsent"),
            "declared-vs-absent — the summary must name both workloads; got:\n{msg}"
        );
    }

    // NEGATIVE: the row filter carries an apiVersion predicate, never
    // `kind == "Application"` alone. Argo CD's own CRD is
    // `argoproj.io/v1alpha1, kind: Application` — the one foreign
    // apiVersion that collides exactly with ours — and it lives in
    // `argocd` with no `spec.environment`, so a kind-only filter reads
    // this package as two namespaces AND two environments and refuses a
    // bundle that is not inconsistent at all.
    #[test]
    fn validate_accepts_a_foreign_application_kind_beside_a_workload() {
        accept("bundle-foreign-kind");
    }

    // NEGATIVE: the strongest non-regression signal available locally —
    // two workloads that agree on everything the checks look at must
    // still validate clean and silently.
    #[test]
    fn validate_accepts_a_consistent_two_workload_bundle() {
        accept("inject-fixture-multi");
    }

    // ── The DEFAULT invocation, over a MULTI-FILE bundle ───────────
    //
    // Every check above hands `validate_manifest` the fixture DIRECTORY,
    // which is the one form that always read the whole package. The bare
    // `apprafter app validate` does not: it goes through
    // `resolve_manifest_path` first, and that is where a bundle used to
    // be reduced to one file of itself.
    //
    // All seven fixtures those checks share are a single `.cue` file, so
    // none of them could tell the two apart — a one-file package is the
    // same package whether you name the file or the directory. The two
    // fixtures below are the ones that can, and they are modelled on
    // this repository's own `landing/web/apprafter/`, the only
    // multi-file bundle that exists today.
    //
    // These drive resolution and validation TOGETHER, deliberately.
    // Splitting them would leave each half green while the command they
    // compose stays wrong, which is exactly the state that shipped.

    /// Run the bare `apprafter app validate` — no argument, from the
    /// directory that HOLDS the fixture's `apprafter/` — and return what
    /// the command would have reported.
    ///
    /// The `TempDir` guard rides along in the return so the caller keeps
    /// the fixture alive for the length of the assertion.
    fn bare_validate(
        name: &str,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        std::result::Result<Vec<BundleWorkload>, Vec<String>>,
    ) {
        assert!(cue_available(), "{CUE_REQUIRED}");
        let (guard, dir) = sidecar_fixture(name);
        let cwd = dir
            .parent()
            .expect("the fixture's apprafter/ has a parent")
            .to_path_buf();
        let resolved = resolve_manifest_path(None, &cwd, &fs_path_kind)
            .unwrap_or_else(|e| panic!("the bare command must resolve fixture '{name}': {e}"));
        let outcome = validate_manifest_workloads(&resolved);
        (guard, resolved, outcome)
    }

    #[test]
    fn validate_reads_every_file_of_a_multi_file_bundle_by_default() {
        let (_guard, resolved, outcome) = bare_validate("bundle-multi-file");
        assert!(
            resolved.is_dir(),
            "the bare command must resolve the bundle DIRECTORY, not one file of it; got {}",
            resolved.display()
        );
        let workloads = outcome.unwrap_or_else(|msgs| {
            panic!(
                "the consistent multi-file bundle must validate; got:\n{}",
                msgs.join("\n")
            )
        });
        // The SET, not the sequence — `validate_orders_workloads_the_way_the_sidecar_does`
        // owns the order, so each defect keeps a test of its own.
        let mut names: Vec<&str> = workloads.iter().map(|w| w.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["multi-file-web", "multi-file-web-preview"],
            "a two-FILE bundle is two workloads. Reporting one is the shape that shipped: the \
             sibling file was dropped at resolution, so all four cross-workload checks saw N=1 \
             and none of them could fire"
        );
    }

    #[test]
    fn validate_refuses_a_multi_file_bundle_that_contradicts_itself_by_default() {
        let (_guard, _resolved, outcome) = bare_validate("bundle-multi-file-split-ns");
        let Err(msgs) = outcome else {
            panic!(
                "the cue-cmp sidecar REFUSES this bundle at render (`bundle is inconsistent: \
                 workloads declare 2 different namespaces`); the bare `app validate` answering \
                 `✓ valid` is the local twin returning the OPPOSITE verdict"
            )
        };
        let msg = msgs.join("\n");
        assert!(
            msg.contains("2 different namespaces"),
            "the refusal must name the divergence in the sidecar's words; got:\n{msg}"
        );
        assert!(
            msg.contains(r#"splitFileWeb -> "prod""#)
                && msg.contains(r#"splitFileWebPreview -> "preview""#),
            "the refusal must name BOTH workloads and their values, across the file boundary; \
             got:\n{msg}"
        );
    }

    // Workload ORDER, which a multi-file bundle is also the only local
    // shape that can test. `cue export . --out json` — the document the
    // sidecar hands `jq to_entries` — emits a multi-file package's keys
    // sorted, while `cue def ./...` follows FILE order. Measured on this
    // fixture (and on `landing/web/apprafter`, which has the same file
    // names): `Application-preview.cue` sorts before `Application.cue`,
    // so the two disagree and the CLI printed the sidecar's findings in
    // the reverse sequence from the Argo CD tile.
    #[test]
    fn validate_orders_workloads_the_way_the_sidecar_does() {
        let (_guard, _resolved, outcome) = bare_validate("bundle-multi-file");
        let workloads = outcome.expect("the consistent multi-file bundle must validate");
        assert_eq!(
            format_roster(&workloads),
            vec![
                "✓ valid — 2 workloads".to_string(),
                "  • multi-file-web  namespace multi-file".to_string(),
                "  • multi-file-web-preview  namespace multi-file".to_string(),
            ],
            "the roster must follow the exported JSON's own key order — the sequence the \
             sidecar's `jq to_entries` walks — so one finding reads as one finding in both places"
        );

        // And the same order in a REFUSAL, which is the line Argo CD
        // truncates onto the Application tile.
        let (_g2, _r2, refused) = bare_validate("bundle-multi-file-split-ns");
        let msg = refused
            .expect_err("the split-namespace bundle must be refused")
            .join("\n");
        let web = msg
            .find("splitFileWeb ->")
            .expect("summary names the prod workload");
        let preview = msg
            .find("splitFileWebPreview ->")
            .expect("summary names the preview workload");
        assert!(
            web < preview,
            "the refusal must list the workloads in the sidecar's order; got:\n{msg}"
        );
    }

    // A Style-A (unwrapped) package renders exactly ONE row into the
    // table, so all three cross-workload checks are no-ops on it. That
    // is what keeps every pre-bundle single-manifest layout working;
    // assert it rather than assume it.
    #[test]
    fn validate_still_accepts_an_unwrapped_style_a_manifest() {
        assert!(cue_available(), "{CUE_REQUIRED}");
        let dir = tempdir().unwrap();
        let manifest = write_manifest(
            dir.path(),
            "package apprafter\n\
             \n\
             apiVersion: \"apprafter.io/v1alpha1\"\n\
             kind:       \"Application\"\n\
             metadata: {\n\
             \tname:      \"style-a\"\n\
             \tnamespace: \"apprafter\"\n\
             }\n\
             spec: base: image: \"nginxdemos/hello:plain-text\"\n",
        );
        let workloads = validate_manifest_workloads(&manifest)
            .expect("an unwrapped Style-A manifest must still validate");
        assert_eq!(
            workloads.len(),
            1,
            "Style A is exactly one row — got {workloads:?}"
        );
        assert_eq!(workloads[0].key, PACKAGE_SCOPE_KEY);
        assert_eq!(workloads[0].name, "style-a");
    }

    // ── the success roster ────────────────────────────────────

    fn wl(key: &str, name: &str, ns: &str, envs: &[&str]) -> BundleWorkload {
        BundleWorkload {
            key: key.to_string(),
            namespace: ns.to_string(),
            name: name.to_string(),
            environment: String::new(),
            environments: envs.iter().map(|e| e.to_string()).collect(),
        }
    }

    #[test]
    fn validate_roster_names_every_workload_of_a_bundle() {
        let ws = vec![
            wl("api", "acme-api", "acme", &["dev", "prod"]),
            wl("web", "acme-web", "acme", &["dev", "prod"]),
        ];
        assert_eq!(
            format_roster(&ws),
            vec![
                "✓ valid — 2 workloads".to_string(),
                "  • acme-api  namespace acme  envs: dev, prod".to_string(),
                "  • acme-web  namespace acme  envs: dev, prod".to_string(),
            ],
            "the roster answers 'how many workloads is this?', which a bare `✓ valid` cannot"
        );
    }

    #[test]
    fn validate_roster_at_one_workload_is_two_lines() {
        // The decision, pinned: the roster DOES appear at N=1. The count
        // is as load-bearing there as anywhere — a package whose second
        // workload silently failed to parse as one reports `1 workload`,
        // and printing nothing would hide the only evidence of that.
        assert_eq!(
            format_roster(&[wl("app", "solo", "apprafter", &[])]),
            vec![
                "✓ valid — 1 workload".to_string(),
                "  • solo  namespace apprafter".to_string(),
            ],
            "N=1 prints the count and one bullet — and NO `envs:` segment when none are declared"
        );
    }

    #[test]
    fn validate_roster_with_no_workloads_stays_bare() {
        assert_eq!(
            format_roster(&[]),
            vec!["✓ valid".to_string()],
            "a package with no workloads is supporting CUE; a '0 workloads' header plus an \
             empty list says less than the line above it"
        );
    }

    #[test]
    fn validate_roster_spells_out_an_undeclared_namespace() {
        assert_eq!(
            format_roster(&[wl("app", "nsless", "", &[])]),
            vec![
                "✓ valid — 1 workload".to_string(),
                "  • nsless  namespace (not declared)".to_string(),
            ],
            "an absent namespace is filled in by the registration's destination.namespace, \
             which this layer cannot see — say so rather than print an empty field"
        );
    }

    #[test]
    fn validate_roster_reads_a_real_two_workload_fixture() {
        // End-to-end: the roster comes off the SAME filtered row set the
        // checks use, against the sidecar's own consistent fixture.
        assert!(cue_available(), "{CUE_REQUIRED}");
        let (_guard, dir) = sidecar_fixture("inject-fixture-multi");
        let workloads =
            validate_manifest_workloads(&dir).expect("the consistent bundle must validate");
        assert_eq!(
            format_roster(&workloads),
            vec![
                "✓ valid — 2 workloads".to_string(),
                "  • inject-multi-one  namespace (not declared)".to_string(),
                "  • inject-multi-two  namespace (not declared)".to_string(),
            ],
            "declaration order, both workloads, no invented namespace"
        );
    }

    #[test]
    fn validate_roster_omits_the_foreign_kind_object() {
        // The roster is a roster of WORKLOADS. `bundle-foreign-kind`
        // ships an `argoproj.io` Application beside one, and the row
        // filter's apiVersion predicate is what keeps it out of both the
        // checks and this list.
        assert!(cue_available(), "{CUE_REQUIRED}");
        let (_guard, dir) = sidecar_fixture("bundle-foreign-kind");
        let workloads =
            validate_manifest_workloads(&dir).expect("the foreign-kind fixture must validate");
        assert_eq!(
            workloads
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>(),
            vec!["foreign-web"],
            "only the apprafter.io workload is a workload of this bundle"
        );
    }
}
