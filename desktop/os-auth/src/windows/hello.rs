// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Windows Hello through `Windows.Security.Credentials.UI.UserConsentVerifier`.
//!
//! Two asynchronous operations per request: `CheckAvailabilityAsync` first (asking for a
//! verification without it can hang), then the interop's
//! `RequestVerificationForWindowAsync` (`IUserConsentVerifierInterop`) with the app's own
//! window, which parents the prompt so it opens in front of the app rather than behind it
//! (documented from Windows 11, build 22000).
//!
//! Waiting ([`wait_for`]): the operation's completion handler sends its answer into a channel,
//! and the caller's thread reads it in [`TICK`] steps, looking at the [`CancellationToken`]
//! between them. A tripped token cancels the operation through the OS
//! (`IAsyncInfo::Cancel`, which closes the prompt), waits up to [`CANCEL_GRACE`] for it to end,
//! and answers [`Asked::Cancelled`] whatever the operation then says: a prompt the app closed is
//! the app's cancel even if the user had just verified. An answer that arrives once the token
//! has tripped is that cancel too.
//!
//! Threads: the caller's, a blocking worker, never the main thread (a single-threaded COM
//! apartment, whose completion handlers would wait for a message loop this wait does not run).
//! windows-core puts the process in the multithreaded apartment on its first activation, which
//! is the apartment such a worker uses.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use ::windows::core::{factory, RuntimeType, HRESULT, HSTRING};
use ::windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use ::windows::Win32::Foundation::{E_FAIL, HWND};
use ::windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use apprafter_core::CancellationToken;
use windows_future::IAsyncOperation;

/// How often a wait looks at the token.
pub const TICK: Duration = Duration::from_millis(100);
/// How long a cancelled operation may take to end before the wait gives up on it.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);
/// How long `CheckAvailabilityAsync` may take: it asks nobody, so it answers at once or not at
/// all.
pub const AVAILABILITY_LIMIT: Duration = Duration::from_secs(10);

/// What one Hello operation came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// The operation's answer, the raw value of `UserConsentVerifierAvailability` or
    /// `UserConsentVerificationResult`.
    Answered(i32),
    /// The operation could not be started (no such WinRT class, no interop, a window the call
    /// refused), so no prompt opened.
    NotStarted,
    /// The operation started, then failed or gave no answer within its limit.
    Failed,
    /// The token tripped; the operation was cancelled through the OS.
    Cancelled,
}

/// Windows Hello as the authenticator asks it: the system's in the app, a script in the tests.
pub trait Hello: Send + Sync {
    /// `CheckAvailabilityAsync`, within [`AVAILABILITY_LIMIT`]. Opens no prompt.
    fn availability(&self, cancel: &CancellationToken) -> Asked;
    /// `RequestVerificationForWindowAsync(window, message)`: opens Hello's prompt in front of
    /// `window` and waits until it answers, or until `cancel` closes it.
    fn verify(&self, window: isize, message: &str, cancel: &CancellationToken) -> Asked;
}

/// An asynchronous operation as [`wait_for`] waits for it.
pub trait Pending {
    /// The answer, if it comes within `timeout`: the raw value, or the error the operation
    /// failed with.
    fn wait(&self, timeout: Duration) -> Option<Result<i32, HRESULT>>;
    /// Asks the OS to cancel the operation. It may still answer afterwards, or not at all.
    fn cancel(&self);
}

/// How long [`wait_for`] waits at a time and for a cancelled operation to end.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub tick: Duration,
    pub grace: Duration,
}

/// The app's timing: [`TICK`] and [`CANCEL_GRACE`].
pub const TIMING: Timing = Timing {
    tick: TICK,
    grace: CANCEL_GRACE,
};

/// Waits for `operation` (the module docs give the rules), at most `limit` when there is one.
/// An operation that answers nothing within its limit is cancelled and [`Asked::Failed`].
pub fn wait_for(
    operation: &impl Pending,
    cancel: &CancellationToken,
    limit: Option<Duration>,
    timing: Timing,
) -> Asked {
    let started = Instant::now();
    loop {
        if cancel.is_cancelled() {
            end(operation, timing);
            return Asked::Cancelled;
        }
        if let Some(answer) = operation.wait(timing.tick) {
            return match answer {
                _ if cancel.is_cancelled() => Asked::Cancelled,
                Ok(raw) => Asked::Answered(raw),
                Err(_) => Asked::Failed,
            };
        }
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            end(operation, timing);
            return Asked::Failed;
        }
    }
}

