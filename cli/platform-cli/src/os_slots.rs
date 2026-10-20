// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The CLI's glue to `apprafter-os-slots` (D.4b B9, B15, B16, B30, B55, B82, B88; ADR 0069
//! §9): what `lock::plan_on`, the prompt and the lock report read about this computer, and the
//! one check of the account password `lock on` makes. The core never calls the OS (rule 6):
//! this file answers its `SlotFacts`, `WayFacts` and `AccountPasswordCheck` over the OS crate.
//!
//! One backend per process ([`process`]):
//! - by default the real one, made only by `fence::admit` with `APPRAFTER_SANDBOX`'s roots
//!   ([`admit_real`]). Where the fence refuses it — under `APPRAFTER_SANDBOX`, for any account
//!   but a test account whose passwd home lies inside the sandbox and holds the marker — there
//!   is none: every fact is unknown and nothing is asked, so `lock on` takes a lock password
//!   before the OS is asked anything (B82, B88 (1)) and the prompt tries no account-password
//!   slot (B30);
//! - in a build with the dev-only feature `test-os-slots`, the scripted stand-in when
//!   `APPRAFTER_TEST_OS_SLOTS` names its script (B15, B16). A release build has neither the
//!   variable nor the stand-in (release-cli.yml's G-OS3);
//! - in this crate's own unit tests, none: each test passes the `OsSlots` it means (G-OS1).
//!
//! Each fact is one call to the backend, so the core keeps B88's order and asks nothing past
//! the fact that decided. This file alone builds an account-password slot
//! (`NewSlot::OsPassword`, G-OS6), and only after the system's check said `Verified`.

use std::fmt;
#[cfg(feature = "test-os-slots")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use apprafter_core::lock::{
    self, AccountChecked, AccountPasswordCheck, BackoffFact, CheckBackend, HostOs, MachineFact,
    PasswordFact, SlotFacts, WayFacts,
};
use apprafter_core::vault::{MachineId, NewSlot};
use apprafter_core::{EnvSource, SecretString, Zeroizing};
use apprafter_os_slots::{
    fence, AccountPassword, BackoffState, BackoffWait, CheckOutcome, HostFacts, InputRefused,
    NoMachineId, Os, PasswordState, PasswordStatus, SystemHost, Unavailable,
};
use cli_core::{AccountCheckUnavailable, AccountPasswordRefusal, AccountPasswordUnusable};

/// The variable that names the stand-in's script (dev-only feature `test-os-slots`, B16).
#[cfg(feature = "test-os-slots")]
const TEST_OS_SLOTS_ENV: &str = "APPRAFTER_TEST_OS_SLOTS";
/// Set inside WSL 2 (B76: a machine id that may change when the distribution is re-made).
const WSL_INTEROP_ENV: &str = "WSL_INTEROP";

/// This process's account-password backend and this computer's facts (module docs).
pub(crate) struct OsSlots {
    backend: Backend,
}

enum Backend {
    /// None: the fence refused the real one (B16, B82), or this crate's unit tests made none.
    /// Nothing is asked; every fact is unknown.
    Absent,
    /// The real backend through the fence, or the stand-in.
    Ready {
        account: Arc<dyn AccountPassword>,
        host: Arc<dyn HostFacts>,
    },
}

/// The process's backend (module docs), made once.
pub(crate) fn process() -> Arc<OsSlots> {
    static SLOTS: OnceLock<Arc<OsSlots>> = OnceLock::new();
    SLOTS
        .get_or_init(|| {
            if cfg!(test) {
                // G-OS1 (F8): no unit test of this crate reaches a backend of the process's own.
                return Arc::new(OsSlots::absent());
            }
            Arc::new(OsSlots::from_env(&crate::context::ProcessEnv))
        })
        .clone()
}

impl OsSlots {
    /// No backend (module docs).
    pub(crate) fn absent() -> OsSlots {
        OsSlots {
            backend: Backend::Absent,
        }
    }

