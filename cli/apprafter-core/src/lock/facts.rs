// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What only the client can find out about this computer (D.4b B30, B15, B76; ADR 0069 §9):
//! the core never calls the OS (B9, rule 6), so the slot flows and the lock report take these
//! facts from the client — the CLI and the desktop answer them over `apprafter-os-slots`
//! (d4b-e1a, d4b-g1b). A fact a client cannot ask — under the `APPRAFTER_SANDBOX` fence,
//! without a backend, in a test — stays unknown, and every surface says "not checked here",
//! never a pass. Every method has an "unknown" default, so a fact a later step adds (Windows
//! Hello's, d4b-w1) breaks no implementor.

use std::fmt;

use serde::Serialize;

use crate::secrets::StoreId;
use crate::vault::MachineId;
use crate::vault::SlotRecord;

/// The operating system the slot backend plays (B15): the scripted stand-in names the one it
/// plays, so S3's refusal (`not_on_this_os`) reads it here, never `cfg!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostOs {
    Linux,
    Macos,
    Windows,
}

impl HostOs {
    /// The OS this build runs on.
    pub fn this() -> HostOs {
        if cfg!(windows) {
            HostOs::Windows
        } else if cfg!(target_os = "macos") {
            HostOs::Macos
        } else {
            HostOs::Linux
        }
    }

    /// Whether the account password can open a store here (S3: Linux and macOS).
    pub fn has_account_password_slot(self) -> bool {
        !matches!(self, HostOs::Windows)
    }
}

/// This computer's machine id, as the client found it (B54, B76).
#[derive(Debug, Clone)]
pub enum MachineFact {
    /// Read: the bytes the vault binds an account-password slot to.
    Id(MachineId),
    /// This computer has none the vault can use: a container without `/etc/machine-id`,
    /// WSL 1.
    NoId,
    /// Not asked: under the fence, without a backend, in a test.
    Unknown,
}

impl MachineFact {
    /// The id, when it was read.
    pub fn id(&self) -> Option<&MachineId> {
        match self {
            MachineFact::Id(id) => Some(id),
            MachineFact::NoId | MachineFact::Unknown => None,
        }
    }
}

/// What only the client can find out about this computer (module docs).
pub trait SlotFacts: Send + Sync + fmt::Debug {
    /// The OS the slot backend plays.
    fn os(&self) -> HostOs {
        HostOs::this()
    }

    /// This computer's machine id.
    fn machine_id(&self) -> MachineFact {
        MachineFact::Unknown
    }

    /// Whether this computer's machine id may change when it is re-created — a container or
    /// WSL (B76); `None` where the client cannot tell.
    fn machine_id_may_change(&self) -> Option<bool> {
        None
    }
}

/// Every fact unknown: what both clients pass until the os-slots glue answers them (d4b-e1a,
/// d4b-g1b, B81), and what a core test passes that asks nothing of this computer.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnknownFacts;

impl SlotFacts for UnknownFacts {}

/// Whether the system can check the account password here (D.4b B10, B88 (3)): libpam and
/// one of its services on Linux, the one PAM service that checks the typed text on macOS,
/// and an account for the user running AppRafter — on Linux in a process without no_new_privs
/// (WI-545). Read without asking the system to check anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckBackend {
    Ready,
    /// libpam does not load.
    NoLibpam,
    /// Linux: none of `common-auth`, `system-auth`, `login` is set up.
    NoService,
    /// macOS: no PAM service that checks the typed text (`chkpasswd`).
    NoCheckRoute,
    /// The user running AppRafter has no account entry.
    NoAccount,
    /// Linux: AppRafter runs with no_new_privs (a sandbox such as Flatpak or bubblewrap, a
    /// service started with `NoNewPrivileges=`): the system's set-uid password helper cannot
    /// read the password database, so the system would refuse even the right password and may
    /// count it (WI-545).
    NoNewPrivs,
}

/// The back-off across runs as it stands (D.4b B12, B74, B88 (6)), read without changing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackoffFact {
    Clear,
    /// A check failed recently: the next is allowed in `wait_secs` seconds; `None` while
    /// another AppRafter process is checking.
    Wait {
        wait_secs: Option<u64>,
    },
    /// Its record has no place that is this account's alone (`/run/user/<uid>` missing).
    NoRecord,
}

/// What this account's password status reads without privileges (D.4b B75).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordFact {
    /// A password is set and not locked.
    Usable,
    /// None, an empty one, or a locked one: no password the system checks opens the account.
    NoPassword,
    /// Not readable here (macOS tells an empty password from a set one only at a check).
    Unknown,
}

