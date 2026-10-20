// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock on`'s ways (d4b-c3) on scratch stores in files, under the fast KDF policy of
//! `on_tests`' fixtures: the plan's way from the client's facts, in B88's order (B55, B74,
//! B75, B88). The client's facts are this file's stand-in (B15): nothing reaches PAM or
//! Windows Hello, a machine id is a fixed byte string, and the OS the stand-in plays is its
//! own, so every OS's way runs on every CI leg.

use std::sync::Mutex;

use cli_core::{AccountPasswordRefusal, CliError};

use super::on_tests::Files;
use super::on_tests::{assert_locked_whole, lock, PASSWORD};
use super::on_tests::{files_store, new_input};
use super::*;
use crate::secrets::{read_store_file, LockSlotKind, SlotInfo};
use crate::vault::MachineId;
use crate::vault::{KdfPolicy, NewSlot, OsRandom, RecoveryKey, SlotInput, VaultId};
use crate::vault::{SlotBody, SlotRecord, FORMAT_V1};
use crate::vault::{SlotId, SlotKind, VaultFile, VaultHeader, VAULT_FILE};
use crate::Context;
use crate::SecretString;
use crate::{CancellationToken, CoreError, CoreResult, NullReporter, Outcome, Plan};
use cli_core::{AccountCheckUnavailable, AccountPasswordUnusable};
use cli_core::{LockPhase, StoreLock};

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
            "the build, before no_new_privs: Windows is unaffected (WI-545)",
            Stand {
                backend: CheckBackend::NoNewPrivs,
                ..Stand::ready(Windows)
            },
            WayFallback::HelloNotBuilt,
            2,
        ),
        (
            "no_new_privs, before the machine id, the back-off's record and the password \
             (WI-545)",
            Stand {
                backend: CheckBackend::NoNewPrivs,
                machine: MachineFact::NoId,
                backoff: BackoffFact::NoRecord,
                status: PasswordFact::NoPassword,
                ..Stand::ready(Linux)
            },
            WayFallback::NoNewPrivs,
            3,
        ),
        (
            "no_new_privs, before the back-off's refusal: a run that checks nothing is never \
             held back (B74; WI-545)",
            Stand {
                backend: CheckBackend::NoNewPrivs,
                backoff: BackoffFact::Wait {
                    wait_secs: Some(540),
                },
                ..Stand::ready(Linux)
            },
            WayFallback::NoNewPrivs,
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

/// WI-545: the reason's name on the wire, as the desktop reads it (c4's TypeScript, g2b's
/// Protect): the kernel's own name for the flag.
#[test]
fn no_new_privs_has_its_wire_name() {
    assert_eq!(
        serde_json::to_value(WayFallback::NoNewPrivs).unwrap(),
        serde_json::json!("no_new_privs")
    );
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

const ACCOUNT: &str = "hunter2 is not a password";

/// Another computer's machine id.
fn elsewhere() -> MachineId {
    MachineId::new(&[0x0b; 16]).unwrap()
}

/// What the stand-in check answers (B15).
#[derive(Debug, Clone)]
enum Answer {
    /// The system took the typed text: the slot of that text, bound to the machine id given.
    Verified,
    /// The system refused it for `reason`.
    Refused(AccountPasswordRefusal),
    /// "Verified", but the slot holds other text.
    OtherText,
    /// "Verified", but the slot is bound to another computer.
    OtherMachine,
    /// "Verified", but the slot is a lock password's.
    LockPasswordSlot,
}

/// The client's check with the system, played (B15): it counts its calls and never reaches
/// PAM; it builds the slot as the os-slots glue does (test code, which G-OS6 lets build it).
#[derive(Debug)]
struct Check {
    answer: Answer,
    calls: Mutex<u32>,
}

impl Check {
    fn answering(answer: Answer) -> Check {
        Check {
            answer,
            calls: Mutex::new(0),
        }
    }

    fn calls(&self) -> u32 {
        *self.calls.lock().unwrap()
    }
}

impl AccountPasswordCheck for Check {
    fn check(&self, typed: &SecretString, machine: &MachineId) -> AccountChecked {
        *self.calls.lock().unwrap() += 1;
        let slot = |text: &str, machine: MachineId| NewSlot::OsPassword {
            secret: SecretString::new(text),
            label: "account password of walker on box-a".into(),
            enrolled_at: 1_760_000_000,
            machine,
        };
        match &self.answer {
            Answer::Verified => AccountChecked::Verified(slot(typed.expose(), machine.clone())),
            Answer::Refused(reason) => AccountChecked::Refused {
                reason: reason.clone(),
                messages: vec!["Authentication failure".to_string()],
            },
            Answer::OtherText => {
                AccountChecked::Verified(slot("not what was typed", machine.clone()))
            }
            Answer::OtherMachine => AccountChecked::Verified(slot(typed.expose(), elsewhere())),
            Answer::LockPasswordSlot => AccountChecked::Verified(NewSlot::Password {
                secret: typed.clone(),
                label: "lock password".into(),
            }),
        }
    }
}

/// The account password's secret for `first_slot`: `typed`, checked by `check`.
fn account<'a>(typed: &str, check: &'a Check) -> FirstSecret<'a> {
    FirstSecret::AccountPassword {
        typed: SecretString::new(typed),
        check,
    }
}

