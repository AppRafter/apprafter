// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter lock on`'s ways (D.4b-e1a; overview B55, B56, B74, B75, B88; ADR 0069 §5) as a
//! person meets them at a terminal, over the scripted account-password stand-in (B15, B16): the
//! default, each fallback reason on the OS it belongs to, each refusal, the empty entry and the
//! resume over the account password — each a transcript golden (`common::transcript`). Built
//! only with the dev-only feature `test-os-slots` (`required-features` in Cargo.toml: no such
//! target in a plain build; every `--all-features` CI leg runs it). `APPRAFTER_TEST_OS_SLOTS`
//! names the script each child reads, so nothing here reaches PAM, Windows Hello or this
//! computer's machine id, and the stand-in's `<script>.calls` shows what each run asked of the
//! system, in order. The stand-in plays the OS a case needs, so every OS's way runs on every
//! Unix leg. Unix: the pseudo-terminal's.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use apprafter_core::vault::{SlotKind, VaultFile};
use common::spawn::Spawn;
use common::terminal::{converse, line, shown_recovery_key, Ended, Step};
use common::transcript::check_transcript;

const TOKEN: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
/// The account password the stand-in's system takes (25 characters: no short-password
/// question).
const ACCOUNT: &str = "walker-account-password-1";
const PASSWORD: &str = "correct horse battery staple";
/// The variable that names the stand-in's script (`src/os_slots.rs`).
const STAND_IN: &str = "APPRAFTER_TEST_OS_SLOTS";
/// Every case's script starts so: Linux, `walker` on `box-a`, a usable password.
const DEFAULTS: &str = "os linux\naccount walker\nhost box-a\nstatus usable\n";
/// The system takes ACCOUNT.
const VERIFIED: &str = "check verified-if walker-account-password-1\n";
const NEW: &str = "New lock password for ";
const REPEAT: &str = "Repeat the lock password: ";
const CONFIRM: &str = "Type the recovery key to confirm you saved it";
const FALLS_BACK: &str = "The store gets a separate lock password instead.";

/// One run that derives keys at a time.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A scratch machine whose children read the stand-in's script — DEFAULTS, then `lines`, the
/// last line of a keyword winning — holding `prod` in a store in files; and the script's path.
fn scripted(lines: &str) -> (Spawn, PathBuf) {
    let sp = Spawn::new();
    let script = sp.path("os-slots.script");
    std::fs::write(&script, format!("{DEFAULTS}{lines}")).unwrap();
    let sp = sp.with_env(STAND_IN, &script);
    seeded(&sp);
    (sp, script)
}

/// `prod` (TOKEN), the CLI default, in a store in files (as `lock_test.rs` seeds it).
fn seeded(sp: &Spawn) {
    let store = cli_core::TargetStorePaths::for_root(sp.store());
    cli_core::save_target(
        &store,
        &cli_core::Target {
            name: "prod".into(),
            config: cli_core::TargetConfig {
                provider: "hetzner-cloud".into(),
                region: Some("nbg1".into()),
                ..Default::default()
            },
            credentials: cli_core::TargetCredentials {
                hetzner_token: Some(TOKEN.into()),
            },
        },
    )
    .unwrap();
    cli_core::save_global_config(
        &store,
        &cli_core::GlobalConfig {
            active_target: "prod".into(),
            version: cli_core::TARGET_STORE_VERSION,
        },
    )
    .unwrap();
}

