// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock rekey` and `lock recovery` in the core: a key change (ADR 0069 §1 answers 4 and 9,
//! §7; D.4b overview §5.3, B63, B65, B68, B78–B80, B84, B85, B87). It replaces the vault's
//! data key, its vault id and the store's own age key, keeps the store id and every way in
//! proven in the run under its own slot id, and re-encrypts every cached kubeconfig and Argo
//! CD password under the new store key, in three phases:
//!
//! 1. a read-only plan (`plan_rotate`): no secret, no key derivation — every slot's fate
//!    (B68, B87), every state file loaded (B79), what the store key's re-encryption covers,
//!    the ids minted (B80);
//! 2. the questions, every one before the store lock (rule 4): a fresh secret of any slot
//!    opens the vault in this process (`begin_rotate`; never a holder's key or the app's, B19,
//!    B60), then a new lock password, each typed way in to keep (Enter asks again; only `drop`
//!    drops it), the recovery key kept or a new one shown with its slot's id and typed back —
//!    each answer checked against its slot at once;
//! 3. the steps (`execute_rotate`) under the store's held write lock, the vault read again
//!    and opened with the opener's data key — a vault replaced since is `store_changed`:
//!
//! ```text
//! R1  store.yaml: rotating, rotating_to, hello_drop; root synced    readers refuse from here
//! R2  vault::rotate → durable replace → root synced → read back, opened with every kept and
//!     new slot, the payload compared                    Cancel is honoured until here
//! R3  every folder of <root>/state/ re-sealed from the retiring store key to the new one,
//!     each state.json durable (lock::reseal under Rotate: nothing dropped, an unparsable
//!     file set aside)
//! R4  the retiring key out of the vault → durable replace → read back
//! R4b this computer's Windows Hello keys of dropped slots handed to the client, while
//!     store.yaml still lists them and the vault no longer has their slots
//! R5  store.yaml: locked, its slot list from the new vault; root synced
//! R6  the outcome: kept, replaced, added, dropped, caches re-sealed or left, folders named
//! ```
//!
//! An error before the new vault is durable puts `store.yaml` back to `locked` when the vault
//! on disk is still the old one (B84 b); from then on the store stays `rotating` for a finish
//! (d4b-c2b), and the error says which (`key_change_stopped`). Finishing an interrupted key
//! change, `lock off` from it and telling a running one from an interrupted one are
//! d4b-c2b's. Nothing here prompts or calls the OS: Windows Hello keys go through the
//! client's `HelloKeys` (d4b-c3).

use std::collections::HashSet;

use cli_core::target::TargetStorePaths;
use cli_core::{AccountCheckUnavailable, AccountPasswordRefusal};
use cli_state::{State, StatePaths};
use serde::Serialize;

use super::facts::{MachineFact, SlotFacts};
use super::on::{change, title, vault_err};
use super::reseal::state_folders;
use super::slots::{account_refused, locked_vault};
use super::{text, SlotRef};
use crate::context::Context;
use crate::error::CoreResult;
use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli_core::{CliError, LockPhase, SecretsModeKind, StoreLock};
use subtle::ConstantTimeEq;

use super::facts::HelloKeys;
use super::faults::{self, FaultPoint};
use super::off::HelloKeyGone;
use super::on::{DroppedCache, PASSWORD_LABEL, RECOVERY_LABEL};
use super::open::{check_password, open_at, OpenedVault};
use super::reseal::{orphan_line, reseal, set_aside_line, OldKeys, ResealMode, Resealed};
use super::slots::{cancelled, held_lock, incomplete, reread};
use super::{PasswordAdvice, PasswordProblem, RecoveryKeyProblem};
use crate::context::SecretString;
use crate::error::CoreError;
use crate::op::Outcome;
use crate::op::{ChangeAction, Plan, PlanClass, PlannedChange};
use crate::report::{Event, Reporter};
use crate::secrets::transfer;
use crate::secrets::{
    not_kept, read_store_file, read_vault, sync_root, vault_path, write_store_file, KeySession,
    Rotation, SecretsMode, SlotInfo, StoreFile,
};
use crate::secrets::{LockSlotKind, StoreId, STORE_FILE};
use crate::vault::{
    self, KdfPolicy, KeptMaterial, KeptSlot, NewSlot, RecoveryKey, SlotInput, SlotSecret,
    UnlockedVault, VaultError, VaultFile, VaultHeader,
};
use crate::vault::{
    MachineId, OsRandom, RotationIds, SlotBody, SlotId, SlotKind, SlotRecord, VaultId,
};
use crate::CancellationToken;
use cli_core::KeyChangeLeft;

use crate::secrets::vault_id_on_disk;

/// Which key change (B68; ADR 0069 §2 reading 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RotateKind {
    /// `lock rekey`: a new lock password, where the store has one; the recovery key kept if
    /// given, else a new one.
    Rekey,
    /// `lock recovery`: a new recovery key; the lock password kept if given.
    Recovery,
}

/// Why a key change drops a slot (B68, B87), said in its plan and its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DropWhy {
    /// The person typed `drop` at its question.
    Declined,
    /// An account-password slot made on another computer: its machine secret is not here.
    OtherComputer,
    /// This computer has no machine id an account-password slot could be opened with.
    NoMachineId,
    /// A Windows Hello slot: this AppRafter cannot confirm one (d4b-w1).
    GestureNotHere,
    /// Made by a newer AppRafter: Touch ID, an unknown kind, a newer layout.
    NewerKind,
    /// This build refuses its key settings (`kdf_too_weak`, B24).
    KdfRefused,
    /// A second slot of a typed kind, which no prompt tries: a forged or damaged header
    /// (B18, B77).
    SecondOfKind,
}

/// A slot's fate in a key change, listed before anything is asked (B68, B87).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(
    tag = "fate",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SlotFate {
    /// A new secret replaces it: `lock rekey`'s lock password, `lock recovery`'s recovery
    /// key.
    Replaced,
    /// Kept if it opens the store now or is typed at its question; Enter asks again, and
    /// only the word `drop` drops it (B87).
    KeptIfGiven,
    /// The recovery key at `lock rekey`: kept if it opens the store now or is typed at its
    /// question; Enter makes a new one.
    KeptOrNew,
    /// Dropped: it cannot be kept here.
    Dropped { why: DropWhy },
}

/// One slot of the vault and its fate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RotateRow {
    pub slot: SlotRef,
    pub fate: SlotFate,
}

/// What the store key's re-encryption covers (B65's plan line): the cached secrets sealed
/// to a key, the targets whose state folders hold them, and the folders no target uses that
/// hold them (B85).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreKeyLine {
    pub caches: u32,
    pub targets: u32,
    pub orphans: u32,
}

/// The ids a key change's plan mints (B6, B80; F4): the rotated vault's; the new recovery
/// key's slot, shown with the key when one is made; the new lock password's, where `lock
/// rekey` replaces one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MintedIds {
    pub(super) vault_id: VaultId,
    pub(super) recovery: SlotId,
    pub(super) password: Option<SlotId>,
}

/// What `execute_rotate` carries out. It never crosses IPC (`Plan::payload`). Not
/// `PartialEq`: it may hold this computer's machine id.
#[derive(Debug, Clone)]
pub struct RotatePayload {
    kind: RotateKind,
    id: StoreId,
    /// The vault the plan read: the opener must open this one (B19).
    vault_id: VaultId,
    /// This computer's machine id, which an account-password slot is opened and kept with.
    machine: Option<MachineId>,
    rows: Vec<RotateRow>,
    ids: MintedIds,
    store_key: StoreKeyLine,
}

