// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! PAM: the password typed into the app's own field, checked by the system's PAM stack where
//! polkit cannot prompt (the routing is [`crate::linux::OsAuthenticator`]'s).
//!
//! One check is:
//! 1. The service: the first of [`SERVICES`] that is a file in one of [`SERVICE_DIRS`], the
//!    directories libpam reads a service from, in its order. The names go from the auth-only
//!    stacks to the console login's, so a name found in either directory beats the next name in
//!    both. None found is `Unavailable { NoPamService }`: libpam would run a name it cannot find
//!    as the `other` service, whose answer says nothing about this user.
//! 2. The account: `getpwuid_r(getuid())`, the user running the app, never a name from the UI.
//!    `pam_unix` cannot read `/etc/shadow` for a user who is not root and checks the password
//!    through the set-group-id `unix_chkpwd`, which verifies the caller's own account only.
//! 3. `pam_authenticate` with `PAM_DISALLOW_NULL_AUTHTOK` (an account without a password proves
//!    nothing), and nothing else: no `pam_acct_mgmt`, `pam_setcred` or `pam_open_session`. The
//!    app asks who is at the keyboard; it logs nobody in.
//!
//! The conversation answers the first hidden prompt with the password. Any other prompt — a
//! second hidden one, as a one-time-code module asks, one that echoes, Linux-PAM's radio and
//! binary kinds — is refused: the field holds one secret. Info and error messages are
//! acknowledged and returned with the outcome, e.g. fprintd's "Place your finger on the reader".
//!
//! The outcome comes from this module's own record before PAM's code, in this order:
//! - the token tripped during the check: `Cancelled { by: App }`, whatever PAM answers (as for
//!   polkit: a stack can turn the conversation's refusal into `PAM_AUTH_ERR`);
//! - `PAM_SUCCESS`: `Verified`, if `PAM_USER` is still the app's user (a module may change it,
//!   and another account's success is not the owner's), else `Failed`;
//! - a prompt the conversation refused: `Unavailable { NotInteractive }`;
//! - `PAM_CONV_ERR`: a message nonstick could not hand to the conversation, so `NotInteractive`
//!   too; `PAM_ABORT`: `Failed`. [`map_pam`] makes both `Cancelled { by: App }`, which only the
//!   token decides here;
//! - any other code: [`map_pam`].
//!
//! Back-off ([`Backoff`], one per [`Pam`], so per process): every check that reached PAM and did
//! not end `Verified` is a failure, `NotInteractive` and cancels included. With a `requisite`
//! password module before a one-time-code module, "a second prompt came" says the password was
//! right, so it costs what a wrong one does. The failure that starts a refusal answers
//! `Failed { exhausted: true }`, and so does every check the refusal turns away, without calling
//! PAM: that is how "refused by the back-off" reads. Checks run one at a time: one asked while
//! another runs is `Busy`, since parallel checks would each pass the back-off before any failed.
//!
//! Memory: the password is a [`Zeroizing`] string, wiped when the check ends, and nothing here
//! formats or logs it (the conversation's `Debug` leaves it out). nonstick hands the answer to
//! libpam through an `OsString` it frees without wiping, and libpam owns the C copy from then
//! on: one short-lived copy per check that this crate cannot reach.
//!
//! Blocking: PAM sleeps a few seconds after a failure and a fingerprint module waits for a
//! finger, so a check runs on a blocking worker, never on an async worker or the main thread.
//! Nothing interrupts `pam_authenticate`: the token refuses any later prompt and decides the
//! outcome.

use std::ffi::{CStr, OsStr, OsString};
use std::fmt;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthOutcome, CancelledBy, UnavailableReason};
use nonstick::constants::ReturnCode;
use nonstick::conv::Exchange;
use nonstick::items::Items;
use nonstick::{AuthnFlags, ErrorCode, PamShared, Transaction, TransactionBuilder};
use zeroize::Zeroizing;

use crate::outcome::{map_pam, Backoff, PamCode};

/// The PAM services a check may use, in the order they are tried: the auth-only stacks of
/// Debian, Ubuntu and openSUSE (`common-auth`) and of Fedora, RHEL and Arch (`system-auth`),
/// then the console login's.
pub const SERVICES: [&str; 3] = ["common-auth", "system-auth", "login"];

