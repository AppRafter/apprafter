// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `ResticRunner` — abstract interface over the restic subprocess calls the
//! backup engine needs. The CLI implementation [`SubprocessRestic`] lives here
//! so the in-cluster runner (a coming phase) can reuse the same trait without
//! depending on the CLI binary.

use std::process::Command;

use cli_core::{CliError, Result};
use serde_json::Value;

/// Restic operations the backup engine needs.
///
/// * `run` — fire-and-forget (init, etc.); silent on success.
/// * `run_stdout` — capture and return stdout verbatim (snapshots --json, etc.).
/// * `run_backup` — run `restic backup --json` and parse the summary line to
///   extract the snapshot id. Returns `None` when the summary JSON line is
///   absent (a restic version difference — the backup still succeeded).
pub trait ResticRunner {
    /// Run a restic command; return `Err` on non-zero exit.
    fn run(&self, argv: &[String], passphrase: &str) -> Result<()>;

    /// Run a restic command and return its stdout verbatim.
    fn run_stdout(&self, argv: &[String], passphrase: &str) -> Result<String>;

    /// Run `restic backup --json` and return the snapshot id extracted from the
    /// structured summary line, or `None` when the summary object is absent.
    fn run_backup(&self, argv: &[String], passphrase: &str) -> Result<Option<String>>;
}

// ---------------------------------------------------------------------------
// Concrete subprocess implementation
// ---------------------------------------------------------------------------

/// CLI's (and in-cluster runner's) concrete implementation of [`ResticRunner`]:
/// shells out to a `restic` binary on `$PATH` with `RESTIC_PASSWORD` injected
/// via the environment (never on argv).
pub struct SubprocessRestic;

impl ResticRunner for SubprocessRestic {
    fn run(&self, argv: &[String], pass: &str) -> Result<()> {
        let out = Command::new("restic")
            .args(argv)
            .env("RESTIC_PASSWORD", pass)
            .output()
            .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
        if !out.status.success() {
            return Err(CliError::Other(format!(
                "restic {} failed (exit {:?}): {}",
                argv.first().map(String::as_str).unwrap_or("?"),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(())
    }

    fn run_stdout(&self, argv: &[String], pass: &str) -> Result<String> {
        let out = Command::new("restic")
            .args(argv)
            .env("RESTIC_PASSWORD", pass)
            .output()
            .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
        if !out.status.success() {
            return Err(CliError::Other(format!(
                "restic {} failed (exit {:?}): {}",
                argv.first().map(String::as_str).unwrap_or("?"),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run_backup(&self, argv: &[String], pass: &str) -> Result<Option<String>> {
        let stdout = self.run_stdout(argv, pass)?;
        let snapshot_id = stdout.lines().find_map(|line| {
            let obj: Value = serde_json::from_str(line.trim()).ok()?;
            if obj.pointer("/message_type").and_then(Value::as_str) == Some("summary") {
                obj.pointer("/snapshot_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            }
        });
        Ok(snapshot_id)
    }
}