impl RotatePayload {
    pub fn kind(&self) -> RotateKind {
        self.kind
    }

    /// The store whose vault the change replaces.
    pub fn store_id(&self) -> &StoreId {
        &self.id
    }

    /// Every slot of the vault, in its order, with its fate.
    pub fn rows(&self) -> &[RotateRow] {
        &self.rows
    }

    /// The rotated vault's id, 32 hex characters.
    pub fn new_vault_id(&self) -> String {
        self.ids.vault_id.to_string()
    }

    /// The new recovery key's slot id, 32 hex characters (B80): shown with the key, when one
    /// is made — `lock recovery` always, `lock rekey` on Enter at its question.
    pub fn recovery_slot_id(&self) -> String {
        self.ids.recovery.to_string()
    }

    /// Whether the change sets a new lock password (`lock rekey` on a store with one).
    pub fn new_lock_password(&self) -> bool {
        self.ids.password.is_some()
    }

    /// What the store key's re-encryption covers (B65).
    pub fn store_key(&self) -> StoreKeyLine {
        self.store_key
    }

    /// What a key change leaves exposed (B65): said in its plan and after it.
    pub fn leaves(&self) -> &'static str {
        text::KEY_CHANGE_LEAVES
    }
}

/// The plan of `lock rekey` or `lock recovery` (Bounded, ADR 0069 §11; B68): read-only — it
/// asks no secret, derives nothing and asks the client for no key. Refused first: a store
/// not locked — in files, or an earlier `lock on`, `lock off` or key change not finished
/// (`lock_incomplete`) — or one another account can change (WI-475); a weak key-derivation
/// policy outside its roots (T23); a vault with an account-password slot when the client did
/// not ask this computer's machine id (F12: never a guess that drops this computer's way
/// in); a state file that does not load (B79: `state::corrupt` naming it). Every slot gets
/// its fate (B68, B87); the ids are minted (B80).
pub fn plan_rotate(
    ctx: &Context,
    kind: RotateKind,
    facts: &dyn SlotFacts,
) -> CoreResult<Plan<RotatePayload>> {
    let root = ctx.config_root();
    let store = ctx.store();
    let (id, file) = locked_vault(ctx)?;
    super::permit(ctx)?;
    let header = file.header();
    let machine = match facts.machine_id() {
        MachineFact::Id(machine) => Some(machine),
        MachineFact::NoId => None,
        MachineFact::Unknown
            if header
                .slots
                .iter()
                .any(|slot| slot.kind == SlotKind::OsPassword) =>
        {
            return Err(account_refused(AccountPasswordRefusal::Unavailable {
                what: AccountCheckUnavailable::NoMachineId,
            }));
        }
        MachineFact::Unknown => None,
    };
    let refused: HashSet<SlotId> = file
        .slot_policy(super::kdf_policy(ctx))
        .into_iter()
        .filter(|(_, verdict)| verdict.is_err())
        .map(|(slot, _)| slot)
        .collect();
    let rows = fates(kind, &header.slots, machine.as_ref(), &refused);
    let store_key = survey_caches(&store)?;
    let new_password = rows
        .iter()
        .any(|row| row.slot.kind == LockSlotKind::Password && row.fate == SlotFate::Replaced);
    let minted = RotationIds::draw(1 + usize::from(new_password), &mut OsRandom)
        .map_err(|e| vault_err(root, e))?;
    let ids = MintedIds {
        vault_id: minted.vault_id,
        recovery: minted.new_slots[0],
        password: minted.new_slots.get(1).copied(),
    };
    let targets = cli_core::list_target_names(&store)?;
    let lead = match kind {
        RotateKind::Rekey => "Replace the keys of",
        RotateKind::Recovery => "Make a new recovery key for",
    };
    let mut changes = vec![
        change(
            "Vault",
            "vault key",
            ChangeAction::Replace,
            "a new data key and vault id; every way in that is kept is re-made under them, \
             under its own id",
        ),
        change(
            "Key",
            "the store's own key",
            ChangeAction::Replace,
            &text::store_key_line(store_key),
        ),
    ];
    changes.extend(rows.iter().map(row_change));
    changes.push(change(
        "StoreFile",
        STORE_FILE,
        ChangeAction::Update,
        "`secrets: rotating` until every cached secret is re-encrypted and the old store key is \
         out of the vault, then `locked`",
    ));
    Ok(Plan {
        class: PlanClass::Bounded,
        title: title(lead, root, &targets),
        changes,
        payload: RotatePayload {
            kind,
            id,
            vault_id: header.vault_id,
            machine,
            rows,
            ids,
            store_key,
        },
    })
}

/// Each slot's fate, in the vault's order (B68, B87; module docs). `machine` is this
/// computer's id (`None`: it has none); `refused` the slots whose key settings this build
/// refuses.
fn fates(
    kind: RotateKind,
    slots: &[SlotRecord],
    machine: Option<&MachineId>,
    refused: &HashSet<SlotId>,
) -> Vec<RotateRow> {
    let dropped = |why| SlotFate::Dropped { why };
    let (mut password, mut recovery, mut here) = (false, false, false);
    let mut rows = Vec::with_capacity(slots.len());
    for slot in slots {
        let fate = match slot.kind {
            // B18: one slot of each typed kind is tried; a second one is a forged or
            // damaged header.
            SlotKind::Password if password => dropped(DropWhy::SecondOfKind),
            SlotKind::Password => {
                password = true;
                match kind {
                    RotateKind::Rekey => SlotFate::Replaced,
                    RotateKind::Recovery if refused.contains(&slot.id) => {
                        dropped(DropWhy::KdfRefused)
                    }
                    RotateKind::Recovery => SlotFate::KeptIfGiven,
                }
            }
            SlotKind::RecoveryKey if recovery => dropped(DropWhy::SecondOfKind),
            SlotKind::RecoveryKey => {
                recovery = true;
                match kind {
                    RotateKind::Rekey => SlotFate::KeptOrNew,
                    RotateKind::Recovery => SlotFate::Replaced,
                }
            }
            SlotKind::OsPassword => {
                if matches!(slot.body, SlotBody::Opaque(_)) {
                    dropped(DropWhy::NewerKind)
                } else if let Some(machine) = machine {
                    if !slot.made_on(machine) {
                        dropped(DropWhy::OtherComputer)
                    } else if here {
                        dropped(DropWhy::SecondOfKind)
                    } else {
                        here = true;
                        if refused.contains(&slot.id) {
                            dropped(DropWhy::KdfRefused)
                        } else {
                            SlotFate::KeptIfGiven
                        }
                    }
                } else {
                    dropped(DropWhy::NoMachineId)
                }
            }
            // F9: opaque until d4b-w1, which confirms a kept one.
            SlotKind::WindowsHello => dropped(DropWhy::GestureNotHere),
            SlotKind::TouchId | SlotKind::Unknown(_) => dropped(DropWhy::NewerKind),
        };
        rows.push(RotateRow {
            slot: SlotRef::from(slot),
            fate,
        });
    }
    rows
}

