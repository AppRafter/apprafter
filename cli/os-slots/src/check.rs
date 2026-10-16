// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! One account-password check, end to end (D.4b B10–B13, B74): the entry refused before PAM
//! when it is empty or holds a NUL; the account, libpam and its service; the back-off record's
//! place; the attempt counted before PAM is asked; PAM; the attempt taken back when PAM
//! reported success. A [`Checker`] is what `fence::SystemAccountPassword` wraps; tests build
//! one over a scripted libpam and a scratch record directory, so they never name the real
//! backend (G-OS1).

#[cfg(all(test, unix))]
use std::path::PathBuf;
use std::time::SystemTime;

use zeroize::Zeroizing;

use crate::account::{self, Account};
use crate::backoff::{Begin, Record};
use crate::outcome::{
    BackoffState, CheckOutcome, InputRefused, Os, PasswordCheck, PasswordStatus, Unavailable,
};
use crate::pam::{self, Pam};

/// Where the back-off record lives: the system's place, or a directory a test gives.
/// `Dir` exists only in the Unix tests, its one user, so no build has a dead variant (critic
/// round, B1).
pub(crate) enum RecordAt {
    System,
    #[cfg(all(test, unix))]
    Dir(PathBuf),
}

/// The check behind every real account-password backend (module docs).
pub(crate) struct Checker {
    pub(crate) os: Os,
    pub(crate) account: Option<Account>,
    pub(crate) pam: Pam,
    pub(crate) record_at: RecordAt,
}

impl Checker {
    fn account(&self) -> Result<&Account, Unavailable> {
        self.account.as_ref().ok_or(Unavailable::NoAccount)
    }

    fn record(&self) -> Result<Record, Unavailable> {
        // Windows first, as `service`: the slot does not exist there, whatever the account.
        if self.os == Os::Windows {
            return Err(Unavailable::NotOnThisOs);
        }
        let account = self.account()?;
        match &self.record_at {
            RecordAt::System => Record::locate(account),
            #[cfg(all(test, unix))]
            RecordAt::Dir(dir) => Record::in_dir(dir, account.uid),
        }
    }