/// What a run of `lock on` with this computer's account password left.
struct AccountRun {
    /// The recovery key's text.
    shown: String,
    vault_id: VaultId,
    recovery_id: SlotId,
    run: CoreResult<Outcome<LockedOn>>,
}

/// `lock on` with this computer's account password on `ctx` — a stand-in playing `os`, its
/// check taking `ACCOUNT` — interrupted at `at` when one is given.
fn account_password_run(ctx: &Context, os: HostOs, at: Option<FaultPoint>) -> AccountRun {
    let stand = Stand::ready(os);
    let check = Check::answering(Answer::Verified);
    let mut plan = plan_on(ctx, None, WayAsked::Everyday(&stand)).unwrap();
    let vault_id = plan.payload.vault_id().unwrap();
    let recovery_id = plan.payload.recovery_slot_id().unwrap();
    let (input, shown) = input_for(&mut plan, account(ACCOUNT, &check));
    assert_eq!(check.calls(), 1, "one check a run");
    let ctx = match at {
        Some(at) => ctx.clone().with_fault(at),
        None => ctx.clone(),
    };
    AccountRun {
        shown,
        vault_id,
        recovery_id,
        run: execute_on(&ctx, plan, input, &NullReporter, &CancellationToken::new()),
    }
}

/// B55, B56 with each stand-in (acceptance 1): the account password is checked once and
/// makes the first slot, bound to this computer — the vault holds the plan's ids,
/// `store.yaml` mirrors the slot, and the recovery key and the account password with this
/// computer's machine id open the store — and the run is D.4a's from there.
#[test]
fn the_account_password_makes_the_first_slot_bound_to_this_computer() {
    for os in [HostOs::Linux, HostOs::Macos] {
        let f = files_store();
        let run = account_password_run(&f.ctx, os, None);
        assert!(!done(run.run).already_locked, "{os:?}");
        let header = header_of(&f.ctx);
        assert_eq!(header.vault_id, run.vault_id, "{os:?}");
        let kinds: Vec<SlotKind> = header.slots.iter().map(|slot| slot.kind).collect();
        assert_eq!(
            kinds,
            [SlotKind::OsPassword, SlotKind::RecoveryKey],
            "{os:?}"
        );
        assert_eq!(header.slots[1].id, run.recovery_id, "{os:?}");
        assert!(header.slots[0].made_on(&here()), "{os:?}");
        assert!(!header.slots[0].made_on(&elsewhere()), "{os:?}");
        let file = read_store_file(f.ctx.config_root()).unwrap().unwrap();
        assert_eq!(
            file.slots,
            [
                SlotInfo {
                    kind: LockSlotKind::OsPassword,
                    label: "account password of walker on box-a".into(),
                },
                SlotInfo {
                    kind: LockSlotKind::RecoveryKey,
                    label: RECOVERY_LABEL.into(),
                },
            ],
            "{os:?}"
        );
        assert_locked_whole(&f.ctx, &run.shown);
        let input = SlotInput::AccountPassword(SecretString::new(ACCOUNT), here());
        let id = file.id.as_ref().unwrap();
        let opened = open_at(f.ctx.config_root(), id, input, &KdfPolicy::INSECURE_FAST).unwrap();
        assert_eq!(opened.vault_id, run.vault_id, "{os:?}");
    }
}

