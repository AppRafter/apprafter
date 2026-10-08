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
/// Through its [`EnvSource`] a release build reads only `APPRAFTER_CONFIG_DIR`;
/// a test build (walks and CI, cargo feature `test-build` in desktop/) also
/// reads `APPRAFTER_HCLOUD_BASE_URL`, to point the Hetzner API at a loopback
/// mock. When `APPRAFTER_CONFIG_DIR` is unset, the default store root comes
/// from the platform config directory (HOME / XDG_CONFIG_HOME on Unix, via
/// `dirs`), exactly as for the CLI — that is what keeps both clients on the
/// same store.
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

/// What [`is_loopback_http`] accepts, in words, for the refusal message.
const LOOPBACK_HTTP_FORMS: &str =
    "a test build accepts only http://127.0.0.1[:port] or http://[::1][:port], \
     optionally followed by a /path";

/// `http://127.0.0.1` or `http://[::1]`, then nothing or `:` and a port of
/// one to five ASCII digits in 1..=65535, then nothing or a `/` path. No TLS
/// (a mock), no userinfo, no `?` or `#` straight after the authority, and
/// no `localhost`: a name goes through getaddrinfo, which is not bound to
/// answer with a loopback address (a resolver that misses it in /etc/hosts
/// may try the search domains). The walks' mockito server binds 127.0.0.1.
/// The scheme is matched case-sensitively on purpose: the value is ours to
/// write.
fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let Some(tail) = rest
        .strip_prefix("127.0.0.1")
        .or_else(|| rest.strip_prefix("[::1]"))
    else {
        return false;
    };
    let port = tail.find('/').map_or(tail, |path| &tail[..path]);
    port.is_empty() || port.strip_prefix(':').is_some_and(is_port)
}

