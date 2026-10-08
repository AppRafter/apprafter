// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The inputs of every core operation (ADR 0067 §2).
//!
//! The CLI builds a [`Context`] once from its environment
//! ([`Context::from_cli_env`]). The desktop app builds one from an
//! allow-list of its environment under a [`DesktopPolicy`]
//! ([`Context::from_desktop_env`]); tests that need an explicit store root
//! and API base use [`Context::for_desktop`]. Fields arrive with their first
//! consumer: today the target store, the Hetzner API base and the CLI-only
//! token override.

use std::fmt;
use std::path::{Path, PathBuf};

use cli_core::target::TargetStorePaths;

use crate::env::EnvSource;
use crate::error::{CoreError, CoreResult};

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

/// What the desktop allows itself to read from its environment (ADR 0067 §2).
///
/// A release build reads `APPRAFTER_CONFIG_DIR` and nothing else. A test build
/// (walks and CI, cargo feature `test-build` in desktop/) may also point the
/// Hetzner API at a loopback mock through `APPRAFTER_HCLOUD_BASE_URL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopPolicy {
    pub loopback_api_base: bool,
}

impl DesktopPolicy {
    pub const RELEASE: DesktopPolicy = DesktopPolicy {
        loopback_api_base: false,
    };
    pub const TEST_BUILD: DesktopPolicy = DesktopPolicy {
        loopback_api_base: true,
    };
}

