// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock on` (ADR 0068 §5; D.4a T36, §3.9). A store in files moves its secrets into a new vault
//! — each target's Hetzner token, a new age identity of its own, and the cached kubeconfigs and
//! Argo CD passwords re-sealed to that identity — in two phases: a read-only plan
//! ([`plan_on`]), then six steps ([`execute_on`]), every write under the store's held lock:
//!
//! 1. `store.yaml` says `locking`, with a fresh id (after a re-read under the lock: a run that
//!    won the race finished meanwhile, G13);
//! 2. the vault is written by no-replace, its directory synced (c1's `write_new`);
//! 3. it is read back from disk and opened with the password slot AND the recovery slot, and
//!    every entry compared;
//! 4. per target, each cache is re-sealed to the store's key and checked (Cancel is honoured
//!    until here: before it, what this run wrote is undone);
//! 5. per target, `credentials.yaml` becomes the marker — only over the very token the vault
//!    holds for it (critic G2): a token written meanwhile is left, and named;
//! 6. `store.yaml` says `locked`.
//!
//! No clear datum is deleted before a vault verified with both slots (rule 4). A run
//! interrupted with its vault on disk resumes at step 3 with both slots' secrets — the vault on
//! disk opened with the password slot's key AND the recovery slot's, each slot opened by a
//! secret of its own kind (`lock::open_slot_at`; team decision 2026-10-13); without one of
//! them the way out is `lock off`, which backs out, and a new `lock on`. One interrupted before
//! starts over, with a new id and a new recovery key (the one shown before is void, G13).
//! Readers in between refuse (`lock_incomplete`). A cache no key of this run opens is dropped
//! when the plan listed it; one that stops opening after the plan is left, and a read later
//! says `cache_undecryptable` and names the refresh.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use age::secrecy::ExposeSecret;
use cli_core::secrets::{CachedSecret, Identity};
use cli_core::target::TargetStorePaths;
use cli_core::{CliError, LockPhase, SecretsModeKind, StoreLock};
use cli_state::{State, StatePaths};
use serde::Serialize;
use zeroize::Zeroizing;

use super::faults::{self, FaultPoint};
use super::open::{check_password, SlotOpened};
use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult};
use crate::op::{ChangeAction, Outcome, Plan, PlanClass, PlannedChange};
use crate::report::{Event, Reporter};
use crate::secrets::marker::write_marker;
use crate::secrets::transfer::{self, Credentials};
use crate::secrets::{
    not_kept, read_store_file, read_vault, sync_root, vault_error, vault_on_disk, vault_path,
    vault_write_error, CacheKey, KeySession, LockSlotKind, SecretsMode, SlotInfo, StoreFile,
    StoreId, STORE_FILE, STORE_FILE_FORMAT,
};
use crate::vault::{
    self, ConfirmedRecoveryKey, NewSlot, OsRandom, RecoveryKey, SlotSecret, UnlockedVault,
    VaultError, VaultFile, VaultPayload, DEFAULT_UNLOCK_TIMEOUT_MIN,
};
use crate::CancellationToken;

/// The labels of the two slots `lock on` makes (`store.yaml` mirrors them).
pub const PASSWORD_LABEL: &str = "lock password";
pub const RECOVERY_LABEL: &str = "recovery key";

/// Tests: what [`AFTER_VAULT_WRITE`] runs on the vault's path.
#[cfg(test)]
pub(super) type VaultHook = Box<dyn Fn(&Path)>;

#[cfg(test)]
thread_local! {
    /// Tests: runs on the vault's path right after step 2 wrote it, standing in for a vault on
    /// disk that does not hold what was written (step 3 must catch it).
    pub(super) static AFTER_VAULT_WRITE: std::cell::RefCell<Option<VaultHook>> =
        const { std::cell::RefCell::new(None) };
}

/// What a plan of `lock on` starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOnStart {
    /// From files, or after a run that wrote no vault (`restart`: its recovery key is void).
    New { restart: bool },
    /// A run interrupted with its vault on disk: finished once the vault opens with both
    /// slots' secrets (rule 4).
    Resume { id: StoreId },
    /// The store is locked already.
    AlreadyLocked,
}

