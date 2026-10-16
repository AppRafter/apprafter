// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The account-password check against real PAM stacks, as a normal user, inside the throwaway
//! containers `scripts/test-os-slots-linux.sh` builds (Debian 13 and Fedora 42): their libpam,
//! loaded at run time, `pam_unix` and its `unix_chkpwd`, a test account `walker` with a known
//! password, its home `/home/walker` holding the test-account marker, and `/run/user/<uid>` as
//! logind makes it. The script prepares what each case needs and runs exactly that test as
//! `walker`.
//!
//! ```text
//! bash scripts/test-os-slots-linux.sh       # every case, on both distributions
//! ```
//!
//! They run nowhere else (CLAUDE.md; ADR 0069 answer 6): [`walker`] refuses unless the script
//! said this is its container and the test runs as `walker`, not root, and every real backend
//! comes from `fence::admit` with `/home` as the only sandbox root, which admits only an account
//! whose home holds the marker. Outside the container they fail; they never pass quietly.
#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::time::SystemTime;

use apprafter_os_slots::account::current_account;
use apprafter_os_slots::fence::{self, FencedWhy, SystemAccountPassword};
use apprafter_os_slots::machine::{NoMachineId, SystemHost};
use apprafter_os_slots::{
    AccountPassword, BackoffState, BackoffWait, CheckOutcome, Environment, HostFacts, InputRefused,
    NotCheckedWhy, PasswordChanged, PasswordState, Unavailable, WaitWhy,
};
use zeroize::Zeroizing;

/// The one sandbox root these cases pass: `walker`'s home is `/home/walker`.
fn roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/home")]
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is unset: run these through scripts/test-os-slots-linux.sh")
    })
}

/// The container's password for `walker`, after checking that this is the container and that
/// the test runs as `walker`: as root, `pam_unix` would read `/etc/shadow` itself, and the
/// set-uid helper a person's check goes through would go untested.
fn walker() -> Zeroizing<String> {
    assert_eq!(
        var("APPRAFTER_OS_SLOTS_CONTAINER"),
        "1",
        "run these through scripts/test-os-slots-linux.sh: they check a password with the system's PAM"
    );
    let account = current_account().expect("walker has a passwd entry");
    assert_eq!(account.name, b"walker", "the cases run as walker");
    assert_ne!(account.uid, 0, "the cases do not run as root");
    Zeroizing::new(var("APPRAFTER_OS_SLOTS_PASSWORD"))
}

/// A real backend, through the fence, as every product path gets one.
fn backend() -> SystemAccountPassword {
    fence::admit(Some(&roots())).expect("walker is the marked test account under /home")
}

fn now() -> SystemTime {
    SystemTime::now()
}

fn text(value: &str) -> Zeroizing<String> {
    Zeroizing::new(value.to_owned())
}

fn recent_failure(state: Result<BackoffState, Unavailable>) -> bool {
    matches!(
        state,
        Ok(BackoffState::Wait(BackoffWait {
            why: WaitWhy::RecentFailure,
            ..
        }))
    )
}

/// Debian's `common-auth`, Fedora's `system-auth`: `pam_unix`, which cannot read `/etc/shadow`
/// as walker, asks `unix_chkpwd`. A success is not counted, so a second check runs at once.
/// `passwd -S` reads the password usable, changed the day the image set it.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn the_right_password_is_verified_and_not_counted() {
    let password = walker();
    let backend = backend();
    assert_eq!(backend.backend(), Ok(()));
    assert_eq!(backend.backoff(now()), Ok(BackoffState::Clear));
    for _ in 0..2 {
        let check = backend.check(&password, now());
        assert_eq!(check.outcome, CheckOutcome::Verified, "{check:?}");
    }
    let status = backend.password_status();
    assert_eq!(status.state, PasswordState::Usable, "{status:?}");
    let recent = matches!(status.changed, PasswordChanged::Day { days_since_epoch } if days_since_epoch > 20_000);
    assert!(recent, "{status:?}");
}

