// SPDX-License-Identifier: FSL-1.1-MIT
//! Error type used throughout `apprafter` and its libraries.
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

    /// Pre-flight rejection: the requested Hetzner server type is
    /// unknown / deprecated / unavailable in the requested region.
    /// `alternatives` carries up to 3 suggested live names for the
    /// same region.
    #[error(
        "server type `{requested}` is unavailable in region `{location}`: {reason}\n  \
         try one of: {alternatives}\n  \
         (override via APPRAFTER_MANIFEST or `--server-type` once it lands)"
    )]
    ServerTypeUnavailable {
        requested: String,
        location: String,
        reason: String,
        /// Comma-separated list of suggestions (already formatted
        /// for the error message).
        alternatives: String,
    },

    /// State file present but unparseable.
    #[error("state file at {path}: {message}")]
    InvalidState { path: PathBuf, message: String },

    /// Target config / credentials / global-config file present but
    /// unparseable. Distinct from `InvalidState` so error messages
    /// can point users at `apprafter target add <name> --renew`
    /// instead of `apprafter init`.
    #[error("target config at {path}: {message}")]
    InvalidTargetConfig { path: PathBuf, message: String },

    /// A subcommand asked for target `name`, but the target store
    /// has no such target. `available` lists what *is* configured
    /// (may be empty, signalling first-run + `apprafter target add`
    /// is the right next step).
    #[error("target `{name}` not found (available: {available})")]
    TargetNotFound {
        name: String,
        /// Comma-separated list of configured target names; the
        /// empty string `""` means "no targets configured yet".
        available: String,
    },

    /// Pass-through for `std::io::Error`.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// JSON encode/decode error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// YAML encode/decode error (target store files use YAML).
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// Catch-all, free-form message.
    #[error("{0}")]
    Other(String),
}

/// `Result` alias used everywhere in the CLI crates.
pub type Result<T> = std::result::Result<T, CliError>;
