// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The types AppRafter Desktop sends between Rust and the webview (ADR 0067 §3).
//!
//! Tauri-free on purpose: the TypeScript bindings are exported from this crate's tests,
//! which then run on any OS without WebKitGTK.

/// What every failed command returns: the core's serialisable error projection.
pub use apprafter_core::UiError;
