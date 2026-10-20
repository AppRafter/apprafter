// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The target-store lock (ADR 0068; D.4a §3.9, §3.13): turning it on and off, opening a locked
//! store with one prompt's input, and the one report every surface shows — `lock status`,
//! doctor's lock rows, `status`'s line and the desktop's Security page — from one function
//! (`status`, d4a-e), so they cannot disagree (T73). The flows arrive with d4a-d and d4a-e;
//! D.4a c2 freezes the types first, and the desktop's Security page is built against them.
//!
//! Every type here is `Serialize` and exported to TypeScript, never `Deserialize` (D.3 §3.10).

use serde::Serialize;

use crate::secrets::{LockSlotKind, SecretsMode};

pub mod facts;
pub mod faults;
pub mod limits;
pub mod off;
#[cfg(test)]
mod off_tests;
pub mod on;
#[cfg(test)]
mod on_tests;
pub mod open;
mod reseal;
#[cfg(test)]
mod reseal_tests;
pub mod rotate;
#[cfg(test)]
mod rotate_tests;
pub mod slots;
#[cfg(test)]
mod slots_tests;
mod status;
pub mod text;
pub mod ways;
#[cfg(test)]
mod ways_tests;
pub use status::status;

pub use facts::{BackoffFact, CheckBackend, PasswordFact, WayFacts};
pub use facts::{HelloKeyDeleted, HelloKeys, NoHelloKeys};
pub use facts::{HostOs, MachineFact, SlotFacts, UnknownFacts};
pub use faults::FaultPoint;
pub use off::HelloKeyGone;
pub use off::{execute_off, plan_off, LockOffPayload, LockOffStart, LockedOff};
pub use on::{
    execute_on, plan_on, CacheDrop, DroppedCache, LockOnInput, LockOnPayload, LockOnStart,
    LockedOn, PASSWORD_LABEL, RECOVERY_LABEL,
};
pub use open::{
    check_password, open_at, open_slot_at, set_unlock_timeout, OpenedVault, SlotOpened,
};
pub use rotate::{
    plan_rotate, DropWhy, RotateKind, RotatePayload, RotateRow, SlotFate, StoreKeyLine,
};
pub use slots::{
    account_password_label, check_account_password, execute_slot_add, execute_slot_remove,
    plan_slot_add, plan_slot_remove, SlotAddKind, SlotAddPayload, SlotAdded, SlotRemovePayload,
    SlotRemoved,
};
pub use ways::{first_slot, FirstSecret, FirstSlot, FirstStep};
pub use ways::{AccountChecked, AccountPasswordCheck};
pub use ways::{LockOnWay, WayAsked, WayFallback};

use std::path::{Path, PathBuf};

use crate::context::Context;
use crate::error::{CoreError, CoreResult};
use crate::vault::{KdfPolicy, MAX_UNLOCK_TIMEOUT_MIN};

/// The KDF policy the lock flows run under: an in-process test's (`Context::with_kdf_policy`),
/// else this build's.
pub(crate) fn kdf_policy(ctx: &Context) -> &'static KdfPolicy {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(policy) = ctx.kdf_policy_override() {
        return policy;
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = ctx;
    KdfPolicy::active()
}

/// Whether a password slot may be created for `ctx`'s store (D.4a T23, critic S2): always under
/// release parameters; under weak ones only when the store's root lies under one of
/// `Context::insecure_kdf_roots` — a test build never writes a slot guessable at microseconds
/// per try into a real store.
pub fn insecure_kdf_permitted(ctx: &Context) -> bool {
    if kdf_policy(ctx).is_release() {
        return true;
    }
    let Some(root) = canonical(ctx.config_root()) else {
        return false;
    };
    ctx.insecure_kdf_roots()
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .any(|r| root.starts_with(&r))
}

/// [`insecure_kdf_permitted`] as the refusal the flows raise, before anything is asked.
pub(crate) fn permit(ctx: &Context) -> CoreResult<()> {
    if insecure_kdf_permitted(ctx) {
        Ok(())
    } else {
        Err(CoreError::InsecureKdfRefused {
            root: ctx.config_root().to_path_buf(),
        })
    }
}

