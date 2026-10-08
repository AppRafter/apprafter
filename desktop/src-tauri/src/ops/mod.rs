// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Operations as the shell runs them: core events in, [`OpEvent`]s out.
//!
//! [`text`] turns a tool's raw output bytes into text, [`reporter`] coalesces that text and
//! converts every core event, [`replay`] keeps a bounded history so a page that reloads
//! or re-attaches can catch up, and [`manager`] holds plans and runs operations.
//!
//! [`OpEvent`]: apprafter_desktop_ipc::OpEvent

pub mod manager;
pub mod replay;
pub mod reporter;
pub mod text;

pub use manager::{EventSink, Executor, OperationManager, PlanParts, PLAN_TTL_MS};
pub use replay::ReplayBuffer;
pub use reporter::OpReporter;
pub use text::Utf8Stream;

/// Milliseconds since the Unix epoch, injected so tests drive time by hand.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[cfg(test)]
pub(crate) mod test_clock {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::Clock;

    /// A clock that only moves when a test moves it.
    #[derive(Debug, Default)]
    pub(crate) struct ManualClock(AtomicU64);

    impl ManualClock {
        pub(crate) fn at(ms: u64) -> Self {
            Self(AtomicU64::new(ms))
        }

        pub(crate) fn set(&self, ms: u64) {
            self.0.store(ms, Ordering::SeqCst);
        }

        pub(crate) fn advance(&self, ms: u64) {
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl Clock for ManualClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }
}
