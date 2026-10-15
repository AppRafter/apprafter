// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The points of a `state.json` write a test may stop at (WI-487).
//!
//! Every writer of a target's `state.json` does its slow work first, then takes the store's
//! write lock and, under it, reads the file again, writes the fields it owns into that fresh
//! copy and saves it ([`State::update`](crate::State::update)). A test shows that no writer
//! loses another's fields by running a second writer exactly at one of these points — before
//! the lock ([`WritePoint::BeforeLock`]), or under it, the file just read again
//! ([`WritePoint::Reread`]) — never by timing. A writer started on another thread meanwhile
//! says, at [`WritePoint::LockWait`], that it waits for the lock.
//!
//! In a shipped build [`reached`] does nothing: the hook ([`testing`]) exists only under the
//! `test-support` feature, which only `[dev-dependencies]` enable, and in this crate's own
//! tests.

/// Where a `state.json` writer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePoint {
    /// The writer has read the state for its own decisions and done its slow work (a
    /// provider's calls); it is about to take the store lock for its final read → merge → save.
    BeforeLock,
    /// Under the store lock: the state has just been read again, and is about to be changed
    /// and saved.
    Reread,
    /// Another process (or thread) holds the store lock: the writer is about to wait for it.
    LockWait,
}

/// A writer has reached `point`: in a test that armed a hook on this thread, the hook runs
/// here; everywhere else nothing happens.
#[inline]
pub fn reached(point: WritePoint) {
    #[cfg(any(test, feature = "test-support"))]
    testing::fire(point);
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = point;
}

/// The hook a test arms (never in a shipped build).
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use std::cell::RefCell;
    use std::marker::PhantomData;

    use super::WritePoint;

    type Hook = Box<dyn FnMut(WritePoint)>;

    thread_local! {
        /// The hook armed on this thread, if any.
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// While the returned guard lives, `hook` runs at every [`WritePoint`] a writer on this
    /// thread reaches. A writer the hook itself runs on this thread does not call it again: the
    /// hook is taken out while it runs. One hook per thread.
    pub fn arm(hook: impl FnMut(WritePoint) + 'static) -> Armed {
        HOOK.with(|slot| {
            let previous = slot.borrow_mut().replace(Box::new(hook));
            assert!(
                previous.is_none(),
                "a write-point hook is already armed on this thread"
            );
        });
        Armed {
            _this_thread: PhantomData,
        }
    }

    /// The hook of [`arm`], disarmed when this drops. Not `Send`: it belongs to its thread.
    #[must_use = "the hook is armed only while this value is alive"]
    pub struct Armed {
        _this_thread: PhantomData<*const ()>,
    }

    impl Drop for Armed {
        fn drop(&mut self) {
            HOOK.with(|slot| slot.borrow_mut().take());
        }
    }

    pub(super) fn fire(point: WritePoint) {
        let Some(mut hook) = HOOK.with(|slot| slot.borrow_mut().take()) else {
            return;
        };
        hook(point);
        HOOK.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(hook);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn an_armed_hook_sees_each_point_and_not_the_points_it_reaches_itself() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&seen);
        let armed = testing::arm(move |point| {
            log.borrow_mut().push(point);
            // A writer the hook runs reaches its own points: not seen again.
            reached(WritePoint::Reread);
        });
        reached(WritePoint::BeforeLock);
        reached(WritePoint::Reread);
        drop(armed);
        reached(WritePoint::BeforeLock);
        assert_eq!(
            *seen.borrow(),
            vec![WritePoint::BeforeLock, WritePoint::Reread]
        );
    }

    #[test]
    fn a_hook_is_this_threads_alone() {
        let seen = Rc::new(RefCell::new(0));
        let log = Rc::clone(&seen);
        let _armed = testing::arm(move |_| *log.borrow_mut() += 1);
        std::thread::spawn(|| reached(WritePoint::Reread))
            .join()
            .unwrap();
        assert_eq!(*seen.borrow(), 0);
        reached(WritePoint::Reread);
        assert_eq!(*seen.borrow(), 1);
    }
}
