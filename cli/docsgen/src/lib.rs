// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Generate and drift-check the AppRafter documentation reference.
//!
//! The CLI reference is derived from the clap definitions rather than
//! hand-written, so a renamed flag cannot silently outdate the docs.
//! This crate reads the `apprafter` clap tree through that crate's
//! `docs_api` facade and projects it into [`model::Tree`] — the shape
//! the renderer emits as `docs/reference/cli/**` and the documentation
//! snippet gate consumes as `commands.json`.
//!
//! It is a separate crate rather than a second bin of `platform-cli`
//! because the release workflow builds `-p apprafter`, which builds
//! every bin of that package: a sibling bin would ship in every
//! release artefact for every target. `operator/crdgen` is the
//! precedent.

pub mod check;
pub mod model;
pub mod render;

use std::error::Error;
use std::path::PathBuf;

/// The repository root, found by walking up from the current directory
/// until `cue.mod/module.cue` appears.
///
/// Same anchor and same walk as `operator/crdgen` — one convention for
/// both generators. It matters here because `cargo run -p docsgen` is
/// invoked from `cli/`, the Justfile recipe from the repo root and the
/// lefthook hook from wherever the commit was made; all three must
/// resolve to the same `docs/reference/cli`.
///
/// This only composes paths. No rendered byte depends on it, so a
/// checkout at a different absolute path produces the same artefacts.
pub fn repo_root() -> Result<PathBuf, Box<dyn Error>> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("cue.mod/module.cue").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err("could not locate the repo root (no cue.mod/module.cue above CWD)".into());
        }
    }
}