/// Where libpam reads a service from, relative to `/`: the administrator's directory, then the
/// vendor's (openSUSE keeps its services in the second).
pub const SERVICE_DIRS: [&str; 2] = ["etc/pam.d", "usr/lib/pam.d"];

/// `PAM_SUCCESS`.
const SUCCESS: i32 = 0;

const APP_CANCELLED: AuthOutcome = AuthOutcome::Cancelled {
    by: CancelledBy::App,
};
const EXHAUSTED: AuthOutcome = AuthOutcome::Failed { exhausted: true };
const FAILED: AuthOutcome = AuthOutcome::Failed { exhausted: false };

const fn unavailable(reason: UnavailableReason) -> AuthOutcome {
    AuthOutcome::Unavailable { reason }
}

/// How a password check ended, with what the PAM stack said on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordCheck {
    pub outcome: AuthOutcome,
    /// PAM's info and error messages, in the order it sent them.
    pub messages: Vec<String>,
}

impl PasswordCheck {
    /// An outcome reached without asking PAM: nothing was said.
    const fn unasked(outcome: AuthOutcome) -> Self {
        Self {
            outcome,
            messages: Vec::new(),
        }
    }
}

/// Checks the device owner's password through PAM, with this process's back-off.
pub struct Pam {
    /// What [`SERVICE_DIRS`] are relative to: `/`, or a directory a test builds.
    root: PathBuf,
    library: Box<dyn Library>,
    /// The account a check is for: [`current_user`], or a test's.
    account: fn() -> Option<OsString>,
    backoff: Mutex<Backoff>,
}

impl Default for Pam {
    fn default() -> Self {
        Self::new()
    }
}

impl Pam {
    /// The system's PAM, for the user running the app.
    pub fn new() -> Self {
        Self::with(Path::new("/"), Box::new(LibPam), current_user)
    }

    fn with(root: &Path, library: Box<dyn Library>, account: fn() -> Option<OsString>) -> Self {
        Self {
            root: root.to_owned(),
            library,
            account,
            backoff: Mutex::default(),
        }
    }

    /// Whether a check could run here: a service, and an account to check. Asks PAM nothing.
    pub fn available(&self) -> Result<(), UnavailableReason> {
        self.target().map(drop)
    }

    /// The service and the account a check uses.
    fn target(&self) -> Result<(&'static str, OsString), UnavailableReason> {
        let service = find_service(&self.root).ok_or(UnavailableReason::NoPamService)?;
        // No passwd entry for the running uid: there is no account to authenticate.
        let user = (self.account)().ok_or(UnavailableReason::NotConfigured)?;
        Ok((service, user))
    }

    /// Checks `password` for the user running the app (the module docs give the steps).
    /// `now_monotonic_ms` is the shell's monotonic clock, which the back-off counts in. Blocks
    /// until PAM answers; a token already tripped asks nothing.
    pub fn verify_password(
        &self,
        password: Zeroizing<String>,
        cancel: &CancellationToken,
        now_monotonic_ms: u64,
    ) -> PasswordCheck {
        if cancel.is_cancelled() {
            return PasswordCheck::unasked(APP_CANCELLED);
        }
        let mut backoff = match self.backoff.try_lock() {
            Ok(backoff) => backoff,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return PasswordCheck::unasked(AuthOutcome::Busy),
        };
        if backoff.allow(now_monotonic_ms).is_err() {
            return PasswordCheck::unasked(EXHAUSTED);
        }
        let (service, user) = match self.target() {
            Ok(target) => target,
            Err(reason) => return PasswordCheck::unasked(unavailable(reason)),
        };
        let dialogue = Dialogue::new(password, cancel.clone());
        let answer = self.library.authenticate(service, &user, &dialogue);
        let record = dialogue.into_record();
        let outcome = decide(&answer, &user, &record, cancel.is_cancelled());
        let outcome = if outcome == AuthOutcome::Verified {
            backoff.record_success();
            outcome
        } else {
            backoff.record_failure(now_monotonic_ms);
            match outcome {
                FAILED if backoff.allow(now_monotonic_ms).is_err() => EXHAUSTED,
                other => other,
            }
        };
        PasswordCheck {
            outcome,
            messages: record.messages,
        }
    }
}

