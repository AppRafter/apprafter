// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock slot add` and `lock slot remove` in the core (ADR 0069 §6, §10; D.4b overview
//! B18–B21, B60, B61, B77): a way to open a locked target store added or removed — a lock
//! password, the account password (Windows Hello from d4b-w2) — in two phases, as `lock on`
//! and `lock off`: a read-only plan that needs no secret and refuses what this computer or
//! this store refuses before anything is asked (GOTCHA-294), then one vault write:
//!
//! 1. the client asks a **fresh** secret of any slot (B60) and opens the vault with it in
//!    this process: an [`OpenedVault`], which only `lock::open_at` makes — never a holder's
//!    key, never the key an unlocked app holds (O11, B19). For an add it also makes the new
//!    slot's material, every prompt before the store lock (rule 4): a lock password typed
//!    twice, or the account password checked with the system by the os-slots glue, which
//!    alone builds that slot (d4b-d's G-OS6);
//! 2. under the store's held write lock the vault is read again and opened with that
//!    secret's data key — a vault replaced since, by a key change, is `store_changed` — and
//!    the store's slots are checked again on it (B18, B77): the write is built from that
//!    vault, so a token another command wrote between the prompt and the lock is kept (M3);
//! 3. one atomic replace, durable before it returns (d4b-b1), then the store root synced
//!    (B51);
//! 4. read back from disk: an add opens the vault with the new slot's own secret, a removal
//!    finds the slot gone and the data key unchanged (B20);
//! 5. `store.yaml`'s slot list mirrors the vault's (display only: a crash before it is the
//!    drift doctor names, which the next write fixes, B42).
//!
//! A removal never changes the keys (B21): its plan says what that leaves.

use std::path::Path;

use cli_core::{
    AccountCheckUnavailable, AccountPasswordRefusal, AccountPasswordUnusable, CliError, LockPhase,
    SecretsModeKind, SlotRefusal, StoreLock,
};
use serde::Serialize;

use super::facts::{MachineFact, SlotFacts};
use super::faults::{self, FaultPoint};
use super::on::{change, title, vault_err};
use super::open::{OpenedVault, PasswordAdvice};
use super::{SlotNote, SlotRef, SlotView};
use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult};
use crate::op::{ChangeAction, Outcome, Plan, PlanClass};
use crate::report::Reporter;
use crate::secrets::{
    not_kept, read_store_file, read_vault, sync_root, vault_path, write_store_file, LockSlotKind,
    SecretsMode, SlotInfo, StoreId, STORE_FILE,
};
use crate::vault::{
    self, check_new_password, KdfPolicy, MachineId, NewSlot, OsRandom, PasswordProblem, SlotId,
    SlotKind, SlotRecord, SlotSecret, UnlockedVault, VaultError, VaultFile, VaultHeader, LABEL_MAX,
    RECOMMENDED_PASSWORD_CHARS, SLOT_MAX,
};
use crate::CancellationToken;

/// Tests: what [`AFTER_SLOT_WRITE`] runs on the store root.
#[cfg(test)]
pub(super) type SlotHook = Box<dyn Fn(&Path)>;

#[cfg(test)]
thread_local! {
    /// Tests: runs on the store root right after a slot write replaced the vault, standing
    /// in for a vault on disk that does not hold what was written (the read-back must catch
    /// it).
    pub(super) static AFTER_SLOT_WRITE: std::cell::RefCell<Option<SlotHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Tests: [`AFTER_SLOT_WRITE`], when a test set it.
#[cfg(test)]
fn after_slot_write(root: &Path) {
    AFTER_SLOT_WRITE.with(|hook| {
        if let Some(spoil) = hook.borrow().as_ref() {
            spoil(root);
        }
    });
}

/// What `lock slot add` adds (B26's `<KIND>`; Windows Hello joins in d4b-w2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotAddKind {
    /// A separate lock password, when the store has none (B18).
    Password,
    /// The account password, on Linux and macOS, bound to this computer (B54, B77).
    OsPassword,
}

impl SlotAddKind {
    /// Its name on the command line (`lock slot add <KIND>`).
    pub fn as_str(self) -> &'static str {
        match self {
            SlotAddKind::Password => "password",
            SlotAddKind::OsPassword => "os-password",
        }
    }
}