/// Cancels `operation` and gives it [`Timing::grace`] to end, so that its prompt has closed by
/// the time the caller answers, if the OS closes it at all.
fn end(operation: &impl Pending, timing: Timing) {
    operation.cancel();
    let _ = operation.wait(timing.grace);
}

/// A WinRT `IAsyncOperation` whose completion handler sends its answer into a channel.
struct Operation<T: RuntimeType + 'static> {
    operation: IAsyncOperation<T>,
    answer: Receiver<Result<i32, HRESULT>>,
}

impl<T: RuntimeType + 'static> Operation<T> {
    /// Sets the completion handler, which runs at once if the operation has already ended.
    /// Without a handler nothing could hear the answer, so the operation is cancelled.
    fn new(operation: IAsyncOperation<T>, raw: fn(T) -> i32) -> Option<Self> {
        let (sender, answer) = mpsc::channel();
        let handler = operation.when(move |result| {
            // The waiter may have given up and gone: nobody needs the answer then.
            let _ = sender.send(result.map(raw).map_err(|error| error.code()));
        });
        match handler {
            Ok(()) => Some(Self { operation, answer }),
            Err(_) => {
                let _ = operation.Cancel();
                None
            }
        }
    }
}

impl<T: RuntimeType + 'static> Pending for Operation<T> {
    fn wait(&self, timeout: Duration) -> Option<Result<i32, HRESULT>> {
        match self.answer.recv_timeout(timeout) {
            Ok(answer) => Some(answer),
            Err(RecvTimeoutError::Timeout) => None,
            // The handler was released without being called: no answer will come.
            Err(RecvTimeoutError::Disconnected) => Some(Err(E_FAIL)),
        }
    }

    fn cancel(&self) {
        // Fails only for an operation that has already ended.
        let _ = self.operation.Cancel();
    }
}

/// Waits for an operation that has started, or [`Asked::Failed`] when nothing could hear it.
fn wait_started<T: RuntimeType + 'static>(
    operation: IAsyncOperation<T>,
    raw: fn(T) -> i32,
    cancel: &CancellationToken,
    limit: Option<Duration>,
) -> Asked {
    match Operation::new(operation, raw) {
        Some(operation) => wait_for(&operation, cancel, limit, TIMING),
        None => Asked::Failed,
    }
}

/// The system's Windows Hello.
pub struct SystemHello;

impl Hello for SystemHello {
    fn availability(&self, cancel: &CancellationToken) -> Asked {
        match UserConsentVerifier::CheckAvailabilityAsync() {
            Ok(operation) => wait_started(
                operation,
                |availability: UserConsentVerifierAvailability| availability.0,
                cancel,
                Some(AVAILABILITY_LIMIT),
            ),
            Err(_) => Asked::NotStarted,
        }
    }

    fn verify(&self, window: isize, message: &str, cancel: &CancellationToken) -> Asked {
        let started =
            factory::<UserConsentVerifier, IUserConsentVerifierInterop>().and_then(|interop| {
                // SAFETY: `window` is the handle of the app's own top-level window, which lives
                // as long as the app does; the call only reads it (a handle that no longer names
                // a window is refused with an error, not undefined behaviour). `message` is a
                // valid HSTRING that outlives the call, and the generic parameter is the
                // interface the call is documented to return
                // (`IAsyncOperation<UserConsentVerificationResult>`), whose IID it passes.
                unsafe {
                    interop.RequestVerificationForWindowAsync::<
                        IAsyncOperation<UserConsentVerificationResult>,
                    >(HWND(window as *mut _), &HSTRING::from(message))
                }
            });
        match started {
            Ok(operation) => wait_started(
                operation,
                |result: UserConsentVerificationResult| result.0,
                cancel,
                None,
            ),
            Err(_) => Asked::NotStarted,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;

    use windows_future::AsyncStatus;

    use super::*;

    /// Short enough for a quick test, long enough to tell "waited for the grace" apart.
    const FAST: Timing = Timing {
        tick: Duration::from_millis(5),
        grace: Duration::from_millis(300),
    };

    const VERIFIED: i32 = UserConsentVerificationResult::Verified.0;
    const CANCELED: i32 = UserConsentVerificationResult::Canceled.0;

    /// An operation that answers what the test sends, and on `cancel` answers `on_cancel` (as
    /// the OS answers a cancelled prompt) if there is one.
    struct FakeOperation {
        answer: Receiver<Result<i32, HRESULT>>,
        sender: Sender<Result<i32, HRESULT>>,
        on_cancel: Option<Result<i32, HRESULT>>,
        cancels: AtomicUsize,
    }

    impl FakeOperation {
        fn new(on_cancel: Option<Result<i32, HRESULT>>) -> Self {
            let (sender, answer) = mpsc::channel();
            Self {
                answer,
                sender,
                on_cancel,
                cancels: AtomicUsize::new(0),
            }
        }

        fn answer_later(&self, after: Duration, answer: Result<i32, HRESULT>) {
            let sender = self.sender.clone();
            std::thread::spawn(move || {
                std::thread::sleep(after);
                let _ = sender.send(answer);
            });
        }

        fn cancels(&self) -> usize {
            self.cancels.load(Ordering::SeqCst)
        }
    }

    impl Pending for FakeOperation {
        fn wait(&self, timeout: Duration) -> Option<Result<i32, HRESULT>> {
            self.answer.recv_timeout(timeout).ok()
        }

        fn cancel(&self) {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            if let Some(answer) = self.on_cancel {
                let _ = self.sender.send(answer);
            }
        }
    }

    fn trip_after(after: Duration) -> CancellationToken {
        let token = CancellationToken::new();
        let tripped = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            tripped.cancel();
        });
        token
    }

