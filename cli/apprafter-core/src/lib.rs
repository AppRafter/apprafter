// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The AppRafter CLI domain, shared by the `apprafter` CLI and AppRafter
//! Desktop (ADR 0067).
//!
//! Rules every module here follows:
//!
//! - it never writes to stdout or stderr: progress and captured tool output
//!   go through a [`Reporter`];
//! - it never ends the process: failures are [`CoreError`] values;
//! - it never reads the environment: inputs arrive in a [`Context`], built
//!   by the client from an [`EnvSource`] it supplies (the CLI from its whole
//!   environment, the desktop from an allow-list of it under a
//!   [`DesktopPolicy`]);
//! - it never prompts: a mutation returns a [`Plan`] for the client to
//!   confirm, then executes it;
//! - cancellation is a [`CancellationToken`] per operation, never a process
//!   signal handler.
//!
//! `tests/guards.rs` enforces the first three on the parsed source, not its
//! text, so production code after a test module, a renamed import
//! (`use std::env::var as v`) and a call inside a macro's arguments are all
//! seen. It fails on a print macro (`print!` … `dbg!`, any delimiter),
//! `std::process::{exit, abort}`, `std::io::{stdout, stderr, stdin}`, any
//! `std::env` read or write, any `dirs::*` call, and any call to a
//! lower-crate function that reads the environment or touches the terminal
//! on the core's behalf (the credential resolvers, `default_config_root`,
//! `default_age_key_path`, `logging::init`, `KubectlCli` / `HelmCli`). The
//! one sanctioned exception is `cli_core::target::config_root_from_override`,
//! and only inside [`Context::from_cli_env`] and [`Context::from_desktop_env`]:
//! its fallback reads the platform config directory, so both clients open the
//! same default target store. For the last two rules it keeps the crates
//! they would need out of the core's dependencies — no prompt, progress,
//! table, colour or signal crate. And it holds env reads in the crates below
//! the core, and in the CLI, to a count that only goes down.

pub mod cancel;
pub mod context;
pub mod env;
pub mod error;
pub mod op;
pub mod report;
pub mod target_ref;

pub use cancel::{CancellationToken, Cancelled, Registration};
pub use context::{CliOverrides, Context, DesktopPolicy, SecretString};
pub use env::{EnvSource, MapEnv};
pub use error::{CoreError, CoreResult, UiError};
pub use op::{Outcome, Plan, PlanClass, PlannedChange};
pub use report::{CollectReporter, Event, NullReporter, Reporter, Stream};
pub use target_ref::{ActivePointerChange, TargetRef};