/// What [`execute_slot_add`] carries out. It never crosses IPC (`Plan::payload`). Not
/// `PartialEq`: its machine id is compared only as bytes, where it binds a slot.
#[derive(Debug, Clone)]
pub struct SlotAddPayload {
    id: StoreId,
    kind: SlotAddKind,
    replaces: Vec<SlotRef>,
    notes: Vec<SlotNote>,
    /// The machine id the plan named this computer's slot by (an account-password add): the
    /// new slot must be bound to it (critic round, finding 5).
    machine: Option<MachineId>,
}

impl SlotAddPayload {
    /// The store whose vault the opener's secret opens.
    pub fn store_id(&self) -> &StoreId {
        &self.id
    }

    pub fn kind(&self) -> SlotAddKind {
        self.kind
    }

    /// This computer's account-password slot the add replaces in the same write (B77): the
    /// re-enrolment after an account-password change.
    pub fn replaces(&self) -> &[SlotRef] {
        &self.replaces
    }

    /// What the new slot costs and adds, each said before its secret is asked (B61, B70,
    /// B76; rule 1): the CLI's words are `text::plan_note_line`.
    pub fn notes(&self) -> &[SlotNote] {
        &self.notes
    }
}

/// What a run of `lock slot add` did. `Serialize` for the desktop's flow (its TypeScript
/// export is d4b-c4's); the CLI prints text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotAdded {
    pub store: String,
    pub store_id: String,
    pub added: SlotRef,
    /// This computer's account-password slot it replaced (B77).
    pub replaced: Vec<SlotRef>,
    /// The slot whose fresh secret allowed it (B60).
    pub opened_with: SlotRef,
}

/// The plan of `lock slot add <kind>` (Bounded, B20): read-only, no secret. Refused before
/// anything is asked: a store not locked, or one another account can change (WI-475); an
/// account-password slot on an OS without one (S3: the OS `facts` plays, never `cfg!`, B15)
/// or on a computer whose machine id cannot be read (B54); a second lock password (B18); a
/// vault that holds 16 slots already; a weak key-derivation policy outside its roots (S12,
/// T23). Its notes say what the new slot costs (B61). A machine id the client did not ask for
/// (`MachineFact::Unknown`) is refused as `no_machine_id`, fail-closed: the glue applies B88's
/// order first — under `APPRAFTER_SANDBOX` the fence's `apprafter::sandbox::os_auth`, which
/// G-OS3 expects at plan time — so a person meets this arm only where no backend could be
/// asked (critic round, finding 4).
pub fn plan_slot_add(
    ctx: &Context,
    kind: SlotAddKind,
    facts: &dyn SlotFacts,
) -> CoreResult<Plan<SlotAddPayload>> {
    let root = ctx.config_root();
    let (id, file) = locked_vault(ctx)?;
    let header = file.header();
    let (replaces, notes, object, detail, machine) = match kind {
        SlotAddKind::Password => {
            if header
                .slots
                .iter()
                .any(|slot| slot.kind == SlotKind::Password)
            {
                return Err(slot_refused(root, SlotRefusal::AlreadyHasPassword));
            }
            (
                Vec::new(),
                Vec::new(),
                "lock password",
                "a separate lock password: AppRafter asks it twice, and only its prompt ever \
                 sees it",
                None,
            )
        }
        SlotAddKind::OsPassword => {
            if !facts.os().has_account_password_slot() {
                return Err(slot_refused(root, SlotRefusal::NotOnThisOs));
            }
            let MachineFact::Id(machine) = facts.machine_id() else {
                return Err(account_refused(AccountPasswordRefusal::Unavailable {
                    what: AccountCheckUnavailable::NoMachineId,
                }));
            };
            let replaces: Vec<SlotRef> = header
                .slots
                .iter()
                .filter(|slot| slot.made_on(&machine))
                .map(SlotRef::from)
                .collect();
            let mut notes = vec![SlotNote::AccountPasswordOffline, SlotNote::ManagesStore];
            if facts.machine_id_may_change() == Some(true) {
                notes.push(SlotNote::MachineIdMayChange);
            }
            (
                replaces,
                notes,
                "account password",
                "your account password on this computer, checked with the system once",
                Some(machine),
            )
        }
    };
    if header.slots.len() - replaces.len() >= SLOT_MAX {
        return Err(slot_refused(root, SlotRefusal::SlotsFull));
    }
    super::permit(ctx)?;
    let targets = cli_core::list_target_names(&ctx.store())?;
    let mut changes = vec![change("Slot", object, ChangeAction::Create, detail)];
    for old in &replaces {
        changes.push(change(
            "Slot",
            &old.label,
            ChangeAction::Replace,
            "this computer's account-password slot: the new one replaces it in the same write",
        ));
    }
    Ok(Plan {
        class: PlanClass::Bounded,
        title: title("Add a way to open", root, &targets),
        changes,
        payload: SlotAddPayload {
            id,
            kind,
            replaces,
            notes,
            machine,
        },
    })
}

