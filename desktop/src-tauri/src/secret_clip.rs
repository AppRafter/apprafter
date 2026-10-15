// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The recovery key's Copy (D.4a O12, T81; ADR 0068 §3). Rust copies the key it holds — the
//! page never has it to copy — marked for the OS's clipboard managers to leave out of their
//! history: `exclude_from_history` on Linux (`x-kde-passwordManagerHint`) and macOS
//! (`org.nspasteboard.ConcealedType`), `exclude_from_monitoring` on Windows, which keeps it out
//! of the history and the cloud clipboard both (arboard's own advice: not combined with the
//! other two). It clears the clipboard after [`CLEAR_AFTER`] when the clipboard still holds the
//! key — never what the person copied since — and on every lock ([`SecretClipboard::clear_soon`],
//! off the lock's thread) and the quit ([`SecretClipboard::clear_now`]). The plugin's
//! `writeText`, a page's Copy, sets no hint and never carries the key. "I saved it" clears it at
//! once ([`SecretClipboard::clear_now`], O12). A lock never waits on the clipboard: a copy holds
//! its state across the OS's call, which can wait on a hung display server, and the lock only
//! moves an atomic counter and wakes the timer (critic B finding 18).
//!
//! On macOS the app writes the general pasteboard itself ([`MacClipboard`]): for this Mac only,
//! so Universal Clipboard (Handoff) does not carry the key to the person's other devices (ADR
//! 0068 §3's cloud clipboard sync), and marked concealed as arboard marks it. arboard's own
//! `set` clears the pasteboard with no options, which would let Handoff carry it (critic B
//! finding 14).
//!
//! Only the app builds the real clipboard ([`SystemClipboard`], in `lib.rs`): it reaches the
//! owner's display session (tests/plugin_guard.rs). Tests use a stand-in backend.

use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use apprafter_core::SecretString;
use zeroize::Zeroizing;

use crate::errors::DesktopError;
use crate::ops::{Clock, Stamp};

/// How long the key stays on the clipboard.
pub const CLEAR_AFTER: Duration = Duration::from_secs(60);

/// How long the quit waits for the clipboard's clear (it runs on a thread of its own: a
/// clipboard call can hang on a display server, and the quit runs on the main thread).
pub const CLEAR_ON_QUIT: Duration = Duration::from_millis(500);

/// The timer's slice: a sleep the monotonic wait does not count shows on the wall clock within
/// this.
const SLICE_MS: u64 = 250;

/// The OS clipboard, as the key's Copy needs it.
pub trait ClipboardBackend: Send {
    /// Put `text` on the clipboard, marked for clipboard managers to leave out.
    fn set_concealed(&mut self, text: &str) -> Result<(), String>;
    /// The clipboard's text now: `Ok(None)` when it holds no text (something else the person
    /// copied since), `Err` when it cannot be read.
    fn text(&mut self) -> Result<Option<Zeroizing<String>>, String>;
    fn clear(&mut self) -> Result<(), String>;
}

/// Opens the clipboard on first use: the app's arboard, a test's stand-in.
pub type ClipboardMaker = Arc<dyn Fn() -> Result<Box<dyn ClipboardBackend>, String> + Send + Sync>;

/// The key's Copy (module docs).
pub struct SecretClipboard {
    maker: Option<ClipboardMaker>,
    /// The backend and what was copied. Held across the OS's clipboard calls, which can wait on
    /// a display server: a lock never takes it ([`clear_soon`](Self::clear_soon)).
    state: Mutex<State>,
    /// Moved on by every lock, outside `state`: a copy's timer that sees it move clears at once.
    locks: AtomicU64,
    /// Wakes the timers: a lock takes only this small mutex, never `state`.
    wake: Mutex<()>,
    woken: Condvar,
    clear_after: Duration,
    /// The shell's clock: the 60 s are measured as every deadline in the shell is
    /// (`Stamp::elapsed_ms`, the wall clock too), so a sleep counts.
    clock: Arc<dyn Clock>,
}

#[derive(Default)]
struct State {
    /// Kept while a copy is up: on X11 and Wayland the app serves what it put there while it
    /// lives; dropped once cleared.
    backend: Option<Box<dyn ClipboardBackend>>,
    /// What was copied, to tell it from what the person copied since; zeroed on drop.
    copied: Option<Zeroizing<String>>,
    /// Each copy's number: a timer clears only its own copy.
    generation: u64,
}