/// One to five ASCII digits naming a port in 1..=65535.
fn is_port(digits: &str) -> bool {
    (1..=5).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && digits
            .parse::<u32>()
            .is_ok_and(|p| (1..=65535).contains(&p))
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

    /// The desktop's context, read from an allow-list of its environment.
    /// Through `env` it reads only `APPRAFTER_CONFIG_DIR` and, under
    /// [`DesktopPolicy::TEST_BUILD`] only, `APPRAFTER_HCLOUD_BASE_URL`, which
    /// must then be a loopback `http://` URL. The store root is resolved
    /// exactly as the CLI resolves it — `APPRAFTER_CONFIG_DIR`, else the
    /// platform config directory (HOME / XDG_CONFIG_HOME on Unix, via `dirs`) +
    /// `apprafter` — so both clients open the same target store. A release
    /// build never looks at `APPRAFTER_HCLOUD_BASE_URL`, so one exported for
    /// the CLI in the shell it starts from is no error. Every CLI override
    /// (`HCLOUD_TOKEN`, `APPRAFTER_AGE_KEY`, `APPRAFTER_SSH_*`,
    /// `APPRAFTER_SERVER_TYPE`) is ignored: a desktop started from a terminal
    /// inherits them, and every tab would then act on that one provider
    /// project.
    pub fn from_desktop_env(env: &dyn EnvSource, policy: DesktopPolicy) -> CoreResult<Self> {
        let config_root =
            cli_core::target::config_root_from_override(env.var(cli_core::CONFIG_DIR_ENV))?;
        let inherited_base = if policy.loopback_api_base {
            env.var(HCLOUD_BASE_URL_ENV)
        } else {
            None
        };
        let hcloud_base_url = match inherited_base {
            // The refusal names what is accepted, never the value: it may
            // carry userinfo, and the message reaches the UI.
            Some(url) if !is_loopback_http(&url) => {
                return Err(CoreError::UnsafeOverride {
                    var: HCLOUD_BASE_URL_ENV,
                    reason: LOOPBACK_HTTP_FORMS.to_string(),
                });
            }
            Some(url) => url,
            None => cli_providers::hetzner_cloud::DEFAULT_BASE_URL.to_string(),
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
    use std::cell::RefCell;

    use super::*;
    use crate::env::MapEnv;
    use crate::error::UiError;

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

    /// A developer's shell may export the variable for the CLI; a release
    /// desktop started from that shell must still start, on the real API.
    #[test]
    fn a_release_build_ignores_an_inherited_non_loopback_api_base() {
        let env = MapEnv::new().with("APPRAFTER_HCLOUD_BASE_URL", "https://evil.example");
        let ctx = Context::from_desktop_env(&env, DesktopPolicy::RELEASE).unwrap();
        assert_eq!(
            ctx.hcloud_base_url(),
            cli_providers::hetzner_cloud::DEFAULT_BASE_URL
        );
    }

    /// An [`EnvSource`] that records every key read through it.
    struct Recording {
        env: MapEnv,
        keys: RefCell<Vec<String>>,
    }

    impl EnvSource for Recording {
        fn var(&self, key: &str) -> Option<String> {
            self.keys.borrow_mut().push(key.to_string());
            self.env.var(key)
        }
    }

    #[test]
    fn the_desktop_reads_only_its_allow_list() {
        for (policy, allowed) in [
            (DesktopPolicy::RELEASE, vec!["APPRAFTER_CONFIG_DIR"]),
            (
                DesktopPolicy::TEST_BUILD,
                vec!["APPRAFTER_CONFIG_DIR", "APPRAFTER_HCLOUD_BASE_URL"],
            ),
        ] {
            let env = Recording {
                env: ambient_cli_overrides(),
                keys: RefCell::new(Vec::new()),
            };
            Context::from_desktop_env(&env, policy).unwrap();
            let mut read = env.keys.into_inner();
            read.sort();
            read.dedup();
            assert_eq!(read, allowed, "{policy:?}");
        }
    }

    #[test]
    fn a_test_build_honours_a_loopback_api_base_only() {
        for ok in ["http://127.0.0.1:8080", "http://[::1]:1/v1"] {
            let env = MapEnv::new().with("APPRAFTER_HCLOUD_BASE_URL", ok);
            let ctx = Context::from_desktop_env(&env, DesktopPolicy::TEST_BUILD).unwrap();
            assert_eq!(ctx.hcloud_base_url(), ok);
        }
        for bad in [
            "https://127.0.0.1:8080",
            "http://api.hetzner.cloud",
            "http://127.0.0.1.evil.example",
            "http://localhost:9",
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
    fn a_refused_api_base_never_shows_its_value() {
        let env = MapEnv::new().with(
            "APPRAFTER_HCLOUD_BASE_URL",
            "http://user:secret@127.0.0.1:8080",
        );
        let err = Context::from_desktop_env(&env, DesktopPolicy::TEST_BUILD).unwrap_err();
        let ui = UiError::from(&err);
        for shown in [err.to_string(), serde_json::to_string(&ui).unwrap()] {
            assert!(
                !shown.contains("secret") && !shown.contains("user:"),
                "the refused value leaked: {shown}"
            );
        }
        assert!(
            ui.message.contains("http://127.0.0.1[:port]"),
            "the message says what is accepted: {}",
            ui.message
        );
    }

    /// Only the IP literals: `localhost` resolves through getaddrinfo, and a
    /// resolver that misses it in /etc/hosts may try the search domains.
    #[test]
    fn loopback_http_is_a_loopback_ip_literal_over_plain_http() {
        for ok in [
            "http://127.0.0.1",
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/v1",
            "http://[::1]",
            "http://[::1]:1/v1",
        ] {
            assert!(is_loopback_http(ok), "{ok:?} is a loopback http URL");
        }
        for bad in [
            "http://localhost",
            "http://localhost:9",
            "http://localhost/path?q",
            "http://[::1]x",
            "HTTP://127.0.0.1",
        ] {
            assert!(!is_loopback_http(bad), "{bad:?} must be refused");
        }
    }

    /// After the host: nothing, or `:` and a port of one to five digits in
    /// 1..=65535; then nothing, or a path.
    #[test]
    fn loopback_http_takes_only_a_port_and_a_path_after_the_host() {
        for ok in ["http://127.0.0.1:1", "http://127.0.0.1:65535/"] {
            assert!(is_loopback_http(ok), "{ok:?} is a loopback http URL");
        }
        for bad in [
            "http://127.0.0.1:abc",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:65536",
            "http://127.0.0.1:0",
            "http://127.0.0.1:",
            "http://127.0.0.1:+80",
            "http://127.0.0.1:80 .evil",
            "http://[::1]:80]",
            "http://127.0.0.1?x",
            "http://127.0.0.1#x",
            "http://127.0.0.1:80%40evil.com",
        ] {
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