/// Carries out `plan` (module docs, steps 2–5): `opened` is the vault opened in this process
/// by a fresh secret of any slot (B60; only `lock::open_at` makes one), `new` the slot to
/// add, of the plan's kind (the os-slots glue builds an account-password one once the system
/// checked it). `Cancelled`, with nothing written, until the store lock is held.
pub fn execute_slot_add(
    ctx: &Context,
    plan: Plan<SlotAddPayload>,
    opened: OpenedVault,
    new: NewSlot,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<SlotAdded>> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let payload = plan.payload;
    let root = ctx.config_root();
    let store = ctx.store();
    let policy = super::kdf_policy(ctx);
    let again = reopener(&new, payload.kind, payload.machine.as_ref())?;
    super::permit(ctx)?;
    let held = held_lock(ctx, reporter)?;
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let mut vault = reread(ctx, &payload.id, &opened)?;
    // B18, B77 on the vault as it is now, not as the plan read it.
    let replaced: Vec<SlotRef> = match &new {
        NewSlot::OsPassword { machine, .. } => vault
            .header()
            .slots
            .iter()
            .filter(|slot| slot.made_on(machine))
            .map(SlotRef::from)
            .collect(),
        NewSlot::Password { .. } => {
            if vault
                .header()
                .slots
                .iter()
                .any(|slot| slot.kind == SlotKind::Password)
            {
                return Err(slot_refused(root, SlotRefusal::AlreadyHasPassword));
            }
            Vec::new()
        }
        // `reopener` refused it above.
        NewSlot::RecoveryKey { .. } => Vec::new(),
    };
    let added_id = vault
        .add_slot(new, policy, &mut OsRandom)
        .map_err(|e| add_err(root, payload.kind, e))?;
    let writer = ctx.vault_access().writer();
    let sealed = vault
        .seal(&writer, &mut OsRandom)
        .map_err(|e| vault_err(root, e))?;
    faults::hit(ctx, FaultPoint::BeforeSlotWrite)?;
    vault::file::replace(&vault_path(root), &sealed, &held).map_err(|e| vault_err(root, e))?;
    #[cfg(test)]
    after_slot_write(root);
    faults::hit(ctx, FaultPoint::AfterSlotWrite)?;
    sync_root(&store)?;
    // B20: read back from disk, opened with the new slot's own secret, to this vault.
    let disk = read_vault(&store, &payload.id)?;
    let kept = VaultFile::parse(&disk)
        .and_then(|back| back.open(&again, policy))
        .ok()
        .filter(|back| {
            back.opened_by() == Some(added_id) && back.header().vault_id == opened.vault_id
        })
        .ok_or_else(|| CoreError::Cli(not_kept(&store, "the slot this write added")))?;
    let added = kept
        .header()
        .slots
        .iter()
        .find(|slot| slot.id == added_id)
        .map(SlotRef::from)
        .ok_or_else(|| CoreError::Cli(not_kept(&store, "the slot this write added")))?;
    mirror(root, &held, kept.header())?;
    Ok(Outcome::Completed {
        result: SlotAdded {
            store: root.display().to_string(),
            store_id: payload.id.to_string(),
            added,
            replaced,
            opened_with: opened.opened_with().clone(),
        },
    })
}