/// B79, B85: every folder of `<root>/state/` read as R3 will read it — no file, or a path
/// through a file, is nothing; any other read error, or content that does not parse,
/// refuses the plan naming the file (`state::corrupt`: `apprafter import --force` rebuilds
/// it) — and what the store key's re-encryption covers.
fn survey_caches(store: &TargetStorePaths) -> CoreResult<StoreKeyLine> {
    let mut line = StoreKeyLine::default();
    for folder in state_folders(store)? {
        let paths = StatePaths::for_active_target(store, &folder.name);
        let Some(state) = State::read_existing(&paths)? else {
            continue;
        };
        let caches = state.hetzner_cloud.map_or(0, |h| {
            u32::from(h.kubeconfig_age.is_some()) + u32::from(h.argocd_admin_password_age.is_some())
        });
        if caches == 0 {
            continue;
        }
        line.caches += caches;
        if folder.orphan {
            line.orphans += 1;
        } else {
            line.targets += 1;
        }
    }
    Ok(line)
}

/// A row as the plan lists it.
fn row_change(row: &RotateRow) -> PlannedChange {
    let action = match row.fate {
        SlotFate::Replaced => ChangeAction::Replace,
        SlotFate::KeptIfGiven | SlotFate::KeptOrNew => ChangeAction::Keep,
        SlotFate::Dropped { .. } => ChangeAction::Delete,
    };
    change(
        "Slot",
        &row.slot.label,
        action,
        &text::fate_line(row.fate, row.slot.kind),
    )
}

/// The word that drops a typed way in at its question (B87).
pub const DROP: &str = "drop";

/// B87: whether dropping a way in at its question takes the word `drop`. A typed way in's
/// always does — Enter asks again, so a stray key never drops one. A cancelled Windows
/// Hello confirmation drops it, except where it is the store's last way in besides the
/// recovery key (ADR 0069 §2 reading 4); d4b-w1's question goes through this same rule.
pub fn drop_takes_the_word(kind: LockSlotKind, only_recovery_after_drop: bool) -> bool {
    match kind {
        LockSlotKind::WindowsHello => only_recovery_after_drop,
        _ => true,
    }
}

/// What a key change asks next, before the store lock (rule 4).
#[derive(Debug)]
pub enum RotateQuestion {
    /// `lock rekey` on a store with a lock password: a new one, which the client asks twice
    /// (O2: a short one is said, never refused; S21).
    NewLockPassword,
    /// A typed way in to keep (B68, B87): its secret keeps it, `drop` drops it, Enter asks
    /// again (`text::KEEP_OR_DROP`). `only_recovery_after_drop`: dropping it leaves only the
    /// recovery key — said first (`text::ONLY_RECOVERY_AFTER_DROP`).
    Keep {
        slot: SlotRef,
        only_recovery_after_drop: bool,
    },
    /// `lock rekey`: the recovery key — typed, it is kept (typing it exposes it like any
    /// typed secret); Enter makes a new one.
    KeepRecoveryKey { slot: SlotRef },
    /// A new recovery key (B80): say `text::NEW_KEY_VALID_WHEN_DONE`, show it with its
    /// slot's id, and have it typed back in full (T25, O12).
    NewRecoveryKey {
        shown: SecretString,
        slot_id: String,
    },
}

/// What the person gave at a question.
#[derive(Debug)]
pub enum RotateAnswer {
    /// The text typed (Enter: empty).
    Typed(SecretString),
    /// A confirmation's dialog was cancelled (Windows Hello's, d4b-w1).
    Cancelled,
}

/// What an answer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// Taken: ask the next question.
    Taken,
    /// Taken; the new lock password is short (O2): said, never refused.
    TakenShort(PasswordAdvice),
    /// Not taken: the same question again, and why.
    Again(Again),
}

/// Why a question is asked again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Again {
    /// Enter at a typed way in's question (B87): it is never dropped without the word.
    Empty,
    /// The secret typed does not open that slot.
    Wrong,
    /// A cancelled confirmation where dropping it takes the word `drop` (B87).
    TypeDrop,
    /// The new lock password, refused (O2).
    PasswordRefused(PasswordProblem),
    /// Not a recovery key.
    NotARecoveryKey(RecoveryKeyProblem),
    /// The recovery key typed back is not the one shown.
    NotTheKeyShown(RecoveryKeyProblem),
}

/// Where a row stands while the questions are asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settled {
    /// Kept if given: its question is still to come, or its secret opened the store.
    Pending,
    Kept,
    Replaced,
    Dropped(DropWhy),
}

impl Settled {
    fn of(fate: SlotFate) -> Settled {
        match fate {
            SlotFate::Replaced => Settled::Replaced,
            SlotFate::KeptIfGiven | SlotFate::KeptOrNew => Settled::Pending,
            SlotFate::Dropped { why } => Settled::Dropped(why),
        }
    }
}

/// A question still to ask.
enum Step {
    NewLockPassword,
    Keep(usize),
    KeepRecoveryKey(usize),
    NewRecoveryKey {
        key: RecoveryKey,
        shown: SecretString,
    },
}

/// A secret the written vault is opened with again (R2's read-back, rule 5):
/// `vault::rotate` consumes the proofs and the new slots (d4b-b2's hand-off), so the run
/// keeps its own copy.
struct Reopen {
    slot: SlotId,
    secret: ReopenSecret,
}

enum ReopenSecret {
    Password(SecretString),
    AccountPassword(SecretString, MachineId),
    /// The recovery key as typed or shown: `RecoveryKey` has no `Clone`.
    RecoveryKey(SecretString),
}

impl Reopen {
    fn secret(&self) -> CoreResult<SlotSecret> {
        Ok(match &self.secret {
            ReopenSecret::Password(text) => SlotSecret::Password(text.clone()),
            ReopenSecret::AccountPassword(text, machine) => {
                SlotSecret::AccountPassword(text.clone(), machine.clone())
            }
            ReopenSecret::RecoveryKey(text) => SlotSecret::RecoveryKey(
                RecoveryKey::parse(text.expose())
                    .map_err(|e| CoreError::RecoveryKeyInvalid { reason: e.into() })?,
            ),
        })
    }
}

/// The secrets a key change needs, all gathered before the store lock (rule 4): the vault
/// the opener's fresh secret opened in this process (B60), each kept slot proven against
/// it, the new lock password, the new recovery key typed back. Made only by
/// [`begin_rotate`], for its plan; no `Clone`; `..` in `Debug`. Dropped without
/// [`execute_rotate`] — the person stopped, the process died — it changed nothing, and a
/// recovery key it showed opens nothing (B80).
pub struct RotateMaterial {
    kind: RotateKind,
    ids: MintedIds,
    slots: Vec<SlotId>,
    root: PathBuf,
    policy: &'static KdfPolicy,
    machine: Option<MachineId>,
    opener: OpenedVault,
    vault: UnlockedVault,
    rows: Vec<(RotateRow, Settled)>,
    steps: VecDeque<Step>,
    kept: Vec<KeptSlot>,
    reopen: Vec<Reopen>,
    new_password: Option<SecretString>,
    new_key: Option<(RecoveryKey, SecretString)>,
}

impl fmt::Debug for RotateMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RotateMaterial(..)")
    }
}

/// What a run takes from its material.
struct Parts {
    /// The ids `vault::rotate` is given: the new lock password's and the new recovery
    /// key's, in that order, each when it is made (F4).
    ids: RotationIds,
    kept: Vec<KeptSlot>,
    new: Vec<NewSlot>,
    reopen: Vec<Reopen>,
    rows: Vec<(RotateRow, Settled)>,
    opened_with: SlotRef,
}

