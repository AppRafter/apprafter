// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Run `cue` as a subprocess, mirroring `cli-providers/build.rs`:
//! the bare `cue` on PATH, falling back to `nix run nixpkgs#cue --`.
//! ADR 0047 R4: the generator + CI + the drift gate must agree on one
//! cue version for the byte-compare to be reproducible.

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::path::Path;
use std::process::{Command, Stdio};

fn run_cue(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let bare = Command::new("cue")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output();
    let out = match bare {
        Ok(o) => o,
        Err(_) => {
            let mut nix: Vec<&str> = vec!["run", "nixpkgs#cue", "--"];
            nix.extend_from_slice(args);
            Command::new("nix")
                .args(&nix)
                .current_dir(dir)
                .stdin(Stdio::null())
                .output()
                .context(
                    "neither `cue` nor `nix run nixpkgs#cue` is on PATH \
                     (install cue v0.10+ or run under `nix develop`)",
                )?
        }
    };
    if !out.status.success() {
        bail!(
            "cue {:?} failed (exit {:?}):\n{}",
            args,
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out.stdout)
}

/// Export the `v1alpha1` package as OpenAPI; return `components.schemas`.
pub fn export_schemas(repo_root: &Path) -> Result<Map<String, Value>> {
    let bytes = run_cue(
        repo_root,
        &["export", "./schemas/v1alpha1", "--out", "openapi"],
    )?;
    let v: Value = serde_json::from_slice(&bytes).context("parse cue openapi output")?;
    v.get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(Value::as_object)
        .cloned()
        .context("cue openapi output missing components.schemas")
}

/// Export the hidden `_crdMetas` map (kind → CRD-envelope metadata).
pub fn export_crd_metas(repo_root: &Path) -> Result<Map<String, Value>> {
    let bytes = run_cue(
        repo_root,
        &[
            "export",
            "./schemas/v1alpha1",
            "-e",
            "_crdMetas",
            "--out",
            "json",
        ],
    )?;
    let v: Value = serde_json::from_slice(&bytes).context("parse _crdMetas")?;
    v.as_object().cloned().context("_crdMetas is not an object")
}