/// The account password, typed for a new slot, before the system is asked to check it
/// (B10, B75): refused when no slot could hold it — empty (`no_password`), longer than the
/// vault takes, or shaped like a recovery key, which a prompt reads as one — so no check is
/// spent on a password the vault would refuse. Shorter than `RECOMMENDED_PASSWORD_CHARS`
/// it is advice (B2: quicker to guess from a copy of the vault), never a refusal.
pub fn check_account_password(password: &SecretString) -> CoreResult<Option<PasswordAdvice>> {
    let accepted = check_new_password(password).map_err(|p| account_refused(unusable(p)))?;
    Ok(accepted.is_short().then(|| PasswordAdvice::Short {
        chars: u32::try_from(accepted.chars()).unwrap_or(u32::MAX),
        recommended: RECOMMENDED_PASSWORD_CHARS as u32,
    }))
}

/// An account-password slot's default label (B77): `account password of <account> on
/// <host>`, each control character as `?`, cut at a character boundary to the vault's 64
/// bytes.
pub fn account_password_label(account: &str, host: &str) -> String {
    let full: String = format!("account password of {account} on {host}")
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    let mut end = full.len().min(LABEL_MAX);
    while !full.is_char_boundary(end) {
        end -= 1;
    }
    full[..end].to_string()
}

/// The locked store's id and its vault, parsed — never opened — and read without a secret
/// (B30). A store in files has no ways in to change (`lock on` protects it); an interrupted
/// `lock on` or `lock off` is finished first (`lock_incomplete`); a store another account
/// can change is refused before its secret is asked (WI-475, d4a-h F1).
fn locked_vault(ctx: &Context) -> CoreResult<(StoreId, VaultFile)> {
    let root = ctx.config_root();
    let Some(file) = read_store_file(root)? else {
        return Err(not_locked(root));
    };
    match file.secrets {
        SecretsMode::Locked => {}
        SecretsMode::Locking => return Err(incomplete(root, LockPhase::Locking)),
        SecretsMode::Unlocking => return Err(incomplete(root, LockPhase::Unlocking)),
        SecretsMode::Rotating => return Err(incomplete(root, LockPhase::Rotating)),
        SecretsMode::Files => return Err(not_locked(root)),
    }
    cli_core::store_privacy::ensure_private(&ctx.store())?;
    let Some(id) = file.id else {
        return Err(CoreError::Cli(CliError::StoreFileUnreadable {
            path: root.join(STORE_FILE),
            detail: "it names no id".to_string(),
        }));
    };
    let bytes = read_vault(&ctx.store(), &id)?;
    let parsed = VaultFile::parse(&bytes).map_err(|e| vault_err(root, e))?;
    Ok((id, parsed))
}

/// B19, M3: under the held lock, the store still locked under `id`, and its vault as it is
/// now, opened with `opened`'s data key — a vault replaced since (a key change, or `lock
/// off` and `lock on`) is `store_changed`, never damage.
fn reread(ctx: &Context, id: &StoreId, opened: &OpenedVault) -> CoreResult<UnlockedVault> {
    let root = ctx.config_root();
    let now = read_store_file(root)?;
    let same =
        matches!(&now, Some(f) if f.secrets == SecretsMode::Locked && f.id.as_ref() == Some(id));
    if !same {
        return Err(CoreError::Cli(CliError::StoreChanged {
            store: root.to_path_buf(),
            prepared: SecretsModeKind::Locked,
            now: now.map_or(SecretsModeKind::Files, |f| f.secrets.into()),
        }));
    }
    let bytes = read_vault(&ctx.store(), id)?;
    VaultFile::parse(&bytes)
        .and_then(|parsed| parsed.open_with_key(&opened.key, opened.vault_id))
        .map_err(|e| vault_err(root, e))
}

/// `store.yaml`'s slot list as `header` has it (B42: display only).
fn mirror(root: &Path, held: &StoreLock, header: &VaultHeader) -> CoreResult<()> {
    if let Some(mut file) = read_store_file(root)? {
        file.slots = header
            .slots
            .iter()
            .map(|slot| SlotInfo {
                kind: slot.kind.into(),
                label: slot.label.clone(),
            })
            .collect();
        write_store_file(root, &file, held)?;
    }
    Ok(())
}

