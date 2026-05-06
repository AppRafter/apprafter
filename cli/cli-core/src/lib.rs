// SPDX-License-Identifier: FSL-1.1-MIT
//! Shared utilities for `platform-cli` and its sister crates.

pub mod error;
pub mod logging;
pub mod tier;

pub use error::{CliError, Result};
pub use tier::Tier;
