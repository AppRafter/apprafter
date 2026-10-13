// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The recovery key (D.4a T24, T25; ADR 0068 §3): 32 random bytes that open the vault without
//! a key-derivation function, shown once as Bech32m text with the human-readable part
//! `apprafter-recovery-key-`:
//!
//! ```text
//! APPRAFTER-RECOVERY-KEY-1
//! QQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK
//! ```
//!
//! (the key of bytes `00 01 … 1f`). Typed back, any case, prefix optional, dashes and spaces
//! ignored; a checksum turns a typo into "mistyped near group N" and is never auto-corrected.

use std::fmt;

use bech32::primitives::decode::CheckedHrpstring;
use bech32::{Bech32m, Hrp};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

use super::error::{RecoveryKeyError, VaultResult};
use super::rng::VaultRng;
use crate::context::SecretString;

/// The human-readable part of a recovery key's Bech32m text.
pub const RECOVERY_KEY_HRP: &str = "apprafter-recovery-key-";
/// What a recovery key's text starts with, lower-cased: the HRP and Bech32's separator `1`.
const PREFIX: &str = "apprafter-recovery-key-1";
/// Data characters after the prefix: 52 for the 32 bytes, 6 for the checksum.
const DATA_CHARS: usize = 58;
/// Bech32's 32 characters: every lower-case letter and digit but `1`, `b`, `i` and `o`.
const CHARSET: &str = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
/// Characters per group in the shown text.
const GROUP: usize = 5;
/// The canonical text's length.
const TEXT_LEN: usize = PREFIX.len() + DATA_CHARS;

/// A recovery key. No `Clone`, no `Copy`, `..` in `Debug`, zeroed on drop, boxed so it never
/// moves.
pub struct RecoveryKey(Box<[u8; 32]>);

/// A recovery key the person retyped in full. Only [`RecoveryKey::confirm`] makes one, from
/// the key it consumes, so a confirmation is never paired with another key and neither client
/// can lock a store whose recovery key was not typed back (T25, critic C5). No `Clone`, `..` in
/// `Debug`; the key inside is zeroed on drop.
pub struct ConfirmedRecoveryKey(RecoveryKey);

fn hrp() -> Hrp {
    Hrp::parse(RECOVERY_KEY_HRP).expect("the recovery key's HRP is a valid Bech32 HRP")
}

impl RecoveryKey {
    /// 32 bytes from `rng`, written straight into the box.
    pub fn generate(rng: &mut dyn VaultRng) -> VaultResult<Self> {
        let mut key = Box::new([0u8; 32]);
        if let Err(e) = rng.fill(&mut key[..]) {
            key.zeroize();
            return Err(e);
        }
        Ok(RecoveryKey(key))
    }

    /// The key's bytes: a slot's key material (seal.rs).
    pub(crate) fn key_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The text shown once: `APPRAFTER-RECOVERY-KEY-1`, a line break, then eleven groups of
    /// five characters and one of three, joined by `-`.
    pub fn to_display(&self) -> SecretString {
        let canonical = self.canonical(Case::Upper);
        let data = &canonical[PREFIX.len()..];
        let mut shown = Zeroizing::new(String::with_capacity(
            PREFIX.len() + 1 + DATA_CHARS + DATA_CHARS / GROUP,
        ));
        shown.push_str(&canonical[..PREFIX.len()]);
        shown.push('\n');
        for (i, c) in data.chars().enumerate() {
            if i > 0 && i % GROUP == 0 {
                shown.push('-');
            }
            shown.push(c);
        }
        SecretString::from(shown)
    }

    fn canonical(&self, case: Case) -> Zeroizing<String> {
        // Pre-sized, so the encoder's writes never reallocate and leave a copy behind.
        let mut text = Zeroizing::new(String::with_capacity(TEXT_LEN));
        let written = match case {
            Case::Upper => {
                bech32::encode_upper_to_fmt::<Bech32m, String>(&mut *text, hrp(), &self.0[..])
            }
            Case::Lower => {
                bech32::encode_lower_to_fmt::<Bech32m, String>(&mut *text, hrp(), &self.0[..])
            }
        };
        written.expect("32 bytes always encode under the recovery key's HRP");
        text
    }

