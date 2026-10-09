// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the authenticator says it can do here ([`AuthInfo`]), kept, so the lock never waits on
//! the OS to read it.
//!
//! Reading it asks the OS: on Linux a new system-bus connection and a polkit check, on Windows
//! Hello's availability, which can take seconds. The lock reads it on every idle tick, every
//! lock, every state read and every settings save, so an OS that answers slowly or never would
//! hold up all of them — and the idle lock with them. [`AuthCache`] asks the OS on a thread of
//! its own, keeps the last answer, and every reader takes that ([`AuthCache::info`]), never
//! the OS.
//!
//! It asks again ([`AuthCache::refresh`]): at start; after every lock ([`Authenticator::locked`]);
//! after every prompt and every password check, whatever they answered; after every settings
//! save (Windows' `hello` changes the method); on idle ticks while it has no answer or its last
//! one says nothing can verify the owner, 5 s apart at first and up to 5 minutes as that answer
//! stays ([`LockMachine::tick`]); and whenever `app_info` asks, which then waits for that
//! answer, bounded ([`AuthCache::fresh`]) — before any answer, for the question already out.
//! One question at a time: asked while one is out, they become one more question after it. A
//! question that never comes back leaves the last answer in place, so the idle lock goes on.
//!
//! It keeps what the OS can do, never an authorisation: every prompt and every password is
//! asked of the OS afresh. Until the first answer, [`AuthCache::info`] is [`UNANSWERED`],
//! counted as available, so nothing goes unlocked for want of an answer.
//!
//! Every answer the OS gives passes through here (the lock and the destructive-operation gesture
//! both ask through the cache), so this is where the log hears them, at `info`, which a release
//! build keeps:
//! - `the OS answered`, once per prompt or password check: why it asked (`purpose`: `unlock`, or
//!   `confirm(<verb>)` with the app's own word for the operation; the target's name is the
//!   owner's, and stays out of a file people attach to bug reports), the `way` (`prompt` or
//!   `password_field`), the `outcome` by its IPC names, how long it took (`took_ms`), what the
//!   cache knew when it asked (`method`, `unanswered` before the first answer; `available`;
//!   `password_field`), and, on the password field, how many `messages` the OS sent on the way.
//!   Never their text: PAM modules the app does not control write it, and it can name the
//!   account or echo what was typed. The page shows them. Never the password: the check has
//!   taken it, and wiped it, before the line is written.
//! - `what can verify the owner here`, at the first answer and whenever an answer differs from
//!   the last: the whole [`AuthInfo`]. Not for the same answer again, which the idle tick asks
//!   for every few seconds while nothing can verify the owner.
//!
//! [`LockMachine::tick`]: crate::lock::LockMachine::tick

use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthInfo, AuthOutcome, Settings};
use serde::Serialize;
use zeroize::Zeroizing;

use crate::auth::{AuthPurpose, Authenticator, PasswordAnswer};
use crate::ops::panic_message;

/// What [`AuthCache::info`] says before the OS first answered: available, method unknown. The
/// lock counts it as in effect, so a slow first answer leaves nothing unlocked that should be
/// locked, and every request still asks the OS itself.
pub const UNANSWERED: AuthInfo = AuthInfo {
    available: true,
    method: None,
    unavailable: None,
    biometrics_choice: false,
    password_field: false,
};

/// Called after every answer, on the asking thread, holding nothing of the cache's.
type Listener = Box<dyn Fn() + Send + Sync>;

/// An [`Authenticator`] whose [`info`](Authenticator::info) is kept (see the module docs);
/// every other call goes to the OS's.
pub struct AuthCache {
    auth: Arc<dyn Authenticator>,
    state: Mutex<State>,
    /// Notified when an answer comes in.
    answered: Condvar,
    listener: OnceLock<Listener>,
    /// Itself, for the asking thread.
    me: Weak<AuthCache>,
}

