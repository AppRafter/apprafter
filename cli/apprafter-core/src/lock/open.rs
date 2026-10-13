// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Choosing a lock password and typing a recovery key (D.4a §3.9; O2, T25, T62): what the core
//! refuses, and why. `open`, `check_password` and `set_unlock_timeout` arrive with d4a-d.
//!
//! The password rules are the vault's, which `create` and `add_slot` enforce (c1's Deviation
//! 6): [`PasswordProblem`] (empty, too long, shaped like a recovery key — no minimum length,
//! O2), [`MAX_PASSWORD_BYTES`] and [`RECOMMENDED_PASSWORD_CHARS`] are re-exported from there,
//! one copy of each, and this module adds only their wire name and their words.

pub use crate::vault::{PasswordProblem, MAX_PASSWORD_BYTES, RECOMMENDED_PASSWORD_CHARS};

use crate::vault::RecoveryKeyError;
use serde::Serialize;

/// Advice on a new lock password, never a refusal: below [`RECOMMENDED_PASSWORD_CHARS`] Unicode
/// scalar values (D.4a O2: anyone who copies the vault guesses it offline, where Argon2id only
/// slows each guess) the CLI asks `Use it anyway? [y/N]`, the desktop shows its warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PasswordAdvice {
    /// `chars` Unicode scalar values, fewer than `recommended`.
    Short { chars: u32, recommended: u32 },
}

/// The wire name and the words of the vault's refusal of a new lock password; an inherent
/// `impl` of the vault's type, in the same crate.
impl PasswordProblem {
    /// Its name on the wire (`fields.reason`).
    pub fn as_str(self) -> &'static str {
        match self {
            PasswordProblem::Empty => "empty",
            PasswordProblem::TooLong { .. } => "too_long",
            PasswordProblem::RecoveryKeyShape => "recovery_key_shape",
        }
    }

    /// The refusal's message, which names no password.
    pub fn message(self) -> String {
        match self {
            PasswordProblem::Empty => "a lock password cannot be empty".to_string(),
            PasswordProblem::TooLong { max } => {
                format!("a lock password can be at most {max} bytes long")
            }
            PasswordProblem::RecoveryKeyShape => {
                "that password has the shape of a recovery key, and a lock password must not"
                    .to_string()
            }
        }
    }
}

/// Why a typed recovery key was refused: the vault's parse and comparison errors, and a lock-on
/// plan executed before its key was typed back (T25, critic C5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryKeyProblem {
    Empty,
    WrongPrefix,
    /// A character recovery keys do not use. `position` counts the 58 characters after the
    /// prefix from 0, dashes and spaces left out (c1's count; words show `position + 1`);
    /// `hint` is the look-alike a person most likely meant (`l`, `0` or `8`), never the
    /// character typed (T24).
    InvalidChar {
        position: usize,
        hint: Option<char>,
    },
    Checksum {
        group: Option<u8>,
    },
    WrongLength,
    Mismatch,
    NotConfirmed,
}

impl From<RecoveryKeyError> for RecoveryKeyProblem {
    fn from(e: RecoveryKeyError) -> Self {
        match e {
            RecoveryKeyError::Empty => RecoveryKeyProblem::Empty,
            RecoveryKeyError::WrongPrefix => RecoveryKeyProblem::WrongPrefix,
            RecoveryKeyError::InvalidChar {
                position,
                lookalike,
            } => RecoveryKeyProblem::InvalidChar {
                position,
                hint: lookalike.and_then(meant),
            },
            RecoveryKeyError::Checksum { group } => RecoveryKeyProblem::Checksum { group },
            RecoveryKeyError::WrongLength => RecoveryKeyProblem::WrongLength,
            RecoveryKeyError::Mismatch => RecoveryKeyProblem::Mismatch,
        }
    }
}

/// The character a person most likely meant when they typed `typed`, one of the four Bech32
/// leaves out — c1's `lookalike`, which is `Some` only for `1`, `b`, `i` and `o`, lower-cased
/// (its Deviation 7); the same pairs as c1's `lookalike_hint`.
fn meant(typed: char) -> Option<char> {
    match typed {
        '1' | 'i' => Some('l'),
        'o' => Some('0'),
        'b' => Some('8'),
        _ => None,
    }
}

impl RecoveryKeyProblem {
    /// Its name on the wire (`fields.reason`).
    pub fn as_str(self) -> &'static str {
        match self {
            RecoveryKeyProblem::Empty => "empty",
            RecoveryKeyProblem::WrongPrefix => "wrong_prefix",
            RecoveryKeyProblem::InvalidChar { .. } => "invalid_char",
            RecoveryKeyProblem::Checksum { .. } => "checksum",
            RecoveryKeyProblem::WrongLength => "wrong_length",
            RecoveryKeyProblem::Mismatch => "mismatch",
            RecoveryKeyProblem::NotConfirmed => "not_confirmed",
        }
    }

    /// The refusal's message, which never repeats what was typed.
    pub fn message(self) -> String {
        match self {
            RecoveryKeyProblem::Empty => "no recovery key was typed".to_string(),
            RecoveryKeyProblem::WrongPrefix => {
                "that does not start like a recovery key".to_string()
            }
            RecoveryKeyProblem::InvalidChar { position, .. } => format!(
                "character {} of the recovery key is not one a recovery key uses",
                position + 1
            ),
            RecoveryKeyProblem::Checksum { group: Some(group) } => {
                format!("the recovery key is mistyped near group {group}")
            }
            RecoveryKeyProblem::Checksum { group: None } => {
                "the recovery key is mistyped".to_string()
            }
            RecoveryKeyProblem::WrongLength => {
                "that is too short or too long for a recovery key".to_string()
            }
            RecoveryKeyProblem::Mismatch => "that is not the recovery key shown".to_string(),
            RecoveryKeyProblem::NotConfirmed => {
                "the recovery key was not typed back, so nothing was locked".to_string()
            }
        }
    }
}