    /// Parses typed text (T24): trimmed, any case, the prefix optional, `-` and whitespace
    /// ignored; then 58 characters of the Bech32 charset and a valid Bech32m checksum over the
    /// one canonical spelling of 32 bytes.
    pub fn parse(input: &str) -> Result<Self, RecoveryKeyError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(RecoveryKeyError::Empty);
        }
        let lower = Zeroizing::new(trimmed.to_lowercase());
        let rest: &str = match lower.strip_prefix(PREFIX) {
            Some(rest) => rest,
            None if lower.starts_with("apprafter") || lower.starts_with("age-secret-key-1") => {
                return Err(RecoveryKeyError::WrongPrefix)
            }
            None => lower.as_str(),
        };
        let mut data = Zeroizing::new(String::with_capacity(DATA_CHARS));
        let mut count = 0usize;
        for c in rest.chars().filter(|c| *c != '-' && !c.is_whitespace()) {
            if !CHARSET.contains(c) {
                return Err(RecoveryKeyError::InvalidChar {
                    position: count,
                    lookalike: matches!(c, '1' | 'b' | 'i' | 'o').then_some(c),
                });
            }
            if count < DATA_CHARS {
                data.push(c);
            }
            count += 1;
        }
        if count != DATA_CHARS {
            return Err(RecoveryKeyError::WrongLength);
        }
        let mut full = Zeroizing::new(String::with_capacity(TEXT_LEN));
        full.push_str(PREFIX);
        full.push_str(&data);
        let checked = match CheckedHrpstring::new::<Bech32m>(&full) {
            Ok(checked) => checked,
            Err(_) => {
                return Err(RecoveryKeyError::Checksum {
                    group: locate_error(data.as_bytes()),
                })
            }
        };
        let mut key = Box::new([0u8; 32]);
        let mut n = 0;
        for byte in checked.byte_iter() {
            if n == key.len() {
                key.zeroize();
                return Err(RecoveryKeyError::WrongLength);
            }
            key[n] = byte;
            n += 1;
        }
        if n != key.len() {
            key.zeroize();
            return Err(RecoveryKeyError::WrongLength);
        }
        let key = RecoveryKey(key);
        // One text per key: padding bits that are not zero would spell the same bytes twice.
        if !bool::from(key.canonical(Case::Lower).as_bytes().ct_eq(full.as_bytes())) {
            return Err(RecoveryKeyError::Checksum { group: None });
        }
        Ok(key)
    }

    /// Consumes the key and checks that `typed` is it, retyped in full (O12, T25): it parses (a
    /// checksum failure names the group), and its bytes equal this key's, compared in constant
    /// time. A refusal hands the same key back with its error, for the next attempt.
    pub fn confirm(
        self,
        typed: &str,
    ) -> Result<ConfirmedRecoveryKey, (RecoveryKey, RecoveryKeyError)> {
        let typed = match RecoveryKey::parse(typed) {
            Ok(typed) => typed,
            Err(error) => return Err((self, error)),
        };
        if !bool::from(self.key_bytes()[..].ct_eq(&typed.key_bytes()[..])) {
            return Err((self, RecoveryKeyError::Mismatch));
        }
        Ok(ConfirmedRecoveryKey(self))
    }
}

#[derive(Clone, Copy)]
enum Case {
    Upper,
    Lower,
}

impl ConfirmedRecoveryKey {
    /// The confirmed key's text, shown again (the desktop's "Show it again" and Copy).
    pub fn to_display(&self) -> SecretString {
        self.0.to_display()
    }

    /// The key, for its slot: what `lock on` writes once the person has typed it back.
    pub fn into_key(self) -> RecoveryKey {
        self.0
    }
}

impl Drop for RecoveryKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryKey(..)")
    }
}

impl fmt::Debug for ConfirmedRecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfirmedRecoveryKey(..)")
    }
}

/// T62's shape rule: after trimming, the text starts (any case) with
/// `apprafter-recovery-key-1`, or, with `-` and whitespace removed, it is exactly 58
/// characters of the Bech32 charset plus `1`, `b`, `i` and `o` (every ASCII letter and digit).
/// One prompt takes either secret: input of this shape is tried as a recovery key first.
pub fn looks_like_recovery_key(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed
        .get(..PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PREFIX))
    {
        return true;
    }
    let mut count = 0usize;
    for c in trimmed.chars().filter(|c| *c != '-' && !c.is_whitespace()) {
        let c = c.to_ascii_lowercase();
        if !(CHARSET.contains(c) || matches!(c, '1' | 'b' | 'i' | 'o')) {
            return false;
        }
        count += 1;
    }
    count == DATA_CHARS
}