impl SecretClipboard {
    /// `None`: this app has no clipboard to copy to, and Copy says so.
    pub fn new(maker: Option<ClipboardMaker>, clock: Arc<dyn Clock>) -> Arc<Self> {
        Self::with_clear_after(maker, CLEAR_AFTER, clock)
    }

    pub(crate) fn with_clear_after(
        maker: Option<ClipboardMaker>,
        after: Duration,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new(SecretClipboard {
            maker,
            state: Mutex::default(),
            locks: AtomicU64::new(0),
            wake: Mutex::new(()),
            woken: Condvar::new(),
            clear_after: after,
            clock,
        })
    }

    /// How many locks there were: a command reads it before anything, and passes it to
    /// [`copy_since`](Self::copy_since).
    pub fn locks(&self) -> u64 {
        self.locks.load(SeqCst)
    }

    /// Copy `key`, counting locks from now ([`copy_since`](Self::copy_since)).
    pub fn copy(self: &Arc<Self>, key: &SecretString) -> Result<(), DesktopError> {
        self.copy_since(key, self.locks())
    }

    /// Copy `key`; the clipboard is cleared after [`CLEAR_AFTER`] if it still holds it, or at
    /// once after a lock since `locks` was read — one that came before the copy began, or while
    /// the OS call waited, included (the Rust verifier's finding 8).
    pub fn copy_since(
        self: &Arc<Self>,
        key: &SecretString,
        locks: u64,
    ) -> Result<(), DesktopError> {
        let maker = self.maker.as_ref().ok_or_else(|| {
            DesktopError::Internal("this computer gives AppRafter no clipboard to copy to".into())
        })?;
        let (generation, start) = {
            let mut state = self.lock();
            if state.backend.is_none() {
                state.backend = Some(maker().map_err(|e| {
                    DesktopError::Internal(format!("the clipboard could not be opened: {e}"))
                })?);
            }
            // The 60 s count from here: a sleep while the OS takes the key, or before the timer's
            // thread starts, counts.
            let start = Stamp::now(&*self.clock);
            if let Some(backend) = state.backend.as_mut() {
                if let Err(e) = backend.set_concealed(key.expose()) {
                    // The text can be there without its hint — Windows' exclusion and macOS's
                    // marker are written after it — so the clipboard is cleared, not left so.
                    if let Err(cleared) = backend.clear() {
                        tracing::warn!(
                            "the clipboard was not cleared after a failed copy: {cleared}"
                        );
                    }
                    return Err(DesktopError::Internal(format!(
                        "the recovery key was not copied: {e}"
                    )));
                }
            }
            state.copied = Some(Zeroizing::new(key.expose().to_string()));
            state.generation += 1;
            (state.generation, start)
        };
        let me = self.clone();
        thread::Builder::new()
            .name("secret-clip".into())
            .spawn(move || me.clear_when_due(generation, locks, start))
            .map(drop)
            .map_err(|e| {
                self.clear_now();
                DesktopError::Internal(format!("no thread to clear the clipboard on ({e})"))
            })
    }

    /// A lock: the key comes off the clipboard now, off this thread. The lock's hook runs under
    /// the lock machine's lock, and a clipboard call can wait on a hung display server, so this
    /// takes nothing a copy holds (critic B finding 18).
    pub fn clear_soon(&self) {
        self.locks.fetch_add(1, SeqCst);
        let _wake = self.wake.lock().unwrap_or_else(PoisonError::into_inner);
        self.woken.notify_all();
    }

    /// "I saved it" (O12): the key comes off the clipboard now, on this thread (a command's, on
    /// the blocking pool), and no timer clears anything after.
    pub fn clear_now(&self) {
        let mut state = self.lock();
        clear_if_ours(&mut state);
        state.generation += 1;
    }

    /// The quit: [`clear_now`](Self::clear_now) on a thread of its own, waited for at most
    /// `bound` — a clipboard call can hang on a display server, and the quit runs on the main
    /// thread (the Rust verifier's finding 7). Whether it was done in time.
    pub fn clear_within(self: &Arc<Self>, bound: Duration) -> bool {
        let (done, finished) = mpsc::channel();
        let me = self.clone();
        let spawned = thread::Builder::new()
            .name("secret-clip-quit".into())
            .spawn(move || {
                me.clear_now();
                let _ = done.send(());
            });
        spawned.is_ok() && finished.recv_timeout(bound).is_ok()
    }

