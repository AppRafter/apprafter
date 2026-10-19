// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The vault's payload (overview §3.7; T21, T26): key–value entries, encoded inside the AEAD as
//! a strict TLV — `entry_count u16`, then per entry `key_len u8 ‖ key ‖ value_len u16 ‖ value`,
//! keys in strictly ascending byte order, nothing after the last entry. Known keys:
//! `age-identity` (the store's own age identity, required), `age-identity-retiring` (the store
//! key a rotation retires, D.4b B63: present only between its two vault writes, never used by a
//! read), `target/<name>/hetzner-token` and `settings/unlock-timeout-min` (`0`..=`1440`, absent
//! = 5). Every other key is kept byte for byte and never read. Values are bytes; the known ones
//! are UTF-8.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use zeroize::Zeroizing;

use super::error::{InputWhat, PayloadWhat, SizeWhat, VaultError, VaultResult};
use super::format::PAYLOAD_MAX;
use super::rng::VaultRng;
use crate::context::SecretString;
use crate::target::validate_name;

/// At most 8192 entries: the identity, the timeout and about 8190 targets, or 1 MiB.
pub const ENTRY_MAX: usize = 8192;
/// An entry key: at most 128 bytes.
pub const KEY_MAX: usize = 128;
/// An entry value: 1 to 16384 bytes.
pub const VALUE_MAX: usize = 16_384;
/// The unlock window when the vault holds no `settings/unlock-timeout-min`.
pub const DEFAULT_UNLOCK_TIMEOUT_MIN: u32 = 5;
/// The longest unlock window (owner decision O3).
pub const MAX_UNLOCK_TIMEOUT_MIN: u32 = 1440;

const AGE_IDENTITY: &str = "age-identity";
const AGE_IDENTITY_RETIRING: &str = "age-identity-retiring";
/// The human-readable part of age's X25519 identity text (Bech32, BIP 173).
const AGE_IDENTITY_HRP: &str = "age-secret-key-";
/// An age X25519 identity's text: `AGE-SECRET-KEY-1`, 52 characters for 32 bytes, 6 of
/// checksum.
const IDENTITY_TEXT_LEN: usize = 74;
const UNLOCK_TIMEOUT: &str = "settings/unlock-timeout-min";
const TOKEN_LEAF: &str = "hetzner-token";

/// What an entry key means to this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKey {
    AgeIdentity,
    /// The store key a rotation retires (D.4b B63).
    AgeIdentityRetiring,
    HetznerToken {
        target: String,
    },
    UnlockTimeoutMin,
    /// Unknown: kept as it is, never read.
    Other(String),
}

impl EntryKey {
    pub fn parse(key: &str) -> EntryKey {
        match key {
            AGE_IDENTITY => EntryKey::AgeIdentity,
            AGE_IDENTITY_RETIRING => EntryKey::AgeIdentityRetiring,
            UNLOCK_TIMEOUT => EntryKey::UnlockTimeoutMin,
            _ => match key.split('/').collect::<Vec<_>>().as_slice() {
                ["target", name, TOKEN_LEAF] => EntryKey::HetznerToken {
                    target: (*name).to_string(),
                },
                _ => EntryKey::Other(key.to_string()),
            },
        }
    }

    pub fn as_key(&self) -> String {
        match self {
            EntryKey::AgeIdentity => AGE_IDENTITY.to_string(),
            EntryKey::AgeIdentityRetiring => AGE_IDENTITY_RETIRING.to_string(),
            EntryKey::UnlockTimeoutMin => UNLOCK_TIMEOUT.to_string(),
            EntryKey::HetznerToken { target } => token_key(target),
            EntryKey::Other(key) => key.clone(),
        }
    }
}

fn token_key(target: &str) -> String {
    format!("target/{target}/{TOKEN_LEAF}")
}

fn target_prefix(target: &str) -> String {
    format!("target/{target}/")
}

fn payload_invalid(what: PayloadWhat) -> VaultError {
    VaultError::PayloadInvalid { what }
}

