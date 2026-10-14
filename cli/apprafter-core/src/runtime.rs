// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The core's private runtime directory (spec §4.1, overview R9): where a decrypted kubeconfig
//! lives while a probe uses it, and nowhere else.
//!
//! `Context::runtime_dir()` is `<config root>/run` for the CLI (on Windows with no store
//! variable, `%LOCALAPPDATA%\apprafter\run\<tag>`) and `<desktop local data dir>/run` for
//! the desktop, so one client never sweeps the other's live file. The directory and its files
//! are owner-only ([`cli_core::owner_only`], WI-472), so a store in a folder other users can
//! read keeps them out: on Unix the directory is 0700 and each file 0600, created with
//! `create_new`; on Windows the directory has a protected DACL whose one ACE, full access for
//! this account, its files inherit, and each file gets its own protected one-ACE DACL on its
//! handle before its first byte. A [`MaterialisedKubeconfig`] removes its file when it drops;
//! [`sweep_stale`] removes what a crashed process left behind — by its owner, never by
//! its age (D.4a T44): a copy's name records its writer's pid and start as the OS reports it,
//! and the copy goes only when that pid is gone or now belongs to a process with another start,
//! so a long session's copy (`top`, `open argocd`, `app logs -f`) stays however old it is, and
//! a clock step changes nothing. Reads here are lockless: `state.json` is replaced atomically
//! (overview R3).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cli_state::{State, StatePaths};

use crate::context::Context;
use crate::error::{CoreError, CoreResult};
use crate::target_ref::TargetRef;

const PREFIX: &str = "kubeconfig-";
/// Fresh names to try when a crashed process with a reused pid left one behind.
const NAME_ATTEMPTS: u32 = 16;
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A decrypted kubeconfig on disk, readable only by this account; dropping it removes the file.
#[derive(Debug)]
pub struct MaterialisedKubeconfig {
    path: PathBuf,
}

impl MaterialisedKubeconfig {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MaterialisedKubeconfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The target's cached kubeconfig, written to `<runtime_dir>/kubeconfig-<pid>-<start>-<n>.yaml`.
///
/// `None` when the state records no server or caches no kubeconfig. The secret store opens
/// it: the encrypted slot wins over the legacy plaintext one (a plaintext copy beside it may
/// be stale), the age identity is read WITHOUT creating one — a missing key is
/// [`CoreError::AgeKeyMissing`], never a new key — and a key that does not open it is
/// `CliError::CacheUndecryptable` (as [`CoreError::Cli`]) with the target, so a caller names
/// the refetch (WI-457 review #8).
pub fn materialise_kubeconfig(
    ctx: &Context,
    target: &TargetRef,
) -> CoreResult<Option<MaterialisedKubeconfig>> {
    let paths = StatePaths::for_active_target(&ctx.store(), target.name());
    let state = State::load_or_default(&paths)?;
    let Some(server) = state.hetzner_cloud else {
        return Ok(None);
    };
    let Some(body) = ctx.secrets().cached_kubeconfig(&server, target.name())? else {
        return Ok(None);
    };
    materialise(ctx, &body).map(Some)
}

/// `body`, a decrypted kubeconfig, written to `<runtime_dir>/kubeconfig-<pid>-<start>-<n>.yaml`
/// (owner-only, in an owner-only dir), after [`sweep_stale`] — best effort: a sweep that fails
/// never stops the write. The one materialiser of the CLI and the core (D.4a T44).
pub fn materialise(ctx: &Context, body: &str) -> CoreResult<MaterialisedKubeconfig> {
    let _ = sweep_stale(ctx);
    write_private(ctx.runtime_dir(), body.as_bytes())
}

/// Whether a copy is a leftover, from what the OS says of the pid in its name: its process is
/// gone, or the live one has another start than the one the name `recorded` (a reused pid).
/// Unknown is never a leftover; neither is a copy that recorded no start while its pid lives;
/// age and the clock never count.
fn stale(
    alive: Option<bool>,
    now: Option<crate::process::ProcStart>,
    recorded: Option<u64>,
) -> bool {
    match alive {
        Some(false) => true,
        Some(true) => match (now, recorded) {
            (Some(now), Some(recorded)) => now.raw() != recorded,
            _ => false,
        },
        None => false,
    }
}

/// Remove the `kubeconfig-<pid>-<start>-<n>.yaml` copies in the runtime dir whose writer is
/// gone (T44: by owner, never by age); returns how many. A copy named without a start (an
/// older AppRafter's, or an OS that did not say) goes only when its pid is gone. A missing
/// dir is zero and is not created; a name with no pid, a link and anything else are not ours
/// to judge.
pub fn sweep_stale(ctx: &Context) -> CoreResult<usize> {
    let entries = match fs::read_dir(ctx.runtime_dir()) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(io_error(e)),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(parts) = name
            .strip_prefix(PREFIX)
            .and_then(|rest| rest.strip_suffix(".yaml"))
            .map(|rest| rest.split('-').collect::<Vec<_>>())
        else {
            continue;
        };
        let (pid, recorded) = match parts.as_slice() {
            [pid, start, _n] => (pid.parse::<u32>().ok(), start.parse::<u64>().ok()),
            [pid, _n] => (pid.parse::<u32>().ok(), None),
            _ => continue,
        };
        let Some(pid) = pid else {
            continue;
        };
        // DirEntry::metadata does not follow a symlink: a link is never ours, never removed.
        let meta = entry.metadata().map_err(io_error)?;
        if !meta.is_file()
            || !stale(
                crate::process::alive(pid),
                crate::process::started(pid),
                recorded,
            )
        {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(e)),
        }
    }
    Ok(removed)
}

