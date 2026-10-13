// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The target-store lock (ADR 0068; D.4a §3.9, §3.13): turning it on and off, opening a locked
//! store with one prompt's input, and the one report every surface shows. The flows arrive with
//! d4a-d and d4a-e; D.4a c2 adds first the types the errors and the desktop's IPC need.

pub mod open;

pub use open::{
    PasswordProblem, RecoveryKeyProblem, MAX_PASSWORD_BYTES, RECOMMENDED_PASSWORD_CHARS,
};
