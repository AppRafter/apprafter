// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The PAM fallback against a real PAM stack, and the Linux authenticator's move to it, inside
//! the container `scripts/test-osauth-linux.sh` builds: Debian's libpam, `pam_unix` and the
//! set-group-id `unix_chkpwd`, its `/etc/pam.d/common-auth` (`nullok`), a user `walk` with a
//! known password, and polkitd with the app's policy and the driver's logind files for the
//! routing cases. The script prepares the system for each case (the service files present or
//! not, `walk`'s password removed, the policy file present or not, a `rules.d` rule, the session
//! active, inactive or not joined) and runs exactly that test as `walk`, through the
//! container's own loader, so the libpam and the modules it loads are the container's.
//!
//! ```text
//! bash scripts/test-osauth-linux.sh          # every case, each in a fresh container
//! ```
//!
//! They run nowhere else: they need a PAM stack that knows `walk`'s password, so outside the
//! container they fail instead of passing quietly.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::time::{Duration, Instant};

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthInfo, AuthMethod, AuthOutcome, UnavailableReason};
use apprafter_os_auth::linux::pam::{current_user, find_service, Pam, SERVICES, SERVICE_DIRS};
use apprafter_os_auth::linux::polkit::Action;
use apprafter_os_auth::linux::session::active_local_session;
use apprafter_os_auth::outcome::Backoff;
use apprafter_os_auth::OsAuthenticator;
use zeroize::Zeroizing;

/// A check the back-off refuses touches no PAM; PAM's own delay after a failure is seconds.
const NO_PAM_CALL: Duration = Duration::from_millis(500);

/// The container's password for `walk`, after checking that this is the container and that
/// the test runs as `walk`: as root, `pam_unix` would read `/etc/shadow` itself and the
/// set-group-id helper the app depends on would go untested.
fn container() -> String {
    let var = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| {
            panic!("{name} is unset: run these through scripts/test-osauth-linux.sh")
        })
    };
    assert_eq!(
        var("APPRAFTER_OSAUTH_CONTAINER"),
        "1",
        "run these through scripts/test-osauth-linux.sh: they check a password with the \
         system's PAM"
    );
    assert_eq!(
        current_user().as_deref(),
        Some("walk".as_ref()),
        "the cases run as walk"
    );
    // SAFETY: getuid cannot fail.
    assert_ne!(unsafe { libc::getuid() }, 0, "the cases do not run as root");
    var("APPRAFTER_OSAUTH_PASSWORD")
}

fn password(text: &str) -> Zeroizing<String> {
    Zeroizing::new(text.to_owned())
}

fn check(pam: &Pam, text: &str, now: u64) -> AuthOutcome {
    pam.verify_password(password(text), &CancellationToken::new(), now)
        .outcome
}

const fn unavailable(reason: UnavailableReason) -> AuthOutcome {
    AuthOutcome::Unavailable { reason }
}

fn available(method: AuthMethod, password_field: bool) -> AuthInfo {
    AuthInfo {
        available: true,
        method: Some(method),
        unavailable: None,
        biometrics_choice: false,
        password_field,
    }
}

/// `walk`'s password through Debian's `common-auth`: `pam_unix`, which cannot read
/// `/etc/shadow` as `walk`, asks `unix_chkpwd`.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-osauth-linux.sh"]
fn the_right_password_is_verified() {
    let right = container();
    assert_eq!(find_service(Path::new("/")), Some("common-auth"));
    let pam = Pam::new();
    assert_eq!(pam.available(), Ok(()));
    let outcome = pam.verify_password(password(&right), &CancellationToken::new(), 0);
    assert_eq!(outcome.outcome, AuthOutcome::Verified, "{outcome:?}");
}