/// What the stand-in was asked, in order (its `<script>.calls`, without its first line).
fn calls(script: &Path) -> Vec<String> {
    let mut log = script.as_os_str().to_owned();
    log.push(".calls");
    match std::fs::read_to_string(PathBuf::from(log)) {
        Ok(text) => text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(str::to_owned)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// How many checks the system was asked to make.
fn checks(script: &Path) -> usize {
    calls(script)
        .iter()
        .filter(|c| c.starts_with("Check"))
        .count()
}

/// The kinds of the slots of `sp`'s vault, in its header's order.
fn slot_kinds(sp: &Spawn) -> Vec<SlotKind> {
    let bytes = std::fs::read(sp.store().join("vault")).unwrap();
    let vault = VaultFile::parse(&bytes).unwrap();
    vault.header().slots.iter().map(|slot| slot.kind).collect()
}

/// Nothing of the store moved: no lock state, no vault.
fn nothing_written(sp: &Spawn) -> bool {
    !sp.store().join("store.yaml").exists() && !sp.store().join("vault").exists()
}

fn type_key(terminal: &str) -> Vec<u8> {
    format!("{}\r", shown_recovery_key(terminal)).into_bytes()
}

/// The default run: the account password, then the recovery key typed back.
fn default_run(sp: &Spawn) -> Ended {
    converse(
        sp,
        &["lock", "on"],
        &[
            Step::Expect("It will open with your account password on this computer"),
            Step::Expect("Account password of walker: "),
            line(ACCOUNT),
            Step::Expect(CONFIRM),
            Step::TypeFrom(type_key),
        ],
    )
}

/// `sp`'s locked store as a run interrupted with its vault on disk leaves it for a resume
/// (as `lock_test.rs` makes one).
fn interrupted(sp: &Spawn) {
    let root = sp.store();
    let held = cli_core::StoreLock::exclusive(&cli_core::TargetStorePaths::for_root(root.clone()))
        .unwrap();
    let mut file = apprafter_core::secrets::read_store_file(&root)
        .unwrap()
        .unwrap();
    file.secrets = apprafter_core::secrets::SecretsMode::Locking;
    apprafter_core::secrets::write_store_file(&root, &file, &held).unwrap();
}

/// B55, B56 (acceptance 2): the default sets the store up to open with this computer's
/// account password — its costs said, checked once — and the recovery key; the store then
/// opens with that password at `lock off`'s prompt (the machine id comes from the stand-in).
#[test]
fn the_default_locks_with_the_account_password_checked_once() {
    let _one = one_at_a_time();
    let (sp, script) = scripted(VERIFIED);
    let end = default_run(&sp);
    assert!(end.status.success(), "{}", end.terminal);
    assert!(end.restored);
    assert_eq!(
        calls(&script),
        [
            "Backend",
            "MachineId",
            "Backoff",
            "Status",
            "Check { chars: 25 }"
        ]
    );
    assert_eq!(
        slot_kinds(&sp),
        [SlotKind::OsPassword, SlotKind::RecoveryKey]
    );
    check_transcript("lock/lock_on_default", &sp, &end);
    let off = converse(
        &sp,
        &["lock", "off"],
        &[
            Step::Expect("Lock password or recovery key of "),
            line(ACCOUNT),
        ],
    );
    assert!(off.status.success(), "{}", off.terminal);
    assert!(off.stdout.contains("Unlocked for good:"), "{}", off.stdout);
    assert_eq!(
        checks(&script),
        1,
        "opening a slot asks the system nothing (B13)"
    );
}

/// B88 (acceptance 2): a fallback reason, played as `lines` say on the OS it belongs to, is
/// said first — `says`, then that the store gets a lock password — then D.4a's dialogue,
/// cancelled at its first prompt with nothing written; the system was asked exactly `asked`,
/// in B88's order, and no check; the transcript is the golden `case`, named whole so
/// `golden.rs`'s orphan guard finds it. One test per reason (critic round, finding 1): the
/// golden harness's update mode writes a test's first golden and fails on purpose, so a loop
/// would record one.
fn a_fallback_is_said_first(case: &str, lines: &str, says: &'static str, asked: &[&str]) {
    let (sp, script) = scripted(lines);
    let end = converse(
        &sp,
        &["lock", "on"],
        &[
            Step::Expect(says),
            Step::Expect(FALLS_BACK),
            Step::Expect(NEW),
            Step::Type(vec![0x03]),
        ],
    );
    assert_eq!(end.status.code(), Some(1), "{case}: {}", end.terminal);
    assert!(
        end.terminal.contains("apprafter::op::cancelled"),
        "{case}: {}",
        end.terminal
    );
    assert_eq!(calls(&script), asked, "{case}");
    assert!(nothing_written(&sp), "{case}");
    check_transcript(case, &sp, &end);
}

#[test]
fn a_fallback_said_first_hello_not_built_on_windows() {
    a_fallback_is_said_first(
        "lock/lock_on_fallback_hello_not_built",
        "os windows\n",
        "This AppRafter cannot open a store with Windows Hello yet.",
        &[],
    );
}

#[test]
fn a_fallback_said_first_no_libpam() {
    a_fallback_is_said_first(
        "lock/lock_on_fallback_no_libpam",
        "backend no-libpam\n",
        "libpam is not installed.",
        &["Backend"],
    );
}

#[test]
fn a_fallback_said_first_no_service() {
    a_fallback_is_said_first(
        "lock/lock_on_fallback_no_service",
        "backend no-service\n",
        "none of the sign-in services AppRafter uses",
        &["Backend"],
    );
}

#[test]
fn a_fallback_said_first_no_check_route_on_macos() {
    a_fallback_is_said_first(
        "lock/lock_on_fallback_no_check_route",
        "os macos\nbackend no-check-route\n",
        "the system offers no check that sees what was typed.",
        &["Backend"],
    );
}

#[test]
fn a_fallback_said_first_no_account() {
    a_fallback_is_said_first(
        "lock/lock_on_fallback_no_account",
        "account none\n",
        "the user running AppRafter has no account entry.",
        &["Backend"],
    );
}

/// WI-545: under no_new_privs the plan falls back at B88 (3) — after the account, before the
/// machine id — though a back-off is in force and the account has no password: the system was
/// asked only whether it can check, never the back-off's record or the password's status.
#[test]
fn a_fallback_said_first_no_new_privs() {
    a_fallback_is_said_first(
        "lock/lock_on_fallback_no_new_privs",
        "backend no-new-privs\nbackoff wait 540 recent-failure\nstatus empty\n",
        "it runs with no_new_privs",
        &["Backend"],
    );
}

#[test]
fn a_fallback_said_first_no_machine_id() {
    a_fallback_is_said_first(
        "lock/lock_on_fallback_no_machine_id",
        "machine-id missing\n",
        "no machine id to bind an account-password slot to",
        &["Backend", "MachineId"],
    );
}

#[test]
fn a_fallback_said_first_no_backoff_record() {
    a_fallback_is_said_first(
        "lock/lock_on_fallback_no_backoff_record",
        "backoff no-record\n",
        "the record that spaces out failed checks",
        &["Backend", "MachineId", "Backoff"],
    );
}

#[test]
fn a_fallback_said_first_no_password() {
    a_fallback_is_said_first(
        "lock/lock_on_fallback_no_password",
        "status locked\n",
        "This account has no password the system can check",
        &["Backend", "MachineId", "Backoff", "Status"],
    );
}

/// B74 (acceptance 2): a back-off in force refuses the run when it is planned — nothing told
/// before it, nothing written, no recovery key — naming the wait and `--lock-password`; it
/// never falls back.
#[test]
fn a_back_off_in_force_refuses_before_anything_is_asked() {
    let (sp, script) = scripted(&format!("backoff wait 540 recent-failure\n{VERIFIED}"));
    let end = converse(&sp, &["lock", "on"], &[]);
    assert_eq!(end.status.code(), Some(1), "{}", end.terminal);
    for said in [
        "apprafter::secrets::account_password_refused",
        "the next is allowed in 9 min",
        "--lock-password",
    ] {
        assert!(end.terminal.contains(said), "{said}: {}", end.terminal);
    }
    assert!(
        !end.terminal.contains("Lock the secrets"),
        "{}",
        end.terminal
    );
    assert!(!end.terminal.contains(FALLS_BACK), "{}", end.terminal);
    assert_eq!(
        calls(&script),
        ["Backend", "MachineId", "Backoff", "Status"]
    );
    assert!(nothing_written(&sp));
    check_transcript("lock/lock_on_refused_backoff", &sp, &end);
}

/// B75 (3) (acceptance 2): a check that did not see the typed text ends the run — nothing
/// written, no recovery key shown.
#[test]
fn a_check_that_saw_no_typed_password_ends_the_run() {
    let (sp, script) = scripted("check not-checked no-password-asked\n");
    let end = converse(
        &sp,
        &["lock", "on"],
        &[Step::Expect("Account password of walker: "), line(ACCOUNT)],
    );
    assert_eq!(end.status.code(), Some(1), "{}", end.terminal);
    assert!(
        end.terminal
            .contains("apprafter::secrets::account_password_refused"),
        "{}",
        end.terminal
    );
    assert!(
        !end.terminal.contains("APPRAFTER-RECOVERY-KEY-1"),
        "{}",
        end.terminal
    );
    assert_eq!(checks(&script), 1);
    assert!(nothing_written(&sp));
    check_transcript("lock/lock_on_refused_not_checked", &sp, &end);
}

/// B55 (acceptance 2): a wrong account password ends the run with c1's refusal — nothing
/// written, no recovery key shown, one check.
#[test]
fn a_wrong_account_password_ends_the_run_with_nothing_written() {
    let (sp, script) = scripted("check wrong\n");
    let end = converse(
        &sp,
        &["lock", "on"],
        &[
            Step::Expect("Account password of walker: "),
            line("not the password"),
        ],
    );
    assert_eq!(end.status.code(), Some(1), "{}", end.terminal);
    assert!(
        end.terminal
            .contains("apprafter::secrets::account_password_refused"),
        "{}",
        end.terminal
    );
    assert!(
        !end.terminal.contains("APPRAFTER-RECOVERY-KEY-1"),
        "{}",
        end.terminal
    );
    assert_eq!(
        calls(&script).last().map(String::as_str),
        Some("Check { chars: 16 }")
    );
    assert!(nothing_written(&sp));
    check_transcript("lock/lock_on_wrong_account_password", &sp, &end);
}

/// B75 (2) (acceptance 2): where the password status cannot be read, Enter on an empty line
/// means no password — the store gets a separate lock password, said, and D.4a's dialogue
/// locks it; the system is asked nothing.
#[test]
fn an_empty_entry_where_the_status_is_unknown_takes_a_lock_password() {
    let _one = one_at_a_time();
    let (sp, script) = scripted(&format!("status unknown\n{VERIFIED}"));
    let end = converse(
        &sp,
        &["lock", "on"],
        &[
            Step::Expect("Account password of walker (Enter on an empty line if it has none): "),
            line(""),
            Step::Expect("You pressed Enter on an empty line"),
            Step::Expect(NEW),
            line(PASSWORD),
            Step::Expect(REPEAT),
            line(PASSWORD),
            Step::Expect(CONFIRM),
            Step::TypeFrom(type_key),
        ],
    );
    assert!(end.status.success(), "{}", end.terminal);
    assert_eq!(slot_kinds(&sp), [SlotKind::Password, SlotKind::RecoveryKey]);
    assert_eq!(checks(&script), 0, "an empty entry reaches no check");
    check_transcript("lock/lock_on_empty_entry", &sp, &end);
}

/// B56 (acceptance 2): a default run interrupted with its vault on disk resumes over the
/// account password and the recovery key, each by its own question, and asks the system
/// nothing (B13).
#[test]
fn an_interrupted_default_run_resumes_over_the_account_password() {
    let _one = one_at_a_time();
    let (sp, script) = scripted(VERIFIED);
    let first = default_run(&sp);
    assert!(first.status.success(), "{}", first.terminal);
    let key = shown_recovery_key(&first.terminal);
    interrupted(&sp);
    let end = converse(
        &sp,
        &["lock", "on"],
        &[
            Step::Expect("did not finish; finishing it."),
            Step::Expect("Account password of "),
            line(ACCOUNT),
            Step::Expect("Recovery key of "),
            line(&key),
        ],
    );
    assert!(end.status.success(), "{}", end.terminal);
    assert!(
        end.terminal
            .contains("opened with your account password and with the recovery key"),
        "{}",
        end.terminal
    );
    assert_eq!(checks(&script), 1, "the resume checks nothing");
    let yaml = std::fs::read_to_string(sp.store().join("store.yaml")).unwrap();
    assert!(yaml.contains("secrets: locked"), "{yaml}");
    check_transcript("lock/lock_on_resume_account_password", &sp, &end);
}
