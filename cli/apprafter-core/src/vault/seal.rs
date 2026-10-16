// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Creating, sealing and opening a vault (D.4a T18–T21, T63; overview §3.7).
//!
//! ```text
//! material  = Argon2id(password)  |  recovery key bytes
//! kek, key_check = HKDF-SHA256(salt = slot salt, ikm = material), info "…/kek/" ‖ kind, "…/key-check/" ‖ kind
//! slot_aad  = SLOT_AAD_DOMAIN ‖ vault_id ‖ slot_id ‖ kind ‖ body[..end of salt]
//! wrapped   = XChaCha20Poly1305(kek).encrypt(wrap_nonce, slot_aad, data key)
//! payload   = XChaCha20Poly1305(data key).encrypt(payload_nonce, every byte before it, TLV)
//! ```
//!
//! Opening a slot: the policy (ceiling first) → derive → `key_check` compared in constant time
//! (`WrongSecret`) → unwrap (`SlotCorrupt`) → payload (`Tampered`). A slot's associated data is
//! immutable — never its position, which a removal would shift and brick every later slot —
//! and the payload's covers the whole header, slot order and labels included, rewritten under
//! the data key on every save. Plaintext lives only in pre-sized `Zeroizing` buffers inside
//! `seal` and `open`, encrypted or parsed in place.

use std::fmt;
use std::sync::Arc;

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Key, Tag, XChaCha20Poly1305, XNonce};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

use super::error::{InputWhat, SizeWhat, VaultError, VaultResult};
use super::format::{
    check_label, check_store_id, check_writer, SlotBody, SlotId, SlotKind, SlotRecord, VaultFile,
    VaultHeader, VaultId, FORMAT_V1, NONCE_LEN, PAYLOAD_MAX, SALT_LEN, SLOT_AAD_DOMAIN, SLOT_MAX,
    TAG_LEN, VAULT_FILE_MAX, WRAPPED_DK_LEN,
};
use super::kdf::{derive_password_material, slot_keys, KdfPolicy, MATERIAL_LEN};
use super::keys::{DataKey, SlotSecret};
use super::password::{check_new_password, MAX_PASSWORD_BYTES};
use super::payload::VaultPayload;
use super::recovery::RecoveryKey;
use super::rng::VaultRng;
use crate::context::SecretString;

/// A slot to create. Its secret is checked before anything is drawn or derived: a password by
/// O2's rules ([`check_new_password`]), a label by the format's.
pub enum NewSlot {
    Password { secret: SecretString, label: String },
    RecoveryKey { key: RecoveryKey, label: String },
}

impl fmt::Debug for NewSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NewSlot::Password { label, .. } => write!(f, "NewSlot::Password({label:?}, ..)"),
            NewSlot::RecoveryKey { label, .. } => write!(f, "NewSlot::RecoveryKey({label:?}, ..)"),
        }
    }
}

/// An opened vault: its header, its data key and its authenticated payload.
pub struct UnlockedVault {
    header: VaultHeader,
    dk: Arc<DataKey>,
    payload: VaultPayload,
}

impl fmt::Debug for UnlockedVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UnlockedVault(..)")
    }
}

/// A new vault for store `store_id`: a fresh vault id and data key, one record per slot (at
/// least one a recovery key: ADR 0068 §3), sealed. Draws, in order: the vault id, the data key,
/// per slot its id, salt and wrap nonce, then the payload nonce. Nothing is drawn or derived
/// before every argument has been checked.
pub fn create(
    store_id: &str,
    writer: &str,
    payload: VaultPayload,
    slots: Vec<NewSlot>,
    policy: &KdfPolicy,
    rng: &mut dyn VaultRng,
) -> VaultResult<(Vec<u8>, UnlockedVault)> {
    if !slots
        .iter()
        .any(|slot| matches!(slot, NewSlot::RecoveryKey { .. }))
    {
        return Err(VaultError::InvalidInput(InputWhat::NoRecoverySlot));
    }
    if slots.len() > SLOT_MAX {
        return Err(VaultError::TooLarge {
            what: SizeWhat::Slots,
            limit: SLOT_MAX as u64,
        });
    }
    for slot in &slots {
        check_new_slot(slot)?;
    }
    check_store_id(store_id.as_bytes())
        .map_err(|_| VaultError::InvalidInput(InputWhat::StoreId))?;
    check_writer(writer.as_bytes()).map_err(|_| VaultError::InvalidInput(InputWhat::Writer))?;
    payload.check_size()?;
    let vault_id = VaultId(draw(rng)?);
    let dk = Arc::new(DataKey::generate(rng)?);
    let mut vault = UnlockedVault {
        header: VaultHeader {
            format_version: FORMAT_V1,
            writer: writer.to_string(),
            vault_id,
            store_id: store_id.to_string(),
            slots: Vec::with_capacity(slots.len()),
        },
        dk,
        payload,
    };
    for slot in slots {
        let record = vault.new_record(slot, policy, rng)?;
        vault.header.slots.push(record);
    }
    let bytes = vault.seal(writer, rng)?;
    Ok((bytes, vault))
}

fn draw<const N: usize>(rng: &mut dyn VaultRng) -> VaultResult<[u8; N]> {
    let mut out = [0u8; N];
    rng.fill(&mut out)?;
    Ok(out)
}

fn check_new_slot(slot: &NewSlot) -> VaultResult<()> {
    let label = match slot {
        NewSlot::Password { secret, label } => {
            check_new_password(secret).map_err(VaultError::PasswordRefused)?;
            label
        }
        NewSlot::RecoveryKey { label, .. } => label,
    };
    check_label(label.as_bytes()).map_err(|_| VaultError::InvalidInput(InputWhat::Label))?;
    Ok(())
}