fn write_private(dir: &Path, bytes: &[u8]) -> CoreResult<MaterialisedKubeconfig> {
    ensure_private_dir(dir)?;
    let pid = std::process::id();
    let start = crate::process::started(pid).map(crate::process::ProcStart::raw);
    for _ in 0..NAME_ATTEMPTS {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        // D.4a T44: the writer's pid and start, as the sweep reads them back; no start (an OS
        // that does not say) is the name without one, kept while the pid lives.
        let path = dir.join(match start {
            Some(start) => format!("{PREFIX}{pid}-{start}-{n}.yaml"),
            None => format!("{PREFIX}{pid}-{n}.yaml"),
        });
        match cli_core::owner_only::create_new(&path) {
            Ok(mut file) => {
                #[cfg(all(test, windows))]
                cli_core::owner_only::testing::record_before_first_byte(&file);
                let written = file.write_all(bytes);
                // Close before the guard can remove it: Windows refuses to delete an open file.
                drop(file);
                let guard = MaterialisedKubeconfig { path };
                written.map_err(io_error)?;
                return Ok(guard);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(e)),
        }
    }
    Err(io_error(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("no free kubeconfig file name in {}", dir.display()),
    )))
}

/// Create `dir` owner-only, or tighten an existing one — 0700 on Unix, the protected DACL whose
/// one ACE its files inherit on Windows (WI-472) — and refuse anything that is not a plain
/// directory (a symlink would put the decrypted file somewhere else). On Windows the DACL is set
/// just after the directory is made; each file is restricted on its own handle as well, so a
/// file made in that moment is no one else's to read.
fn ensure_private_dir(dir: &Path) -> CoreResult<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(io_error(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a directory", dir.display()),
            )))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            // Built per cfg: `create` takes `&self`, so a `let mut` that only Unix mutates is
            // `unused_mut` on the Windows clippy leg.
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut b = fs::DirBuilder::new();
                b.mode(0o700);
                b
            };
            #[cfg(not(unix))]
            let builder = fs::DirBuilder::new();
            match builder.create(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_error(e)),
            }
        }
        Err(e) => return Err(io_error(e)),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    #[cfg(windows)]
    cli_core::owner_only::restrict_dir(dir).map_err(io_error)?;
    Ok(())
}