    fn clear_when_due(&self, generation: u64, locks: u64, start: Stamp) {
        let after = u64::try_from(self.clear_after.as_millis()).unwrap_or(u64::MAX);
        let mut wake = self.wake.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if self.locks.load(SeqCst) != locks {
                break;
            }
            let passed = start.elapsed_ms(&*self.clock);
            if passed >= after {
                break;
            }
            let slice = Duration::from_millis((after - passed).min(SLICE_MS));
            wake = self
                .woken
                .wait_timeout(wake, slice)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        drop(wake);
        let mut state = self.lock();
        if state.generation == generation {
            clear_if_ours(&mut state);
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Clears the clipboard when it still holds what was copied; the backend goes either way.
fn clear_if_ours(state: &mut State) {
    let Some(copied) = state.copied.take() else {
        return;
    };
    if let Some(backend) = state.backend.as_mut() {
        let ours = match backend.text() {
            Ok(now) => now.is_some_and(|now| now.as_str() == copied.as_str()),
            // Not read back: cleared rather than the key left there (the Rust verifier's
            // finding 9).
            Err(e) => {
                tracing::warn!("the clipboard was not read back ({e}): clearing it");
                true
            }
        };
        if ours {
            if let Err(e) = backend.clear() {
                tracing::warn!("the clipboard was not cleared: {e}");
            }
        }
    }
    state.backend = None;
}

/// The OS clipboard the app's Copy uses: arboard's on Linux and Windows, the general pasteboard
/// for this Mac only on macOS. Built only by the app (`lib.rs`; tests/plugin_guard.rs).
pub struct SystemClipboard;

impl SystemClipboard {
    /// What the app gives its shell: opens arboard's clipboard on the first Copy.
    #[cfg(not(target_os = "macos"))]
    pub fn maker() -> ClipboardMaker {
        Arc::new(|| {
            arboard::Clipboard::new()
                .map(|clipboard| Box::new(ArboardClipboard(clipboard)) as Box<dyn ClipboardBackend>)
                .map_err(|e| e.to_string())
        })
    }

    /// What the app gives its shell: the general pasteboard, written for this Mac only.
    #[cfg(target_os = "macos")]
    pub fn maker() -> ClipboardMaker {
        Arc::new(|| Ok(Box::new(MacClipboard) as Box<dyn ClipboardBackend>))
    }
}

/// Linux and Windows: arboard 3.6, the version the clipboard plugin links.
#[cfg(not(target_os = "macos"))]
struct ArboardClipboard(arboard::Clipboard);

#[cfg(not(target_os = "macos"))]
impl ClipboardBackend for ArboardClipboard {
    fn set_concealed(&mut self, text: &str) -> Result<(), String> {
        let set = self.0.set();
        #[cfg(target_os = "linux")]
        let set = {
            use arboard::SetExtLinux;
            set.exclude_from_history()
        };
        #[cfg(windows)]
        let set = {
            use arboard::SetExtWindows;
            set.exclude_from_monitoring()
        };
        set.text(text).map_err(|e| e.to_string())
    }

    fn text(&mut self) -> Result<Option<Zeroizing<String>>, String> {
        match self.0.get_text() {
            Ok(text) => Ok(Some(Zeroizing::new(text))),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn clear(&mut self) -> Result<(), String> {
        self.0.clear().map_err(|e| e.to_string())
    }
}

/// macOS: the general pasteboard, written for this Mac only — `CurrentHostOnly` keeps it off
/// Universal Clipboard (Handoff) — then marked `org.nspasteboard.ConcealedType`, as arboard
/// marks it, for clipboard history apps (macOS 10.12+; the app's minimum is 13).
#[cfg(target_os = "macos")]
struct MacClipboard;

#[cfg(target_os = "macos")]
impl ClipboardBackend for MacClipboard {
    fn set_concealed(&mut self, text: &str) -> Result<(), String> {
        use objc2_app_kit::{NSPasteboard, NSPasteboardContentsOptions, NSPasteboardTypeString};
        use objc2_foundation::{ns_string, NSString};
        let board = NSPasteboard::generalPasteboard();
        board.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
        // SAFETY: AppKit's constant, read once.
        let kind = unsafe { NSPasteboardTypeString };
        if !board.setString_forType(&NSString::from_str(text), kind) {
            return Err("the pasteboard did not take the text".into());
        }
        if !board.setString_forType(ns_string!(""), ns_string!("org.nspasteboard.ConcealedType")) {
            return Err("the pasteboard did not take the concealed marker".into());
        }
        Ok(())
    }

    fn text(&mut self) -> Result<Option<Zeroizing<String>>, String> {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        // SAFETY: as above.
        let kind = unsafe { NSPasteboardTypeString };
        Ok(NSPasteboard::generalPasteboard()
            .stringForType(kind)
            .map(|text| Zeroizing::new(text.to_string())))
    }

    fn clear(&mut self) -> Result<(), String> {
        objc2_app_kit::NSPasteboard::generalPasteboard().clearContents();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use apprafter_core::SecretString;
    use zeroize::Zeroizing;

    use super::{ClipboardBackend, ClipboardMaker, SecretClipboard};
    use crate::ops::test_clock::ManualClock;
    use crate::ops::test_wait::within;
    use crate::ops::SystemClock;

    const LONG: Duration = Duration::from_secs(10);

    /// The OS clipboard, as a test holds it: its text, whether the last write was concealed,
    /// and how many times its text was read.
    #[derive(Clone, Default)]
    struct Board {
        text: Arc<Mutex<Option<String>>>,
        concealed: Arc<AtomicBool>,
        reads: Arc<AtomicUsize>,
    }

    impl ClipboardBackend for Board {
        fn set_concealed(&mut self, text: &str) -> Result<(), String> {
            *self.text.lock().unwrap() = Some(text.to_owned());
            self.concealed.store(true, SeqCst);
            Ok(())
        }
        fn text(&mut self) -> Result<Option<Zeroizing<String>>, String> {
            self.reads.fetch_add(1, SeqCst);
            Ok(self.text.lock().unwrap().clone().map(Zeroizing::new))
        }
        fn clear(&mut self) -> Result<(), String> {
            *self.text.lock().unwrap() = None;
            Ok(())
        }
    }

    fn clip(after: Duration) -> (Board, Arc<SecretClipboard>) {
        let board = Board::default();
        let maker: ClipboardMaker = {
            let board = board.clone();
            Arc::new(move || Ok(Box::new(board.clone()) as Box<dyn ClipboardBackend>))
        };
        (
            board,
            SecretClipboard::with_clear_after(Some(maker), after, Arc::new(SystemClock)),
        )
    }

    fn key() -> SecretString {
        SecretString::new("APPRAFTER-RECOVERY-KEY-1\nQQQSY-QCYQ5")
    }

    fn until(board: &Board, holds: Option<&str>) -> bool {
        let deadline = Instant::now() + LONG;
        while Instant::now() < deadline {
            if board.text.lock().unwrap().as_deref() == holds {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn the_key_is_copied_concealed_and_cleared_after_its_time() {
        let (board, clip) = clip(Duration::from_millis(200));
        clip.copy(&key()).unwrap();
        assert_eq!(board.text.lock().unwrap().as_deref(), Some(key().expose()));
        assert!(board.concealed.load(SeqCst));
        assert!(until(&board, None), "not cleared");
    }

    #[test]
    fn what_the_person_copied_since_is_left_alone() {
        let (board, clip) = clip(Duration::from_millis(200));
        clip.copy(&key()).unwrap();
        *board.text.lock().unwrap() = Some("a command".into());
        // The timer has looked, and decided: it reads the clipboard, and clears it or not,
        // under the state's lock (GOTCHA-332: after that, not after a moment).
        let deadline = Instant::now() + LONG;
        while board.reads.load(SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the timer never looked");
            thread::sleep(Duration::from_millis(5));
        }
        drop(clip.lock());
        assert_eq!(board.text.lock().unwrap().as_deref(), Some("a command"));
    }

    #[test]
    fn a_lock_clears_it_before_its_time_and_the_quit_at_once() {
        let (board, clip) = clip(Duration::from_secs(600));
        clip.copy(&key()).unwrap();
        clip.clear_soon();
        assert!(until(&board, None), "a lock did not clear it");
        clip.copy(&key()).unwrap();
        clip.clear_now();
        assert_eq!(*board.text.lock().unwrap(), None);
    }

    #[test]
    fn without_a_clipboard_nothing_is_copied() {
        let clip = SecretClipboard::new(None, Arc::new(SystemClock));
        assert!(clip.copy(&key()).is_err());
    }

    /// A clipboard stuck in a hung display server: it says when it is inside its call, and
    /// waits there until released.
    struct Stuck {
        board: Board,
        entered: Mutex<mpsc::Sender<()>>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl ClipboardBackend for Stuck {
        fn set_concealed(&mut self, text: &str) -> Result<(), String> {
            let _ = self.entered.lock().unwrap().send(());
            let (open, opened) = &*self.release;
            let mut now = open.lock().unwrap();
            while !*now {
                now = opened.wait(now).unwrap();
            }
            self.board.set_concealed(text)
        }
        fn text(&mut self) -> Result<Option<Zeroizing<String>>, String> {
            self.board.text()
        }
        fn clear(&mut self) -> Result<(), String> {
            self.board.clear()
        }
    }

    /// Critic B finding 18: the lock's hook never waits on the clipboard — a copy stuck in the
    /// OS's call holds it — and the key the copy then puts there is cleared at once.
    #[test]
    fn a_lock_never_waits_on_a_stuck_clipboard() {
        let board = Board::default();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let maker: ClipboardMaker = {
            let (board, release) = (board.clone(), release.clone());
            let entered = Mutex::new(entered_tx);
            Arc::new(move || {
                Ok(Box::new(Stuck {
                    board: board.clone(),
                    entered: Mutex::new(entered.lock().unwrap().clone()),
                    release: release.clone(),
                }) as Box<dyn ClipboardBackend>)
            })
        };
        let clip = SecretClipboard::with_clear_after(
            Some(maker),
            Duration::from_secs(600),
            Arc::new(SystemClock),
        );
        let copying = {
            let clip = clip.clone();
            thread::spawn(move || clip.copy(&key()))
        };
        entered_rx
            .recv_timeout(LONG)
            .expect("the copy never reached the clipboard");
        // The copy stays stuck until the lock has returned: it returned without the clipboard
        // (GOTCHA-332: `within` fails a lock that waits on it, whatever the clock says).
        {
            let clip = clip.clone();
            within("the lock's clear", move || clip.clear_soon());
        }
        {
            let (open, opened) = &*release;
            *open.lock().unwrap() = true;
            opened.notify_all();
        }
        copying.join().unwrap().unwrap();
        assert!(until(&board, None), "the key outlived the lock");
    }

    /// The Rust verifier's finding 8: a lock that lands between the command's start and the
    /// copy (the counter is read first) clears what the copy puts there at once.
    #[test]
    fn a_lock_before_the_copy_clears_it_at_once() {
        let (board, clip) = clip(Duration::from_secs(600));
        let locks = clip.locks();
        clip.clear_soon();
        clip.copy_since(&key(), locks).unwrap();
        assert!(
            until(&board, None),
            "a lock before the copy left the key there"
        );
    }

    /// The Rust verifier's finding 9: a clipboard that cannot be read back is cleared — the
    /// key is never left there because the check failed; one that holds something else (the
    /// person's own copy) is left alone.
    #[test]
    fn a_clipboard_that_cannot_be_read_back_is_cleared() {
        #[derive(Clone)]
        struct Unreadable(Board);
        impl ClipboardBackend for Unreadable {
            fn set_concealed(&mut self, text: &str) -> Result<(), String> {
                self.0.set_concealed(text)
            }
            fn text(&mut self) -> Result<Option<Zeroizing<String>>, String> {
                Err("the display server did not answer".into())
            }
            fn clear(&mut self) -> Result<(), String> {
                self.0.clear()
            }
        }
        let board = Board::default();
        let maker: ClipboardMaker = {
            let board = board.clone();
            Arc::new(move || Ok(Box::new(Unreadable(board.clone())) as Box<dyn ClipboardBackend>))
        };
        let clip = SecretClipboard::with_clear_after(
            Some(maker),
            Duration::from_millis(100),
            Arc::new(SystemClock),
        );
        clip.copy(&key()).unwrap();
        assert!(until(&board, None), "an unreadable clipboard kept the key");
    }

    /// The Rust verifier's finding 9: the 60 s count a sleep — measured as every deadline in
    /// the shell is (`elapsed_ms`: the wall clock too), so a key copied before a sleep is gone
    /// on the wake.
    #[test]
    fn the_sixty_seconds_count_a_sleep() {
        let board = Board::default();
        let maker: ClipboardMaker = {
            let board = board.clone();
            Arc::new(move || Ok(Box::new(board.clone()) as Box<dyn ClipboardBackend>))
        };
        let clock = Arc::new(ManualClock::at(1_700_000_000_000));
        let clip =
            SecretClipboard::with_clear_after(Some(maker), Duration::from_secs(60), clock.clone());
        clip.copy(&key()).unwrap();
        clock.sleep(61_000);
        assert!(
            until(&board, None),
            "the key outlived a sleep longer than its 60 s"
        );
    }

    /// Beyond the plan: the 60 s count from the copy itself — a sleep while the OS took the key
    /// counts, as one before the timer's thread started does (the timer once read its own start,
    /// which a late thread moved past such a sleep).
    #[test]
    fn the_sixty_seconds_count_from_the_copy() {
        struct SleepsInTheCall(Board, Arc<ManualClock>);
        impl ClipboardBackend for SleepsInTheCall {
            fn set_concealed(&mut self, text: &str) -> Result<(), String> {
                self.1.sleep(61_000);
                self.0.set_concealed(text)
            }
            fn text(&mut self) -> Result<Option<Zeroizing<String>>, String> {
                self.0.text()
            }
            fn clear(&mut self) -> Result<(), String> {
                self.0.clear()
            }
        }
        let board = Board::default();
        let clock = Arc::new(ManualClock::at(1_700_000_000_000));
        let maker: ClipboardMaker = {
            let (board, clock) = (board.clone(), clock.clone());
            Arc::new(move || {
                Ok(Box::new(SleepsInTheCall(board.clone(), clock.clone()))
                    as Box<dyn ClipboardBackend>)
            })
        };
        let clip = SecretClipboard::with_clear_after(Some(maker), Duration::from_secs(60), clock);
        clip.copy(&key()).unwrap();
        assert!(
            until(&board, None),
            "a sleep during the copy was not counted"
        );
    }

    /// Beyond the plan: a copy the OS took only half — the text, not its hint (Windows writes
    /// the exclusion, macOS the concealed marker, after the text) — is refused and cleared, never
    /// left on the clipboard unmarked and with no timer to clear it.
    #[test]
    fn a_copy_that_fails_half_way_is_cleared() {
        struct HintRefused(Board);
        impl ClipboardBackend for HintRefused {
            fn set_concealed(&mut self, text: &str) -> Result<(), String> {
                *self.0.text.lock().unwrap() = Some(text.to_owned());
                Err("the hint was not taken".into())
            }
            fn text(&mut self) -> Result<Option<Zeroizing<String>>, String> {
                self.0.text()
            }
            fn clear(&mut self) -> Result<(), String> {
                self.0.clear()
            }
        }
        let board = Board::default();
        let maker: ClipboardMaker = {
            let board = board.clone();
            Arc::new(move || Ok(Box::new(HintRefused(board.clone())) as Box<dyn ClipboardBackend>))
        };
        let clip = SecretClipboard::with_clear_after(
            Some(maker),
            Duration::from_secs(600),
            Arc::new(SystemClock),
        );
        let e = clip.copy(&key()).unwrap_err();
        assert!(!e.to_ui().message.contains("QQQSY"), "{:?}", e.to_ui());
        assert_eq!(
            *board.text.lock().unwrap(),
            None,
            "a half-made copy was left there"
        );
    }

    /// The Rust verifier's finding 7: the quit clears on a thread of its own and waits at most
    /// its bound — a stuck clipboard does not hold the main thread.
    #[test]
    fn the_quit_waits_at_most_its_bound() {
        let (board, clip) = clip(Duration::from_secs(600));
        clip.copy(&key()).unwrap();
        assert!(clip.clear_within(LONG));
        assert_eq!(*board.text.lock().unwrap(), None);
        // A copy stuck in the OS's call holds the clipboard: the quit gives up in time.
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let stuck_board = Board::default();
        let maker: ClipboardMaker = {
            let (board, release) = (stuck_board.clone(), release.clone());
            let entered = Mutex::new(entered_tx);
            Arc::new(move || {
                Ok(Box::new(Stuck {
                    board: board.clone(),
                    entered: Mutex::new(entered.lock().unwrap().clone()),
                    release: release.clone(),
                }) as Box<dyn ClipboardBackend>)
            })
        };
        let stuck = SecretClipboard::with_clear_after(
            Some(maker),
            Duration::from_secs(600),
            Arc::new(SystemClock),
        );
        let copying = {
            let stuck = stuck.clone();
            thread::spawn(move || stuck.copy(&key()))
        };
        entered_rx.recv_timeout(LONG).unwrap();
        // The copy stays stuck until the quit has answered: it can only have answered by
        // giving up, and `within` fails one that never does (GOTCHA-332: not a bound on the
        // clock).
        let done = {
            let stuck = stuck.clone();
            within("the quit's clear", move || {
                stuck.clear_within(Duration::from_millis(200))
            })
        };
        assert!(!done, "the quit waited out the stuck copy");
        {
            let (open, opened) = &*release;
            *open.lock().unwrap() = true;
            opened.notify_all();
        }
        copying.join().unwrap().unwrap();
    }
}