    #[test]
    fn an_answer_is_the_operation_s_and_nothing_is_cancelled() {
        for answer in [Ok(VERIFIED), Ok(CANCELED), Ok(42)] {
            let operation = FakeOperation::new(None);
            operation.answer_later(Duration::from_millis(30), answer);
            let asked = wait_for(&operation, &CancellationToken::new(), None, FAST);
            assert_eq!(asked, Asked::Answered(answer.unwrap()));
            assert_eq!(operation.cancels(), 0);
        }
    }

    #[test]
    fn an_operation_that_fails_is_failed() {
        let operation = FakeOperation::new(None);
        operation.answer_later(Duration::ZERO, Err(E_FAIL));
        assert_eq!(
            wait_for(&operation, &CancellationToken::new(), None, FAST),
            Asked::Failed
        );
        assert_eq!(operation.cancels(), 0);
    }

    /// The OS ends a cancelled prompt at once: the wait answers then, not after the grace.
    #[test]
    fn a_tripped_token_cancels_the_operation_through_the_os() {
        let operation = FakeOperation::new(Some(Ok(CANCELED)));
        let token = trip_after(Duration::from_millis(30));
        let started = Instant::now();
        assert_eq!(wait_for(&operation, &token, None, FAST), Asked::Cancelled);
        assert_eq!(operation.cancels(), 1);
        assert!(started.elapsed() < FAST.grace, "{:?}", started.elapsed());
    }

    /// A cancel the operation ignores is waited for no longer than the grace.
    #[test]
    fn a_cancel_the_os_does_not_end_is_given_up_after_the_grace() {
        let operation = FakeOperation::new(None);
        let token = trip_after(Duration::from_millis(30));
        let started = Instant::now();
        assert_eq!(wait_for(&operation, &token, None, FAST), Asked::Cancelled);
        assert_eq!(operation.cancels(), 1);
        let waited = started.elapsed();
        assert!(waited >= FAST.grace, "{waited:?}");
        assert!(waited < FAST.grace * 3, "{waited:?}");
    }

    /// The prompt closed under the user's finger: the app's cancel, not a verification.
    #[test]
    fn a_verification_from_a_cancelled_prompt_is_the_app_s_cancel() {
        let operation = FakeOperation::new(Some(Ok(VERIFIED)));
        let token = trip_after(Duration::from_millis(30));
        assert_eq!(wait_for(&operation, &token, None, FAST), Asked::Cancelled);
        assert_eq!(operation.cancels(), 1);
    }

    /// An answer that arrives once the token has tripped is not taken either.
    #[test]
    fn an_answer_after_the_token_tripped_is_the_app_s_cancel() {
        /// Trips the token as it hands over the answer, as a lock-on-sleep in the same instant
        /// would.
        struct Racing {
            token: CancellationToken,
        }
        impl Pending for Racing {
            fn wait(&self, _timeout: Duration) -> Option<Result<i32, HRESULT>> {
                self.token.cancel();
                Some(Ok(VERIFIED))
            }
            fn cancel(&self) {
                panic!("the operation had ended");
            }
        }
        let token = CancellationToken::new();
        let racing = Racing {
            token: token.clone(),
        };
        assert_eq!(wait_for(&racing, &token, None, FAST), Asked::Cancelled);
    }

