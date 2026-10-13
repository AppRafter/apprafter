// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared by the integration tests that `mod common;` it; each test crate uses part of it.
//! `spawn` is the one way to run the CLI, on every OS, and `mask` normalises a run's output on
//! every OS; the golden harness is Unix-only, as `golden.rs` and `golden_doctor.rs` are.
#![allow(dead_code)]
#[cfg(unix)]
pub mod golden;
pub mod mask;
pub mod spawn;
pub mod stand_in;
