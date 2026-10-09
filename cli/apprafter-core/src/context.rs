// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The inputs of every core operation (ADR 0067 §2).
//!
//! The CLI builds a [`Context`] once from its environment
//! ([`Context::from_cli_env`]). The desktop app builds one from an
//! allow-list of its environment under a [`DesktopPolicy`], plus what only
//! its process knows ([`DesktopHost`]: the tool search path and its runtime
//! dir) ([`Context::from_desktop_env`]); tests that need an explicit store
//! root and API base use [`Context::for_desktop`]. It carries:
//!
//! - the target store root and the Hetzner API base;
//! - the CLI-only overrides ([`CliOverrides`]: `HCLOUD_TOKEN`,
//!   `APPRAFTER_AGE_KEY`, `CUE_BIN`), never read for the desktop;
//! - the home directory (the age key's default, the `~/` of shown paths),
//!   the age key path, the tool search path and where it came from
//!   ([`PathSource`]), and a private runtime dir;
//! - a request timeout, the `no_ping` switch, and the one HTTP agent every
//!   network client is built on (connect, read and write bounded by the
//!   request timeout, names resolved by a [`DeadlineResolver`]).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cli_core::target::TargetStorePaths;
use cli_providers::hetzner_cloud::HetznerCloudClient;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::env::EnvSource;
use crate::error::{CoreError, CoreResult};
use crate::net::DeadlineResolver;

/// Points every Hetzner call at another base URL (integration tests).
pub const HCLOUD_BASE_URL_ENV: &str = "APPRAFTER_HCLOUD_BASE_URL";

/// How long one network request may take: connect, each read and each write, and a name
/// lookup, each bounded by it.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The tool search path variable.
pub const PATH_ENV: &str = "PATH";

/// A string whose `Debug` never shows the value and whose memory is zeroed on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        SecretString(Zeroizing::new(value.into()))
    }

    /// The secret itself — call only where it is used.
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

/// The desktop hands its `Zeroizing` token straight in, without an unzeroed copy.
impl From<Zeroizing<String>> for SecretString {
    fn from(value: Zeroizing<String>) -> Self {
        SecretString(value)
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
    /// `APPRAFTER_AGE_KEY` when set (even empty), as `default_age_key_path` reads it.
    pub age_key: Option<PathBuf>,
    /// `CUE_BIN` when set (even empty), as `cue::cue_bin` reads it.
    pub cue_bin: Option<PathBuf>,
}

/// Where the tool search path came from (shown by the toolchain panel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PathSource {
    /// The client's own `PATH` (the CLI; the desktop on Linux and Windows).
    Environment,
    /// The account's login shell (the desktop on macOS).
    LoginShell,
    /// A fixed default, because the login shell gave none (the desktop on macOS).
    Fallback,
    /// Set by the caller ([`Context::for_desktop`], [`Context::with_tool_search_path`]).
    Explicit,
}

/// What only the desktop process knows when it builds its context.
#[derive(Debug, Clone)]
pub struct DesktopHost {
    /// `<desktop data dir>/run`.
    pub runtime_dir: PathBuf,
    /// The allow-listed `PATH` (Linux and Windows) or the login-shell probe's (macOS).
    pub tool_search_path: OsString,
    pub tool_search_path_source: PathSource,
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
    home_dir: Option<PathBuf>,
    tool_search_path: OsString,
    tool_search_path_source: PathSource,
    age_key_path: PathBuf,
    runtime_dir: PathBuf,
    request_timeout: Duration,
    no_ping: bool,
    /// Rebuilt by [`Context::with_request_timeout`].
    http: ureq::Agent,
}

/// The agent every network client of the core is built on: the whole request (redirects and
/// the body included) bounded by `timeout`, as are connect, each read and each write, and
/// names resolved within it. The overall bound is what stops a server or middlebox that drips
/// a byte at a time: `timeout_read` bounds each read alone, and a blocking request cannot be
/// cancelled.
fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(timeout)
        .timeout_connect(timeout)
        .timeout_read(timeout)
        .timeout_write(timeout)
        .resolver(DeadlineResolver::new(timeout))
        .build()
}

