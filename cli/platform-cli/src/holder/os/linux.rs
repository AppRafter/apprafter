// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Linux calls of the holder (D.4a T52, T56).

use std::time::Duration;

/// Counts suspend: the window's clock. `Instant` reads `CLOCK_MONOTONIC`, which does not
/// (holder finding 5), so a lid closed for the night would leave a window its minutes.
pub(crate) const WITH_SLEEP: libc::clockid_t = libc::CLOCK_BOOTTIME;
/// Stops in suspend: the sleep detector's second clock (O4).
pub(crate) const AWAKE: libc::clockid_t = libc::CLOCK_MONOTONIC;

pub(crate) fn clock_with_sleep() -> Duration {
    super::clock(WITH_SLEEP)
}

pub(crate) fn clock_awake() -> Duration {
    super::clock(AWAKE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_clocks_are_the_ones_that_count_suspend() {
        assert_eq!(
            (WITH_SLEEP, AWAKE),
            (libc::CLOCK_BOOTTIME, libc::CLOCK_MONOTONIC)
        );
    }
}
