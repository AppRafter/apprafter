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

pub mod model;
