// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The macOS account-password check against accounts the CI job makes:
//! `scripts/test-os-slots-macos.sh` creates `walker` (its home `/Users/walker` holds the
//! test-account marker) and `walker2` (none) on a GitHub-hosted runner and runs exactly one case
//! as one of them. PAM through OpenPAM's `/usr/lib/libpam.2.dylib`, loaded at run time, and the
//! service `chkpasswd` (`pam_opendirectory` alone).
//!
//! They run nowhere else (CLAUDE.md; ADR 0069 answer 6): [`job`] refuses unless the script said
//! so and the test runs as the job-made account it expects, not root, and every real backend
//! comes from `fence::admit` with `/Users` as the only sandbox root, which admits only a marked
//! home. The script itself refuses to run outside a GitHub-hosted runner.
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::time::SystemTime;

use apprafter_os_slots::account::current_account;
use apprafter_os_slots::fence::{self, FencedWhy, SystemAccountPassword};
use apprafter_os_slots::{
    AccountPassword, BackoffState, CheckOutcome, HostFacts, InputRefused, PasswordChanged,
    PasswordState, SystemHost,
};
use zeroize::Zeroizing;

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is unset: run these through scripts/test-os-slots-macos.sh")
    })
}

/// The job's password, after checking that the job runs this test as `account`.
fn job(account: &str) -> Zeroizing<String> {
    assert_eq!(
        var("APPRAFTER_OS_SLOTS_MACOS_JOB"),
        "1",
        "run these through scripts/test-os-slots-macos.sh on a GitHub-hosted runner"
    );
    let me = current_account().expect("a passwd entry");
    assert_eq!(me.name, account.as_bytes(), "the case runs as {account}");
    assert_ne!(me.uid, 0, "the cases do not run as root");
    Zeroizing::new(var("APPRAFTER_OS_SLOTS_PASSWORD"))
}

fn backend() -> SystemAccountPassword {
    fence::admit(Some(&[PathBuf::from("/Users")]))
        .expect("walker is the marked test account under /Users")
}

fn now() -> SystemTime {
    SystemTime::now()
}

#[test]
#[ignore = "needs a job-made macOS account: bash scripts/test-os-slots-macos.sh"]
fn the_right_password_is_verified_through_chkpasswd() {
    let password = job("walker");
    let backend = backend();
    assert_eq!(backend.backend(), Ok(()));
    for _ in 0..2 {
        let check = backend.check(&password, now());
        assert_eq!(check.outcome, CheckOutcome::Verified, "{check:?}");
    }
}

#[test]
#[ignore = "needs a job-made macOS account: bash scripts/test-os-slots-macos.sh"]
fn a_wrong_password_is_wrong_and_the_next_run_is_refused_before_pam() {
    let password = job("walker");
    let wrong = Zeroizing::new(format!("not-{}", password.as_str()));
    assert_eq!(backend().check(&wrong, now()).outcome, CheckOutcome::Wrong);
    let refused = backend().check(&password, now());
    assert!(
        matches!(refused.outcome, CheckOutcome::Backoff(_)),
        "{refused:?}"
    );
}

#[test]
#[ignore = "needs a job-made macOS account: bash scripts/test-os-slots-macos.sh"]
fn an_empty_entry_never_reaches_pam() {
    job("walker");
    let backend = backend();
    let empty = backend.check(&Zeroizing::new(String::new()), now());
    assert_eq!(empty.outcome, CheckOutcome::Refused(InputRefused::Empty));
    assert_eq!(backend.backoff(now()), Ok(BackoffState::Clear));
}

/// The hardware UUID `ioreg` shows, as `gethostuuid`'s 16 bytes; the per-user temporary
/// directory is the back-off's place.
#[test]
#[ignore = "needs a job-made macOS account: bash scripts/test-os-slots-macos.sh"]
fn the_hardware_uuid_is_its_16_bytes_and_the_back_off_has_its_place() {
    job("walker");
    let hex: String = var("APPRAFTER_OS_SLOTS_HOST_UUID")
        .chars()
        .filter(|c| *c != '-')
        .collect();
    let expected: Vec<u8> = (0..32)
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    assert_eq!(
        SystemHost::new(false).machine_id().unwrap().expose(),
        expected
    );
    assert_eq!(backend().backoff(now()), Ok(BackoffState::Clear));
}

/// An empty password reads like a set one on macOS: unknown; the change time is
/// `passwordLastSetTime`, the account's creation by the job.
#[test]
#[ignore = "needs a job-made macOS account: bash scripts/test-os-slots-macos.sh"]
fn the_password_status_is_unknown_and_its_change_time_is_read() {
    job("walker");
    let status = backend().password_status();
    assert_eq!(status.state, PasswordState::Unknown, "{status:?}");
    let recent = matches!(status.changed, PasswordChanged::At { unix_seconds } if unix_seconds > 1_700_000_000);
    assert!(recent, "{status:?}");
}

#[test]
#[ignore = "needs a job-made macOS account: bash scripts/test-os-slots-macos.sh"]
fn an_unmarked_account_is_fenced() {
    job("walker2");
    let refused = fence::admit(Some(&[PathBuf::from("/Users")])).unwrap_err();
    assert_eq!(
        refused.why,
        FencedWhy::NoMarker {
            home: PathBuf::from("/Users/walker2")
        }
    );
}
