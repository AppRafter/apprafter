// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The CLI's `apprafter_core::Context`: its whole environment, read here and nowhere else in
//! the core-backed arms (ADR 0067 §2).

use std::ffi::OsString;

use apprafter_core::{Context, EnvSource};

/// The process environment. The one `std::env::var` + `std::env::var_os` pair the core-backed
/// arms read through (counted by the core's env-read ratchet).
pub(crate) struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn var_os(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }
}

/// Built by each core-backed arm, not once in `run()`: a command that never touches the core
/// cannot fail on it.
pub(crate) fn cli_context() -> miette::Result<Context> {
    Context::from_cli_env(&ProcessEnv).map_err(crate::render::core_error::report)
}
