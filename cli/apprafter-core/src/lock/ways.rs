// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock on`'s ways in (D.4b B55, B56, B74, B75, B88; ADR 0069 §5): which slot a new vault
//! opens with every day — this computer's account password (Linux, macOS) or Windows Hello
//! (d4b-w2) — or a separate lock password, decided when the run is planned, from the
//! client's facts, never from the person's answer or a typo.
//!
//! Without `--lock-password` a plan takes this computer's everyday way and falls back to a
//! lock password on facts only, checked in B88's order and named by the first that holds:
//! the `APPRAFTER_SANDBOX` fence; a build without Windows Hello; no check on this system; no
//! machine id to bind the slot to; no place for the back-off's record; no password on this
//! account. Each fact is one call to the client, made only when every earlier one was
//! clear, so under the fence the OS is asked nothing at all (B82). A back-off in force
//! refuses the run instead (B74): it exists only after a failed check, and falling back on
//! it would turn one typo into a store of another shape without a word. It is weighed after
//! every fallback, so a run that will check nothing is never held back by it.

use std::fmt;

use cli_core::{AccountPasswordRefusal, CliError};
use serde::Serialize;
use subtle::ConstantTimeEq;

use super::facts::{BackoffFact, CheckBackend, HostOs, MachineFact, PasswordFact, WayFacts};
use super::on::{LockOnPayload, PASSWORD_LABEL};
use super::open::{check_password, PasswordAdvice};
use super::SlotNote;
use crate::context::SecretString;
use crate::error::{CoreError, CoreResult};
use crate::op::Plan;
use crate::secrets::{LockSlotKind, SlotInfo};
use crate::vault::MachineId;
use crate::vault::{NewSlot, OsRandom, RecoveryKey, SlotSecret, VaultId};

/// What a run of `lock on` asks for (B55).
#[derive(Debug, Clone, Copy)]
pub enum WayAsked<'a> {
    /// This computer's everyday way, the default: with what the client found out about it.
    Everyday(&'a dyn WayFacts),
    /// A separate lock password (`--lock-password`; Protect's other choice): D.4a's flow.
    LockPassword,
}

/// The way a new vault's first slot takes (B55).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LockOnWay {
    /// This computer's account password, checked with the system once, bound to it.
    AccountPassword,
    /// Windows Hello on this computer (d4b-w2).
    WindowsHello,
    /// A separate lock password, which only AppRafter's prompt sees.
    LockPassword,
}

/// Why a plan takes a separate lock password instead of this computer's everyday way: B88's
/// reasons, declared in the order the plan checks them — (1) the fence, (2) the build, (3)
/// the system's check, (4) the session, (5) the machine id, (6) the back-off's record, (7)
/// the password — `no_account` beside the system's check, as d4b-d reads it. Never
/// `backoff`: a back-off in force refuses (B74).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WayFallback {
    /// (1) `APPRAFTER_SANDBOX`'s fence: the OS is asked nothing (B82).
    Sandbox,
    /// (2) Windows: this build has no Windows Hello slot yet (until d4b-w2).
    HelloNotBuilt,
    /// (3) libpam does not load.
    NoLibpam,
    /// (3) Linux: none of the PAM services a check may use is set up.
    NoService,
    /// (3) macOS: no PAM service that checks the typed text.
    NoCheckRoute,
    /// (3) The user running AppRafter has no account entry.
    NoAccount,
    /// (3) Windows Hello is not supported on this computer (d4b-w2).
    HelloNotSupported,
    /// (3) Windows Hello is not set up for this account (d4b-w2).
    HelloNotSetUp,
    /// (4) A remote session, where Windows Hello cannot be asked (d4b-w2).
    RemoteSession,
    /// (5) No machine id to bind an account-password slot to.
    NoMachineId,
    /// (6) No place that is this account's alone for the back-off's record: never a check
    /// without one.
    NoBackoffRecord,
    /// (7) No password on this account: a fact where readable, else the empty entry (B75).
    NoPassword,
}

/// The way a plan takes, with what its first slot needs (B55, B56).
#[derive(Debug, Clone)]
pub(super) struct Way {
    pub(super) way: LockOnWay,
    pub(super) fallback: Option<WayFallback>,
    /// The machine id an account-password slot is bound to (B54).
    pub(super) machine: Option<MachineId>,
    /// B75 (2): the password status could not be read, so an empty entry means "no
    /// password".
    pub(super) empty_entry_falls_back: bool,
    /// What the everyday slot costs and adds, said before its secret is asked (B61, B70,
    /// B76).
    pub(super) notes: Vec<SlotNote>,
}

