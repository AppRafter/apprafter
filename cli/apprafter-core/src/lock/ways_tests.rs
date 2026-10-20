// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock on`'s ways (d4b-c3) on scratch stores in files, under the fast KDF policy of
//! `on_tests`' fixtures: the plan's way from the client's facts, in B88's order (B55, B74,
//! B75, B88). The client's facts are this file's stand-in (B15): nothing reaches PAM or
//! Windows Hello, a machine id is a fixed byte string, and the OS the stand-in plays is its
//! own, so every OS's way runs on every CI leg.

use std::sync::Mutex;

use cli_core::{AccountPasswordRefusal, CliError};

use super::on_tests::{assert_locked_whole, lock, PASSWORD};
use super::on_tests::{files_store, new_input};
use super::*;
use crate::vault::MachineId;
use crate::vault::{SlotId, SlotKind, VaultFile, VaultHeader, VAULT_FILE};
use crate::Context;
use crate::SecretString;
use crate::{CancellationToken, CoreError, CoreResult, NullReporter, Outcome, Plan};

/// This computer's facts as a test sets them (B15): each call recorded by name, in order, so
/// a test sees what the plan asked — and what it did not.
#[derive(Debug)]
struct Stand {
    os: HostOs,
    fenced: bool,
    backend: CheckBackend,
    machine: MachineFact,
    may_change: Option<bool>,
    backoff: BackoffFact,
    status: PasswordFact,
    asked: Mutex<Vec<&'static str>>,
}

impl Stand {
    /// Every fact clear on `os`: its everyday way is usable here.
    fn ready(os: HostOs) -> Stand {
        Stand {
            os,
            fenced: false,
            backend: CheckBackend::Ready,
            machine: MachineFact::Id(here()),
            may_change: Some(false),
            backoff: BackoffFact::Clear,
            status: PasswordFact::Usable,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn ask(&self, what: &'static str) {
        self.asked.lock().unwrap().push(what);
    }

    fn asked(&self) -> Vec<&'static str> {
        self.asked.lock().unwrap().clone()
    }
}

impl SlotFacts for Stand {
    fn os(&self) -> HostOs {
        self.ask("os");
        self.os
    }

    fn machine_id(&self) -> MachineFact {
        self.ask("machine_id");
        self.machine.clone()
    }

    fn machine_id_may_change(&self) -> Option<bool> {
        self.ask("machine_id_may_change");
        self.may_change
    }
}

impl WayFacts for Stand {
    fn fenced(&self) -> bool {
        self.ask("fenced");
        self.fenced
    }

    fn account_check(&self) -> CheckBackend {
        self.ask("account_check");
        self.backend
    }

    fn backoff(&self) -> BackoffFact {
        self.ask("backoff");
        self.backoff
    }