    /// Over `account` and `host`: the real backend in [`admit_real`], the stand-in otherwise.
    pub(crate) fn over(account: Arc<dyn AccountPassword>, host: Arc<dyn HostFacts>) -> OsSlots {
        OsSlots {
            backend: Backend::Ready { account, host },
        }
    }

    /// The backend `env` asks for (module docs).
    pub(crate) fn from_env(env: &dyn EnvSource) -> OsSlots {
        #[cfg(feature = "test-os-slots")]
        if let Some(script) = env.var_os(TEST_OS_SLOTS_ENV) {
            return OsSlots::scripted(Path::new(&script));
        }
        admit_real(
            crate::sandbox::fence_roots(env).as_deref(),
            env.var_os(WSL_INTEROP_ENV).is_some(),
        )
    }

    /// The stand-in that reads `script` (B15), every call appended to `<script>.calls`. A
    /// script it cannot read ends the process: only a test build reaches here.
    #[cfg(feature = "test-os-slots")]
    fn scripted(script: &Path) -> OsSlots {
        let stand = apprafter_os_slots::stand_in::ScriptedAccountPassword::from_file(script)
            .unwrap_or_else(|e| panic!("{e}"));
        let stand = Arc::new(stand);
        OsSlots::over(stand.clone(), stand)
    }

    /// The account a check is for, as the account-password prompt names it. Critic round,
    /// finding 3: read outside tests from Task 4 on (`commands::lock::new_input`), which
    /// removes the attribute — left behind, the expectation itself is a warning.
    pub(crate) fn account_name(&self) -> Option<String> {
        match &self.backend {
            Backend::Absent => None,
            Backend::Ready { account, .. } => account.account_name(),
        }
    }
}

/// The real backend through the fence (B16, B82), or none where the fence refuses it. G-OS1's
/// `ADMIT_WRAPPERS` names this function: no test calls it.
fn admit_real(sandbox_roots: Option<&[PathBuf]>, wsl_interop: bool) -> OsSlots {
    match fence::admit(sandbox_roots) {
        Ok(admitted) => OsSlots::over(Arc::new(admitted), Arc::new(SystemHost::new(wsl_interop))),
        Err(_fenced) => OsSlots::absent(),
    }
}

impl fmt::Debug for OsSlots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let backend = match self.backend {
            Backend::Absent => "absent",
            Backend::Ready { .. } => "ready",
        };
        f.debug_struct("OsSlots")
            .field("backend", &backend)
            .finish()
    }
}

impl SlotFacts for OsSlots {
    fn os(&self) -> HostOs {
        match &self.backend {
            Backend::Absent => HostOs::this(),
            Backend::Ready { account, .. } => host_os(account.os()),
        }
    }

    fn machine_id(&self) -> MachineFact {
        match &self.backend {
            Backend::Absent => MachineFact::Unknown,
            Backend::Ready { host, .. } => machine_fact(host.machine_id()),
        }
    }

    fn machine_id_may_change(&self) -> Option<bool> {
        match &self.backend {
            Backend::Absent => None,
            Backend::Ready { host, .. } => Some(host.environment().machine_id_may_change()),
        }
    }
}

impl WayFacts for OsSlots {
    fn fenced(&self) -> bool {
        matches!(self.backend, Backend::Absent)
    }

    fn account_check(&self) -> CheckBackend {
        match &self.backend {
            Backend::Absent => CheckBackend::NoAccount,
            Backend::Ready { account, .. } => check_backend(account.backend()),
        }
    }

    fn backoff(&self) -> BackoffFact {
        match &self.backend {
            Backend::Absent => BackoffFact::NoRecord,
            Backend::Ready { account, .. } => {
                let now = SystemTime::now();
                backoff_fact(account.backoff(now), now)
            }
        }
    }

    fn password_status(&self) -> PasswordFact {
        match &self.backend {
            Backend::Absent => PasswordFact::Unknown,
            Backend::Ready { account, .. } => password_fact(account.password_status()),
        }
    }
}

