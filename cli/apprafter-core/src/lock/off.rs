// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock off` (ADR 0068 §5; D.4a T37, §3.9). A locked store's secrets go back to files — each
//! target's token into its `credentials.yaml`, its caches re-sealed to the shared age key at
//! `Context::age_key_path` (`APPRAFTER_AGE_KEY`, else the default: O13) — in two phases: a
//! read-only plan ([`plan_off`]) that needs no secret, then six steps ([`execute_off`]) with
//! the vault opened from one prompt's input, every write under the store's held lock:
//!
//! 1. `store.yaml` says `unlocking` (after a re-read under the lock: a run that won the race
//!    finished meanwhile, and this one writes nothing);
//! 2. when a target has a cache to write back, the shared age key is there — loaded, or
//!    created when there is none, never replaced; a store with no cache gets none (D.4a f
//!    Task 3);
//! 3. per target, the vault's token written in clear where the file holds no clear token (a
//!    marker of this store, or no token) — a clear token is never overwritten (critic G2):
//!    one that differs from the vault's is kept, and named — and each cache re-sealed to the
//!    shared key and checked;
//! 4. every token written is read back from its file, and no marker of this store is left;
//! 5. the vault is deleted, the directory synced;
//! 6. `store.yaml` says `files`.
//!
//! No vault datum is deleted before the files that replace it were read back (rule 4). A run
//! interrupted with the vault on disk finishes with one slot's secret; one interrupted after
//! step 5 finishes without one. `lock off` also backs out of an interrupted `lock on` (critic
//! G5): with no vault only `store.yaml` changes (a marker of the store then stops it:
//! `vault_gone`); with one, the steps above undo whatever it had moved, and a cache it had
//! not re-sealed is left as it was. Readers in between refuse (`lock_incomplete`). The plan is
//! Destructive, names each cache the destination key does not open (read without a secret),
//! and on a `locked` store refuses while a `credentials.yaml` holds a clear token (T42). On an
//! unlock a cache neither key opens is dropped and listed; a damaged one is said to be. A
//! marker of this store whose token the vault does not hold stops the run before anything is
//! written (`token_missing_from_vault`). Cancel is honoured until step 3: `store.yaml` is put
//! back.

use std::path::{Path, PathBuf};

use cli_core::secrets::{CachedSecret, Identity};
use cli_core::target::TargetStorePaths;
use cli_core::{CliError, StoreLock, VaultProblem};
use cli_state::{State, StatePaths};
use serde::Serialize;

use super::faults::{self, FaultPoint};
use super::on::{
    change, dropped, refresh, sealed_and_checked, sealed_cache_in, title, vault_err, DroppedCache,
};
use super::open::OpenedVault;
use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult};
use crate::op::{ChangeAction, Outcome, Plan, PlanClass, PlannedChange};
use crate::report::{Event, Reporter};
use crate::secrets::transfer::{self, Credentials, Opened};
use crate::secrets::{
    read_store_file, read_vault, sync_root, vault_on_disk, vault_path, write_store_file, CacheKey,
    KeySession, SecretsMode, StoreFile, StoreId, STORE_FILE, STORE_FILE_FORMAT,
};
use crate::vault::VaultFile;
use crate::CancellationToken;

/// Tests: what [`AFTER_FILES_WRITE`] runs on the store.
#[cfg(test)]
pub(super) type FilesHook = Box<dyn Fn(&TargetStorePaths)>;

#[cfg(test)]
thread_local! {
    /// Tests: runs on the store right after step 3 wrote the files, standing in for a file
    /// that does not hold what was written (step 4 must catch it).
    pub(super) static AFTER_FILES_WRITE: std::cell::RefCell<Option<FilesHook>> =
        const { std::cell::RefCell::new(None) };
}

/// What a plan of `lock off` starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOffStart {
    /// The store is in files: nothing to do.
    NotLocked,
    /// An interrupted `lock on` that wrote no vault: only `store.yaml` goes back to files.
    BackOutNoVault,
    /// The vault's secrets go back to files, with one slot's secret. `from` is the state the
    /// plan read: `Locked`; `Locking` (an interrupted `lock on`, backed out); `Unlocking` (an
    /// interrupted `lock off`, finished).
    Unlock { id: StoreId, from: SecretsMode },
    /// An interrupted `lock off` past its vault's deletion: every secret is in files already.
    FinishNoVault { id: StoreId },
}