/// Three wrong passwords fail, the third saying no attempt is left, and for how long; then the
/// right one is refused at once, so PAM was not asked (it would have verified it), saying how
/// much of the refusal is left; once the refusal is over PAM is asked again and verifies it.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-osauth-linux.sh"]
fn wrong_passwords_fail_and_the_back_off_refuses_without_asking_pam() {
    let right = container();
    let wrong = format!("not-{right}");
    let pam = Pam::new();
    // Nothing printed: the script reads the test's own `... ok` line, which output would split.
    for (now, exhausted) in [(0, false), (1, false), (2, true)] {
        assert_eq!(
            check(&pam, &wrong, now),
            AuthOutcome::Failed {
                exhausted,
                retry_in_ms: exhausted.then_some(Backoff::REFUSAL_MS),
            },
            "wrong password #{}",
            now + 1
        );
    }
    let started = Instant::now();
    assert_eq!(
        check(&pam, &right, 3),
        AuthOutcome::Failed {
            exhausted: true,
            retry_in_ms: Some(Backoff::REFUSAL_MS - 1),
        },
        "the refusal answers before PAM, even for the right password"
    );
    let refused_in = started.elapsed();
    assert!(
        refused_in < NO_PAM_CALL,
        "the refusal took {refused_in:?}: PAM was asked"
    );
    assert_eq!(
        check(&pam, &right, 2 + Backoff::REFUSAL_MS - 1),
        AuthOutcome::Failed {
            exhausted: true,
            retry_in_ms: Some(1),
        },
        "a millisecond before the refusal ends"
    );
    assert_eq!(
        check(&pam, &right, 2 + Backoff::REFUSAL_MS),
        AuthOutcome::Verified,
        "the refusal is over"
    );
}

/// The script removed every service the probe looks for from both directories.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-osauth-linux.sh"]
fn without_a_service_file_there_is_no_pam_service() {
    let right = container();
    for dir in SERVICE_DIRS {
        for service in SERVICES {
            let path = Path::new("/").join(dir).join(service);
            assert!(!path.exists(), "{} is still there", path.display());
        }
    }
    let pam = Pam::new();
    assert_eq!(pam.available(), Err(UnavailableReason::NoPamService));
    assert_eq!(
        check(&pam, &right, 0),
        unavailable(UnavailableReason::NoPamService)
    );
    assert_eq!(
        OsAuthenticator::new().info(),
        available(AuthMethod::Polkit, false),
        "polkit itself can still prompt here"
    );
}

/// polkit can prompt as far as its probe tells, so the password is refused, pointing at the
/// system's prompt (`UseSystemPrompt`, not a final refusal); the first dialog finds no agent,
/// and from then on the password field stands in and PAM verifies.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-osauth-linux.sh"]
fn without_an_agent_the_authenticator_moves_to_the_password() {
    let right = container();
    let auth = OsAuthenticator::new();
    assert_eq!(auth.info(), available(AuthMethod::Polkit, false));
    let refused = auth.verify_password(
        Action::Unlock,
        password(&right),
        &CancellationToken::new(),
        0,
    );
    assert_eq!(
        refused.outcome,
        unavailable(UnavailableReason::UseSystemPrompt)
    );
    assert_eq!(
        auth.verify(Action::Unlock, &CancellationToken::new()),
        unavailable(UnavailableReason::NoAgent)
    );
    assert_eq!(auth.info(), available(AuthMethod::Pam, true));
    let checked = auth.verify_password(
        Action::Unlock,
        password(&right),
        &CancellationToken::new(),
        0,
    );
    assert_eq!(checked.outcome, AuthOutcome::Verified, "{checked:?}");
}

/// The script removed the policy file: polkit's probe says so, and the password field is the
/// method from the start.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-osauth-linux.sh"]
fn without_the_policy_file_the_authenticator_offers_the_password() {
    let right = container();
    let auth = OsAuthenticator::new();
    assert_eq!(auth.info(), available(AuthMethod::Pam, true));
    assert_eq!(
        auth.verify(Action::Confirm, &CancellationToken::new()),
        unavailable(UnavailableReason::PolicyMissing)
    );
    for (text, outcome) in [
        (
            format!("not-{right}"),
            AuthOutcome::Failed {
                exhausted: false,
                retry_in_ms: None,
            },
        ),
        (right, AuthOutcome::Verified),
    ] {
        let checked = auth.verify_password(
            Action::Confirm,
            password(&text),
            &CancellationToken::new(),
            0,
        );
        assert_eq!(checked.outcome, outcome);
    }
}

