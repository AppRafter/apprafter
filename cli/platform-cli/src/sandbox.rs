// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `APPRAFTER_SANDBOX`: a fence a caller puts around the CLI (ADR 0068 §9; D.4a T5).
//!
//! When the variable is set — one or more directories, separated as in `PATH`; set to an empty
//! value counts — every command refuses to run, after its arguments are parsed and before
//! anything reads or writes the target store, if a location it would use resolves outside all
//! of them. It only refuses: it moves no location, so a command it lets through runs exactly as
//! without it. The test suite (`tests/common/spawn.rs`), the e2e walks (`e2e/lib.sh`) and
//! automated agents set it, so a variable they forgot, a removed `HOME` (the password database
//! answers instead), an exported `XDG_CONFIG_HOME` or a symlink out of the scratch area is
//! refused instead of reaching a real store.
//!
//! The locations: the target store (as the core and the legacy resolver find it), the home
//! directory on Linux and macOS, the age key (both resolvers), the SSH identity and the runtime
//! directory. Windows resolves the profile through a Known Folder no variable moves, so there
//! the fence checks what the profile would otherwise supply — the age key and the SSH
//! identity, which a caller names explicitly. D.4a adds `store.yaml` and the vault (c2) and
//! the holder's base (e); on a store whose `store.yaml` says locked, locking or unlocking the
//! age key is not fenced (d: the variable is refused there, and nothing reads the shared key).
//!
//! A location is inside a root when the canonical form of its deepest existing ancestor, with
//! the rest appended, starts with the root's canonical form (macOS `/tmp` is `/private/tmp`;
//! Windows canonical paths are `\\?\` on both sides). The fence fails closed: a relative
//! location, a `..` in one, an ancestor it cannot read, a root that is relative or no
//! directory, a missing home on Unix and a context that cannot be built are refusals.
//!
//! Every read goes through [`EnvSource`] or a resolver that already reads its own variable, so
//! the env-read ratchet (`apprafter-core/tests/guards.rs`) does not move.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use apprafter_core::{Context, EnvSource};

/// The variable (module docs).
pub(crate) const SANDBOX_ENV: &str = "APPRAFTER_SANDBOX";

/// What the fence checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Location {
    ConfigRoot,
    Home,
    AgeKey,
    SshIdentity,
    RuntimeDir,
    /// `<store>/store.yaml`, the store's lock state (D.4a c2).
    StoreFile,
    /// `<store>/vault` (D.4a c2).
    VaultFile,
}

impl Location {
    /// The location as a refusal names it, with the variable that moves it.
    pub(crate) fn what(self) -> &'static str {
        match self {
            Location::ConfigRoot => "the target store (APPRAFTER_CONFIG_DIR)",
            Location::Home => "the home directory (HOME)",
            Location::AgeKey => "the age key (APPRAFTER_AGE_KEY)",
            Location::SshIdentity => "the SSH identity (APPRAFTER_SSH_PRIVATE_KEY)",
            Location::RuntimeDir => "the runtime directory (<target store>/run)",
            Location::StoreFile => "the target store's lock state (<target store>/store.yaml)",
            Location::VaultFile => "the target store's vault (<target store>/vault)",
        }
    }
}

/// A location outside every root of `APPRAFTER_SANDBOX`.
#[derive(Debug, miette::Diagnostic)]
#[diagnostic(
    code(apprafter::sandbox::escape),
    help(
        "set the variable that moves it to a path inside one of the APPRAFTER_SANDBOX \
         directories, or unset APPRAFTER_SANDBOX"
    )
)]
pub(crate) struct SandboxEscape {
    what: &'static str,
    path: String,
}

impl fmt::Display for SandboxEscape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} resolves to {}, outside {SANDBOX_ENV}",
            self.what, self.path
        )
    }
}

impl std::error::Error for SandboxEscape {}

/// A root of `APPRAFTER_SANDBOX` that is no existing directory given by an absolute path.
#[derive(Debug, miette::Diagnostic)]
#[diagnostic(
    code(apprafter::sandbox::bad_root),
    help("list existing directories by absolute path, separated as in PATH")
)]
pub(crate) struct SandboxRootRefused {
    root: String,
}

