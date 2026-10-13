// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The per-terminal unlock holder (ADR 0068 §4; D.4a §3.10, T47–T59).
//!
//! A person who unlocks a locked target store in a terminal starts a holder: this binary, run
//! as `apprafter __holder`, which keeps the store's data key in one locked page and serves the
//! commands started from that terminal — read-only, one item per request — until its window
//! ends. The data key reaches it only over the init pipe its starter hands it, and never leaves
//! it (T48).

pub mod binding;
pub mod endpoint;
pub mod protocol;
pub mod window;