/// The questions of `plan`, once a fresh secret of any slot — `opener`, typed at the prompt
/// over the typed kinds (B60; `SlotInput::from_input` with the plan's machine id) — opened
/// the store's vault in this process (`lock::open_at`: never a holder's key, never the key
/// an unlocked app holds, B19). The opener's slot is kept, proven by its own key check (F3:
/// one more key derivation), unless the change replaces it. Refused: a vault written since
/// the plan — another vault id or another slot list (`store_changed`) — and a vault whose
/// payload still retires a store key (an earlier key change did not finish:
/// `lock_incomplete{rotating}`, d4b-c2b's finish). A wrong opener is the opening's own
/// refusal, for the client's prompt to ask again.
pub fn begin_rotate(
    ctx: &Context,
    plan: &Plan<RotatePayload>,
    opener: SecretString,
) -> CoreResult<RotateMaterial> {
    let payload = &plan.payload;
    let root = ctx.config_root().to_path_buf();
    let policy = super::kdf_policy(ctx);
    let input = SlotInput::from_input(opener.clone(), payload.machine.clone());
    let opened = open_at(&root, &payload.id, input, policy)?;
    let bytes = read_vault(&ctx.store(), &payload.id)?;
    let vault = VaultFile::parse(&bytes)
        .and_then(|parsed| parsed.open_with_key(&opened.key, opened.vault_id))
        .map_err(|e| vault_err(&root, e))?;
    if opened.vault_id != payload.vault_id || !same_slots(vault.header(), payload) {
        return Err(vault_replaced(&root));
    }
    if vault
        .payload()
        .retiring_identity()
        .map_err(|e| vault_err(&root, e))?
        .is_some()
    {
        return Err(incomplete(&root, LockPhase::Rotating));
    }
    let opened_at = payload
        .rows
        .iter()
        .position(|row| row.slot.id == opened.opened_with().id)
        .ok_or_else(|| misused("the slot that opened the vault is not in the plan"))?;
    let mut material = RotateMaterial {
        kind: payload.kind,
        ids: payload.ids,
        slots: vault.header().slots.iter().map(|slot| slot.id).collect(),
        root,
        policy,
        machine: payload.machine.clone(),
        opener: opened,
        vault,
        rows: payload
            .rows
            .iter()
            .map(|row| (row.clone(), Settled::of(row.fate)))
            .collect(),
        steps: VecDeque::new(),
        kept: Vec::new(),
        reopen: Vec::new(),
        new_password: None,
        new_key: None,
    };
    // B68: the opener's slot is kept, unless the change replaces it.
    if material.rows[opened_at].1 == Settled::Pending && !material.prove(opened_at, &opener)? {
        return Err(misused(
            "the secret that opened the vault does not prove its own slot",
        ));
    }
    if material.ids.password.is_some() {
        material.steps.push_back(Step::NewLockPassword);
    }
    for (at, (row, settled)) in material.rows.iter().enumerate() {
        if *settled == Settled::Pending && row.fate == SlotFate::KeptIfGiven {
            material.steps.push_back(Step::Keep(at));
        }
    }
    // F11: the recovery key last — a run stopped at an earlier question never showed a key.
    match material.kind {
        RotateKind::Rekey => {
            let pending = material.rows.iter().position(|(row, settled)| {
                row.fate == SlotFate::KeptOrNew && *settled == Settled::Pending
            });
            if let Some(at) = pending {
                material.steps.push_back(Step::KeepRecoveryKey(at));
            }
        }
        RotateKind::Recovery => {
            let step = material.new_key_step()?;
            material.steps.push_back(step);
        }
    }
    Ok(material)
}

impl RotateMaterial {
    /// What to ask next; `None` once every question is answered.
    pub fn question(&self) -> Option<RotateQuestion> {
        Some(match self.steps.front()? {
            Step::NewLockPassword => RotateQuestion::NewLockPassword,
            Step::Keep(at) => RotateQuestion::Keep {
                slot: self.rows[*at].0.slot.clone(),
                only_recovery_after_drop: self.only_recovery_after_drop(*at),
            },
            Step::KeepRecoveryKey(at) => RotateQuestion::KeepRecoveryKey {
                slot: self.rows[*at].0.slot.clone(),
            },
            Step::NewRecoveryKey { shown, .. } => RotateQuestion::NewRecoveryKey {
                shown: shown.clone(),
                slot_id: self.ids.recovery.to_string(),
            },
        })
    }

    /// Whether every question is answered: the material is ready for [`execute_rotate`].
    pub fn is_complete(&self) -> bool {
        self.steps.is_empty()
    }

    /// The answer to the question [`Self::question`] gave last (B68, B80, B87, O2). A
    /// typed secret is checked against its slot at once (`UnlockedVault::keep`: its key
    /// check, never PAM, B13); Enter at a typed way in's question asks again, and only the
    /// word [`DROP`] drops it; Enter at `lock rekey`'s recovery key makes a new one; a new
    /// recovery key counts once it is typed back in full.
    pub fn answer(&mut self, answer: RotateAnswer) -> CoreResult<Answered> {
        let Some(step) = self.steps.pop_front() else {
            return Err(misused(
                "a key change was given an answer with no question left",
            ));
        };
        match (step, answer) {
            (Step::NewLockPassword, RotateAnswer::Typed(password)) => {
                match check_password(&password) {
                    Ok(advice) => {
                        self.new_password = Some(password);
                        Ok(advice.map_or(Answered::Taken, Answered::TakenShort))
                    }
                    Err(CoreError::LockPasswordRefused { reason }) => {
                        self.steps.push_front(Step::NewLockPassword);
                        Ok(Answered::Again(Again::PasswordRefused(reason)))
                    }
                    Err(e) => Err(e),
                }
            }
            (Step::Keep(at), RotateAnswer::Typed(text)) => {
                if text.expose().is_empty() {
                    self.steps.push_front(Step::Keep(at));
                    return Ok(Answered::Again(Again::Empty));
                }
                if text.expose() == DROP {
                    self.rows[at].1 = Settled::Dropped(DropWhy::Declined);
                    return Ok(Answered::Taken);
                }
                if self.prove(at, &text)? {
                    Ok(Answered::Taken)
                } else {
                    self.steps.push_front(Step::Keep(at));
                    Ok(Answered::Again(Again::Wrong))
                }
            }
            (Step::Keep(at), RotateAnswer::Cancelled) => {
                let kind = self.rows[at].0.slot.kind;
                if drop_takes_the_word(kind, self.only_recovery_after_drop(at)) {
                    self.steps.push_front(Step::Keep(at));
                    Ok(Answered::Again(Again::TypeDrop))
                } else {
                    self.rows[at].1 = Settled::Dropped(DropWhy::Declined);
                    Ok(Answered::Taken)
                }
            }
            (Step::KeepRecoveryKey(at), RotateAnswer::Typed(text)) => {
                if text.expose().is_empty() {
                    self.rows[at].1 = Settled::Replaced;
                    let step = self.new_key_step()?;
                    self.steps.push_front(step);
                    return Ok(Answered::Taken);
                }
                if let Err(e) = RecoveryKey::parse(text.expose()) {
                    self.steps.push_front(Step::KeepRecoveryKey(at));
                    return Ok(Answered::Again(Again::NotARecoveryKey(e.into())));
                }
                if self.prove(at, &text)? {
                    Ok(Answered::Taken)
                } else {
                    self.steps.push_front(Step::KeepRecoveryKey(at));
                    Ok(Answered::Again(Again::Wrong))
                }
            }
            (Step::NewRecoveryKey { key, shown }, RotateAnswer::Typed(text)) => {
                match key.confirm(text.expose()) {
                    Ok(confirmed) => {
                        self.new_key = Some((confirmed.into_key(), shown));
                        Ok(Answered::Taken)
                    }
                    Err((key, e)) => {
                        self.steps.push_front(Step::NewRecoveryKey { key, shown });
                        Ok(Answered::Again(Again::NotTheKeyShown(e.into())))
                    }
                }
            }
            (step, RotateAnswer::Cancelled) => {
                self.steps.push_front(step);
                Err(misused(
                    "a cancelled confirmation answers no typed question",
                ))
            }
        }
    }