    /// The service a check would use: this OS has the slot, the account exists, libpam loads.
    fn service(&self) -> Result<&'static str, Unavailable> {
        if self.os == Os::Windows {
            return Err(Unavailable::NotOnThisOs);
        }
        self.account()?;
        self.pam.backend()
    }

    pub(crate) fn backend(&self) -> Result<(), Unavailable> {
        self.service().map(drop)
    }

    pub(crate) fn backoff(&self, now: SystemTime) -> Result<BackoffState, Unavailable> {
        Ok(self.record()?.state(now))
    }

    pub(crate) fn password_status(&self) -> PasswordStatus {
        self.account
            .as_ref()
            .map_or(PasswordStatus::UNKNOWN, account::password_status)
    }

    pub(crate) fn check(&self, password: &Zeroizing<String>, now: SystemTime) -> PasswordCheck {
        self.checked(password, now)
            .unwrap_or_else(|outcome| PasswordCheck {
                outcome,
                messages: Vec::new(),
            })
    }

    fn checked(
        &self,
        password: &Zeroizing<String>,
        now: SystemTime,
    ) -> Result<PasswordCheck, CheckOutcome> {
        if password.is_empty() {
            return Err(CheckOutcome::Refused(InputRefused::Empty));
        }
        if password.as_bytes().contains(&0) {
            return Err(CheckOutcome::Refused(InputRefused::ContainsNul));
        }
        let service = self.service().map_err(CheckOutcome::Unavailable)?;
        let account = self.account().map_err(CheckOutcome::Unavailable)?;
        let attempt = self
            .record()
            .map_err(CheckOutcome::Unavailable)?
            .begin(now)
            .map_err(|begin| match begin {
                Begin::Wait(wait) => CheckOutcome::Backoff(wait),
                Begin::Unavailable(why) => CheckOutcome::Unavailable(why),
            })?;
        let (answer, asked) = self
            .pam
            .authenticate(service, &account.name, password.as_bytes());
        let (outcome, counted) = pam::decide(&answer, &account.name, &asked, self.pam.codes());
        if !counted {
            attempt.not_counted();
        }
        Ok(PasswordCheck {
            outcome,
            messages: asked.messages,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::outcome::{BackoffWait, NotCheckedWhy, WaitWhy};
    use crate::pam::scripted::{ScriptedLibrary, Step};

    const RIGHT: &str = "correct horse battery staple";

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn secret(text: &str) -> Zeroizing<String> {
        Zeroizing::new(text.to_owned())
    }

    /// A checker over a scripted libpam with `common-auth`, for an account `walker` with this
    /// process's uid, its record in a scratch directory.
    struct Fixture {
        _services: tempfile::TempDir,
        record: tempfile::TempDir,
        library: Arc<ScriptedLibrary>,
        checker: Checker,
    }

    fn fixture(library: ScriptedLibrary) -> Fixture {
        let services = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(services.path().join("etc/pam.d")).unwrap();
        std::fs::write(
            services.path().join("etc/pam.d/common-auth"),
            "auth required pam_unix.so\n",
        )
        .unwrap();
        let record = tempfile::tempdir().unwrap();
        std::fs::set_permissions(record.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let library = Arc::new(library);
        // SAFETY: getuid cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        let checker = Checker {
            os: Os::Linux,
            account: Some(Account {
                name: b"walker".to_vec(),
                uid,
                home: PathBuf::from("/home/walker"),
            }),
            pam: Pam::with(services.path(), Os::Linux, Box::new(Arc::clone(&library))),
            record_at: RecordAt::Dir(record.path().to_path_buf()),
        };
        Fixture {
            _services: services,
            record,
            library,
            checker,
        }
    }

    fn recent(seconds: u64) -> BackoffWait {
        BackoffWait {
            until: Some(now() + Duration::from_secs(seconds)),
            why: WaitWhy::RecentFailure,
        }
    }

    #[test]
    fn a_verified_check_is_not_counted() {
        let f = fixture(ScriptedLibrary::new(vec![Step::Hidden], RIGHT));
        assert_eq!(f.checker.backend(), Ok(()));
        for _ in 0..2 {
            assert_eq!(
                f.checker.check(&secret(RIGHT), now()).outcome,
                CheckOutcome::Verified
            );
        }
        assert_eq!(f.checker.backoff(now()), Ok(BackoffState::Clear));
        assert_eq!(f.library.calls().len(), 2);
    }

    #[test]
    fn a_check_in_a_back_off_is_refused_before_pam() {
        let f = fixture(ScriptedLibrary::new(vec![Step::Hidden], RIGHT));
        assert_eq!(
            f.checker.check(&secret("not it"), now()).outcome,
            CheckOutcome::Wrong
        );
        assert_eq!(
            f.checker.backoff(now()),
            Ok(BackoffState::Wait(recent(600)))
        );
        let refused = f.checker.check(&secret(RIGHT), now());
        assert_eq!(refused.outcome, CheckOutcome::Backoff(recent(600)));
        assert_eq!(
            f.library.calls().len(),
            1,
            "PAM was not asked the second time"
        );
    }

    #[test]
    fn an_empty_or_nul_entry_never_reaches_pam_and_is_not_counted() {
        let f = fixture(ScriptedLibrary::new(vec![], RIGHT));
        let empty = f.checker.check(&secret(""), now());
        assert_eq!(empty.outcome, CheckOutcome::Refused(InputRefused::Empty));
        let nul = f.checker.check(&secret("a\0b"), now());
        assert_eq!(
            nul.outcome,
            CheckOutcome::Refused(InputRefused::ContainsNul)
        );
        assert!(f.library.calls().is_empty());
        assert_eq!(f.checker.backoff(now()), Ok(BackoffState::Clear));
    }

    #[test]
    fn a_stack_that_asks_nothing_is_not_checked_and_not_counted() {
        let f = fixture(ScriptedLibrary::new(vec![], RIGHT));
        let unasked = CheckOutcome::NotChecked(NotCheckedWhy::NoPasswordAsked);
        assert_eq!(f.checker.check(&secret(RIGHT), now()).outcome, unasked);
        assert_eq!(f.checker.backoff(now()), Ok(BackoffState::Clear));
    }

    #[test]
    fn a_second_prompt_is_not_checked_and_counted() {
        let f = fixture(ScriptedLibrary::new(
            vec![Step::Hidden, Step::Hidden],
            RIGHT,
        ));
        let other = CheckOutcome::NotChecked(NotCheckedWhy::OtherPrompt);
        assert_eq!(f.checker.check(&secret(RIGHT), now()).outcome, other);
        assert_eq!(
            f.checker.backoff(now()),
            Ok(BackoffState::Wait(recent(600)))
        );
    }

    #[test]
    fn the_stack_s_messages_come_back_with_the_outcome() {
        let locked = "The account is locked due to 3 failed logins.";
        let f = fixture(ScriptedLibrary::new(
            vec![Step::Info(locked), Step::Hidden],
            RIGHT,
        ));
        let check = f.checker.check(&secret(RIGHT), now());
        assert_eq!(
            check,
            PasswordCheck {
                outcome: CheckOutcome::Verified,
                messages: vec![locked.to_owned()]
            }
        );
    }

    #[test]
    fn without_a_record_place_no_check_runs() {
        let f = fixture(ScriptedLibrary::new(vec![Step::Hidden], RIGHT));
        std::fs::set_permissions(f.record.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let no_place = Unavailable::NoBackoffRecord {
            dir: f.record.path().to_path_buf(),
        };
        assert_eq!(f.checker.backoff(now()), Err(no_place.clone()));
        assert_eq!(
            f.checker.check(&secret(RIGHT), now()).outcome,
            CheckOutcome::Unavailable(no_place)
        );
        assert!(f.library.calls().is_empty());
    }

    #[test]
    fn without_libpam_or_a_service_no_check_runs() {
        let mut library = ScriptedLibrary::new(vec![Step::Hidden], RIGHT);
        library.loads = false;
        let f = fixture(library);
        let no_libpam = Unavailable::NoLibpam {
            tried: vec!["scripted".to_owned()],
        };
        assert_eq!(f.checker.backend(), Err(no_libpam.clone()));
        assert_eq!(
            f.checker.check(&secret(RIGHT), now()).outcome,
            CheckOutcome::Unavailable(no_libpam)
        );
        let g = fixture(ScriptedLibrary::new(vec![Step::Hidden], RIGHT));
        std::fs::remove_file(g._services.path().join("etc/pam.d/common-auth")).unwrap();
        assert_eq!(g.checker.backend(), Err(Unavailable::NoService));
        assert!(f.library.calls().is_empty() && g.library.calls().is_empty());
    }

    #[test]
    fn without_an_account_nothing_runs() {
        let mut f = fixture(ScriptedLibrary::new(vec![Step::Hidden], RIGHT));
        f.checker.account = None;
        assert_eq!(f.checker.backend(), Err(Unavailable::NoAccount));
        assert_eq!(f.checker.backoff(now()), Err(Unavailable::NoAccount));
        assert_eq!(f.checker.password_status(), PasswordStatus::UNKNOWN);
        let check = f.checker.check(&secret(RIGHT), now());
        assert_eq!(
            check.outcome,
            CheckOutcome::Unavailable(Unavailable::NoAccount)
        );
    }

    #[test]
    fn windows_has_no_account_password_slot() {
        let mut f = fixture(ScriptedLibrary::new(vec![Step::Hidden], RIGHT));
        f.checker.os = Os::Windows;
        assert_eq!(f.checker.backend(), Err(Unavailable::NotOnThisOs));
        assert_eq!(f.checker.backoff(now()), Err(Unavailable::NotOnThisOs));
        let check = f.checker.check(&secret(RIGHT), now());
        assert_eq!(
            check.outcome,
            CheckOutcome::Unavailable(Unavailable::NotOnThisOs)
        );
        assert!(f.library.calls().is_empty());
    }

    #[test]
    fn the_record_is_this_fixture_s_scratch_directory() {
        let f = fixture(ScriptedLibrary::new(vec![Step::Hidden], RIGHT));
        let _ = f.checker.check(&secret("not it"), now());
        assert!(Path::new(&f.record.path().join(crate::backoff::RECORD_FILE)).is_file());
    }
}
