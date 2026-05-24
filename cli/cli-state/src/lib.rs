// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Local runtime state for `apprafter`. As of v0.1.154 the on-disk
//! location moved from per-cwd `.apprafter/state.json` to per-target
//! `<config>/state/<target>/.apprafter/state.json` — see the module
//! comment in `state.rs` for the full rationale and the migration
//! helper that smooths upgrades.

pub mod state;

pub use state::{migrate_legacy_state_if_present, HetznerCloudState, State, StatePaths};
