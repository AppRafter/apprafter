// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The app lock as the webview sees it; Rust decides it (design spec §4.5).

use serde::Serialize;

use crate::PlanView;

/// The event every lock transition emits to the webview, with the new [`LockState`].
pub const LOCK_CHANGED: &str = "lock-changed";

/// Why the app is locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LockReason {
    /// Locked at start (`lockOnStart`).
    Startup,
    /// No activity for the `autoLock` time.
    Idle,
    /// The OS session locked or went to sleep (`lockOnSleep`).
    OsSession,
    /// The owner chose Lock now.
    Manual,
    /// The target store's secrets were locked from a terminal while the app held no key
    /// (D.4a T74).
    SecretsLocked,
}

/// The lock as the webview renders it: `lock_status` and every `lock-changed` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LockState {
    pub locked: bool,
    /// Set while locked, `None` while unlocked.
    pub reason: Option<LockReason>,
    /// When the current state began, in milliseconds since the Unix epoch.
    pub since_ms: u64,
    /// The idle time before an automatic lock; `None` when the lock is off or set to never.
    pub auto_lock_minutes: Option<u32>,
    /// The transition this state follows: 0 at start, one more on every lock and every unlock.
    /// The page keeps the state with the highest `seq` it has received, never orders by
    /// `since_ms`: that is wall-clock time, which can step back, and two transitions can share a
    /// millisecond. Two states with one `seq` describe the same transition.
    pub seq: u64,
}

/// What `op_plan_lock_on` answers (D.4a §3.14): the plan to confirm, and the store's new
/// recovery key to show once. The key lives in the page only while the Protect dialog shows or
/// checks it, and comes back across IPC only as the person types it (`op_confirm_recovery_key`).
/// No `Clone`, and a `Debug` that names no key.
#[derive(Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LockOnPlan {
    pub plan: PlanView,
    /// Bech32m text, shown once.
    pub recovery_key: String,
}

impl std::fmt::Debug for LockOnPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockOnPlan")
            .field("plan", &self.plan)
            .field("recovery_key", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Gesture, OpId};
    use apprafter_core::PlanClass;

    #[test]
    fn a_locked_state_has_the_wire_shape() {
        let state = LockState {
            locked: true,
            reason: Some(LockReason::Idle),
            since_ms: 12,
            auto_lock_minutes: Some(10),
            seq: 3,
        };
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            r#"{"locked":true,"reason":"idle","sinceMs":12,"autoLockMinutes":10,"seq":3}"#
        );
    }

    #[test]
    fn an_unlocked_state_carries_nulls_not_missing_keys() {
        let state = LockState {
            locked: false,
            reason: None,
            since_ms: 0,
            auto_lock_minutes: None,
            seq: 0,
        };
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            r#"{"locked":false,"reason":null,"sinceMs":0,"autoLockMinutes":null,"seq":0}"#
        );
    }

    #[test]
    fn every_lock_reason_is_snake_case() {
        for (reason, wire) in [
            (LockReason::Startup, r#""startup""#),
            (LockReason::Idle, r#""idle""#),
            (LockReason::OsSession, r#""os_session""#),
            (LockReason::Manual, r#""manual""#),
            (LockReason::SecretsLocked, r#""secrets_locked""#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), wire);
        }
    }

    #[test]
    fn a_lock_on_plan_carries_its_key_once_and_debug_never_shows_it() {
        let key = "test-recovery-key-text".to_string();
        let plan = LockOnPlan {
            plan: PlanView {
                op_id: OpId(9),
                class: PlanClass::Bounded,
                title: "Protect secrets".into(),
                changes: Vec::new(),
                target: None,
                expires_at_ms: 600_000,
                gesture: Gesture::None,
            },
            recovery_key: key.clone(),
        };
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(v["recoveryKey"], key.as_str());
        assert_eq!(v["plan"]["opId"], 9);
        assert_eq!(v["plan"]["gesture"], "none");
        let shown = format!("{plan:?}");
        assert!(
            !shown.contains(&key) && shown.contains("<redacted>"),
            "{shown}"
        );
    }

    /// Compiles only while `LockOnPlan` is not `Clone`: with a `Clone` impl the call is
    /// ambiguous between the two impls (the `static_assertions::assert_not_impl_any` trick).
    #[test]
    fn a_lock_on_plan_is_never_cloned() {
        trait AmbiguousIfClone<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfClone<()> for T {}
        struct IsClone;
        // `Clone` implies `Sized`, so this impl takes no `?Sized` (clippy::needless_maybe_sized).
        impl<T: Clone> AmbiguousIfClone<IsClone> for T {}
        <LockOnPlan as AmbiguousIfClone<_>>::check();
    }

    #[cfg(feature = "ts")]
    #[test]
    fn the_lock_on_plan_declares_its_plan_and_key() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::new().with_large_int("number");
        let decl = LockOnPlan::decl(&cfg);
        assert!(
            decl.contains("plan: PlanView") && decl.contains("recoveryKey: string"),
            "{decl}"
        );
        let reason = LockReason::decl(&cfg);
        assert!(reason.contains("\"secrets_locked\""), "{reason}");
    }
}