impl UnlockedVault {
    pub fn header(&self) -> &VaultHeader {
        &self.header
    }

    pub fn payload(&self) -> &VaultPayload {
        &self.payload
    }

    pub fn payload_mut(&mut self) -> &mut VaultPayload {
        &mut self.payload
    }

    pub fn data_key(&self) -> &Arc<DataKey> {
        &self.dk
    }

    /// Adds a slot wrapping this vault's data key; checked like `create`'s.
    pub fn add_slot(
        &mut self,
        slot: NewSlot,
        policy: &KdfPolicy,
        rng: &mut dyn VaultRng,
    ) -> VaultResult<SlotId> {
        if self.header.slots.len() >= SLOT_MAX {
            return Err(VaultError::TooLarge {
                what: SizeWhat::Slots,
                limit: SLOT_MAX as u64,
            });
        }
        check_new_slot(&slot)?;
        let record = self.new_record(slot, policy, rng)?;
        let id = record.id;
        self.header.slots.push(record);
        Ok(id)
    }

    /// Removes a slot. Refuses the last recovery-key slot (ADR 0068 §3), and any removal that
    /// would leave no recovery-key slot or password slot `policy` accepts (T63; critic C6, S11).
    pub fn remove_slot(&mut self, id: SlotId, policy: &KdfPolicy) -> VaultResult<()> {
        let index = self
            .header
            .slots
            .iter()
            .position(|slot| slot.id == id)
            .ok_or(VaultError::SlotNotFound { slot: id })?;
        let recovery_slots = self
            .header
            .slots
            .iter()
            .filter(|slot| slot.kind == SlotKind::RecoveryKey)
            .count();
        if self.header.slots[index].kind == SlotKind::RecoveryKey && recovery_slots == 1 {
            return Err(VaultError::LastRecoverySlot);
        }
        let usable_left = self
            .header
            .slots
            .iter()
            .enumerate()
            .any(|(i, slot)| i != index && opens_under(slot, policy));
        if !usable_left {
            return Err(VaultError::NoUsableSlotLeft);
        }
        self.header.slots.remove(index);
        Ok(())
    }

    pub fn set_label(&mut self, id: SlotId, label: String) -> VaultResult<()> {
        check_label(label.as_bytes()).map_err(|_| VaultError::InvalidInput(InputWhat::Label))?;
        let slot = self
            .header
            .slots
            .iter_mut()
            .find(|slot| slot.id == id)
            .ok_or(VaultError::SlotNotFound { slot: id })?;
        slot.label = label;
        Ok(())
    }

    /// The vault's bytes: slot records verbatim (unknown ones included), the payload sealed
    /// under a fresh nonce. The payload's size is checked before anything is drawn; the header
    /// and the file's size after the nonce is drawn and before any byte is handed out; and the
    /// result is parsed back before it is returned: nothing `parse` refuses leaves (W7).
    pub fn seal(&self, writer: &str, rng: &mut dyn VaultRng) -> VaultResult<Vec<u8>> {
        check_writer(writer.as_bytes()).map_err(|_| VaultError::InvalidInput(InputWhat::Writer))?;
        let mut header = self.header.clone();
        header.writer = writer.to_string();
        let mut plain = self.payload.encode()?;
        let ct_len = plain.len() + TAG_LEN;
        let payload_nonce: [u8; NONCE_LEN] = draw(rng)?;
        let aad = header.encode_aad(&payload_nonce, ct_len)?;
        let total = aad.len() + ct_len;
        if total > VAULT_FILE_MAX {
            return Err(VaultError::TooLarge {
                what: SizeWhat::File,
                limit: VAULT_FILE_MAX as u64,
            });
        }
        let tag = XChaCha20Poly1305::new(Key::from_slice(self.dk.expose()))
            .encrypt_in_place_detached(XNonce::from_slice(&payload_nonce), &aad, &mut plain[..])
            .map_err(|_| VaultError::TooLarge {
                what: SizeWhat::Payload,
                limit: PAYLOAD_MAX as u64,
            })?;
        let mut bytes = Vec::with_capacity(total);
        bytes.extend_from_slice(&aad);
        bytes.extend_from_slice(&plain);
        bytes.extend_from_slice(&tag);
        VaultFile::parse(&bytes)?;
        Ok(bytes)
    }

    fn new_record(
        &self,
        slot: NewSlot,
        policy: &KdfPolicy,
        rng: &mut dyn VaultRng,
    ) -> VaultResult<SlotRecord> {
        let id = SlotId(draw(rng)?);
        let salt: [u8; SALT_LEN] = draw(rng)?;
        let wrap_nonce: [u8; NONCE_LEN] = draw(rng)?;
        let (kind, mut body, material, label) = match slot {
            NewSlot::Password { secret, label } => {
                let kdf = policy.create_params();
                let material = derive_password_material(&secret, &salt, &kdf, policy)?;
                let body = SlotBody::Password {
                    kdf,
                    salt,
                    wrap_nonce,
                    key_check: [0; 32],
                    wrapped_dk: [0; WRAPPED_DK_LEN],
                };
                (SlotKind::Password, body, material, label)
            }
            NewSlot::RecoveryKey { key, label } => {
                let mut material = Zeroizing::new([0u8; MATERIAL_LEN]);
                material.copy_from_slice(key.key_bytes());
                let body = SlotBody::RecoveryKey {
                    salt,
                    wrap_nonce,
                    key_check: [0; 32],
                    wrapped_dk: [0; WRAPPED_DK_LEN],
                };
                (SlotKind::RecoveryKey, body, material, label)
            }
        };
        let keys = slot_keys(&material[..], &salt, kind.byte());
        let aad = slot_aad(&self.header.vault_id, &id, kind, &body);
        let sealed = wrap_data_key(&self.dk, &keys.kek, &aad, &wrap_nonce)?;
        if let SlotBody::Password {
            key_check,
            wrapped_dk,
            ..
        }
        | SlotBody::RecoveryKey {
            key_check,
            wrapped_dk,
            ..
        } = &mut body
        {
            key_check.copy_from_slice(&keys.key_check[..]);
            *wrapped_dk = sealed;
        }
        Ok(SlotRecord {
            id,
            kind,
            body,
            label,
        })
    }
}

