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

/// AppRafter v1alpha1 schema files embedded at compile time —
/// the SAME `include_str!` sources `app_scaffold` reuses. The
/// validate path lays these into the temp workspace's
/// `cue.mod/pkg/` (inject-wins) so the manifest's
/// `import "apprafter.io/schemas/v1alpha1"` resolves against
/// the schema THIS CLI ships with (no vendored-copy drift —
/// ADR 0046 Decision #7).
const SCHEMA_TYPES_CUE: &str = include_str!("../../../../schemas/v1alpha1/types.cue");
const SCHEMA_APPLICATION_CUE: &str = include_str!("../../../../schemas/v1alpha1/application.cue");

/// Schema files written into the temp workspace's
/// `cue.mod/pkg/apprafter.io/schemas/v1alpha1/`. Only the two
/// files the `Application` manifest imports are needed (types +
/// application); the other CRDs in `schemas/v1alpha1/` are not
/// referenced by a user `Application.cue`.
const WORKSPACE_SCHEMAS: &[(&str, &str)] = &[
    ("types.cue", SCHEMA_TYPES_CUE),
    ("application.cue", SCHEMA_APPLICATION_CUE),
];

/// Minimal render-workspace module file. Mirrors the
/// `entrypoint.sh` `inject_schema_and_claim` module: the module
/// path is irrelevant to resolving the bundled `pkg/`, but cue
/// requires the file to exist. A cwd-local `cue.mod` shadows any
/// parent module so the import resolves unambiguously through
/// our injected bundle.
const WORKSPACE_MODULE_CUE: &str = "module: \"apprafter.io/render-workspace\"

language: {
\tversion: \"v0.10.0\"
}
";

/// The generated claim-binding filename. NO leading underscore:
/// cue IGNORES files whose name begins with `_` (or `.`), so
/// `_apprafter_claim_gen.cue` would silently never load. Same
/// name the cue-cmp's `entrypoint.sh` writes (`CLAIM_GEN`).
const CLAIM_GEN_FILE: &str = "apprafter_claim_gen.cue";

