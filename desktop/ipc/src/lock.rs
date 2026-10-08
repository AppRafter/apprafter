// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The app lock as the webview sees it; Rust decides it (design spec §4.5).

use serde::Serialize;

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locked_state_has_the_wire_shape() {
        let state = LockState {
            locked: true,
            reason: Some(LockReason::Idle),
            since_ms: 12,
            auto_lock_minutes: Some(10),
        };
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            r#"{"locked":true,"reason":"idle","sinceMs":12,"autoLockMinutes":10}"#
        );
    }

    #[test]
    fn an_unlocked_state_carries_nulls_not_missing_keys() {
        let state = LockState {
            locked: false,
            reason: None,
            since_ms: 0,
            auto_lock_minutes: None,
        };
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            r#"{"locked":false,"reason":null,"sinceMs":0,"autoLockMinutes":null}"#
        );
    }

    #[test]
    fn every_lock_reason_is_snake_case() {
        for (reason, wire) in [
            (LockReason::Startup, r#""startup""#),
            (LockReason::Idle, r#""idle""#),
            (LockReason::OsSession, r#""os_session""#),
            (LockReason::Manual, r#""manual""#),
        ] {
            assert_eq!(serde_json::to_string(&reason).unwrap(), wire);
        }
    }
}