/// The new slot's own secret, to open the vault with once it is written (B20); `new` must
/// be of the plan's kind, and an account-password slot bound to the machine id the plan named
/// this computer's slot by (`planned`) — anything else is the caller's mistake, refused before
/// anything is written: a slot bound to another id would replace another slot than the plan
/// said, never open here, and still read back (critic round, finding 5).
fn reopener(
    new: &NewSlot,
    kind: SlotAddKind,
    planned: Option<&MachineId>,
) -> CoreResult<SlotSecret> {
    match (new, kind) {
        (NewSlot::Password { secret, .. }, SlotAddKind::Password) => {
            Ok(SlotSecret::Password(secret.clone()))
        }
        (
            NewSlot::OsPassword {
                secret, machine, ..
            },
            SlotAddKind::OsPassword,
        ) => {
            if planned.map(MachineId::bytes) != Some(machine.bytes()) {
                return Err(CoreError::Cli(CliError::Other(
                    "the account-password slot given is bound to another machine id than the \
                     plan's; nothing was written"
                        .to_string(),
                )));
            }
            Ok(SlotSecret::AccountPassword(secret.clone(), machine.clone()))
        }
        _ => Err(CoreError::Cli(CliError::Other(format!(
            "a `{}` slot was planned, and another kind was given to add; nothing was written",
            kind.as_str()
        )))),
    }
}

/// `add_slot`'s refusal of the new slot, in its kind's words: a lock password's O2 refusal
/// is `password_refused`, an account password's `account_password_refused` (B75: an empty
/// one is no password); anything else is the vault's.
fn add_err(root: &Path, kind: SlotAddKind, e: VaultError) -> CoreError {
    match (kind, e) {
        (SlotAddKind::Password, VaultError::PasswordRefused(reason)) => {
            CoreError::LockPasswordRefused { reason }
        }
        (SlotAddKind::OsPassword, VaultError::PasswordRefused(problem)) => {
            account_refused(unusable(problem))
        }
        (_, other) => vault_err(root, other),
    }
}

/// O2's refusal of an account password, as `account_password_refused` says it.
fn unusable(problem: PasswordProblem) -> AccountPasswordRefusal {
    match problem {
        PasswordProblem::Empty => AccountPasswordRefusal::NoPassword,
        PasswordProblem::TooLong { max } => AccountPasswordRefusal::Unusable {
            why: AccountPasswordUnusable::TooLong { max },
        },
        PasswordProblem::RecoveryKeyShape => AccountPasswordRefusal::Unusable {
            why: AccountPasswordUnusable::RecoveryKeyShape,
        },
    }
}

/// The store's write lock, held (T28).
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

fn cancelled<T>() -> Outcome<T> {
    Outcome::Cancelled {
        cleaned: Vec::new(),
        left: Vec::new(),
    }
}

fn slot_refused(root: &Path, reason: SlotRefusal) -> CoreError {
    CoreError::Cli(CliError::SlotRefused {
        store: root.to_path_buf(),
        reason,
    })
}

fn account_refused(reason: AccountPasswordRefusal) -> CoreError {
    CoreError::Cli(CliError::AccountPasswordRefused {
        reason,
        messages: Vec::new(),
    })
}

fn incomplete(root: &Path, phase: LockPhase) -> CoreError {
    CoreError::Cli(CliError::LockIncomplete {
        store: root.to_path_buf(),
        phase,
    })
}

/// A store in files has no ways in to change: `lock on` protects it.
fn not_locked(root: &Path) -> CoreError {
    CoreError::Cli(CliError::UsageRefused {
        message: format!(
            "the target store at {} is not locked, so it has no ways in to change",
            root.display()
        ),
        help: "`apprafter lock on` protects it: it sets up the ways in, and a recovery key."
            .to_string(),
    })
}

/// What [`execute_slot_remove`] carries out. It never crosses IPC (`Plan::payload`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRemovePayload {
    id: StoreId,
    slot: SlotRef,
    slot_id: SlotId,
}

impl SlotRemovePayload {
    /// The store whose vault the opener's secret opens.
    pub fn store_id(&self) -> &StoreId {
        &self.id
    }

    /// The slot the removal takes out.
    pub fn slot(&self) -> &SlotRef {
        &self.slot
    }
}

