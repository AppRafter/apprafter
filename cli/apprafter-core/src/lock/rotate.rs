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
use crate::op::{ChangeAction, Plan, PlanClass, PlannedChange};
use crate::secrets::{LockSlotKind, StoreId, STORE_FILE};
use crate::vault::{
    MachineId, OsRandom, RotationIds, SlotBody, SlotId, SlotKind, SlotRecord, VaultId,
};

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