/// B75 (2) (acceptance 2): where this account's password status could not be read, an empty
/// entry is "no password": the plan falls back to a separate lock password before the system
/// is asked anything — `no_password` named, its Vault line a lock password's, its ids the
/// same (B80) — and takes the account password no more.
#[test]
fn an_empty_entry_falls_back_before_any_check() {
    let f = files_store();
    let stand = Stand {
        status: PasswordFact::Unknown,
        ..Stand::ready(HostOs::Macos)
    };
    let check = Check::answering(Answer::Verified);
    let mut plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
    assert!(plan.payload.empty_entry_falls_back());
    let minted = (plan.payload.vault_id(), plan.payload.recovery_slot_id());
    let step = first_slot(&mut plan, account("", &check)).unwrap();
    assert!(matches!(step, FirstStep::FellBack), "{step:?}");
    assert_eq!(check.calls(), 0, "nothing asked of the system");
    assert_eq!(
        (plan.payload.way(), plan.payload.fallback()),
        (Some(LockOnWay::LockPassword), Some(WayFallback::NoPassword))
    );
    assert!(plan.payload.notes().is_empty());
    assert!(!plan.payload.empty_entry_falls_back());
    assert_eq!(
        vault_line(&plan),
        "opened by a lock password or a recovery key"
    );
    assert_eq!(
        (plan.payload.vault_id(), plan.payload.recovery_slot_id()),
        minted
    );
    let again = first_slot(&mut plan, account(ACCOUNT, &check));
    assert!(
        matches!(again, Err(CoreError::Cli(CliError::Other(_)))),
        "{again:?}"
    );
    assert_eq!(check.calls(), 0);
    let secret = FirstSecret::LockPassword(SecretString::new(PASSWORD));
    let (input, _) = input_for(&mut plan, secret);
    done(execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    ));
    assert_eq!(Some(header_of(&f.ctx).vault_id), minted.0);
    assert_locked_whole(&f.ctx, PASSWORD);
}