    #[test]
    fn a_token_tripped_before_the_wait_cancels_at_once() {
        let operation = FakeOperation::new(Some(Ok(CANCELED)));
        let token = CancellationToken::new();
        token.cancel();
        operation.answer_later(Duration::from_millis(50), Ok(VERIFIED));
        assert_eq!(wait_for(&operation, &token, None, FAST), Asked::Cancelled);
        assert_eq!(operation.cancels(), 1);
    }

    #[test]
    fn an_operation_silent_past_its_limit_is_cancelled_and_failed() {
        let operation = FakeOperation::new(None);
        let started = Instant::now();
        let limit = Duration::from_millis(50);
        let asked = wait_for(&operation, &CancellationToken::new(), Some(limit), FAST);
        assert_eq!(asked, Asked::Failed);
        assert_eq!(operation.cancels(), 1);
        assert!(started.elapsed() >= limit);
    }

    #[test]
    fn an_answer_within_the_limit_is_taken() {
        let operation = FakeOperation::new(None);
        operation.answer_later(Duration::from_millis(10), Ok(0));
        let asked = wait_for(
            &operation,
            &CancellationToken::new(),
            Some(Duration::from_secs(5)),
            FAST,
        );
        assert_eq!(asked, Asked::Answered(0));
        assert_eq!(operation.cancels(), 0);
    }

    // The WinRT side, with operations windows-future runs on the Windows thread pool: no
    // prompt, but the real completion handler, channel and `Cancel`.

    /// An operation that answers `result` once the returned sender is dropped.
    fn held(
        result: ::windows::core::Result<UserConsentVerificationResult>,
    ) -> (IAsyncOperation<UserConsentVerificationResult>, Sender<()>) {
        let (release, released) = mpsc::channel::<()>();
        let operation = IAsyncOperation::spawn(move || {
            // Err once the sender is dropped.
            let _ = released.recv();
            result
        });
        (operation, release)
    }

    fn result_raw(result: UserConsentVerificationResult) -> i32 {
        result.0
    }

    #[test]
    fn a_winrt_operation_s_answer_reaches_the_wait() {
        let result = UserConsentVerificationResult::Verified;
        let (operation, release) = held(Ok(result));
        let operation = Operation::new(operation, result_raw).expect("a handler");
        drop(release);
        assert_eq!(
            wait_for(&operation, &CancellationToken::new(), None, FAST),
            Asked::Answered(result.0)
        );
    }

    /// Ended before the handler is set: the handler runs at once.
    #[test]
    fn a_winrt_operation_that_has_ended_answers_at_once() {
        let result = UserConsentVerificationResult::RetriesExhausted;
        let (operation, release) = held(Ok(result));
        drop(release);
        while operation.Status().unwrap() == AsyncStatus::Started {
            std::thread::sleep(Duration::from_millis(5));
        }
        let operation = Operation::new(operation, result_raw).expect("a handler");
        assert_eq!(operation.wait(Duration::ZERO), Some(Ok(result.0)));
    }

    #[test]
    fn a_failed_winrt_operation_is_failed() {
        let (operation, release) = held(Err(E_FAIL.into()));
        let operation = Operation::new(operation, result_raw).expect("a handler");
        drop(release);
        assert_eq!(
            wait_for(&operation, &CancellationToken::new(), None, FAST),
            Asked::Failed
        );
    }

    /// windows-future's own operations ignore `Cancel`: the wait still ends after the grace.
    #[test]
    fn a_winrt_operation_is_cancelled_when_the_token_trips() {
        let (operation, release) = held(Ok(UserConsentVerificationResult::Verified));
        let operation = Operation::new(operation, result_raw).expect("a handler");
        let token = trip_after(Duration::from_millis(30));
        assert_eq!(wait_for(&operation, &token, None, FAST), Asked::Cancelled);
        drop(release);
    }

    /// A WinRT operation takes one completion handler: a second cannot hear it, so there is
    /// nothing to wait for.
    #[test]
    fn a_second_completion_handler_is_refused() {
        let (operation, release) = held(Ok(UserConsentVerificationResult::Verified));
        let first = Operation::new(operation.clone(), result_raw).expect("a handler");
        assert!(Operation::new(operation, result_raw).is_none());
        drop(release);
        assert_eq!(first.wait(Duration::from_secs(5)), Some(Ok(VERIFIED)));
    }
}
