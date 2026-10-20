// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The lock's words for what it reports (ADR 0068 §7; D.4a §3.13, T72, T73): one sentence per
//! limit of this computer and one for a weakened holder, shared by `apprafter lock status`,
//! doctor's rows and `apprafter status`'s line, so that each limit reads the same wherever it is
//! shown. The desktop words its own Settings section; its doctor overlay shows doctor's rows.

use crate::lock::{DropWhy, SlotFate, StoreKeyLine};
use crate::lock::{
    HolderHardening, HolderUnavailableReason, MachineLimit, SlotNote, SlotRefusalView, SlotView,
    UnlockWindow,
};
use crate::secrets::LockSlotKind;

/// One limit as a surface states it: a warning or a fact, and its sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitLine {
    pub warn: bool,
    pub text: String,
}

/// `l`'s sentence; `None` for the holder base's fallback, which `lock status` puts on its
/// terminal line instead.
pub fn limit_line(l: &MachineLimit) -> Option<LimitLine> {
    let (warn, text) = match l {
        MachineLimit::X11Session => (
            true,
            "X11 session: any program running as you sees what you type, the lock password included"
                .to_string(),
        ),
        MachineLimit::Xwayland {
            terminal_is_x_client: Some(true),
        } => (
            true,
            "This terminal runs under Xwayland: other X programs see what you type into it, the \
             lock password included"
                .to_string(),
        ),
        MachineLimit::Xwayland { .. } => (
            false,
            "Xwayland is running: a terminal that runs under it shows what you type to other X \
             programs"
                .to_string(),
        ),
        MachineLimit::AppOnXwayland => (
            true,
            "AppRafter runs under Xwayland: other X programs see what you type into it, the lock \
             password included"
                .to_string(),
        ),
        MachineLimit::PtraceScope { value: Some(v) } => (
            true,
            format!("Yama ptrace_scope is {v}: programs running as you can attach to your shell"),
        ),
        MachineLimit::PtraceScope { value: None } => (
            true,
            "No Yama ptrace_scope: programs running as you can attach to your shell".to_string(),
        ),
        MachineLimit::WindowsSameUser => (
            true,
            "Unlock windows on Windows are open to every program running as you".to_string(),
        ),
        MachineLimit::MacosTaskPort => (
            false,
            "A program running as you with debugging rights can read an unlocked window's memory"
                .to_string(),
        ),
        MachineLimit::Multiplexer { name } => (
            false,
            format!("This terminal runs inside {name}: programs running as you can type into it"),
        ),
        MachineLimit::InheritedWindow => (
            false,
            "Everything started from this terminal can use its window, an AI agent included"
                .to_string(),
        ),
        MachineLimit::SleepUnreported => (
            true,
            "This computer does not tell the app when it sleeps or locks".to_string(),
        ),
        MachineLimit::Hibernation => (
            false,
            "Hibernation writes memory to disk, an open window's key included".to_string(),
        ),
        MachineLimit::SshKeyUsable {
            path,
            unencrypted,
            in_agent,
        } => (
            true,
            format!(
                "The SSH key {path} that reaches your nodes {}",
                match (unencrypted, in_agent) {
                    (true, Some(true)) => "has no passphrase and is loaded in ssh-agent",
                    (true, _) => "has no passphrase",
                    (false, Some(true)) => "is loaded in ssh-agent",
                    (false, _) => "may be loaded in ssh-agent, which did not answer within 2 s",
                }
            ),
        ),
        MachineLimit::RuntimeDirFallback { .. } => return None,
        MachineLimit::SecretsInUse => (
            false,
            "While a command talks to a cluster, its kubeconfig is a file programs running as \
             you can read"
                .to_string(),
        ),
        MachineLimit::SameUserPrograms => (
            false,
            "A program running as you that sets out to get past the lock can read what you \
             type, change what you run or use an unlocked window; run AI agents under a \
             separate OS account or in a sandbox"
                .to_string(),
        ),
    };
    Some(LimitLine { warn, text })
}