/// The failure is on disk before PAM is asked, so another process — a fresh backend over the
/// same record — is refused for ten minutes: the right password, which PAM would verify, comes
/// back refused.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn a_wrong_password_is_wrong_and_the_next_run_is_refused_before_pam() {
    let password = walker();
    let check = backend().check(&text(&format!("not-{}", password.as_str())), now());
    assert_eq!(check.outcome, CheckOutcome::Wrong, "{check:?}");
    let next = backend();
    assert!(
        recent_failure(next.backoff(now())),
        "{:?}",
        next.backoff(now())
    );
    let refused = next.check(&password, now());
    assert!(
        matches!(refused.outcome, CheckOutcome::Backoff(_)),
        "{refused:?}"
    );
}

/// `auth sufficient pam_permit.so`: success without a prompt.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn a_stack_that_asks_for_nothing_is_not_checked_and_not_counted() {
    let password = walker();
    let backend = backend();
    let unasked = CheckOutcome::NotChecked(NotCheckedWhy::NoPasswordAsked);
    assert_eq!(backend.check(&password, now()).outcome, unasked);
    assert_eq!(
        backend.backoff(now()),
        Ok(BackoffState::Clear),
        "PAM reported no failure"
    );
}

/// `pam_permit` before `pam_unix`, as a fingerprint module succeeds first.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn a_fingerprint_module_before_the_password_is_not_checked() {
    let password = walker();
    let unasked = CheckOutcome::NotChecked(NotCheckedWhy::NoPasswordAsked);
    assert_eq!(backend().check(&password, now()).outcome, unasked);
}

/// `pam_unix`, then `pam_twoprompt.so` asking a second hidden prompt (a one-time code): the
/// conversation answers the first with the password and refuses the second; the stack fails,
/// which may count toward pam_faillock, so the back-off counts it.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn a_second_hidden_prompt_is_not_checked_and_counts() {
    let password = walker();
    let backend = backend();
    let check = backend.check(&password, now());
    assert_eq!(
        check.outcome,
        CheckOutcome::NotChecked(NotCheckedWhy::OtherPrompt),
        "{check:?}"
    );
    assert!(recent_failure(backend.backoff(now())));
}

/// `pam_unix`, then `pam_twoprompt.so echo` asking a prompt that echoes.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn an_echoing_prompt_is_not_checked() {
    let password = walker();
    let check = backend().check(&password, now());
    assert_eq!(
        check.outcome,
        CheckOutcome::NotChecked(NotCheckedWhy::OtherPrompt),
        "{check:?}"
    );
}

/// Over a `pam_permit` stack, which answers any call with success: an empty entry and one
/// holding a NUL come back refused, so PAM was never asked, and nothing is counted.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn an_empty_entry_never_reaches_pam() {
    walker();
    let backend = backend();
    let empty = backend.check(&text(""), now());
    assert_eq!(empty.outcome, CheckOutcome::Refused(InputRefused::Empty));
    let nul = backend.check(&text("a\0b"), now());
    assert_eq!(
        nul.outcome,
        CheckOutcome::Refused(InputRefused::ContainsNul)
    );
    assert_eq!(backend.backoff(now()), Ok(BackoffState::Clear));
}

/// `passwd -d walker` under the distributions' `nullok`: `passwd -S` reads it empty, and no text
/// verifies the account, since PAM_DISALLOW_NULL_AUTHTOK overrides `nullok`.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn an_account_without_a_password_reads_empty_and_is_never_verified() {
    walker();
    let backend = backend();
    assert_eq!(backend.password_status().state, PasswordState::Empty);
    assert_eq!(
        backend.check(&text("anything"), now()).outcome,
        CheckOutcome::Wrong
    );
}

/// `passwd -l walker`: read locked, and the right password does not verify.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn a_locked_password_reads_locked_and_is_wrong() {
    let password = walker();
    let backend = backend();
    assert_eq!(backend.password_status().state, PasswordState::Locked);
    assert_eq!(backend.check(&password, now()).outcome, CheckOutcome::Wrong);
}