/// The first of [`SERVICES`] that is a file (or a link to one) in one of [`SERVICE_DIRS`] under
/// `root`.
pub fn find_service(root: &Path) -> Option<&'static str> {
    SERVICES.into_iter().find(|service| {
        SERVICE_DIRS
            .iter()
            .any(|dir| root.join(dir).join(service).is_file())
    })
}

/// The name of the user running the process: `getpwuid_r(getuid())`. `None` when the uid has
/// no passwd entry.
pub fn current_user() -> Option<OsString> {
    /// glibc asks for 1 KiB; a directory service can need more, but not without bound.
    const FIRST: usize = 1024;
    const LAST: usize = 1 << 20;
    // SAFETY: getuid cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    let mut buffer = vec![0_u8; FIRST];
    loop {
        let mut entry = MaybeUninit::<libc::passwd>::uninit();
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `entry` and `buffer` are valid for writes of their sizes, and getpwuid_r
        // writes no further; it points `found` at `entry` or leaves it null.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && buffer.len() < LAST {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r succeeded, so `found` points at the initialised `entry`, whose
        // `pw_name` is a NUL-terminated string inside `buffer`, which is still alive.
        let name = unsafe { CStr::from_ptr((*found).pw_name) }.to_bytes();
        return (!name.is_empty()).then(|| OsStr::from_bytes(name).to_owned());
    }
}

/// What PAM answered one check.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Answer {
    /// `PAM_SUCCESS`, or the code of the call that failed: `pam_start`'s or
    /// `pam_authenticate`'s.
    code: i32,
    /// `PAM_USER` once `pam_authenticate` returned.
    user: Option<OsString>,
}

/// The outcome of a check that reached PAM (the module docs give the order).
fn decide(answer: &Answer, user: &OsStr, record: &Record, cancelled: bool) -> AuthOutcome {
    if cancelled {
        return APP_CANCELLED;
    }
    if answer.code == SUCCESS {
        return if answer.user.as_deref() == Some(user) {
            AuthOutcome::Verified
        } else {
            FAILED
        };
    }
    if record.refused {
        return unavailable(UnavailableReason::NotInteractive);
    }
    match PamCode::from_raw(answer.code) {
        PamCode::ConvErr => unavailable(UnavailableReason::NotInteractive),
        PamCode::Abort => FAILED,
        _ => map_pam(answer.code),
    }
}

/// libpam as a check calls it: the system's in the app, a script in the tests.
trait Library: Send + Sync {
    /// `pam_start` for `service` with `PAM_USER` set to `user` and `dialogue` as the
    /// conversation, then `pam_authenticate`.
    fn authenticate(&self, service: &str, user: &OsStr, dialogue: &Dialogue) -> Answer;
}

/// The system's libpam, through nonstick.
struct LibPam;

impl Library for LibPam {
    fn authenticate(&self, service: &str, user: &OsStr, dialogue: &Dialogue) -> Answer {
        let started = TransactionBuilder::new_with_service(service)
            .username(user)
            .build(Conversation(dialogue));
        let mut transaction = match started {
            Ok(transaction) => transaction,
            Err(error) => {
                return Answer {
                    code: raw_code(error),
                    user: None,
                }
            }
        };
        let code = match transaction.authenticate(AuthnFlags::DISALLOW_NULL_AUTHTOK) {
            Ok(()) => SUCCESS,
            Err(error) => raw_code(error),
        };
        let user = transaction.items().user().ok().flatten();
        Answer { code, user }
    }
}

fn raw_code(error: ErrorCode) -> i32 {
    ReturnCode::from(error).into()
}

/// The [`Dialogue`] as nonstick's conversation.
struct Conversation<'a>(&'a Dialogue);

impl nonstick::Conversation for Conversation<'_> {
    fn communicate(&self, exchanges: &[Exchange]) {
        for exchange in exchanges {
            match exchange {
                Exchange::MaskedPrompt(prompt) => prompt.set_answer(
                    self.0
                        .hidden_prompt()
                        .map(OsString::from)
                        .ok_or(ErrorCode::ConversationError),
                ),
                Exchange::Info(message) => {
                    self.0.message(message.question());
                    message.set_answer(Ok(()));
                }
                Exchange::Error(message) => {
                    self.0.message(message.question());
                    message.set_answer(Ok(()));
                }
                // A prompt that echoes, and Linux-PAM's radio and binary kinds.
                other => {
                    self.0.other_prompt();
                    other.set_error(ErrorCode::ConversationError);
                }
            }
        }
    }
}