/// A slot `policy` would open: a recovery key always, a password slot whose parameters pass.
fn opens_under(slot: &SlotRecord, policy: &KdfPolicy) -> bool {
    match &slot.body {
        SlotBody::RecoveryKey { .. } => true,
        SlotBody::Password { kdf, .. } => policy.check(kdf).is_ok(),
        SlotBody::Opaque(_) => false,
    }
}

/// `SLOT_AAD_DOMAIN ‖ vault_id ‖ slot_id ‖ kind ‖ body[..end of salt]`: never the position.
pub(super) fn slot_aad(
    vault_id: &VaultId,
    slot: &SlotId,
    kind: SlotKind,
    body: &SlotBody,
) -> Vec<u8> {
    let kdf_block = body.kdf_block();
    let mut aad = Vec::with_capacity(SLOT_AAD_DOMAIN.len() + 16 + 16 + 1 + kdf_block.len());
    aad.extend_from_slice(SLOT_AAD_DOMAIN);
    aad.extend_from_slice(&vault_id.0);
    aad.extend_from_slice(&slot.0);
    aad.push(kind.byte());
    aad.extend_from_slice(&kdf_block);
    aad
}

fn wrap_data_key(
    dk: &DataKey,
    kek: &[u8; 32],
    aad: &[u8],
    nonce: &[u8; NONCE_LEN],
) -> VaultResult<[u8; WRAPPED_DK_LEN]> {
    let mut buf = Zeroizing::new([0u8; 32]);
    buf.copy_from_slice(dk.expose());
    let tag = XChaCha20Poly1305::new(Key::from_slice(kek))
        .encrypt_in_place_detached(XNonce::from_slice(nonce), aad, &mut buf[..])
        .map_err(|_| VaultError::TooLarge {
            what: SizeWhat::Payload,
            limit: PAYLOAD_MAX as u64,
        })?;
    let mut out = [0u8; WRAPPED_DK_LEN];
    out[..32].copy_from_slice(&buf[..]);
    out[32..].copy_from_slice(&tag);
    Ok(out)
}

/// The data key a slot wraps, or `None` when the tag does not verify (the buffer is zeroed).
fn unwrap_data_key(
    kek: &[u8; 32],
    aad: &[u8],
    nonce: &[u8; NONCE_LEN],
    wrapped: &[u8; WRAPPED_DK_LEN],
) -> Option<DataKey> {
    let mut key = Box::new([0u8; 32]);
    key.copy_from_slice(&wrapped[..32]);
    match XChaCha20Poly1305::new(Key::from_slice(kek)).decrypt_in_place_detached(
        XNonce::from_slice(nonce),
        aad,
        &mut key[..],
        Tag::from_slice(&wrapped[32..]),
    ) {
        Ok(()) => Some(DataKey::from_box(key)),
        Err(_) => {
            key.zeroize();
            None
        }
    }
}