/// libpam moved out of the library path: nothing to load, every path tried named, nothing
/// counted.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn without_libpam_there_is_no_check() {
    let password = walker();
    let backend = backend();
    let Err(Unavailable::NoLibpam { tried }) = backend.backend() else {
        panic!("{:?}", backend.backend());
    };
    assert_eq!(
        tried.last().map(String::as_str),
        Some("libpam.so.0"),
        "{tried:?}"
    );
    let check = backend.check(&password, now());
    assert_eq!(
        check.outcome,
        CheckOutcome::Unavailable(Unavailable::NoLibpam { tried })
    );
    assert_eq!(backend.backoff(now()), Ok(BackoffState::Clear));
}

#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn without_a_pam_service_there_is_no_check() {
    let password = walker();
    let backend = backend();
    assert_eq!(backend.backend(), Err(Unavailable::NoService));
    assert_eq!(
        backend.check(&password, now()).outcome,
        CheckOutcome::Unavailable(Unavailable::NoService)
    );
}

/// No `/run/user/<uid>`, as in a container or under `su` without a logind session, over a
/// `pam_permit` stack: no record, no check — PAM would have answered `NotChecked`.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn without_a_runtime_directory_there_is_no_back_off_record() {
    let password = walker();
    let uid = current_account().unwrap().uid;
    let no_place = Unavailable::NoBackoffRecord {
        dir: PathBuf::from(format!("/run/user/{uid}")),
    };
    let backend = backend();
    assert_eq!(backend.backoff(now()), Err(no_place.clone()));
    assert_eq!(
        backend.check(&password, now()).outcome,
        CheckOutcome::Unavailable(no_place)
    );
}

#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn an_unmarked_account_is_fenced() {
    walker();
    let refused = fence::admit(Some(&roots())).unwrap_err();
    assert_eq!(
        refused.why,
        FencedWhy::NoMarker {
            home: PathBuf::from("/home/walker")
        }
    );
}

/// A marker root owns (only root can make one another account owns: the unit tests cannot).
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn a_marker_another_account_owns_is_no_marker() {
    walker();
    let refused = fence::admit(Some(&roots())).unwrap_err();
    assert_eq!(
        refused.why,
        FencedWhy::NoMarker {
            home: PathBuf::from("/home/walker")
        }
    );
}

/// `pam_faillock` deny=3 with walker's tally file made by a login (`su`): the non-root failure
/// is recorded (the script checks the tally holds exactly one), it reads wrong — Debian's
/// `[default=die]` answers PAM_PERM_DENIED, Fedora's PAM_AUTH_ERR — and the back-off refuses the
/// next check before PAM, so a run never adds a second.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn a_failure_counts_toward_faillock_once_its_tally_exists() {
    let password = walker();
    let check = backend().check(&text(&format!("not-{}", password.as_str())), now());
    assert_eq!(check.outcome, CheckOutcome::Wrong, "{check:?}");
    let refused = backend().check(&password, now());
    assert!(
        matches!(refused.outcome, CheckOutcome::Backoff(_)),
        "{refused:?}"
    );
}

/// The same stack with no tally file: the failure reads wrong and is not recorded (the script
/// checks the tally is empty).
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn without_a_tally_file_faillock_records_no_failure() {
    let password = walker();
    let check = backend().check(&text(&format!("not-{}", password.as_str())), now());
    assert_eq!(check.outcome, CheckOutcome::Wrong, "{check:?}");
}

#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn the_machine_id_is_its_32_characters() {
    walker();
    let id = SystemHost::new(false).machine_id().unwrap();
    assert_eq!(id.expose(), b"0123456789abcdef0123456789abcdef");
}

#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn an_empty_machine_id_is_none() {
    walker();
    assert_eq!(
        SystemHost::new(false).machine_id().err(),
        Some(NoMachineId::Empty)
    );
}

#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn a_missing_machine_id_is_none() {
    walker();
    assert_eq!(
        SystemHost::new(false).machine_id().err(),
        Some(NoMachineId::Missing)
    );
}

/// podman writes `/run/.containerenv` in every container it runs.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-os-slots-linux.sh"]
fn this_container_is_seen_as_one() {
    walker();
    let seen = SystemHost::new(false).environment();
    assert_eq!(
        seen,
        Environment {
            container: true,
            wsl: false
        }
    );
    assert!(seen.machine_id_may_change());
}