impl Context {
    /// The CLI's context, read from its environment exactly as the CLI read
    /// these values before the core existed:
    /// - `APPRAFTER_CONFIG_DIR` (non-empty, verbatim) or the platform config
    ///   dir + `apprafter`;
    /// - `APPRAFTER_HCLOUD_BASE_URL` when set (even empty), else the real API;
    /// - `HCLOUD_TOKEN` when set and non-empty;
    /// - `APPRAFTER_AGE_KEY` and `CUE_BIN` when set, even empty;
    /// - `PATH` (raw, through [`EnvSource::var_os`]) as the tool search path.
    ///
    /// The home directory comes from `cli_core::paths::home_dir()`; the
    /// runtime dir is `<config_root>/run`.
    pub fn from_cli_env(env: &dyn EnvSource) -> CoreResult<Self> {
        let config_root =
            cli_core::target::config_root_from_override(env.var(cli_core::CONFIG_DIR_ENV))?;
        let hcloud_base_url = env
            .var(HCLOUD_BASE_URL_ENV)
            .unwrap_or_else(|| cli_providers::hetzner_cloud::DEFAULT_BASE_URL.to_string());
        let overrides = CliOverrides {
            hetzner_token: env
                .var(cli_core::HCLOUD_TOKEN_ENV)
                .filter(|t| !t.is_empty())
                .map(SecretString::new),
            age_key: env.var(cli_core::secrets::AGE_KEY_ENV).map(PathBuf::from),
            cue_bin: env.var(cli_core::cue::CUE_BIN_ENV).map(PathBuf::from),
        };
        let home_dir = cli_core::paths::home_dir();
        let runtime_dir = config_root.join("run");
        Ok(Self::assemble(
            config_root,
            hcloud_base_url,
            overrides,
            home_dir,
            env.var_os(PATH_ENV).unwrap_or_default(),
            PathSource::Environment,
            runtime_dir,
        ))
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
    /// (`HCLOUD_TOKEN`, `APPRAFTER_AGE_KEY`, `CUE_BIN`, `APPRAFTER_SSH_*`,
    /// `APPRAFTER_SERVER_TYPE`) is ignored: a desktop started from a terminal
    /// inherits them, and every tab would then act on that one provider
    /// project. The tool search path and the runtime dir come from `host`.
    pub fn from_desktop_env(
        env: &dyn EnvSource,
        policy: DesktopPolicy,
        host: DesktopHost,
    ) -> CoreResult<Self> {
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
        let home_dir = cli_core::paths::home_dir();
        Ok(Self::assemble(
            config_root,
            hcloud_base_url,
            CliOverrides::default(),
            home_dir,
            host.tool_search_path,
            host.tool_search_path_source,
            host.runtime_dir,
        ))
    }

    /// A desktop context from an explicit store root and API base, and no
    /// CLI overrides — whatever the process inherited: no home, an empty
    /// tool search path ([`PathSource::Explicit`]), the runtime dir
    /// `<config_root>/run`. For tests and callers that already hold both;
    /// the app itself uses [`Context::from_desktop_env`].
    pub fn for_desktop(config_root: PathBuf, hcloud_base_url: impl Into<String>) -> Self {
        let runtime_dir = config_root.join("run");
        Self::assemble(
            config_root,
            hcloud_base_url.into(),
            CliOverrides::default(),
            None,
            OsString::new(),
            PathSource::Explicit,
            runtime_dir,
        )
    }

    fn assemble(
        config_root: PathBuf,
        hcloud_base_url: String,
        overrides: CliOverrides,
        home_dir: Option<PathBuf>,
        tool_search_path: OsString,
        tool_search_path_source: PathSource,
        runtime_dir: PathBuf,
    ) -> Self {
        let age_key_path =
            cli_core::secrets::age_key_path_from(overrides.age_key.as_deref(), home_dir.as_deref());
        Context {
            config_root,
            hcloud_base_url,
            overrides,
            home_dir,
            tool_search_path,
            tool_search_path_source,
            age_key_path,
            runtime_dir,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            no_ping: false,
            http: http_agent(DEFAULT_REQUEST_TIMEOUT),
        }
    }

    /// Skip every provider round-trip that only verifies (the CLI's `--no-ping`).
    pub fn with_no_ping(mut self, no_ping: bool) -> Self {
        self.no_ping = no_ping;
        self
    }

    /// The bound of one network request; the HTTP agent is rebuilt with it.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self.http = http_agent(timeout);
        self
    }