/// What [`execute_off`] carries out. It never crosses IPC (`Plan::payload`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockOffPayload {
    start: LockOffStart,
    dest: PathBuf,
    dest_exists: bool,
    needs_key: bool,
}

impl LockOffPayload {
    pub fn start(&self) -> &LockOffStart {
        &self.start
    }

    /// The shared age key the caches go to (O13).
    pub fn dest(&self) -> &Path {
        &self.dest
    }

    /// Whether a key is at [`LockOffPayload::dest`] already; else a run that needs one
    /// ([`LockOffPayload::needs_key`]) creates it.
    pub fn dest_exists(&self) -> bool {
        self.dest_exists
    }

    /// Whether the plan found a cache to write back under the shared key (D.4a f Task 3):
    /// only then is the key at [`LockOffPayload::dest`] read, created or named — the run
    /// decides again under the store's lock. [`LockOffPayload::dest_exists`] is `false` when
    /// it was not read.
    pub fn needs_key(&self) -> bool {
        self.needs_key
    }

    /// Whether the run needs the vault opened with one slot's secret (`lock::open_at`).
    pub fn needs_secret(&self) -> bool {
        matches!(self.start, LockOffStart::Unlock { .. })
    }

    /// The store whose vault the secret opens.
    pub fn store_id(&self) -> Option<&StoreId> {
        match &self.start {
            LockOffStart::Unlock { id, .. } | LockOffStart::FinishNoVault { id } => Some(id),
            LockOffStart::NotLocked | LockOffStart::BackOutNoVault => None,
        }
    }
}

/// What a run of `lock off` did. `Serialize` for the desktop's flow (d4a-g), which adds its
/// TypeScript export; the CLI prints text (T60: no `--json` in D.4a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockedOff {
    pub store: String,
    pub store_id: Option<String>,
    /// The store was in files, or another run finished first: nothing was written.
    pub not_locked: bool,
    /// An interrupted `lock on` was backed out.
    pub backed_out: bool,
    /// Targets whose token was written back in clear.
    pub tokens_restored: Vec<String>,
    /// Targets whose clear token, written while a run was interrupted, differs from the
    /// vault's: the clear one was kept (critic G2's mirror rule).
    pub tokens_kept: Vec<String>,
    pub caches_resealed: u32,
    /// Caches neither the vault's key nor the shared one could open, on an unlock: dropped
    /// (`--refresh` fetches them again).
    pub caches_dropped: Vec<DroppedCache>,
    /// The same, on a back-out of `lock on`, which had not touched them: left as they were
    /// (critic finding 10).
    pub caches_left: Vec<DroppedCache>,
    /// The shared age key the caches are sealed to now; `None` when the run moved no cache,
    /// and so loaded and created no key (D.4a f Task 3).
    pub age_key: Option<String>,
    pub age_key_created: bool,
}

impl LockedOff {
    fn empty(ctx: &Context) -> LockedOff {
        LockedOff {
            store: ctx.config_root().display().to_string(),
            store_id: None,
            not_locked: false,
            backed_out: false,
            tokens_restored: Vec::new(),
            tokens_kept: Vec::new(),
            caches_resealed: 0,
            caches_dropped: Vec::new(),
            caches_left: Vec::new(),
            age_key: None,
            age_key_created: false,
        }
    }
}

fn stage(reporter: &dyn Reporter, index: u32, title: &str) {
    reporter.report(Event::Stage {
        index,
        total: 4,
        title: title.to_string(),
    });
}

/// The first target whose credentials file is a marker of store `id`.
fn marked(
    store: &TargetStorePaths,
    targets: &[String],
    id: &StoreId,
) -> CoreResult<Option<String>> {
    for target in targets {
        if let Credentials::Marker(m) = transfer::credentials(store, target)? {
            if &m == id {
                return Ok(Some(target.clone()));
            }
        }
    }
    Ok(None)
}

