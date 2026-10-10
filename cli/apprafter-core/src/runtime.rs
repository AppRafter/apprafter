// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The core's private runtime directory (spec §4.1, overview R9): where a decrypted kubeconfig
//! lives while a probe uses it, and nowhere else.
//!
//! `Context::runtime_dir()` is `<config root>/run` for the CLI and `<desktop data dir>/run` for
//! the desktop, so one client never sweeps the other's live file. On Unix the directory is
//! 0700 and each file 0600, created with `create_new`; on Windows it sits under the per-user
//! profile (owner-only ACLs arrive with D.4). A [`MaterialisedKubeconfig`] removes its file when
//! it drops; [`sweep_stale`] removes what a crashed process left behind. Reads here are
//! lockless: `state.json` is replaced atomically (overview R3).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use cli_state::{State, StatePaths};
use zeroize::Zeroizing;

use crate::context::Context;
use crate::error::{CoreError, CoreResult};
use crate::target_ref::TargetRef;

/// A copy older than this is a leftover: no probe holds a kubeconfig for an hour.
pub const STALE_KUBECONFIG_AGE: Duration = Duration::from_secs(60 * 60);

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

/// The target's cached kubeconfig, written to `<runtime_dir>/kubeconfig-<pid>-<n>.yaml`.
///
/// `None` when the state records no server or caches no kubeconfig. The encrypted slot wins over
/// the legacy plaintext one (a plaintext copy beside it may be stale, as the CLI's
/// `k8s_helpers::select_cached_kubeconfig` documents); decrypting it loads the age identity
/// WITHOUT creating one — a missing key is [`CoreError::AgeKeyMissing`], never a new key.
pub fn materialise_kubeconfig(
    ctx: &Context,
    target: &TargetRef,
) -> CoreResult<Option<MaterialisedKubeconfig>> {
    let paths = StatePaths::for_active_target(&ctx.store(), target.name());
    let state = State::load_or_default(&paths)?;
    let Some(server) = state.hetzner_cloud else {
        return Ok(None);
    };
    let body = match (
        server.kubeconfig_age.as_deref(),
        server.kubeconfig_yaml.as_deref(),
    ) {
        (Some(armored), _) => {
            let key = ctx.age_key_path();
            let identity =
                cli_core::secrets::load_identity(key)?.ok_or_else(|| CoreError::AgeKeyMissing {
                    path: key.display().to_string(),
                })?;
            Zeroizing::new(cli_core::secrets::decrypt_with_identity(
                armored, &identity,
            )?)
        }
        (None, Some(plain)) => Zeroizing::new(plain.to_string()),
        (None, None) => return Ok(None),
    };
    write_private(ctx.runtime_dir(), body.as_bytes()).map(Some)
}

/// Remove `kubeconfig-*` files in the runtime dir older than `older_than`; returns how many.
/// A missing dir is zero and is not created.
pub fn sweep_stale(ctx: &Context, older_than: Duration) -> CoreResult<usize> {
    let entries = match fs::read_dir(ctx.runtime_dir()) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(io_error(e)),
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(io_error)?;
        if !entry.file_name().to_string_lossy().starts_with(PREFIX) {
            continue;
        }
        // DirEntry::metadata does not follow a symlink: a link is never ours, never removed.
        let meta = entry.metadata().map_err(io_error)?;
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= older_than);
        if !meta.is_file() || !old {
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
    for _ in 0..NAME_ATTEMPTS {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{PREFIX}{pid}-{n}.yaml"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
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

/// Create `dir` owner-only, or tighten an existing one; refuse anything that is not a plain
/// directory (a symlink would put the decrypted file somewhere else).
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
        let f = OpenOptions::new().write(true).open(path).unwrap();
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

    #[test]
    fn sweep_removes_only_old_kubeconfig_copies() {
        let s = store(None);
        let dir = s.ctx.runtime_dir();
        fs::create_dir_all(dir).unwrap();
        for name in ["kubeconfig-1-0.yaml", "kubeconfig-2-0.yaml", "notes.txt"] {
            fs::write(dir.join(name), "x").unwrap();
        }
        aged(
            &dir.join("kubeconfig-1-0.yaml"),
            Duration::from_secs(2 * 3600),
        );
        aged(&dir.join("notes.txt"), Duration::from_secs(2 * 3600));
        assert_eq!(sweep_stale(&s.ctx, STALE_KUBECONFIG_AGE).unwrap(), 1);
        assert!(!dir.join("kubeconfig-1-0.yaml").exists());
        assert!(
            dir.join("kubeconfig-2-0.yaml").exists(),
            "a live copy is never swept"
        );
        assert!(
            dir.join("notes.txt").exists(),
            "only kubeconfig-* files are ours to sweep"
        );
    }

    #[test]
    fn sweeping_a_missing_dir_is_zero_and_creates_nothing() {
        let s = store(None);
        assert_eq!(sweep_stale(&s.ctx, STALE_KUBECONFIG_AGE).unwrap(), 0);
        assert!(!s.ctx.runtime_dir().exists());
    }
}