    fn password_status(&self) -> PasswordFact {
        self.ask("password_status");
        self.status
    }
}

/// Every ask of a plan whose facts were all clear, in B88's order: a plan that stops at a
/// reason asks a prefix of it.
const EVERY_ASK: [&str; 7] = [
    "fenced",
    "os",
    "account_check",
    "machine_id",
    "backoff",
    "password_status",
    "machine_id_may_change",
];

/// This computer's machine id in these tests: a fixed byte string, never the host's.
fn here() -> MachineId {
    MachineId::new(&[0x0a; 16]).unwrap()
}

/// The new vault's line of a `lock on` plan: what opens it.
fn vault_line(plan: &Plan<LockOnPayload>) -> String {
    plan.changes
        .iter()
        .find(|change| change.kind == "Vault")
        .and_then(|change| change.detail.clone())
        .unwrap()
}

/// A run's outcome, completed.
fn done(outcome: CoreResult<Outcome<LockedOn>>) -> LockedOn {
    match outcome.unwrap() {
        Outcome::Completed { result } => result,
        other => panic!("{other:?}"),
    }
}

/// B88 (acceptance 1): the plan names the first reason that holds, in B88's order — two
/// neighbours set together name the earlier one — and asks nothing after it: under the fence
/// not even which OS the backend plays (B82).
#[test]
fn each_fallback_reason_is_named_in_b88s_order_and_nothing_after_it_is_asked() {
    use HostOs::{Linux, Macos, Windows};
    let cases = [
        (
            "the fence, before the build",
            Stand {
                fenced: true,
                ..Stand::ready(Windows)
            },
            WayFallback::Sandbox,
            1,
        ),
        (
            "the build, before the system's check",
            Stand {
                backend: CheckBackend::NoLibpam,
                ..Stand::ready(Windows)
            },
            WayFallback::HelloNotBuilt,
            2,
        ),
        (
            "libpam, before the machine id",
            Stand {
                backend: CheckBackend::NoLibpam,
                machine: MachineFact::NoId,
                ..Stand::ready(Linux)
            },
            WayFallback::NoLibpam,
            3,
        ),
        (
            "no service",
            Stand {
                backend: CheckBackend::NoService,
                ..Stand::ready(Linux)
            },
            WayFallback::NoService,
            3,
        ),
        (
            "no check route",
            Stand {
                backend: CheckBackend::NoCheckRoute,
                ..Stand::ready(Macos)
            },
            WayFallback::NoCheckRoute,
            3,
        ),
        (
            "no account",
            Stand {
                backend: CheckBackend::NoAccount,
                ..Stand::ready(Linux)
            },
            WayFallback::NoAccount,
            3,
        ),
        (
            "the machine id, before the back-off's record",
            Stand {
                machine: MachineFact::NoId,
                backoff: BackoffFact::NoRecord,
                ..Stand::ready(Linux)
            },
            WayFallback::NoMachineId,
            4,
        ),
        (
            "a machine id nobody read is none",
            Stand {
                machine: MachineFact::Unknown,
                ..Stand::ready(Macos)
            },
            WayFallback::NoMachineId,
            4,
        ),
        (
            "the back-off's record, before the password",
            Stand {
                backoff: BackoffFact::NoRecord,
                status: PasswordFact::NoPassword,
                ..Stand::ready(Linux)
            },
            WayFallback::NoBackoffRecord,
            5,
        ),
        (
            "no password, before the back-off's refusal",
            Stand {
                status: PasswordFact::NoPassword,
                backoff: BackoffFact::Wait {
                    wait_secs: Some(540),
                },
                ..Stand::ready(Linux)
            },
            WayFallback::NoPassword,
            6,
        ),
    ];
    for (case, stand, reason, asks) in cases {
        let f = files_store();
        let plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
        assert_eq!(plan.payload.way(), Some(LockOnWay::LockPassword), "{case}");
        assert_eq!(plan.payload.fallback(), Some(reason), "{case}");
        assert_eq!(stand.asked(), EVERY_ASK[..asks], "{case}");
        assert!(plan.payload.notes().is_empty(), "{case}");
        assert!(!plan.payload.empty_entry_falls_back(), "{case}");
        assert_eq!(
            vault_line(&plan),
            "opened by a lock password or a recovery key",
            "{case}"
        );
    }
}

/// B55 with each stand-in (B15; acceptance 1): the account password is this computer's
/// everyday way on Linux and macOS, bound to its machine id, with what it costs said before
/// anything is asked (B61, B70) — and, in a container or WSL, that its machine id may change
/// (B76); on Windows this build has no Windows Hello yet. An empty entry means "no password"
/// only where the password status could not be read (B75 (2)).
#[test]
fn the_default_is_this_computers_everyday_way() {
    for os in [HostOs::Linux, HostOs::Macos] {
        let f = files_store();
        let stand = Stand::ready(os);
        let plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
        assert_eq!(
            plan.payload.way(),
            Some(LockOnWay::AccountPassword),
            "{os:?}"
        );
        assert_eq!(plan.payload.fallback(), None, "{os:?}");
        assert_eq!(
            plan.payload.notes(),
            [SlotNote::AccountPasswordOffline, SlotNote::ManagesStore],
            "{os:?}"
        );
        assert!(
            !plan.payload.empty_entry_falls_back(),
            "{os:?}: a usable password is a fact"
        );
        assert_eq!(
            vault_line(&plan),
            "opened by your account password on this computer or a recovery key"
        );
        assert_eq!(stand.asked(), EVERY_ASK, "{os:?}");
    }
    let f = files_store();
    let container = Stand {
        may_change: Some(true),
        status: PasswordFact::Unknown,
        ..Stand::ready(HostOs::Linux)
    };
    let plan = plan_on(&f.ctx, None, WayAsked::Everyday(&container)).unwrap();
    assert_eq!(
        plan.payload.notes().last(),
        Some(&SlotNote::MachineIdMayChange)
    );
    assert!(
        plan.payload.empty_entry_falls_back(),
        "a status nobody could read: Enter means no password"
    );
    let windows = Stand::ready(HostOs::Windows);
    let plan = plan_on(&f.ctx, None, WayAsked::Everyday(&windows)).unwrap();
    assert_eq!(plan.payload.fallback(), Some(WayFallback::HelloNotBuilt));
}

/// B74 (acceptance 2): a back-off in force refuses the run when it is planned, naming the
/// wait — or another AppRafter process checking now — before anything is asked; it never
/// falls back, and the plan writes nothing (a plan never writes: the snapshot guards a later
/// change that would).
#[test]
fn a_back_off_in_force_refuses_the_plan_and_never_falls_back() {
    for wait_secs in [Some(540), None] {
        let f = files_store();
        let before = crate::target::testkit::snapshot(f.ctx.config_root());
        let stand = Stand {
            backoff: BackoffFact::Wait { wait_secs },
            ..Stand::ready(HostOs::Linux)
        };
        match plan_on(&f.ctx, None, WayAsked::Everyday(&stand)) {
            Err(CoreError::Cli(CliError::AccountPasswordRefused {
                reason: AccountPasswordRefusal::Backoff { wait_secs: said },
                messages,
            })) => assert_eq!((said, messages.len()), (wait_secs, 0)),
            other => panic!("{wait_secs:?}: {:?}", other.map(|plan| plan.payload.way())),
        }
        assert_eq!(
            crate::target::testkit::snapshot(f.ctx.config_root()),
            before
        );
    }
}

/// B55 (acceptance 3): `--lock-password` takes a separate lock password and asks this
/// computer nothing; a store locked already, or a run to finish, asks nothing either —
/// their way is chosen.
#[test]
fn the_lock_password_and_a_chosen_way_ask_this_computer_nothing() {
    let f = files_store();
    let plan = plan_on(&f.ctx, None, WayAsked::LockPassword).unwrap();
    assert_eq!(
        (plan.payload.way(), plan.payload.fallback()),
        (Some(LockOnWay::LockPassword), None)
    );
    let (input, _) = new_input();
    done(execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    ));
    let stand = Stand::ready(HostOs::Linux);
    let plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
    assert_eq!(plan.payload.start(), &LockOnStart::AlreadyLocked);
    assert_eq!(plan.payload.way(), None);
    let g = files_store();
    let plan = plan_on(&g.ctx, None, WayAsked::LockPassword).unwrap();
    let (input, _) = new_input();
    execute_on(
        &g.ctx.clone().with_fault(FaultPoint::AfterVaultWritten),
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    let plan = plan_on(&g.ctx, None, WayAsked::Everyday(&stand)).unwrap();
    assert!(
        matches!(plan.payload.start(), LockOnStart::Resume { .. }),
        "{:?}",
        plan.payload.start()
    );
    assert!(stand.asked().is_empty(), "{:?}", stand.asked());
}

/// B56: a plan for the account password writes no vault a lock password opens — given one,
/// the run writes nothing.
#[test]
fn a_plan_for_another_way_is_never_written_with_a_lock_password() {
    let f = files_store();
    let stand = Stand::ready(HostOs::Linux);
    let plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
    let before = crate::target::testkit::snapshot(f.ctx.config_root());
    let (input, _) = new_input();
    let run = execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    );
    assert!(
        matches!(run, Err(CoreError::Cli(CliError::Other(_)))),
        "{run:?}"
    );
    assert_eq!(
        crate::target::testkit::snapshot(f.ctx.config_root()),
        before
    );
}

