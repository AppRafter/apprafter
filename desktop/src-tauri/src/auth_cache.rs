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
//! save (Windows' `hello` changes the method); on every idle tick while it has no answer or its
//! last one says nothing can verify the owner ([`LockMachine::tick`]); and whenever `app_info`
//! asks, which then waits for that answer, bounded ([`AuthCache::fresh`]) — before any answer,
//! for the question already out. One question at a time: asked while one is out, they become
//! one more question after it. A question that never comes back leaves the last answer in
//! place, so the idle lock goes on.
//!
//! It keeps what the OS can do, never an authorisation: every prompt and every password is
//! asked of the OS afresh. Until the first answer, [`AuthCache::info`] is [`UNANSWERED`],
//! counted as available, so nothing goes unlocked for want of an answer.
//!
//! [`LockMachine::tick`]: crate::lock::LockMachine::tick

use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthInfo, AuthOutcome, Settings};
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
                Ok(info) => self.lock().info = Some(info),
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
        let outcome = self.auth.verify(purpose, cancel);
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
        let answer = self.auth.verify_password(purpose, password, cancel);
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc;

    use apprafter_desktop_ipc::{AuthMethod, UnavailableReason};

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