impl AccountPasswordCheck for OsSlots {
    fn check(&self, typed: &SecretString, machine: &MachineId) -> AccountChecked {
        let Backend::Ready { account, host } = &self.backend else {
            // Never asked: without a backend `lock::plan_on` takes a lock password (B88 (1)).
            return AccountChecked::Refused {
                reason: AccountPasswordRefusal::Unavailable {
                    what: AccountCheckUnavailable::NoAccount,
                },
                messages: Vec::new(),
            };
        };
        let now = SystemTime::now();
        let checked = account.check(&Zeroizing::new(typed.expose().to_owned()), now);
        match refusal(checked.outcome, now) {
            None => AccountChecked::Verified(NewSlot::OsPassword {
                secret: typed.clone(),
                label: lock::account_password_label(
                    &account
                        .account_name()
                        .unwrap_or_else(|| "this account".to_owned()),
                    &host
                        .host_name()
                        .unwrap_or_else(|| "this computer".to_owned()),
                ),
                enrolled_at: unix_seconds(now),
                machine: machine.clone(),
            }),
            Some(reason) => AccountChecked::Refused {
                reason,
                messages: checked.messages,
            },
        }
    }
}

fn host_os(os: Os) -> HostOs {
    match os {
        Os::Linux => HostOs::Linux,
        Os::Macos => HostOs::Macos,
        Os::Windows => HostOs::Windows,
    }
}

/// This computer's machine id as the vault binds a slot to it (B54, B76): none where it cannot
/// be read, or where the vault would refuse it.
fn machine_fact(read: Result<apprafter_os_slots::MachineId, NoMachineId>) -> MachineFact {
    match read.map(|id| MachineId::new(id.expose())) {
        Ok(Ok(id)) => MachineFact::Id(id),
        Ok(Err(_)) | Err(_) => MachineFact::NoId,
    }
}

/// Whether a check could run here (B88 (3)). `NotOnThisOs` and `NoBackoffRecord` are not
/// `backend`'s answers (the OS is the plan's (2), the record `backoff`'s (6)); read as no check
/// route, they fail closed.
fn check_backend(answer: Result<(), Unavailable>) -> CheckBackend {
    match answer {
        Ok(()) => CheckBackend::Ready,
        Err(Unavailable::NoLibpam { .. }) => CheckBackend::NoLibpam,
        Err(Unavailable::NoService) => CheckBackend::NoService,
        Err(Unavailable::NoAccount) => CheckBackend::NoAccount,
        Err(
            Unavailable::NoCheckRoute
            | Unavailable::NotOnThisOs
            | Unavailable::NoBackoffRecord { .. },
        ) => CheckBackend::NoCheckRoute,
    }
}

/// A back-off's wait in whole seconds from `now`; `None` while another process checks (B12).
fn wait_secs(wait: BackoffWait, now: SystemTime) -> Option<u64> {
    wait.until.map(|until| seconds_until(now, until))
}

/// The back-off as the plan weighs it (B74, B88 (6)): a missing record — or any answer without
/// one — is `NoRecord`, never a check without it.
fn backoff_fact(state: Result<BackoffState, Unavailable>, now: SystemTime) -> BackoffFact {
    match state {
        Ok(BackoffState::Clear) => BackoffFact::Clear,
        Ok(BackoffState::Wait(wait)) => BackoffFact::Wait {
            wait_secs: wait_secs(wait, now),
        },
        Err(_) => BackoffFact::NoRecord,
    }
}

/// This account's password status as the plan weighs it (B75): a locked or an empty password
/// is none.
fn password_fact(status: PasswordStatus) -> PasswordFact {
    match status.state {
        PasswordState::Usable => PasswordFact::Usable,
        PasswordState::Locked | PasswordState::Empty => PasswordFact::NoPassword,
        PasswordState::Unknown => PasswordFact::Unknown,
    }
}

