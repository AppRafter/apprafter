// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The data key, and what a person types to open a slot (D.4a T18, T62).

use std::fmt;

use zeroize::{Zeroize, Zeroizing};

use super::error::{InputWhat, RecoveryKeyError, VaultError, VaultResult};
use super::format::SlotKind;
use super::recovery::{looks_like_recovery_key, RecoveryKey};
use super::rng::VaultRng;
use crate::context::SecretString;

/// The vault's data key: 32 random bytes that encrypt the payload, wrapped once per slot. No
/// `Clone`, no `Copy`, `..` in `Debug`, zeroed on drop, boxed so a holder can lock one stable
/// page (T54).
pub struct DataKey(Box<[u8; 32]>);

impl DataKey {
    pub fn generate(rng: &mut dyn VaultRng) -> VaultResult<Self> {
        let mut key = Box::new([0u8; 32]);
        if let Err(e) = rng.fill(&mut key[..]) {
            key.zeroize();
            return Err(e);
        }
        Ok(DataKey(key))
    }

    /// A copy of a key held elsewhere (a holder's per-request copy, d4a-e), zeroed on drop.
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        let mut key = Box::new([0u8; 32]);
        key.copy_from_slice(bytes);
        DataKey(key)
    }

    /// The key's bytes, for the holder's init frame only (d4a-e).
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// A key a slot just unwrapped in place.
    pub(crate) fn from_box(key: Box<[u8; 32]>) -> Self {
        DataKey(key)
    }
}

impl Drop for DataKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for DataKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DataKey(..)")
    }
}

/// This computer's machine id as the OS gives it (D.4b B54, B76): on Linux the 32 lower-case
/// hex characters of `/etc/machine-id` without the line end, on macOS the 16 raw bytes
/// `gethostuuid()` fills. The OS crate's glue reads it (d4b-d); the vault only hashes it,
/// into an account-password slot's key and its per-slot tag. `Debug` shows nothing; zeroed on
/// drop.
#[derive(Clone)]
pub struct MachineId(Zeroizing<Vec<u8>>);

impl MachineId {
    /// `bytes` as a machine id: 16 raw bytes, or 32 lower-case hex characters, and not a null
    /// id. Anything else — empty, a line end left on, upper case, another length, all zero
    /// bytes or all `'0'`s — is refused, so a reader's slip can never bind a slot to a
    /// constant (`InvalidInput(MachineId)`).
    pub fn new(bytes: &[u8]) -> VaultResult<MachineId> {
        let raw_uuid = bytes.len() == 16;
        let hex_id = bytes.len() == 32
            && bytes
                .iter()
                .all(|b| matches!(*b, b'0'..=b'9' | b'a'..=b'f'));
        // A null id — what a failed read leaves in a zeroed buffer, and an id systemd calls
        // uninitialised — would bind every such slot to one constant (critic round, 2).
        let null = bytes.iter().all(|b| *b == 0) || bytes.iter().all(|b| *b == b'0');
        if !(raw_uuid || hex_id) || null {
            return Err(VaultError::InvalidInput(InputWhat::MachineId));
        }
        Ok(MachineId(Zeroizing::new(bytes.to_vec())))
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MachineId(..)")
    }
}

/// A secret that opens the slots of its kind.
pub enum SlotSecret {
    /// A lock password: the `0x01` slots only.
    Password(SecretString),
    /// The account password as typed, with this computer's machine id: the first `0x03` slot
    /// made on this computer only (B54, B77), never another computer's.
    AccountPassword(SecretString, MachineId),
    RecoveryKey(RecoveryKey),
}

impl SlotSecret {
    /// The kind of slot this secret opens.
    pub(crate) fn kind(&self) -> SlotKind {
        match self {
            SlotSecret::Password(_) => SlotKind::Password,
            SlotSecret::AccountPassword(..) => SlotKind::OsPassword,
            SlotSecret::RecoveryKey(_) => SlotKind::RecoveryKey,
        }
    }
}

impl fmt::Debug for SlotSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SlotSecret::Password(_) => "SlotSecret::Password(..)",
            SlotSecret::AccountPassword(..) => "SlotSecret::AccountPassword(..)",
            SlotSecret::RecoveryKey(_) => "SlotSecret::RecoveryKey(..)",
        })
    }
}

/// What one prompt's input is tried as (T62): one prompt and one field take either secret.
pub enum SlotInput {
    /// Input of a recovery key's shape whose checksum holds.
    RecoveryKey(RecoveryKey),
    /// Anything else.
    Password(SecretString),
    /// Recovery-key shaped but failing its checksum: it is tried against the password slots
    /// too, and only when they refuse it is `error` reported, as one attempt.
    MistypedRecoveryKey {
        as_password: SecretString,
        error: RecoveryKeyError,
    },
}