fn io_error(e: io::Error) -> CoreError {
    CoreError::from(cli_core::CliError::Io(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cli_core::target::{Target, TargetConfig, TargetCredentials};
    use cli_state::HetznerCloudState;
    use std::time::{Duration, SystemTime};

    const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";

    struct Store {
        _dir: tempfile::TempDir,
        ctx: Context,
        target: TargetRef,
    }

    /// A store holding target `prod` (state as given) and a context whose age key lives under
    /// the scratch home.
    fn store(server: Option<serde_json::Value>) -> Store {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().join("config"), "http://127.0.0.1:1")
            .with_home_dir(Some(dir.path().join("home")));
        let t = Target {
            name: "prod".into(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                ..Default::default()
            },
            credentials: TargetCredentials::default(),
        };
        cli_core::save_target(&ctx.store(), &t).unwrap();
        if let Some(server) = server {
            seed(&ctx, server);
        }
        let target = TargetRef::named(&ctx, "prod").unwrap();
        Store {
            _dir: dir,
            ctx,
            target,
        }
    }

    fn seed(ctx: &Context, server: serde_json::Value) {
        let hetzner: HetznerCloudState = serde_json::from_value(server).unwrap();
        State {
            hetzner_cloud: Some(hetzner),
            ..Default::default()
        }
        .save(&StatePaths::for_active_target(&ctx.store(), "prod"))
        .unwrap();
    }

    /// The age key at the context's key path, and `YAML` encrypted to it.
    fn encrypted(ctx: &Context) -> String {
        let id = cli_core::secrets::load_or_create_identity(ctx.age_key_path()).unwrap();
        cli_core::secrets::encrypt_for_recipient(YAML, &id.to_public()).unwrap()
    }

    fn kubeconfig_files(ctx: &Context) -> Vec<String> {
        match fs::read_dir(ctx.runtime_dir()) {
            Ok(rd) => rd
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(PREFIX))
                .collect(),
            Err(_) => vec![],
        }
    }

    fn aged(path: &Path, age: Duration) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
    }

    fn plain() -> serde_json::Value {
        serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_yaml": YAML})
    }

    #[test]
    fn nothing_cached_is_none_and_writes_nothing() {
        let s = store(None);
        assert!(materialise_kubeconfig(&s.ctx, &s.target).unwrap().is_none());
        let s = store(Some(
            serde_json::json!({"server_id": 7, "server_name": "n"}),
        ));
        assert!(materialise_kubeconfig(&s.ctx, &s.target).unwrap().is_none());
        assert!(
            !s.ctx.runtime_dir().exists(),
            "no runtime dir without a kubeconfig to write"
        );
    }

    #[test]
    fn the_age_slot_is_decrypted_into_the_runtime_dir_and_drop_removes_it() {
        let s = store(None);
        let armored = encrypted(&s.ctx);
        seed(
            &s.ctx,
            serde_json::json!({"server_id": 7, "server_name": "n",
                "kubeconfig_age": armored, "kubeconfig_yaml": "stale: plaintext\n"}),
        );
        let file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        assert!(file.path().starts_with(s.ctx.runtime_dir()));
        // Precedence: the encrypted slot wins over a plaintext one beside it.
        assert_eq!(fs::read_to_string(file.path()).unwrap(), YAML);
        let path = file.path().to_path_buf();
        drop(file);
        assert!(
            !path.exists(),
            "dropping the handle must remove the decrypted copy"
        );
    }

    #[test]
    fn a_legacy_plaintext_slot_needs_no_key() {
        let s = store(Some(plain()));
        let file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        assert_eq!(fs::read_to_string(file.path()).unwrap(), YAML);
        assert!(!s.ctx.age_key_path().exists());
    }

    #[test]
    fn a_missing_age_key_is_age_key_missing_and_creates_nothing() {
        let s = store(Some(serde_json::json!({"server_id": 7, "server_name": "n",
            "kubeconfig_age": "-----BEGIN AGE ENCRYPTED FILE-----\n-----END AGE ENCRYPTED FILE-----\n"})));
        match materialise_kubeconfig(&s.ctx, &s.target) {
            Err(CoreError::AgeKeyMissing { path }) => {
                assert_eq!(path, s.ctx.age_key_path().display().to_string())
            }
            other => panic!("expected AgeKeyMissing, got {other:?}"),
        }
        // A Read never writes: no key appears (load_or_create_identity would make one).
        assert!(!s.ctx.age_key_path().exists());
        assert!(kubeconfig_files(&s.ctx).is_empty());
    }

    /// WI-457 review #8: a key that does not open the cache (it was cached under a lost key that
    /// a new one replaced) is the CLI's `CacheUndecryptable`, with the target and the key's path,
    /// so doctor can name the refetch; nothing is written.
    #[test]
    fn a_cache_under_another_key_is_cache_undecryptable_and_writes_nothing() {
        let s = store(None);
        cli_core::secrets::load_or_create_identity(s.ctx.age_key_path()).unwrap();
        let other =
            cli_core::secrets::load_or_create_identity(&s._dir.path().join("other.key")).unwrap();
        let armored = cli_core::secrets::encrypt_for_recipient(YAML, &other.to_public()).unwrap();
        seed(
            &s.ctx,
            serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_age": armored}),
        );
        match materialise_kubeconfig(&s.ctx, &s.target) {
            Err(CoreError::Cli(cli_core::CliError::CacheUndecryptable {
                secret: cli_core::secrets::CachedSecret::Kubeconfig,
                target,
                path,
                ..
            })) => {
                assert_eq!(target, "prod");
                assert_eq!(path, s.ctx.age_key_path().display().to_string());
            }
            other => panic!("expected CacheUndecryptable, got {other:?}"),
        }
        assert!(kubeconfig_files(&s.ctx).is_empty());
    }

    #[test]
    fn two_materialisations_get_distinct_files() {
        let s = store(Some(plain()));
        let a = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        let b = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        assert_ne!(a.path(), b.path());
        assert_eq!(kubeconfig_files(&s.ctx).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn materialised_file_and_dir_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let s = store(Some(plain()));
        let file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(s.ctx.runtime_dir()), 0o700);
        assert_eq!(mode(file.path()), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_runtime_dir_is_tightened_to_0700() {
        use std::os::unix::fs::PermissionsExt;
        let s = store(Some(plain()));
        fs::create_dir_all(s.ctx.runtime_dir()).unwrap();
        fs::set_permissions(s.ctx.runtime_dir(), fs::Permissions::from_mode(0o755)).unwrap();
        let _file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        assert_eq!(
            fs::metadata(s.ctx.runtime_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    /// WI-472: on Windows the runtime dir has the protected DACL whose one ACE (this user, full
    /// access) its files inherit, and the decrypted kubeconfig its own protected one-ACE DACL,
    /// set on its handle before its first byte.
    #[cfg(windows)]
    #[test]
    fn materialised_file_and_dir_have_the_owner_only_dacl() {
        use cli_core::owner_only::testing;
        let s = store(Some(plain()));
        let _ = testing::take_before_first_byte();
        let file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        testing::take_before_first_byte()
            .expect("the write recorded the kubeconfig file's DACL")
            .unwrap()
            .assert_owner_only_file("the kubeconfig before its first byte");
        testing::of_path(file.path())
            .unwrap()
            .assert_owner_only_file("the materialised kubeconfig");
        testing::of_path(s.ctx.runtime_dir())
            .unwrap()
            .assert_owner_only_dir("the runtime dir");
    }

    /// WI-472: a runtime dir that exists already (made by an older AppRafter, or by hand) is
    /// given the owner-only DACL before a kubeconfig is written into it.
    #[cfg(windows)]
    #[test]
    fn an_existing_runtime_dir_gets_the_owner_only_dacl() {
        use cli_core::owner_only::testing;
        let s = store(Some(plain()));
        fs::create_dir_all(s.ctx.runtime_dir()).unwrap();
        assert!(
            !testing::of_path(s.ctx.runtime_dir()).unwrap().protected,
            "a directory made by hand inherits (the test's premise)"
        );
        let _file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        testing::of_path(s.ctx.runtime_dir())
            .unwrap()
            .assert_owner_only_dir("the existing runtime dir");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_runtime_dir_is_refused() {
        let s = store(Some(plain()));
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), s.ctx.runtime_dir()).unwrap();
        assert!(materialise_kubeconfig(&s.ctx, &s.target).is_err());
        assert_eq!(
            fs::read_dir(elsewhere.path()).unwrap().count(),
            0,
            "nothing written through the link"
        );
    }

    /// T44, critic W2 and finding 7: a copy's fate is its owner's — gone, or another process
    /// under its pid, told by the start its name records — and never its age or the clock.
    #[test]
    fn the_sweep_judges_a_copy_by_its_owner_never_by_its_age() {
        let me = crate::process::started(std::process::id()).unwrap();
        let other = me.raw().wrapping_add(1);
        assert!(
            !stale(Some(true), Some(me), Some(me.raw())),
            "the writer's own copy stays"
        );
        assert!(
            !stale(Some(true), None, Some(other)),
            "an owner of unknown start stays"
        );
        assert!(
            !stale(Some(true), Some(me), None),
            "no recorded start: kept while it lives"
        );
        assert!(
            !stale(None, None, Some(other)),
            "an owner the OS will not name stays"
        );
        assert!(
            stale(Some(false), None, Some(me.raw())),
            "a gone owner's copy goes"
        );
        assert!(
            stale(Some(false), None, None),
            "with or without a recorded start"
        );
        assert!(
            stale(Some(true), Some(me), Some(other)),
            "a reused pid's copy goes"
        );
    }

    #[test]
    fn the_sweep_removes_what_a_gone_or_earlier_process_left() {
        let s = store(None);
        let dir = s.ctx.runtime_dir();
        fs::create_dir_all(dir).unwrap();
        let me = std::process::id();
        let start = crate::process::started(me).unwrap().raw();
        let dead = crate::process::owner_tests::NO_SUCH_PID;
        let at = |name: &str| {
            let path = dir.join(name);
            fs::write(&path, "x").unwrap();
            path
        };
        let mine = at(&format!("kubeconfig-{me}-{start}-1.yaml"));
        // A long session, or a clock stepped by hours: the writer's own copy stays.
        aged(&mine, Duration::from_secs(48 * 3600));
        let earlier = at(&format!("kubeconfig-{me}-{}-2.yaml", start.wrapping_sub(1)));
        let gone = at(&format!("kubeconfig-{dead}-{start}-1.yaml"));
        let legacy_mine = at(&format!("kubeconfig-{me}-3.yaml"));
        let legacy_gone = at(&format!("kubeconfig-{dead}-1.yaml"));
        let notes = at("notes.txt");
        aged(&notes, Duration::from_secs(48 * 3600));
        let odd = at("kubeconfig-x-1.yaml");
        assert_eq!(sweep_stale(&s.ctx).unwrap(), 3);
        assert!(mine.exists(), "this process's copy is live, however old");
        assert!(
            legacy_mine.exists(),
            "no recorded start: kept while its pid lives"
        );
        assert!(
            !earlier.exists(),
            "an earlier process under this pid left it"
        );
        assert!(
            !gone.exists() && !legacy_gone.exists(),
            "its writer is gone"
        );
        assert!(
            notes.exists() && odd.exists(),
            "only kubeconfig-<pid>-…yaml is ours"
        );
    }

    /// The materialiser sweeps before it writes (after the fence, never at every start), and
    /// names its copy with this process's pid and start.
    #[test]
    fn materialising_sweeps_what_a_gone_process_left() {
        let s = store(None);
        fs::create_dir_all(s.ctx.runtime_dir()).unwrap();
        let gone = s.ctx.runtime_dir().join(format!(
            "kubeconfig-{}-1-1.yaml",
            crate::process::owner_tests::NO_SUCH_PID
        ));
        fs::write(&gone, "x").unwrap();
        let file = materialise(&s.ctx, YAML).unwrap();
        assert!(!gone.exists());
        assert_eq!(fs::read_to_string(file.path()).unwrap(), YAML);
        let me = std::process::id();
        let start = crate::process::started(me).unwrap().raw();
        let name = file
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with(&format!("kubeconfig-{me}-{start}-")),
            "{name}"
        );
    }

    #[test]
    fn sweeping_a_missing_dir_is_zero_and_creates_nothing() {
        let s = store(None);
        assert_eq!(sweep_stale(&s.ctx).unwrap(), 0);
        assert!(!s.ctx.runtime_dir().exists());
    }
}