/// A cache the plan drops, which the person agrees to by going on (T36).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheDrop {
    pub target: String,
    pub secret: CachedSecret,
}

/// What [`execute_on`] carries out. It never crosses IPC (`Plan::payload`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockOnPayload {
    start: LockOnStart,
    timeout_min: u32,
    keys: Vec<PathBuf>,
    drops: Vec<CacheDrop>,
}

impl LockOnPayload {
    pub fn start(&self) -> &LockOnStart {
        &self.start
    }

    /// The default unlock window a new vault gets, in minutes.
    pub fn timeout_min(&self) -> u32 {
        self.timeout_min
    }

    /// The shared age key the caches are read with: `APPRAFTER_AGE_KEY`, else the default —
    /// one path (critic finding 3), a list so the plan can say there is none.
    pub fn keys(&self) -> &[PathBuf] {
        &self.keys
    }
}

/// The secrets a run is given.
pub enum LockOnInput {
    /// A new vault: the lock password (`check_password`'s), and the recovery key the person typed
    /// back in full (T25, O12).
    New {
        password: SecretString,
        recovery: ConfirmedRecoveryKey,
    },
    /// Finishing an interrupted run: its vault, opened once by the lock password's slot and
    /// once by the recovery key's (`lock::open_slot_at`; rule 4, team decision 2026-10-13).
    Resume {
        password: SlotOpened,
        recovery: SlotOpened,
    },
}

impl fmt::Debug for LockOnInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LockOnInput::New { .. } => "LockOnInput::New(..)",
            LockOnInput::Resume { .. } => "LockOnInput::Resume(..)",
        })
    }
}

/// A cache a run dropped or left, and why. `Serialize` for the desktop's flow (d4a-g), which
/// adds its TypeScript export, as it does `LockedOn`'s; the CLI prints text (T60: no `--json`
/// in D.4a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedCache {
    pub target: String,
    /// `kubeconfig` or `argocd_password`.
    pub secret: String,
    pub why: String,
}

/// What a run of `lock on` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockedOn {
    pub store: String,
    pub store_id: Option<String>,
    /// Another run locked the store first, or it was locked already: nothing was written.
    pub already_locked: bool,
    /// An earlier run wrote no vault: this one started over.
    pub restarted: bool,
    /// Targets whose credentials file became a marker in this run.
    pub tokens_moved: Vec<String>,
    /// Targets whose clear token is not the vault's: left in their file (critic G2).
    pub tokens_left: Vec<String>,
    pub caches_resealed: u32,
    pub caches_dropped: Vec<DroppedCache>,
    pub caches_left: Vec<DroppedCache>,
    pub unlock_timeout_min: u32,
}

impl LockedOn {
    fn empty(ctx: &Context, timeout: u32) -> LockedOn {
        LockedOn {
            store: ctx.config_root().display().to_string(),
            store_id: None,
            already_locked: false,
            restarted: false,
            tokens_moved: Vec::new(),
            tokens_left: Vec::new(),
            caches_resealed: 0,
            caches_dropped: Vec::new(),
            caches_left: Vec::new(),
            unlock_timeout_min: timeout,
        }
    }
}

fn vault_err(root: &Path, e: VaultError) -> CoreError {
    CoreError::Cli(vault_error(root, e))
}

fn stage(reporter: &dyn Reporter, index: u32, title: &str) {
    reporter.report(Event::Stage {
        index,
        total: 4,
        title: title.to_string(),
    });
}

fn change(kind: &str, object: &str, action: ChangeAction, detail: &str) -> PlannedChange {
    PlannedChange {
        kind: kind.to_string(),
        object: object.to_string(),
        action,
        detail: Some(detail.to_string()),
    }
}

fn dropped(target: &str, secret: CachedSecret, why: &str) -> DroppedCache {
    DroppedCache {
        target: target.to_string(),
        secret: secret.as_str().to_string(),
        why: why.to_string(),
    }
}