/// One check's side of the PAM conversation: the password, and the record of what was asked.
struct Dialogue {
    password: Zeroizing<String>,
    cancel: CancellationToken,
    /// A mutex, not a cell: nothing stops a module from calling the conversation on a thread
    /// of its own.
    record: Mutex<Record>,
}

/// What a [`Dialogue`] was asked.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Record {
    /// The first hidden prompt has had the password.
    password_given: bool,
    /// A prompt was refused (the token tripped before it, or the password was spent).
    refused: bool,
    messages: Vec<String>,
}

impl fmt::Debug for Dialogue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dialogue")
            .field("cancel", &self.cancel)
            .field("record", &*self.record())
            .finish_non_exhaustive()
    }
}

impl Dialogue {
    fn new(password: Zeroizing<String>, cancel: CancellationToken) -> Self {
        Self {
            password,
            cancel,
            record: Mutex::default(),
        }
    }

    fn record(&self) -> MutexGuard<'_, Record> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A hidden prompt: the password the first time, unless the token has tripped; `None`
    /// refuses it.
    fn hidden_prompt(&self) -> Option<&str> {
        let mut record = self.record();
        if record.password_given || self.cancel.is_cancelled() {
            record.refused = true;
            return None;
        }
        record.password_given = true;
        Some(self.password.as_str())
    }

    /// Any other prompt, which is refused.
    fn other_prompt(&self) {
        self.record().refused = true;
    }

    /// An info or error message.
    fn message(&self, text: &OsStr) {
        self.record()
            .messages
            .push(text.to_string_lossy().into_owned());
    }

    /// What was asked; the password is wiped here.
    fn into_record(self) -> Record {
        self.record
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use UnavailableReason::{NoPamService, NotConfigured, NotInteractive};

    const RIGHT: &str = "correct horse battery staple";
    const WRONG: &str = "not the password";
    const OWNER: &str = "walk";
    /// Longer than any wait a passing test makes; a failing one panics instead of hanging.
    const PATIENCE: Duration = Duration::from_secs(10);

    const VERIFIED: AuthOutcome = AuthOutcome::Verified;

    fn password(text: &str) -> Zeroizing<String> {
        Zeroizing::new(text.to_owned())
    }

    fn owner() -> Option<OsString> {
        Some(OWNER.into())
    }

    // ---- the service probe -----------------------------------------------------------------

    /// A root with these files (paths relative to it).
    fn root_with(files: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for file in files {
            let path = root.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "auth required pam_unix.so\n").unwrap();
        }
        root
    }

    #[test]
    fn no_service_file_is_no_service() {
        assert_eq!(find_service(root_with(&[]).path()), None);
        let others = root_with(&["etc/pam.d/other", "etc/pam.d/sudo", "usr/lib/pam.d/su"]);
        assert_eq!(find_service(others.path()), None);
    }

    #[test]
    fn each_name_is_found_in_either_directory() {
        for service in SERVICES {
            for dir in SERVICE_DIRS {
                let root = root_with(&[&format!("{dir}/{service}")]);
                assert_eq!(find_service(root.path()), Some(service), "{dir}/{service}");
            }
        }
    }

    #[test]
    fn the_earlier_name_wins_whichever_directory_holds_it() {
        for (files, service) in [
            (
                ["etc/pam.d/login", "usr/lib/pam.d/common-auth"],
                "common-auth",
            ),
            (
                ["usr/lib/pam.d/login", "etc/pam.d/system-auth"],
                "system-auth",
            ),
            (
                ["usr/lib/pam.d/system-auth", "etc/pam.d/login"],
                "system-auth",
            ),
            (
                ["etc/pam.d/common-auth", "etc/pam.d/system-auth"],
                "common-auth",
            ),
        ] {
            let root = root_with(&files);
            assert_eq!(find_service(root.path()), Some(service), "{files:?}");
        }
    }

