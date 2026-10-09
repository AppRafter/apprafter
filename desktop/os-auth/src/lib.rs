// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Device-owner authentication through the operating system (ADR 0067 §5): Windows Hello with
//! the Windows credential dialog as its fallback, macOS LocalAuthentication, and Linux polkit
//! with PAM as its fallback.
//!
//! Tauri-free, so the shell depends on this crate and never the other way round. Every backend
//! answers with the one [`AuthOutcome`](apprafter_desktop_ipc::AuthOutcome); [`outcome`] holds
//! how each OS's result becomes one, as pure functions that compile and are tested on every OS.
//! [`session`] watches the OS's lock and sleep signals, which lock the app.

mod action;
pub mod outcome;

#[cfg(target_os = "linux")]
pub mod linux;

// Everywhere in tests: all but the framework's calls is tested on every OS, against a fake.
#[cfg(any(target_os = "macos", test))]
pub mod macos;

pub mod session;

#[cfg(windows)]
pub mod windows;

pub use action::Action;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub use session::watch;
pub use session::{SessionEvent, SessionWatch};

#[cfg(target_os = "linux")]
pub use linux::OsAuthenticator;
#[cfg(target_os = "macos")]
pub use macos::OsAuthenticator;
// `self::`: a bare `windows` here would also name the `windows` crate.
#[cfg(windows)]
pub use self::windows::OsAuthenticator;