/// Resolve which manifest file/dir to validate, given the
/// optional CLI argument and the cwd. Pure — filesystem
/// presence is injected via `exists` so the discovery rules are
/// unit-testable without touching disk.
///
/// Rules (ADR 0046 Decision #6):
///   1. explicit `arg` always wins — error if it doesn't exist.
///   2. else `<cwd>/apprafter/Application.cue` if it exists
///      (the `apprafter app scaffold` convention).
///   3. else if cwd holds EXACTLY ONE `*.cue` file, use it.
///   4. else error, telling the user to pass a manifest path.
pub fn resolve_manifest_path(
    arg: Option<&Path>,
    cwd: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf> {
    if let Some(arg) = arg {
        let p = if arg.is_absolute() {
            arg.to_path_buf()
        } else {
            cwd.join(arg)
        };
        if exists(&p) {
            return Ok(p);
        }
        return Err(CliError::Other(format!(
            "manifest path '{}' does not exist.",
            arg.display()
        )));
    }

    let scaffold_default = cwd.join("apprafter").join("Application.cue");
    if exists(&scaffold_default) {
        return Ok(scaffold_default);
    }

    let cues = list_cue_files(cwd);
    match cues.as_slice() {
        [single] => Ok(single.clone()),
        [] => Err(CliError::Other(format!(
            "no manifest found. Looked for '{}' and a single `*.cue` in '{}'. \
             Pass an explicit manifest path: `apprafter app validate <manifest>`.",
            scaffold_default.display(),
            cwd.display()
        ))),
        many => Err(CliError::Other(format!(
            "{} `*.cue` files found in '{}' — ambiguous. Pass the manifest \
             explicitly: `apprafter app validate <manifest>`.",
            many.len(),
            cwd.display()
        ))),
    }
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
/// render-time injection in a fresh temp dir and runs `cue vet`.
///
/// On success → `Ok(())`. On a non-zero `cue` exit → `Err` with
/// the captured stderr split into lines (each a diagnostic). If
/// `cue` is absent → `Err` carrying [`CliError::CueNotFound`]'s
/// message so the caller can surface the install hint.
pub fn validate_manifest(manifest: &Path) -> std::result::Result<(), Vec<String>> {
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

    run_cue_vet(root)
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

/// Entry point for `apprafter app validate [manifest]`. Resolves
/// the manifest path, runs the cue-cmp-equivalent validation,
/// and prints `✓ valid` or the diagnostics. Returns `Err` (so
/// the process exits non-zero) on validation failure.
pub fn run_validate(arg: Option<PathBuf>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let manifest = resolve_manifest_path(arg.as_deref(), &cwd, &|p| p.exists())?;

    println!("Validating {} …", manifest.display());
    match validate_manifest(&manifest) {
        Ok(()) => {
            println!("✓ valid");
            Ok(())
        }
        Err(msgs) => {
            eprintln!("✗ validation failed:");
            for m in &msgs {
                eprintln!("  {m}");
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

    #[test]
    fn resolve_explicit_arg_wins_when_present() {
        let cwd = Path::new("/work");
        let arg = Path::new("custom/My.cue");
        let present: HashSet<PathBuf> = [PathBuf::from("/work/custom/My.cue")].into();
        let got = resolve_manifest_path(Some(arg), cwd, &|p| present.contains(p)).unwrap();
        assert_eq!(got, PathBuf::from("/work/custom/My.cue"));
    }

    #[test]
    fn resolve_explicit_absolute_arg_used_verbatim() {
        let cwd = Path::new("/work");
        let arg = Path::new("/elsewhere/App.cue");
        let present: HashSet<PathBuf> = [PathBuf::from("/elsewhere/App.cue")].into();
        let got = resolve_manifest_path(Some(arg), cwd, &|p| present.contains(p)).unwrap();
        assert_eq!(got, PathBuf::from("/elsewhere/App.cue"));
    }

    #[test]
    fn resolve_explicit_missing_arg_errors() {
        let cwd = Path::new("/work");
        let arg = Path::new("nope.cue");
        let err = resolve_manifest_path(Some(arg), cwd, &|_| false).unwrap_err();
        assert!(
            err.to_string().contains("does not exist"),
            "missing explicit arg must error with 'does not exist'; got: {err}"
        );
    }

    #[test]
    fn resolve_defaults_to_apprafter_application_cue() {
        let cwd = Path::new("/work");
        let default = PathBuf::from("/work/apprafter/Application.cue");
        let present: HashSet<PathBuf> = [default.clone()].into();
        let got = resolve_manifest_path(None, cwd, &|p| present.contains(p)).unwrap();
        assert_eq!(got, default);
    }

    #[test]
    fn resolve_single_cue_in_cwd_when_no_scaffold_default() {
        // No apprafter/Application.cue, but exactly one *.cue in cwd.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("App.cue"), "package x\n").unwrap();
        let got = resolve_manifest_path(None, dir.path(), &|p| p.exists()).unwrap();
        assert_eq!(got, dir.path().join("App.cue"));
    }

    #[test]
    fn resolve_errors_when_no_manifest_found() {
        // Empty dir, no scaffold default, no *.cue.
        let dir = tempdir().unwrap();
        let err = resolve_manifest_path(None, dir.path(), &|p| p.exists()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no manifest found") && msg.contains("apprafter app validate"),
            "empty cwd must error pointing at an explicit path; got: {msg}"
        );
    }

    #[test]
    fn resolve_errors_when_multiple_cue_files_ambiguous() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.cue"), "package x\n").unwrap();
        fs::write(dir.path().join("b.cue"), "package x\n").unwrap();
        let err = resolve_manifest_path(None, dir.path(), &|p| p.exists()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ambiguous"),
            "multiple *.cue must error as ambiguous; got: {msg}"
        );
    }

    #[test]
    fn resolve_scaffold_default_preferred_over_single_cwd_cue() {
        // Both an apprafter/Application.cue AND a cwd *.cue exist —
        // the scaffold default must win.
        let cwd = Path::new("/work");
        let default = PathBuf::from("/work/apprafter/Application.cue");
        // `exists` only knows about the scaffold default (list_cue_files
        // reads the real fs, so we keep this on the injected-`exists`
        // path by ensuring the default is present).
        let present: HashSet<PathBuf> = [default.clone()].into();
        let got = resolve_manifest_path(None, cwd, &|p| present.contains(p)).unwrap();
        assert_eq!(got, default);
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
}