    #[test]
    fn only_a_file_or_a_link_to_one_is_a_service() {
        let root = root_with(&["etc/pam.d/real-login"]);
        let dir = root.path().join("etc/pam.d");
        fs::create_dir(dir.join("common-auth")).unwrap();
        std::os::unix::fs::symlink(dir.join("missing"), dir.join("system-auth")).unwrap();
        assert_eq!(
            find_service(root.path()),
            None,
            "a directory, a dangling link"
        );
        std::os::unix::fs::symlink(dir.join("real-login"), dir.join("login")).unwrap();
        assert_eq!(find_service(root.path()), Some("login"), "a link to a file");
    }

    // ---- the conversation ------------------------------------------------------------------

    fn dialogue(cancel: &CancellationToken) -> Dialogue {
        Dialogue::new(password(RIGHT), cancel.clone())
    }

    #[test]
    fn the_first_hidden_prompt_gets_the_password_and_a_second_is_refused() {
        let dialogue = dialogue(&CancellationToken::new());
        assert_eq!(dialogue.hidden_prompt(), Some(RIGHT));
        assert_eq!(dialogue.hidden_prompt(), None);
        assert_eq!(dialogue.hidden_prompt(), None);
        assert_eq!(
            dialogue.into_record(),
            Record {
                password_given: true,
                refused: true,
                messages: vec![],
            }
        );
    }

    #[test]
    fn one_hidden_prompt_is_not_a_refusal() {
        let dialogue = dialogue(&CancellationToken::new());
        assert_eq!(dialogue.hidden_prompt(), Some(RIGHT));
        assert!(!dialogue.into_record().refused);
    }

    #[test]
    fn any_other_prompt_is_refused_and_the_password_is_still_given_once() {
        let dialogue = dialogue(&CancellationToken::new());
        dialogue.other_prompt();
        assert_eq!(dialogue.hidden_prompt(), Some(RIGHT));
        let record = dialogue.into_record();
        assert!(record.refused && record.password_given, "{record:?}");
    }

    #[test]
    fn messages_are_kept_in_order_and_refuse_nothing() {
        let dialogue = dialogue(&CancellationToken::new());
        dialogue.message(OsStr::new("Place your finger on the reader"));
        dialogue.message(OsStr::from_bytes(b"not \xff UTF-8"));
        assert_eq!(dialogue.hidden_prompt(), Some(RIGHT));
        dialogue.message(OsStr::new("Last login: never"));
        assert_eq!(
            dialogue.into_record(),
            Record {
                password_given: true,
                refused: false,
                messages: vec![
                    "Place your finger on the reader".to_owned(),
                    "not \u{fffd} UTF-8".to_owned(),
                    "Last login: never".to_owned(),
                ],
            }
        );
    }

    #[test]
    fn after_the_token_trips_no_prompt_gets_the_password() {
        let cancel = CancellationToken::new();
        let dialogue = dialogue(&cancel);
        cancel.cancel();
        assert_eq!(dialogue.hidden_prompt(), None);
        let record = dialogue.into_record();
        assert!(record.refused && !record.password_given, "{record:?}");
    }

    /// The password reaches no `Debug` output: not the dialogue's, not a check's.
    #[test]
    fn debug_output_never_carries_the_password() {
        let cancel = CancellationToken::new();
        let dialogue = dialogue(&cancel);
        dialogue.message(OsStr::new("Password:"));
        assert_eq!(dialogue.hidden_prompt(), Some(RIGHT));
        for text in [format!("{dialogue:?}"), format!("{dialogue:#?}")] {
            assert!(!text.contains(RIGHT), "{text}");
            assert!(text.contains("Password:"), "the record is shown: {text}");
        }
        let fake = Fake::script(&[Step::Hidden], Code::Right);
        let pam = fake.pam();
        let check = pam.verify_password(password(RIGHT), &cancel, 0);
        for text in [format!("{check:?}"), format!("{check:#?}")] {
            assert!(!text.contains(RIGHT), "{text}");
        }
    }

    // ---- a check, against a scripted PAM ---------------------------------------------------

    /// What the scripted PAM sends, in order.
    #[derive(Debug, Clone, Copy)]
    enum Step {
        Hidden,
        Visible,
        Info(&'static str),
    }

