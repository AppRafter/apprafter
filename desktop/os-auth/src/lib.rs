// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Device-owner authentication through the operating system (ADR 0067 §5): Windows Hello with
//! the Windows credential dialog as its fallback, macOS LocalAuthentication, and Linux polkit
//! with PAM as its fallback.
//!
//! Tauri-free, so the shell depends on this crate and never the other way round. Every backend
//! answers with the one [`AuthOutcome`](apprafter_desktop_ipc::AuthOutcome); [`outcome`] holds
//! how each OS's result becomes one, as pure functions that compile and are tested on every OS.

pub mod outcome;

#[cfg(target_os = "linux")]
pub mod linux;
