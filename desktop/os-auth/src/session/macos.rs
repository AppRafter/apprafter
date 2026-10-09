// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! macOS: `NSWorkspace`'s sleep notifications and the distributed screen-lock notification.
//!
//! - `NSWorkspaceWillSleepNotification` and `NSWorkspaceScreensDidSleepNotification`, on
//!   `NSWorkspace.sharedWorkspace.notificationCenter`, through block observers. AppKit posts
//!   them on the main thread, and the block, which only hands the signal to the watch's
//!   dispatcher, runs there.
//! - `com.apple.screenIsLocked`, a distributed notification, on Core Foundation's distributed
//!   centre with `CFNotificationSuspensionBehaviorDeliverImmediately`: the application object
//!   suspends distributed delivery while the app is not the active one, and the screen locks
//!   exactly when another app (loginwindow) takes over, so a coalesced notification would
//!   arrive only once the owner is back. Its callback gets no closure, only the observer
//!   value it was registered with: a key into [`DISTRIBUTED`], never a pointer, so a callback
//!   already under way when the watch stops finds no entry rather than freed memory.
//!
//! Both centres deliver through the main thread's run loop, which the app's event loop runs.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{
    NSWorkspace, NSWorkspaceScreensDidSleepNotification, NSWorkspaceWillSleepNotification,
};
use objc2_core_foundation::{
    CFDictionary, CFNotificationCenter, CFNotificationName, CFNotificationSuspensionBehavior,
    CFString,
};
use objc2_foundation::{NSNotification, NSNotificationName};

use super::{Emitter, Listening, Signal};

/// The distributed notification the screen's lock posts.
const SCREEN_IS_LOCKED: &str = "com.apple.screenIsLocked";

/// The emitters of the running watches' distributed observers, by observer key.
static DISTRIBUTED: Mutex<Vec<(usize, Emitter)>> = Mutex::new(Vec::new());
/// The next observer key; 0 is never one (a NULL observer means "every observer").
static NEXT_KEY: AtomicUsize = AtomicUsize::new(1);

/// A block observer's token, which only `removeObserver:` ever reads.
struct Observer(Retained<ProtocolObject<dyn NSObjectProtocol>>);

// SAFETY: the token is an opaque object the notification centre created; this crate never
// calls a method on it, it only passes it back to `-[NSNotificationCenter removeObserver:]`,
// which may be called from any thread (objc2 marks NSNotificationCenter `Send + Sync`), and
// then releases it, and retain/release of an Objective-C object are thread-safe.
unsafe impl Send for Observer {}

/// The running observers: dropping them removes every one.
pub(super) struct Sources {
    workspace: Vec<Observer>,
    /// The distributed observer's key, when it was registered.
    distributed: Option<usize>,
}

impl Drop for Sources {
    fn drop(&mut self) {
        if !self.workspace.is_empty() {
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            for observer in self.workspace.drain(..) {
                // SAFETY: the token `addObserverForName:object:queue:usingBlock:` returned for
                // this same centre, removed once; the centre then releases the block.
                unsafe { center.removeObserver(observer.0.as_ref()) };
            }
        }
        if let Some(key) = self.distributed.take() {
            DISTRIBUTED
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|(registered, _)| *registered != key);
            if let Some(center) = CFNotificationCenter::distributed_center() {
                // SAFETY: `key` is the observer value this watch registered with; Core
                // Foundation compares it and never dereferences it.
                unsafe { center.remove_every_observer(key as *const c_void) };
            }
        }
    }
}

pub(super) fn start(emitter: Emitter) -> Sources {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let mut workspace = Vec::new();
    // SAFETY: AppKit's notification names, immutable NSString constants that live as long as
    // the process.
    let names: [(&NSNotificationName, Signal); 2] = unsafe {
        [
            (NSWorkspaceWillSleepNotification, Signal::WillSleep),
            (
                NSWorkspaceScreensDidSleepNotification,
                Signal::ScreensDidSleep,
            ),
        ]
    };
    for (name, signal) in names {
        let emitter = emitter.clone();
        let block = RcBlock::new(move |_: NonNull<NSNotification>| emitter.signal(signal));
        // SAFETY: `name` is a valid notification name; no object and no queue, so the block
        // runs on the thread that posts (the main thread). The block is sendable, as the
        // method requires: it captures only an Emitter, which is `Send + Sync`, and the
        // centre copies it and keeps it until the observer is removed.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
        };
        workspace.push(Observer(token));
    }
    let distributed = distributed(&emitter);
    emitter.ready(Listening {
        lock: distributed.is_some(),
        sleep: !workspace.is_empty(),
    });
    Sources {
        workspace,
        distributed,
    }
}

