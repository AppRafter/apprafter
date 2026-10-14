// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The lock's words for what it reports (ADR 0068 §7; D.4a §3.13, T72, T73): one sentence per
//! limit of this computer and one for a weakened holder, shared by `apprafter lock status`,
//! doctor's rows and `apprafter status`'s line, so that each limit reads the same wherever it is
//! shown. The desktop words its own Settings section; its doctor overlay shows doctor's rows.

use crate::lock::{HolderHardening, HolderUnavailableReason, MachineLimit, SlotView, UnlockWindow};
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
                LockSlotKind::OsPassword => "OS password",
                LockSlotKind::WindowsHello => "Windows Hello",
                LockSlotKind::TouchId => "Touch ID",
                LockSlotKind::Unknown => "a slot this AppRafter does not know",
            };
            match s.refused {
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
            kind,
            label: "x".into(),
            refused: refused.then_some(crate::lock::SlotRefusalView::KdfTooWeak),
        }
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