    /// The code the scripted PAM returns.
    #[derive(Debug, Clone, Copy)]
    enum Code {
        /// `PAM_SUCCESS` when the first hidden prompt got [`RIGHT`], else `PAM_AUTH_ERR`.
        Right,
        Fixed(i32),
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Call {
        service: String,
        user: OsString,
        /// What each hidden prompt got; `None` for a refusal.
        hidden: Vec<Option<String>>,
    }

    type Hook = Box<dyn Fn() + Send + Sync>;

    /// PAM from a script, recording every call.
    struct Fake {
        steps: Vec<Step>,
        code: Code,
        /// `PAM_USER` after the check, when a module changed it.
        user_after: Option<Option<OsString>>,
        /// Runs inside `pam_authenticate`, before the script.
        during: Option<Hook>,
        calls: Mutex<Vec<Call>>,
    }

    impl Fake {
        fn script(steps: &[Step], code: Code) -> Arc<Self> {
            Arc::new(Self {
                steps: steps.to_vec(),
                code,
                user_after: None,
                during: None,
                calls: Mutex::default(),
            })
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        /// A [`Pam`] asking this script, with every service file present and [`OWNER`] as
        /// the account.
        fn pam(self: &Arc<Self>) -> Pam {
            self.pam_in(
                root_with(&["etc/pam.d/common-auth", "etc/pam.d/login"]),
                owner,
            )
        }

        fn pam_in(
            self: &Arc<Self>,
            root: tempfile::TempDir,
            account: fn() -> Option<OsString>,
        ) -> Pam {
            let path = root.path().to_owned();
            let library = Shared {
                fake: Arc::clone(self),
                _root: root,
            };
            Pam::with(&path, Box::new(library), account)
        }
    }

    /// The fake, shared between the test and the [`Pam`] that owns its library, which also
    /// keeps the root the probe reads alive.
    struct Shared {
        fake: Arc<Fake>,
        _root: tempfile::TempDir,
    }

    impl Library for Shared {
        fn authenticate(&self, service: &str, user: &OsStr, dialogue: &Dialogue) -> Answer {
            let fake = &self.fake;
            if let Some(hook) = &fake.during {
                hook();
            }
            let mut hidden = Vec::new();
            for step in &fake.steps {
                match step {
                    Step::Hidden => hidden.push(dialogue.hidden_prompt().map(str::to_owned)),
                    Step::Visible => dialogue.other_prompt(),
                    Step::Info(text) => dialogue.message(OsStr::new(text)),
                }
            }
            let code = match fake.code {
                Code::Right if hidden.first() == Some(&Some(RIGHT.to_owned())) => SUCCESS,
                Code::Right => 7,
                Code::Fixed(code) => code,
            };
            fake.calls.lock().unwrap().push(Call {
                service: service.to_owned(),
                user: user.to_owned(),
                hidden,
            });
            Answer {
                code,
                user: fake.user_after.clone().unwrap_or_else(|| Some(user.into())),
            }
        }
    }

    fn check(pam: &Pam, text: &str, now: u64) -> AuthOutcome {
        pam.verify_password(password(text), &CancellationToken::new(), now)
            .outcome
    }

    #[test]
    fn the_right_password_is_verified_for_the_running_user_through_the_first_service() {
        let fake = Fake::script(&[Step::Info("Hello"), Step::Hidden], Code::Right);
        let pam = fake.pam();
        let check = pam.verify_password(password(RIGHT), &CancellationToken::new(), 0);
        assert_eq!(
            check,
            PasswordCheck {
                outcome: VERIFIED,
                messages: vec!["Hello".to_owned()],
            }
        );
        assert_eq!(
            fake.calls(),
            [Call {
                service: "common-auth".to_owned(),
                user: OWNER.into(),
                hidden: vec![Some(RIGHT.to_owned())],
            }]
        );
    }

    #[test]
    fn a_wrong_password_fails() {
        let fake = Fake::script(&[Step::Hidden], Code::Right);
        assert_eq!(
            check(&fake.pam(), WRONG, 0),
            AuthOutcome::Failed { exhausted: false }
        );
    }