/// A window of more than a day is refused (O3), before anything is asked.
pub(crate) fn check_timeout(minutes: u32) -> CoreResult<()> {
    if minutes <= MAX_UNLOCK_TIMEOUT_MIN {
        return Ok(());
    }
    Err(CoreError::Cli(cli_core::CliError::UsageRefused {
        message: format!(
            "an unlock window lasts at most {MAX_UNLOCK_TIMEOUT_MIN} minutes, and {minutes} \
             were asked for"
        ),
        help: "Pass `--timeout` with a number of minutes from 0 to 1440.".to_string(),
    }))
}

/// `path` with its deepest existing ancestor canonical and the rest appended: the root of a
/// store that `lock on` creates may not exist yet.
fn canonical(path: &Path) -> Option<PathBuf> {
    let mut tail = Vec::new();
    let mut at = path;
    loop {
        if let Ok(found) = at.canonicalize() {
            let mut out = found;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
        tail.push(at.file_name()?.to_os_string());
        at = at.parent()?;
    }
}

pub use open::{
    PasswordAdvice, PasswordProblem, RecoveryKeyProblem, MAX_PASSWORD_BYTES,
    RECOMMENDED_PASSWORD_CHARS,
};

/// What `lock status`, doctor's lock rows, `status`'s line and the desktop's Security page
/// show (T73).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LockReport {
    /// The store root, home-abbreviated as the client shows paths.
    pub store: String,
    /// `store.yaml`'s id; `None` in files.
    pub store_id: Option<String>,
    pub targets: Vec<String>,
    pub mode: SecretsMode,
    pub slots: Vec<SlotView>,
    /// The store's unlock window, in minutes (`0`: every command asks).
    pub window_minutes: u32,
    pub window: UnlockWindow,
    pub limits: Vec<MachineLimit>,
}

/// One slot, as a surface lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SlotView {
    /// The slot's id, 32 hex characters (a surface shows its first 8, `text::short_id`);
    /// `None` on `store.yaml`'s mirror, whose rows offer no action (D.4b B36).
    pub id: Option<String>,
    pub kind: LockSlotKind,
    pub label: String,
    /// Why this build will not open it, when it will not (told without deriving).
    pub refused: Option<SlotRefusalView>,
    /// What this slot costs and adds, said wherever it is listed (D.4b B61, B70, B76; rule
    /// 1).
    pub notes: Vec<SlotNote>,
}

/// Why this build will not open a slot here, told before deriving anything (T22; D.4b B18,
/// B36).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SlotRefusalView {
    KdfTooWeak,
    KdfOutOfRange,
    /// It opens elsewhere only: an account-password slot made on another computer (or
    /// before this computer's id changed), or on Windows; a Windows Hello slot of another
    /// computer (d4b-w1).
    GestureNotHere,
    /// A kind, or a layout of a known kind, that a newer AppRafter made (B1): kept, never
    /// opened.
    NewerKind,
    /// A second slot of a typed kind — a lock password, or this computer's account password
    /// — which the prompt never tries: a forged or damaged header (B18, B77).
    SecondOfKind,
}

impl SlotRefusalView {
    /// Its name on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            SlotRefusalView::KdfTooWeak => "kdf_too_weak",
            SlotRefusalView::KdfOutOfRange => "kdf_out_of_range",
            SlotRefusalView::GestureNotHere => "gesture_not_here",
            SlotRefusalView::NewerKind => "newer_kind",
            SlotRefusalView::SecondOfKind => "second_of_kind",
        }
    }
}