/// The script removed `walk`'s password (`passwd -d`). Debian's `common-auth` says `nullok`,
/// under which `pam_unix` verifies such an account without a prompt, whatever was typed, unless
/// the caller passes `PAM_DISALLOW_NULL_AUTHTOK`. Neither probe verifies: the empty password
/// never reaches PAM, and the flag refuses the other.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-osauth-linux.sh"]
fn an_empty_password_is_never_verified() {
    container();
    // A Pam per probe, so that the back-off's count stays out of the outcomes.
    let outcomes: Vec<_> = ["", "anything at all"]
        .into_iter()
        .map(|text| (text, check(&Pam::new(), text, 0)))
        .collect();
    assert_eq!(
        outcomes,
        [
            (
                "",
                AuthOutcome::Failed {
                    exhausted: false,
                    retry_in_ms: None
                }
            ),
            (
                "anything at all",
                AuthOutcome::Failed {
                    exhausted: false,
                    retry_in_ms: None
                }
            ),
        ]
    );
}

/// The script installed a `rules.d` rule that answers NO for the app's actions, and the test
/// runs in the driver's active local session, where the policy's defaults ask: the refusal is
/// the administrator's, and the password field must not get round it.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-osauth-linux.sh"]
fn an_administrator_s_no_in_an_active_session_is_final() {
    let right = container();
    assert!(
        active_local_session(),
        "the case runs in an active local session"
    );
    let auth = OsAuthenticator::new();
    assert_eq!(
        auth.info(),
        AuthInfo {
            available: false,
            method: None,
            unavailable: Some(UnavailableReason::NotPermittedHere),
            biometrics_choice: false,
            password_field: false,
        }
    );
    assert_eq!(
        auth.verify(Action::Unlock, &CancellationToken::new()),
        unavailable(UnavailableReason::NotPermittedHere)
    );
    for action in [Action::Unlock, Action::Confirm] {
        let refused = auth.verify_password(action, password(&right), &CancellationToken::new(), 0);
        assert_eq!(
            refused.outcome,
            unavailable(UnavailableReason::NotPermittedHere),
            "{action:?}"
        );
    }
    assert_eq!(
        check(&Pam::new(), &right, 0),
        AuthOutcome::Verified,
        "PAM itself verifies it: the refusal is the authenticator's"
    );
}

/// Where polkit's own defaults refuse, the password field stands in for it.
fn the_password_stands_in_for_polkit_s_refusal(right: &str) {
    let auth = OsAuthenticator::new();
    assert_eq!(auth.info(), available(AuthMethod::Pam, true));
    assert_eq!(
        auth.verify(Action::Unlock, &CancellationToken::new()),
        unavailable(UnavailableReason::NotPermittedHere)
    );
    let checked = auth.verify_password(
        Action::Unlock,
        password(right),
        &CancellationToken::new(),
        0,
    );
    assert_eq!(checked.outcome, AuthOutcome::Verified, "{checked:?}");
}

/// The script ran this case outside the session: no session, so `allow_any` (`no`) applies.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-osauth-linux.sh"]
fn outside_a_session_the_authenticator_offers_the_password() {
    let right = container();
    assert!(!active_local_session(), "the case runs outside the session");
    the_password_stands_in_for_polkit_s_refusal(&right);
}

/// The script made the session, and so its user, inactive: `allow_inactive` (`no`) applies.
#[test]
#[ignore = "needs the PAM container: bash scripts/test-osauth-linux.sh"]
fn in_an_inactive_session_the_authenticator_offers_the_password() {
    let right = container();
    assert!(!active_local_session(), "the case's session is inactive");
    the_password_stands_in_for_polkit_s_refusal(&right);
}