/// The 1-based group of a mistyped key's error: the one position where a substitution makes
/// the checksum valid, else the one adjacent swap that does, else none. It only names a group;
/// nothing is corrected.
fn locate_error(data: &[u8]) -> Option<u8> {
    let mut trial = Zeroizing::new(data.to_vec());
    let mut text = Zeroizing::new(String::with_capacity(TEXT_LEN));
    let mut valid = |candidate: &[u8]| {
        text.clear();
        text.push_str(PREFIX);
        text.push_str(std::str::from_utf8(candidate).expect("charset characters are ASCII"));
        CheckedHrpstring::new::<Bech32m>(&text).is_ok()
    };
    let mut substituted: Option<usize> = None;
    let mut several = false;
    for i in 0..trial.len() {
        let original = trial[i];
        for &c in CHARSET.as_bytes() {
            if c == original {
                continue;
            }
            trial[i] = c;
            if valid(&trial[..]) {
                several |= substituted.is_some_and(|j| j != i);
                substituted = Some(i);
            }
        }
        trial[i] = original;
    }
    if let Some(i) = substituted {
        return (!several).then(|| group_of(i));
    }
    let mut swapped: Option<usize> = None;
    for i in 0..trial.len().saturating_sub(1) {
        if trial[i] == trial[i + 1] {
            continue;
        }
        trial.swap(i, i + 1);
        if valid(&trial[..]) {
            several |= swapped.is_some();
            swapped = Some(i);
        }
        trial.swap(i, i + 1);
    }
    swapped.filter(|_| !several).map(group_of)
}