impl fmt::Display for SandboxRootRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{SANDBOX_ENV} names {:?}, which is not an existing directory given by an absolute path",
            self.root
        )
    }
}

impl std::error::Error for SandboxRootRefused {}

/// Refuse to go on when `APPRAFTER_SANDBOX` is set and a location resolves outside it (module
/// docs). Called once, by `run()`, between argument parsing and the startup checks.
pub(crate) fn enforce(env: &dyn EnvSource) -> miette::Result<()> {
    let Some(value) = env.var_os(SANDBOX_ENV) else {
        return Ok(());
    };
    let roots = roots(&value)?;
    let ctx = Context::from_cli_env(env).map_err(crate::render::core_error::report)?;
    let all = locations(&ctx)?;
    for (location, path) in all.iter().filter(|(l, _)| *l != Location::AgeKey) {
        check(*location, path, &roots)?;
    }
    // Only now, with the store and its lock state inside the fence, is that state read (T5).
    let mode = apprafter_core::secrets::read_store_file(ctx.config_root())
        .ok()
        .flatten()
        .map(|f| f.secrets);
    if fences_the_age_key(mode) {
        for (location, path) in all.iter().filter(|(l, _)| *l == Location::AgeKey) {
            check(*location, path, &roots)?;
        }
    }
    Ok(())
}

/// D.4a T5: whether the age key is fenced, for a store in `mode` (`None`: no lock state, or
/// one that cannot be read — fenced). On a locked, locking or unlocking store nothing reads the
/// shared key (`APPRAFTER_AGE_KEY` is refused, T35), so it is not; d4a-d's `lock on` and
/// `lock off` read or write it and fence it whatever the state (Task 9 of its plan).
pub(crate) fn fences_the_age_key(mode: Option<apprafter_core::secrets::SecretsMode>) -> bool {
    use apprafter_core::secrets::SecretsMode;
    !matches!(
        mode,
        Some(SecretsMode::Locked | SecretsMode::Locking | SecretsMode::Unlocking)
    )
}

/// The canonical roots `value` lists; a refusal for one that is relative or no directory.
fn roots(value: &OsStr) -> miette::Result<Vec<PathBuf>> {
    std::env::split_paths(value)
        .map(|root| {
            root.is_absolute()
                .then(|| std::fs::canonicalize(&root).ok())
                .flatten()
                .filter(|canonical| canonical.is_dir())
                .ok_or_else(|| {
                    miette::Report::new(SandboxRootRefused {
                        root: root.display().to_string(),
                    })
                })
        })
        .collect()
}

/// Every location a command may use, where it resolves.
fn locations(ctx: &Context) -> miette::Result<Vec<(Location, PathBuf)>> {
    let mut out = vec![(Location::ConfigRoot, ctx.config_root().to_path_buf())];
    // The resolvers the commands outside the core still call read the same variables; a drift
    // between the two would itself be an escape.
    out.push((
        Location::ConfigRoot,
        cli_core::target::default_config_root().map_err(miette::Report::new)?,
    ));
    if cfg!(unix) {
        match ctx.home_dir() {
            Some(home) => out.push((Location::Home, home.to_path_buf())),
            None => {
                return Err(SandboxEscape {
                    what: Location::Home.what(),
                    path: "(none)".into(),
                }
                .into())
            }
        }
    }
    out.push((Location::AgeKey, ctx.age_key_path().to_path_buf()));
    out.push((
        Location::SshIdentity,
        cli_providers::hetzner_cloud::default_ssh_identity_path(),
    ));
    out.push((Location::RuntimeDir, ctx.runtime_dir().to_path_buf()));
    // A symlink from either out of the sandbox would be read or written outside it (D.4a c2).
    out.push((
        Location::StoreFile,
        ctx.config_root().join(apprafter_core::secrets::STORE_FILE),
    ));
    out.push((
        Location::VaultFile,
        ctx.config_root().join(apprafter_core::vault::VAULT_FILE),
    ));
    Ok(out)
}