/// Why the system did not take the password, in c1's terms — `None` when it did (`Verified`).
fn refusal(outcome: CheckOutcome, now: SystemTime) -> Option<AccountPasswordRefusal> {
    let unavailable = |what| AccountPasswordRefusal::Unavailable { what };
    Some(match outcome {
        CheckOutcome::Verified => return None,
        CheckOutcome::Wrong => AccountPasswordRefusal::Wrong,
        CheckOutcome::NotChecked(_) => AccountPasswordRefusal::NotChecked,
        CheckOutcome::SystemError { name, .. } => {
            unavailable(AccountCheckUnavailable::SystemError {
                name: name.to_owned(),
            })
        }
        CheckOutcome::Refused(InputRefused::Empty) => AccountPasswordRefusal::NoPassword,
        CheckOutcome::Refused(InputRefused::ContainsNul) => AccountPasswordRefusal::Unusable {
            why: AccountPasswordUnusable::ContainsNul,
        },
        CheckOutcome::Backoff(wait) => AccountPasswordRefusal::Backoff {
            wait_secs: wait_secs(wait, now),
        },
        CheckOutcome::Unavailable(why) => unavailable(match why {
            Unavailable::NoLibpam { .. } => AccountCheckUnavailable::NoLibpam,
            Unavailable::NoService => AccountCheckUnavailable::NoService,
            Unavailable::NoAccount => AccountCheckUnavailable::NoAccount,
            Unavailable::NoBackoffRecord { .. } => AccountCheckUnavailable::NoBackoffRecord,
            // The plan stops at (2) on Windows; read as no check route, it fails closed.
            Unavailable::NoCheckRoute | Unavailable::NotOnThisOs => {
                AccountCheckUnavailable::NoCheckRoute
            }
        }),
    })
}

/// Whole seconds from `now` until `until`, rounded up; 0 once it has passed.
fn seconds_until(now: SystemTime, until: SystemTime) -> u64 {
    let Ok(left) = until.duration_since(now) else {
        return 0;
    };
    left.as_secs() + u64::from(left.subsec_nanos() > 0)
}