/// What a holder failed to do to protect its key (T54, T72), as one sentence; `None` when
/// nothing failed.
pub fn hardening_line(h: &HolderHardening) -> Option<String> {
    let mut weak = Vec::new();
    if !h.memory_locked {
        weak.push("its memory is not locked against paging");
    }
    if h.non_dumpable == Some(false) {
        weak.push("it can be dumped");
    }
    if h.deny_attach == Some(false) {
        weak.push("a debugger may attach to it");
    }
    if !h.core_off {
        weak.push("a crash may write it to disk");
    }
    if h.process_dacl == Some(false) {
        weak.push("its process keeps the default access list");
    }
    (!weak.is_empty()).then(|| {
        format!(
            "The unlock window's key is less protected than usual: {}",
            weak.join(", ")
        )
    })
}

/// The client's window on the store, as doctor's `secrets` row and `status`'s line say it.
pub fn window_words(window: &UnlockWindow) -> String {
    match window {
        UnlockWindow::Open { left_secs, .. } => {
            let left = if *left_secs < 60 {
                format!("{left_secs} s")
            } else {
                format!("{} min", left_secs.div_ceil(60))
            };
            format!("unlocked in this terminal, {left} left")
        }
        UnlockWindow::Closed => "no unlock window in this terminal".into(),
        UnlockWindow::NoTerminal => "not run from a terminal".into(),
        UnlockWindow::EveryCommand => "every command asks (the store's window is 0 min)".into(),
        // A holder that does not answer still holds the key: an open window, never "no window"
        // (rule 1), and not one a command can use — as `lock status` says it.
        UnlockWindow::Unavailable {
            reason: HolderUnavailableReason::NotResponding,
        } => "open in this terminal, but its holder does not answer: every command asks; \
              `apprafter lock now` ends it"
            .into(),
        UnlockWindow::Unavailable { .. } => {
            "no unlock window possible here (`apprafter lock status` says why)".into()
        }
        UnlockWindow::App => "unlocked in this app".into(),
    }
}

