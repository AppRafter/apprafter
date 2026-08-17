// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Placeholder entry point.
//!
//! The `generate` and `check` subcommands land in a later task; this
//! exists so the `[[bin]]` target declared in `Cargo.toml` builds and
//! the library half of the crate can be tested in the meantime.

fn main() {
    eprintln!("docsgen: usage: docsgen <generate|check> (not implemented yet)");
    std::process::exit(2);
}