/// What a run of `lock slot remove` did. `Serialize` for the desktop's flow (its TypeScript
/// export is d4b-c4's); the CLI prints text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotRemoved {
    pub store: String,
    pub store_id: String,
    pub removed: SlotRef,
    /// The slots the store opens with now, in the vault's order.
    pub remains: Vec<SlotRef>,
    /// The slot whose fresh secret allowed it (B60).
    pub opened_with: SlotRef,
}

/// What a removal leaves as it was (B21), in its plan: it never changes the keys — the store
/// key inside the vault included, which only a key change replaces (O-b9; critic round,
/// finding 13).
const REMOVE_KEEPS: &str = "its keys stay as they are: a copy of the vault made before this \
     still opens with this slot's secret, the data key that secret gave still opens this \
     vault and the store key inside it, which decrypts the cached kubeconfigs and Argo CD \
     passwords, and an unlock window opened with it stays open until it ends — if you think \
     the secret leaked, `apprafter lock rekey` replaces the keys";

/// What removing an account-password slot leaves, too (B21, Q1).
const REMOVE_ACCOUNT_PASSWORD: &str = "; copies of the vault made before this still let \
     someone test guesses of that account password — change it if that matters";

/// The plan of `lock slot remove <given>` (Destructive, B21): read-only, no secret. `given`
/// is 4 or more hex characters that begin exactly one slot's id, or a kind the store has
/// exactly one slot of (`password`, `os-password`, `recovery-key`); anything else is
/// `slot_not_found`, listing what it could mean. Refused before anything is asked: the
/// store's last recovery key (ADR 0068 §3: `lock recovery` replaces it), a removal that
/// leaves no slot this build opens (`no_usable_slot_left`), a store not locked. The plan
/// says what a removal does not do — it never changes the keys — and what the store opens
/// with after it (on this computer, from `facts`).
pub fn plan_slot_remove(
    ctx: &Context,
    given: &str,
    facts: &dyn SlotFacts,
) -> CoreResult<Plan<SlotRemovePayload>> {
    let root = ctx.config_root();
    let policy = super::kdf_policy(ctx);
    let (id, file) = locked_vault(ctx)?;
    let slot = resolve(root, file.header(), given)?;
    let removal = file.header().check_removal(slot.id, policy);
    removal.map_err(|e| vault_err(root, e))?;
    let mut keeps = REMOVE_KEEPS.to_string();
    if slot.kind == SlotKind::OsPassword {
        keeps.push_str(REMOVE_ACCOUNT_PASSWORD);
    }
    let after = remains_line(&file, slot.id, policy, facts);
    let targets = cli_core::list_target_names(&ctx.store())?;
    Ok(Plan {
        class: PlanClass::Destructive,
        title: title("Remove a way to open", root, &targets),
        changes: vec![
            change(
                "Slot",
                &slot.label,
                ChangeAction::Delete,
                "it stops opening this vault file",
            ),
            change(
                "Vault",
                &vault_path(root).display().to_string(),
                ChangeAction::Keep,
                &keeps,
            ),
            change("Slots", "after this", ChangeAction::Keep, &after),
        ],
        payload: SlotRemovePayload {
            id,
            slot: SlotRef::from(slot),
            slot_id: slot.id,
        },
    })
}