/// No vault, yet `target`'s credentials file names it: its token went with the vault. `phase`
/// is what `store.yaml` says.
fn vault_gone(root: &Path, target: &str, phase: SecretsMode) -> CoreError {
    let detail = match phase {
        SecretsMode::Locking => format!(
            "`store.yaml` says a `lock on` was interrupted, and there is no vault, yet the \
             credentials file of `{target}` names it"
        ),
        _ => format!(
            "`store.yaml` says an earlier `lock off` deleted it, and the credentials file of \
             `{target}` still names it"
        ),
    };
    CoreError::Cli(CliError::VaultUnreadable {
        path: vault_path(root),
        problem: VaultProblem::Missing,
        detail,
    })
}

fn store_changed(root: &Path, prepared: SecretsMode, now: SecretsMode) -> CoreError {
    CoreError::Cli(CliError::StoreChanged {
        store: root.to_path_buf(),
        prepared: prepared.into(),
        now: now.into(),
    })
}

fn files_not_kept(store: &TargetStorePaths, what: &str) -> CoreError {
    CoreError::Cli(CliError::FilesNotKept {
        what: what.to_string(),
        vault: vault_path(store.root()),
    })
}

/// The store lock, held: every write of `lock off` needs it (T28).
fn held_lock(ctx: &Context, reporter: &dyn Reporter) -> CoreResult<StoreLock> {
    let held = crate::target::lock_store(ctx, reporter)?;
    if held.is_held() {
        Ok(held)
    } else {
        Err(CoreError::Cli(CliError::VaultNeedsWriteLock {
            store: ctx.config_root().to_path_buf(),
        }))
    }
}

/// `store.yaml` says files, on disk.
fn write_files(store: &TargetStorePaths, held: &StoreLock) -> CoreResult<()> {
    write_store_file(
        store.root(),
        &StoreFile {
            format: STORE_FILE_FORMAT,
            secrets: SecretsMode::Files,
            id: None,
            slots: Vec::new(),
            unlock_timeout_min: None,
        },
        held,
    )?;
    sync_root(store)?;
    Ok(())
}

/// The plan of `lock off` (Destructive, T37): read-only, and without a secret — `store.yaml`,
/// every target's credentials and state, the key at the destination when a cache goes back
/// under it (read, never created), and which caches it does not open. A key there that does
/// not load stops such a plan: it is never replaced.
pub fn plan_off(ctx: &Context) -> CoreResult<Plan<LockOffPayload>> {
    let store = ctx.store();
    let root = ctx.config_root();
    let dest = ctx.age_key_path().to_path_buf();
    let targets = cli_core::list_target_names(&store)?;
    // D.4a f Task 3: only an unlock moves caches; the other starts read no key and no state.
    let plan = |start, title: String, changes, dest_keys: &[Identity], needs_key| Plan {
        class: PlanClass::Destructive,
        title,
        changes,
        payload: LockOffPayload {
            start,
            dest: dest.clone(),
            dest_exists: !dest_keys.is_empty(),
            needs_key,
        },
    };
    let Some(file) = read_store_file(root)?.filter(|f| f.secrets != SecretsMode::Files) else {
        return Ok(plan(
            LockOffStart::NotLocked,
            format!("The target store at {} is not locked", root.display()),
            Vec::new(),
            &[],
            false,
        ));
    };
    // WI-475's refusal, when the change is planned (d4a-h F1): a locked store another account
    // can change is refused before its lock password is asked. Read only, as the plan is.
    cli_core::store_privacy::ensure_private(&store)?;
    let Some(id) = file.id.clone() else {
        return Err(CoreError::Cli(CliError::StoreFileUnreadable {
            path: root.join(STORE_FILE),
            detail: "it names no id".to_string(),
        }));
    };
    let vault = vault_on_disk(root)?;
    match file.secrets {
        SecretsMode::Locking if !vault => {
            // Critic finding 16 (b): a marker of this store with no vault — its token went
            // with the vault — is never called files.
            if let Some(target) = marked(&store, &targets, &id)? {
                return Err(vault_gone(root, &target, SecretsMode::Locking));
            }
            Ok(plan(
                LockOffStart::BackOutNoVault,
                title("Back out of the interrupted `lock on` of", root, &targets),
                vec![change(
                    "StoreFile",
                    STORE_FILE,
                    ChangeAction::Update,
                    "back to `secrets: files`: the interrupted `lock on` had moved nothing",
                )],
                &[],
                false,
            ))
        }
        SecretsMode::Unlocking if !vault => {
            if let Some(target) = marked(&store, &targets, &id)? {
                return Err(vault_gone(root, &target, SecretsMode::Unlocking));
            }
            Ok(plan(
                LockOffStart::FinishNoVault { id },
                title("Finish unlocking the secrets of", root, &targets),
                vec![change(
                    "StoreFile",
                    STORE_FILE,
                    ChangeAction::Update,
                    "`secrets: files`: every secret is in files already",
                )],
                &[],
                false,
            ))
        }
        from => {
            let lead = match from {
                SecretsMode::Locking => "Back out of locking the secrets of",
                SecretsMode::Unlocking => "Finish unlocking the secrets of",
                _ => "Unlock the secrets of",
            };
            // D.4a f Task 3: the destination key is read only when a cache goes back under it.
            let needs_key = sealed_cache_in(&store, &targets)?;
            let dest_keys = if needs_key {
                transfer::shared_identities(std::slice::from_ref(&dest))?
            } else {
                Vec::new()
            };
            let changes = survey(
                &store, root, &targets, &id, from, &dest, &dest_keys, needs_key,
            )?;
            Ok(plan(
                LockOffStart::Unlock { id, from },
                title(lead, root, &targets),
                changes,
                &dest_keys,
                needs_key,
            ))
        }
    }
}