/// A fact about one slot that every surface listing it says (D.4b B41, B61, B70, B76; §7.4).
/// The CLI's words are `text::note_line`, a plan's `text::plan_note_line`; the desktop words
/// its own. The Windows Hello notes and `AccountPasswordChanged` are produced from d4b-w1
/// and d4b-f.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlotNote {
    /// An account-password slot (Q1, B54): it opens only on the computer that made it, and
    /// anyone with a copy of the vault and that computer's id can guess the password offline.
    AccountPasswordOffline,
    /// The account password changed after this slot was added (B41; d4b-f, where the system
    /// tells the change time).
    AccountPasswordChanged,
    /// An account-password slot made on another computer, or before this computer's id
    /// changed: not tried here (B70, B76).
    OsPasswordOtherComputer,
    /// Whoever opens the store with it can also change how it opens (O-b8, B61).
    ManagesStore,
    /// The Windows Hello dialog does not name the program that asks (O-b5; d4b-w1).
    HelloPromptUnnamed,
    /// A Windows Hello slot made on another computer (d4b-w1).
    HelloOtherComputer,
    /// A Windows Hello slot made here whose key is gone (d4b-w1).
    HelloKeyMissing,
    /// A Windows Hello key kept in software, not in a TPM (B52; d4b-w1).
    HelloSoftwareKey,
    /// A fact about this slot this AppRafter could not check here (B30): under the fence,
    /// without a backend.
    NotCheckedHere,
    /// This computer looks like a container or WSL: its machine id may change when it is
    /// re-created (B76).
    MachineIdMayChange,
}

/// One slot by its id, kind and label: what an outcome names — the slot a fresh secret opened
/// the vault with (D.4b B60), the one a `lock slot add` made or replaced, the one a removal
/// took out. `Serialize` for the desktop's flows (their TypeScript export is d4b-c4's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotRef {
    /// 32 hex characters (surfaces show the first 8).
    pub id: String,
    pub kind: LockSlotKind,
    pub label: String,
}

impl From<&crate::vault::SlotRecord> for SlotRef {
    fn from(slot: &crate::vault::SlotRecord) -> Self {
        SlotRef {
            id: slot.id.to_string(),
            kind: LockSlotKind::from(slot.kind),
            label: slot.label.clone(),
        }
    }
}

/// This client's unlock window on the store (T56): the CLI's from its terminal's holder, the
/// desktop's while it holds the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum UnlockWindow {
    /// Not run from a terminal: no window can exist here.
    NoTerminal,
    /// A terminal with no window open.
    Closed,
    /// A window is open: `left_secs` of its `minutes`, kept by a holder of `holder_version`,
    /// protected as `hardening` reports.
    Open {
        left_secs: u64,
        minutes: u32,
        holder_version: String,
        hardening: HolderHardening,
    },
    /// The store's window is 0 minutes: every command asks.
    EveryCommand,
    /// No window is possible here, for `reason`.
    Unavailable { reason: HolderUnavailableReason },
    /// The app holds the key while it is unlocked.
    App,
}

/// What a holder reports of its own protections (T54): `None` where the measure does not exist
/// on this OS; a failed one is reported, never hidden (rule 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HolderHardening {
    pub non_dumpable: Option<bool>,
    pub deny_attach: Option<bool>,
    pub core_off: bool,
    pub memory_locked: bool,
    pub process_dacl: Option<bool>,
}

/// Why no unlock window is possible here (T49–T52, T59).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HolderUnavailableReason {
    NoRuntimeDir,
    UnsafeRuntimeDir,
    PathTooLong,
    NoSessionLeader,
    NoConsole,
    NotResponding,
    Timeout,
}

/// What this computer lets other programs running as the person do (T72, O1), each shown in the
/// CLI and the desktop before the guide states it (rule 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MachineLimit {
    X11Session,
    Xwayland {
        terminal_is_x_client: Option<bool>,
    },
    /// The desktop app is an X11 program inside a Wayland session (GDK opened Xwayland, as
    /// `GDK_BACKEND=x11` makes it): other X programs see what is typed into it (F21). The CLI's
    /// `Xwayland` is about a terminal; this one is about the app.
    AppOnXwayland,
    PtraceScope {
        value: Option<u8>,
    },
    WindowsSameUser,
    MacosTaskPort,
    Multiplexer {
        name: String,
    },
    InheritedWindow,
    SleepUnreported,
    Hibernation,
    SshKeyUsable {
        path: String,
        unencrypted: bool,
        in_agent: Option<bool>,
    },
    RuntimeDirFallback {
        path: String,
    },
    SecretsInUse,
    /// A program running as the person that sets out to get past the lock — an AI agent
    /// turned by a prompt injection, say — can read what they type, change what they run or
    /// use an unlocked window: the lock slows it and an OS boundary stops it (ADR 0068 §1;
    /// O1). Static: every computer.
    SameUserPrograms,
}