/// Registers for [`SCREEN_IS_LOCKED`] with immediate delivery; the observer's key, or `None`,
/// logged.
fn distributed(emitter: &Emitter) -> Option<usize> {
    let Some(center) = CFNotificationCenter::distributed_center() else {
        tracing::info!("no distributed notification centre: the screen's locks are not watched");
        return None;
    };
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    DISTRIBUTED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((key, emitter.clone()));
    let name = CFString::from_static_str(SCREEN_IS_LOCKED);
    // SAFETY: `key` is an opaque observer value Core Foundation only compares and passes back
    // to `screen_is_locked`, which has the callback's signature; `name` is a valid CFString the
    // centre copies; no object (NULL) matches any sender.
    unsafe {
        center.add_observer(
            key as *const c_void,
            Some(screen_is_locked),
            Some(&name),
            std::ptr::null(),
            CFNotificationSuspensionBehavior::DeliverImmediately,
        );
    }
    Some(key)
}

/// The distributed centre's callback for [`SCREEN_IS_LOCKED`]: `observer` is a watch's key.
/// Never panics: it is called from Core Foundation.
unsafe extern "C-unwind" fn screen_is_locked(
    _center: *mut CFNotificationCenter,
    observer: *mut c_void,
    _name: *const CFNotificationName,
    _object: *const c_void,
    _user_info: *const CFDictionary,
) {
    let key = observer as usize;
    let registered = DISTRIBUTED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, emitter)) = registered.iter().find(|(registered, _)| *registered == key) {
        emitter.signal(Signal::ScreenIsLocked);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::super::{SessionEvent, SessionWatch};
    use super::*;

    /// No main run loop runs in a test binary, so nothing is delivered: what is tested is that
    /// the observers register and go again, and the callback's routing by key.
    #[test]
    fn a_watch_registers_its_observers_and_removes_them() {
        let (keys, key) = mpsc::channel();
        let watch = SessionWatch::start(
            |_| {},
            move |emitter| {
                let sources = start(emitter);
                assert_eq!(sources.workspace.len(), 2, "both workspace observers");
                keys.send(sources.distributed).unwrap();
                Box::new(sources)
            },
        );
        assert_eq!(
            watch.listening(Duration::from_secs(10)),
            Some(Listening {
                lock: true,
                sleep: true
            }),
            "both centres registered"
        );
        let key = key.recv().unwrap().expect("the distributed observer");
        assert!(DISTRIBUTED.lock().unwrap().iter().any(|(k, _)| *k == key));
        drop(watch);
        assert!(
            !DISTRIBUTED.lock().unwrap().iter().any(|(k, _)| *k == key),
            "removed with the watch"
        );
    }

    #[test]
    fn the_distributed_callback_reaches_its_own_watch_only() {
        let (events, received) = mpsc::channel();
        let (emitters, emitter) = mpsc::channel();
        let watch = SessionWatch::start(
            move |event| {
                let _ = events.send(event);
            },
            move |emitter: Emitter| {
                emitters.send(emitter).unwrap();
                Box::new(())
            },
        );
        let emitter: Emitter = emitter.recv().unwrap();
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        DISTRIBUTED.lock().unwrap().push((key, emitter));
        let call = |key: usize| {
            // SAFETY: the callback reads none of its pointers but `observer`, as a key.
            unsafe {
                screen_is_locked(
                    std::ptr::null_mut(),
                    key as *mut c_void,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                );
            }
        };
        call(key);
        assert_eq!(
            received.recv_timeout(Duration::from_secs(10)),
            Ok(SessionEvent::Locked)
        );
        call(0);
        call(usize::MAX);
        assert!(received.recv_timeout(Duration::from_millis(200)).is_err());
        DISTRIBUTED.lock().unwrap().retain(|(k, _)| *k != key);
        drop(watch);
    }
}