/// `now` as Unix seconds (an account-password slot's `enrolled_at`, B2).
fn unix_seconds(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The stand-in playing `script` (B15) over this crate's tests, and the stand-in itself, whose
/// calls a test reads.
#[cfg(test)]
pub(crate) fn scripted(
    script: &str,
) -> (
    OsSlots,
    Arc<apprafter_os_slots::stand_in::ScriptedAccountPassword>,
) {
    use apprafter_os_slots::stand_in::{Script, ScriptedAccountPassword};
    let script = Script::parse(script).expect("a script the stand-in reads");
    let stand = Arc::new(ScriptedAccountPassword::new(script));
    (OsSlots::over(stand.clone(), stand.clone()), stand)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use apprafter_os_slots::stand_in::{Call, SCRIPTED_MACHINE_ID};

    use super::*;

    /// This computer's machine id in these tests: the stand-in's, as the vault takes it.
    fn here() -> MachineId {
        MachineId::new(SCRIPTED_MACHINE_ID.as_bytes()).unwrap()
    }

    /// B30, B88: each fact is what the backend answers, in the core's terms.
    #[test]
    fn each_fact_reads_as_the_backend_answers_it() {
        let facts = |script: &str| scripted(script).0;
        for (script, backend) in [
            ("", CheckBackend::Ready),
            ("backend no-libpam\n", CheckBackend::NoLibpam),
            ("backend no-service\n", CheckBackend::NoService),
            (
                "os macos\nbackend no-check-route\n",
                CheckBackend::NoCheckRoute,
            ),
            ("account none\n", CheckBackend::NoAccount),
        ] {
            assert_eq!(facts(script).account_check(), backend, "{script}");
        }
        for (script, backoff) in [
            ("", BackoffFact::Clear),
            (
                "backoff wait 540 recent-failure\n",
                BackoffFact::Wait {
                    wait_secs: Some(540),
                },
            ),
            (
                "backoff wait 9 another-check-running\n",
                BackoffFact::Wait { wait_secs: None },
            ),
            ("backoff no-record\n", BackoffFact::NoRecord),
        ] {
            assert_eq!(facts(script).backoff(), backoff, "{script}");
        }
        for (script, status) in [
            ("status usable\n", PasswordFact::Usable),
            ("status locked\n", PasswordFact::NoPassword),
            ("status empty\n", PasswordFact::NoPassword),
            ("status unknown\n", PasswordFact::Unknown),
        ] {
            assert_eq!(facts(script).password_status(), status, "{script}");
        }
        assert!(matches!(facts("").machine_id(), MachineFact::Id(_)));
        assert!(matches!(
            facts("machine-id missing\n").machine_id(),
            MachineFact::NoId
        ));
        assert!(matches!(
            facts("os windows\n").machine_id(),
            MachineFact::NoId
        ));
        assert_eq!(facts("os macos\n").os(), HostOs::Macos);
        assert_eq!(facts("").os(), HostOs::Linux);
        assert_eq!(
            facts("environment container\n").machine_id_may_change(),
            Some(true)
        );
        assert_eq!(facts("").machine_id_may_change(), Some(false));
        assert!(!facts("").fenced());
        assert_eq!(
            facts("account walker\n").account_name().as_deref(),
            Some("walker")
        );
    }

    /// B16, B82: without a backend — the fence refused it, or a unit test made none — nothing
    /// is asked and nothing is known: the plan stops at (1), and every other answer fails
    /// closed.
    #[test]
    fn without_a_backend_nothing_is_asked_and_nothing_known() {
        let none = OsSlots::absent();
        assert!(none.fenced());
        assert!(matches!(none.machine_id(), MachineFact::Unknown));
        assert_eq!(none.machine_id_may_change(), None);
        assert_eq!(none.os(), HostOs::this());
        assert_eq!(none.account_name(), None);
        assert_eq!(none.account_check(), CheckBackend::NoAccount);
        assert_eq!(none.backoff(), BackoffFact::NoRecord);
        assert_eq!(none.password_status(), PasswordFact::Unknown);
        match none.check(&SecretString::new("x"), &here()) {
            AccountChecked::Refused {
                reason:
                    AccountPasswordRefusal::Unavailable {
                        what: AccountCheckUnavailable::NoAccount,
                    },
                ..
            } => {}
            _ => panic!("checked without a backend"),
        }
        assert!(
            matches!(process().machine_id(), MachineFact::Unknown),
            "a unit test's process has no backend"
        );
    }

    /// B10, B56, B77: a check the system took builds this computer's slot — the text typed,
    /// labelled with the account and the host (or "this computer" where the OS names none),
    /// enrolled now — after one call to the system's check.
    #[test]
    fn a_verified_check_builds_the_slot_of_the_text_typed() {
        let typed = "walker-account-password-1";
        let (slots, stand) = scripted(&format!("host box-a\ncheck verified-if {typed}\n"));
        // Critic round, finding 7: `enrolled_at` lies between two readings of the clock taken
        // around the check, whatever the clock says (GOTCHA-332).
        let before = unix_seconds(SystemTime::now());
        let checked = slots.check(&SecretString::new(typed), &here());
        let after = unix_seconds(SystemTime::now());
        match checked {
            AccountChecked::Verified(NewSlot::OsPassword {
                secret,
                label,
                enrolled_at,
                ..
            }) => {
                assert_eq!(secret.expose(), typed);
                assert_eq!(label, "account password of walker on box-a");
                assert!(
                    (before..=after).contains(&enrolled_at),
                    "{before} <= {enrolled_at} <= {after}"
                );
            }
            _ => panic!("not verified as an account-password slot"),
        }
        assert_eq!(stand.calls(), [Call::Check { chars: 25 }]);
        let (nameless, _) = scripted(&format!("host none\ncheck verified-if {typed}\n"));
        match nameless.check(&SecretString::new(typed), &here()) {
            AccountChecked::Verified(NewSlot::OsPassword { label, .. }) => {
                assert_eq!(label, "account password of walker on this computer");
            }
            _ => panic!("not verified"),
        }
    }

    /// B11, B12, B74, B75: every refused check names its reason in c1's terms, and an entry
    /// the system's check takes no answer for is refused as such.
    #[test]
    fn every_refused_check_names_its_reason() {
        let unavailable = |what| AccountPasswordRefusal::Unavailable { what };
        for (script, typed, reason) in [
            ("check wrong\n", "x", AccountPasswordRefusal::Wrong),
            (
                "check not-checked no-password-asked\n",
                "x",
                AccountPasswordRefusal::NotChecked,
            ),
            (
                "check not-checked other-prompt\n",
                "x",
                AccountPasswordRefusal::NotChecked,
            ),
            (
                "check system-error 4\n",
                "x",
                unavailable(AccountCheckUnavailable::SystemError {
                    name: "PAM_SCRIPTED".into(),
                }),
            ),
            (
                "backoff wait 600 recent-failure\ncheck wrong\n",
                "x",
                AccountPasswordRefusal::Backoff {
                    wait_secs: Some(600),
                },
            ),
            (
                "backoff wait 600 another-check-running\ncheck wrong\n",
                "x",
                AccountPasswordRefusal::Backoff { wait_secs: None },
            ),
            (
                "backoff no-record\ncheck wrong\n",
                "x",
                unavailable(AccountCheckUnavailable::NoBackoffRecord),
            ),
            (
                "backend no-libpam\ncheck wrong\n",
                "x",
                unavailable(AccountCheckUnavailable::NoLibpam),
            ),
            ("check wrong\n", "", AccountPasswordRefusal::NoPassword),
            (
                "check wrong\n",
                "a\0b",
                AccountPasswordRefusal::Unusable {
                    why: AccountPasswordUnusable::ContainsNul,
                },
            ),
        ] {
            let (slots, _) = scripted(script);
            match slots.check(&SecretString::new(typed), &here()) {
                AccountChecked::Refused {
                    reason: said,
                    messages,
                } => {
                    assert_eq!(said, reason, "{script:?} {typed:?}");
                    assert!(messages.is_empty(), "{messages:?}");
                }
                AccountChecked::Verified(_) => panic!("{script:?}: verified"),
            }
        }
    }

    /// B12: a wait is whole seconds from now, rounded up, and none once it has passed.
    #[test]
    fn a_wait_is_whole_seconds_rounded_up() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert_eq!(seconds_until(now, now + Duration::from_secs(540)), 540);
        assert_eq!(seconds_until(now, now + Duration::from_millis(1_500)), 2);
        assert_eq!(seconds_until(now + Duration::from_secs(5), now), 0);
    }

    /// B15, B16: with the dev-only feature, APPRAFTER_TEST_OS_SLOTS names the stand-in's
    /// script, which it reads — its calls logged beside it — and the fence does not apply to
    /// a stand-in; the sandbox root set here keeps a real backend fenced all the same.
    #[cfg(feature = "test-os-slots")]
    #[test]
    fn the_stand_in_is_the_script_the_variable_names() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("os-slots.script");
        std::fs::write(&script, "os macos\naccount walker\n").unwrap();
        let env = apprafter_core::MapEnv::new()
            .with("APPRAFTER_TEST_OS_SLOTS", script.to_str().unwrap())
            .with("APPRAFTER_SANDBOX", dir.path().to_str().unwrap());
        let slots = OsSlots::from_env(&env);
        assert!(!slots.fenced());
        assert_eq!(slots.os(), HostOs::Macos);
        assert!(dir.path().join("os-slots.script.calls").exists());
    }
}