/// Deviation 7 (revisit after use): where this account's password status reads as set, an
/// empty entry is a slip, not "no password" — said and asked again, the plan as it was, the
/// system asked nothing.
#[test]
fn an_empty_entry_where_the_account_has_a_password_is_asked_again() {
    let f = files_store();
    let stand = Stand::ready(HostOs::Linux);
    let check = Check::answering(Answer::Verified);
    let mut plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
    match first_slot(&mut plan, account("", &check)).unwrap() {
        FirstStep::Mistake(CoreError::Cli(CliError::AccountPasswordRefused {
            reason: AccountPasswordRefusal::NoPassword,
            ..
        })) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(check.calls(), 0);
    assert_eq!(
        (plan.payload.way(), plan.payload.fallback()),
        (Some(LockOnWay::AccountPassword), None)
    );
}

/// d4b-d's hand-off (F10, Deviation 13): an account password no slot holds — longer than
/// 1024 bytes, shaped like a recovery key — is said before the system's check and asked
/// again, so no check is spent on it; one the vault takes is checked then.
#[test]
fn a_password_no_slot_holds_is_said_before_the_check() {
    let f = files_store();
    let stand = Stand::ready(HostOs::Linux);
    let check = Check::answering(Answer::Verified);
    let mut plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
    let long = "a".repeat(1025);
    let shaped = RecoveryKey::generate(&mut OsRandom)
        .unwrap()
        .to_display()
        .expose()
        .to_string();
    for (typed, why) in [
        (
            long.as_str(),
            AccountPasswordUnusable::TooLong { max: 1024 },
        ),
        (shaped.as_str(), AccountPasswordUnusable::RecoveryKeyShape),
    ] {
        match first_slot(&mut plan, account(typed, &check)).unwrap() {
            FirstStep::Mistake(CoreError::Cli(CliError::AccountPasswordRefused {
                reason: AccountPasswordRefusal::Unusable { why: said },
                ..
            })) => assert_eq!(said, why),
            other => panic!("{why:?}: {other:?}"),
        }
    }
    assert_eq!(check.calls(), 0, "no check spent");
    let step = first_slot(&mut plan, account(ACCOUNT, &check)).unwrap();
    assert!(matches!(step, FirstStep::Ready { .. }), "{step:?}");
    assert_eq!(check.calls(), 1);
}

/// B55, B74, B75 (3) (acceptance 2): a wrong password, a stack that checked nothing, a
/// back-off in force by the time of the check and a check gone missing end the run —
/// `account_password_refused` with the system's words, nothing written (no `FirstSlot`
/// exists to write with: the snapshot guards a later step that would write), no recovery
/// key drawn (only `Ready` holds one) — and never fall back. One check a run: asked again,
/// the plan refuses before the system is asked.
#[test]
fn a_refused_check_ends_the_run_with_nothing_written() {
    for reason in [
        AccountPasswordRefusal::Wrong,
        AccountPasswordRefusal::NotChecked,
        AccountPasswordRefusal::Backoff {
            wait_secs: Some(600),
        },
        AccountPasswordRefusal::Unavailable {
            what: AccountCheckUnavailable::NoBackoffRecord,
        },
    ] {
        let f = files_store();
        let stand = Stand::ready(HostOs::Linux);
        let check = Check::answering(Answer::Refused(reason.clone()));
        let mut plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
        let before = crate::target::testkit::snapshot(f.ctx.config_root());
        match first_slot(&mut plan, account(ACCOUNT, &check)) {
            Err(CoreError::Cli(CliError::AccountPasswordRefused {
                reason: said,
                messages,
            })) => {
                assert_eq!(said, reason);
                assert_eq!(messages, ["Authentication failure"]);
            }
            other => panic!("{reason:?}: {other:?}"),
        }
        assert_eq!(
            plan.payload.way(),
            Some(LockOnWay::AccountPassword),
            "{reason:?}"
        );
        let again = first_slot(&mut plan, account(ACCOUNT, &check));
        assert!(
            matches!(again, Err(CoreError::Cli(CliError::Other(_)))),
            "{reason:?}: {again:?}"
        );
        assert_eq!(check.calls(), 1, "{reason:?}: one check a run");
        assert_eq!(
            crate::target::testkit::snapshot(f.ctx.config_root()),
            before,
            "{reason:?}"
        );
    }
}

/// F2: the client's check builds the slot (G-OS6), and the core takes it only for the text
/// typed, bound to this computer's machine id — a slot of other text, of another computer
/// or of a lock password is refused, nothing written.
#[test]
fn a_slot_not_made_for_the_text_typed_here_is_never_taken() {
    for answer in [
        Answer::OtherText,
        Answer::OtherMachine,
        Answer::LockPasswordSlot,
    ] {
        let f = files_store();
        let stand = Stand::ready(HostOs::Linux);
        let check = Check::answering(answer.clone());
        let mut plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
        let before = crate::target::testkit::snapshot(f.ctx.config_root());
        let step = first_slot(&mut plan, account(ACCOUNT, &check));
        assert!(
            matches!(step, Err(CoreError::Cli(CliError::Other(_)))),
            "{answer:?}: {step:?}"
        );
        assert_eq!(
            crate::target::testkit::snapshot(f.ctx.config_root()),
            before,
            "{answer:?}"
        );
    }
}

/// B2 (critic round, finding 3): a short account password the system took comes with O2's
/// advice, read from the first slot — the client asks "Add it anyway? [y/N]" before the
/// recovery key is shown and keeps no copy of the password for it; a longer one comes with
/// none.
#[test]
fn a_short_account_password_comes_with_its_advice() {
    let f = files_store();
    let stand = Stand::ready(HostOs::Linux);
    let check = Check::answering(Answer::Verified);
    for (typed, advice) in [
        (
            "hunter2",
            Some(PasswordAdvice::Short {
                chars: 7,
                recommended: 15,
            }),
        ),
        (ACCOUNT, None),
    ] {
        let mut plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
        match first_slot(&mut plan, account(typed, &check)).unwrap() {
            FirstStep::Ready { first, .. } => assert_eq!(first.advice(), advice, "{typed}"),
            other => panic!("{typed}: {other:?}"),
        }
    }
}

/// A lock-password run interrupted with its vault on disk, then this computer's
/// account-password slot added to that vault — a vault no `lock on` writes, which opens by
/// both typed kinds (critic-plan M1's case): the recovery key's text.
fn interrupted_with_both_kinds(f: &Files) -> String {
    let plan = plan_on(&f.ctx, None, WayAsked::LockPassword).unwrap();
    let (input, shown) = new_input();
    let at = f.ctx.clone().with_fault(FaultPoint::AfterVaultVerified);
    execute_on(&at, plan, input, &NullReporter, &CancellationToken::new()).unwrap_err();
    let root = f.ctx.config_root();
    let id = read_store_file(root).unwrap().unwrap().id.unwrap();
    let input = SlotInput::from_input(SecretString::new(shown.as_str()), None);
    let key = open_at(root, &id, input, &KdfPolicy::INSECURE_FAST).unwrap();
    let path = root.join(VAULT_FILE);
    let bytes = crate::vault::file::read(&path).unwrap();
    let mut vault = VaultFile::parse(&bytes)
        .unwrap()
        .open_with_key(&key.key, key.vault_id)
        .unwrap();
    let slot = NewSlot::OsPassword {
        secret: SecretString::new(ACCOUNT),
        label: "account password of walker on box-a".into(),
        enrolled_at: 1_760_000_000,
        machine: here(),
    };
    vault
        .add_slot(slot, &KdfPolicy::INSECURE_FAST, &mut OsRandom)
        .unwrap();
    let sealed = vault.seal("apprafter 0.0.0-test", &mut OsRandom).unwrap();
    let held = StoreLock::exclusive(&f.ctx.store()).unwrap();
    crate::vault::file::replace(&path, &sealed, &held).unwrap();
    shown
}

/// B56, critic-plan M1 (acceptance 3): a resume asks its plan's everyday slot by that slot's
/// own kind and takes no other — the account password typed at the lock password's question
/// opens nothing, nor the lock password at the account password's, and a vault opened by
/// the account-password slot where the plan asked the lock password's finishes nothing and
/// writes nothing; the lock password and the recovery key finish it.
#[test]
fn a_lock_password_resume_takes_no_account_password() {
    let f = files_store();
    let shown = interrupted_with_both_kinds(&f);
    let root = f.ctx.config_root().to_path_buf();
    let plan = plan_on(&f.ctx, None, WayAsked::LockPassword).unwrap();
    let LockOnStart::Resume { id, everyday } = plan.payload.start().clone() else {
        panic!("{:?}", plan.payload.start());
    };
    assert_eq!(everyday, LockSlotKind::Password);
    let slot = |kind, secret: &str, machine| {
        let secret = SecretString::new(secret);
        open_slot_at(&root, &id, kind, secret, machine, &KdfPolicy::INSECURE_FAST)
    };
    let one_attempt = |opened: CoreResult<SlotOpened>| {
        matches!(
            opened,
            Err(CoreError::Cli(CliError::UnlockFailed { attempts: 1, .. }))
        )
    };
    assert!(
        one_attempt(slot(LockSlotKind::Password, ACCOUNT, Some(here()))),
        "the account password at the lock password's question"
    );
    assert!(
        one_attempt(slot(LockSlotKind::OsPassword, PASSWORD, Some(here()))),
        "the lock password at the account password's question"
    );
    let by_account = slot(LockSlotKind::OsPassword, ACCOUNT, Some(here())).unwrap();
    assert_eq!(by_account.kind(), LockSlotKind::OsPassword);
    let recovery = || slot(LockSlotKind::RecoveryKey, shown.as_str(), None).unwrap();
    let before = crate::target::testkit::snapshot(&root);
    let input = LockOnInput::Resume {
        everyday: by_account,
        recovery: recovery(),
    };
    let run = execute_on(
        &f.ctx,
        plan.clone(),
        input,
        &NullReporter,
        &CancellationToken::new(),
    );
    assert!(
        matches!(
            run,
            Err(CoreError::Cli(CliError::LockIncomplete {
                phase: LockPhase::Locking,
                ..
            }))
        ),
        "{run:?}"
    );
    assert_eq!(
        crate::target::testkit::snapshot(&root),
        before,
        "nothing written"
    );
    let input = LockOnInput::Resume {
        everyday: slot(LockSlotKind::Password, PASSWORD, None).unwrap(),
        recovery: recovery(),
    };
    done(execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    ));
    assert_locked_whole(&f.ctx, &shown);
}

