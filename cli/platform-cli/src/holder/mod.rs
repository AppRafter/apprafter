// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The per-terminal unlock holder (ADR 0068 §4; D.4a §3.10, T47–T59).
//!
//! A person who unlocks a locked target store in a terminal starts a holder: this binary, run
//! as `apprafter __holder`, which keeps the store's data key in one locked page and serves the
//! commands started from that terminal — read-only, one item per request — until its window
//! ends. The data key reaches it only over the init pipe its starter hands it, and never leaves
//! it (T48).

pub mod binding;
pub mod client;
pub mod endpoint;
pub mod harden;
pub mod memory;
pub(crate) mod os;
pub mod protocol;
pub(crate) mod serve;
mod server;
pub mod spawn;
pub mod watch;
pub mod window;

/// The argument that makes this binary a holder: matched in `main` before `apprafter::run()` —
/// no clap parse, no startup checks, no logging, nothing docsgen projects (T47; HG2).
pub const HOLDER_ARG: &str = "__holder";

/// What a holder started at a terminal says before it exits 2 (T47). Spelled here, beside
/// [`HOLDER_ARG`], because HG2 allows the text `__holder` in this file only: each OS's server
/// prints this constant and names neither (critic-d4a-e finding 2).
pub(crate) const NOT_A_COMMAND: &str =
    "error: `__holder` is not a command: apprafter starts it itself";

/// The holder process. Never returns.
pub fn run_holder() -> ! {
    std::process::exit(server::run())
}

/// This user's SID as text, for tests that name a Windows holder's pipe.
#[cfg(windows)]
#[doc(hidden)]
pub fn os_user_sid() -> std::io::Result<String> {
    os::windows::user_sid_string()
}
