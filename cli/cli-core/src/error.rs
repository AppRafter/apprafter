// SPDX-License-Identifier: FSL-1.1-MIT
//! Error type used throughout `platform-cli` and its libraries.
//!
//! `CliError` carries variants for every recoverable failure mode
//! the CLI surfaces. Anything we cannot recover from (programmer
//! error, broken invariants) panics.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    /// The `cue` binary was not found on `PATH`.
    #[error("`cue` binary not found on PATH (install via `nix develop` or follow docs/contributing/setup.md)")]
    CueNotFound,

    /// Calling `cue export` produced a non-zero exit code.
    #[error("cue export failed (exit {exit}): {stderr}")]
    CueExport { exit: i32, stderr: String },

    /// Hetzner Cloud API call failed.
    #[error("hetzner-cloud {endpoint} failed (status {status}): {code}: {message}")]
    Hetzner {
        endpoint: String,
        status: u16,
        code: String,
        message: String,
    },

    /// State file present but unparseable.
    #[error("state file at {path}: {message}")]
    InvalidState { path: PathBuf, message: String },

    /// Pass-through for `std::io::Error`.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// JSON encode/decode error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Catch-all, free-form message.
    #[error("{0}")]
    Other(String),
}

/// `Result` alias used everywhere in the CLI crates.
pub type Result<T> = std::result::Result<T, CliError>;