fn group_of(position: usize) -> u8 {
    (position / GROUP + 1) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::rng::ScriptedRandom;
    use crate::vault::testutil::{unhex, vector_path};
    use bech32::{Fe32, Fe32IterExt};
    use serde_json::Value;

    /// `tests/vault_vectors/v1/recovery-key.json`, written by the independent generator.
    fn vectors() -> Value {
        let bytes = std::fs::read(vector_path("recovery-key.json"))
            .expect("tests/vault_vectors/v1/recovery-key.json");
        serde_json::from_slice(&bytes).unwrap()
    }

    fn text<'a>(v: &'a Value, key: &str) -> &'a str {
        v[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key} is not a string"))
    }

    fn key_of(bytes: &[u8]) -> RecoveryKey {
        RecoveryKey::generate(&mut ScriptedRandom::new(vec![bytes.to_vec()])).unwrap()
    }

    #[test]
    fn the_first_vector_is_shown_as_recorded() {
        let v = vectors();
        let key = key_of(&unhex(text(&v, "bytes")));
        assert_eq!(key.to_display().expose(), text(&v, "display"));
        assert_eq!(
            key.to_display().expose(),
            "APPRAFTER-RECOVERY-KEY-1\nQQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK"
        );
        assert_eq!(key.canonical(Case::Upper).as_str(), text(&v, "canonical"));
        let parsed = RecoveryKey::parse(text(&v, "canonical")).unwrap();
        assert_eq!(parsed.key_bytes(), key.key_bytes());
    }

    #[test]
    fn every_typed_variant_parses_or_fails_as_recorded() {
        let v = vectors();
        let bytes = unhex(text(&v, "bytes"));
        let cases = v["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 17);
        for case in cases {
            let name = text(case, "name");
            let input = text(case, "input");
            let expect = &case["expect"];
            let got = RecoveryKey::parse(input);
            if expect["ok"] == true {
                let key = got.unwrap_or_else(|e| panic!("{name}: {e:?}"));
                assert_eq!(&key.key_bytes()[..], &bytes[..], "{name}");
                continue;
            }
            let Err(err) = got else {
                panic!("{name}: parsed");
            };
            let expected = match text(expect, "error") {
                "empty" => RecoveryKeyError::Empty,
                "wrong_prefix" => RecoveryKeyError::WrongPrefix,
                "wrong_length" => RecoveryKeyError::WrongLength,
                "checksum" => RecoveryKeyError::Checksum {
                    group: expect["group"].as_u64().map(|g| g as u8),
                },
                "invalid_char" => {
                    let position = expect["position"].as_u64().unwrap() as usize;
                    // The vectors' three are look-alikes (`1`, `O`, `B`): named, lower-cased.
                    let typed = input
                        .chars()
                        .filter(|c| *c != '-' && !c.is_whitespace())
                        .nth(position)
                        .unwrap()
                        .to_ascii_lowercase();
                    assert!(matches!(typed, '1' | 'b' | 'i' | 'o'), "{name}");
                    RecoveryKeyError::InvalidChar {
                        position,
                        lookalike: Some(typed),
                    }
                }
                other => panic!("{name}: an expectation this test does not know: {other}"),
            };
            assert_eq!(err, expected, "{name}");
        }
    }

    /// T24's look-alike hint names `1 b i o` only: any other character the person typed is
    /// never repeated in a message (it may be a character of their password).
    #[test]
    fn an_invalid_character_is_named_only_when_it_is_a_lookalike() {
        let typed = "QQQSY-QC!Q5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK";
        let error = RecoveryKey::parse(typed).unwrap_err();
        assert_eq!(
            error,
            RecoveryKeyError::InvalidChar {
                position: 7,
                lookalike: None
            }
        );
        assert_eq!(
            error.to_string(),
            "character 8 of the recovery key (group 2) is not used in recovery keys"
        );
        let error = RecoveryKey::parse(&typed.replace('!', "I")).unwrap_err();
        assert_eq!(
            error,
            RecoveryKeyError::InvalidChar {
                position: 7,
                lookalike: Some('i')
            }
        );
        assert_eq!(
            error.to_string(),
            "character 8 of the recovery key (group 2) is not used in recovery keys: `i`; did \
             you mean `l`?"
        );
    }

    #[test]
    fn keys_round_trip_through_their_shown_text() {
        for k in 0..64u8 {
            let bytes: Vec<u8> = (0..32u8)
                .map(|i| i.wrapping_mul(37).wrapping_add(k.wrapping_mul(101)))
                .collect();
            let key = key_of(&bytes);
            let shown = key.to_display();
            let back = RecoveryKey::parse(shown.expose()).unwrap();
            assert_eq!(&back.key_bytes()[..], &bytes[..]);
            assert!(looks_like_recovery_key(shown.expose()));
        }
    }

    #[test]
    fn a_second_spelling_of_the_same_bytes_is_refused() {
        let key = key_of(&unhex(text(&vectors(), "bytes")));
        let canonical = key.canonical(Case::Lower);
        let mut fes: Vec<Fe32> = canonical[PREFIX.len()..PREFIX.len() + 52]
            .chars()
            .map(|c| Fe32::from_char(c).unwrap())
            .collect();
        // The last data character carries 1 bit of the key and 4 bits of zero padding.
        fes[51] = Fe32::try_from(fes[51].to_u8() | 0x01).unwrap();
        let hrp = hrp();
        let spelled: String = fes
            .into_iter()
            .with_checksum::<Bech32m>(&hrp)
            .chars()
            .collect();
        assert!(
            CheckedHrpstring::new::<Bech32m>(&spelled).is_ok(),
            "a valid Bech32m string"
        );
        assert_eq!(
            RecoveryKey::parse(&spelled).unwrap_err(),
            RecoveryKeyError::Checksum { group: None }
        );
    }

    #[test]
    fn confirm_wants_the_whole_key_that_was_shown() {
        let v = vectors();
        let bytes = unhex(text(&v, "bytes"));
        let mut key = key_of(&bytes);
        for (typed, expected) in [
            (
                text(&v["other_key"], "canonical"),
                RecoveryKeyError::Mismatch,
            ),
            (
                "QQQSY-QCYQ5-RQWZQ-FPH9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK",
                RecoveryKeyError::Checksum { group: Some(4) },
            ),
            // The last two groups alone prove only the checksum was copied (O12): refused.
            ("0SY3K-4UK", RecoveryKeyError::WrongLength),
        ] {
            let (back, error) = key.confirm(typed).unwrap_err();
            assert_eq!(error, expected, "{typed}");
            // A refusal hands back the key that was shown, for the next attempt.
            assert_eq!(&back.key_bytes()[..], &bytes[..], "{typed}: the same key");
            key = back;
        }
        let confirmed = key
            .confirm("qqqsy qcyq5 rqwzq fpg9s crgwp ugpzy snzs2 3v9cc rydpk 8qarc 0sy3k 4uk")
            .unwrap();
        assert_eq!(confirmed.to_display().expose(), text(&v, "display"));
        assert_eq!(&confirmed.into_key().key_bytes()[..], &bytes[..]);
        assert!(key_of(&bytes).confirm(text(&v, "display")).is_ok());
    }

    #[test]
    fn the_shape_rule_of_one_prompt() {
        let v = vectors();
        let fruit = [
            "apple", "grape", "lemon", "melon", "peach", "berry", "guava", "mango", "olive",
            "onion", "bread", "pea",
        ]
        .join("-");
        for shaped in [
            text(&v, "display").to_string(),
            text(&v, "canonical").to_lowercase(),
            "apprafter-recovery-key-1".to_string(),
            "a".repeat(58),
            fruit,
            "QQQSY-QC1Q5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK".to_string(),
        ] {
            assert!(looks_like_recovery_key(&shaped), "{shaped}");
        }
        for not in [
            "a".repeat(57),
            "a".repeat(59),
            format!("{}!", "a".repeat(57)),
            "correct horse battery staple".to_string(),
            String::new(),
        ] {
            assert!(!looks_like_recovery_key(&not), "{not}");
        }
    }

    #[test]
    fn a_key_and_its_confirmation_print_nothing() {
        let key = key_of(&[0x5a; 32]);
        assert_eq!(format!("{key:?}"), "RecoveryKey(..)");
        let shown = key.to_display();
        let confirmed = key.confirm(shown.expose()).unwrap();
        assert_eq!(format!("{confirmed:?}"), "ConfirmedRecoveryKey(..)");
    }
}
