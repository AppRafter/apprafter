// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared by the integration tests that `mod common;` it; each test crate uses part of it.
//! The golden harness is Unix-only, as `golden.rs` and `golden_doctor.rs` are.
#![allow(dead_code)]
#[cfg(unix)]
pub mod golden;
pub mod stand_in;