/// `http://` to a loopback host and nothing else: no TLS (a mock), no
/// userinfo, no host that merely starts with a loopback name. The scheme is
/// matched case-sensitively on purpose: the value is ours to write.
fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        match v6.split_once(']') {
            Some((h, tail)) if tail.is_empty() || tail.starts_with(':') => h,
            _ => return false,
        }
    } else {
        authority.split(':').next().unwrap_or("")
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
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

    /// The desktop's context, read from an allow-list of its environment:
    /// `APPRAFTER_CONFIG_DIR` (resolved exactly as the CLI resolves it, so both
    /// clients open the same target store), and — under
    /// [`DesktopPolicy::TEST_BUILD`] only — a loopback `APPRAFTER_HCLOUD_BASE_URL`.
    /// Every CLI override (`HCLOUD_TOKEN`, `APPRAFTER_AGE_KEY`,
    /// `APPRAFTER_SSH_*`, `APPRAFTER_SERVER_TYPE`) is ignored: a desktop started
    /// from a terminal inherits them, and every tab would then act on that one
    /// provider project.
    pub fn from_desktop_env(env: &dyn EnvSource, policy: DesktopPolicy) -> CoreResult<Self> {
        let config_root =
            cli_core::target::config_root_from_override(env.var(cli_core::CONFIG_DIR_ENV))?;
        let hcloud_base_url = match env.var(HCLOUD_BASE_URL_ENV) {
            Some(url) if policy.loopback_api_base => {
                if !is_loopback_http(&url) {
                    return Err(CoreError::UnsafeOverride {
                        var: HCLOUD_BASE_URL_ENV,
                        reason: format!(
                            "a test build accepts only a loopback http:// URL, got {url:?}"
                        ),
                    });
                }
                url
            }
            _ => cli_providers::hetzner_cloud::DEFAULT_BASE_URL.to_string(),
        };
        Ok(Context {
            config_root,
            hcloud_base_url,
            overrides: CliOverrides::default(),
        })
    }

    /// A desktop context from an explicit store root and API base, and no
    /// CLI overrides — whatever the process inherited. For tests and callers
    /// that already hold both; the app itself uses
    /// [`Context::from_desktop_env`].
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

    fn ambient_cli_overrides() -> MapEnv {
        MapEnv::new()
            .with("APPRAFTER_CONFIG_DIR", "/tmp/store")
            .with("HCLOUD_TOKEN", "inherited-token")
            .with("APPRAFTER_AGE_KEY", "/tmp/age.key")
            .with("APPRAFTER_SSH_PUBLIC_KEY", "ssh-ed25519 AAAA x")
            .with("APPRAFTER_SSH_PUBLIC_KEY_PATH", "/tmp/id.pub")
            .with("APPRAFTER_SSH_PRIVATE_KEY", "/tmp/id")
            .with("APPRAFTER_SERVER_TYPE", "cx99")
            .with("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:9")
    }

    #[test]
    fn the_desktop_ignores_every_cli_override_it_inherits() {
        let ctx =
            Context::from_desktop_env(&ambient_cli_overrides(), DesktopPolicy::RELEASE).unwrap();
        assert_eq!(ctx.config_root(), Path::new("/tmp/store"));
        assert_eq!(ctx.overrides(), &CliOverrides::default());
        assert_eq!(
            ctx.hcloud_base_url(),
            cli_providers::hetzner_cloud::DEFAULT_BASE_URL,
            "a release desktop never redirects the provider API"
        );
    }

    #[test]
    fn the_desktop_store_root_defaults_like_the_cli() {
        let desk = Context::from_desktop_env(&MapEnv::new(), DesktopPolicy::RELEASE).unwrap();
        let cli = Context::from_cli_env(&MapEnv::new()).unwrap();
        assert_eq!(desk.config_root(), cli.config_root());
    }

    #[test]
    fn a_test_build_honours_a_loopback_api_base_only() {
        for ok in [
            "http://127.0.0.1:8080",
            "http://localhost:9",
            "http://[::1]:1/v1",
        ] {
            let env = MapEnv::new().with("APPRAFTER_HCLOUD_BASE_URL", ok);
            let ctx = Context::from_desktop_env(&env, DesktopPolicy::TEST_BUILD).unwrap();
            assert_eq!(ctx.hcloud_base_url(), ok);
        }
        for bad in [
            "https://127.0.0.1:8080",
            "http://api.hetzner.cloud",
            "http://127.0.0.1.evil.example",
            "http://localhost.evil.example:80",
            "http://user@127.0.0.1",
            "",
        ] {
            let env = MapEnv::new().with("APPRAFTER_HCLOUD_BASE_URL", bad);
            let err = Context::from_desktop_env(&env, DesktopPolicy::TEST_BUILD).unwrap_err();
            assert!(
                matches!(err, CoreError::UnsafeOverride { var, .. } if var == HCLOUD_BASE_URL_ENV),
                "{bad:?} must be refused, got {err:?}"
            );
        }
        let unset = Context::from_desktop_env(&MapEnv::new(), DesktopPolicy::TEST_BUILD).unwrap();
        assert_eq!(
            unset.hcloud_base_url(),
            cli_providers::hetzner_cloud::DEFAULT_BASE_URL
        );
    }

    #[test]
    fn a_test_build_still_ignores_the_token_override() {
        let ctx =
            Context::from_desktop_env(&ambient_cli_overrides(), DesktopPolicy::TEST_BUILD).unwrap();
        assert_eq!(ctx.overrides(), &CliOverrides::default());
    }

    #[test]
    fn loopback_http_is_exactly_a_loopback_host_over_plain_http() {
        for ok in [
            "http://127.0.0.1",
            "http://[::1]",
            "http://localhost/path?q",
        ] {
            assert!(is_loopback_http(ok), "{ok:?} is a loopback http URL");
        }
        for bad in ["http://[::1]x", "HTTP://127.0.0.1"] {
            assert!(!is_loopback_http(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_secret_never_shows_in_debug() {
        let s = SecretString::new("hunter2");
        assert!(!format!("{s:?}").contains("hunter2"));
        let ctx = Context::from_cli_env(&MapEnv::new().with("HCLOUD_TOKEN", "hunter2")).unwrap();
        assert!(!format!("{ctx:?}").contains("hunter2"));
    }
}