fn plan(
    start: LockOnStart,
    title: String,
    changes: Vec<PlannedChange>,
    timeout_min: u32,
    keys: Vec<PathBuf>,
    drops: Vec<CacheDrop>,
) -> Plan<LockOnPayload> {
    Plan {
        class: PlanClass::Bounded,
        title,
        changes,
        payload: LockOnPayload {
            start,
            timeout_min,
            keys,
            drops,
        },
    }
}

fn title(lead: &str, root: &Path, targets: &[String]) -> String {
    let named = if targets.is_empty() {
        "no targets".to_string()
    } else {
        format!("targets: {}", targets.join(", "))
    };
    format!("{lead} the target store at {} ({named})", root.display())
}

/// The shared key a store in files cached under, as this run reads it: `APPRAFTER_AGE_KEY`
/// when it is set, else the default — the one path the CLI's fence checks (T5). The default is
/// not read when the variable moves the key (D.4a d, critic finding 3): on Windows it is the
/// real profile's Known Folder, which no variable moves. No home, no key: an empty path is
/// none.
fn shared_key_paths(ctx: &Context) -> Vec<PathBuf> {
    let path = ctx.age_key_path().to_path_buf();
    if path.as_os_str().is_empty() {
        Vec::new()
    } else {
        vec![path]
    }
}

