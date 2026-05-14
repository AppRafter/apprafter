// SPDX-License-Identifier: FSL-1.1-MIT
//! Shared utilities for `apprafter` and its sister crates.

pub mod cue;
pub mod error;
pub mod logging;
pub mod manifest;
pub mod secrets;
pub mod target;
pub mod tier;

pub use error::{CliError, Result};
pub use target::{
    default_config_root, list_target_names, load_global_config, load_target, remove_target,
    save_global_config, save_target, GlobalConfig, Target, TargetConfig, TargetCredentials,
    TargetStorePaths, TARGET_STORE_VERSION,
};
pub use tier::Tier;
