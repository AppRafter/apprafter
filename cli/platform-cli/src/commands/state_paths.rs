// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Helper that gives every operational command the same answer
//! to the question "where does my state live, and how do I get
//! there from here?".
//!
//! ## Why a shared helper
//!
//! Pre-v0.1.154 every command opened with the same three lines:
//!
//! ```ignore
//! let cwd = std::env::current_dir()?;
//! let paths = StatePaths::for_root(&cwd);
//! let state = State::load_or_default(&paths)?;
//! ```
//!
//! That cwd anchor was the root cause of the v0.1.154 walk-fix:
//! an operator running `apprafter apply` from the project root
//! and `apprafter app add` from `landing/cms/` got two different
//! state files because the two invocations had two different
//! cwds. The new layout pins state to the active **target**
//! (`<config>/state/<active-target>/`), which is the same answer
//! regardless of cwd.
//!
//! ## What [`resolve_state_paths`] does
//!
//! 1. Resolve the target-store root via
//!    `cli_core::target::default_config_root()` (honours
//!    `APPRAFTER_CONFIG_DIR` so integration tests can redirect).
//! 2. Resolve the active target name — `--target <name>`
//!    override → `GlobalConfig.active_target` →
//!    `CliError::NoActiveTarget` if nothing's set.
//! 3. Build the per-target [`StatePaths`].
//! 4. Best-effort migration: if the operator has a legacy
//!    `<cwd>/.apprafter/state.json` from v0.1.153 and the new
//!    per-target slot is empty, move the legacy file across,
//!    under the store lock. Skipped silently when the legacy file
//!    is absent — the common case for fresh installs.
//!
//! Every command that reads state calls this BEFORE
//! `State::load_or_default`. The function returns both the
//! `StatePaths` and the `TargetStorePaths` so callers that
//! also need credential resolution (`apply`, `destroy`,
//! `import`, `kubeconfig`) reuse the same store handle without
//! re-running the env-var probe.
//!
//! ## Writing the state: [`write_state`]
//!
//! A command that writes its target's `state.json` (`apply`,
//! `destroy`, `import`, `init`) reads it first for its own
//! decisions, does its slow work, and then writes through
//! [`write_state`]: the store lock, the file read again under it,
//! the command's own fields merged into that copy, the save
//! (WI-487).

use std::path::PathBuf;

use cli_core::target::{
    default_config_root, list_target_names, resolve_active_target_name, TargetStorePaths,
};
use cli_core::{CliError, Result};
use cli_state::seam::{self, WritePoint};
use cli_state::{migrate_legacy_state_if_present, State, StatePaths, Write};

/// Bundle returned to operational commands: per-target
/// `StatePaths` plus the `TargetStorePaths` they can reuse for
/// credential resolution. Carrying both saves callers a second
/// `default_config_root()` round-trip and keeps the
/// "resolve once, use everywhere" property.
pub struct ResolvedStatePaths {
    pub paths: StatePaths,
    pub store: TargetStorePaths,
    pub target_name: String,
}

/// Resolve the per-target state directory and trigger the
/// one-shot legacy migration if needed.
///
/// `target_override` is the value of the `--target <name>` flag
/// (when the command carries one). Pass `None` for commands like
/// `apprafter app add` that don't expose `--target` — they
/// always read the active target.
///
/// Returns `CliError::NoActiveTarget` when the target store has
/// no active target and no override was given, and
/// `CliError::TargetNotFound` when the override names a target the
/// store does not have. Both are recoverable — `apprafter target
/// add <name>` / `target use <name>` — and their help says how.
///
/// WI-481: first of all, before any file of the store is read, a
/// store other accounts can change is refused
/// (`CliError::StoreNotPrivate`, naming the folder): every command
/// that resolves its state here goes on to the target's state and
/// its secrets — a token, the cached kubeconfig, the SSH host key
/// it pins — which such a store could have replaced. `status` and
/// `doctor` report instead ([`resolve_state_paths_to_report`]).
pub fn resolve_state_paths(target_override: Option<&str>) -> Result<ResolvedStatePaths> {
    resolve(target_override, true)
}