/// B56 (acceptance 3): a run of the account password interrupted with its vault on disk
/// resumes over that slot and the recovery key, each opened by its own kind — the account
/// password with this computer's machine id, never without it, never as a lock password —
/// and finishes; the resume asks this computer nothing.
#[test]
fn an_account_password_run_resumes_over_its_own_slot() {
    let f = files_store();
    let run = account_password_run(&f.ctx, HostOs::Linux, Some(FaultPoint::AfterVaultVerified));
    let e = run.run.unwrap_err();
    assert!(e.to_string().contains("injected fault"), "{e}");
    let stand = Stand::ready(HostOs::Linux);
    let plan = plan_on(&f.ctx, None, WayAsked::Everyday(&stand)).unwrap();
    assert!(stand.asked().is_empty(), "{:?}", stand.asked());
    let LockOnStart::Resume { id, everyday } = plan.payload.start().clone() else {
        panic!("{:?}", plan.payload.start());
    };
    assert_eq!(everyday, LockSlotKind::OsPassword);
    assert_eq!(
        vault_line(&plan),
        "written by the earlier run; opened with your account password and with the \
         recovery key before anything leaves the files"
    );
    let root = f.ctx.config_root().to_path_buf();
    let slot = |kind, secret: &str, machine| {
        let secret = SecretString::new(secret);
        open_slot_at(&root, &id, kind, secret, machine, &KdfPolicy::INSECURE_FAST)
    };
    assert!(
        matches!(
            slot(LockSlotKind::Password, ACCOUNT, None),
            Err(CoreError::Cli(CliError::UnlockFailed { attempts: 1, .. }))
        ),
        "never as a lock password"
    );
    match slot(LockSlotKind::OsPassword, ACCOUNT, None) {
        Err(CoreError::Cli(CliError::AccountPasswordRefused {
            reason:
                AccountPasswordRefusal::Unavailable {
                    what: AccountCheckUnavailable::NoMachineId,
                },
            ..
        })) => {}
        other => panic!("without this computer's machine id: {other:?}"),
    }
    let everyday = slot(LockSlotKind::OsPassword, ACCOUNT, Some(here())).unwrap();
    assert_eq!(everyday.kind(), LockSlotKind::OsPassword);
    let recovery = slot(LockSlotKind::RecoveryKey, run.shown.as_str(), None).unwrap();
    let input = LockOnInput::Resume { everyday, recovery };
    done(execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    ));
    assert_locked_whole(&f.ctx, &run.shown);
}