fn listed(keys: &[PathBuf]) -> String {
    keys.iter()
        .map(|k| k.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ")
}

fn refresh(secret: CachedSecret, target: &str) -> String {
    match secret {
        CachedSecret::Kubeconfig => format!("apprafter kubeconfig --refresh --target {target}"),
        CachedSecret::ArgocdPassword => {
            format!("apprafter argocd-password --refresh --target {target}")
        }
    }
}

/// The plan of `lock on` (Bounded, T36): read-only — `store.yaml`, every target's
/// credentials and state, the shared keys (never created) — refusing a record it cannot read,
/// which may hold the only copy of a secret. Under a weak KDF policy outside its roots it is
/// `insecure_kdf_refused` before anything is asked (T23).
pub fn plan_on(ctx: &Context, timeout_min: Option<u32>) -> CoreResult<Plan<LockOnPayload>> {
    super::permit(ctx)?;
    let timeout = timeout_min.unwrap_or(DEFAULT_UNLOCK_TIMEOUT_MIN);
    super::check_timeout(timeout)?;
    let store = ctx.store();
    let root = ctx.config_root();
    let keys = shared_key_paths(ctx);
    let file = read_store_file(root)?;
    let targets = cli_core::list_target_names(&store)?;
    match file.map(|f| (f.secrets, f.id)) {
        Some((SecretsMode::Locked, _)) => Ok(plan(
            LockOnStart::AlreadyLocked,
            format!("The target store at {} is already locked", root.display()),
            Vec::new(),
            timeout,
            keys,
            Vec::new(),
        )),
        Some((SecretsMode::Unlocking, _)) => Err(CoreError::Cli(CliError::LockIncomplete {
            store: root.to_path_buf(),
            phase: LockPhase::Unlocking,
        })),
        Some((SecretsMode::Locking, Some(id))) if vault_on_disk(root)? => {
            let mut changes = Vec::new();
            for target in &targets {
                if let Credentials::Clear(_) = transfer::credentials(&store, target)? {
                    changes.push(change(
                        "Credentials",
                        target,
                        ChangeAction::Move,
                        "credentials.yaml → vault, when it holds the token the vault holds",
                    ));
                }
            }
            changes.push(change(
                "Cache",
                "every target",
                ChangeAction::Update,
                "cached secrets re-sealed to the store's own age key; one no age key of this \
                 run opens is dropped",
            ));
            changes.push(change(
                "Vault",
                &vault_path(root).display().to_string(),
                ChangeAction::Keep,
                "written by the earlier run; opened with the lock password and with the \
                 recovery key before anything leaves the files",
            ));
            Ok(plan(
                LockOnStart::Resume { id },
                title("Finish locking the secrets of", root, &targets),
                changes,
                timeout,
                keys,
                Vec::new(),
            ))
        }
        mode => {
            let restart = matches!(mode, Some((SecretsMode::Locking, _)));
            let (changes, drops) = survey(&store, root, &targets, &keys)?;
            Ok(plan(
                LockOnStart::New { restart },
                title("Lock the secrets of", root, &targets),
                changes,
                timeout,
                keys,
                drops,
            ))
        }
    }
}

/// What a new run will move (§3.11 strings C2–C6).
fn survey(
    store: &TargetStorePaths,
    root: &Path,
    targets: &[String],
    keys: &[PathBuf],
) -> CoreResult<(Vec<PlannedChange>, Vec<CacheDrop>)> {
    let identities = transfer::shared_identities(keys)?;
    let mut changes = Vec::new();
    let mut drops = Vec::new();
    for target in targets {
        match transfer::credentials(store, target)? {
            Credentials::Clear(_) => changes.push(change(
                "Credentials",
                target,
                ChangeAction::Move,
                "credentials.yaml → vault",
            )),
            Credentials::Marker(id) => changes.push(change(
                "Credentials",
                target,
                ChangeAction::Keep,
                &format!(
                    "its token is in the vault of another target store ({id}), and stays there"
                ),
            )),
            Credentials::Absent => {}
        }
        let state = State::load_or_default(&StatePaths::for_active_target(store, target))?;
        let Some(h) = state.hetzner_cloud else {
            continue;
        };
        let mut moved = false;
        for (secret, armored) in [
            (CachedSecret::Kubeconfig, &h.kubeconfig_age),
            (CachedSecret::ArgocdPassword, &h.argocd_admin_password_age),
        ] {
            let Some(armored) = armored else {
                continue;
            };
            if transfer::open_with_any(armored, &identities).is_some() {
                moved = true;
                continue;
            }
            drops.push(CacheDrop {
                target: target.clone(),
                secret,
            });
            changes.push(change(
                "Cache",
                target,
                ChangeAction::Delete,
                &format!(
                    "its cached {}: the age key at {} does not decrypt it (it was sealed to \
                     another key, or it is damaged); put that key back and run `apprafter lock \
                     on` again, or go on: it is dropped, and `{}` fetches it again",
                    secret.noun(),
                    listed(keys),
                    refresh(secret, target)
                ),
            ));
        }
        match (&h.kubeconfig_yaml, &h.kubeconfig_age) {
            (Some(_), Some(_)) => changes.push(change(
                "Cache",
                target,
                ChangeAction::Delete,
                "its plaintext kubeconfig, beside the encrypted one that is read",
            )),
            (Some(_), None) => moved = true,
            _ => {}
        }
        if moved {
            changes.push(change(
                "Cache",
                target,
                ChangeAction::Update,
                "its cached secrets, re-sealed to the store's own age key",
            ));
        }
    }
    changes.push(change(
        "AgeKey",
        "this store",
        ChangeAction::Create,
        &format!(
            "its own, in the vault ({} stays, for other target stores)",
            listed(keys)
        ),
    ));
    changes.push(change(
        "Vault",
        &vault_path(root).display().to_string(),
        ChangeAction::Create,
        "opened by a lock password or a recovery key",
    ));
    Ok((changes, drops))
}

/// Carries out `plan` with `input` (module docs). `Completed { already_locked }` when another
/// run locked the store first; `Cancelled` before step 4, with what this run wrote undone.
pub fn execute_on(
    ctx: &Context,
    plan: Plan<LockOnPayload>,
    input: LockOnInput,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<LockedOn>> {
    let payload = plan.payload;
    match (payload.start.clone(), input) {
        (LockOnStart::AlreadyLocked, _) => Ok(Outcome::Completed {
            result: LockedOn {
                already_locked: true,
                ..LockedOn::empty(ctx, payload.timeout_min)
            },
        }),
        (LockOnStart::New { restart }, LockOnInput::New { password, recovery }) => {
            on_new(ctx, &payload, restart, password, recovery, reporter, cancel)
        }
        (LockOnStart::Resume { id }, LockOnInput::Resume { password, recovery }) => {
            on_resume(ctx, &payload, &id, password, recovery, reporter, cancel)
        }
        (start, _) => Err(CoreError::Cli(CliError::Other(format!(
            "a `lock on` plan to start {start:?} was given the other kind of input; nothing \
             was written"
        )))),
    }
}

/// What every step after the first works with.
struct Run<'a> {
    ctx: &'a Context,
    store: &'a TargetStorePaths,
    root: &'a Path,
    held: &'a StoreLock,
    id: &'a StoreId,
}

fn on_new(
    ctx: &Context,
    payload: &LockOnPayload,
    restart: bool,
    password: SecretString,
    recovery: ConfirmedRecoveryKey,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<LockedOn>> {
    if cancel.is_cancelled() {
        return Ok(Outcome::Cancelled {
            cleaned: Vec::new(),
            left: Vec::new(),
        });
    }
    check_password(&password)?;
    super::permit(ctx)?;
    let policy = super::kdf_policy(ctx);
    let store = ctx.store();
    let root = ctx.config_root();
    stage(reporter, 1, "Writing the vault");
    let held = crate::target::lock_store(ctx, reporter)?;
    if !held.is_held() {
        return Err(CoreError::Cli(CliError::VaultNeedsWriteLock {
            store: root.to_path_buf(),
        }));
    }
    match read_store_file(root)? {
        None => {}
        Some(f) if f.secrets == SecretsMode::Files => {}
        Some(f) if f.secrets == SecretsMode::Locking && !vault_on_disk(root)? => {}
        Some(f) if f.secrets == SecretsMode::Locked => {
            return Ok(Outcome::Completed {
                result: LockedOn {
                    already_locked: true,
                    ..LockedOn::empty(ctx, payload.timeout_min)
                },
            })
        }
        Some(f) => {
            return Err(CoreError::Cli(CliError::StoreChanged {
                store: root.to_path_buf(),
                prepared: SecretsModeKind::Files,
                now: f.secrets.into(),
            }))
        }
    }
    // Step 1: locking, with a fresh id.
    let id = StoreId::generate(&mut OsRandom).map_err(|e| vault_err(root, e))?;
    let file = StoreFile {
        format: STORE_FILE_FORMAT,
        secrets: SecretsMode::Locking,
        id: Some(id.clone()),
        slots: vec![
            SlotInfo {
                kind: LockSlotKind::Password,
                label: PASSWORD_LABEL.to_string(),
            },
            SlotInfo {
                kind: LockSlotKind::RecoveryKey,
                label: RECOVERY_LABEL.to_string(),
            },
        ],
        unlock_timeout_min: Some(payload.timeout_min),
    };
    crate::secrets::write_store_file(root, &file, &held)?;
    sync_root(&store)?;
    let run = Run {
        ctx,
        store: &store,
        root,
        held: &held,
        id: &id,
    };
    faults::hit(ctx, FaultPoint::AfterStoreFileLocking)?;
    if cancel.is_cancelled() {
        return undo_locking(&run, false);
    }
    // Step 2: the vault, by no-replace.
    let identity = Identity::generate();
    let mut contents = VaultPayload::new(&identity);
    let mut placed: Vec<(String, SecretString)> = Vec::new();
    for target in cli_core::list_target_names(&store)? {
        if let Credentials::Clear(token) = transfer::credentials(&store, &target)? {
            contents
                .set_hetzner_token(&target, token.clone())
                .map_err(|e| CoreError::Cli(vault_write_error(root, e)))?;
            placed.push((target, token));
        }
    }
    contents
        .set_unlock_timeout_min(payload.timeout_min)
        .map_err(|e| vault_err(root, e))?;
    let shown = recovery.to_display();
    let again = RecoveryKey::parse(shown.expose())
        .map_err(|e| CoreError::RecoveryKeyInvalid { reason: e.into() })?;
    let writer = ctx.vault_access().writer();
    let (bytes, made) = vault::create(
        id.as_str(),
        &writer,
        contents,
        vec![
            NewSlot::Password {
                secret: password.clone(),
                label: PASSWORD_LABEL.to_string(),
            },
            NewSlot::RecoveryKey {
                key: recovery.into_key(),
                label: RECOVERY_LABEL.to_string(),
            },
        ],
        policy,
        &mut OsRandom,
    )
    .map_err(|e| vault_err(root, e))?;
    let path = vault_path(root);
    vault::file::write_new(&path, &bytes, &held).map_err(|e| vault_err(root, e))?;
    #[cfg(test)]
    AFTER_VAULT_WRITE.with(|hook| {
        if let Some(spoil) = hook.borrow().as_ref() {
            spoil(&path);
        }
    });
    faults::hit(ctx, FaultPoint::AfterVaultWritten)?;
    if cancel.is_cancelled() {
        return undo_locking(&run, true);
    }
    // Step 3: read back from disk, opened with both slots, every entry compared.
    stage(reporter, 2, "Checking the vault");
    let disk: Arc<[u8]> = Arc::from(vault::file::read(&path).map_err(|e| vault_err(root, e))?);
    let parsed = VaultFile::parse(&disk).map_err(|e| vault_err(root, e))?;
    if parsed.header().store_id != id.as_str() || parsed.header().vault_id != made.header().vault_id
    {
        return Err(CoreError::Cli(not_kept(&store, "the vault this run wrote")));
    }
    let by_password = parsed
        .open(&SlotSecret::Password(password), policy)
        .map_err(|_| CoreError::Cli(not_kept(&store, "a slot the lock password opens")))?;
    let by_recovery = parsed
        .open(&SlotSecret::RecoveryKey(again), policy)
        .map_err(|_| CoreError::Cli(not_kept(&store, "a slot the recovery key opens")))?;
    for opened in [&by_password, &by_recovery] {
        check_entries(&store, opened, &placed, &identity, payload.timeout_min)?;
    }
    faults::hit(ctx, FaultPoint::AfterVaultVerified)?;
    if cancel.is_cancelled() {
        return undo_locking(&run, true);
    }
    // Steps 4–6.
    let session = KeySession::new(
        by_password.data_key().clone(),
        by_password.header().vault_id,
        root.to_path_buf(),
    );
    let key = transfer::store_key(root, session, disk);
    let mut done = LockedOn {
        store_id: Some(id.to_string()),
        restarted: restart,
        ..LockedOn::empty(ctx, payload.timeout_min)
    };
    finish(
        &run,
        file,
        &key,
        &by_password,
        payload,
        false,
        reporter,
        &mut done,
    )?;
    Ok(Outcome::Completed { result: done })
}

/// Step 3 again, then steps 4–6 (rule 4; team decision 2026-10-13): the vault on disk opens
/// with the password slot's key AND the recovery slot's, each slot opened by a secret of its
/// own kind, before any clear datum goes. Two openings of one kind are refused as the
/// interrupted lock they leave (`lock_incomplete`, whose help names `lock off` and a new
/// `lock on`); a key for another vault is the store changing under the run (`store_changed`).
fn on_resume(
    ctx: &Context,
    payload: &LockOnPayload,
    id: &StoreId,
    password: SlotOpened,
    recovery: SlotOpened,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<LockedOn>> {
    let store = ctx.store();
    let root = ctx.config_root();
    if password.kind() != LockSlotKind::Password || recovery.kind() != LockSlotKind::RecoveryKey {
        return Err(CoreError::Cli(CliError::LockIncomplete {
            store: root.to_path_buf(),
            phase: LockPhase::Locking,
        }));
    }
    stage(reporter, 2, "Checking the vault");
    let held = crate::target::lock_store(ctx, reporter)?;
    if !held.is_held() {
        return Err(CoreError::Cli(CliError::VaultNeedsWriteLock {
            store: root.to_path_buf(),
        }));
    }
    let file = match read_store_file(root)? {
        Some(f) if f.secrets == SecretsMode::Locked => {
            return Ok(Outcome::Completed {
                result: LockedOn {
                    already_locked: true,
                    ..LockedOn::empty(ctx, payload.timeout_min)
                },
            })
        }
        Some(f) if f.secrets == SecretsMode::Locking && f.id.as_ref() == Some(id) => f,
        other => {
            return Err(CoreError::Cli(CliError::StoreChanged {
                store: root.to_path_buf(),
                prepared: SecretsModeKind::Locking,
                now: other.map_or(SecretsMode::Files, |f| f.secrets).into(),
            }))
        }
    };
    let disk = read_vault(&store, id)?;
    let parsed = VaultFile::parse(&disk).map_err(|e| vault_err(root, e))?;
    let vault = parsed
        .open_with_key(&password.opened().key, password.opened().vault_id)
        .map_err(|e| vault_err(root, e))?;
    parsed
        .open_with_key(&recovery.opened().key, recovery.opened().vault_id)
        .map_err(|e| vault_err(root, e))?;
    if cancel.is_cancelled() {
        return Ok(Outcome::Cancelled {
            cleaned: Vec::new(),
            left: Vec::new(),
        });
    }
    let run = Run {
        ctx,
        store: &store,
        root,
        held: &held,
        id,
    };
    let opened = password.opened();
    let key = transfer::store_key(
        root,
        KeySession::new(opened.key.clone(), opened.vault_id, root.to_path_buf()),
        disk,
    );
    let timeout = file.unlock_timeout_min.unwrap_or(payload.timeout_min);
    let mut done = LockedOn {
        store_id: Some(id.to_string()),
        ..LockedOn::empty(ctx, timeout)
    };
    finish(&run, file, &key, &vault, payload, true, reporter, &mut done)?;
    Ok(Outcome::Completed { result: done })
}

/// Every entry the run put, read back from `opened` (step 3).
fn check_entries(
    store: &TargetStorePaths,
    opened: &UnlockedVault,
    placed: &[(String, SecretString)],
    identity: &Identity,
    timeout: u32,
) -> CoreResult<()> {
    for (target, token) in placed {
        // T36: byte for byte after a trim. c1 hands out owned copies (its Deviation 12).
        let kept = opened.payload().hetzner_token(target);
        if kept.as_ref().map(|t| t.expose().trim()) != Some(token.expose().trim()) {
            return Err(CoreError::Cli(not_kept(
                store,
                &format!("the token of `{target}`"),
            )));
        }
    }
    // T36: the identity by its canonical string and by its recipient.
    let kept = opened
        .payload()
        .age_identity()
        .map_err(|e| vault_err(store.root(), e))?;
    if kept.to_string().expose_secret() != identity.to_string().expose_secret()
        || kept.to_public().to_string() != identity.to_public().to_string()
    {
        return Err(CoreError::Cli(not_kept(store, "the store's age identity")));
    }
    if opened
        .payload()
        .unlock_timeout_min()
        .map_err(|e| vault_err(store.root(), e))?
        != timeout
    {
        return Err(CoreError::Cli(not_kept(store, "the unlock window")));
    }
    Ok(())
}

/// Steps 4–6, idempotent: a resume redoes them over whatever an interruption left.
#[allow(clippy::too_many_arguments)]
fn finish(
    run: &Run<'_>,
    file: StoreFile,
    key: &CacheKey,
    vault: &UnlockedVault,
    payload: &LockOnPayload,
    resume: bool,
    reporter: &dyn Reporter,
    done: &mut LockedOn,
) -> CoreResult<()> {
    stage(reporter, 3, "Re-encrypting cached secrets");
    let old = transfer::shared_identities(&payload.keys)?;
    let targets = cli_core::list_target_names(run.store)?;
    for (n, target) in targets.iter().enumerate() {
        reseal(run.store, target, key, &old, &payload.drops, resume, done)?;
        faults::hit(run.ctx, FaultPoint::AfterCachesTarget(n))?;
    }
    stage(reporter, 4, "Removing the secrets from files");
    for (n, target) in targets.iter().enumerate() {
        if let Credentials::Clear(token) = transfer::credentials(run.store, target)? {
            let vaulted = vault.payload().hetzner_token(target);
            if vaulted.as_ref().map(|t| t.expose().trim()) == Some(token.expose().trim()) {
                write_marker(run.store, target, run.id)?;
                done.tokens_moved.push(target.clone());
            } else {
                done.tokens_left.push(target.clone());
            }
        }
        faults::hit(run.ctx, FaultPoint::AfterMarkerTarget(n))?;
    }
    let locked = StoreFile {
        secrets: SecretsMode::Locked,
        ..file
    };
    crate::secrets::write_store_file(run.root, &locked, run.held)?;
    sync_root(run.store)?;
    faults::hit(run.ctx, FaultPoint::AfterLocked)?;
    Ok(())
}

/// Step 4 for one target (T36): a cache under an old key re-sealed to the store's and checked;
/// one already the store's skipped (a resume); one no key of this run opens dropped when the
/// plan listed it (or on a resume, whose plan says so), else left; a winning plaintext
/// kubeconfig sealed, a shadowed one dropped.
fn reseal(
    store: &TargetStorePaths,
    target: &str,
    key: &CacheKey,
    old: &[Identity],
    agreed: &[CacheDrop],
    resume: bool,
    done: &mut LockedOn,
) -> CoreResult<()> {
    let paths = StatePaths::for_active_target(store, target);
    let mut state = State::load_or_default(&paths)?;
    let Some(mut h) = state.hetzner_cloud.clone() else {
        return Ok(());
    };
    let shadowed = h.kubeconfig_age.is_some();
    let mut changed = false;
    for secret in [CachedSecret::Kubeconfig, CachedSecret::ArgocdPassword] {
        let slot = match secret {
            CachedSecret::Kubeconfig => &mut h.kubeconfig_age,
            CachedSecret::ArgocdPassword => &mut h.argocd_admin_password_age,
        };
        let Some(armored) = slot.clone() else {
            continue;
        };
        if transfer::opens(key, secret, &armored).is_some() {
            continue;
        }
        match transfer::open_with_any(&armored, old) {
            Some(plain) => {
                *slot = Some(sealed_and_checked(store, key, secret, target, &plain)?);
                changed = true;
                done.caches_resealed += 1;
            }
            None if resume
                || agreed.contains(&CacheDrop {
                    target: target.to_string(),
                    secret,
                }) =>
            {
                *slot = None;
                changed = true;
                done.caches_dropped.push(dropped(
                    target,
                    secret,
                    "no age key this run read decrypts it",
                ));
            }
            None => done.caches_left.push(dropped(
                target,
                secret,
                "it stopped decrypting after the plan was made; `--refresh` fetches it again",
            )),
        }
    }
    if let Some(plain) = h.kubeconfig_yaml.take().map(Zeroizing::new) {
        changed = true;
        if shadowed {
            done.caches_dropped.push(dropped(
                target,
                CachedSecret::Kubeconfig,
                "a plaintext copy beside the encrypted one",
            ));
        } else {
            h.kubeconfig_age = Some(sealed_and_checked(
                store,
                key,
                CachedSecret::Kubeconfig,
                target,
                &plain,
            )?);
            done.caches_resealed += 1;
        }
    }
    if changed {
        state.hetzner_cloud = Some(h);
        state.save(&paths)?;
    }
    Ok(())
}

/// `plain` sealed to `key`, and opened again with it before it replaces anything.
fn sealed_and_checked(
    store: &TargetStorePaths,
    key: &CacheKey,
    secret: CachedSecret,
    target: &str,
    plain: &str,
) -> CoreResult<String> {
    let sealed = transfer::seal(key, plain)?;
    match transfer::opens(key, secret, &sealed) {
        Some(back) if back.as_str() == plain => Ok(sealed),
        _ => Err(CoreError::Cli(not_kept(
            store,
            &format!("the re-sealed {} of `{target}`", secret.noun()),
        ))),
    }
}

/// A cancel before step 4: the vault this run wrote goes and `store.yaml` says files —
/// nothing had left the files yet (rule 4).
fn undo_locking(run: &Run<'_>, wrote_vault: bool) -> CoreResult<Outcome<LockedOn>> {
    let mut cleaned = Vec::new();
    if wrote_vault {
        let path = vault_path(run.root);
        std::fs::remove_file(&path).map_err(|e| CoreError::Cli(CliError::Io(e)))?;
        cleaned.push(path.display().to_string());
    }
    crate::secrets::write_store_file(
        run.root,
        &StoreFile {
            format: STORE_FILE_FORMAT,
            secrets: SecretsMode::Files,
            id: None,
            slots: Vec::new(),
            unlock_timeout_min: None,
        },
        run.held,
    )?;
    sync_root(run.store)?;
    cleaned.push(run.root.join(STORE_FILE).display().to_string());
    Ok(Outcome::Cancelled {
        cleaned,
        left: Vec::new(),
    })
}