/// The payload of an opened vault. It exists only after the payload authenticated (or as the
/// input of `create`), so an absent entry here means absent, never "could not decrypt".
pub struct VaultPayload {
    entries: BTreeMap<String, Zeroizing<Vec<u8>>>,
}

impl fmt::Debug for VaultPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VaultPayload({} entries, ..)", self.entries.len())
    }
}

impl VaultPayload {
    /// A payload holding only the store's own age identity, in its canonical text.
    pub fn new(identity: &age::x25519::Identity) -> Self {
        let text = identity.to_string();
        let mut entries = BTreeMap::new();
        entries.insert(
            AGE_IDENTITY.to_string(),
            Zeroizing::new(text.expose_secret().as_bytes().to_vec()),
        );
        VaultPayload { entries }
    }

    /// The store's own age identity.
    pub fn age_identity(&self) -> VaultResult<age::x25519::Identity> {
        let value = self
            .entries
            .get(AGE_IDENTITY)
            .ok_or_else(|| payload_invalid(PayloadWhat::AgeIdentityMissing))?;
        parse_identity(value)
    }

    /// The store key a rotation is retiring (D.4b B63, §4.5), or `None` when this
    /// authenticated payload holds none. It is there only between a rotation's two vault
    /// writes (R2 and R4), while caches may still be sealed to it: a rotation's re-seal and
    /// `lock off` from `rotating` open caches with it; a read never does — every read and
    /// every new cache uses [`VaultPayload::age_identity`].
    pub fn retiring_identity(&self) -> VaultResult<Option<age::x25519::Identity>> {
        match self.entries.get(AGE_IDENTITY_RETIRING) {
            None => Ok(None),
            Some(value) => parse_identity(value)
                .map(Some)
                .map_err(|_| payload_invalid(PayloadWhat::RetiringIdentity)),
        }
    }

    /// `target`'s Hetzner token, or `None` when this authenticated payload holds none.
    pub fn hetzner_token(&self, target: &str) -> Option<SecretString> {
        self.entries.get(&token_key(target)).map(|value| {
            SecretString::new(
                std::str::from_utf8(value).expect("a token is UTF-8: checked when decoded or set"),
            )
        })
    }

    pub fn set_hetzner_token(&mut self, target: &str, token: SecretString) -> VaultResult<()> {
        validate_name(target).map_err(|_| payload_invalid(PayloadWhat::TargetName))?;
        let value = token.expose().as_bytes();
        if value.is_empty() || value.len() > VALUE_MAX {
            return Err(payload_invalid(PayloadWhat::HetznerToken));
        }
        self.entries
            .insert(token_key(target), Zeroizing::new(value.to_vec()));
        Ok(())
    }

    /// The default unlock window in minutes: the stored value, or 5 when none is stored.
    pub fn unlock_timeout_min(&self) -> VaultResult<u32> {
        match self.entries.get(UNLOCK_TIMEOUT) {
            None => Ok(DEFAULT_UNLOCK_TIMEOUT_MIN),
            Some(value) => parse_timeout(value),
        }
    }

    pub fn set_unlock_timeout_min(&mut self, minutes: u32) -> VaultResult<()> {
        if minutes > MAX_UNLOCK_TIMEOUT_MIN {
            return Err(payload_invalid(PayloadWhat::UnlockTimeout));
        }
        self.entries.insert(
            UNLOCK_TIMEOUT.to_string(),
            Zeroizing::new(minutes.to_string().into_bytes()),
        );
        Ok(())
    }

    /// Removes every entry under `target/<target>/`, known or not; how many it removed.
    pub fn remove_target(&mut self, target: &str) -> usize {
        let keys = self.keys_under(&target_prefix(target));
        for key in &keys {
            self.entries.remove(key);
        }
        keys.len()
    }