    /// Proves `text` is row `at`'s secret — its key check, and its wrap opens to this
    /// vault's data key (`UnlockedVault::keep`) — and keeps the slot; `false` when the
    /// check refuses it.
    fn prove(&mut self, at: usize, text: &SecretString) -> CoreResult<bool> {
        let id = self.slots[at];
        let (material, secret) = match self.rows[at].0.slot.kind {
            LockSlotKind::Password => (
                KeptMaterial::Password(text.clone()),
                ReopenSecret::Password(text.clone()),
            ),
            LockSlotKind::OsPassword => {
                let machine = self.machine.clone().ok_or_else(|| {
                    misused("an account-password slot was kept without this computer's id")
                })?;
                (
                    KeptMaterial::AccountPassword(text.clone(), machine.clone()),
                    ReopenSecret::AccountPassword(text.clone(), machine),
                )
            }
            LockSlotKind::RecoveryKey => (
                KeptMaterial::RecoveryKey(
                    RecoveryKey::parse(text.expose())
                        .map_err(|e| CoreError::RecoveryKeyInvalid { reason: e.into() })?,
                ),
                ReopenSecret::RecoveryKey(text.clone()),
            ),
            _ => return Err(misused("only a typed way in is kept by its secret")),
        };
        match self.vault.keep(id, material, self.policy) {
            Ok(kept) => {
                self.kept.push(kept);
                self.reopen.push(Reopen { slot: id, secret });
                self.rows[at].1 = Settled::Kept;
                Ok(true)
            }
            Err(VaultError::WrongSecret { .. }) => Ok(false),
            Err(e) => Err(vault_err(&self.root, e)),
        }
    }

    /// A new recovery key's question: the key drawn now, its display kept to show it.
    fn new_key_step(&self) -> CoreResult<Step> {
        let key = RecoveryKey::generate(&mut OsRandom).map_err(|e| vault_err(&self.root, e))?;
        let shown = key.to_display();
        Ok(Step::NewRecoveryKey { key, shown })
    }

    /// B68: whether dropping row `at` leaves only the recovery key — no new lock password,
    /// and no other way in kept or still to be asked.
    fn only_recovery_after_drop(&self, at: usize) -> bool {
        self.ids.password.is_none()
            && !self.rows.iter().enumerate().any(|(i, (row, settled))| {
                i != at
                    && row.slot.kind != LockSlotKind::RecoveryKey
                    && (*settled == Settled::Kept
                        || (*settled == Settled::Pending && row.fate == SlotFate::KeptIfGiven))
            })
    }

    /// Whether this is its plan's material, every question answered (B80: a recovery key
    /// shown belongs to the plan whose slot id it was shown with).
    fn fits(&self, payload: &RotatePayload) -> CoreResult<()> {
        if self.kind != payload.kind
            || self.ids != payload.ids
            || self.opener.vault_id != payload.vault_id
        {
            return Err(misused(
                "a key change was given the answers of another plan",
            ));
        }
        if !self.steps.is_empty() {
            return Err(misused(
                "a key change was asked to run with a question unanswered",
            ));
        }
        Ok(())
    }

    /// The run's parts (F4): the new lock password's and the new recovery key's slots, each
    /// under the id the plan minted for it, and every secret to read the vault back with.
    fn into_parts(self) -> Parts {
        let mut reopen = self.reopen;
        let mut new = Vec::new();
        let mut new_slots = Vec::new();
        if let (Some(password), Some(id)) = (self.new_password, self.ids.password) {
            reopen.push(Reopen {
                slot: id,
                secret: ReopenSecret::Password(password.clone()),
            });
            new.push(NewSlot::Password {
                secret: password,
                label: PASSWORD_LABEL.to_string(),
            });
            new_slots.push(id);
        }
        if let Some((key, shown)) = self.new_key {
            reopen.push(Reopen {
                slot: self.ids.recovery,
                secret: ReopenSecret::RecoveryKey(shown),
            });
            new.push(NewSlot::RecoveryKey {
                key,
                label: RECOVERY_LABEL.to_string(),
            });
            new_slots.push(self.ids.recovery);
        }
        Parts {
            ids: RotationIds {
                vault_id: self.ids.vault_id,
                new_slots,
            },
            kept: self.kept,
            new,
            reopen,
            rows: self.rows,
            opened_with: self.opener.opened_with().clone(),
        }
    }
}

/// What a key change did (R6). `Serialize` for the desktop's flow — its TypeScript export
/// is d4b-c4's; the CLI words it (d4b-e2), `text::KEY_CHANGE_LEAVES` (B65) and
/// `text::KEEP_PREVIOUS_RECOVERY_KEY` (B51) after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeysRotated {
    pub store: String,
    pub store_id: String,
    pub kind: RotateKind,
    /// The new vault's id, 32 hex characters: holders and the app follow it (B35, B38).
    pub vault_id: String,
    /// The slot whose fresh secret opened the store (B60).
    pub opened_with: SlotRef,
    /// Ways in kept, each under its own id (B6).
    pub kept: Vec<SlotRef>,
    /// Ways in a new secret replaced: `lock rekey`'s lock password, `lock recovery`'s and
    /// `lock rekey`'s-on-Enter recovery key.
    pub replaced: Vec<SlotRef>,
    /// The new lock password's and the new recovery key's slots.
    pub added: Vec<SlotRef>,
    pub dropped: Vec<DroppedSlot>,
    pub caches_resealed: u32,
    /// Caches no key of the change opened, or kubeconfigs in clear: left as they were, each
    /// named (B85).
    pub caches_left: Vec<DroppedCache>,
    /// B85's line for each state folder of no target the change re-sealed.
    pub orphans: Vec<String>,
    /// B79's line for each state file that did not parse, renamed aside.
    pub set_aside: Vec<String>,
    /// This computer's Windows Hello keys of dropped slots, handed to the client (R4b).
    pub hello_keys: Vec<HelloKeyGone>,
}

/// A slot a key change dropped, and why (B68).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedSlot {
    pub slot: SlotRef,
    pub why: DropWhy,
}

/// What every step under the lock works with.
struct Run<'a> {
    ctx: &'a Context,
    root: &'a Path,
    store: &'a TargetStorePaths,
    held: &'a StoreLock,
    id: &'a StoreId,
    kind: RotateKind,
    /// `store.yaml` as the lock found it: `locked`.
    before: &'a StoreFile,
    /// The vault's header as the lock found it: the old vault's.
    old: &'a VaultHeader,
    new_vault: VaultId,
    policy: &'static KdfPolicy,
    /// The old vault as the lock found it, opened: put back when the vault on disk is not
    /// one this run proved, before any cache moved.
    current: &'a UnlockedVault,
    /// What every vault this run writes says wrote it.
    writer: &'a str,
}

