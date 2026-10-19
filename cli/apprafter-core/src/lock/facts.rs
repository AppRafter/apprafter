// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What only the client can find out about this computer (D.4b B30, B15, B76; ADR 0069 §9):
//! the core never calls the OS (B9, rule 6), so the slot flows and the lock report take these
//! facts from the client — the CLI and the desktop answer them over `apprafter-os-slots`
//! (d4b-e1a, d4b-g1b). A fact a client cannot ask — under the `APPRAFTER_SANDBOX` fence,
//! without a backend, in a test — stays unknown, and every surface says "not checked here",
//! never a pass. Every method has an "unknown" default, so a fact a later step adds (Windows
//! Hello's, d4b-w1) breaks no implementor.

use std::fmt;

use crate::vault::MachineId;

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
