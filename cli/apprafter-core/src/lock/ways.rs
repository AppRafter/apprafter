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

use cli_core::{AccountPasswordRefusal, CliError};
use serde::Serialize;

use super::facts::{BackoffFact, CheckBackend, HostOs, MachineFact, PasswordFact, WayFacts};
use super::SlotNote;
use crate::error::{CoreError, CoreResult};
use crate::vault::MachineId;

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
    #[expect(
        dead_code,
        reason = "read by `LockOnPayload::machine` from d4b-c3 Task 5 on"
    )]
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
