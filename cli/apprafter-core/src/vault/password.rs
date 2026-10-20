// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A new lock password's rules (owner decision O2, 2026-10-13; T62). There is no minimum
//! length: below [`RECOMMENDED_PASSWORD_CHARS`] the client warns and the person decides (the
//! CLI's `Use it anyway? [y/N]`, the desktop's warning under the field). A password is refused
//! only when it is empty, when it is longer than [`MAX_PASSWORD_BYTES`] — a bound on what is
//! hashed, not a strength rule — or when it has the shape of a recovery key: one prompt takes
//! either secret and tries recovery-key-shaped input as a recovery key first, so such a
//! password could leave a store that only its recovery key opens (critic G4). No composition
//! rules and no Unicode normalisation: Argon2id hashes the bytes as typed.

use super::error::PasswordProblem;
use super::recovery::looks_like_recovery_key;
use crate::context::SecretString;

/// Below this many Unicode scalar values a new lock password is short: anyone with a copy of
/// the vault file can guess it offline, where Argon2id only slows each guess (NIST SP
/// 800-63B-4 sets 15 for single-factor passwords). A warning, never a refusal.
pub const RECOMMENDED_PASSWORD_CHARS: usize = 15;
/// The longest lock password, in bytes.
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// A new lock password the vault accepts, and its length in Unicode scalar values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptedPassword {
    chars: usize,
}

impl AcceptedPassword {
    pub fn chars(&self) -> usize {
        self.chars
    }

    /// Shorter than [`RECOMMENDED_PASSWORD_CHARS`]: the client warns, the person decides.
    pub fn is_short(&self) -> bool {
        self.chars < RECOMMENDED_PASSWORD_CHARS
    }
}

/// O2's rules, in this order: empty, too long, recovery-key shaped.
pub fn check_new_password(password: &SecretString) -> Result<AcceptedPassword, PasswordProblem> {
    let text = password.expose();
    if text.is_empty() {
        return Err(PasswordProblem::Empty);
    }
    if text.len() > MAX_PASSWORD_BYTES {
        return Err(PasswordProblem::TooLong {
            max: MAX_PASSWORD_BYTES,
        });
    }
    if looks_like_recovery_key(text) {
        return Err(PasswordProblem::RecoveryKeyShape);
    }
    Ok(AcceptedPassword {
        chars: text.chars().count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str) -> Result<AcceptedPassword, PasswordProblem> {
        check_new_password(&SecretString::new(text))
    }

    #[test]
    fn empty_is_refused_and_every_other_length_accepted() {
        assert_eq!(check(""), Err(PasswordProblem::Empty));
        let one = check("x").unwrap();
        assert_eq!((one.chars(), one.is_short()), (1, true));
        assert_eq!(check("   ").unwrap().chars(), 3);
    }

    #[test]
    fn short_means_under_fifteen_unicode_scalar_values() {
        assert!(check(&"a".repeat(14)).unwrap().is_short());
        assert!(!check(&"a".repeat(15)).unwrap().is_short());
        // 8 characters in 16 bytes: short by characters, not by bytes.
        let accented = check("éèêëàâäô").unwrap();
        assert_eq!((accented.chars(), accented.is_short()), (8, true));
        // 15 characters in 60 bytes.
        let keys = check(&"🔑".repeat(15)).unwrap();
        assert_eq!((keys.chars(), keys.is_short()), (15, false));
    }

    #[test]
    fn at_most_1024_bytes_are_hashed() {
        assert!(check(&"a".repeat(1024)).is_ok());
        assert_eq!(
            check(&"a".repeat(1025)),
            Err(PasswordProblem::TooLong { max: 1024 })
        );
        assert!(check(&"é".repeat(512)).is_ok());
        assert_eq!(
            check(&"é".repeat(513)),
            Err(PasswordProblem::TooLong { max: 1024 })
        );
    }

    #[test]
    fn a_recovery_key_shaped_password_is_refused() {
        let fruit = [
            "apple", "grape", "lemon", "melon", "peach", "berry", "guava", "mango", "olive",
            "onion", "bread", "pea",
        ]
        .join("-");
        for shaped in [
            "APPRAFTER-RECOVERY-KEY-1\nQQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK".to_string(),
            "QQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK".to_string(),
            "apprafter-recovery-key-1 and then anything".to_string(),
            "a".repeat(58),
            fruit,
        ] {
            assert_eq!(check(&shaped), Err(PasswordProblem::RecoveryKeyShape), "{shaped}");
        }
        for fine in [
            "a".repeat(57),
            "a".repeat(59),
            format!("{}!", "a".repeat(57)),
            "correct horse battery staple".to_string(),
        ] {
            assert!(check(&fine).is_ok(), "{fine}");
        }
    }

    #[test]
    fn the_rules_apply_in_their_order() {
        let long_and_shaped = format!("apprafter-recovery-key-1{}", "q".repeat(1001));
        assert_eq!(long_and_shaped.len(), 1025);
        assert_eq!(
            check(&long_and_shaped),
            Err(PasswordProblem::TooLong { max: 1024 })
        );
    }
}