impl Way {
    /// A separate lock password: asked for (no fallback), or fallen back to.
    pub(super) fn lock_password(fallback: Option<WayFallback>) -> Way {
        Way {
            way: LockOnWay::LockPassword,
            fallback,
            machine: None,
            empty_entry_falls_back: false,
            notes: Vec::new(),
        }
    }
}

/// The way `asked` takes (module docs): B88's order, one fact at a time, then B74's refusal.
pub(super) fn decide(asked: WayAsked<'_>) -> CoreResult<Way> {
    let facts = match asked {
        WayAsked::LockPassword => return Ok(Way::lock_password(None)),
        WayAsked::Everyday(facts) => facts,
    };
    if facts.fenced() {
        return fall_back(WayFallback::Sandbox);
    }
    if facts.os() == HostOs::Windows {
        return fall_back(WayFallback::HelloNotBuilt);
    }
    match facts.account_check() {
        CheckBackend::Ready => {}
        CheckBackend::NoLibpam => return fall_back(WayFallback::NoLibpam),
        CheckBackend::NoService => return fall_back(WayFallback::NoService),
        CheckBackend::NoCheckRoute => return fall_back(WayFallback::NoCheckRoute),
        CheckBackend::NoAccount => return fall_back(WayFallback::NoAccount),
    }
    let MachineFact::Id(machine) = facts.machine_id() else {
        return fall_back(WayFallback::NoMachineId);
    };
    let backoff = facts.backoff();
    if backoff == BackoffFact::NoRecord {
        return fall_back(WayFallback::NoBackoffRecord);
    }
    let status = facts.password_status();
    if status == PasswordFact::NoPassword {
        return fall_back(WayFallback::NoPassword);
    }
    if let BackoffFact::Wait { wait_secs } = backoff {
        return Err(CoreError::Cli(CliError::AccountPasswordRefused {
            reason: AccountPasswordRefusal::Backoff { wait_secs },
            messages: Vec::new(),
        }));
    }
    let mut notes = vec![SlotNote::AccountPasswordOffline, SlotNote::ManagesStore];
    if facts.machine_id_may_change() == Some(true) {
        notes.push(SlotNote::MachineIdMayChange);
    }
    Ok(Way {
        way: LockOnWay::AccountPassword,
        fallback: None,
        machine: Some(machine),
        empty_entry_falls_back: status == PasswordFact::Unknown,
        notes,
    })
}

/// A separate lock password, and why: the first of B88's reasons that held (B55).
fn fall_back(reason: WayFallback) -> CoreResult<Way> {
    Ok(Way::lock_password(Some(reason)))
}

/// The first slot's secret, as the client got it (D.4b B55, B56): what [`first_slot`] checks
/// for the plan's way.
pub enum FirstSecret<'a> {
    /// A new lock password, as the person chose it (asked twice by the client, O2).
    LockPassword(SecretString),
    /// This computer's account password as typed, once — an empty entry is B75 (2)'s "no
    /// password" where the plan says so (`empty_entry_falls_back`) — with the client's check
    /// with the system.
    AccountPassword {
        typed: SecretString,
        check: &'a dyn AccountPasswordCheck,
    },
}

/// A new vault's first slot, its secret checked for its plan's way (D.4b B55, B56). Its fields
/// are private and only this module makes one — [`first_slot`], once the check passed and
/// before any recovery key exists — so `execute_on` writes no first slot whose secret was not
/// checked, and none of another way than its plan's. `Debug` shows no secret.
pub struct FirstSlot {
    way: LockOnWay,
    slot: NewSlot,
    /// What step 3 opens the slot with when it reads the vault back (D.4a rule 4).
    secret: SlotSecret,
    /// O2's advice on its secret (B2), for the client's question after the check.
    advice: Option<PasswordAdvice>,
    /// The vault id of the plan whose step made it (B80, F13); `None` only for
    /// `lock_password_slot` called alone — D.4a's suites (F14).
    plan_vault: Option<VaultId>,
}

impl fmt::Debug for FirstSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FirstSlot({:?}, ..)", self.way)
    }
}

impl FirstSlot {
    /// The way it opens the store (B55).
    pub fn way(&self) -> LockOnWay {
        self.way
    }

    /// O2's advice on its secret (D.4b B2): `Some` below 15 characters, where the client asks
    /// "Add it anyway? [y/N]" before it shows the recovery key — read here, so no client
    /// keeps a copy of the account password for it (critic round, finding 3).
    pub fn advice(&self) -> Option<PasswordAdvice> {
        self.advice
    }