/// The header of `ctx`'s vault, read without a secret.
fn header_of(ctx: &Context) -> VaultHeader {
    let bytes = crate::vault::file::read(&ctx.config_root().join(VAULT_FILE)).unwrap();
    VaultFile::parse(&bytes).unwrap().header().clone()
}

/// B57, B80 (acceptance 4): a new run's plan mints the vault's id and its slots' — a plan of
/// its own, ids of its own — and the run writes exactly them, the recovery key's slot second,
/// whatever the way: asked for, or fallen back to. A store locked already mints none.
#[test]
fn every_new_run_writes_the_ids_its_plan_minted() {
    let fenced = Stand {
        fenced: true,
        ..Stand::ready(HostOs::Linux)
    };
    for asked in [WayAsked::LockPassword, WayAsked::Everyday(&fenced)] {
        let f = files_store();
        let plan = plan_on(&f.ctx, None, asked).unwrap();
        let other = plan_on(&f.ctx, None, asked).unwrap();
        let vault_id = plan.payload.vault_id().unwrap();
        let recovery_id = plan.payload.recovery_slot_id().unwrap();
        assert_ne!(other.payload.vault_id(), Some(vault_id));
        assert_ne!(other.payload.recovery_slot_id(), Some(recovery_id));
        let (input, _) = new_input();
        done(execute_on(
            &f.ctx,
            plan,
            input,
            &NullReporter,
            &CancellationToken::new(),
        ));
        let header = header_of(&f.ctx);
        assert_eq!(header.vault_id, vault_id);
        let slots: Vec<(SlotKind, SlotId)> = header
            .slots
            .iter()
            .map(|slot| (slot.kind, slot.id))
            .collect();
        assert_eq!(slots.len(), 2, "{slots:?}");
        assert_eq!(slots[0].0, SlotKind::Password);
        assert_eq!(slots[1], (SlotKind::RecoveryKey, recovery_id));
        let locked = plan_on(&f.ctx, None, asked).unwrap();
        assert_eq!(
            (locked.payload.vault_id(), locked.payload.recovery_slot_id()),
            (None, None)
        );
    }
}