#[derive(Default)]
struct State {
    /// The last answer.
    info: Option<AuthInfo>,
    /// The questions asked so far.
    asked: u64,
    /// The question the last answer (or failure) was for.
    answered: u64,
    /// A thread is asking.
    asking: bool,
}

impl AuthCache {
    /// The cache over `auth`, with no answer yet and no question asked.
    pub fn new(auth: Arc<dyn Authenticator>) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            auth,
            state: Mutex::default(),
            answered: Condvar::new(),
            listener: OnceLock::new(),
            me: me.clone(),
        })
    }

    /// The OS's own answer, once there is one.
    pub fn known(&self) -> Option<AuthInfo> {
        self.lock().info.clone()
    }

    /// Ask the OS again, on the asking thread; returns at once.
    pub fn refresh(&self) {
        self.ask();
    }

    /// Ask the OS again, and wait for that answer up to `within`; the answer, or `None` when
    /// it did not come in time (the last one stays in [`known`](Self::known)).
    ///
    /// Before the OS has answered anything, a question already out will do: it was asked as the
    /// app started, so its answer is as fresh as a new one's, and the start's bound holds one
    /// round trip to the OS rather than two (the settings' question, then this one).
    pub fn fresh(&self, within: Duration) -> Option<AuthInfo> {
        let out = {
            let state = self.lock();
            (state.answered == 0 && state.asking).then_some(state.asked)
        };
        let question = out.unwrap_or_else(|| self.ask());
        let deadline = Instant::now() + within;
        let mut state = self.lock();
        while state.answered < question {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            state = self
                .answered
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.info.clone()
    }

    /// Waits up to `within` until every question asked so far has its answer; whether it has.
    pub fn settled(&self, within: Duration) -> bool {
        let state = self.lock();
        let (state, _) = self
            .answered
            .wait_timeout_while(state, within, |state| state.answered < state.asked)
            .unwrap_or_else(PoisonError::into_inner);
        state.answered >= state.asked
    }

    /// `listener` is called after every answer, on the asking thread, holding nothing of the
    /// cache's. One listener: a second is ignored.
    pub fn on_answer(&self, listener: impl Fn() + Send + Sync + 'static) {
        if self.listener.set(Box::new(listener)).is_err() {
            tracing::warn!("the authenticator's cache has a listener already");
        }
    }

    /// A new question; its number. Starts the asking thread unless it is already asking, in
    /// which case it asks once more when its question is answered.
    fn ask(&self) -> u64 {
        let mut state = self.lock();
        state.asked += 1;
        let question = state.asked;
        if state.asking {
            return question;
        }
        let Some(me) = self.me.upgrade() else {
            return question;
        };
        state.asking = true;
        drop(state);
        let spawned = thread::Builder::new()
            .name("auth-info".into())
            .spawn(move || me.answer_all());
        if let Err(e) = spawned {
            tracing::warn!("no thread to ask the authenticator on ({e}): its last answer stays");
            self.lock().asking = false;
        }
        question
    }

    /// The asking thread: asks until no question is left unanswered. A question counts as
    /// answered once the listener has heard the answer, so whoever waits for it ([`fresh`],
    /// [`settled`]) also finds what the listener did with it.
    ///
    /// [`fresh`]: Self::fresh
    /// [`settled`]: Self::settled
    fn answer_all(&self) {
        loop {
            let question = self.lock().asked;
            match panic::catch_unwind(AssertUnwindSafe(|| self.auth.info())) {
                Ok(info) => {
                    let last = self.lock().info.replace(info.clone());
                    if last.as_ref() != Some(&info) {
                        log_info(&info);
                    }
                }
                Err(payload) => tracing::error!(
                    "the authenticator panicked when asked what it can do: {}; its last answer \
                     stays",
                    panic_message(&*payload)
                ),
            }
            if let Some(listener) = self.listener.get() {
                if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(listener)) {
                    tracing::error!(
                        "the authenticator's listener panicked: {}",
                        panic_message(&*payload)
                    );
                }
            }
            let mut state = self.lock();
            state.answered = question;
            self.answered.notify_all();
            state.asking = state.asked > question;
            if !state.asking {
                return;
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Authenticator for AuthCache {
    /// The last answer, or [`UNANSWERED`] before the first; never the OS.
    fn info(&self) -> AuthInfo {
        self.known().unwrap_or(UNANSWERED)
    }

    fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
        let known = self.known();
        let asked = Instant::now();
        let outcome = self.auth.verify(purpose, cancel);
        log_answer(
            purpose,
            "prompt",
            &outcome,
            asked.elapsed(),
            known.as_ref(),
            None,
        );
        // A prompt can change what the OS can do (Linux: a dialog that found no agent).
        self.refresh();
        outcome
    }

    fn verify_password(
        &self,
        purpose: &AuthPurpose,
        password: Zeroizing<String>,
        cancel: &CancellationToken,
    ) -> PasswordAnswer {
        let known = self.known();
        let asked = Instant::now();
        let answer = self.auth.verify_password(purpose, password, cancel);
        log_answer(
            purpose,
            "password_field",
            &answer.outcome,
            asked.elapsed(),
            known.as_ref(),
            Some(answer.messages.len()),
        );
        self.refresh();
        answer
    }

    fn locked(&self) {
        self.auth.locked();
        self.refresh();
    }

    fn apply_settings(&self, settings: &Settings) {
        self.auth.apply_settings(settings);
        self.refresh();
    }

    fn set_window(&self, hwnd: isize) {
        self.auth.set_window(hwnd);
    }
}

/// The `the OS answered` line (see the module docs). `known` is what the cache knew when it
/// asked; `messages`, on the password field, how many messages the OS sent.
fn log_answer(
    purpose: &AuthPurpose,
    way: &str,
    outcome: &AuthOutcome,
    took: Duration,
    known: Option<&AuthInfo>,
    messages: Option<usize>,
) {
    let info = known.unwrap_or(&UNANSWERED);
    let method = known.map_or_else(|| "unanswered".to_owned(), |info| name_or_none(info.method));
    tracing::info!(
        purpose = %Purpose(purpose),
        way = %way,
        outcome = %Outcome(outcome),
        took_ms = u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
        method = %method,
        available = info.available,
        password_field = info.password_field,
        messages,
        "the OS answered"
    );
}

/// The `what can verify the owner here` line (see the module docs).
fn log_info(info: &AuthInfo) {
    tracing::info!(
        available = info.available,
        method = %name_or_none(info.method),
        unavailable = %name_or_none(info.unavailable),
        password_field = info.password_field,
        biometrics_choice = info.biometrics_choice,
        "what can verify the owner here"
    );
}

/// Why the OS was asked: `unlock`, or `confirm(<verb>)`. Never the target's name.
struct Purpose<'a>(&'a AuthPurpose);

impl fmt::Display for Purpose<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            AuthPurpose::Unlock => f.write_str("unlock"),
            AuthPurpose::Confirm { verb, .. } => write!(f, "confirm({verb})"),
        }
    }
}