/// Carries out `plan` with `opened`, the vault opened in this process by a fresh secret of
/// any slot (B60): under the held lock the vault is read again and opened with its key
/// (`store_changed` when it was replaced since); a slot another process removed meanwhile is
/// `slot_not_found`; the vault's own rules are checked again (`remove_slot`); one durable
/// replace; the root synced; read back, the slot gone and the payload opening with the same
/// key; `store.yaml` rewritten. `Cancelled`, with nothing written, until the lock is held.
pub fn execute_slot_remove(
    ctx: &Context,
    plan: Plan<SlotRemovePayload>,
    opened: OpenedVault,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<SlotRemoved>> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let payload = plan.payload;
    let root = ctx.config_root();
    let store = ctx.store();
    let policy = super::kdf_policy(ctx);
    let held = held_lock(ctx, reporter)?;
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let mut vault = reread(ctx, &payload.id, &opened)?;
    let still_there = vault
        .header()
        .slots
        .iter()
        .any(|slot| slot.id == payload.slot_id);
    if !still_there {
        return Err(CoreError::Cli(CliError::SlotNotFound {
            store: root.to_path_buf(),
            given: payload.slot.id.clone(),
            matches: Vec::new(),
        }));
    }
    vault
        .remove_slot(payload.slot_id, policy)
        .map_err(|e| vault_err(root, e))?;
    let writer = ctx.vault_access().writer();
    let sealed = vault
        .seal(&writer, &mut OsRandom)
        .map_err(|e| vault_err(root, e))?;
    faults::hit(ctx, FaultPoint::BeforeSlotWrite)?;
    vault::file::replace(&vault_path(root), &sealed, &held).map_err(|e| vault_err(root, e))?;
    #[cfg(test)]
    after_slot_write(root);
    faults::hit(ctx, FaultPoint::AfterSlotWrite)?;
    sync_root(&store)?;
    // B20: read back from disk — the slot gone, the payload opening with the same key.
    let disk = read_vault(&store, &payload.id)?;
    let kept = VaultFile::parse(&disk)
        .and_then(|back| back.open_with_key(&opened.key, opened.vault_id))
        .ok()
        .filter(|back| {
            back.header()
                .slots
                .iter()
                .all(|slot| slot.id != payload.slot_id)
        })
        .ok_or_else(|| CoreError::Cli(not_kept(&store, "the removal")))?;
    mirror(root, &held, kept.header())?;
    Ok(Outcome::Completed {
        result: SlotRemoved {
            store: root.display().to_string(),
            store_id: payload.id.to_string(),
            remains: kept.header().slots.iter().map(SlotRef::from).collect(),
            removed: payload.slot,
            opened_with: opened.opened_with().clone(),
        },
    })
}

/// The slot `given` names (B21): 4 or more hex characters that begin exactly one slot's
/// id, or a kind the store has exactly one slot of. Anything else is `slot_not_found`,
/// listing the slots it could mean.
fn resolve<'a>(root: &Path, header: &'a VaultHeader, given: &str) -> CoreResult<&'a SlotRecord> {
    let wanted = given.trim().to_ascii_lowercase();
    let hex_id = wanted.len() >= 4 && wanted.bytes().all(|b| b.is_ascii_hexdigit());
    let kind = match wanted.as_str() {
        "password" => Some(SlotKind::Password),
        "os-password" => Some(SlotKind::OsPassword),
        "recovery-key" => Some(SlotKind::RecoveryKey),
        _ => None,
    };
    let matches: Vec<&SlotRecord> = match kind {
        Some(kind) => header
            .slots
            .iter()
            .filter(|slot| slot.kind == kind)
            .collect(),
        None if hex_id => header
            .slots
            .iter()
            .filter(|slot| slot.id.to_string().starts_with(&wanted))
            .collect(),
        None => Vec::new(),
    };
    match matches.as_slice() {
        [one] => Ok(*one),
        _ => Err(CoreError::Cli(CliError::SlotNotFound {
            store: root.to_path_buf(),
            given: given.to_string(),
            matches: matches.iter().map(|slot| slot.id.to_string()).collect(),
        })),
    }
}

/// What the store opens with once `removed` is gone (B21, B68, B76): every other slot's
/// label — after the words that on this computer only the recovery key would open it, when
/// no other slot opens here.
fn remains_line(
    file: &VaultFile,
    removed: SlotId,
    policy: &KdfPolicy,
    facts: &dyn SlotFacts,
) -> String {
    let views = super::status::header_views(file.header(), file.slot_policy(policy), facts);
    let left: Vec<&SlotView> = file
        .header()
        .slots
        .iter()
        .zip(&views)
        .filter(|(slot, _)| slot.id != removed)
        .map(|(_, view)| view)
        .collect();
    let labels = left
        .iter()
        .map(|view| view.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if left
        .iter()
        .any(|view| view.kind != LockSlotKind::RecoveryKey && view.refused.is_none())
    {
        format!("the store then opens with: {labels}")
    } else {
        format!(
            "on this computer the store then opens only with the recovery key; it opens \
             with: {labels}"
        )
    }
}