    /// The third failure starts the refusal and says so; the refusal asks PAM nothing, not even
    /// with the right password, until it ends.
    #[test]
    fn three_failures_refuse_without_asking_pam_until_the_refusal_ends() {
        let fake = Fake::script(&[Step::Hidden], Code::Right);
        let pam = fake.pam();
        assert_eq!(
            check(&pam, WRONG, 1_000),
            AuthOutcome::Failed { exhausted: false }
        );
        assert_eq!(
            check(&pam, WRONG, 2_000),
            AuthOutcome::Failed { exhausted: false }
        );
        assert_eq!(check(&pam, WRONG, 3_000), EXHAUSTED);
        assert_eq!(fake.calls().len(), 3);
        assert_eq!(check(&pam, RIGHT, 3_001), EXHAUSTED);
        assert_eq!(check(&pam, RIGHT, 32_999), EXHAUSTED);
        assert_eq!(fake.calls().len(), 3, "a refused check asks PAM nothing");
        assert_eq!(check(&pam, RIGHT, 33_000), VERIFIED);
        assert_eq!(fake.calls().len(), 4);
    }

    #[test]
    fn a_success_starts_the_count_again() {
        let fake = Fake::script(&[Step::Hidden], Code::Right);
        let pam = fake.pam();
        check(&pam, WRONG, 0);
        check(&pam, WRONG, 0);
        assert_eq!(check(&pam, RIGHT, 0), VERIFIED);
        check(&pam, WRONG, 0);
        assert_eq!(
            check(&pam, WRONG, 0),
            AuthOutcome::Failed { exhausted: false },
            "two since the success"
        );
    }

    /// An OTP stack's second hidden prompt: refused, whatever code the stack makes of that.
    #[test]
    fn a_second_hidden_prompt_is_not_interactive_and_counts_as_a_failure() {
        for code in [7, 19, 26, 4, 9] {
            let fake = Fake::script(&[Step::Hidden, Step::Hidden], Code::Fixed(code));
            let pam = fake.pam();
            for now in 0..3 {
                assert_eq!(
                    check(&pam, RIGHT, now),
                    unavailable(NotInteractive),
                    "code {code}"
                );
            }
            assert_eq!(fake.calls()[0].hidden, [Some(RIGHT.to_owned()), None]);
            assert_eq!(check(&pam, RIGHT, 3), EXHAUSTED, "code {code}");
            assert_eq!(fake.calls().len(), 3, "code {code}");
        }
    }

    #[test]
    fn a_prompt_that_echoes_is_not_interactive() {
        let fake = Fake::script(&[Step::Visible, Step::Hidden], Code::Fixed(7));
        assert_eq!(check(&fake.pam(), RIGHT, 0), unavailable(NotInteractive));
    }

    /// The stack decided: a module it could do without was refused, and it still succeeded.
    #[test]
    fn pam_s_success_is_verified_though_a_prompt_was_refused() {
        let fake = Fake::script(&[Step::Visible, Step::Hidden, Step::Hidden], Code::Right);
        assert_eq!(check(&fake.pam(), RIGHT, 0), VERIFIED);
    }

    #[test]
    fn a_success_for_another_account_is_a_failure() {
        for user_after in [Some("root".into()), Some(OsString::new()), None] {
            let mut fake = Fake::script(&[Step::Hidden], Code::Right);
            Arc::get_mut(&mut fake).unwrap().user_after = Some(user_after.clone());
            assert_eq!(
                check(&fake.pam(), RIGHT, 0),
                AuthOutcome::Failed { exhausted: false },
                "{user_after:?}"
            );
        }
    }

    /// `PAM_CONV_ERR` and `PAM_ABORT` without a cancel: the app closed nothing, so neither is
    /// `Cancelled { by: App }` here; every other code is `map_pam`'s.
    #[test]
    fn pam_s_codes_without_a_refusal_or_a_cancel() {
        for (code, outcome) in [
            (19, unavailable(NotInteractive)),
            (26, AuthOutcome::Failed { exhausted: false }),
            (7, AuthOutcome::Failed { exhausted: false }),
            (10, AuthOutcome::Failed { exhausted: false }),
            (4, AuthOutcome::Failed { exhausted: false }),
            (9, AuthOutcome::Failed { exhausted: false }),
            (11, EXHAUSTED),
            (-1, AuthOutcome::Failed { exhausted: false }),
        ] {
            let fake = Fake::script(&[Step::Hidden], Code::Fixed(code));
            assert_eq!(check(&fake.pam(), RIGHT, 0), outcome, "code {code}");
        }
    }