/// An outcome by its IPC names, one word: `verified`, `cancelled(by=user)`,
/// `failed(exhausted=true,retry_in_ms=29873)`, `busy`, `unavailable(reason=policy_missing)`.
struct Outcome<'a>(&'a AuthOutcome);

impl fmt::Display for Outcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self.0 {
            AuthOutcome::Verified => f.write_str("verified"),
            AuthOutcome::Cancelled { by } => write!(f, "cancelled(by={})", name(by)),
            AuthOutcome::Failed {
                exhausted,
                retry_in_ms: None,
            } => write!(f, "failed(exhausted={exhausted})"),
            AuthOutcome::Failed {
                exhausted,
                retry_in_ms: Some(ms),
            } => write!(f, "failed(exhausted={exhausted},retry_in_ms={ms})"),
            AuthOutcome::Busy => f.write_str("busy"),
            AuthOutcome::Unavailable { reason } => {
                write!(f, "unavailable(reason={})", name(reason))
            }
        }
    }
}

/// A unit variant's name on the IPC wire (`policy_missing`, `polkit`), the names the page and
/// its generated types use.
fn name(variant: impl Serialize + fmt::Debug) -> String {
    match serde_json::to_value(&variant) {
        Ok(serde_json::Value::String(name)) => name,
        _ => format!("{variant:?}"),
    }
}