    /// The home directory; the age key path follows it unless `APPRAFTER_AGE_KEY` overrides it.
    pub fn with_home_dir(mut self, home: Option<PathBuf>) -> Self {
        self.age_key_path = cli_core::secrets::age_key_path_from(
            self.overrides.age_key.as_deref(),
            home.as_deref(),
        );
        self.home_dir = home;
        self
    }

    pub fn with_tool_search_path(mut self, path: OsString, source: PathSource) -> Self {
        self.tool_search_path = path;
        self.tool_search_path_source = source;
        self
    }

    pub fn with_runtime_dir(mut self, dir: PathBuf) -> Self {
        self.runtime_dir = dir;
        self
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

    pub fn home_dir(&self) -> Option<&Path> {
        self.home_dir.as_deref()
    }

    pub fn tool_search_path(&self) -> &OsStr {
        &self.tool_search_path
    }

    pub fn tool_search_path_source(&self) -> PathSource {
        self.tool_search_path_source
    }

    /// `CUE_BIN` when the CLI has it set (even empty); never for the desktop.
    pub fn cue_override(&self) -> Option<&Path> {
        self.overrides.cue_bin.as_deref()
    }

    pub fn age_key_path(&self) -> &Path {
        &self.age_key_path
    }

    /// The core's private directory for short-lived files (a materialised kubeconfig).
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    pub fn no_ping(&self) -> bool {
        self.no_ping
    }

    pub fn http_agent(&self) -> &ureq::Agent {
        &self.http
    }

    /// The only way the core builds a Hetzner client: this base URL and this agent.
    pub fn hetzner_client(&self, token: &SecretString) -> HetznerCloudClient {
        HetznerCloudClient::with_agent(
            self.hcloud_base_url.clone(),
            token.expose(),
            self.http.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use std::ffi::OsStr;
    use std::time::Duration;

    use super::*;
    use crate::env::MapEnv;
    use crate::error::UiError;

    fn host() -> DesktopHost {
        DesktopHost {
            runtime_dir: "/tmp/desk/run".into(),
            tool_search_path: "/opt/bin".into(),
            tool_search_path_source: PathSource::Explicit,
        }
    }

    #[test]
    fn cli_env_reads_every_cli_input() {
        let env = MapEnv::new()
            .with("APPRAFTER_CONFIG_DIR", "/tmp/store")
            .with("APPRAFTER_AGE_KEY", "/k/age.key")
            .with("CUE_BIN", "/opt/cue")
            .with("PATH", "/a:/b");
        let ctx = Context::from_cli_env(&env).unwrap();
        assert_eq!(ctx.age_key_path(), Path::new("/k/age.key"));
        assert_eq!(ctx.cue_override(), Some(Path::new("/opt/cue")));
        assert_eq!(ctx.tool_search_path(), OsStr::new("/a:/b"));
        assert_eq!(ctx.tool_search_path_source(), PathSource::Environment);
        assert_eq!(ctx.runtime_dir(), Path::new("/tmp/store/run"));
        assert_eq!(ctx.request_timeout(), DEFAULT_REQUEST_TIMEOUT);
        assert!(!ctx.no_ping());
    }

    #[test]
    fn empty_cli_overrides_are_kept_as_the_cli_reads_them() {
        let ctx = Context::from_cli_env(
            &MapEnv::new()
                .with("APPRAFTER_AGE_KEY", "")
                .with("CUE_BIN", ""),
        )
        .unwrap();
        assert_eq!(ctx.age_key_path(), Path::new(""));
        assert_eq!(ctx.cue_override(), Some(Path::new("")));
    }

    #[test]
    fn the_age_key_defaults_under_the_home_unless_overridden() {
        let ctx = Context::for_desktop("/s".into(), "u").with_home_dir(Some("/home/u".into()));
        assert_eq!(
            ctx.age_key_path(),
            Path::new("/home/u/.config/apprafter/age.key")
        );
        assert_eq!(ctx.home_dir(), Some(Path::new("/home/u")));
        let cli = Context::from_cli_env(&MapEnv::new().with("APPRAFTER_AGE_KEY", "/k"))
            .unwrap()
            .with_home_dir(Some("/home/u".into()));
        assert_eq!(cli.age_key_path(), Path::new("/k"));
    }

    #[test]
    fn the_desktop_takes_tool_path_and_runtime_dir_from_its_host() {
        let ctx = Context::from_desktop_env(
            &MapEnv::new()
                .with("PATH", "/evil")
                .with("CUE_BIN", "/evil/cue"),
            DesktopPolicy::RELEASE,
            host(),
        )
        .unwrap();
        assert_eq!(ctx.tool_search_path(), OsStr::new("/opt/bin"));
        assert_eq!(ctx.runtime_dir(), Path::new("/tmp/desk/run"));
        assert_eq!(ctx.cue_override(), None);
    }

    #[test]
    fn for_desktop_has_no_home_and_an_explicit_empty_path() {
        let ctx = Context::for_desktop("/s".into(), "u");
        assert_eq!(
            (ctx.home_dir(), ctx.tool_search_path_source()),
            (None, PathSource::Explicit)
        );
        assert!(ctx.tool_search_path().is_empty());
        assert_eq!(ctx.runtime_dir(), Path::new("/s/run"));
    }

    #[test]
    fn the_http_agent_gives_up_on_a_silent_server_at_the_request_timeout() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", silent.local_addr().unwrap());
        let ctx =
            Context::for_desktop("/s".into(), url).with_request_timeout(Duration::from_millis(300));
        let client = ctx.hetzner_client(&SecretString::new("t"));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(client.list_locations().is_err());
        });
        assert!(rx
            .recv_timeout(Duration::from_secs(5))
            .expect("bounded by the request timeout"));
        drop(silent);
    }