/// What an unlock will move (§3.11's `lock off` strings). On a `locked` store a clear token
/// stops the plan (T37, T42). Each cache the destination key (`dest_keys`, empty when none is
/// there yet) does not open is named with what may become of it (critic finding 10): on an
/// unlock dropped when the vault's key cannot open it either; on a back-out left as it was.
/// The shared key is named only when a cache goes back under it (`needs_key`).
#[allow(clippy::too_many_arguments)]
fn survey(
    store: &TargetStorePaths,
    root: &Path,
    targets: &[String],
    id: &StoreId,
    from: SecretsMode,
    dest: &Path,
    dest_keys: &[Identity],
    needs_key: bool,
) -> CoreResult<Vec<PlannedChange>> {
    let dest_exists = !dest_keys.is_empty();
    let mut changes = Vec::new();
    for target in targets {
        changes.push(match transfer::credentials(store, target)? {
            Credentials::Marker(m) if &m == id => change(
                "Credentials",
                target,
                ChangeAction::Move,
                "vault → credentials.yaml",
            ),
            Credentials::Marker(m) => change(
                "Credentials",
                target,
                ChangeAction::Keep,
                &format!(
                    "its token is in the vault of another target store ({m}), and stays there"
                ),
            ),
            Credentials::Clear(_) if from == SecretsMode::Locked => {
                return Err(CoreError::Cli(transfer::outside_vault(store, target)))
            }
            Credentials::Clear(_) => change(
                "Credentials",
                target,
                ChangeAction::Keep,
                "a token in clear in credentials.yaml: kept, never replaced by the vault's",
            ),
            Credentials::Absent => change(
                "Credentials",
                target,
                ChangeAction::Move,
                "vault → credentials.yaml, when the vault holds a token for it",
            ),
        });
        let state = State::load_or_default(&StatePaths::for_active_target(store, target))?;
        let Some(h) = state.hetzner_cloud else {
            continue;
        };
        for (secret, armored) in [
            (CachedSecret::Kubeconfig, &h.kubeconfig_age),
            (CachedSecret::ArgocdPassword, &h.argocd_admin_password_age),
        ] {
            let Some(armored) = armored else {
                continue;
            };
            if transfer::open_with_any(armored, dest_keys).is_some() {
                continue;
            }
            let otherwise = if from == SecretsMode::Locking {
                "left as it is if the vault's age key cannot open it either: the interrupted \
                 `lock on` had not re-sealed it"
                    .to_string()
            } else {
                format!(
                    "dropped if the vault's age key cannot open it either, and `{}` fetches \
                     it again",
                    refresh(secret, target)
                )
            };
            changes.push(change(
                "Cache",
                target,
                ChangeAction::Update,
                &format!(
                    "its cached {}: re-sealed from the vault to the shared age key at {}; \
                     {otherwise}",
                    secret.noun(),
                    dest.display()
                ),
            ));
        }
    }
    // D.4a f Task 3: a run that writes no cache back loads and creates no key: none is named.
    if needs_key {
        changes.push(if dest_exists {
            change(
                "AgeKey",
                &dest.display().to_string(),
                ChangeAction::Keep,
                "used as it is: every target store in files shares it",
            )
        } else {
            change(
                "AgeKey",
                &dest.display().to_string(),
                ChangeAction::Create,
                "created: the key every target store in files shares",
            )
        });
    }
    changes.push(change(
        "Vault",
        &vault_path(root).display().to_string(),
        ChangeAction::Delete,
        "after every token and cache written to files was read back",
    ));
    Ok(changes)
}