    /// Moves every entry under `target/<from>/` to `target/<to>/`. Refuses a `to` that breaks
    /// the target-name rule or already has entries.
    pub fn rename_target(&mut self, from: &str, to: &str) -> VaultResult<()> {
        validate_name(to).map_err(|_| payload_invalid(PayloadWhat::TargetName))?;
        let to_prefix = target_prefix(to);
        if !self.keys_under(&to_prefix).is_empty() {
            return Err(payload_invalid(PayloadWhat::TargetExists));
        }
        let from_prefix = target_prefix(from);
        let keys = self.keys_under(&from_prefix);
        if keys
            .iter()
            .any(|key| to_prefix.len() + key.len() - from_prefix.len() > KEY_MAX)
        {
            return Err(VaultError::TooLarge {
                what: SizeWhat::Key,
                limit: KEY_MAX as u64,
            });
        }
        for key in keys {
            let value = self.entries.remove(&key).expect("listed above");
            let moved = format!("{to_prefix}{}", &key[from_prefix.len()..]);
            self.entries.insert(moved, value);
        }
        Ok(())
    }

    /// Every entry's key, sorted; never a value.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    fn keys_under(&self, prefix: &str) -> Vec<String> {
        self.entries
            .range(prefix.to_string()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// This payload as a rotation writes it (R2, B63): `next` as the store key, the current
    /// one — byte for byte — as `age-identity-retiring`, every other entry as it is. Refused
    /// when a rotation is already under way (a retiring key is there): that one is finished
    /// first (§5.4), or its key would be lost while caches may still be sealed to it.
    pub(crate) fn rotated_to(&self, next: &age::x25519::Identity) -> VaultResult<VaultPayload> {
        if self.entries.contains_key(AGE_IDENTITY_RETIRING) {
            return Err(VaultError::InvalidInput(InputWhat::RotationUnfinished));
        }
        let current = self
            .entries
            .get(AGE_IDENTITY)
            .ok_or_else(|| payload_invalid(PayloadWhat::AgeIdentityMissing))?
            .clone();
        let mut entries = self.entries.clone();
        entries.insert(AGE_IDENTITY_RETIRING.to_string(), current);
        entries.insert(
            AGE_IDENTITY.to_string(),
            Zeroizing::new(next.to_string().expose_secret().as_bytes().to_vec()),
        );
        Ok(VaultPayload { entries })
    }

    /// The encoded length, and the two limits a payload can break.
    pub(crate) fn check_size(&self) -> VaultResult<usize> {
        if self.entries.len() > ENTRY_MAX {
            return Err(VaultError::TooLarge {
                what: SizeWhat::Entries,
                limit: ENTRY_MAX as u64,
            });
        }
        let len = 2 + self
            .entries
            .iter()
            .map(|(key, value)| 1 + key.len() + 2 + value.len())
            .sum::<usize>();
        if len > PAYLOAD_MAX {
            return Err(VaultError::TooLarge {
                what: SizeWhat::Payload,
                limit: PAYLOAD_MAX as u64,
            });
        }
        Ok(len)
    }

    /// The TLV plaintext, in one pre-sized buffer that never reallocates and is zeroed on drop.
    pub(crate) fn encode(&self) -> VaultResult<Zeroizing<Vec<u8>>> {
        let len = self.check_size()?;
        let mut out = Zeroizing::new(Vec::with_capacity(len));
        out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        for (key, value) in &self.entries {
            out.push(key.len() as u8);
            out.extend_from_slice(key.as_bytes());
            out.extend_from_slice(&(value.len() as u16).to_le_bytes());
            out.extend_from_slice(value);
        }
        debug_assert_eq!(out.len(), len);
        Ok(out)
    }

    /// Parses an authenticated plaintext: only the one canonical encoding, known keys checked.
    pub(crate) fn decode(plain: &[u8]) -> VaultResult<Self> {
        let mut r = Cursor {
            input: plain,
            pos: 0,
        };
        let count = usize::from(r.u16()?);
        if count == 0 || count > ENTRY_MAX {
            return Err(payload_invalid(PayloadWhat::EntryCount));
        }
        let mut entries: BTreeMap<String, Zeroizing<Vec<u8>>> = BTreeMap::new();
        let mut previous: Option<String> = None;
        for _ in 0..count {
            let key_len = usize::from(r.take(1)?[0]);
            if key_len == 0 || key_len > KEY_MAX {
                return Err(payload_invalid(PayloadWhat::KeyLength));
            }
            let key = r.take(key_len)?;
            if !valid_key(key) {
                return Err(payload_invalid(PayloadWhat::KeyText));
            }
            let key = String::from_utf8(key.to_vec()).expect("checked ASCII");
            if previous.as_deref().is_some_and(|p| p >= key.as_str()) {
                return Err(payload_invalid(PayloadWhat::KeyOrder));
            }
            let value_len = usize::from(r.u16()?);
            if value_len == 0 || value_len > VALUE_MAX {
                return Err(payload_invalid(PayloadWhat::ValueLength));
            }
            let value = Zeroizing::new(r.take(value_len)?.to_vec());
            previous = Some(key.clone());
            entries.insert(key, value);
        }
        if r.pos != plain.len() {
            return Err(payload_invalid(PayloadWhat::TrailingBytes));
        }
        let payload = VaultPayload { entries };
        payload.check_known()?;
        Ok(payload)
    }

    /// The rules of the known keys: one parsable identity (and a parsable retiring one, when
    /// there is one), a valid timeout, tokens of valid target names in UTF-8.
    fn check_known(&self) -> VaultResult<()> {
        self.age_identity()?;
        self.retiring_identity()?;
        self.unlock_timeout_min()?;
        for (key, value) in &self.entries {
            if let EntryKey::HetznerToken { target } = EntryKey::parse(key) {
                validate_name(&target).map_err(|_| payload_invalid(PayloadWhat::TargetName))?;
                std::str::from_utf8(value)
                    .map_err(|_| payload_invalid(PayloadWhat::HetznerToken))?;
            }
        }
        Ok(())
    }

    /// An entry's raw bytes (tests: unknown entries survive byte for byte).
    #[cfg(test)]
    pub(crate) fn raw(&self, key: &str) -> Option<&[u8]> {
        self.entries.get(key).map(|value| value.as_slice())
    }
}

/// A bounds-checked cursor over an authenticated plaintext.
struct Cursor<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> VaultResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.input.len())
            .ok_or_else(|| payload_invalid(PayloadWhat::Truncated))?;
        let out = &self.input[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u16(&mut self) -> VaultResult<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
}