/// B56, Deviation 10: a resume finishes over an everyday slot this build opens by a typed
/// secret — a lock password's or an account password's, wherever it sits — and over none
/// other: a Windows Hello slot (until d4b-w2), or a vault holding the recovery key alone,
/// is `lock off`'s to back out.
#[test]
fn a_resume_takes_an_everyday_slot_this_build_opens() {
    let header = |kinds: &[SlotKind]| VaultHeader {
        format_version: FORMAT_V1,
        writer: "test".into(),
        vault_id: VaultId([1; 16]),
        store_id: "0123456789abcdef".into(),
        slots: kinds
            .iter()
            .enumerate()
            .map(|(i, kind)| SlotRecord {
                id: SlotId([i as u8; 16]),
                kind: *kind,
                body: SlotBody::Opaque(Vec::new()),
                label: String::new(),
            })
            .collect(),
    };
    let everyday = |kinds: &[SlotKind]| super::on::everyday_slot(&header(kinds));
    assert_eq!(
        everyday(&[SlotKind::Password, SlotKind::RecoveryKey]),
        Some(LockSlotKind::Password)
    );
    assert_eq!(
        everyday(&[SlotKind::RecoveryKey, SlotKind::OsPassword]),
        Some(LockSlotKind::OsPassword)
    );
    assert_eq!(
        everyday(&[SlotKind::WindowsHello, SlotKind::RecoveryKey]),
        None
    );
    assert_eq!(everyday(&[SlotKind::RecoveryKey]), None);
}