/// Carries out `plan` (module docs); `opened` is the vault opened with one slot's secret when
/// [`LockOffPayload::needs_secret`]. `Completed { not_locked }` when another run finished
/// first; `Cancelled` before step 3, with `store.yaml` put back.
pub fn execute_off(
    ctx: &Context,
    plan: Plan<LockOffPayload>,
    opened: Option<OpenedVault>,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<LockedOff>> {
    if cancel.is_cancelled() {
        return Ok(Outcome::Cancelled {
            cleaned: Vec::new(),
            left: Vec::new(),
        });
    }
    let payload = plan.payload;
    let store = ctx.store();
    let root = ctx.config_root();
    let mut done = LockedOff::empty(ctx);
    match payload.start.clone() {
        LockOffStart::NotLocked => done.not_locked = true,
        LockOffStart::BackOutNoVault => {
            let held = held_lock(ctx, reporter)?;
            match read_store_file(root)? {
                Some(f) if f.secrets == SecretsMode::Locking && !vault_on_disk(root)? => {
                    // Re-read under the lock, as the finish below: a marker of this store
                    // with no vault stops it (critic finding 16 (b)).
                    if let Some(id) = f.id.as_ref() {
                        let targets = cli_core::list_target_names(&store)?;
                        if let Some(left) = marked(&store, &targets, id)? {
                            return Err(vault_gone(root, &left, SecretsMode::Locking));
                        }
                    }
                    write_files(&store, &held)?;
                    done.backed_out = true;
                    done.store_id = f.id.map(|i| i.to_string());
                }
                Some(f) if f.secrets != SecretsMode::Files => {
                    return Err(store_changed(root, SecretsMode::Locking, f.secrets))
                }
                _ => done.not_locked = true,
            }
        }
        LockOffStart::FinishNoVault { id } => {
            let held = held_lock(ctx, reporter)?;
            match read_store_file(root)? {
                Some(f)
                    if f.secrets == SecretsMode::Unlocking
                        && f.id.as_ref() == Some(&id)
                        && !vault_on_disk(root)? =>
                {
                    let targets = cli_core::list_target_names(&store)?;
                    if let Some(left) = marked(&store, &targets, &id)? {
                        return Err(vault_gone(root, &left, SecretsMode::Unlocking));
                    }
                    write_files(&store, &held)?;
                    faults::hit(ctx, FaultPoint::AfterFiles)?;
                    done.store_id = Some(id.to_string());
                }
                Some(f) if f.secrets != SecretsMode::Files => {
                    return Err(store_changed(root, SecretsMode::Unlocking, f.secrets))
                }
                _ => done.not_locked = true,
            }
        }
        LockOffStart::Unlock { id, from } => {
            let Some(opened) = opened else {
                return Err(CoreError::Cli(CliError::Other(
                    "`lock off` needs the store's lock password or recovery key to open its \
                     vault; nothing was written"
                        .to_string(),
                )));
            };
            return unlock(ctx, &payload, &id, from, opened, reporter, cancel, done);
        }
    }
    Ok(Outcome::Completed { result: done })
}

/// Steps 1–6 with the vault open.
#[allow(clippy::too_many_arguments)]
fn unlock(
    ctx: &Context,
    payload: &LockOffPayload,
    id: &StoreId,
    from: SecretsMode,
    opened: OpenedVault,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
    mut done: LockedOff,
) -> CoreResult<Outcome<LockedOff>> {
    let store = ctx.store();
    let root = ctx.config_root();
    stage(reporter, 1, "Opening the vault");
    let held = held_lock(ctx, reporter)?;
    let file = match read_store_file(root)? {
        Some(f) if f.secrets != SecretsMode::Files => f,
        _ => {
            done.not_locked = true;
            return Ok(Outcome::Completed { result: done });
        }
    };
    if file.id.as_ref() != Some(id) || !vault_on_disk(root)? {
        return Err(store_changed(root, from, file.secrets));
    }
    let disk = read_vault(&store, id)?;
    let vault = VaultFile::parse(&disk)
        .and_then(|parsed| parsed.open_with_key(&opened.key, opened.vault_id))
        .map_err(|e| vault_err(root, e))?;
    let targets = cli_core::list_target_names(&store)?;
    // Before any write: every marker of this store has its token in the vault (T43).
    for target in &targets {
        if let Credentials::Marker(m) = transfer::credentials(&store, target)? {
            if &m == id && vault.payload().hetzner_token(target).is_none() {
                return Err(CoreError::Cli(CliError::TokenMissingFromVault {
                    target: target.clone(),
                    store: root.to_path_buf(),
                }));
            }
        }
    }
    // Step 1: unlocking.
    let unlocking = StoreFile {
        secrets: SecretsMode::Unlocking,
        ..file.clone()
    };
    write_store_file(root, &unlocking, &held)?;
    sync_root(&store)?;
    faults::hit(ctx, FaultPoint::AfterStoreFileUnlocking)?;
    // Step 2: the shared key, never replaced — and only when a cache goes back under it,
    // decided here under the held lock (D.4a f Task 3): a store with no cache gets no key, and
    // none is read.
    let shared = if sealed_cache_in(&store, &targets)? {
        let (identity, created) = transfer::shared_identity_at(&payload.dest)?;
        done.age_key = Some(payload.dest.display().to_string());
        done.age_key_created = created;
        Some(transfer::shared_key(identity, payload.dest.clone()))
    } else {
        None
    };
    faults::hit(ctx, FaultPoint::AfterSharedKey)?;
    if cancel.is_cancelled() {
        write_store_file(root, &file, &held)?;
        sync_root(&store)?;
        let left = if done.age_key_created {
            vec![payload.dest.display().to_string()]
        } else {
            Vec::new()
        };
        return Ok(Outcome::Cancelled {
            cleaned: vec![root.join(STORE_FILE).display().to_string()],
            left,
        });
    }
    // Step 3: tokens and caches into files.
    stage(reporter, 2, "Writing the secrets to files");
    let vault_key = transfer::store_key(
        root,
        KeySession::new(opened.key.clone(), opened.vault_id, root.to_path_buf()),
        disk,
    );
    let mut written: Vec<(String, SecretString)> = Vec::new();
    for (n, target) in targets.iter().enumerate() {
        let vaulted = vault.payload().hetzner_token(target);
        let restore = match transfer::credentials(&store, target)? {
            Credentials::Marker(m) => &m == id,
            Credentials::Absent => true,
            Credentials::Clear(clear) => {
                if vaulted
                    .as_ref()
                    .is_some_and(|t| t.expose() != clear.expose())
                {
                    done.tokens_kept.push(target.clone());
                }
                false
            }
        };
        if let (true, Some(token)) = (restore, vaulted) {
            transfer::write_clear_token(&store, target, &token)?;
            done.tokens_restored.push(target.clone());
            written.push((target.clone(), token));
        }
        if let Some(shared) = &shared {
            reseal(&store, target, &vault_key, shared, from, &mut done)?;
        }
        faults::hit(ctx, FaultPoint::AfterFilesTarget(n))?;
    }
    #[cfg(test)]
    AFTER_FILES_WRITE.with(|hook| {
        if let Some(spoil) = hook.borrow().as_ref() {
            spoil(&store);
        }
    });
    // Step 4: read back.
    stage(reporter, 3, "Checking the files");
    for (target, token) in &written {
        match transfer::credentials(&store, target)? {
            Credentials::Clear(back) if back.expose() == token.expose() => {}
            _ => {
                return Err(files_not_kept(
                    &store,
                    &format!("the credentials file of `{target}`"),
                ))
            }
        }
    }
    if let Some(target) = marked(&store, &targets, id)? {
        return Err(files_not_kept(
            &store,
            &format!("the credentials file of `{target}`"),
        ));
    }
    if let Some(shared) = &shared {
        let reloaded = transfer::shared_identities(std::slice::from_ref(&payload.dest))?;
        if reloaded.first().map(|k| k.to_public().to_string())
            != Some(shared.recipient()?.to_string())
        {
            return Err(files_not_kept(&store, &payload.dest.display().to_string()));
        }
    }
    faults::hit(ctx, FaultPoint::AfterFilesVerified)?;
    // Step 5: the vault goes.
    stage(reporter, 4, "Removing the vault");
    std::fs::remove_file(vault_path(root)).map_err(|e| CoreError::Cli(CliError::Io(e)))?;
    sync_root(&store)?;
    faults::hit(ctx, FaultPoint::AfterVaultDeleted)?;
    // Step 6: files.
    write_files(&store, &held)?;
    faults::hit(ctx, FaultPoint::AfterFiles)?;
    done.store_id = Some(id.to_string());
    done.backed_out = from == SecretsMode::Locking;
    Ok(Outcome::Completed { result: done })
}

/// Step 3's caches for one target: one the shared key opens already skipped (a resume, or
/// never re-sealed by an interrupted `lock on`); one the vault's key opens re-sealed to the
/// shared key and checked; one neither opens dropped and listed on an unlock (T37, as the plan
/// said), but left and listed on a back-out of `lock on` (`phase` locking), which had not
/// touched it (critic G5, finding 10). A damaged one is named so (rule 3).
fn reseal(
    store: &TargetStorePaths,
    target: &str,
    vault_key: &CacheKey,
    shared: &CacheKey,
    phase: SecretsMode,
    done: &mut LockedOff,
) -> CoreResult<()> {
    let paths = StatePaths::for_active_target(store, target);
    let mut state = State::load_or_default(&paths)?;
    let Some(mut h) = state.hetzner_cloud.clone() else {
        return Ok(());
    };
    let mut changed = false;
    for secret in [CachedSecret::Kubeconfig, CachedSecret::ArgocdPassword] {
        let slot = match secret {
            CachedSecret::Kubeconfig => &mut h.kubeconfig_age,
            CachedSecret::ArgocdPassword => &mut h.argocd_admin_password_age,
        };
        let Some(armored) = slot.clone() else {
            continue;
        };
        let under_shared = transfer::opened_by(shared, secret, &armored)?;
        if let Opened::Plain(_) = under_shared {
            continue;
        }
        let under_vault = transfer::opened_by(vault_key, secret, &armored)?;
        if let Opened::Plain(plain) = &under_vault {
            *slot = Some(sealed_and_checked(store, shared, secret, target, plain)?);
            changed = true;
            done.caches_resealed += 1;
            continue;
        }
        let damaged =
            matches!(under_vault, Opened::Damaged) || matches!(under_shared, Opened::Damaged);
        let why = if damaged {
            "it is damaged: the age key it was sealed to does not decrypt it"
        } else {
            "neither the vault's age key nor the shared one decrypts it"
        };
        if phase == SecretsMode::Locking {
            done.caches_left.push(dropped(
                target,
                secret,
                &format!("{why}; the interrupted `lock on` had not re-sealed it, so it stays"),
            ));
        } else {
            *slot = None;
            changed = true;
            done.caches_dropped.push(dropped(
                target,
                secret,
                &format!("{why}; `--refresh` fetches it again"),
            ));
        }
    }
    if changed {
        state.hetzner_cloud = Some(h);
        state.save(&paths)?;
    }
    Ok(())
}