impl VaultFile {
    /// Opens the vault with a slot secret: every slot of its kind is tried; `WrongSecret` only
    /// when each one's key check refused it, the policy's refusal when it refused them all
    /// (T22: open such a vault with its recovery key).
    pub fn open(&self, secret: &SlotSecret, policy: &KdfPolicy) -> VaultResult<UnlockedVault> {
        let kind = match secret {
            SlotSecret::Password(_) => SlotKind::Password,
            SlotSecret::RecoveryKey(_) => SlotKind::RecoveryKey,
        };
        let candidates: Vec<&SlotRecord> = self
            .header()
            .slots
            .iter()
            .filter(|slot| slot.kind == kind)
            .collect();
        if candidates.is_empty() {
            return Err(VaultError::NoUsableSlot {
                kinds: self.header().slots.iter().map(|slot| slot.kind).collect(),
            });
        }
        if let SlotSecret::Password(password) = secret {
            if password.expose().len() > MAX_PASSWORD_BYTES {
                // No password slot holds a longer one (`check_new_password`): nothing to derive.
                return Err(VaultError::WrongSecret { kinds: vec![kind] });
            }
        }
        let mut refused = None;
        let mut tried = false;
        for slot in candidates {
            let material = match (&slot.body, secret) {
                (SlotBody::Password { kdf, salt, .. }, SlotSecret::Password(password)) => {
                    match derive_password_material(password, salt, kdf, policy) {
                        Ok(material) => material,
                        Err(
                            e @ (VaultError::KdfTooWeak { .. } | VaultError::KdfOutOfRange { .. }),
                        ) => {
                            if refused.is_none() {
                                refused = Some(e);
                            }
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                (SlotBody::RecoveryKey { .. }, SlotSecret::RecoveryKey(key)) => {
                    let mut material = Zeroizing::new([0u8; MATERIAL_LEN]);
                    material.copy_from_slice(key.key_bytes());
                    material
                }
                // `parse` ties a known kind to its body; nothing else is a candidate.
                _ => continue,
            };
            tried = true;
            if let Some(dk) = self.try_slot(slot, &material[..])? {
                return self.open_payload(dk);
            }
        }
        Err(match refused {
            Some(e) if !tried => e,
            _ => VaultError::WrongSecret { kinds: vec![kind] },
        })
    }

    /// Opens the payload with a data key already held (a holder, the desktop, a session):
    /// `KeyStale` when the vault was replaced since that key was taken.
    pub fn open_with_key(&self, dk: &DataKey, expected: VaultId) -> VaultResult<UnlockedVault> {
        if self.header().vault_id != expected {
            return Err(VaultError::KeyStale);
        }
        self.open_payload(DataKey::from_bytes(dk.expose()))
    }

    /// Each slot's verdict under `policy`, without deriving anything (`lock status`; L3).
    pub fn slot_policy(&self, policy: &KdfPolicy) -> Vec<(SlotId, VaultResult<()>)> {
        self.header()
            .slots
            .iter()
            .map(|slot| {
                let verdict = match &slot.body {
                    SlotBody::Password { kdf, .. } => policy.check(kdf),
                    _ => Ok(()),
                };
                (slot.id, verdict)
            })
            .collect()
    }

    /// `Ok(None)`: the key check says the secret is not this slot's. `Ok(Some(key))`: it is,
    /// and the wrapped key decrypted. `SlotCorrupt`: it is, and the wrapped key did not.
    fn try_slot(&self, slot: &SlotRecord, material: &[u8]) -> VaultResult<Option<DataKey>> {
        let (salt, wrap_nonce, stored_check, wrapped) = match &slot.body {
            SlotBody::Password {
                salt,
                wrap_nonce,
                key_check,
                wrapped_dk,
                ..
            }
            | SlotBody::RecoveryKey {
                salt,
                wrap_nonce,
                key_check,
                wrapped_dk,
            } => (salt, wrap_nonce, key_check, wrapped_dk),
            SlotBody::Opaque(_) => return Ok(None),
        };
        let keys = slot_keys(material, salt, slot.kind.byte());
        if !bool::from(keys.key_check[..].ct_eq(&stored_check[..])) {
            return Ok(None);
        }
        let aad = slot_aad(&self.header().vault_id, &slot.id, slot.kind, &slot.body);
        unwrap_data_key(&keys.kek, &aad, wrap_nonce, wrapped)
            .map(Some)
            .ok_or(VaultError::SlotCorrupt { slot: slot.id })
    }

    fn open_payload(&self, dk: DataKey) -> VaultResult<UnlockedVault> {
        let (ct, tag) = self.payload_ct.split_at(self.payload_ct.len() - TAG_LEN);
        let mut plain = Zeroizing::new(ct.to_vec());
        XChaCha20Poly1305::new(Key::from_slice(dk.expose()))
            .decrypt_in_place_detached(
                XNonce::from_slice(&self.payload_nonce),
                &self.payload_aad,
                &mut plain[..],
                Tag::from_slice(tag),
            )
            .map_err(|_| VaultError::Tampered)?;
        let payload = VaultPayload::decode(&plain)?;
        Ok(UnlockedVault {
            header: self.header().clone(),
            dk: Arc::new(dk),
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::error::{MalformedWhat, PasswordProblem};
    use crate::vault::kdf::DERIVATIONS;
    use crate::vault::keys::SlotInput;
    use crate::vault::payload::{ENTRY_MAX, VALUE_MAX};
    use crate::vault::rng::{OsRandom, ScriptedRandom};
    use crate::vault::testutil::{hex, seq, seq_array};

    const PASSWORD: &str = "correct horse battery staple";
    const IDENTITY: &str =
        "AGE-SECRET-KEY-1YQSJYGEYY5NZW2PF9G4JCTFW9UCRZV3NXS6NVDEC8YARK0PA8CLSWXGG7Q";
    const STORE: &str = "0123456789abcdef";
    const WRITER: &str = "apprafter 0.0.0-test";
    const FAST: &KdfPolicy = &KdfPolicy::INSECURE_FAST;

    fn identity() -> age::x25519::Identity {
        IDENTITY.parse().unwrap()
    }

    fn payload() -> VaultPayload {
        let mut payload = VaultPayload::new(&identity());
        payload
            .set_hetzner_token("prod", SecretString::new("A1b2C3d4".repeat(8)))
            .unwrap();
        payload
    }

    fn recovery(byte: u8) -> RecoveryKey {
        RecoveryKey::generate(&mut ScriptedRandom::new(vec![vec![byte; 32]])).unwrap()
    }

    fn slots(password: &str, recovery_byte: u8) -> Vec<NewSlot> {
        vec![
            NewSlot::Password {
                secret: SecretString::new(password),
                label: "lock password".into(),
            },
            NewSlot::RecoveryKey {
                key: recovery(recovery_byte),
                label: "recovery key".into(),
            },
        ]
    }

    /// A fast-parameter vault made with OS randomness: its bytes, and the vault.
    fn vault() -> (Vec<u8>, UnlockedVault) {
        create(
            STORE,
            WRITER,
            payload(),
            slots(PASSWORD, 0x11),
            FAST,
            &mut OsRandom,
        )
        .unwrap()
    }

    fn password(text: &str) -> SlotSecret {
        SlotSecret::Password(SecretString::new(text))
    }

    fn recovery_secret(byte: u8) -> SlotSecret {
        SlotSecret::RecoveryKey(recovery(byte))
    }

    fn derivations() -> u32 {
        DERIVATIONS.with(std::cell::Cell::get)
    }

    /// `file` with its header replaced by `header` and its nonce and ciphertext kept.
    fn with_header(file: &VaultFile, header: &VaultHeader) -> VaultFile {
        let mut bytes = header
            .encode_aad(&file.payload_nonce, file.payload_ct.len())
            .unwrap();
        bytes.extend_from_slice(&file.payload_ct);
        VaultFile::parse(&bytes).unwrap()
    }

    /// The draws of a two-slot `create`, in order, from the v1 vectors' fixed bytes.
    fn vector_draws() -> Vec<Vec<u8>> {
        vec![
            seq(0x60, 16),
            seq(0x40, 32),
            seq(0x70, 16),
            seq(0x80, 16),
            seq(0x90, 24),
            seq(0xa8, 16),
            seq(0xb8, 16),
            seq(0xc8, 24),
            seq(0xe0, 24),
        ]
    }

    /// A payload whose encoding is exactly `len` bytes (any length above 31 + the identity's).
    fn payload_of_len(len: usize) -> VaultPayload {
        const OVERHEAD: usize = 1 + "target/t0000/hetzner-token".len() + 2;
        let mut payload = VaultPayload::new(&identity());
        let mut left = len - 2 - (1 + "age-identity".len() + 2 + IDENTITY.len());
        let mut i = 0;
        while left > 0 {
            let value = if left >= 2 * (OVERHEAD + VALUE_MAX) {
                VALUE_MAX
            } else if left > OVERHEAD + VALUE_MAX {
                (left - 2 * OVERHEAD) / 2
            } else {
                left - OVERHEAD
            };
            payload
                .set_hetzner_token(&format!("t{i:04}"), SecretString::new("x".repeat(value)))
                .unwrap();
            left -= OVERHEAD + value;
            i += 1;
        }
        payload
    }

    #[test]
    fn a_vault_opens_with_either_secret_to_one_data_key() {
        let (bytes, created) = vault();
        let file = VaultFile::parse(&bytes).unwrap();
        let by_password = file.open(&password(PASSWORD), FAST).unwrap();
        let by_recovery = file.open(&recovery_secret(0x11), FAST).unwrap();
        assert_eq!(by_password.data_key().expose(), created.data_key().expose());
        assert_eq!(by_recovery.data_key().expose(), created.data_key().expose());
        assert_eq!(by_password.header(), created.header());
        assert_eq!(
            by_recovery
                .payload()
                .hetzner_token("prod")
                .unwrap()
                .expose(),
            "A1b2C3d4".repeat(8)
        );
        assert_eq!(
            by_password
                .payload()
                .age_identity()
                .unwrap()
                .to_public()
                .to_string(),
            identity().to_public().to_string()
        );
    }

    #[test]
    fn a_wrong_secret_is_wrong_secret_never_absent() {
        let (bytes, _) = vault();
        let file = VaultFile::parse(&bytes).unwrap();
        match file.open(&password("correct horse battery stapler"), FAST) {
            Err(VaultError::WrongSecret { kinds }) => assert_eq!(kinds, [SlotKind::Password]),
            other => panic!("{:?}", other.map(|_| "opened")),
        }
        match file.open(&recovery_secret(0x12), FAST) {
            Err(e @ VaultError::WrongSecret { .. }) => {
                assert!(
                    matches!(&e, VaultError::WrongSecret { kinds } if *kinds == [SlotKind::RecoveryKey])
                );
                assert_eq!(e.to_string(), "this recovery key does not open the vault");
            }
            other => panic!("{:?}", other.map(|_| "opened")),
        }
        // Two kinds tried (typed text, B27) are named in the order tried.
        let both = VaultError::WrongSecret {
            kinds: vec![SlotKind::Password, SlotKind::OsPassword],
        };
        assert_eq!(
            both.to_string(),
            "this lock password or OS password does not open the vault"
        );
    }

    #[test]
    fn a_password_no_slot_can_hold_is_refused_unhashed() {
        let (bytes, _) = vault();
        let file = VaultFile::parse(&bytes).unwrap();
        let before = derivations();
        assert!(matches!(
            file.open(&password(&"a".repeat(MAX_PASSWORD_BYTES + 1)), FAST),
            Err(VaultError::WrongSecret { .. })
        ));
        assert_eq!(derivations(), before);
    }

    #[test]
    fn a_secret_of_a_kind_the_vault_lacks_is_no_usable_slot() {
        let (bytes, _) = vault();
        let file = VaultFile::parse(&bytes).unwrap();
        let mut header = file.header().clone();
        header
            .slots
            .retain(|slot| slot.kind == SlotKind::RecoveryKey);
        let only_recovery = with_header(&file, &header);
        match only_recovery.open(&password(PASSWORD), FAST) {
            Err(VaultError::NoUsableSlot { kinds }) => assert_eq!(kinds, [SlotKind::RecoveryKey]),
            other => panic!("{:?}", other.map(|_| "opened")),
        }
    }

    #[test]
    fn a_weak_slot_is_refused_by_the_release_policy_and_the_recovery_key_still_opens() {
        let (bytes, _) = vault();
        let file = VaultFile::parse(&bytes).unwrap();
        let before = derivations();
        assert!(matches!(
            file.open(&password(PASSWORD), &KdfPolicy::RELEASE),
            Err(VaultError::KdfTooWeak { .. })
        ));
        assert_eq!(derivations(), before, "a refused slot derives nothing");
        assert!(file
            .open(&recovery_secret(0x11), &KdfPolicy::RELEASE)
            .is_ok());
        let verdicts = file.slot_policy(&KdfPolicy::RELEASE);
        assert_eq!(verdicts[0].0, file.header().slots[0].id);
        assert!(matches!(verdicts[0].1, Err(VaultError::KdfTooWeak { .. })));
        assert!(verdicts[1].1.is_ok());
    }

    #[test]
    fn the_payload_is_bound_to_the_whole_header() {
        let (bytes, _) = vault();
        let file = VaultFile::parse(&bytes).unwrap();
        let mut header = file.header().clone();
        header.slots[0].label = "Lock password".into();
        let relabelled = with_header(&file, &header);
        assert!(matches!(
            relabelled.open(&password(PASSWORD), FAST),
            Err(VaultError::Tampered)
        ));
        assert!(matches!(
            relabelled.open(&recovery_secret(0x11), FAST),
            Err(VaultError::Tampered)
        ));
    }

    #[test]
    fn a_slot_is_bound_to_its_vault() {
        let (bytes_a, _) = vault();
        let (bytes_b, _) = vault();
        let a = VaultFile::parse(&bytes_a).unwrap();
        let b = VaultFile::parse(&bytes_b).unwrap();
        let mut header = b.header().clone();
        header.slots[0] = a.header().slots[0].clone();
        let transplanted = with_header(&b, &header);
        let slot = a.header().slots[0].id;
        assert!(matches!(
            transplanted.open(&password(PASSWORD), FAST),
            Err(VaultError::SlotCorrupt { slot: s }) if s == slot
        ));
    }

    #[test]
    fn a_damaged_wrap_is_slot_corrupt_and_a_damaged_check_a_wrong_secret() {
        let (bytes, _) = vault();
        let file = VaultFile::parse(&bytes).unwrap();
        let slot = file.header().slots[0].id;
        let mut header = file.header().clone();
        if let SlotBody::Password { wrapped_dk, .. } = &mut header.slots[0].body {
            wrapped_dk[0] ^= 1;
        }
        assert!(matches!(
            with_header(&file, &header).open(&password(PASSWORD), FAST),
            Err(VaultError::SlotCorrupt { slot: s }) if s == slot
        ));
        let mut header = file.header().clone();
        if let SlotBody::Password { key_check, .. } = &mut header.slots[0].body {
            key_check[0] ^= 1;
        }
        assert!(matches!(
            with_header(&file, &header).open(&password(PASSWORD), FAST),
            Err(VaultError::WrongSecret { .. })
        ));
    }

    #[test]
    fn a_held_key_opens_its_own_vault_only() {
        let (bytes, created) = vault();
        let file = VaultFile::parse(&bytes).unwrap();
        let dk = created.data_key();
        assert!(file.open_with_key(dk, file.header().vault_id).is_ok());
        assert!(matches!(
            file.open_with_key(dk, VaultId([0; 16])),
            Err(VaultError::KeyStale)
        ));
        let wrong = DataKey::from_bytes(&[0x99; 32]);
        assert!(matches!(
            file.open_with_key(&wrong, file.header().vault_id),
            Err(VaultError::Tampered)
        ));
    }

    #[test]
    fn every_seal_draws_a_new_payload_nonce() {
        let (_, opened) = vault();
        let first = VaultFile::parse(&opened.seal(WRITER, &mut OsRandom).unwrap()).unwrap();
        let second = VaultFile::parse(&opened.seal(WRITER, &mut OsRandom).unwrap()).unwrap();
        assert_ne!(first.payload_nonce, second.payload_nonce);
        for file in [&first, &second] {
            assert!(file
                .open_with_key(opened.data_key(), file.header().vault_id)
                .is_ok());
        }
        let desktop = VaultFile::parse(
            &opened
                .seal("apprafter-desktop 0.1.0", &mut OsRandom)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(desktop.header().writer, "apprafter-desktop 0.1.0");
    }

    #[test]
    fn create_checks_everything_before_drawing_a_byte_or_deriving() {
        let nothing = || ScriptedRandom::new(Vec::new());
        let before = derivations();
        let only_password = vec![NewSlot::Password {
            secret: SecretString::new(PASSWORD),
            label: "lock password".into(),
        }];
        assert!(matches!(
            create(
                STORE,
                WRITER,
                payload(),
                only_password,
                FAST,
                &mut nothing()
            ),
            Err(VaultError::InvalidInput(InputWhat::NoRecoverySlot))
        ));
        assert!(matches!(
            create(STORE, WRITER, payload(), slots("", 1), FAST, &mut nothing()),
            Err(VaultError::PasswordRefused(PasswordProblem::Empty))
        ));
        let shaped = "QQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK";
        assert!(matches!(
            create(
                STORE,
                WRITER,
                payload(),
                slots(shaped, 1),
                FAST,
                &mut nothing()
            ),
            Err(VaultError::PasswordRefused(
                PasswordProblem::RecoveryKeyShape
            ))
        ));
        assert!(matches!(
            create(
                STORE,
                WRITER,
                payload(),
                slots(&"a".repeat(1025), 1),
                FAST,
                &mut nothing()
            ),
            Err(VaultError::PasswordRefused(PasswordProblem::TooLong {
                max: 1024
            }))
        ));
        let mut bad_label = slots(PASSWORD, 1);
        if let NewSlot::RecoveryKey { label, .. } = &mut bad_label[1] {
            *label = "bell\u{7}".into();
        }
        assert!(matches!(
            create(STORE, WRITER, payload(), bad_label, FAST, &mut nothing()),
            Err(VaultError::InvalidInput(InputWhat::Label))
        ));
        let seventeen = (0..17u8)
            .map(|i| NewSlot::RecoveryKey {
                key: recovery(i),
                label: String::new(),
            })
            .collect();
        assert!(matches!(
            create(STORE, WRITER, payload(), seventeen, FAST, &mut nothing()),
            Err(VaultError::TooLarge {
                what: SizeWhat::Slots,
                ..
            })
        ));
        assert!(matches!(
            create(
                "has space",
                WRITER,
                payload(),
                slots(PASSWORD, 1),
                FAST,
                &mut nothing()
            ),
            Err(VaultError::InvalidInput(InputWhat::StoreId))
        ));
        assert!(matches!(
            create(
                STORE,
                "",
                payload(),
                slots(PASSWORD, 1),
                FAST,
                &mut nothing()
            ),
            Err(VaultError::InvalidInput(InputWhat::Writer))
        ));
        assert_eq!(derivations(), before);
    }

    #[test]
    fn create_draws_in_the_documented_order() {
        let mut rng = ScriptedRandom::new(vector_draws());
        let (bytes, vault) = create(
            STORE,
            WRITER,
            payload(),
            slots(PASSWORD, 0x11),
            FAST,
            &mut rng,
        )
        .unwrap();
        assert_eq!(rng.drawn(), 9);
        let file = VaultFile::parse(&bytes).unwrap();
        assert_eq!(file.header().vault_id, VaultId(seq_array(0x60)));
        assert_eq!(vault.data_key().expose(), &seq_array::<32>(0x40));
        assert_eq!(file.header().slots[0].id, SlotId(seq_array(0x70)));
        assert_eq!(file.header().slots[1].id, SlotId(seq_array(0xa8)));
        assert_eq!(file.payload_nonce, seq_array::<24>(0xe0));
    }

    #[test]
    fn a_failing_generator_fails_create_at_every_draw() {
        for draw in 0..9 {
            let earlier = vector_draws()[..draw].to_vec();
            let result = create(
                STORE,
                WRITER,
                payload(),
                slots(PASSWORD, 0x11),
                FAST,
                &mut ScriptedRandom::failing_at(draw, earlier),
            );
            assert!(
                matches!(result, Err(VaultError::RandomUnavailable { .. })),
                "draw {draw}"
            );
        }
    }

    #[test]
    fn two_slots_drawn_with_one_id_never_leave_seal() {
        let mut draws = vector_draws();
        draws[5] = draws[2].clone();
        assert!(matches!(
            create(
                STORE,
                WRITER,
                payload(),
                slots(PASSWORD, 0x11),
                FAST,
                &mut ScriptedRandom::new(draws)
            ),
            Err(VaultError::Malformed {
                what: MalformedWhat::DuplicateSlotId,
                ..
            })
        ));
    }

    #[test]
    fn slots_are_added_relabelled_and_removed_under_the_rules() {
        let (_, mut v) = vault();
        let second = v
            .add_slot(
                NewSlot::Password {
                    secret: SecretString::new("another lock password"),
                    label: "second".into(),
                },
                FAST,
                &mut OsRandom,
            )
            .unwrap();
        let file = VaultFile::parse(&v.seal(WRITER, &mut OsRandom).unwrap()).unwrap();
        assert!(file.open(&password("another lock password"), FAST).is_ok());
        assert!(file.open(&password(PASSWORD), FAST).is_ok());
        v.set_label(second, "renamed".into()).unwrap();
        assert_eq!(v.header().slots[2].label, "renamed");
        assert!(matches!(
            v.set_label(second, "two\nlines".into()),
            Err(VaultError::InvalidInput(InputWhat::Label))
        ));
        assert!(matches!(
            v.set_label(SlotId([0; 16]), "x".into()),
            Err(VaultError::SlotNotFound { .. })
        ));
        let recovery_id = v.header().slots[1].id;
        assert!(matches!(
            v.remove_slot(recovery_id, FAST),
            Err(VaultError::LastRecoverySlot)
        ));
        let spare = v
            .add_slot(
                NewSlot::RecoveryKey {
                    key: recovery(0x22),
                    label: "spare".into(),
                },
                FAST,
                &mut OsRandom,
            )
            .unwrap();
        v.remove_slot(recovery_id, FAST).unwrap();
        assert!(matches!(
            v.remove_slot(spare, FAST),
            Err(VaultError::LastRecoverySlot)
        ));
        v.remove_slot(second, FAST).unwrap();
        assert!(matches!(
            v.remove_slot(SlotId([0; 16]), FAST),
            Err(VaultError::SlotNotFound { .. })
        ));
        assert_eq!(v.header().slots.len(), 2);
        let file = VaultFile::parse(&v.seal(WRITER, &mut OsRandom).unwrap()).unwrap();
        assert!(file.open(&recovery_secret(0x22), FAST).is_ok());
    }

    #[test]
    fn a_removal_never_leaves_only_slots_this_build_refuses() {
        let (_, mut v) = vault();
        let second = v
            .add_slot(
                NewSlot::Password {
                    secret: SecretString::new("another lock password"),
                    label: "second".into(),
                },
                FAST,
                &mut OsRandom,
            )
            .unwrap();
        // A vault without a recovery slot (damaged, or written elsewhere): two fast password slots.
        v.header
            .slots
            .retain(|slot| slot.kind != SlotKind::RecoveryKey);
        let first = v.header.slots[0].id;
        assert!(matches!(
            v.remove_slot(first, &KdfPolicy::RELEASE),
            Err(VaultError::NoUsableSlotLeft)
        ));
        v.remove_slot(first, FAST).unwrap();
        assert!(matches!(
            v.remove_slot(second, FAST),
            Err(VaultError::NoUsableSlotLeft)
        ));
    }

    #[test]
    fn sixteen_slots_and_no_more() {
        let (_, mut v) = vault();
        for i in 0..14u8 {
            v.add_slot(
                NewSlot::RecoveryKey {
                    key: recovery(0x30 + i),
                    label: String::new(),
                },
                FAST,
                &mut OsRandom,
            )
            .unwrap();
        }
        assert_eq!(v.header().slots.len(), SLOT_MAX);
        assert!(matches!(
            v.add_slot(
                NewSlot::RecoveryKey {
                    key: recovery(0x50),
                    label: String::new()
                },
                FAST,
                &mut OsRandom
            ),
            Err(VaultError::TooLarge {
                what: SizeWhat::Slots,
                ..
            })
        ));
        VaultFile::parse(&v.seal(WRITER, &mut OsRandom).unwrap()).unwrap();
    }

    /// W7: what `seal` refuses it refuses before writing; what it hands out, `parse` reads.
    #[test]
    fn no_seal_hands_out_a_file_parse_refuses() {
        let nothing = || ScriptedRandom::new(Vec::new());
        // Entries: exactly ENTRY_MAX seals and opens; one more is refused before any draw.
        let mut full = VaultPayload::new(&identity());
        for i in 0..(ENTRY_MAX - 1) {
            full.set_hetzner_token(&format!("t{i}"), SecretString::new("x"))
                .unwrap();
        }
        let (bytes, v) =
            create(STORE, WRITER, full, slots(PASSWORD, 1), FAST, &mut OsRandom).unwrap();
        let opened = VaultFile::parse(&bytes)
            .unwrap()
            .open_with_key(v.data_key(), v.header().vault_id)
            .unwrap();
        assert_eq!(opened.payload().keys().count(), ENTRY_MAX);
        let mut over = VaultPayload::new(&identity());
        for i in 0..ENTRY_MAX {
            over.set_hetzner_token(&format!("t{i}"), SecretString::new("x"))
                .unwrap();
        }
        assert!(matches!(
            create(
                STORE,
                WRITER,
                over,
                slots(PASSWORD, 1),
                FAST,
                &mut nothing()
            ),
            Err(VaultError::TooLarge {
                what: SizeWhat::Entries,
                ..
            })
        ));
        // Bytes: exactly PAYLOAD_MAX seals and opens; one more is refused before any draw.
        let exact = payload_of_len(PAYLOAD_MAX);
        assert_eq!(exact.check_size().unwrap(), PAYLOAD_MAX);
        let (bytes, v) = create(
            STORE,
            WRITER,
            exact,
            slots(PASSWORD, 1),
            FAST,
            &mut OsRandom,
        )
        .unwrap();
        assert!(VaultFile::parse(&bytes)
            .unwrap()
            .open_with_key(v.data_key(), v.header().vault_id)
            .is_ok());
        assert!(matches!(
            create(
                STORE,
                WRITER,
                payload_of_len(PAYLOAD_MAX + 1),
                slots(PASSWORD, 1),
                FAST,
                &mut nothing()
            ),
            Err(VaultError::TooLarge {
                what: SizeWhat::Payload,
                ..
            })
        ));
        // File: sixteen largest opaque slots and a full payload pass 2 MiB.
        let (_, mut v) = create(
            STORE,
            "w",
            payload_of_len(PAYLOAD_MAX),
            slots(PASSWORD, 1),
            FAST,
            &mut OsRandom,
        )
        .unwrap();
        v.header.slots = (0..16u8)
            .map(|i| SlotRecord {
                id: SlotId([i; 16]),
                kind: SlotKind::Unknown(0x7f),
                body: SlotBody::Opaque(vec![i; 65_515]),
                label: String::new(),
            })
            .collect();
        assert!(matches!(
            v.seal("w", &mut OsRandom),
            Err(VaultError::TooLarge {
                what: SizeWhat::File,
                ..
            })
        ));
    }

    #[test]
    fn random_payloads_seal_and_open_back() {
        let (_, mut v) = vault();
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..24 {
            let mut payload = VaultPayload::new(&identity());
            for i in 0..(next() % 40) {
                let len = 1 + (next() % VALUE_MAX as u64) as usize;
                payload
                    .set_hetzner_token(&format!("r{i}"), SecretString::new("y".repeat(len)))
                    .unwrap();
            }
            if payload.check_size().is_err() {
                continue;
            }
            *v.payload_mut() = payload;
            let bytes = v.seal(WRITER, &mut OsRandom).unwrap();
            let back = VaultFile::parse(&bytes)
                .unwrap()
                .open_with_key(v.data_key(), v.header().vault_id)
                .unwrap();
            assert_eq!(
                back.payload().keys().collect::<Vec<_>>(),
                v.payload().keys().collect::<Vec<_>>(),
                "round {round}"
            );
        }
    }

    #[test]
    fn no_debug_output_shows_a_secret() {
        let fold = |text: &str| text.to_lowercase().replace(['-', '_', ' ', '\n'], "");
        let key = recovery(0x11);
        let shown = key.to_display();
        let (_, v) = vault();
        // Recovery-key shaped, `1` at position 2: a mistyped key, kept as a password.
        let mistyped = "QQ1SY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK";
        let secrets = [
            fold(PASSWORD),
            fold(mistyped),
            fold(&"A1b2C3d4".repeat(8)),
            fold(IDENTITY),
            fold(&shown.expose()["APPRAFTER-RECOVERY-KEY-1\n".len()..]),
            hex(v.data_key().expose()),
        ];
        let outputs = [
            format!("{v:?}"),
            format!("{:?}", v.payload()),
            format!("{:?}", DataKey::from_bytes(v.data_key().expose())),
            format!("{key:?}"),
            format!("{:?}", password(PASSWORD)),
            format!("{:?}", recovery_secret(0x11)),
            format!("{:?}", slots(PASSWORD, 0x11)),
            format!("{:?}", SlotInput::from_input(SecretString::new(PASSWORD))),
            format!("{:?}", SlotInput::from_input(SecretString::new(mistyped))),
            format!("{:?}", key.confirm(shown.expose()).unwrap()),
        ];
        for output in &outputs {
            let folded = fold(output);
            for secret in &secrets {
                assert!(!folded.contains(secret.as_str()), "{output} shows a secret");
            }
        }
    }
}
