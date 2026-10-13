// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The holder's OS calls, one module per OS (`std` + `libc` + `windows-sys`; no `nix`,
//! `rustix` or `interprocess`, holder §7). `current` is the running OS's.

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(windows)]
pub(crate) mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux as current;
#[cfg(target_os = "macos")]
pub(crate) use macos as current;
#[cfg(windows)]
pub(crate) use windows as current;

/// A clock since boot, as a `Duration`.
#[cfg(unix)]
pub(crate) fn clock(id: libc::clockid_t) -> std::time::Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec; the clocks used here always exist.
    unsafe { libc::clock_gettime(id, &mut ts) };
    std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}