/// What only the client can find out about this computer's everyday way (D.4b B55, B88):
/// asked by `lock::plan_on` one fact at a time, in B88's order, stopping at the first that
/// holds — so under `APPRAFTER_SANDBOX`'s fence nothing past [`WayFacts::fenced`] is asked
/// (B82). The CLI's and the desktop's os-slots glue implement it (d4b-e1a, d4b-g2b). No
/// method has a default: an implementor answers every one, and none answers "unknown" for a
/// check it could not make — under the fence it says `fenced`.
pub trait WayFacts: SlotFacts {
    /// `APPRAFTER_SANDBOX`'s fence refuses this account a real backend (B16, B82).
    fn fenced(&self) -> bool;
    /// Whether the system can check the account password here.
    fn account_check(&self) -> CheckBackend;
    /// The back-off as it stands.
    fn backoff(&self) -> BackoffFact;
    /// This account's password status.
    fn password_status(&self) -> PasswordFact;
}

/// This computer's Windows Hello keys, as the client deletes them (D.4b B25; ADR 0069 §4):
/// the core never calls the OS. `lock off` hands the client each Windows Hello slot of the
/// vault it opened — after the files were written and read back, before the vault is
/// deleted — so no key of AppRafter's own outlives its vault unsaid (Windows Hello has no
/// list in which to find one later). d4b-w2 implements it over `apprafter-os-slots`; until
/// then every `Context` carries [`NoHelloKeys`]. It is called under the store's held lock:
/// it must not open a dialog (P-W10, as B84 says of R4b; critic round, finding 15).
pub trait HelloKeys: Send + Sync + fmt::Debug {
    /// Deletes this computer's key of `slot`, a Windows Hello slot of store `store`'s vault —
    /// the key's name is the client's (d4b-w1) — and says what became of it.
    fn delete(&self, store: &StoreId, slot: &SlotRecord) -> HelloKeyDeleted;
}

/// What became of this computer's key of one Windows Hello slot (D.4b B25): `lock off`
/// lists each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum HelloKeyDeleted {
    /// Deleted from this computer.
    Deleted,
    /// The slot was made on another computer (its label names it): its key stays there and
    /// still signs for copies of the vault made while the slot existed — "stays on" its
    /// computer's label (B25; critic round, finding 4).
    OtherComputer,
    /// This computer's key of the slot is gone already.
    NotHere,
    /// Not deleted, and `why`: the key still signs for copies of the vault made while its
    /// slot existed. Never a reason to keep the store from files (ADR 0068: a person can
    /// always return to files).
    Kept { why: String },
}

/// No Windows Hello in this client: what every `Context` builder carries until d4b-w2. A
/// Windows Hello slot's key — one a newer AppRafter made here — is kept, and `lock off`
/// lists it with why.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHelloKeys;

impl HelloKeys for NoHelloKeys {
    fn delete(&self, _store: &StoreId, _slot: &SlotRecord) -> HelloKeyDeleted {
        HelloKeyDeleted::Kept {
            why: "this AppRafter cannot reach Windows Hello".to_string(),
        }
    }
}

/// Tests: the facts a test sets (d4b-c1), on any OS.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct FixedFacts {
    pub os: HostOs,
    pub machine: MachineFact,
    pub may_change: Option<bool>,
}

#[cfg(test)]
impl FixedFacts {
    /// Linux, whose machine id is 16 bytes all `which`, not in a container.
    pub(crate) fn on(which: u8) -> FixedFacts {
        FixedFacts {
            os: HostOs::Linux,
            machine: MachineFact::Id(MachineId::new(&[which; 16]).unwrap()),
            may_change: Some(false),
        }
    }
}

#[cfg(test)]
impl SlotFacts for FixedFacts {
    fn os(&self) -> HostOs {
        self.os
    }

    fn machine_id(&self) -> MachineFact {
        self.machine.clone()
    }

    fn machine_id_may_change(&self) -> Option<bool> {
        self.may_change
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B30: a client that cannot ask answers "unknown", never a fact it made up; the OS is
    /// this build's.
    #[test]
    fn unknown_facts_answer_unknown() {
        let facts = UnknownFacts;
        assert!(matches!(facts.machine_id(), MachineFact::Unknown));
        assert!(facts.machine_id().id().is_none());
        assert_eq!(facts.machine_id_may_change(), None);
        assert_eq!(facts.os(), HostOs::this());
        assert_eq!(HostOs::this() == HostOs::Windows, cfg!(windows));
        assert_eq!(HostOs::this() == HostOs::Macos, cfg!(target_os = "macos"));
    }

    /// S3: the account password opens a store on Linux and macOS only.
    #[test]
    fn the_account_password_slot_is_for_linux_and_macos() {
        assert!(HostOs::Linux.has_account_password_slot());
        assert!(HostOs::Macos.has_account_password_slot());
        assert!(!HostOs::Windows.has_account_password_slot());
        assert!(MachineFact::Id(MachineId::new(&[7; 16]).unwrap())
            .id()
            .is_some());
        assert!(MachineFact::NoId.id().is_none());
    }
}