/// [`resolve_state_paths`] without its check of who can change the
/// store, for a report (`status`, `doctor`'s migration once it has
/// checked for itself): it shows what it can read and reports what
/// it cannot, and each secret it reads is refused by the store's own
/// check. Not for a command that acts on the state.
pub fn resolve_state_paths_to_report(target_override: Option<&str>) -> Result<ResolvedStatePaths> {
    resolve(target_override, false)
}

fn resolve(target_override: Option<&str>, check: bool) -> Result<ResolvedStatePaths> {
    let store_root = default_config_root()?;
    let store = TargetStorePaths::for_root(store_root);
    if check {
        cli_core::store_privacy::ensure_private(&store)?;
    }
    let target_name =
        resolve_active_target_name(&store, target_override)?.ok_or(CliError::NoActiveTarget)?;

    // Eager existence check when the operator supplied a
    // `--target <name>` override. Without it, an operator typing
    // `apprafter apply --target ghost` would see the much later
    // "no provider configured" generic error — the right hint
    // is "ghost not found, available: …" so they can correct the
    // typo. The active-target case (no override) skips the
    // check: state may legitimately not yet exist on a fresh
    // target the operator just added.
    if target_override.is_some() {
        let available = list_target_names(&store).unwrap_or_default();
        if !available.iter().any(|n| n == &target_name) {
            return Err(CliError::TargetNotFound {
                name: target_name,
                available: available.join(", "),
            });
        }
    }

    let paths = StatePaths::for_active_target(&store, &target_name);

    // Best-effort migration from the v0.1.153 per-cwd layout.
    // Caller's cwd may legitimately not be a project root (the
    // operator just opened a fresh shell), so a missing legacy
    // file is the common case — the helper handles that
    // silently. It moves files into the target's `state.json`, so
    // it runs under the store lock, as every writer of that file
    // does (WI-487), taken only when there is a legacy file to
    // move. A report moves nothing into a store other accounts can
    // change (WI-481): the lock refuses such a store, and the
    // report goes on without the move.
    if let Ok(cwd) = std::env::current_dir() {
        if StatePaths::for_root(&cwd).state_file().exists() {
            match crate::commands::target::store_lock(&store) {
                Ok(held) => migrate_legacy_state_if_present(&cwd, &paths, &held)?,
                Err(CliError::StoreNotPrivate { .. }) if !check => {}
                Err(e) => return Err(e),
            }
        }
    }

    Ok(ResolvedStatePaths {
        paths,
        store,
        target_name,
    })
}

/// The one way a command writes its target's `state.json` (WI-487): its slow work done (a
/// provider's calls), it takes the store's write lock — waiting for another AppRafter process
/// as long as it takes, and saying so once on stderr — and under it reads the state again,
/// lets `change` write the fields the command owns into that fresh copy, and saves it
/// ([`State::update`]). A command never saves a copy it read before the lock: that copy would
/// bring back what another writer replaced meanwhile — a cache `lock on` re-sealed to the
/// store's own key, or `kubeconfig --refresh` fetched — or drop what it added.
pub fn write_state(
    store: &TargetStorePaths,
    paths: &StatePaths,
    change: impl FnOnce(&mut State) -> Result<Write>,
) -> Result<Write> {
    seam::reached(WritePoint::BeforeLock);
    let held = state_lock(store)?;
    State::update(paths, &held, change)
}

