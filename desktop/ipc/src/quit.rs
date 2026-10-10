// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A quit as the webview sees it. A quit cancels the running operations and waits for them to
//! stop before the app exits; meanwhile the window would show a page whose every command is
//! refused, so the quit says so, and the page shows that it is stopping them.

use serde::Serialize;

/// The event a quit emits when it begins with operations running, with [`Quitting`]. A quit
/// with nothing running exits at once and emits nothing.
pub const QUITTING: &str = "quitting";

/// What [`QUITTING`] carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Quitting {
    /// The operations the quit cancelled and waits for.
    pub running: u32,
    /// The longest it waits for them before it exits anyway, in milliseconds.
    pub wait_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quitting_has_the_wire_shape() {
        let quitting = Quitting {
            running: 2,
            wait_ms: 15_000,
        };
        assert_eq!(
            serde_json::to_string(&quitting).unwrap(),
            r#"{"running":2,"waitMs":15000}"#
        );
        assert_eq!(QUITTING, "quitting");
    }
}