impl SlotInput {
    pub fn from_input(input: SecretString) -> Self {
        if !looks_like_recovery_key(input.expose()) {
            return SlotInput::Password(input);
        }
        match RecoveryKey::parse(input.expose()) {
            Ok(key) => SlotInput::RecoveryKey(key),
            Err(error) => SlotInput::MistypedRecoveryKey {
                as_password: input,
                error,
            },
        }
    }
}

impl fmt::Debug for SlotInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlotInput::RecoveryKey(_) => f.write_str("SlotInput::RecoveryKey(..)"),
            SlotInput::Password(_) => f.write_str("SlotInput::Password(..)"),
            SlotInput::MistypedRecoveryKey { error, .. } => {
                write!(f, "SlotInput::MistypedRecoveryKey({error:?}, ..)")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::rng::ScriptedRandom;

    const SHOWN: &str =
        "APPRAFTER-RECOVERY-KEY-1\nQQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK";

    #[test]
    fn a_data_key_comes_from_the_generator_and_prints_nothing() {
        let dk = DataKey::generate(&mut ScriptedRandom::new(vec![vec![0x42; 32]])).unwrap();
        assert_eq!(dk.expose(), &[0x42; 32]);
        assert_eq!(format!("{dk:?}"), "DataKey(..)");
        assert_eq!(DataKey::from_bytes(dk.expose()).expose(), dk.expose());
        assert!(matches!(
            DataKey::generate(&mut ScriptedRandom::failing_at(0, vec![])),
            Err(VaultError::RandomUnavailable { .. })
        ));
    }

    #[test]
    fn one_input_is_tried_as_what_it_looks_like() {
        match SlotInput::from_input(SecretString::new(SHOWN)) {
            SlotInput::RecoveryKey(key) => assert_eq!(key.key_bytes()[31], 0x1f),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            SlotInput::from_input(SecretString::new("correct horse battery staple")),
            SlotInput::Password(_)
        ));
        let typo = "QQQSY-QCYQ5-RQWZQ-FPH9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK";
        match SlotInput::from_input(SecretString::new(typo)) {
            SlotInput::MistypedRecoveryKey { as_password, error } => {
                assert_eq!(as_password.expose(), typo);
                assert_eq!(error, RecoveryKeyError::Checksum { group: Some(4) });
            }
            other => panic!("{other:?}"),
        }
        let one = "QQ1SY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK";
        match SlotInput::from_input(SecretString::new(one)) {
            SlotInput::MistypedRecoveryKey { error, .. } => assert_eq!(
                error,
                RecoveryKeyError::InvalidChar {
                    position: 2,
                    lookalike: Some('1')
                }
            ),
            other => panic!("{other:?}"),
        }
    }

    /// B76: 16 raw bytes (macOS) or 32 lower-case hex characters (Linux), nothing else and
    /// never a null id — a reader's slip never weakens the binding to a constant.
    #[test]
    fn a_machine_id_is_16_raw_bytes_or_32_hex_characters() {
        assert!(MachineId::new(&[0xff; 16]).is_ok());
        assert!(MachineId::new(b"5f0d6a3e0c2b4f6a8d9e1b2c3d4e5f60").is_ok());
        for bad in [
            &b""[..],
            &b"5f0d6a3e0c2b4f6a8d9e1b2c3d4e5f6"[..],
            &b"5f0d6a3e0c2b4f6a8d9e1b2c3d4e5f60\n"[..],
            &b"5F0D6A3E0C2B4F6A8D9E1B2C3D4E5F60"[..],
            &b"5f0d6a3e0c2b4f6a8d9e1b2c3d4e5f6g"[..],
            &b"00000000000000000000000000000000"[..],
            &[0u8; 16][..],
            &[0u8; 32][..],
            &[0u8; 15][..],
            &[0u8; 17][..],
        ] {
            assert!(
                matches!(
                    MachineId::new(bad),
                    Err(VaultError::InvalidInput(InputWhat::MachineId))
                ),
                "{bad:?}"
            );
        }
        assert_eq!(
            format!("{:?}", MachineId::new(&[7; 16]).unwrap()),
            "MachineId(..)"
        );
    }

    #[test]
    fn secrets_print_nothing() {
        assert_eq!(
            format!("{:?}", SlotSecret::Password(SecretString::new("hunter2"))),
            "SlotSecret::Password(..)"
        );
        assert_eq!(
            format!("{:?}", SlotInput::from_input(SecretString::new("hunter2"))),
            "SlotInput::Password(..)"
        );
    }
}