    #[test]
    fn a_token_tripped_before_the_check_asks_nothing_and_counts_nothing() {
        let fake = Fake::script(&[Step::Hidden], Code::Right);
        let pam = fake.pam();
        let cancel = CancellationToken::new();
        cancel.cancel();
        for _ in 0..5 {
            assert_eq!(
                pam.verify_password(password(WRONG), &cancel, 0),
                PasswordCheck::unasked(APP_CANCELLED)
            );
        }
        assert_eq!(fake.calls(), []);
        assert_eq!(check(&pam, RIGHT, 0), VERIFIED, "no refusal");
    }

    /// The token trips while PAM runs: whatever PAM then answers, the app closed the check.
    #[test]
    fn a_token_tripped_during_the_check_is_cancelled_by_the_app_whatever_pam_answers() {
        for code in [
            Code::Right,
            Code::Fixed(7),
            Code::Fixed(19),
            Code::Fixed(26),
        ] {
            let cancel = CancellationToken::new();
            let mut fake = Fake::script(&[Step::Info("Hello"), Step::Hidden], code);
            let trip = cancel.clone();
            Arc::get_mut(&mut fake).unwrap().during = Some(Box::new(move || trip.cancel()));
            let check = fake.pam().verify_password(password(RIGHT), &cancel, 0);
            assert_eq!(
                check,
                PasswordCheck {
                    outcome: APP_CANCELLED,
                    messages: vec!["Hello".to_owned()],
                },
                "{code:?}"
            );
            assert_eq!(
                fake.calls()[0].hidden,
                [None],
                "{code:?}: no password after the trip"
            );
        }
    }

    #[test]
    fn without_a_service_file_nothing_is_asked() {
        let fake = Fake::script(&[Step::Hidden], Code::Right);
        let pam = fake.pam_in(root_with(&["etc/pam.d/other"]), owner);
        assert_eq!(pam.available(), Err(NoPamService));
        for now in 0..5 {
            assert_eq!(check(&pam, RIGHT, now), unavailable(NoPamService));
        }
        assert_eq!(fake.calls(), []);
    }

    #[test]
    fn without_an_account_nothing_is_asked() {
        let fake = Fake::script(&[Step::Hidden], Code::Right);
        let pam = fake.pam_in(root_with(&["etc/pam.d/login"]), || None);
        assert_eq!(pam.available(), Err(NotConfigured));
        assert_eq!(check(&pam, RIGHT, 0), unavailable(NotConfigured));
        assert_eq!(fake.calls(), []);
    }

    #[test]
    fn a_service_and_an_account_are_available() {
        assert_eq!(Fake::script(&[], Code::Right).pam().available(), Ok(()));
    }

    /// Parallel checks would each pass the back-off before any failed.
    #[test]
    fn a_check_while_another_runs_is_busy() {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let mut fake = Fake::script(&[Step::Hidden], Code::Right);
        Arc::get_mut(&mut fake).unwrap().during = Some(Box::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(PATIENCE).unwrap();
        }));
        let pam = fake.pam();
        thread::scope(|scope| {
            let first = scope.spawn(|| check(&pam, RIGHT, 0));
            entered.recv_timeout(PATIENCE).unwrap();
            assert_eq!(
                pam.verify_password(password(RIGHT), &CancellationToken::new(), 0),
                PasswordCheck::unasked(AuthOutcome::Busy)
            );
            release.send(()).unwrap();
            assert_eq!(first.join().unwrap(), VERIFIED);
        });
        assert_eq!(fake.calls().len(), 1);
    }

    #[test]
    fn the_running_user_is_the_passwd_entry_of_its_uid() {
        let name = current_user().expect("the test's uid has a passwd entry");
        assert!(!name.is_empty());
        let name = std::ffi::CString::new(name.as_bytes()).unwrap();
        // SAFETY: a NUL-terminated name; the entry is read before any other passwd call.
        let uid = unsafe {
            let entry = libc::getpwnam(name.as_ptr());
            assert!(!entry.is_null());
            (*entry).pw_uid
        };
        // SAFETY: getuid cannot fail.
        assert_eq!(uid, unsafe { libc::getuid() });
    }
}