/// The kinds of a store's slots, in the vault's order, a slot this build refuses marked so.
pub fn slot_kinds(slots: &[SlotView]) -> String {
    slots
        .iter()
        .map(|s| {
            let kind = match s.kind {
                LockSlotKind::Password => "password",
                LockSlotKind::RecoveryKey => "recovery key",
                LockSlotKind::OsPassword => "account password",
                LockSlotKind::WindowsHello => "Windows Hello",
                LockSlotKind::TouchId => "Touch ID",
                LockSlotKind::Unknown => "a slot this AppRafter does not know",
            };
            match s.refused {
                Some(SlotRefusalView::GestureNotHere) => format!("{kind} (not on this computer)"),
                Some(SlotRefusalView::NewerKind) => format!("{kind} (made by a newer AppRafter)"),
                Some(SlotRefusalView::SecondOfKind) => {
                    format!("{kind} (a second one, never tried)")
                }
                Some(_) => format!("{kind} (refused by this build)"),
                None => kind.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// "2 slots: password, recovery key".
pub fn slot_count(slots: &[SlotView]) -> String {
    let n = slots.len();
    format!(
        "{n} {}: {}",
        if n == 1 { "slot" } else { "slots" },
        slot_kinds(slots)
    )
}

/// A slot's id as surfaces show it: its first 8 hex characters (D.4b B30, B80).
pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// One slot note as `lock slot list`, `lock status` and doctor say it (D.4b B41, B61, B70,
/// B76; rule 1): a clause after the slot's label.
pub fn note_line(note: &SlotNote) -> &'static str {
    match note {
        SlotNote::AccountPasswordOffline => {
            "opens the store only on the computer that made it; anyone with a copy of the vault \
             and that computer's id can try to guess it offline"
        }
        SlotNote::AccountPasswordChanged => {
            "your account password changed after it was added: it still opens with the \
             previous one"
        }
        SlotNote::OsPasswordOtherComputer => {
            "made on another computer, or before this computer's id changed: it does not open \
             here"
        }
        SlotNote::ManagesStore => "whoever knows it can also change how the store opens",
        SlotNote::HelloPromptUnnamed => "its Windows Hello dialog does not say which program asks",
        SlotNote::HelloOtherComputer => {
            "made on another computer: its Windows Hello key stays there"
        }
        SlotNote::HelloKeyMissing => "its Windows Hello key is gone from this computer",
        SlotNote::HelloSoftwareKey => {
            "this computer keeps its Windows Hello key in software: a copy of its disk lets \
             someone guess the PIN offline"
        }
        SlotNote::NotCheckedHere => {
            "not checked here: this AppRafter could not ask this computer whether it opens here"
        }
        SlotNote::MachineIdMayChange => {
            "this looks like a container or WSL: its machine id may change when it is re-created"
        }
    }
}

/// B61, in full: what an account-password slot lets its holder do.
const ACCOUNT_PASSWORD_MANAGES: &str = "Anyone who can open this store with your account \
     password — an agent you gave it to for `sudo`, or one that saw you type it — can also \
     change how it opens: give it a lock password and a recovery key only they know and remove \
     yours, which locks you out of it. Run agents under a separate OS user or in a sandbox; on \
     a computer where an agent gets your account password, use a separate lock password \
     instead.";
/// B61's Windows Hello sentence.
const HELLO_MANAGES: &str = "One approval in a Windows Hello dialog — which does not say which \
     program asks — lets that program open this store and change how it opens.";
/// Q1 with the machine binding (O-b1), B70 and B77, in full.
const ACCOUNT_PASSWORD_OFFLINE: &str = "Anyone with a copy of the vault and this computer's id \
     — any program running as you — can try to guess your account password offline; each \
     computer you add it on is one more such copy. On another computer, after you reinstall the \
     system, or if this computer's id changes, only the recovery key opens this store (and a \
     lock password, if you add one): keep the recovery key at hand, or add a lock password to a \
     store you use on more than one computer. After you change your account password, the old \
     one keeps opening the store until you add the new one.";
/// B76, in full.
const MACHINE_ID_MAY_CHANGE: &str = "This looks like a container or WSL: its machine id may \
     change when it is re-created, and then only the recovery key opens this store.";

/// What adding a slot of `kind` with `note` costs, as its plan says it before any secret is
/// asked (D.4b B61, Q1, B70, B76, B77; rule 1): the full sentence where the list's clause
/// would hide a cost.
pub fn plan_note_line(note: &SlotNote, kind: crate::secrets::LockSlotKind) -> &'static str {
    match (note, kind) {
        (SlotNote::ManagesStore, crate::secrets::LockSlotKind::WindowsHello) => HELLO_MANAGES,
        (SlotNote::ManagesStore, _) => ACCOUNT_PASSWORD_MANAGES,
        (SlotNote::AccountPasswordOffline, _) => ACCOUNT_PASSWORD_OFFLINE,
        (SlotNote::MachineIdMayChange, _) => MACHINE_ID_MAY_CHANGE,
        (other, _) => note_line(other),
    }
}

/// D.4b B65: what a key change leaves exposed, in its plan and after it.
pub const KEY_CHANGE_LEAVES: &str = "Copies of the vault made before this — a backup, a \
     snapshot — still open with the old secrets and give the old store key: it decrypts the \
     caches in copies of the targets' state files made before this, and the Hetzner tokens as \
     they are now. Caches written or re-encrypted from now on open only with the new store \
     key. To end the rest: renew the tokens (`apprafter target add <t> --renew`) and the \
     cluster credentials at their source, and delete earlier copies you can.";

/// D.4b B51: a key change's outcome ends with it.
pub const KEEP_PREVIOUS_RECOVERY_KEY: &str =
    "Keep the previous recovery key until this store has opened once with the new one.";

/// D.4b B80: said before a key change shows a new recovery key.
pub const NEW_KEY_VALID_WHEN_DONE: &str = "This recovery key opens the store only when \
     AppRafter says Done; keep your current recovery key until then.";

/// D.4b B87: a typed way in's question at a key change.
pub const KEEP_OR_DROP: &str = "Type it to keep it, or type `drop` to drop it.";

/// D.4b B68: said before a question whose drop would leave only the recovery key.
pub const ONLY_RECOVERY_AFTER_DROP: &str =
    "If you drop it, the store opens only with the recovery key.";

/// The store key's line in a key change's plan (B65): what its re-encryption covers.
pub fn store_key_line(line: StoreKeyLine) -> String {
    let count = |n: u32, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if line.caches == 0 {
        return "replaced; no cached secret to re-encrypt".to_string();
    }
    let mut of = Vec::new();
    if line.targets > 0 {
        of.push(count(line.targets, "target", "targets"));
    }
    if line.orphans > 0 {
        of.push(count(
            line.orphans,
            "state folder no target uses",
            "state folders no target uses",
        ));
    }
    format!(
        "replaced; {} re-encrypted, of {}",
        count(line.caches, "cached secret", "cached secrets"),
        of.join(" and of ")
    )
}

/// A slot's fate in a key change's plan (B68, B87).
pub fn fate_line(fate: SlotFate, kind: LockSlotKind) -> String {
    match fate {
        SlotFate::Replaced if kind == LockSlotKind::RecoveryKey => {
            "replaced: a new recovery key, shown once and typed back".into()
        }
        SlotFate::Replaced => "replaced: you set a new lock password".into(),
        SlotFate::KeptIfGiven => "kept if you open the store with it now, or type it when \
                                  asked; Enter asks again, and only typing `drop` drops it"
            .into(),
        SlotFate::KeptOrNew => "kept if you open the store with it now, or type it when \
                                asked; Enter makes a new one, shown once and typed back"
            .into(),
        SlotFate::Dropped { why } => format!("dropped: {}", drop_line(why)),
    }
}

/// Why a key change drops a slot (B68).
pub fn drop_line(why: DropWhy) -> &'static str {
    match why {
        DropWhy::Declined => "you typed `drop`",
        DropWhy::OtherComputer => {
            "made on another computer, whose machine id this one does not have"
        }
        DropWhy::NoMachineId => "this computer has no machine id to open it with",
        DropWhy::GestureNotHere => "this AppRafter cannot confirm Windows Hello",
        DropWhy::NewerKind => "made by a newer AppRafter",
        DropWhy::KdfRefused => "this AppRafter refuses its key settings",
        DropWhy::SecondOfKind => "a second one of its kind, which no prompt tries",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(warn: bool, text: &str) -> Option<LimitLine> {
        Some(LimitLine {
            warn,
            text: text.into(),
        })
    }

    /// The compiler's list of every limit: a new variant fails here until it has a row below.
    fn _every_variant_is_listed(l: &MachineLimit) {
        match l {
            MachineLimit::X11Session
            | MachineLimit::Xwayland { .. }
            | MachineLimit::AppOnXwayland
            | MachineLimit::PtraceScope { .. }
            | MachineLimit::WindowsSameUser
            | MachineLimit::MacosTaskPort
            | MachineLimit::Multiplexer { .. }
            | MachineLimit::InheritedWindow
            | MachineLimit::SleepUnreported
            | MachineLimit::Hibernation
            | MachineLimit::SshKeyUsable { .. }
            | MachineLimit::RuntimeDirFallback { .. }
            | MachineLimit::SecretsInUse
            | MachineLimit::SameUserPrograms => {}
        }
    }

    /// D.4a T72: every limit's weight and sentence, pinned — `lock status` prints them, and
    /// doctor's rows take them as their titles.
    #[test]
    fn every_limit_has_its_weight_and_sentence() {
        use MachineLimit as L;
        let cases = [
            (L::X11Session, line(true, "X11 session: any program running as you sees what you type, the lock password included")),
            (L::Xwayland { terminal_is_x_client: Some(true) }, line(true, "This terminal runs under Xwayland: other X programs see what you type into it, the lock password included")),
            (L::Xwayland { terminal_is_x_client: None }, line(false, "Xwayland is running: a terminal that runs under it shows what you type to other X programs")),
            (L::AppOnXwayland, line(true, "AppRafter runs under Xwayland: other X programs see what you type into it, the lock password included")),
            (L::PtraceScope { value: Some(0) }, line(true, "Yama ptrace_scope is 0: programs running as you can attach to your shell")),
            (L::PtraceScope { value: None }, line(true, "No Yama ptrace_scope: programs running as you can attach to your shell")),
            (L::WindowsSameUser, line(true, "Unlock windows on Windows are open to every program running as you")),
            (L::MacosTaskPort, line(false, "A program running as you with debugging rights can read an unlocked window's memory")),
            (L::Multiplexer { name: "tmux".into() }, line(false, "This terminal runs inside tmux: programs running as you can type into it")),
            (L::InheritedWindow, line(false, "Everything started from this terminal can use its window, an AI agent included")),
            (L::SleepUnreported, line(true, "This computer does not tell the app when it sleeps or locks")),
            (L::Hibernation, line(false, "Hibernation writes memory to disk, an open window's key included")),
            (L::SshKeyUsable { path: "~/.ssh/id_ed25519".into(), unencrypted: true, in_agent: Some(false) }, line(true, "The SSH key ~/.ssh/id_ed25519 that reaches your nodes has no passphrase")),
            (L::SshKeyUsable { path: "~/k".into(), unencrypted: true, in_agent: Some(true) }, line(true, "The SSH key ~/k that reaches your nodes has no passphrase and is loaded in ssh-agent")),
            (L::SshKeyUsable { path: "~/k".into(), unencrypted: false, in_agent: Some(true) }, line(true, "The SSH key ~/k that reaches your nodes is loaded in ssh-agent")),
            (L::SshKeyUsable { path: "~/k".into(), unencrypted: false, in_agent: None }, line(true, "The SSH key ~/k that reaches your nodes may be loaded in ssh-agent, which did not answer within 2 s")),
            (L::RuntimeDirFallback { path: "/s/run".into() }, None),
            (L::SecretsInUse, line(false, "While a command talks to a cluster, its kubeconfig is a file programs running as you can read")),
            (L::SameUserPrograms, line(false, "A program running as you that sets out to get past the lock can read what you type, change what you run or use an unlocked window; run AI agents under a separate OS account or in a sandbox")),
        ];
        for (limit, want) in cases {
            assert_eq!(limit_line(&limit), want, "{limit:?}");
        }
    }

    fn hardening(memory_locked: bool, non_dumpable: Option<bool>) -> HolderHardening {
        HolderHardening {
            non_dumpable,
            deny_attach: None,
            core_off: true,
            memory_locked,
            process_dacl: None,
        }
    }

    /// D.4a T54, T72: a holder that applied every protection it has is no line; each failed
    /// one is named, in one sentence.
    #[test]
    fn a_weakened_holder_is_one_sentence() {
        assert_eq!(hardening_line(&hardening(true, Some(true))), None);
        assert_eq!(
            hardening_line(&hardening(false, Some(true))).as_deref(),
            Some("The unlock window's key is less protected than usual: its memory is not locked against paging")
        );
        assert_eq!(
            hardening_line(&hardening(false, Some(false))).as_deref(),
            Some("The unlock window's key is less protected than usual: its memory is not locked against paging, it can be dumped")
        );
        // A crash dump the OS may still write (Windows Error Reporting; d4a-h Task 2): the same
        // words on every OS, since Windows has no core file.
        let crash = HolderHardening {
            core_off: false,
            ..hardening(true, Some(true))
        };
        assert_eq!(
            hardening_line(&crash).as_deref(),
            Some("The unlock window's key is less protected than usual: a crash may write it to disk")
        );
    }

    fn slot(kind: crate::secrets::LockSlotKind, refused: bool) -> crate::lock::SlotView {
        crate::lock::SlotView {
            id: None,
            kind,
            label: "x".into(),
            refused: refused.then_some(crate::lock::SlotRefusalView::KdfTooWeak),
            notes: Vec::new(),
        }
    }

    /// D.4b B30, B61, B70, B76: the new refusals and every note in words, and a short id.
    #[test]
    fn the_new_refusals_and_notes_in_words() {
        use crate::lock::{SlotNote as N, SlotRefusalView as V};
        use crate::secrets::LockSlotKind as K;
        let refused_as = |kind, refused| crate::lock::SlotView {
            id: None,
            kind,
            label: "x".into(),
            refused: Some(refused),
            notes: Vec::new(),
        };
        assert_eq!(
            slot_kinds(&[
                refused_as(K::OsPassword, V::GestureNotHere),
                refused_as(K::Unknown, V::NewerKind),
                refused_as(K::Password, V::SecondOfKind),
            ]),
            "account password (not on this computer), a slot this AppRafter does not know \
             (made by a newer AppRafter), password (a second one, never tried)"
        );
        assert_eq!(
            slot_count(&[slot(K::OsPassword, false), slot(K::RecoveryKey, false)]),
            "2 slots: account password, recovery key"
        );
        for note in [
            N::AccountPasswordOffline,
            N::AccountPasswordChanged,
            N::OsPasswordOtherComputer,
            N::ManagesStore,
            N::HelloPromptUnnamed,
            N::HelloOtherComputer,
            N::HelloKeyMissing,
            N::HelloSoftwareKey,
            N::NotCheckedHere,
            N::MachineIdMayChange,
        ] {
            assert!(!note_line(&note).is_empty(), "{note:?}");
            assert!(!plan_note_line(&note, K::OsPassword).is_empty(), "{note:?}");
        }
        assert_eq!(
            note_line(&N::ManagesStore),
            "whoever knows it can also change how the store opens"
        );
        assert!(plan_note_line(&N::ManagesStore, K::OsPassword)
            .starts_with("Anyone who can open this store with your account password"));
        assert!(plan_note_line(&N::ManagesStore, K::WindowsHello)
            .starts_with("One approval in a Windows Hello dialog"));
        assert!(plan_note_line(&N::AccountPasswordOffline, K::OsPassword)
            .contains("only the recovery key opens this store"));
        assert_eq!(short_id("0123456789abcdef0123456789abcdef"), "01234567");
        assert_eq!(short_id("abc"), "abc");
    }

    /// D.4a §3.13: the window and the slots as doctor's `secrets` row and `status`'s line say
    /// them.
    #[test]
    fn the_window_and_the_slots_in_words() {
        use crate::lock::UnlockWindow as W;
        use crate::secrets::LockSlotKind as K;
        let open = |left_secs| W::Open {
            left_secs,
            minutes: 5,
            holder_version: "0".into(),
            hardening: hardening(true, Some(true)),
        };
        for (window, words) in [
            (open(170), "unlocked in this terminal, 3 min left"),
            (open(42), "unlocked in this terminal, 42 s left"),
            (W::Closed, "no unlock window in this terminal"),
            (W::NoTerminal, "not run from a terminal"),
            (W::EveryCommand, "every command asks (the store's window is 0 min)"),
            (
                W::Unavailable { reason: crate::lock::HolderUnavailableReason::PathTooLong },
                "no unlock window possible here (`apprafter lock status` says why)",
            ),
            // A holder that does not answer still holds the key: never "no window" (rule 1).
            (
                W::Unavailable { reason: crate::lock::HolderUnavailableReason::NotResponding },
                "open in this terminal, but its holder does not answer: every command asks; `apprafter lock now` ends it",
            ),
            (W::App, "unlocked in this app"),
        ] {
            assert_eq!(window_words(&window), words, "{window:?}");
        }
        let both = [slot(K::Password, false), slot(K::RecoveryKey, false)];
        assert_eq!(slot_kinds(&both), "password, recovery key");
        assert_eq!(slot_count(&both), "2 slots: password, recovery key");
        assert_eq!(
            slot_count(&[slot(K::RecoveryKey, false)]),
            "1 slot: recovery key"
        );
        assert_eq!(
            slot_kinds(&[slot(K::Password, true), slot(K::RecoveryKey, false)]),
            "password (refused by this build), recovery key"
        );
    }
}