    /// Review finding 4: `timeout_read` bounds each read, so a server that keeps dripping
    /// bytes would hold a request for as long as it likes. The request as a whole is bounded.
    #[test]
    fn the_http_agent_gives_up_on_a_dripping_server_at_the_request_timeout() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((mut conn, _)) = listener.accept() else {
                return;
            };
            // One byte of a never-ending header every 500 ms, for 20 s: every read is answered
            // well inside the per-read timeout.
            let head = b"HTTP/1.1 200 OK\r\nX-Drip: ";
            for byte in head.iter().chain(std::iter::repeat(&b'a')).take(40) {
                if conn.write_all(&[*byte]).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        let timeout = Duration::from_secs(1);
        let ctx = Context::for_desktop("/s".into(), url).with_request_timeout(timeout);
        let client = ctx.hetzner_client(&SecretString::new("t"));
        let started = std::time::Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(client.list_locations().is_err());
        });
        let failed = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the dripping request outlived its timeout");
        let took = started.elapsed();
        assert!(failed);
        assert!(took < timeout + Duration::from_millis(1500), "{took:?}");
    }

    /// Review finding 6: the agent resolves every host through [`DeadlineResolver`] (ureq's
    /// default, `to_socket_addrs`, has no bound). ureq resolves on the requesting thread, an IP
    /// literal included, so a fresh thread's count is this request's.
    #[test]
    fn the_http_agent_resolves_through_the_deadline_resolver() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let ctx = Context::for_desktop("/s".into(), format!("http://{closed}"))
            .with_request_timeout(Duration::from_millis(500));
        let client = ctx.hetzner_client(&SecretString::new("t"));
        let lookups = std::thread::spawn(move || {
            let _ = client.list_locations();
            crate::net::LOOKUPS_ON_THIS_THREAD.with(std::cell::Cell::get)
        })
        .join()
        .unwrap();
        assert!(lookups >= 1, "the request never asked the DeadlineResolver");
    }

    #[test]
    fn a_secret_handed_over_as_zeroizing_keeps_its_value() {
        let s = SecretString::from(zeroize::Zeroizing::new("tok".to_string()));
        assert_eq!(s.expose(), "tok");
        assert!(!format!("{s:?}").contains("tok"));
    }

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
            Context::from_desktop_env(&ambient_cli_overrides(), DesktopPolicy::RELEASE, host())
                .unwrap();
        assert_eq!(ctx.config_root(), Path::new("/tmp/store"));
        assert_eq!(ctx.overrides(), &CliOverrides::default());
        assert_eq!(
            ctx.hcloud_base_url(),
            cli_providers::hetzner_cloud::DEFAULT_BASE_URL,
            "a release desktop never redirects the provider API"
        );
        assert_ne!(
            ctx.age_key_path(),
            Path::new("/tmp/age.key"),
            "the inherited APPRAFTER_AGE_KEY is the CLI's"
        );
    }

    #[test]
    fn the_desktop_store_root_defaults_like_the_cli() {
        let desk =
            Context::from_desktop_env(&MapEnv::new(), DesktopPolicy::RELEASE, host()).unwrap();
        let cli = Context::from_cli_env(&MapEnv::new()).unwrap();
        assert_eq!(desk.config_root(), cli.config_root());
    }

    /// A developer's shell may export the variable for the CLI; a release
    /// desktop started from that shell must still start, on the real API.
    #[test]
    fn a_release_build_ignores_an_inherited_non_loopback_api_base() {
        let env = MapEnv::new().with("APPRAFTER_HCLOUD_BASE_URL", "https://evil.example");
        let ctx = Context::from_desktop_env(&env, DesktopPolicy::RELEASE, host()).unwrap();
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
            Context::from_desktop_env(&env, policy, host()).unwrap();
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
            let ctx = Context::from_desktop_env(&env, DesktopPolicy::TEST_BUILD, host()).unwrap();
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
            let err =
                Context::from_desktop_env(&env, DesktopPolicy::TEST_BUILD, host()).unwrap_err();
            assert!(
                matches!(err, CoreError::UnsafeOverride { var, .. } if var == HCLOUD_BASE_URL_ENV),
                "{bad:?} must be refused, got {err:?}"
            );
        }
        let unset =
            Context::from_desktop_env(&MapEnv::new(), DesktopPolicy::TEST_BUILD, host()).unwrap();
        assert_eq!(
            unset.hcloud_base_url(),
            cli_providers::hetzner_cloud::DEFAULT_BASE_URL
        );
    }

    #[test]
    fn a_test_build_still_ignores_the_token_override() {
        let ctx =
            Context::from_desktop_env(&ambient_cli_overrides(), DesktopPolicy::TEST_BUILD, host())
                .unwrap();
        assert_eq!(ctx.overrides(), &CliOverrides::default());
    }

    #[test]
    fn a_refused_api_base_never_shows_its_value() {
        let env = MapEnv::new().with(
            "APPRAFTER_HCLOUD_BASE_URL",
            "http://user:secret@127.0.0.1:8080",
        );
        let err = Context::from_desktop_env(&env, DesktopPolicy::TEST_BUILD, host()).unwrap_err();
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