/// [`name`], or `none`.
fn name_or_none(variant: Option<impl Serialize + fmt::Debug>) -> String {
    variant.map_or_else(|| "none".to_owned(), name)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc;

    use apprafter_desktop_ipc::{AuthMethod, CancelledBy, UnavailableReason};

    use super::*;
    use crate::auth::FakeAuthenticator;

    /// Longer than any wait a passing test makes.
    const LONG: Duration = Duration::from_secs(10);

    fn method(method: AuthMethod) -> AuthInfo {
        AuthInfo {
            method: Some(method),
            ..FakeAuthenticator::new().info()
        }
    }

    fn unavailable() -> AuthInfo {
        AuthInfo {
            available: false,
            method: None,
            unavailable: Some(UnavailableReason::NoBackend),
            biometrics_choice: false,
            password_field: false,
        }
    }

    /// An OS whose every `info` waits for the test to answer it, and counts the questions.
    struct Gated {
        answers: Mutex<mpsc::Receiver<AuthInfo>>,
        asked: AtomicUsize,
        fake: FakeAuthenticator,
    }

    impl Gated {
        fn new() -> (Arc<Self>, mpsc::Sender<AuthInfo>) {
            let (send, answers) = mpsc::channel();
            let gated = Arc::new(Self {
                answers: Mutex::new(answers),
                asked: AtomicUsize::new(0),
                fake: FakeAuthenticator::new(),
            });
            (gated, send)
        }

        fn asked(&self) -> usize {
            self.asked.load(SeqCst)
        }
    }

    impl Authenticator for Gated {
        fn info(&self) -> AuthInfo {
            self.asked.fetch_add(1, SeqCst);
            // The test dropped its sender: answer at once rather than hang.
            self.answers
                .lock()
                .unwrap()
                .recv_timeout(LONG)
                .unwrap_or_else(|_| unavailable())
        }

        fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
            self.fake.verify(purpose, cancel)
        }
    }

    /// Waits until `done` holds; a test that waits longer than `LONG` fails.
    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + LONG;
        while !done() {
            assert!(Instant::now() < deadline, "{what} never happened");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn reading_never_asks_the_os_and_a_question_that_never_comes_back_keeps_the_last_answer() {
        let (os, answer) = Gated::new();
        let cache = AuthCache::new(os.clone());
        let started = Instant::now();
        assert_eq!(cache.info(), UNANSWERED, "before any answer");
        assert_eq!(cache.known(), None);
        assert_eq!(os.asked(), 0, "reading asked nothing");
        cache.refresh();
        wait_until("the first question", || os.asked() == 1);
        assert_eq!(cache.info(), UNANSWERED, "while it is out");
        answer.send(method(AuthMethod::Polkit)).unwrap();
        assert!(cache.settled(LONG));
        assert_eq!(cache.info(), method(AuthMethod::Polkit));
        // The next question hangs: every read still answers at once, with the last answer.
        cache.refresh();
        wait_until("the second question", || os.asked() == 2);
        for _ in 0..100 {
            assert_eq!(cache.info(), method(AuthMethod::Polkit));
        }
        assert!(started.elapsed() < LONG / 2, "{:?}", started.elapsed());
        drop(answer);
    }

    #[test]
    fn questions_asked_while_one_is_out_become_one_more_question() {
        let (os, answer) = Gated::new();
        let cache = AuthCache::new(os.clone());
        cache.refresh();
        wait_until("the first question", || os.asked() == 1);
        for _ in 0..20 {
            cache.refresh();
        }
        answer.send(method(AuthMethod::Polkit)).unwrap();
        wait_until("one more question", || os.asked() == 2);
        answer.send(method(AuthMethod::Pam)).unwrap();
        assert!(cache.settled(LONG));
        assert_eq!(os.asked(), 2, "twenty asks, one question");
        assert_eq!(cache.info(), method(AuthMethod::Pam), "the later answer");
    }

    #[test]
    fn fresh_waits_for_an_answer_asked_after_it_and_no_longer_than_it_may() {
        let (os, answer) = Gated::new();
        let cache = AuthCache::new(os.clone());
        cache.refresh();
        wait_until("the first question", || os.asked() == 1);
        answer.send(method(AuthMethod::Polkit)).unwrap();
        assert!(cache.settled(LONG));
        cache.refresh();
        wait_until("the second question", || os.asked() == 2);
        // Once anything has answered, an answer to the question already out is not fresh
        // enough: it waits for its own.
        let fresh = {
            let cache = cache.clone();
            thread::spawn(move || cache.fresh(LONG))
        };
        answer.send(method(AuthMethod::Polkit)).unwrap();
        wait_until("its own question", || os.asked() == 3);
        assert!(!fresh.is_finished(), "it took the older answer");
        answer.send(method(AuthMethod::Pam)).unwrap();
        assert_eq!(fresh.join().unwrap(), Some(method(AuthMethod::Pam)));

        // No answer within its bound: none, and the last one stays.
        let started = Instant::now();
        assert_eq!(cache.fresh(Duration::from_millis(100)), None);
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert_eq!(cache.info(), method(AuthMethod::Pam));
        drop(answer);
    }

    /// The start: the settings' question is out and nothing has answered yet. That question was
    /// asked as the app started, so its answer is as fresh as a new one's, and the start's
    /// bound (`STARTUP_WITHIN`) holds one round trip to the OS, not two.
    #[test]
    fn before_any_answer_fresh_takes_the_question_already_out() {
        let (os, answer) = Gated::new();
        let cache = AuthCache::new(os.clone());
        cache.refresh();
        wait_until("the first question", || os.asked() == 1);
        assert_eq!(cache.fresh(Duration::ZERO), None, "nothing to give yet");
        answer.send(method(AuthMethod::Polkit)).unwrap();
        // Any later question would be answered at once, and differently.
        drop(answer);
        assert!(cache.settled(LONG));
        assert_eq!(os.asked(), 1, "fresh asked no question of its own");
        assert_eq!(cache.info(), method(AuthMethod::Polkit));
    }

    /// Counts the questions, answering at once.
    #[derive(Default)]
    struct Counts {
        asked: AtomicUsize,
        fake: FakeAuthenticator,
    }

    impl Authenticator for Counts {
        fn info(&self) -> AuthInfo {
            self.asked.fetch_add(1, SeqCst);
            self.fake.info()
        }

        fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
            self.fake.verify(purpose, cancel)
        }

        fn verify_password(
            &self,
            purpose: &AuthPurpose,
            password: Zeroizing<String>,
            cancel: &CancellationToken,
        ) -> PasswordAnswer {
            self.fake.verify_password(purpose, password, cancel)
        }
    }

    #[test]
    fn every_prompt_check_lock_and_save_asks_again_and_the_listener_hears_each_answer() {
        let os = Arc::new(Counts::default());
        let cache = AuthCache::new(os.clone());
        let heard = Arc::new(AtomicUsize::new(0));
        cache.on_answer({
            let heard = heard.clone();
            move || {
                heard.fetch_add(1, SeqCst);
            }
        });
        let token = CancellationToken::new();
        type Step<'a> = (&'a str, Box<dyn Fn() + 'a>);
        let steps: [Step; 5] = [
            (
                "a prompt",
                Box::new(|| {
                    let _ = cache.verify(&AuthPurpose::Unlock, &token);
                }),
            ),
            (
                "a password check",
                Box::new(|| {
                    let password = Zeroizing::new("pw".to_owned());
                    drop(cache.verify_password(&AuthPurpose::Unlock, password, &token));
                }),
            ),
            ("a lock", Box::new(|| cache.locked())),
            (
                "a settings save",
                Box::new(|| cache.apply_settings(&Settings::default())),
            ),
            (
                "app_info",
                Box::new(|| {
                    let _ = cache.fresh(LONG).expect("an answer");
                }),
            ),
        ];
        for (i, (what, step)) in steps.iter().enumerate() {
            step();
            assert!(cache.settled(LONG), "{what}");
            assert_eq!(os.asked.load(SeqCst), i + 1, "{what} asked again");
            assert_eq!(heard.load(SeqCst), i + 1, "{what}: the listener heard it");
        }
        // Forwarded, and never asking again by itself.
        cache.set_window(1);
        assert_eq!(os.asked.load(SeqCst), 5);
    }

    /// Whoever waits for an answer finds what the listener (the lock) did with it.
    #[test]
    fn a_question_counts_as_answered_once_the_listener_has_heard_it() {
        let cache = AuthCache::new(Arc::new(FakeAuthenticator::new()));
        let (entered_tx, entered) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        cache.on_answer(move || {
            let _ = entered_tx.send(());
            let _ = released.lock().unwrap().recv_timeout(LONG);
        });
        cache.refresh();
        entered.recv_timeout(LONG).expect("the listener heard it");
        assert!(cache.known().is_some(), "the answer is there for readers");
        assert!(
            !cache.settled(Duration::ZERO),
            "not answered while the listener is still at it"
        );
        release.send(()).unwrap();
        assert!(cache.settled(LONG));
    }

    /// The lines the log holds that contain `message`.
    fn lines_with<'a>(log: &'a str, message: &str) -> Vec<&'a str> {
        log.lines().filter(|line| line.contains(message)).collect()
    }

    /// A `field=value` of `line`, up to the next space.
    fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
        line.split(' ')
            .find_map(|word| word.strip_prefix(name)?.strip_prefix('='))
    }

    /// Answers the cache's questions here, on the calling thread (whose log [`logged`] reads),
    /// rather than on the asking thread.
    ///
    /// [`logged`]: crate::runtime::logged
    fn answer_here(cache: &AuthCache) {
        {
            let mut state = cache.lock();
            state.asked += 1;
            state.asking = true;
        }
        cache.answer_all();
    }

    #[test]
    fn every_answer_of_the_os_is_one_line_with_why_how_what_and_how_long() {
        let os = Arc::new(FakeAuthenticator::new());
        let cache = AuthCache::new(os.clone());
        let token = CancellationToken::new();
        let confirm = AuthPurpose::Confirm {
            target: Some("acme-prod".into()),
            verb: "delete".into(),
        };
        for (purpose, outcome, says) in [
            (&AuthPurpose::Unlock, AuthOutcome::Verified, "verified"),
            (
                &AuthPurpose::Unlock,
                AuthOutcome::Failed {
                    exhausted: false,
                    retry_in_ms: None,
                },
                "failed(exhausted=false)",
            ),
            (
                &AuthPurpose::Unlock,
                AuthOutcome::Failed {
                    exhausted: true,
                    retry_in_ms: Some(29_873),
                },
                "failed(exhausted=true,retry_in_ms=29873)",
            ),
            (
                &confirm,
                AuthOutcome::Unavailable {
                    reason: UnavailableReason::PolicyMissing,
                },
                "unavailable(reason=policy_missing)",
            ),
            (
                &AuthPurpose::Unlock,
                AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                },
                "cancelled(by=user)",
            ),
            (&confirm, AuthOutcome::Busy, "busy"),
        ] {
            os.then(outcome);
            let log = crate::runtime::logged(|| {
                assert_eq!(cache.verify(purpose, &token), outcome);
            });
            let lines = lines_with(&log, "the OS answered");
            assert_eq!(lines.len(), 1, "one line per answer: {log}");
            let line = lines[0];
            assert!(
                line.contains(" INFO apprafter_desktop::auth_cache: "),
                "{line}"
            );
            assert_eq!(field(line, "outcome"), Some(says), "{line}");
            assert_eq!(field(line, "way"), Some("prompt"), "{line}");
            let why = match purpose {
                AuthPurpose::Unlock => "unlock",
                AuthPurpose::Confirm { .. } => "confirm(delete)",
            };
            assert_eq!(field(line, "purpose"), Some(why), "{line}");
            assert!(
                !line.contains("acme-prod"),
                "the target's name stays out: {line}"
            );
            assert!(field(line, "took_ms").is_some(), "{line}");
            assert_eq!(
                field(line, "messages"),
                None,
                "a prompt says nothing: {line}"
            );
            assert!(cache.settled(LONG));
        }

        // The app closes the prompt: the token tripped.
        let closed = CancellationToken::new();
        closed.cancel();
        let log = crate::runtime::logged(|| {
            cache.verify(&AuthPurpose::Unlock, &closed);
        });
        let line = lines_with(&log, "the OS answered")[0];
        assert_eq!(field(line, "outcome"), Some("cancelled(by=app)"), "{line}");
    }

    #[test]
    fn an_answer_names_what_the_app_knew_when_it_asked() {
        let cache = AuthCache::new(Arc::new(FakeAuthenticator::new()));
        let token = CancellationToken::new();
        let log = crate::runtime::logged(|| {
            cache.verify(&AuthPurpose::Unlock, &token);
        });
        let before = lines_with(&log, "the OS answered")[0];
        assert_eq!(field(before, "method"), Some("unanswered"), "{before}");
        assert_eq!(field(before, "available"), Some("true"), "{before}");
        assert_eq!(field(before, "password_field"), Some("false"), "{before}");

        let cache = AuthCache::new(Arc::new(
            FakeAuthenticator::new().with_password("pw".to_owned()),
        ));
        cache.fresh(LONG).expect("an answer");
        let log = crate::runtime::logged(|| {
            cache.verify(&AuthPurpose::Unlock, &token);
        });
        let after = lines_with(&log, "the OS answered")[0];
        assert_eq!(field(after, "method"), Some("fake"), "{after}");
        assert_eq!(field(after, "available"), Some("true"), "{after}");
        assert_eq!(field(after, "password_field"), Some("true"), "{after}");
    }

    #[test]
    fn an_answer_says_how_long_the_os_took() {
        /// Takes its time over every prompt.
        struct Slow;
        impl Authenticator for Slow {
            fn info(&self) -> AuthInfo {
                FakeAuthenticator::new().info()
            }
            fn verify(&self, _: &AuthPurpose, _: &CancellationToken) -> AuthOutcome {
                thread::sleep(Duration::from_millis(60));
                AuthOutcome::Verified
            }
        }
        let cache = AuthCache::new(Arc::new(Slow));
        let log = crate::runtime::logged(|| {
            cache.verify(&AuthPurpose::Unlock, &CancellationToken::new());
        });
        let line = lines_with(&log, "the OS answered")[0];
        let took: u64 = field(line, "took_ms").unwrap().parse().unwrap();
        assert!((60..LONG.as_millis() as u64).contains(&took), "{line}");
    }

    /// The password, and what PAM said on the way, never reach the log: PAM's messages come
    /// from modules the app does not control and can name the account, or echo what was typed.
    /// The log keeps how many there were.
    #[test]
    fn a_password_answer_never_logs_the_password_or_what_the_os_said() {
        const SECRET: &str = "hunter2-SENTINEL";
        let os = Arc::new(FakeAuthenticator::new().with_password(SECRET.to_owned()));
        os.saying(&["Account walk: you typed hunter2-SENTINEL", "Try again"]);
        let cache = AuthCache::new(os.clone());
        let token = CancellationToken::new();
        let log = crate::runtime::logged(|| {
            let right =
                cache.verify_password(&AuthPurpose::Unlock, SECRET.to_owned().into(), &token);
            assert_eq!(right.outcome, AuthOutcome::Verified);
            let wrong = format!("not-{SECRET}");
            let wrong = cache.verify_password(&AuthPurpose::Unlock, wrong.into(), &token);
            assert_eq!(wrong.messages.len(), 2, "the page still hears them");
        });
        assert!(!log.contains("SENTINEL"), "{log}");
        assert!(!log.contains("Try again"), "{log}");
        let lines = lines_with(&log, "the OS answered");
        assert_eq!(lines.len(), 2, "{log}");
        assert_eq!(field(lines[0], "way"), Some("password_field"), "{log}");
        assert_eq!(field(lines[0], "outcome"), Some("verified"), "{log}");
        assert_eq!(field(lines[0], "messages"), Some("0"), "{log}");
        assert_eq!(
            field(lines[1], "outcome"),
            Some("failed(exhausted=false)"),
            "{log}"
        );
        assert_eq!(field(lines[1], "messages"), Some("2"), "{log}");
    }

    /// The "what can verify the owner here" line: at the first answer, and whenever an answer
    /// differs from the last, never for the same answer again (the idle tick re-asks every few
    /// seconds while nothing can verify the owner).
    #[test]
    fn what_can_verify_the_owner_is_logged_at_the_first_answer_and_on_every_change() {
        let (os, answer) = Gated::new();
        let cache = AuthCache::new(os);
        let polkit = method(AuthMethod::Polkit);
        let no_agent = AuthInfo {
            available: false,
            method: Some(AuthMethod::Pam),
            unavailable: Some(UnavailableReason::NoAgent),
            biometrics_choice: false,
            password_field: true,
        };
        let mut said = Vec::new();
        for info in [polkit.clone(), polkit, no_agent.clone(), no_agent] {
            answer.send(info).unwrap();
            let log = crate::runtime::logged(|| answer_here(&cache));
            said.push(
                lines_with(&log, "what can verify the owner here")
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect::<Vec<_>>(),
            );
        }
        assert_eq!(
            said.iter().map(Vec::len).collect::<Vec<_>>(),
            [1, 0, 1, 0],
            "{said:#?}"
        );
        let first = &said[0][0];
        assert!(
            first.contains(" INFO apprafter_desktop::auth_cache: "),
            "{first}"
        );
        for (name, value) in [
            ("available", "true"),
            ("method", "polkit"),
            ("unavailable", "none"),
            ("password_field", "false"),
            ("biometrics_choice", "false"),
        ] {
            assert_eq!(field(first, name), Some(value), "{name}: {first}");
        }
        let changed = &said[2][0];
        for (name, value) in [
            ("available", "false"),
            ("method", "pam"),
            ("unavailable", "no_agent"),
            ("password_field", "true"),
        ] {
            assert_eq!(field(changed, name), Some(value), "{name}: {changed}");
        }
    }

    #[test]
    fn a_panicking_question_keeps_the_last_answer_and_answers_whoever_waits() {
        struct PanicsSecond(AtomicUsize);
        impl Authenticator for PanicsSecond {
            fn info(&self) -> AuthInfo {
                if self.0.fetch_add(1, SeqCst) == 1 {
                    panic!("the OS broke");
                }
                FakeAuthenticator::new().info()
            }
            fn verify(&self, _: &AuthPurpose, _: &CancellationToken) -> AuthOutcome {
                AuthOutcome::Verified
            }
        }
        let cache = AuthCache::new(Arc::new(PanicsSecond(AtomicUsize::new(0))));
        let first = cache.fresh(LONG).expect("an answer");
        let started = Instant::now();
        assert_eq!(cache.fresh(LONG), Some(first.clone()), "the last answer");
        assert!(started.elapsed() < LONG / 2, "answered, not timed out");
        assert_eq!(cache.info(), first);
    }
}