    /// The vault id of the plan whose step made it: `execute_on` writes it under that plan's
    /// ids only (critic round, finding 2).
    pub(super) fn plan_vault(&self) -> Option<VaultId> {
        self.plan_vault
    }

    /// Bound to `payload`'s plan (B80): the recovery key's slot id shown with it is the
    /// new vault's.
    fn of_plan(mut self, payload: &LockOnPayload) -> FirstSlot {
        self.plan_vault = payload.vault_id();
        self
    }

    /// Its kind and label, as `store.yaml` mirrors them.
    pub(super) fn info(&self) -> SlotInfo {
        let (kind, label) = match &self.slot {
            NewSlot::Password { label, .. } => (LockSlotKind::Password, label),
            NewSlot::OsPassword { label, .. } => (LockSlotKind::OsPassword, label),
            NewSlot::RecoveryKey { label, .. } => (LockSlotKind::RecoveryKey, label),
        };
        SlotInfo {
            kind,
            label: label.clone(),
        }
    }

    /// The slot to write, and the secret step 3 reads it back with.
    pub(super) fn into_parts(self) -> (NewSlot, SlotSecret) {
        (self.slot, self.secret)
    }
}

/// What [`first_slot`] made of the secret (D.4b B56, B75).
#[derive(Debug)]
pub enum FirstStep {
    /// Checked. The recovery key is drawn only now: show it with the plan's
    /// `recovery_slot_id`, have it typed back, then give both to `execute_on`
    /// (`LockOnInput::New`).
    Ready {
        first: FirstSlot,
        recovery: RecoveryKey,
    },
    /// B75 (2): an empty entry where this account's password status could not be read. The
    /// plan now takes a separate lock password — `fallback()` is `no_password`, its ids are
    /// the same — so say so and ask for one. The system was asked nothing.
    FellBack,
    /// A secret no slot of the plan's way holds (O2's rules; an empty account password where
    /// the account has one, Deviation 7): say it and ask again. Nothing was checked, drawn or
    /// written. For the empty account password the error is c1's `no_password`, whose
    /// message does not fit that case: the client says its own line (critic round, 11).
    Mistake(CoreError),
}

/// The first slot's step of a new `lock on` run (D.4b B55, B56, B75): `secret` checked for
/// `plan`'s way before any recovery key exists, so none is ever shown for a run whose check
/// fails. A lock password by O2's rules; this computer's account password (Task 5) by the
/// vault's rules, then by the client's check with the system. A secret the plan does not
/// take — another way's, or any for a plan that is no new run's — is `Err`, with nothing
/// asked, drawn or written.
pub fn first_slot(
    plan: &mut Plan<LockOnPayload>,
    secret: FirstSecret<'_>,
) -> CoreResult<FirstStep> {
    let Some(way) = plan.payload.way() else {
        return Err(misused("is no new run's"));
    };
    match secret {
        FirstSecret::LockPassword(password) => {
            if way != LockOnWay::LockPassword {
                return Err(misused("takes another way in than a lock password"));
            }
            match lock_password_slot(password) {
                Ok(first) => ready(first.of_plan(&plan.payload)),
                Err(refused) => mistake(refused),
            }
        }
        FirstSecret::AccountPassword { typed, check } => {
            if way != LockOnWay::AccountPassword {
                return Err(misused("takes another way in than the account password"));
            }
            account_password(plan, typed, check)
        }
    }
}

/// A lock password as a new vault's first slot (O2: refused only empty, longer than 1024
/// bytes or shaped like a recovery key — `lock_password_refused`). D.4a's suites take their
/// first slot from here (`on_tests::new_input`), as `first_slot` does for a lock-password
/// plan, which then binds it to the plan (`of_plan`).
pub(super) fn lock_password_slot(password: SecretString) -> CoreResult<FirstSlot> {
    let advice = check_password(&password)?;
    Ok(FirstSlot {
        way: LockOnWay::LockPassword,
        secret: SlotSecret::Password(password.clone()),
        slot: NewSlot::Password {
            secret: password,
            label: PASSWORD_LABEL.to_string(),
        },
        advice,
        plan_vault: None,
    })
}

/// The secret checked: the recovery key is drawn now, never before (B56).
fn ready(first: FirstSlot) -> CoreResult<FirstStep> {
    let recovery = RecoveryKey::generate(&mut OsRandom).map_err(|e| {
        CoreError::Cli(CliError::RandomUnavailable {
            detail: e.to_string(),
        })
    })?;
    Ok(FirstStep::Ready { first, recovery })
}