/// [`write_state`] for `import --force`: a `state.json` that does not parse is renamed aside
/// under the lock (`state.json.corrupt-<epoch>`, never deleted) and `change` starts from an
/// empty state ([`State::update_recovering`]); returns where the file went.
pub fn write_state_recovering(
    store: &TargetStorePaths,
    paths: &StatePaths,
    change: impl FnOnce(&mut State) -> Result<Write>,
) -> Result<(Write, Option<PathBuf>)> {
    seam::reached(WritePoint::BeforeLock);
    let held = state_lock(store)?;
    State::update_recovering(paths, &held, change)
}

/// The store lock for a `state.json` write: the CLI's edit lock, which waits as long as it takes
/// and says so once on stderr ([`crate::commands::target::store_lock`]); a wait is also
/// [`WritePoint::LockWait`], where a test hears a writer it started wait for the lock.
fn state_lock(store: &TargetStorePaths) -> Result<cli_core::StoreLock> {
    cli_core::StoreLock::exclusive_or_wait(store, |event| {
        if matches!(event, cli_core::StoreLockEvent::Waiting { .. }) {
            seam::reached(WritePoint::LockWait);
        }
        crate::commands::target::report_store_lock_event(event);
    })
}

/// Fixtures for the forced interleavings of the `state.json` writers (WI-487): a scratch store
/// in files, the two writers the core runs beside a command's save — a cache write
/// (`kubeconfig --refresh`) and `lock on`'s re-seal — and [`interleave`], which runs one of
/// them exactly at a writer's [`WritePoint`]. Nothing here waits on a clock: a writer started
/// at [`WritePoint::Reread`] is let go when it reports that it waits for the store lock, or
/// when it has finished (the lock was not held, and the test fails on what it lost).
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Mutex, PoisonError};
    use std::thread::JoinHandle;

    use apprafter_core::lock::{execute_on, plan_on, LockOnInput, LockedOn};
    use apprafter_core::vault::{OsRandom, RecoveryKey};
    use apprafter_core::{CancellationToken, Context, Event, Outcome, Reporter, SecretString};
    use cli_core::secrets::CachedSecret;
    use cli_core::target::{
        save_global_config, save_target, GlobalConfig, Target, TargetConfig, TargetCredentials,
        TargetStorePaths, TARGET_STORE_VERSION,
    };
    use cli_core::{StoreLock, StoreLockEvent};
    use cli_state::seam::testing::{arm, Armed};
    use cli_state::seam::WritePoint;
    use cli_state::{HetznerCloudState, State, StatePaths};

    pub(crate) const TOKEN: &str =
        "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    /// The kubeconfig a cache write puts in.
    pub(crate) const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: [refreshed]\n";
    const PASSWORD: &str = "correct horse battery staple";

    /// A store in files at a scratch root: `prod` (the CLI default, `TOKEN`), whose state
    /// records server 42 of cluster `platform-1` in `nbg1`; a context over it whose home — and
    /// so the shared age key — is in the scratch root too, as is its permit for a test KDF.
    pub(crate) struct Scratch {
        pub(crate) _dir: tempfile::TempDir,
        pub(crate) ctx: Context,
        pub(crate) store: TargetStorePaths,
        pub(crate) paths: StatePaths,
    }

    impl Scratch {
        pub(crate) fn new() -> Scratch {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("store");
            let store = TargetStorePaths::for_root(root.clone());
            save_target(
                &store,
                &Target {
                    name: "prod".into(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        region: Some("nbg1".into()),
                        ..Default::default()
                    },
                    credentials: TargetCredentials {
                        hetzner_token: Some(TOKEN.into()),
                    },
                },
            )
            .unwrap();
            save_global_config(
                &store,
                &GlobalConfig {
                    active_target: "prod".into(),
                    version: TARGET_STORE_VERSION,
                },
            )
            .unwrap();
            let paths = StatePaths::for_active_target(&store, "prod");
            State {
                provider: Some("hetzner-cloud".into()),
                cluster_name: Some("platform-1".into()),
                region: Some("nbg1".into()),
                tier: None,
                hetzner_cloud: Some(server(42)),
            }
            .save(&paths)
            .unwrap();
            let ctx = Context::for_desktop(root, "http://127.0.0.1:1")
                .with_home_dir(Some(dir.path().join("home")))
                .with_insecure_kdf_root(dir.path().to_path_buf());
            Scratch {
                _dir: dir,
                ctx,
                store,
                paths,
            }
        }

        /// The state on disk now.
        pub(crate) fn state(&self) -> State {
            State::load_or_default(&self.paths).unwrap()
        }

        /// The cluster record on disk now.
        pub(crate) fn hetzner(&self) -> HetznerCloudState {
            self.state().hetzner_cloud.expect("a cluster record")
        }

        /// The kubeconfig cached on disk now, opened with the store's key now.
        pub(crate) fn cached_kubeconfig(&self, ctx: &Context) -> Option<String> {
            ctx.secrets()
                .cached_kubeconfig(&self.hetzner(), "prod")
                .unwrap()
                .map(|yaml| yaml.as_str().to_string())
        }
    }

    /// Server `id`'s record, named `platform-1`, type `cx22`, nothing cached.
    pub(crate) fn server(id: u64) -> HetznerCloudState {
        HetznerCloudState {
            server_id: id,
            server_name: "platform-1".into(),
            server_type: Some("cx22".into()),
            ssh_key_ids: Vec::new(),
            network_id: None,
            firewall_id: None,
            floating_ip_ids: Vec::new(),
            kubeconfig_yaml: None,
            kubeconfig_age: None,
            argocd_admin_password_age: None,
            kubeconfig_sealed_to: None,
            argocd_admin_password_sealed_to: None,
        }
    }

    /// `kubeconfig --refresh`'s write: [`YAML`] cached as `prod`'s kubeconfig through the
    /// secret store, under the store lock it takes as the CLI does; `signal` hears it wait.
    pub(crate) fn cache_kubeconfig(ctx: &Context, signal: &WaitSignal) -> cli_core::Result<()> {
        let secrets = ctx.secrets();
        let new = secrets.new_cache("prod", CachedSecret::Kubeconfig, true)?;
        let held = StoreLock::exclusive_or_wait(&ctx.store(), |event| {
            if matches!(event, StoreLockEvent::Waiting { .. }) {
                signal.waiting();
            }
        })?;
        let paths = StatePaths::for_active_target(&ctx.store(), "prod");
        secrets.write_cache(&held, &paths, new, YAML, false)?;
        Ok(())
    }

    /// One `lock on` at a time: a plain build derives release keys (256 MiB each).
    static ONE_LOCK_ON: Mutex<()> = Mutex::new(());

    /// `lock on`, run to its end with a lock password and a fresh recovery key, as the CLI
    /// runs it; `signal` hears it wait for the store lock.
    pub(crate) fn lock_on(ctx: &Context, signal: &WaitSignal) -> LockedOn {
        let _one = ONE_LOCK_ON.lock().unwrap_or_else(PoisonError::into_inner);
        let plan = plan_on(ctx, None).expect("the lock on plan");
        let key = RecoveryKey::generate(&mut OsRandom).unwrap();
        let shown = key.to_display();
        let recovery = key.confirm(shown.expose()).unwrap();
        let input = LockOnInput::New {
            password: SecretString::new(PASSWORD),
            recovery,
        };
        match execute_on(
            ctx,
            plan,
            input,
            &HearsWaits(signal),
            &CancellationToken::new(),
        )
        .expect("lock on")
        {
            Outcome::Completed { result } => result,
            other => panic!("lock on did not complete: {other:?}"),
        }
    }

    /// A reporter that tells `signal` when the run waits for the store lock.
    struct HearsWaits<'a>(&'a WaitSignal);

    impl Reporter for HearsWaits<'_> {
        fn report(&self, event: Event) {
            if let Event::Notice { message } = event {
                if message.starts_with("waiting for another AppRafter process") {
                    self.0.waiting();
                }
            }
        }
    }

    /// What a writer started at [`WritePoint::Reread`] says to the thread that holds the lock.
    enum Heard {
        Waiting,
        Finished,
    }

    /// How a writer started by [`interleave`] says that it waits for the store lock.
    pub(crate) struct WaitSignal(Option<Mutex<Sender<Heard>>>);

    impl WaitSignal {
        /// A signal nobody hears: for a writer run outside [`interleave`].
        pub(crate) fn none() -> WaitSignal {
            WaitSignal(None)
        }

        fn waiting(&self) {
            if let Some(tx) = &self.0 {
                let _ = tx
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .send(Heard::Waiting);
            }
        }
    }

    enum Ran<T> {
        Here(T),
        There(JoinHandle<T>),
    }

    /// The writer [`interleave`] ran, and what it returned.
    pub(crate) struct Interleaved<T> {
        ran: Rc<RefCell<Option<Ran<T>>>>,
        _armed: Armed,
    }

    impl<T> Interleaved<T> {
        /// What the other writer returned; its thread joined. Panics when the writer under
        /// test never reached the point.
        pub(crate) fn result(self) -> T {
            match self.ran.borrow_mut().take() {
                Some(Ran::Here(out)) => out,
                Some(Ran::There(handle)) => handle.join().expect("the other writer panicked"),
                None => panic!("the writer under test never reached its write point"),
            }
        }
    }

    /// Runs `other` the first time a `state.json` writer on this thread reaches `point`: at
    /// [`WritePoint::BeforeLock`] right there, to its end — it lands between the writer's
    /// first read and its save; at [`WritePoint::Reread`] on a thread of its own, while the
    /// writer holds the store lock, which it must then wait for: the writer goes on once
    /// `other` says it waits — or once it has finished, the lock not held.
    pub(crate) fn interleave<T: Send + 'static>(
        point: WritePoint,
        other: impl FnOnce(&WaitSignal) -> T + Send + 'static,
    ) -> Interleaved<T> {
        assert_ne!(
            point,
            WritePoint::LockWait,
            "a second writer runs at a writer's BeforeLock or Reread"
        );
        let ran: Rc<RefCell<Option<Ran<T>>>> = Rc::new(RefCell::new(None));
        let slot = Rc::clone(&ran);
        let mut other = Some(other);
        let armed = arm(move |reached| {
            if reached != point {
                return;
            }
            let Some(other) = other.take() else {
                return;
            };
            let outcome = match point {
                WritePoint::BeforeLock => Ran::Here(other(&WaitSignal(None))),
                WritePoint::Reread => Ran::There(until_it_waits(other)),
                WritePoint::LockWait => unreachable!("refused above"),
            };
            *slot.borrow_mut() = Some(outcome);
        });
        Interleaved { ran, _armed: armed }
    }

    /// `other` on a thread of its own, returned once it waits for the store lock or ends. A
    /// command's writer there says it waits at [`WritePoint::LockWait`]; the core's writers
    /// (a cache write, `lock on`) through the [`WaitSignal`] they are given.
    fn until_it_waits<T: Send + 'static>(
        other: impl FnOnce(&WaitSignal) -> T + Send + 'static,
    ) -> JoinHandle<T> {
        let (tx, rx) = channel();
        let done = tx.clone();
        let handle = std::thread::spawn(move || {
            let at_wait = tx.clone();
            let _armed = arm(move |point| {
                if point == WritePoint::LockWait {
                    let _ = at_wait.send(Heard::Waiting);
                }
            });
            let out = other(&WaitSignal(Some(Mutex::new(tx))));
            let _ = done.send(Heard::Finished);
            out
        });
        // Waiting, Finished, or the thread gone (a panic, which `result` reports).
        match rx.recv() {
            Ok(Heard::Waiting) | Ok(Heard::Finished) | Err(_) => {}
        }
        handle
    }
}