/// Refuse `path` unless it resolves inside a root.
fn check(location: Location, path: &Path, roots: &[PathBuf]) -> miette::Result<()> {
    match resolve(path) {
        Some(resolved) if roots.iter().any(|root| resolved.starts_with(root)) => Ok(()),
        resolved => Err(SandboxEscape {
            what: location.what(),
            path: resolved
                .unwrap_or_else(|| path.to_path_buf())
                .display()
                .to_string(),
        }
        .into()),
    }
}

/// `path` with its deepest existing ancestor canonicalised and the rest appended as written;
/// `None` for a relative path, one with `..`, or one whose ancestor cannot be read.
fn resolve(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    let mut tail = Vec::new();
    let mut at = path;
    loop {
        match std::fs::canonicalize(at) {
            Ok(canonical) => {
                let mut out = canonical;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return Some(out);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tail.push(at.file_name()?.to_os_string());
                at = at.parent()?;
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use miette::Diagnostic;

    use super::*;

    #[test]
    fn a_missing_tail_resolves_under_its_deepest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        assert_eq!(resolve(dir.path()), Some(canonical.clone()));
        assert_eq!(
            resolve(&dir.path().join("a").join("b")),
            Some(canonical.join("a").join("b"))
        );
    }

    #[test]
    fn a_relative_path_or_a_parent_component_does_not_resolve() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve(&Path::new("relative").join("x")), None);
        assert_eq!(
            resolve(&dir.path().join("missing").join("..").join("..").join("etc")),
            None
        );
        assert_eq!(resolve(&dir.path().join("..")), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_resolves_to_where_it_points() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = inside.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let roots = [inside.path().canonicalize().unwrap()];
        assert!(check(Location::ConfigRoot, &link.join("store"), &roots).is_err());
        assert!(check(Location::ConfigRoot, &inside.path().join("store"), &roots).is_ok());
    }

    #[test]
    fn a_root_must_be_an_existing_absolute_directory() {
        let dir = tempfile::tempdir().unwrap();
        let list = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(
            roots(&list).unwrap(),
            vec![dir.path().canonicalize().unwrap()]
        );
        let missing = dir.path().join("missing");
        // `src` exists where cargo runs a package's tests (its root), so only the absolute-path
        // check refuses it; `relative` exists nowhere.
        assert!(
            Path::new("src").is_dir(),
            "the unit tests run from the package root"
        );
        for bad in [
            OsStr::new(""),
            OsStr::new("relative"),
            OsStr::new("src"),
            missing.as_os_str(),
        ] {
            let err = roots(bad).unwrap_err();
            assert_eq!(
                err.code().map(|c| c.to_string()).as_deref(),
                Some("apprafter::sandbox::bad_root"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_refusal_names_the_location_the_path_and_the_variable() {
        let e = SandboxEscape {
            what: Location::AgeKey.what(),
            path: "/home/me/.config/apprafter/age.key".into(),
        };
        assert_eq!(
            e.to_string(),
            "the age key (APPRAFTER_AGE_KEY) resolves to /home/me/.config/apprafter/age.key, \
             outside APPRAFTER_SANDBOX"
        );
        assert_eq!(
            e.code().map(|c| c.to_string()).as_deref(),
            Some("apprafter::sandbox::escape")
        );
    }

    /// T5: the age key is fenced unless the store's lock state says the shared key is never
    /// read; a lock state that cannot be read is fenced.
    #[test]
    fn the_age_key_is_skipped_only_on_a_locked_locking_or_unlocking_store() {
        use apprafter_core::secrets::SecretsMode;
        assert!(fences_the_age_key(None));
        assert!(fences_the_age_key(Some(SecretsMode::Files)));
        for mode in [
            SecretsMode::Locked,
            SecretsMode::Locking,
            SecretsMode::Unlocking,
        ] {
            assert!(!fences_the_age_key(Some(mode)), "{mode:?}");
        }
    }
}
