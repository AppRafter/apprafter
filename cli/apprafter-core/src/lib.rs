// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The AppRafter CLI domain, shared by the `apprafter` CLI and AppRafter
//! Desktop (ADR 0067).
//!
//! Rules every module here follows — `tests/guards.rs` enforces the first
//! three by scanning the source:
//!
//! - it never writes to stdout or stderr: progress and captured tool output
//!   go through a [`Reporter`];
//! - it never ends the process: failures are [`CoreError`] values;
//! - it never reads the environment: inputs arrive in a [`Context`], built
//!   by the client (the CLI from its environment via an [`EnvSource`], the
//!   desktop from its settings);
//! - it never prompts: a mutation returns a [`Plan`] for the client to
//!   confirm, then executes it;
//! - cancellation is a [`CancellationToken`] per operation, never a process
//!   signal handler.

pub mod env;

pub use env::{EnvSource, MapEnv};