/// `segment ("/" segment)*`, a segment `[A-Za-z0-9-]+`.
fn valid_key(key: &[u8]) -> bool {
    key.split(|b| *b == b'/').all(|segment| {
        !segment.is_empty()
            && segment
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
    })
}

fn parse_identity(value: &[u8]) -> VaultResult<age::x25519::Identity> {
    let text = std::str::from_utf8(value).map_err(|_| payload_invalid(PayloadWhat::AgeIdentity))?;
    age::x25519::Identity::from_str(text).map_err(|_| payload_invalid(PayloadWhat::AgeIdentity))
}

/// A new store key (D.4b B63) from 32 bytes of the vault's generator — so a rotation's
/// vectors pin it like every other byte the vault draws — in age's own text: Bech32 under
/// [`AGE_IDENTITY_HRP`], upper case, parsed back by age. age clamps the scalar when it uses
/// it, as for a key it generates; its secret keeps the bytes as drawn (x25519-dalek 2), so
/// the text round-trips. The bytes and the text live in zeroed buffers; bech32's own encode
/// buffer is T29's stated limit.
pub(crate) fn draw_identity(rng: &mut dyn VaultRng) -> VaultResult<age::x25519::Identity> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    rng.fill(&mut bytes[..])?;
    let hrp =
        bech32::Hrp::parse(AGE_IDENTITY_HRP).expect("age's identity HRP is a valid Bech32 HRP");
    // Pre-sized, so the encoder's writes never reallocate and leave a copy behind.
    let mut text = Zeroizing::new(String::with_capacity(IDENTITY_TEXT_LEN));
    bech32::encode_upper_to_fmt::<bech32::Bech32, String>(&mut *text, hrp, &bytes[..])
        .expect("32 bytes always encode under age's identity HRP");
    debug_assert_eq!(text.len(), IDENTITY_TEXT_LEN);
    age::x25519::Identity::from_str(&text).map_err(|_| payload_invalid(PayloadWhat::AgeIdentity))
}

