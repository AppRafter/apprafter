// SPDX-License-Identifier: FSL-1.1-MIT
//! Subprocess wrapper around the `cue` binary.
//!
//! The wrapper is intentionally minimal: it shells out to
//! `cue export <path> --out json` and parses the resulting JSON
//! into a `serde_json::Value`. The in-process FFI option
//! (`cuelang-go` via cgo bridge) is reserved for a future ADR.
//!
//! `cue` rejects absolute directory paths (it expects a path
//! relative to the module root, e.g. `./schemas/...`), so the
//! wrapper exposes an `export_in(workdir, path)` helper that
//! invokes `cue` with `current_dir(workdir)`. The simpler
//! [`export`] form runs in the current process directory.
//!
//! The `CUE_BIN` environment variable overrides the binary path,
//! which is useful both for tests and for users with custom
//! installs.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::{CliError, Result};

const DEFAULT_BIN: &str = "cue";

fn cue_bin() -> String {
    std::env::var("CUE_BIN").unwrap_or_else(|_| DEFAULT_BIN.to_string())
}

/// Run `cue export <path> --out json` in the current directory.
pub fn export(path: &Path) -> Result<Value> {
    let cwd = std::env::current_dir()?;
    export_in(&cwd, path)
}

/// Run `cue export <path> --out json` with `cue` invoked from
/// `workdir` (which should be the directory containing `cue.mod/`).
/// `path` is interpreted by `cue` relative to `workdir`.
pub fn export_in(workdir: &Path, path: &Path) -> Result<Value> {
    let bin = cue_bin();

    let output = match Command::new(&bin)
        .current_dir(workdir)
        .arg("export")
        .arg(path)
        .arg("--out")
        .arg("json")
        .output()
    {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CliError::CueNotFound);
        }
        Err(err) => return Err(CliError::from(err)),
    };

    if !output.status.success() {
        return Err(CliError::CueExport {
            exit: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    let value: Value = serde_json::from_slice(&output.stdout)?;
    Ok(value)
}
