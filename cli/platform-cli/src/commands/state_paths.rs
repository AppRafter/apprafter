// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Helper that gives every operational command the same answer
//! to the question "where does my state live, and how do I get
//! there from here?".
//!
//! ## Why a shared helper
//!
//! Pre-v0.1.154 every command opened with the same three lines:
//!
//! ```ignore
//! let cwd = std::env::current_dir()?;
//! let paths = StatePaths::for_root(&cwd);
//! let state = State::load_or_default(&paths)?;
//! ```
//!
//! That cwd anchor was the root cause of the v0.1.154 walk-fix:
//! an operator running `apprafter apply` from the project root
//! and `apprafter app add` from `landing/cms/` got two different
//! state files because the two invocations had two different
//! cwds. The new layout pins state to the active **target**
//! (`<config>/state/<active-target>/`), which is the same answer
//! regardless of cwd.
//!
//! ## What [`resolve_state_paths`] does
//!
//! 1. Resolve the target-store root via
//!    `cli_core::target::default_config_root()` (honours
//!    `APPRAFTER_CONFIG_DIR` so integration tests can redirect).
//! 2. Resolve the active target name — `--target <name>`
//!    override → `GlobalConfig.active_target` →
//!    `CliError::NoActiveTarget` if nothing's set.
//! 3. Build the per-target [`StatePaths`].
//! 4. Best-effort migration: if the operator has a legacy
//!    `<cwd>/.apprafter/state.json` from v0.1.153 and the new
//!    per-target slot is empty, move the legacy file across.
//!    Skipped silently when the legacy file is absent — the
//!    common case for fresh installs.
//!
//! Every command that reads state calls this BEFORE
//! `State::load_or_default`. The function returns both the
//! `StatePaths` and the `TargetStorePaths` so callers that
//! also need credential resolution (`apply`, `destroy`,
//! `import`, `kubeconfig`) reuse the same store handle without
//! re-running the env-var probe.

use cli_core::target::{
    default_config_root, list_target_names, resolve_active_target_name, TargetStorePaths,
};
use cli_core::{CliError, Result};
use cli_state::{migrate_legacy_state_if_present, StatePaths};

/// Bundle returned to operational commands: per-target
/// `StatePaths` plus the `TargetStorePaths` they can reuse for
/// credential resolution. Carrying both saves callers a second
/// `default_config_root()` round-trip and keeps the
/// "resolve once, use everywhere" property.
pub struct ResolvedStatePaths {
    pub paths: StatePaths,
    pub store: TargetStorePaths,
    pub target_name: String,
}

/// Resolve the per-target state directory and trigger the
/// one-shot legacy migration if needed.
///
/// `target_override` is the value of the `--target <name>` flag
/// (when the command carries one). Pass `None` for commands like
/// `apprafter app add` that don't expose `--target` — they
/// always read the active target.
///
/// Returns `CliError::NoActiveTarget` when the target store has
/// no active target and no override was given, and
/// `CliError::TargetNotFound` when the override names a target the
/// store does not have. Both are recoverable — `apprafter target
/// add <name>` / `target use <name>` — and their help says how.
pub fn resolve_state_paths(target_override: Option<&str>) -> Result<ResolvedStatePaths> {
    let store_root = default_config_root()?;
    let store = TargetStorePaths::for_root(store_root);
    let target_name =
        resolve_active_target_name(&store, target_override)?.ok_or(CliError::NoActiveTarget)?;

    // Eager existence check when the operator supplied a
    // `--target <name>` override. Without it, an operator typing
    // `apprafter apply --target ghost` would see the much later
    // "no provider configured" generic error — the right hint
    // is "ghost not found, available: …" so they can correct the
    // typo. The active-target case (no override) skips the
    // check: state may legitimately not yet exist on a fresh
    // target the operator just added.
    if target_override.is_some() {
        let available = list_target_names(&store).unwrap_or_default();
        if !available.iter().any(|n| n == &target_name) {
            return Err(CliError::TargetNotFound {
                name: target_name,
                available: available.join(", "),
            });
        }
    }

    let paths = StatePaths::for_active_target(&store, &target_name);

    // Best-effort migration from the v0.1.153 per-cwd layout.
    // Caller's cwd may legitimately not be a project root (the
    // operator just opened a fresh shell), so a missing legacy
    // file is the common case — the helper handles that
    // silently.
    if let Ok(cwd) = std::env::current_dir() {
        migrate_legacy_state_if_present(&cwd, &paths)?;
    }

    Ok(ResolvedStatePaths {
        paths,
        store,
        target_name,
    })
}
