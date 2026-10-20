// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Shared by the integration tests that `mod common;` it; each test crate uses part of it.
//! `spawn` is the one way to run the CLI, on every OS, and `mask` normalises a run's output on
//! every OS; the golden harness and its fake `kubectl` and `ssh` (`fake_kube`, `fake_ssh`) are
//! Unix-only, as `golden.rs`, `golden_doctor.rs` and `golden_reads.rs` are, and `terminal` is a
//! conversation at a pseudo-terminal (Unix).
#![allow(dead_code)]
#[cfg(unix)]
pub mod fake_kube;
#[cfg(unix)]
pub mod fake_ssh;
#[cfg(unix)]
pub mod golden;
pub mod mask;
pub mod spawn;
pub mod stand_in;
#[cfg(unix)]
pub mod terminal;
#[cfg(unix)]
pub mod transcript;