/// Said, and asked again: nothing was checked, drawn or written.
fn mistake(refused: CoreError) -> CoreResult<FirstStep> {
    Ok(FirstStep::Mistake(refused))
}

/// A client's slip — a plan given a secret it does not take — refused as a caller's bug.
fn misused(what: &str) -> CoreError {
    CoreError::Cli(CliError::Other(format!(
        "this `lock on` plan {what}; nothing was asked, drawn or written"
    )))
}

/// The system's check of this computer's account password, as the client makes it (D.4b
/// B10–B12, B56; ADR 0069 §9): d4b-e1a's and d4b-g2b's glue over `apprafter-os-slots` — the
/// core never calls the OS. [`first_slot`] calls it once a run at most, after the core's own
/// refusals, so no check is spent on a password no slot could hold. `Send + Sync + Debug`
/// as `SlotFacts` and `HelloKeys` are (critic round, finding 9): a `FirstSecret` may cross to
/// the desktop's blocking worker.
pub trait AccountPasswordCheck: Send + Sync + fmt::Debug {
    /// Checks `typed` with the system for the account running AppRafter — one check, under
    /// the back-off across runs — and, when the system took it, builds the slot: `typed`
    /// bound to `machine`, labelled by `lock::account_password_label` (only the glue builds
    /// an account-password slot, G-OS6).
    fn check(&self, typed: &SecretString, machine: &MachineId) -> AccountChecked;
}

/// What the system's check said (D.4b B55, B56, B74, B75): only `Verified` makes a slot.
pub enum AccountChecked {
    /// The system checked the typed text and took it: the slot to write.
    Verified(NewSlot),
    /// Refused — a wrong password, a stack that checked nothing (`not_checked`), a back-off
    /// in force by now, no check here after all: the run ends `account_password_refused`,
    /// nothing written, no recovery key drawn. `messages` are the system's own words.
    Refused {
        reason: AccountPasswordRefusal,
        messages: Vec<String>,
    },
}

/// This computer's account password as a new vault's first slot (D.4b B10, B11, B56, B75;
/// F2, F10, F11): an empty entry falls back where the password status could not be read,
/// before the system is asked anything; a password no slot holds is a mistake, said before
/// the system's check; then the client's check, once a run; and only the slot of the text
/// typed, bound to the plan's machine id, is taken.
fn account_password(
    plan: &mut Plan<LockOnPayload>,
    typed: SecretString,
    check: &dyn AccountPasswordCheck,
) -> CoreResult<FirstStep> {
    if typed.expose().is_empty() && plan.payload.empty_entry_falls_back() {
        super::on::fall_back(plan, WayFallback::NoPassword);
        return Ok(FirstStep::FellBack);
    }
    // B2 (critic round, finding 3): the pre-check's advice rides on the first slot.
    let advice = match super::check_account_password(&typed) {
        Ok(advice) => advice,
        Err(refused) => return mistake(refused),
    };
    let Some(machine) = plan.payload.machine().cloned() else {
        return Err(misused("holds no machine id for the account password"));
    };
    if !plan.payload.take_check() {
        return Err(misused(
            "had the account password checked once already: one check a run",
        ));
    }
    let slot = match check.check(&typed, &machine) {
        AccountChecked::Verified(slot) => slot,
        AccountChecked::Refused { reason, messages } => {
            return Err(CoreError::Cli(CliError::AccountPasswordRefused {
                reason,
                messages,
            }));
        }
    };
    // F2: the client built the slot (G-OS6); the core takes it only for the text typed and
    // this computer's machine id, each compared in constant time.
    let NewSlot::OsPassword {
        secret,
        machine: bound,
        ..
    } = &slot
    else {
        return Err(not_the_slot());
    };
    let same_text = bool::from(secret.expose().as_bytes().ct_eq(typed.expose().as_bytes()));
    let same_machine = bool::from(bound.bytes().ct_eq(machine.bytes()));
    if !(same_text && same_machine) {
        return Err(not_the_slot());
    }
    ready(FirstSlot {
        way: LockOnWay::AccountPassword,
        slot,
        secret: SlotSecret::AccountPassword(typed, machine),
        advice,
        plan_vault: plan.payload.vault_id(),
    })
}

/// A slot the client's check gave back that is not the one asked for (F2): never written.
fn not_the_slot() -> CoreError {
    CoreError::Cli(CliError::Other(
        "the account-password check gave back a slot that is not this computer's for the \
         password typed; nothing was written"
            .to_string(),
    ))
}