/// Tests: what [`AFTER_ROTATED_WRITE`] runs on the store root.
#[cfg(test)]
pub(super) type VaultHook = Box<dyn Fn(&Path)>;

#[cfg(test)]
thread_local! {
    /// Tests: runs on the store root right after R2's write, standing in for a vault on
    /// disk that does not hold what was written (R2's read-back must catch it).
    pub(super) static AFTER_ROTATED_WRITE: std::cell::RefCell<Option<VaultHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Carries out `plan` with `material` (module docs, R1–R6): `material` is
/// [`begin_rotate`]'s for this plan, every question answered. `Cancelled`, with nothing
/// written, until the store lock is held.
pub fn execute_rotate(
    ctx: &Context,
    plan: Plan<RotatePayload>,
    material: RotateMaterial,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<KeysRotated>> {
    let payload = plan.payload;
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    material.fits(&payload)?;
    super::permit(ctx)?;
    let root = ctx.config_root();
    let store = ctx.store();
    let held = held_lock(ctx, reporter)?;
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    // B19, M3: under the lock, the store still locked under its id and its vault as it is
    // now, opened with the opener's data key — another vault id is `store_changed` — with
    // the slots the plan listed: a slot written since is `store_changed`, a token written
    // since is kept (the payload is this read's).
    let current = reread(ctx, &payload.id, &material.opener)?;
    if !same_slots(current.header(), &payload) {
        return Err(vault_replaced(root));
    }
    if current
        .payload()
        .retiring_identity()
        .map_err(|e| vault_err(root, e))?
        .is_some()
    {
        return Err(incomplete(root, LockPhase::Rotating));
    }
    let before = read_store_file(root)?.ok_or_else(|| vault_replaced(root))?;
    let parts = material.into_parts();
    let writer = ctx.vault_access().writer();
    let policy = super::kdf_policy(ctx);
    let rotated = vault::rotate(
        &current,
        &parts.ids,
        parts.kept,
        parts.new,
        &writer,
        policy,
        &mut OsRandom,
    )
    .map_err(|e| vault_err(root, e))?;
    let hello_drop: Vec<SlotId> = current
        .header()
        .slots
        .iter()
        .filter(|slot| slot.kind == SlotKind::WindowsHello && rotated.dropped.contains(&slot.id))
        .map(|slot| slot.id)
        .collect();
    let run = Run {
        ctx,
        root,
        store: &store,
        held: &held,
        id: &payload.id,
        kind: payload.kind,
        before: &before,
        old: current.header(),
        new_vault: parts.ids.vault_id,
        policy,
        current: &current,
        writer: &writer,
    };
    // R1
    stage(reporter, 1, "Marking the key change");
    let rotating = StoreFile {
        secrets: SecretsMode::Rotating,
        rotation: Some(Rotation {
            to: run.new_vault,
            hello_drop: hello_drop.clone(),
        }),
        ..run.before.clone()
    };
    injected(FailAt::StoreFile)
        .and_then(|()| mark(&run, &rotating))
        .map_err(|cause| stopped_before_durable(&run, &cause))?;
    faults::hit(ctx, FaultPoint::AfterStoreFileRotating)?;
    // R2
    stage(reporter, 2, "Writing the new vault");
    // §5.3: Cancel is honoured until here: the store goes back to `locked` (B84 b).
    if cancel.is_cancelled() {
        return back_out_cancelled(&run);
    }
    write_rotated(&run, &rotated.bytes).map_err(|cause| stopped_before_durable(&run, &cause))?;
    faults::hit(ctx, FaultPoint::AfterVaultRotated)?;
    let disk =
        read_back(&run, &rotated.vault, &parts.reopen).map_err(|cause| put_back(&run, &cause))?;
    faults::hit(ctx, FaultPoint::AfterRotatedVerified)?;
    // R3
    stage(reporter, 3, "Re-encrypting cached secrets");
    let resealed =
        reseal_all(&run, &rotated.vault, disk).map_err(|cause| unfinished(&run, cause))?;
    // R4
    stage(reporter, 4, "Retiring the old store key");
    let retired = retire(&run, &rotated.vault, &writer)
        .map_err(|cause| retire_failed(&run, &rotated.vault, cause))?;
    faults::hit(ctx, FaultPoint::AfterRetiringDropped)?;
    // R4b
    stage(
        reporter,
        5,
        "Handing over Windows Hello keys of the ways in dropped",
    );
    let hello_keys = hand_over(&run, &hello_drop, retired.header());
    faults::hit(ctx, FaultPoint::AfterHelloKeysHandedOver)?;
    // R5
    stage(reporter, 6, "Finishing");
    let locked = StoreFile {
        secrets: SecretsMode::Locked,
        rotation: None,
        slots: slot_infos(retired.header()),
        ..run.before.clone()
    };
    mark(&run, &locked).map_err(|cause| unfinished(&run, cause))?;
    faults::hit(ctx, FaultPoint::AfterRotated)?;
    // R6
    Ok(Outcome::Completed {
        result: outcome(
            &run,
            parts.rows,
            &parts.ids.new_slots,
            parts.opened_with,
            retired.header(),
            resealed,
            hello_keys,
        ),
    })
}

/// `store.yaml` as `file`, then the store root synced: on disk before the next step (B51).
fn mark(run: &Run<'_>, file: &StoreFile) -> CoreResult<()> {
    write_store_file(run.root, file, run.held)?;
    sync_root(run.store)?;
    Ok(())
}

/// R2's write: the rotated vault by the durable replace (B78), then the root synced (B51).
/// Tests: [`AFTER_ROTATED_WRITE`] runs between the two (critic round, findings 1, 3, 7).
fn write_rotated(run: &Run<'_>, bytes: &[u8]) -> CoreResult<()> {
    injected(FailAt::VaultReplace)?;
    vault::file::replace(&vault_path(run.root), bytes, run.held)
        .map_err(|e| vault_err(run.root, e))?;
    #[cfg(test)]
    AFTER_ROTATED_WRITE.with(|hook| {
        if let Some(spoil) = hook.borrow().as_ref() {
            spoil(run.root);
        }
    });
    injected(FailAt::VaultSync)?;
    sync_root(run.store)?;
    Ok(())
}

/// R2's read-back (rule 5): the vault on disk is the one written — the new vault id, opened
/// with every kept and new slot's own secret, each opening its own slot, its payload as
/// written (the retiring store key in it). Returns the bytes R3's key reads its vault from.
fn read_back(run: &Run<'_>, written: &UnlockedVault, reopen: &[Reopen]) -> CoreResult<Arc<[u8]>> {
    let not_kept_here = || CoreError::Cli(not_kept(run.store, "the vault this key change wrote"));
    let disk = read_vault(run.store, run.id)?;
    let parsed = VaultFile::parse(&disk).map_err(|_| not_kept_here())?;
    if parsed.header().vault_id != run.new_vault {
        return Err(not_kept_here());
    }
    let want = written
        .payload()
        .encode()
        .map_err(|e| vault_err(run.root, e))?;
    for again in reopen {
        let opened = parsed
            .open(&again.secret()?, run.policy)
            .map_err(|_| not_kept_here())?;
        let got = opened
            .payload()
            .encode()
            .map_err(|e| vault_err(run.root, e))?;
        if opened.opened_by() != Some(again.slot) || !bool::from(got[..].ct_eq(&want[..])) {
            return Err(not_kept_here());
        }
    }
    Ok(disk)
}

/// R3 (B63, B78, B79, B85): every folder of `<root>/state/` — listed again under the lock,
/// as one may have appeared since the plan (F15) — re-sealed from the store key the change
/// retires to the new one, each `state.json` durable before the step goes on.
fn reseal_all(run: &Run<'_>, vault: &UnlockedVault, disk: Arc<[u8]>) -> CoreResult<Resealed> {
    let session = KeySession::new(
        vault.data_key().clone(),
        run.new_vault,
        run.root.to_path_buf(),
    );
    let key = transfer::store_key(run.root, session, disk);
    let retiring = vault
        .payload()
        .retiring_identity()
        .map_err(|e| vault_err(run.root, e))?
        .ok_or_else(|| misused("the rotated vault retires no store key"))?;
    let retiring = [retiring];
    injected(FailAt::Reseal)?;
    let mut done = Resealed::default();
    for (n, folder) in state_folders(run.store)?.iter().enumerate() {
        reseal(
            run.store,
            run.held,
            folder,
            &key,
            &OldKeys::Retiring(&retiring),
            ResealMode::Rotate,
            &mut done,
        )?;
        faults::hit(run.ctx, FaultPoint::AfterCachesRekeyed(n))?;
    }
    Ok(done)
}

/// R4 (B63): the rotated vault without its retiring store key — the same data key, vault
/// id and slots, so nothing is asked — by the durable replace, the root synced, read back
/// with the data key and compared.
fn retire(run: &Run<'_>, vault: &UnlockedVault, writer: &str) -> CoreResult<UnlockedVault> {
    let bytes = vault
        .retire_identity(writer, &mut OsRandom)
        .map_err(|e| vault_err(run.root, e))?;
    vault::file::replace(&vault_path(run.root), &bytes, run.held)
        .map_err(|e| vault_err(run.root, e))?;
    #[cfg(test)]
    AFTER_RETIRED_WRITE.with(|hook| {
        if let Some(spoil) = hook.borrow().as_ref() {
            spoil(run.root);
        }
    });
    sync_root(run.store)?;
    let not_kept_here = || {
        CoreError::Cli(not_kept(
            run.store,
            "the vault without the retired store key",
        ))
    };
    let disk = read_vault(run.store, run.id)?;
    let back = VaultFile::parse(&disk)
        .and_then(|parsed| parsed.open_with_key(vault.data_key(), run.new_vault))
        .map_err(|_| not_kept_here())?;
    let want = vault
        .payload()
        .retired()
        .and_then(|payload| payload.encode())
        .map_err(|e| vault_err(run.root, e))?;
    let got = back
        .payload()
        .encode()
        .map_err(|e| vault_err(run.root, e))?;
    let retires = back
        .payload()
        .retiring_identity()
        .map_err(|e| vault_err(run.root, e))?;
    if retires.is_some() || !bool::from(got[..].ct_eq(&want[..])) {
        return Err(not_kept_here());
    }
    Ok(back)
}

/// R4b (B84 a): this computer's key of each Windows Hello slot the change dropped, handed
/// to the client — after R4, while `store.yaml` still lists it, and only when the vault on
/// disk has no slot of that id (a listed id the vault still has is never deleted). A key
/// the client could not delete is listed, never a reason to stop (d4b-c3's rule for `lock
/// off`).
fn hand_over(run: &Run<'_>, hello_drop: &[SlotId], now: &VaultHeader) -> Vec<HelloKeyGone> {
    hello_drop
        .iter()
        .filter(|id| now.slots.iter().all(|slot| slot.id != **id))
        .filter_map(|id| run.old.slots.iter().find(|slot| slot.id == *id))
        .map(|slot| HelloKeyGone {
            slot: slot.id.to_string(),
            label: slot.label.clone(),
            deleted: HelloKeys::delete(&**run.ctx.hello_keys(), run.id, slot),
        })
        .collect()
}

/// `store.yaml`'s slot list as `header` has it (B42: display only).
fn slot_infos(header: &VaultHeader) -> Vec<SlotInfo> {
    header
        .slots
        .iter()
        .map(|slot| SlotInfo {
            kind: slot.kind.into(),
            label: slot.label.clone(),
        })
        .collect()
}

/// R6: what the change did, from the vault on disk (`now`) and each row as it ended.
fn outcome(
    run: &Run<'_>,
    rows: Vec<(RotateRow, Settled)>,
    added: &[SlotId],
    opened_with: SlotRef,
    now: &VaultHeader,
    resealed: Resealed,
    hello_keys: Vec<HelloKeyGone>,
) -> KeysRotated {
    let on_disk = |id: &str| {
        now.slots
            .iter()
            .find(|slot| slot.id.to_string() == id)
            .map(SlotRef::from)
    };
    let (mut kept, mut replaced, mut dropped) = (Vec::new(), Vec::new(), Vec::new());
    for (row, settled) in rows {
        match settled {
            Settled::Kept => kept.extend(on_disk(&row.slot.id)),
            Settled::Replaced => replaced.push(row.slot),
            Settled::Dropped(why) => dropped.push(DroppedSlot {
                slot: row.slot,
                why,
            }),
            // `fits` refuses a run with a question left.
            Settled::Pending => {}
        }
    }
    KeysRotated {
        store: run.root.display().to_string(),
        store_id: run.id.to_string(),
        kind: run.kind,
        vault_id: run.new_vault.to_string(),
        opened_with,
        kept,
        replaced,
        added: added
            .iter()
            .filter_map(|id| on_disk(&id.to_string()))
            .collect(),
        dropped,
        caches_resealed: resealed.resealed,
        caches_left: resealed.left,
        orphans: resealed
            .orphans
            .iter()
            .map(|path| orphan_line(path.as_path()))
            .collect(),
        set_aside: resealed.set_aside.iter().map(set_aside_line).collect(),
        hello_keys,
    }
}

/// Whether `header` holds the slots the plan listed, in its order (B19).
fn same_slots(header: &VaultHeader, payload: &RotatePayload) -> bool {
    header.slots.len() == payload.rows.len()
        && header
            .slots
            .iter()
            .zip(&payload.rows)
            .all(|(slot, row)| slot.id.to_string() == row.slot.id)
}

/// A vault replaced, or a slot written, since the plan: nothing was written.
fn vault_replaced(root: &Path) -> CoreError {
    CoreError::Cli(CliError::StoreChanged {
        store: root.to_path_buf(),
        prepared: SecretsModeKind::Locked,
        now: SecretsModeKind::Locked,
    })
}

/// A caller's mistake: refused, nothing written.
fn misused(what: &str) -> CoreError {
    CoreError::Cli(CliError::Other(format!("{what}; nothing was written")))
}

fn stage(reporter: &dyn Reporter, index: u32, title: &str) {
    reporter.report(Event::Stage {
        index,
        total: 6,
        title: title.to_string(),
    });
}

/// Where a test makes a step fail as an I/O error would (B84 b) — never a crash: a crash is
/// a `FaultPoint`, which no back-out follows (F5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailAt {
    /// R1's write of `store.yaml`.
    StoreFile,
    /// R2's replace, before the vault moves.
    VaultReplace,
    /// R2's root sync, after the vault moved.
    VaultSync,
    /// The back-out's own write of `store.yaml`.
    BackOut,
    /// R3, before its first folder.
    Reseal,
    /// Putting back the last vault this run proved (critic round, findings 1–3).
    Restore,
}

#[cfg(test)]
thread_local! {
    /// Tests: the steps that fail on this thread, each once.
    pub(super) static FAIL_AT: std::cell::RefCell<Vec<FailAt>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Tests: runs on the store root right after R4's write, standing in for a vault on
    /// disk that does not hold what was written (R4's read-back must catch it).
    pub(super) static AFTER_RETIRED_WRITE: std::cell::RefCell<Option<VaultHook>> =
        const { std::cell::RefCell::new(None) };
}

/// An injected I/O failure at `at`, once, when a test armed it; nothing in any other build.
fn injected(at: FailAt) -> CoreResult<()> {
    #[cfg(test)]
    if FAIL_AT.with(|armed| {
        let mut armed = armed.borrow_mut();
        let found = armed.iter().position(|point| *point == at);
        found.map(|i| armed.remove(i)).is_some()
    }) {
        return Err(CoreError::Cli(CliError::Io(std::io::Error::other(
            format!("injected failure at {at:?}"),
        ))));
    }
    #[cfg(not(test))]
    let _ = at;
    Ok(())
}

/// B84 (b): a step before R3 failed with `cause`, `store.yaml` saying `rotating`. The vault
/// on disk decides — read again, never assumed: the new one, durable, leaves the store
/// `rotating` for a finish (d4b-c2b); the old one, untouched, puts `store.yaml` back to
/// `locked`; anything else — no vault, another, or one that cannot be read — has the old
/// vault put back first, as no cache has moved yet (critic round, finding 3). The error
/// says which.
fn stopped_before_durable(run: &Run<'_>, cause: &dyn fmt::Display) -> CoreError {
    match vault_id_on_disk(run.root) {
        Ok(Some(now)) if now == run.new_vault => stopped(run, KeyChangeLeft::Unfinished, cause),
        Ok(Some(now)) if now == run.old.vault_id => back_out_with(run, cause),
        Ok(_) => put_back(
            run,
            &format!("{cause}; the vault on disk is neither the old one nor the new one"),
        ),
        Err(also) => put_back(
            run,
            &format!("{cause}; the vault on disk could not be read: {also}"),
        ),
    }
}

/// Never losing access (rule 5; critic round, findings 1 and 3): the vault on disk is not
/// one this run proved, and no cache has moved yet (R3 has not begun). The old vault —
/// sealed again from the copy read under the lock: its data key, vault id and every slot as
/// they were, so every earlier secret opens it — is put back and read back, then
/// `store.yaml` goes back to `locked`: the key change was not written. A restore that fails
/// leaves `rotating` over a vault nobody can vouch for: `unknown`, keep every secret.
fn put_back(run: &Run<'_>, cause: &dyn fmt::Display) -> CoreError {
    if let Err(also) = restore(run, run.current) {
        return stopped(
            run,
            KeyChangeLeft::Unknown,
            &format!("{cause}; putting the earlier vault back failed too: {also}"),
        );
    }
    back_out_with(run, cause)
}

/// `store.yaml` back to `locked` over the old vault, and said: not written, or the
/// back-out's own failure.
fn back_out_with(run: &Run<'_>, cause: &dyn fmt::Display) -> CoreError {
    match back_out(run) {
        Ok(()) => stopped(run, KeyChangeLeft::NotWritten, cause),
        Err(also) => stopped(
            run,
            KeyChangeLeft::BackOutFailed,
            &format!("{cause}; putting the lock state back failed too: {also}"),
        ),
    }
}

/// R4's write or its read-back failed (critic round, finding 2): the vault on disk may be
/// one no secret opens. R2's vault — every new and kept secret opens it, and it still holds
/// the retiring store key — is put back, so the store stays a key change a finish completes
/// (d4b-c2b redoes R4). A restore that fails is `unknown`.
fn retire_failed(run: &Run<'_>, rotated: &UnlockedVault, cause: CoreError) -> CoreError {
    match restore(run, rotated) {
        Ok(()) => unfinished(run, cause),
        Err(also) => stopped(
            run,
            KeyChangeLeft::Unknown,
            &format!("{cause}; putting the key change's vault back failed too: {also}"),
        ),
    }
}

/// `vault` — the last one this run proved — sealed again (a fresh payload nonce, its data
/// key, vault id, slots and payload as they are) and written over what is on disk by the
/// durable replace, the root synced, then read back with its own data key and compared.
fn restore(run: &Run<'_>, vault: &UnlockedVault) -> CoreResult<()> {
    injected(FailAt::Restore)?;
    let sealed = vault
        .seal(run.writer, &mut OsRandom)
        .map_err(|e| vault_err(run.root, e))?;
    vault::file::replace(&vault_path(run.root), &sealed, run.held)
        .map_err(|e| vault_err(run.root, e))?;
    sync_root(run.store)?;
    let not_kept_here =
        || CoreError::Cli(not_kept(run.store, "the vault this key change put back"));
    let disk = read_vault(run.store, run.id)?;
    let back = VaultFile::parse(&disk)
        .and_then(|parsed| parsed.open_with_key(vault.data_key(), vault.header().vault_id))
        .map_err(|_| not_kept_here())?;
    let want = vault
        .payload()
        .encode()
        .map_err(|e| vault_err(run.root, e))?;
    let got = back
        .payload()
        .encode()
        .map_err(|e| vault_err(run.root, e))?;
    if back.header().slots != vault.header().slots || !bool::from(got[..].ct_eq(&want[..])) {
        return Err(not_kept_here());
    }
    Ok(())
}

/// A step after the new vault is durable failed with `cause`: the store stays `rotating`,
/// and the retiring store key in the new vault lets a finish (d4b-c2b) re-seal what is
/// left.
fn unfinished(run: &Run<'_>, cause: CoreError) -> CoreError {
    stopped(run, KeyChangeLeft::Unfinished, &cause)
}

fn stopped(run: &Run<'_>, left: KeyChangeLeft, cause: &dyn fmt::Display) -> CoreError {
    CoreError::Cli(CliError::KeyChangeStopped {
        store: run.root.to_path_buf(),
        left,
        cause: cause.to_string(),
    })
}

/// B84 (b): `store.yaml` back to `locked`, its slot list from the old vault, synced.
fn back_out(run: &Run<'_>) -> CoreResult<()> {
    injected(FailAt::BackOut)?;
    let locked = StoreFile {
        secrets: SecretsMode::Locked,
        rotation: None,
        slots: slot_infos(run.old),
        ..run.before.clone()
    };
    mark(run, &locked)
}

/// §5.3: a cancel after R1 and before R2 — the vault on disk still the old one — puts the
/// store back; a back-out that fails leaves `rotating`, said.
fn back_out_cancelled(run: &Run<'_>) -> CoreResult<Outcome<KeysRotated>> {
    match back_out(run) {
        Ok(()) => Ok(Outcome::Cancelled {
            cleaned: vec![format!(
                "{STORE_FILE}: back to `locked`; the key change was not written, and a \
                 recovery key shown in this run is not valid"
            )],
            left: Vec::new(),
        }),
        Err(also) => Err(stopped(
            run,
            KeyChangeLeft::BackOutFailed,
            &format!("cancelled; putting the lock state back failed: {also}"),
        )),
    }
}
