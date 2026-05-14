// SPDX-License-Identifier: FSL-1.1-MIT
//! Shared utilities for `apprafter` and its sister crates.

pub mod credentials;
pub mod cue;
pub mod error;
pub mod logging;
pub mod manifest;
pub mod secrets;
pub mod target;
pub mod tier;

pub use credentials::{
    read_ssh_public_key_body, resolve_hetzner_ssh_public_key, resolve_hetzner_token,
    HCLOUD_TOKEN_ENV, SSH_PUBLIC_KEY_ENV,
};
pub use error::{CliError, Result};
pub use target::{
    default_config_root, list_target_names, load_global_config, load_target, remove_target,
    rename_target, save_global_config, save_target, validate_hetzner_token_format, GlobalConfig,
    Target, TargetConfig, TargetCredentials, TargetStorePaths, CONFIG_DIR_ENV,
    TARGET_STORE_VERSION,
};
pub use tier::Tier;

/// Process-wide test-only mutex serialising env-touching tests
/// across our `#[cfg(test)]` modules. Cargo runs unit tests in
/// parallel by default, and `std::env::set_var` is process-wide
/// — without a shared lock, two tests that flip `HCLOUD_TOKEN`
/// or `APPRAFTER_CONFIG_DIR` (or any other env var the resolver
/// chain consults) race and surface flaky failures.
///
/// `pub(crate)` so the test modules under `credentials::tests` and
/// `target::tests` can both lock the same instance.
#[cfg(test)]
pub(crate) static TEST_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
