// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `ResticRunner` — abstract interface over the restic subprocess calls the
//! backup engine needs. Implemented by
//! `platform_cli::commands::backup::SubprocessRestic` (subprocess path) and,
//! in a later phase, by an in-cluster runner that speaks to the restic REST
//! server directly.

use cli_core::Result;

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