/// ASCII decimal, 0..=1440, no sign, no leading zero, no spaces.
fn parse_timeout(value: &[u8]) -> VaultResult<u32> {
    let canonical = !value.is_empty()
        && value.len() <= 4
        && value.iter().all(u8::is_ascii_digit)
        && (value.len() == 1 || value[0] != b'0');
    let minutes = canonical
        .then(|| std::str::from_utf8(value).ok()?.parse::<u32>().ok())
        .flatten()
        .filter(|m| *m <= MAX_UNLOCK_TIMEOUT_MIN);
    minutes.ok_or_else(|| payload_invalid(PayloadWhat::UnlockTimeout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::rng::ScriptedRandom;
    use crate::vault::testutil::{hex, seq, tlv, unhex};

    const IDENTITY: &str =
        "AGE-SECRET-KEY-1YQSJYGEYY5NZW2PF9G4JCTFW9UCRZV3NXS6NVDEC8YARK0PA8CLSWXGG7Q";

    /// The fast-params vector's payload plaintext as the independent generator encoded it.
    const VECTOR_PLAINTEXT: &str = "\
        04000c6167652d6964656e746974794a004147452d5345435245542d4b45592d315951534a59474559\
        59354e5a573250463947344a43544657395543525a56334e5853364e56444543385941524b30504138\
        434c535758474737511b73657474696e67732f756e6c6f636b2d74696d656f75742d6d696e02003135\
        197461726765742f70726f642f6865747a6e65722d746f6b656e400041316232433364344131623243\
        3364344131623243336434413162324333643441316232433364344131623243336434413162324333\
        643441316232433364341e7461726765742f73746167696e672d312f6865747a6e65722d746f6b656e\
        4000733954387537563673395438753756367339543875375636733954387537563673395438753756\
        36733954387537563673395438753756367339543875375636";

    fn identity() -> age::x25519::Identity {
        IDENTITY.parse().unwrap()
    }

    fn invalid(plain: &[u8]) -> PayloadWhat {
        match VaultPayload::decode(plain) {
            Err(VaultError::PayloadInvalid { what }) => what,
            other => panic!(
                "expected PayloadInvalid, got {:?}",
                other.map(|_| "a payload")
            ),
        }
    }

    #[test]
    fn a_new_payload_holds_the_identity_and_the_default_timeout() {
        let payload = VaultPayload::new(&identity());
        assert_eq!(
            payload.age_identity().unwrap().to_public().to_string(),
            identity().to_public().to_string()
        );
        assert_eq!(
            payload.unlock_timeout_min().unwrap(),
            DEFAULT_UNLOCK_TIMEOUT_MIN
        );
        assert_eq!(payload.keys().collect::<Vec<_>>(), ["age-identity"]);
    }

    #[test]
    fn entries_encode_as_the_independent_generator_wrote_them() {
        let mut payload = VaultPayload::new(&identity());
        payload
            .set_hetzner_token("staging-1", SecretString::new("s9T8u7V6".repeat(8)))
            .unwrap();
        payload
            .set_hetzner_token("prod", SecretString::new("A1b2C3d4".repeat(8)))
            .unwrap();
        payload.set_unlock_timeout_min(15).unwrap();
        let encoded = payload.encode().unwrap();
        assert_eq!(hex(&encoded), hex(&unhex(VECTOR_PLAINTEXT)));
        let back = VaultPayload::decode(&encoded).unwrap();
        assert_eq!(
            back.keys().collect::<Vec<_>>(),
            [
                "age-identity",
                "settings/unlock-timeout-min",
                "target/prod/hetzner-token",
                "target/staging-1/hetzner-token"
            ]
        );
        assert_eq!(
            back.hetzner_token("prod").unwrap().expose(),
            "A1b2C3d4".repeat(8)
        );
        assert_eq!(back.hetzner_token("absent"), None);
        assert_eq!(back.unlock_timeout_min().unwrap(), 15);
        assert_eq!(back.raw("settings/unlock-timeout-min"), Some(&b"15"[..]));
    }

    #[test]
    fn every_entry_under_a_target_moves_or_goes_with_it() {
        let plain = tlv(&[
            ("age-identity", IDENTITY.as_bytes()),
            ("target/prod/future-thing", &b"opaque-1"[..]),
            ("target/prod/hetzner-token", &b"tok"[..]),
            ("target/production/hetzner-token", &b"other"[..]),
            ("x-future/key", &[0xffu8, 0xfe, 0x00, 0x01][..]),
        ]);
        let mut payload = VaultPayload::decode(&plain).unwrap();
        payload.rename_target("prod", "live").unwrap();
        assert_eq!(
            payload.raw("target/live/future-thing"),
            Some(&b"opaque-1"[..])
        );
        assert_eq!(payload.hetzner_token("live").unwrap().expose(), "tok");
        assert_eq!(payload.hetzner_token("prod"), None);
        assert_eq!(
            payload.raw("x-future/key"),
            Some(&[0xff, 0xfe, 0x00, 0x01][..])
        );
        assert!(matches!(
            payload.rename_target("live", "production"),
            Err(VaultError::PayloadInvalid {
                what: PayloadWhat::TargetExists
            })
        ));
        assert!(matches!(
            payload.rename_target("live", "-bad"),
            Err(VaultError::PayloadInvalid {
                what: PayloadWhat::TargetName
            })
        ));
        assert_eq!(payload.remove_target("live"), 2);
        assert_eq!(payload.remove_target("live"), 0);
        assert_eq!(
            payload.keys().collect::<Vec<_>>(),
            [
                "age-identity",
                "target/production/hetzner-token",
                "x-future/key"
            ]
        );
        // An unknown entry survives a round trip byte for byte.
        let again = VaultPayload::decode(&payload.encode().unwrap()).unwrap();
        assert_eq!(
            again.raw("x-future/key"),
            Some(&[0xff, 0xfe, 0x00, 0x01][..])
        );
    }

    #[test]
    fn a_rename_never_makes_a_key_too_long() {
        let long_leaf = format!("target/a/{}", "z".repeat(118));
        assert_eq!(long_leaf.len(), 127);
        let plain = tlv(&[
            ("age-identity", IDENTITY.as_bytes()),
            (long_leaf.as_str(), &b"v"[..]),
        ]);
        let mut payload = VaultPayload::decode(&plain).unwrap();
        assert!(matches!(
            payload.rename_target("a", "abc"),
            Err(VaultError::TooLarge {
                what: SizeWhat::Key,
                ..
            })
        ));
    }

    #[test]
    fn the_decoder_takes_only_the_one_canonical_encoding() {
        let id = ("age-identity", IDENTITY.as_bytes());
        assert_eq!(invalid(&[]), PayloadWhat::Truncated);
        assert_eq!(invalid(&tlv(&[])), PayloadWhat::EntryCount);
        let mut too_many = tlv(&[id]);
        too_many[..2].copy_from_slice(&8193u16.to_le_bytes());
        assert_eq!(invalid(&too_many), PayloadWhat::EntryCount);
        assert_eq!(
            invalid(&tlv(&[id, ("", &b"v"[..])])),
            PayloadWhat::KeyLength
        );
        assert_eq!(
            invalid(&tlv(&[id, ("x_y", &b"v"[..])])),
            PayloadWhat::KeyText
        );
        assert_eq!(
            invalid(&tlv(&[id, ("x//y", &b"v"[..])])),
            PayloadWhat::KeyText
        );
        assert_eq!(
            invalid(&tlv(&[id, ("x/", &b"v"[..])])),
            PayloadWhat::KeyText
        );
        assert_eq!(
            invalid(&tlv(&[("b", &b"v"[..]), id])),
            PayloadWhat::KeyOrder
        );
        assert_eq!(invalid(&tlv(&[id, id])), PayloadWhat::KeyOrder);
        assert_eq!(
            invalid(&tlv(&[id, ("x", &b""[..])])),
            PayloadWhat::ValueLength
        );
        assert_eq!(
            invalid(&tlv(&[id, ("x", &vec![b'v'; VALUE_MAX + 1][..])])),
            PayloadWhat::ValueLength
        );
        let mut trailing = tlv(&[id]);
        trailing.push(0);
        assert_eq!(invalid(&trailing), PayloadWhat::TrailingBytes);
        let whole = tlv(&[id]);
        assert_eq!(invalid(&whole[..whole.len() - 1]), PayloadWhat::Truncated);
        assert_eq!(
            invalid(&tlv(&[("x", &b"v"[..])])),
            PayloadWhat::AgeIdentityMissing
        );
        assert_eq!(
            invalid(&tlv(&[("age-identity", &b"not an identity"[..])])),
            PayloadWhat::AgeIdentity
        );
        for timeout in [
            &b"1441"[..],
            &b"015"[..],
            &b"+5"[..],
            &b"5 "[..],
            &b"99999"[..],
        ] {
            assert_eq!(
                invalid(&tlv(&[id, ("settings/unlock-timeout-min", timeout)])),
                PayloadWhat::UnlockTimeout,
                "{timeout:?}"
            );
        }
        assert_eq!(
            invalid(&tlv(&[id, ("target/-x/hetzner-token", &b"t"[..])])),
            PayloadWhat::TargetName
        );
        assert_eq!(
            invalid(&tlv(&[id, ("target/prod/hetzner-token", &[0xffu8][..])])),
            PayloadWhat::HetznerToken
        );
        for timeout in [&b"0"[..], &b"5"[..], &b"1440"[..]] {
            assert!(
                VaultPayload::decode(&tlv(&[id, ("settings/unlock-timeout-min", timeout)])).is_ok()
            );
        }
    }

    #[test]
    fn the_setters_keep_the_rules() {
        let mut payload = VaultPayload::new(&identity());
        for (target, token, what) in [
            ("Bad_Name", "t".to_string(), PayloadWhat::TargetName),
            ("prod", String::new(), PayloadWhat::HetznerToken),
            ("prod", "t".repeat(VALUE_MAX + 1), PayloadWhat::HetznerToken),
        ] {
            assert!(
                matches!(
                    payload.set_hetzner_token(target, SecretString::new(token)),
                    Err(VaultError::PayloadInvalid { what: w }) if w == what
                ),
                "{target}"
            );
        }
        assert!(matches!(
            payload.set_unlock_timeout_min(1441),
            Err(VaultError::PayloadInvalid {
                what: PayloadWhat::UnlockTimeout
            })
        ));
        for minutes in [0, 1440] {
            payload.set_unlock_timeout_min(minutes).unwrap();
            assert_eq!(payload.unlock_timeout_min().unwrap(), minutes);
        }
    }

    #[test]
    fn entry_keys_are_classified() {
        for (key, expected) in [
            ("age-identity", EntryKey::AgeIdentity),
            ("age-identity-retiring", EntryKey::AgeIdentityRetiring),
            ("settings/unlock-timeout-min", EntryKey::UnlockTimeoutMin),
            (
                "target/prod/hetzner-token",
                EntryKey::HetznerToken {
                    target: "prod".into(),
                },
            ),
            (
                "target/prod/future-thing",
                EntryKey::Other("target/prod/future-thing".into()),
            ),
            ("x-future/key", EntryKey::Other("x-future/key".into())),
        ] {
            assert_eq!(EntryKey::parse(key), expected);
            assert_eq!(expected.as_key(), key);
        }
    }

    /// D.4b B63: the retiring store key is a known entry — read as an age identity, refused
    /// when it does not parse (a D.4a build kept it as an unknown entry) — and its absence
    /// is `None`, never the store key itself.
    #[test]
    fn a_retiring_store_key_is_read_and_checked() {
        let id = ("age-identity", IDENTITY.as_bytes());
        let none = VaultPayload::decode(&tlv(&[id])).unwrap();
        assert!(none.retiring_identity().unwrap().is_none());
        let current = age::x25519::Identity::generate();
        let current_text = current.to_string();
        let rotating = VaultPayload::decode(&tlv(&[
            ("age-identity", current_text.expose_secret().as_bytes()),
            ("age-identity-retiring", IDENTITY.as_bytes()),
        ]))
        .unwrap();
        assert_eq!(
            rotating.age_identity().unwrap().to_public().to_string(),
            current.to_public().to_string()
        );
        assert_eq!(
            rotating
                .retiring_identity()
                .unwrap()
                .unwrap()
                .to_public()
                .to_string(),
            identity().to_public().to_string()
        );
        assert_eq!(
            invalid(&tlv(&[
                id,
                ("age-identity-retiring", &b"not an identity"[..])
            ])),
            PayloadWhat::RetiringIdentity
        );
        // Byte for byte through its own encoding.
        let again = VaultPayload::decode(&rotating.encode().unwrap()).unwrap();
        assert_eq!(
            again.raw("age-identity-retiring"),
            Some(IDENTITY.as_bytes())
        );
    }

    /// D.4b B63, F5: a drawn store key is age's own text for the bytes drawn — D.4a's
    /// vectors' identity, written by the independent generator from bytes `20 … 3f` — and a
    /// failing generator fails it.
    #[test]
    fn a_drawn_store_key_is_ages_text_for_the_drawn_bytes() {
        let drawn = draw_identity(&mut ScriptedRandom::new(vec![seq(0x20, 32)])).unwrap();
        assert_eq!(drawn.to_string().expose_secret(), IDENTITY);
        assert!(matches!(
            draw_identity(&mut ScriptedRandom::failing_at(0, vec![])),
            Err(VaultError::RandomUnavailable { .. })
        ));
    }

    /// D.4b B63: a rotation's payload — the new store key as `age-identity`, the current
    /// one, byte for byte, as `age-identity-retiring`, every other entry as it is — and a
    /// second rotation before the first is finished is refused.
    #[test]
    fn a_rotation_carries_the_store_key_it_retires() {
        let current = VaultPayload::decode(&tlv(&[
            ("age-identity", IDENTITY.as_bytes()),
            ("settings/unlock-timeout-min", &b"15"[..]),
            ("x-future/key", &[0xffu8, 0xfe][..]),
        ]))
        .unwrap();
        let next = draw_identity(&mut ScriptedRandom::new(vec![seq(0xa0, 32)])).unwrap();
        let rotated = current.rotated_to(&next).unwrap();
        assert_eq!(
            rotated.keys().collect::<Vec<_>>(),
            [
                "age-identity",
                "age-identity-retiring",
                "settings/unlock-timeout-min",
                "x-future/key"
            ]
        );
        assert_eq!(
            rotated.raw("age-identity"),
            Some(next.to_string().expose_secret().as_bytes())
        );
        assert_eq!(
            rotated.raw("age-identity-retiring"),
            Some(IDENTITY.as_bytes())
        );
        for key in ["settings/unlock-timeout-min", "x-future/key"] {
            assert_eq!(rotated.raw(key), current.raw(key), "{key}");
        }
        assert_eq!(
            rotated
                .retiring_identity()
                .unwrap()
                .unwrap()
                .to_public()
                .to_string(),
            identity().to_public().to_string()
        );
        assert!(matches!(
            rotated.rotated_to(&identity()),
            Err(VaultError::InvalidInput(InputWhat::RotationUnfinished))
        ));
        assert_eq!(current.raw("age-identity"), Some(IDENTITY.as_bytes()));
        assert!(current.raw("age-identity-retiring").is_none());
    }

    #[test]
    fn a_payload_prints_no_entry() {
        let mut payload = VaultPayload::new(&identity());
        payload
            .set_hetzner_token("prod", SecretString::new("A1b2C3d4".repeat(8)))
            .unwrap();
        assert_eq!(format!("{payload:?}"), "VaultPayload(2 entries, ..)");
    }
}