/// Where a terminal's holder keeps its endpoint (T49), shown on `lock status`'s "This
/// terminal" line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HolderBase {
    pub dir: String,
    pub source: HolderBaseSource,
}

/// Which rule chose the holder's base (T49).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HolderBaseSource {
    XdgRuntimeDir,
    Tmpdir,
    DarwinUserTemp,
    StoreRuntimeDir,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The freeze (D.4a §5 c2): the wire shape the desktop's Security page is built against.
    #[test]
    fn a_lock_report_has_the_frozen_wire_shape() {
        let report = LockReport {
            store: "~/.config/apprafter".into(),
            store_id: Some("3f9c0a1b7d2e4c56".into()),
            targets: vec!["prod".into()],
            mode: SecretsMode::Locked,
            slots: vec![
                SlotView {
                    id: Some("0123456789abcdef0123456789abcdef".into()),
                    kind: LockSlotKind::Password,
                    label: "lock password".into(),
                    refused: Some(SlotRefusalView::KdfTooWeak),
                    notes: Vec::new(),
                },
                SlotView {
                    id: None,
                    kind: LockSlotKind::OsPassword,
                    label: "account password of rem on laptop".into(),
                    refused: Some(SlotRefusalView::GestureNotHere),
                    notes: vec![
                        SlotNote::AccountPasswordOffline,
                        SlotNote::OsPasswordOtherComputer,
                    ],
                },
            ],
            window_minutes: 5,
            window: UnlockWindow::Open {
                left_secs: 180,
                minutes: 5,
                holder_version: "0.2.80".into(),
                hardening: HolderHardening {
                    non_dumpable: Some(true),
                    deny_attach: None,
                    core_off: true,
                    memory_locked: false,
                    process_dacl: None,
                },
            },
            limits: vec![
                MachineLimit::PtraceScope { value: Some(0) },
                MachineLimit::SshKeyUsable {
                    path: "~/.ssh/id_ed25519".into(),
                    unencrypted: true,
                    in_agent: None,
                },
                MachineLimit::SecretsInUse,
            ],
        };
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            json!({
                "store": "~/.config/apprafter",
                "storeId": "3f9c0a1b7d2e4c56",
                "targets": ["prod"],
                "mode": "locked",
                "slots": [
                    {"id": "0123456789abcdef0123456789abcdef", "kind": "password",
                     "label": "lock password", "refused": "kdf_too_weak", "notes": []},
                    {"id": null, "kind": "os_password", "label": "account password of rem on laptop",
                     "refused": "gesture_not_here",
                     "notes": [{"kind": "account_password_offline"},
                               {"kind": "os_password_other_computer"}]}
                ],
                "windowMinutes": 5,
                "window": {
                    "status": "open", "leftSecs": 180, "minutes": 5, "holderVersion": "0.2.80",
                    "hardening": {"nonDumpable": true, "denyAttach": null, "coreOff": true,
                                  "memoryLocked": false, "processDacl": null}
                },
                "limits": [
                    {"kind": "ptrace_scope", "value": 0},
                    {"kind": "ssh_key_usable", "path": "~/.ssh/id_ed25519", "unencrypted": true,
                     "inAgent": null},
                    {"kind": "secrets_in_use"}
                ]
            })
        );
    }

    #[test]
    fn every_window_base_and_mode_has_its_wire_name() {
        for (window, wire) in [
            (UnlockWindow::NoTerminal, json!({"status": "no_terminal"})),
            (UnlockWindow::Closed, json!({"status": "closed"})),
            (
                UnlockWindow::EveryCommand,
                json!({"status": "every_command"}),
            ),
            (
                UnlockWindow::Unavailable {
                    reason: HolderUnavailableReason::PathTooLong,
                },
                json!({"status": "unavailable", "reason": "path_too_long"}),
            ),
            (UnlockWindow::App, json!({"status": "app"})),
        ] {
            assert_eq!(serde_json::to_value(&window).unwrap(), wire);
        }
        assert_eq!(
            serde_json::to_value(HolderBase {
                dir: "/tmp/x/apprafter".into(),
                source: HolderBaseSource::XdgRuntimeDir,
            })
            .unwrap(),
            json!({"dir": "/tmp/x/apprafter", "source": "xdg_runtime_dir"})
        );
        assert_eq!(
            serde_json::to_value(MachineLimit::Xwayland {
                terminal_is_x_client: Some(true)
            })
            .unwrap(),
            json!({"kind": "xwayland", "terminalIsXClient": true})
        );
        assert_eq!(
            serde_json::to_value(MachineLimit::AppOnXwayland).unwrap(),
            json!({"kind": "app_on_xwayland"})
        );
        assert_eq!(
            serde_json::to_value(SecretsMode::Unlocking).unwrap(),
            json!("unlocking")
        );
        assert_eq!(
            serde_json::to_value(LockSlotKind::WindowsHello).unwrap(),
            json!("windows_hello")
        );
        assert_eq!(
            serde_json::to_value(crate::secrets::CacheKeyView::StoreVault).unwrap(),
            json!("store_vault")
        );
        assert_eq!(
            serde_json::to_value(PasswordAdvice::Short {
                chars: 14,
                recommended: 15
            })
            .unwrap(),
            json!({"kind": "short", "chars": 14, "recommended": 15})
        );
        assert_eq!(RECOMMENDED_PASSWORD_CHARS, 15, "O2, decided 2026-10-13");
    }

    /// D.4b §7.4 (frozen with the desktop's own types in d4b-c4): every refusal's and every
    /// note's wire name.
    #[test]
    fn every_slot_refusal_and_note_has_its_wire_name() {
        for (refusal, wire) in [
            (SlotRefusalView::KdfTooWeak, "kdf_too_weak"),
            (SlotRefusalView::KdfOutOfRange, "kdf_out_of_range"),
            (SlotRefusalView::GestureNotHere, "gesture_not_here"),
            (SlotRefusalView::NewerKind, "newer_kind"),
            (SlotRefusalView::SecondOfKind, "second_of_kind"),
        ] {
            assert_eq!(serde_json::to_value(refusal).unwrap(), json!(wire));
            assert_eq!(refusal.as_str(), wire);
        }
        for (note, wire) in [
            (SlotNote::AccountPasswordOffline, "account_password_offline"),
            (SlotNote::AccountPasswordChanged, "account_password_changed"),
            (
                SlotNote::OsPasswordOtherComputer,
                "os_password_other_computer",
            ),
            (SlotNote::ManagesStore, "manages_store"),
            (SlotNote::HelloPromptUnnamed, "hello_prompt_unnamed"),
            (SlotNote::HelloOtherComputer, "hello_other_computer"),
            (SlotNote::HelloKeyMissing, "hello_key_missing"),
            (SlotNote::HelloSoftwareKey, "hello_software_key"),
            (SlotNote::NotCheckedHere, "not_checked_here"),
            (SlotNote::MachineIdMayChange, "machine_id_may_change"),
        ] {
            assert_eq!(serde_json::to_value(note).unwrap(), json!({"kind": wire}));
        }
    }

    #[cfg(feature = "ts")]
    #[test]
    fn the_lock_types_have_typescript_declarations() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::new().with_large_int("number");
        let window = UnlockWindow::decl(&cfg);
        assert!(
            window.contains("leftSecs: number") && window.contains("\"open\""),
            "{window}"
        );
        let limit = MachineLimit::decl(&cfg);
        assert!(limit.contains("inAgent: boolean | null"), "{limit}");
        let report = LockReport::decl(&cfg);
        assert!(
            report.contains("windowMinutes: number") && report.contains("mode: SecretsMode"),
            "{report}"
        );
        let advice = PasswordAdvice::decl(&cfg);
        assert!(advice.contains("recommended: number"), "{advice}");
    }

    /// O1 (d4a-f): the wire name of the limit for programs that set out to get past the lock.
    #[test]
    fn the_same_user_programs_limit_has_its_wire_name() {
        assert_eq!(
            serde_json::to_value(MachineLimit::SameUserPrograms).unwrap(),
            json!({"kind": "same_user_programs"})
        );
    }
}
