// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Entry point for the `apprafter` CLI.
//!
//! Everything from argv onward lives in the `apprafter` lib target
//! so `docsgen` can project the clap tree; this binary is a thin
//! wrapper over [`apprafter::run`] — and, as `apprafter __holder`,
//! the per-terminal unlock holder (`apprafter::holder`).

use miette::Result;

fn main() -> Result<()> {
    // The unlock holder (D.4a T47): this binary started by itself as `apprafter __holder`, before
    // clap, the startup checks, logging or anything that reads the environment.
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(apprafter::holder::HOLDER_ARG))
    {
        apprafter::holder::run_holder();
    }
    apprafter::run()
}