/// A run's input through the first slot's step, as a client makes it (B56): `secret` for the
/// plan's way, then the recovery key that step drew, typed back; and the key's text.
fn input_for(plan: &mut Plan<LockOnPayload>, secret: FirstSecret) -> (LockOnInput, String) {
    match first_slot(plan, secret).unwrap() {
        FirstStep::Ready { first, recovery } => {
            let shown = recovery.to_display().expose().to_string();
            let recovery = recovery.confirm(&shown).unwrap();
            (LockOnInput::New { first, recovery }, shown)
        }
        other => panic!("{other:?}"),
    }
}

/// B56, O2 (acceptance 2, 3): a lock password the vault would refuse is a mistake — said and
/// asked again, no recovery key drawn, nothing written; one it takes is the first slot, and
/// only then is the recovery key drawn. The run is D.4a's: the store opens with both.
#[test]
fn a_lock_password_is_checked_before_its_recovery_key_exists() {
    let f = files_store();
    let mut plan = plan_on(&f.ctx, None, WayAsked::LockPassword).unwrap();
    let before = crate::target::testkit::snapshot(f.ctx.config_root());
    let long = "a".repeat(1025);
    for (typed, reason) in [
        ("", PasswordProblem::Empty),
        (long.as_str(), PasswordProblem::TooLong { max: 1024 }),
    ] {
        let secret = FirstSecret::LockPassword(SecretString::new(typed));
        match first_slot(&mut plan, secret).unwrap() {
            FirstStep::Mistake(CoreError::LockPasswordRefused { reason: said }) => {
                assert_eq!(said, reason)
            }
            other => panic!("{reason:?}: {other:?}"),
        }
    }
    assert_eq!(
        crate::target::testkit::snapshot(f.ctx.config_root()),
        before
    );
    let secret = FirstSecret::LockPassword(SecretString::new(PASSWORD));
    let (input, shown) = input_for(&mut plan, secret);
    done(execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    ));
    let kinds: Vec<SlotKind> = header_of(&f.ctx).slots.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, [SlotKind::Password, SlotKind::RecoveryKey]);
    assert_locked_whole(&f.ctx, PASSWORD);
    assert_locked_whole(&f.ctx, &shown);
}

/// B55, B56: a secret the plan does not take is refused before anything is asked, drawn or
/// written — a lock password for a plan of this computer's account password, any secret for
/// a plan that is no new run's (a store locked already) — and the plan stays as it was.
#[test]
fn a_secret_the_plan_does_not_take_is_refused_before_anything_is_drawn() {
    let f = files_store();
    let stand = Stand::ready(HostOs::Linux);
    let mut plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
    let before = crate::target::testkit::snapshot(f.ctx.config_root());
    let lock_password = || FirstSecret::LockPassword(SecretString::new(PASSWORD));
    let step = first_slot(&mut plan, lock_password());
    assert!(
        matches!(step, Err(CoreError::Cli(CliError::Other(_)))),
        "{step:?}"
    );
    assert_eq!(plan.payload.way(), Some(LockOnWay::AccountPassword));
    assert_eq!(
        crate::target::testkit::snapshot(f.ctx.config_root()),
        before
    );
    lock(&f.ctx);
    let mut locked = plan_on(&f.ctx, None, WayAsked::LockPassword).unwrap();
    assert_eq!(locked.payload.start(), &LockOnStart::AlreadyLocked);
    let step = first_slot(&mut locked, lock_password());
    assert!(
        matches!(step, Err(CoreError::Cli(CliError::Other(_)))),
        "{step:?}"
    );
}

/// B80, F13 (critic round, finding 2): a first slot is written only under the plan whose step
/// made it — the recovery key's slot id the client showed is that plan's — so the first slot
/// of one plan given to another writes nothing.
#[test]
fn a_first_slot_is_written_only_under_its_own_plan() {
    let f = files_store();
    let mut made = plan_on(&f.ctx, None, WayAsked::LockPassword).unwrap();
    let other = plan_on(&f.ctx, None, WayAsked::LockPassword).unwrap();
    let secret = FirstSecret::LockPassword(SecretString::new(PASSWORD));
    let (input, _) = input_for(&mut made, secret);
    let before = crate::target::testkit::snapshot(f.ctx.config_root());
    let run = execute_on(
        &f.ctx,
        other,
        input,
        &NullReporter,
        &CancellationToken::new(),
    );
    assert!(
        matches!(run, Err(CoreError::Cli(CliError::Other(_)))),
        "{run:?}"
    );
    assert_eq!(
        crate::target::testkit::snapshot(f.ctx.config_root()),
        before
    );
}
