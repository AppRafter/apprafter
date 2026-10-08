// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The inputs of every core operation (ADR 0067 §2).
//!
//! The CLI builds a [`Context`] once from its environment
//! ([`Context::from_cli_env`]); the desktop builds one from its settings
//! ([`Context::for_desktop`]). Fields arrive with their first consumer:
//! today the target store, the Hetzner API base and the CLI-only token
//! override.

use std::fmt;
use std::path::{Path, PathBuf};

use cli_core::target::TargetStorePaths;

use crate::env::EnvSource;
use crate::error::CoreResult;

/// Points every Hetzner call at another base URL (integration tests).
pub const HCLOUD_BASE_URL_ENV: &str = "APPRAFTER_HCLOUD_BASE_URL";

/// A string whose `Debug` never shows the value.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        SecretString(value.into())
    }

    /// The secret itself — call only where it is used.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

/// Inputs that redirect credentials. Read by the CLI's builder only: a
/// desktop process may inherit `HCLOUD_TOKEN` from the terminal it was
/// started from, and every cluster tab would then act on that one provider
/// project (ADR 0067 §2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOverrides {
    /// `HCLOUD_TOKEN`, when set and non-empty. Outranks the stored token.
    pub hetzner_token: Option<SecretString>,
}

/// Everything a core operation may read besides its own arguments.
#[derive(Debug, Clone)]
pub struct Context {
    config_root: PathBuf,
    hcloud_base_url: String,
    overrides: CliOverrides,
}

impl Context {
    /// The CLI's context, read from its environment exactly as the CLI read
    /// these values before the core existed:
    /// - `APPRAFTER_CONFIG_DIR` (non-empty, verbatim) or the platform config
    ///   dir + `apprafter`;
    /// - `APPRAFTER_HCLOUD_BASE_URL` when set (even empty), else the real API;
    /// - `HCLOUD_TOKEN` when set and non-empty.
    pub fn from_cli_env(env: &dyn EnvSource) -> CoreResult<Self> {
        let config_root =
            cli_core::target::config_root_from_override(env.var(cli_core::CONFIG_DIR_ENV))?;
        let hcloud_base_url = env
            .var(HCLOUD_BASE_URL_ENV)
            .unwrap_or_else(|| cli_providers::hetzner_cloud::DEFAULT_BASE_URL.to_string());
        let hetzner_token = env
            .var(cli_core::HCLOUD_TOKEN_ENV)
            .filter(|t| !t.is_empty())
            .map(SecretString::new);
        Ok(Context {
            config_root,
            hcloud_base_url,
            overrides: CliOverrides { hetzner_token },
        })
    }

    /// The desktop's context: an explicit store root and API base, and no
    /// CLI overrides — whatever the desktop process inherited.
    pub fn for_desktop(config_root: PathBuf, hcloud_base_url: impl Into<String>) -> Self {
        Context {
            config_root,
            hcloud_base_url: hcloud_base_url.into(),
            overrides: CliOverrides::default(),
        }
    }

    pub fn config_root(&self) -> &Path {
        &self.config_root
    }

    /// The target store under [`Context::config_root`].
    pub fn store(&self) -> TargetStorePaths {
        TargetStorePaths::for_root(self.config_root.clone())
    }

    pub fn hcloud_base_url(&self) -> &str {
        &self.hcloud_base_url
    }

    pub fn overrides(&self) -> &CliOverrides {
        &self.overrides
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;

    #[test]
    fn cli_env_reads_store_root_api_base_and_token() {
        let env = MapEnv::new()
            .with("APPRAFTER_CONFIG_DIR", "/tmp/store")
            .with("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:9")
            .with("HCLOUD_TOKEN", "tok");
        let ctx = Context::from_cli_env(&env).unwrap();
        assert_eq!(ctx.config_root(), Path::new("/tmp/store"));
        assert_eq!(ctx.hcloud_base_url(), "http://127.0.0.1:9");
        assert_eq!(
            ctx.overrides().hetzner_token.as_ref().map(|t| t.expose()),
            Some("tok")
        );
    }

    #[test]
    fn cli_env_defaults_without_variables() {
        let ctx = Context::from_cli_env(&MapEnv::new()).unwrap();
        assert!(ctx.config_root().ends_with("apprafter"));
        assert_eq!(
            ctx.hcloud_base_url(),
            cli_providers::hetzner_cloud::DEFAULT_BASE_URL
        );
        assert_eq!(ctx.overrides().hetzner_token, None);
    }

    #[test]
    fn an_empty_token_is_no_override() {
        let ctx = Context::from_cli_env(&MapEnv::new().with("HCLOUD_TOKEN", "")).unwrap();
        assert_eq!(ctx.overrides().hetzner_token, None);
    }

    #[test]
    fn an_empty_api_base_is_kept_as_the_cli_did() {
        let ctx =
            Context::from_cli_env(&MapEnv::new().with("APPRAFTER_HCLOUD_BASE_URL", "")).unwrap();
        assert_eq!(ctx.hcloud_base_url(), "");
    }

    #[test]
    fn the_desktop_context_carries_no_overrides() {
        let ctx = Context::for_desktop(PathBuf::from("/tmp/d"), "https://api.example");
        assert_eq!(ctx.overrides(), &CliOverrides::default());
        assert_eq!(ctx.store().root(), Path::new("/tmp/d"));
    }

    #[test]
    fn a_secret_never_shows_in_debug() {
        let s = SecretString::new("hunter2");
        assert!(!format!("{s:?}").contains("hunter2"));
        let ctx = Context::from_cli_env(&MapEnv::new().with("HCLOUD_TOKEN", "hunter2")).unwrap();
        assert!(!format!("{ctx:?}").contains("hunter2"));
    }
}
