// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A vault's rotation (D.4b B6, B63, B68; ADR 0069 §7; overview §4.4, §4.5, §5.3).
//!
//! ```text
//! keep    a slot of the opened vault, proven by its own key check (never PAM: B13)
//! rotate  the vault re-read and opened under the store lock (B19) →              R2
//!         a new data key, the vault id minted at plan time, a NEW store key; the
//!         store id as it is; each kept slot under its own id with a new salt and
//!         wrap nonce; each new slot under its minted id; every other slot dropped
//!         and listed; the payload as re-read, the new store key as `age-identity`,
//!         the current one, byte for byte, as `age-identity-retiring`
//! retire  the same vault without `age-identity-retiring`                          R4
//! ```
//!
//! Nothing here writes a file or prompts: the caller writes both vaults through
//! `vault::file::replace`, durable before it returns (B78), and re-seals every cache between
//! the two writes (d4b-c2r, c2a). A child of `seal`, whose slot builder it shares.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use super::{check_new_slot, draw, unwrap_slot, Fresh, NewSlot, UnlockedVault};
use crate::context::SecretString;
use crate::vault::error::{InputWhat, SizeWhat, VaultError, VaultResult};
use crate::vault::format::{
    check_writer, SlotBody, SlotId, SlotKind, SlotRecord, VaultHeader, VaultId, FORMAT_V1, SLOT_MAX,
};
use crate::vault::kdf::{derive_password_material, os_password_material, KdfPolicy};
use crate::vault::keys::{DataKey, MachineId};
use crate::vault::password::MAX_PASSWORD_BYTES;
use crate::vault::payload::draw_identity;
use crate::vault::recovery::RecoveryKey;
use crate::vault::rng::VaultRng;

/// The ids of a rotation, minted at plan time (B6, B80): the rotated vault's id, and one
/// slot id per new slot [`rotate`] is given, in the same order — so a new recovery key is
/// shown with its slot's id before anything is written. Kept slots keep their own ids. (A
/// Windows Hello slot's challenge nonce joins them in d4b-w1.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationIds {
    pub vault_id: VaultId,
    pub new_slots: Vec<SlotId>,
}

impl RotationIds {
    /// A vault id, then `new_slots` slot ids, drawn in that order.
    pub fn draw(new_slots: usize, rng: &mut dyn VaultRng) -> VaultResult<RotationIds> {
        let vault_id = VaultId(draw(rng)?);
        let new_slots = (0..new_slots)
            .map(|_| draw(rng).map(SlotId))
            .collect::<VaultResult<Vec<_>>>()?;
        Ok(RotationIds {
            vault_id,
            new_slots,
        })
    }
}

/// The secret of a slot a rotation keeps (B68: the opener, or typed now): a lock password,
/// this computer's account password with its machine id, or the recovery key. Its slot's key
/// check decides; PAM is never asked (B13) — the account password was checked with the
/// system when its slot was added, so this makes no new account-password slot (what d4b-d's
/// G-OS6 watches). Windows Hello's is d4b-w1's.
pub enum KeptMaterial {
    Password(SecretString),
    AccountPassword(SecretString, MachineId),
    RecoveryKey(RecoveryKey),
}

impl KeptMaterial {
    /// The kind of slot this secret opens.
    fn kind(&self) -> SlotKind {
        match self {
            KeptMaterial::Password(_) => SlotKind::Password,
            KeptMaterial::AccountPassword(..) => SlotKind::OsPassword,
            KeptMaterial::RecoveryKey(_) => SlotKind::RecoveryKey,
        }
    }
}

impl fmt::Debug for KeptMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeptMaterial::Password(_) => "KeptMaterial::Password(..)",
            KeptMaterial::AccountPassword(..) => "KeptMaterial::AccountPassword(..)",
            KeptMaterial::RecoveryKey(_) => "KeptMaterial::RecoveryKey(..)",
        })
    }
}

/// A slot of an opened vault whose secret is proven: it passed the slot's key check and the
/// slot's wrap opened to that vault's data key. Only [`UnlockedVault::keep`] makes one, and
/// [`rotate`] takes it only for the vault it was proven against — so a rotation never keeps a
/// slot under a secret that is not its own (a typo would otherwise replace the slot's secret
/// without a word). No `Clone`; `..` in `Debug`.
pub struct KeptSlot {
    vault_id: VaultId,
    id: SlotId,
    secret: KeptMaterial,
}

impl KeptSlot {
    /// The slot's id — also its id in the rotated vault.
    pub fn id(&self) -> SlotId {
        self.id
    }

    /// Whether `slot` is still the record this slot was proven against: the same kind and,
    /// for an account password, made on the same computer. Within one vault id a slot id
    /// names one record — a write that changes a slot's secret gives it a new id — so only a
    /// forged vault fails this.
    fn fits(&self, slot: &SlotRecord) -> bool {
        let kind = self.secret.kind();
        slot.kind == kind
            && match &self.secret {
                KeptMaterial::AccountPassword(_, machine) => slot.made_on(machine),
                _ => true,
            }
    }
}

impl fmt::Debug for KeptSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeptSlot({}, {:?})", self.id, self.secret)
    }
}

/// A rotated vault (R2): its bytes, ready for `vault::file::replace`; the vault itself,
/// opened (its payload holds the retiring store key); and the current vault's slots it
/// dropped, by id, in header order.
pub struct Rotated {
    pub bytes: Vec<u8>,
    pub vault: UnlockedVault,
    pub dropped: Vec<SlotId>,
}

impl fmt::Debug for Rotated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rotated")
            .field("bytes", &self.bytes.len())
            .field("vault", &self.vault)
            .field("dropped", &self.dropped)
            .finish()
    }
}

impl UnlockedVault {
    /// This vault without its retiring store key (R4, B63): the same data key, vault id,
    /// slots and labels — no slot is re-made, so nothing is asked — and every other payload
    /// entry, sealed by `writer` under a fresh payload nonce, its one draw. Refused before
    /// drawing when the payload retires no store key (`InvalidInput(NothingToRetire)`): a
    /// finish that finds none skips R4 (§5.4). This vault itself is unchanged.
    pub fn retire_identity(&self, writer: &str, rng: &mut dyn VaultRng) -> VaultResult<Vec<u8>> {
        let retired = UnlockedVault {
            header: self.header.clone(),
            dk: Arc::clone(&self.dk),
            payload: self.payload.retired()?,
        };
        retired.seal(writer, rng)
    }

    /// Proves that `secret` is slot `id`'s in this vault, for [`rotate`] (B6, B68): the
    /// slot's own key check under `policy` — never PAM (B13) — and its wrap must open to this
    /// vault's data key. `SlotNotFound` for no such slot; `NoUsableSlot`, deriving nothing,
    /// for a slot `secret` cannot open here — another kind, another computer's
    /// account-password slot (its machine secret is not here: B68 drops it), a slot this
    /// build keeps opaque; `WrongSecret` when the key check refuses it (an over-long
    /// password unhashed, as `open` does); the policy's refusal of the slot's costs;
    /// `SlotCorrupt` for a wrap that does not open to this vault's data key.
    pub fn keep(
        &self,
        id: SlotId,
        secret: KeptMaterial,
        policy: &KdfPolicy,
    ) -> VaultResult<KeptSlot> {
        let slot = self
            .header
            .slots
            .iter()
            .find(|slot| slot.id == id)
            .ok_or(VaultError::SlotNotFound { slot: id })?;
        let wrong = || VaultError::WrongSecret {
            kinds: vec![slot.kind],
        };
        let derived = match (&slot.body, &secret) {
            (SlotBody::Password { kdf, salt, .. }, KeptMaterial::Password(password)) => {
                if password.expose().len() > MAX_PASSWORD_BYTES {
                    return Err(wrong());
                }
                Zeroizing::new(derive_password_material(password, salt, kdf, policy)?.to_vec())
            }
            (
                SlotBody::OsPassword { kdf, salt, .. },
                KeptMaterial::AccountPassword(password, machine),
            ) if slot.made_on(machine) => {
                if password.expose().len() > MAX_PASSWORD_BYTES {
                    return Err(wrong());
                }
                let material = os_password_material(password, salt, kdf, machine, policy)?;
                Zeroizing::new(material.to_vec())
            }
            (SlotBody::RecoveryKey { .. }, KeptMaterial::RecoveryKey(key)) => {
                Zeroizing::new(key.key_bytes().to_vec())
            }
            _ => {
                return Err(VaultError::NoUsableSlot {
                    kinds: vec![slot.kind],
                })
            }
        };
        let Some(unwrapped) = unwrap_slot(&self.header.vault_id, slot, &derived[..])? else {
            return Err(wrong());
        };
        if !bool::from(unwrapped.expose()[..].ct_eq(&self.dk.expose()[..])) {
            return Err(VaultError::SlotCorrupt { slot: id });
        }
        Ok(KeptSlot {
            vault_id: self.header.vault_id,
            id,
            secret,
        })
    }
}

/// The vault `current` rotated (R2; B6, B63, B68): a new data key, the vault id of `ids`, a
/// new store key; the store id as it is; each slot of `keep` — proven against `current` —
/// re-made under its own id with its kind, label and, for an account password,
/// `enrolled_at`, the policy's costs and a new salt and wrap nonce; each slot of `new` under
/// its minted id, after them; every other slot of `current` dropped and listed (an opaque
/// slot — Windows Hello until d4b-w1, Touch ID, an unknown kind, a newer layout — and another
/// computer's account password cannot be re-made here). The payload is `current`'s, the new
/// store key as `age-identity` and the current one, byte for byte, as
/// `age-identity-retiring`. `current` is the vault re-read and opened with its data key under
/// the store lock (B19).
///
/// Refused before a byte is drawn or a key derived: a payload that already retires a store
/// key (`InvalidInput(RotationUnfinished)`: finish that rotation first) or is too large to
/// carry the retiring one; the current vault id (`InvalidInput(SameVaultId)`); not one minted
/// id per new slot (`InvalidInput(SlotIds)`); a kept slot proven against another vault, or
/// one `current` holds as another record (`KeyStale`); one `current` no longer has
/// (`SlotNotFound`); a slot kept twice, or a minted id a slot of `current` or another new slot
/// has (`SlotIdTaken`); not exactly one recovery-key slot (`InvalidInput(OneRecoverySlot)`);
/// more than [`SLOT_MAX`] slots; `create`'s checks of a new slot; two account-password slots
/// of one computer (`InvalidInput(OneSlotPerComputer)`); the writer. Draws, in order: the
/// data key, the store key's 32 bytes, per kept slot in `current`'s header order its salt and
/// wrap nonce, per new slot the same, then the payload nonce.
pub fn rotate(
    current: &UnlockedVault,
    ids: &RotationIds,
    keep: Vec<KeptSlot>,
    new: Vec<NewSlot>,
    writer: &str,
    policy: &KdfPolicy,
    rng: &mut dyn VaultRng,
) -> VaultResult<Rotated> {
    check_rotation(current, ids, &keep, &new, writer)?;
    let dk = Arc::new(DataKey::generate(rng)?);
    let next = draw_identity(rng)?;
    let mut vault = UnlockedVault {
        header: VaultHeader {
            format_version: FORMAT_V1,
            writer: writer.to_string(),
            vault_id: ids.vault_id,
            store_id: current.header.store_id.clone(),
            slots: Vec::with_capacity(keep.len() + new.len()),
        },
        dk,
        payload: current.payload.rotated_to(&next)?,
    };
    let mut proven: HashMap<SlotId, KeptSlot> =
        keep.into_iter().map(|kept| (kept.id, kept)).collect();
    let mut dropped = Vec::new();
    for slot in &current.header.slots {
        let Some(kept) = proven.remove(&slot.id) else {
            dropped.push(slot.id);
            continue;
        };
        let fresh = match kept.secret {
            KeptMaterial::Password(secret) => Fresh::Password(secret),
            KeptMaterial::AccountPassword(secret, machine) => {
                let SlotBody::OsPassword { enrolled_at, .. } = &slot.body else {
                    return Err(VaultError::KeyStale);
                };
                Fresh::AccountPassword {
                    secret,
                    enrolled_at: *enrolled_at,
                    machine,
                }
            }
            KeptMaterial::RecoveryKey(key) => Fresh::RecoveryKey(key),
        };
        let record = vault.fresh_record(slot.id, fresh, slot.label.clone(), policy, rng)?;
        vault.header.slots.push(record);
    }
    for (id, slot) in ids.new_slots.iter().zip(new) {
        let record = vault.new_record_as(*id, slot, policy, rng)?;
        vault.header.slots.push(record);
    }
    let bytes = vault.seal(writer, rng)?;
    Ok(Rotated {
        bytes,
        vault,
        dropped,
    })
}

/// [`rotate`]'s checks, in its order; nothing is drawn or derived here.
fn check_rotation(
    current: &UnlockedVault,
    ids: &RotationIds,
    keep: &[KeptSlot],
    new: &[NewSlot],
    writer: &str,
) -> VaultResult<()> {
    // The rotated payload built around the current store key: it refuses a payload that
    // already retires one (§5.4), and it is exactly as large as the real one — an age
    // identity's text is always 74 bytes — so its size is checked before anything is drawn.
    current
        .payload
        .rotated_to(&current.payload.age_identity()?)?
        .check_size()?;
    if ids.vault_id == current.header.vault_id {
        return Err(VaultError::InvalidInput(InputWhat::SameVaultId));
    }
    if ids.new_slots.len() != new.len() {
        return Err(VaultError::InvalidInput(InputWhat::SlotIds));
    }
    let mut kept_ids = HashSet::with_capacity(keep.len());
    for kept in keep {
        if kept.vault_id != current.header.vault_id {
            return Err(VaultError::KeyStale);
        }
        let slot = current
            .header
            .slots
            .iter()
            .find(|slot| slot.id == kept.id)
            .ok_or(VaultError::SlotNotFound { slot: kept.id })?;
        if !kept.fits(slot) {
            return Err(VaultError::KeyStale);
        }
        if !kept_ids.insert(kept.id) {
            return Err(VaultError::SlotIdTaken { slot: kept.id });
        }
    }
    let mut taken: HashSet<SlotId> = current.header.slots.iter().map(|slot| slot.id).collect();
    if let Some(id) = ids.new_slots.iter().find(|id| !taken.insert(**id)) {
        return Err(VaultError::SlotIdTaken { slot: *id });
    }
    let recovery_slots = keep
        .iter()
        .filter(|kept| matches!(kept.secret, KeptMaterial::RecoveryKey(_)))
        .count()
        + new
            .iter()
            .filter(|slot| matches!(slot, NewSlot::RecoveryKey { .. }))
            .count();
    if recovery_slots != 1 {
        return Err(VaultError::InvalidInput(InputWhat::OneRecoverySlot));
    }
    if keep.len() + new.len() > SLOT_MAX {
        return Err(VaultError::TooLarge {
            what: SizeWhat::Slots,
            limit: SLOT_MAX as u64,
        });
    }
    for slot in new {
        check_new_slot(slot)?;
    }
    // B77: one account-password slot per computer among the kept and the new.
    let machines: Vec<&[u8]> = keep
        .iter()
        .filter_map(|kept| match &kept.secret {
            KeptMaterial::AccountPassword(_, machine) => Some(machine.bytes()),
            _ => None,
        })
        .chain(new.iter().filter_map(|slot| match slot {
            NewSlot::OsPassword { machine, .. } => Some(machine.bytes()),
            _ => None,
        }))
        .collect();
    if (1..machines.len()).any(|i| machines[..i].contains(&machines[i])) {
        return Err(VaultError::InvalidInput(InputWhat::OneSlotPerComputer));
    }
    check_writer(writer.as_bytes()).map_err(|_| VaultError::InvalidInput(InputWhat::Writer))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use age::secrecy::ExposeSecret;

    use crate::secrets::vault_id_on_disk;
    use crate::vault::format::{VaultFile, PAYLOAD_MAX};
    use crate::vault::kdf::DERIVATIONS;
    use crate::vault::keys::SlotSecret;
    use crate::vault::payload::VaultPayload;
    use crate::vault::rng::{OsRandom, ScriptedRandom};
    use crate::vault::seal::{create, create_with, VaultIds};
    use crate::vault::testutil::{hex, seq, seq_array, tlv};
    use crate::vault::{file, VAULT_FILE};
    use cli_core::{StoreLock, TargetStorePaths};

    const PASSWORD: &str = "correct horse battery staple";
    const NEW_PASSWORD: &str = "a new lock password for the rotated vault";
    const ACCOUNT: &str = "hunter2 is not a password";
    const IDENTITY: &str =
        "AGE-SECRET-KEY-1YQSJYGEYY5NZW2PF9G4JCTFW9UCRZV3NXS6NVDEC8YARK0PA8CLSWXGG7Q";
    const STORE: &str = "0123456789abcdef";
    const WRITER: &str = "apprafter 0.0.0-test";
    const FAST: &KdfPolicy = &KdfPolicy::INSECURE_FAST;
    /// This computer's machine id (16 bytes of it), and another computer's.
    const HERE: u8 = 0x0a;
    const THERE: u8 = 0x0b;

    fn identity() -> age::x25519::Identity {
        IDENTITY.parse().unwrap()
    }

    /// The store key, a timeout, a token and an unknown entry.
    fn payload() -> VaultPayload {
        VaultPayload::decode(&tlv(&[
            ("age-identity", IDENTITY.as_bytes()),
            ("settings/unlock-timeout-min", &b"15"[..]),
            ("target/prod/hetzner-token", "A1b2C3d4".repeat(8).as_bytes()),
            ("x-future/key", &[0xffu8, 0xfe, 0x00, 0x01][..]),
        ]))
        .unwrap()
    }

    fn recovery(byte: u8) -> RecoveryKey {
        RecoveryKey::generate(&mut ScriptedRandom::new(vec![vec![byte; 32]])).unwrap()
    }

    fn machine(which: u8) -> MachineId {
        MachineId::new(&[which; 16]).unwrap()
    }

    fn account_slot(on: u8) -> NewSlot {
        NewSlot::OsPassword {
            secret: SecretString::new(ACCOUNT),
            label: format!("account password of walker on box-{on:x}"),
            enrolled_at: 1_760_000_000 + u64::from(on),
            machine: machine(on),
        }
    }

    fn password(text: &str) -> SlotSecret {
        SlotSecret::Password(SecretString::new(text))
    }

    fn account(on: u8) -> SlotSecret {
        SlotSecret::AccountPassword(SecretString::new(ACCOUNT), machine(on))
    }

    fn recovery_secret(byte: u8) -> SlotSecret {
        SlotSecret::RecoveryKey(recovery(byte))
    }

    fn kept_password(text: &str) -> KeptMaterial {
        KeptMaterial::Password(SecretString::new(text))
    }

    fn kept_account(on: u8) -> KeptMaterial {
        KeptMaterial::AccountPassword(SecretString::new(ACCOUNT), machine(on))
    }

    fn kept_recovery(byte: u8) -> KeptMaterial {
        KeptMaterial::RecoveryKey(recovery(byte))
    }

    fn derivations() -> u32 {
        DERIVATIONS.with(std::cell::Cell::get)
    }

    /// A slot this build keeps opaque: `len` bytes of `byte`, id `byte` × 16.
    fn opaque(kind: SlotKind, byte: u8, len: usize) -> SlotRecord {
        SlotRecord {
            id: SlotId([byte; 16]),
            kind,
            body: SlotBody::Opaque(vec![byte; len]),
            label: format!("slot {byte:#04x}"),
        }
    }

    /// A store of every slot shape a rotation meets (B68), opened by its recovery key:
    /// 0 a lock password, 1 this computer's account password, 2 another computer's, 3 the
    /// recovery key `0x11`, then slots kept opaque — 4 Windows Hello (until d4b-w1), 5 Touch
    /// ID, 6 an unknown kind, 7 an account-password slot of a newer layout.
    fn store() -> (VaultFile, UnlockedVault) {
        let (_, mut made) = create(
            STORE,
            WRITER,
            payload(),
            vec![
                NewSlot::Password {
                    secret: SecretString::new(PASSWORD),
                    label: "lock password".into(),
                },
                account_slot(HERE),
                account_slot(THERE),
                NewSlot::RecoveryKey {
                    key: recovery(0x11),
                    label: "recovery key".into(),
                },
            ],
            FAST,
            &mut OsRandom,
        )
        .unwrap();
        made.header.slots.extend([
            opaque(SlotKind::WindowsHello, 0xe4, 40),
            opaque(SlotKind::TouchId, 0xe5, 40),
            opaque(SlotKind::Unknown(0x7f), 0xe6, 40),
            opaque(SlotKind::OsPassword, 0xe7, 150),
        ]);
        let file = VaultFile::parse(&made.seal(WRITER, &mut OsRandom).unwrap()).unwrap();
        let opened = file.open(&recovery_secret(0x11), FAST).unwrap();
        (file, opened)
    }

    fn slot_ids(vault: &UnlockedVault) -> Vec<SlotId> {
        vault.header().slots.iter().map(|slot| slot.id).collect()
    }

    fn salt_and_nonce(body: &SlotBody) -> ([u8; 16], [u8; 24]) {
        match body {
            SlotBody::Password {
                salt, wrap_nonce, ..
            }
            | SlotBody::OsPassword {
                salt, wrap_nonce, ..
            }
            | SlotBody::RecoveryKey {
                salt, wrap_nonce, ..
            } => (*salt, *wrap_nonce),
            SlotBody::Opaque(_) => panic!("an opaque slot has no salt"),
        }
    }

    /// The ids a plan would mint: vault id `c0…cf`, new slot `i` `d0+i…`.
    fn given_ids(new_slots: usize) -> RotationIds {
        RotationIds {
            vault_id: VaultId(seq_array(0xc0)),
            new_slots: (0..new_slots)
                .map(|i| SlotId(seq_array(0xd0 + i as u8)))
                .collect(),
        }
    }

    /// `current` rotated, keeping its lock password, this computer's account password and
    /// the recovery key `0x11`, adding nothing.
    fn rotated(current: &UnlockedVault) -> Rotated {
        let ids = slot_ids(current);
        let keep = vec![
            current.keep(ids[0], kept_password(PASSWORD), FAST).unwrap(),
            current.keep(ids[1], kept_account(HERE), FAST).unwrap(),
            current.keep(ids[3], kept_recovery(0x11), FAST).unwrap(),
        ];
        rotate(
            current,
            &given_ids(0),
            keep,
            Vec::new(),
            WRITER,
            FAST,
            &mut OsRandom,
        )
        .unwrap()
    }

    /// B6, B63, B68, acceptance 1: a rotation keeps the slots proven in the run under their
    /// own ids — their kind, label and `enrolled_at`, a new salt and wrap nonce — adds the new
    /// ones under their minted ids, drops and lists every other slot, and writes a new data
    /// key, vault id and store key; the old store key rides in `age-identity-retiring`; the
    /// store id and every other payload entry stay; the current vault is untouched.
    #[test]
    fn a_rotation_keeps_proven_slots_under_their_own_ids_and_replaces_every_key() {
        let (_, current) = store();
        let old = current.header().clone();
        let ids = slot_ids(&current);
        let keep = vec![
            current.keep(ids[0], kept_password(PASSWORD), FAST).unwrap(),
            current.keep(ids[1], kept_account(HERE), FAST).unwrap(),
        ];
        let new = vec![NewSlot::RecoveryKey {
            key: recovery(0x22),
            label: "recovery key".into(),
        }];
        let rotation = given_ids(1);
        let done = rotate(&current, &rotation, keep, new, WRITER, FAST, &mut OsRandom).unwrap();
        let file = VaultFile::parse(&done.bytes).unwrap();
        let header = file.header();
        assert_eq!(done.vault.header(), header);
        // The ids: a new vault id, the store id as it was, the kept slots' own ids first (in
        // the current header's order), then the new one's minted id.
        assert_eq!(header.vault_id, rotation.vault_id);
        assert_ne!(header.vault_id, old.vault_id);
        assert_eq!(header.store_id.as_bytes(), old.store_id.as_bytes());
        assert_eq!(
            header.slots.iter().map(|s| s.id).collect::<Vec<_>>(),
            [ids[0], ids[1], rotation.new_slots[0]]
        );
        assert_eq!(done.dropped, ids[2..].to_vec());
        // Each kept slot: its kind and label, a new salt and wrap nonce; the account
        // password its `enrolled_at` and this computer's tag.
        for (i, kept) in header.slots[..2].iter().enumerate() {
            let was = &old.slots[i];
            assert_eq!((kept.kind, &kept.label), (was.kind, &was.label), "slot {i}");
            let (salt, nonce) = salt_and_nonce(&kept.body);
            let (old_salt, old_nonce) = salt_and_nonce(&was.body);
            assert_ne!(salt, old_salt, "slot {i}: a new salt");
            assert_ne!(nonce, old_nonce, "slot {i}: a new wrap nonce");
        }
        let (
            SlotBody::OsPassword {
                enrolled_at,
                machine_tag,
                ..
            },
            SlotBody::OsPassword {
                enrolled_at: was_at,
                machine_tag: was_tag,
                ..
            },
        ) = (&header.slots[1].body, &old.slots[1].body)
        else {
            panic!("slot 1 is this computer's account password");
        };
        assert_eq!((enrolled_at, machine_tag), (was_at, was_tag));
        assert!(header.slots[1].made_on(&machine(HERE)));
        // Every kept and new slot opens the rotated file, to its new data key.
        let new_dk = done.vault.data_key().expose().to_vec();
        assert_ne!(&new_dk[..], &current.data_key().expose()[..]);
        for secret in [password(PASSWORD), account(HERE), recovery_secret(0x22)] {
            let opened = file.open(&secret, FAST).unwrap();
            assert_eq!(&opened.data_key().expose()[..], &new_dk[..], "{secret:?}");
        }
        // The old data key and every dropped slot's secret open nothing in it.
        assert!(matches!(
            file.open_with_key(current.data_key(), old.vault_id),
            Err(VaultError::KeyStale)
        ));
        assert!(matches!(
            file.open_with_key(current.data_key(), header.vault_id),
            Err(VaultError::Tampered)
        ));
        assert!(matches!(
            file.open(&recovery_secret(0x11), FAST),
            Err(VaultError::WrongSecret { .. })
        ));
        assert!(matches!(
            file.open(&account(THERE), FAST),
            Err(VaultError::NoUsableSlot { .. })
        ));
        // The store key: a new one, the old one retiring beside it, every other entry as it
        // was.
        let payload = done.vault.payload();
        let new_key = payload.age_identity().unwrap();
        assert_ne!(
            new_key.to_public().to_string(),
            identity().to_public().to_string()
        );
        assert_eq!(
            payload
                .retiring_identity()
                .unwrap()
                .unwrap()
                .to_public()
                .to_string(),
            identity().to_public().to_string()
        );
        assert_eq!(
            payload.raw("age-identity-retiring"),
            current.payload().raw("age-identity")
        );
        for key in current
            .payload()
            .keys()
            .filter(|key| *key != "age-identity")
        {
            assert_eq!(payload.raw(key), current.payload().raw(key), "{key}");
        }
        assert_eq!(payload.keys().count(), current.payload().keys().count() + 1);
        let reopened = file.open(&password(PASSWORD), FAST).unwrap();
        assert_eq!(
            reopened.payload().raw("age-identity"),
            payload.raw("age-identity")
        );
        // The current vault is untouched.
        assert_eq!(current.header(), &old);
        assert!(current.payload().retiring_identity().unwrap().is_none());
    }

    /// The draws, in order: the data key, the store key's 32 bytes, per kept slot in the
    /// current header's order (not the order given) its salt and wrap nonce, per new slot the
    /// same, then the payload nonce — nothing else (the ids are minted at plan time).
    #[test]
    fn a_rotation_draws_in_the_documented_order() {
        let (_, current) = store();
        let ids = slot_ids(&current);
        let keep = vec![
            current.keep(ids[3], kept_recovery(0x11), FAST).unwrap(),
            current.keep(ids[1], kept_account(HERE), FAST).unwrap(),
        ];
        let new = vec![NewSlot::Password {
            secret: SecretString::new(NEW_PASSWORD),
            label: "lock password".into(),
        }];
        let mut rng = ScriptedRandom::new(vec![
            seq(0x10, 32),
            seq(0xa0, 32),
            seq(0x41, 16),
            seq(0x51, 24),
            seq(0x69, 16),
            seq(0x79, 24),
            seq(0x91, 16),
            seq(0xa1, 24),
            seq(0xb9, 24),
        ]);
        let rotation = given_ids(1);
        let before = derivations();
        let done = rotate(&current, &rotation, keep, new, WRITER, FAST, &mut rng).unwrap();
        assert_eq!(rng.drawn(), 9);
        // Its own cost (O-b8): one Argon2id per kept or new password slot, none for the
        // recovery key (critic round, 8).
        assert_eq!(derivations() - before, 2);
        assert_eq!(done.vault.data_key().expose(), &seq_array::<32>(0x10));
        let drawn = draw_identity(&mut ScriptedRandom::new(vec![seq(0xa0, 32)])).unwrap();
        assert_eq!(
            done.vault.payload().raw("age-identity"),
            Some(drawn.to_string().expose_secret().as_bytes())
        );
        let slots = &done.vault.header().slots;
        assert_eq!(
            slots.iter().map(|s| s.id).collect::<Vec<_>>(),
            [ids[1], ids[3], rotation.new_slots[0]]
        );
        assert_eq!(
            salt_and_nonce(&slots[0].body),
            (seq_array(0x41), seq_array(0x51))
        );
        assert_eq!(
            salt_and_nonce(&slots[1].body),
            (seq_array(0x69), seq_array(0x79))
        );
        assert_eq!(
            salt_and_nonce(&slots[2].body),
            (seq_array(0x91), seq_array(0xa1))
        );
        assert_eq!(
            VaultFile::parse(&done.bytes).unwrap().payload_nonce,
            seq_array::<24>(0xb9)
        );
    }

    #[test]
    fn a_failing_generator_fails_a_rotation_at_every_draw() {
        let (_, current) = store();
        let ids = slot_ids(&current);
        let draws = [
            seq(0x10, 32),
            seq(0xa0, 32),
            seq(0x41, 16),
            seq(0x51, 24),
            seq(0x69, 16),
            seq(0x79, 24),
            seq(0x91, 16),
            seq(0xa1, 24),
            seq(0xb9, 24),
        ];
        for draw in 0..draws.len() {
            let keep = vec![
                current.keep(ids[1], kept_account(HERE), FAST).unwrap(),
                current.keep(ids[3], kept_recovery(0x11), FAST).unwrap(),
            ];
            let new = vec![NewSlot::Password {
                secret: SecretString::new(NEW_PASSWORD),
                label: "lock password".into(),
            }];
            let mut rng = ScriptedRandom::failing_at(draw, draws[..draw].to_vec());
            let result = rotate(&current, &given_ids(1), keep, new, WRITER, FAST, &mut rng);
            assert!(
                matches!(result, Err(VaultError::RandomUnavailable { .. })),
                "draw {draw}"
            );
        }
    }

    /// Every refusal comes before a byte is drawn or a key derived (an empty script panics on
    /// a draw; the derivation count stays).
    #[test]
    fn a_rotation_checks_everything_before_drawing_or_deriving() {
        let (file, current) = store();
        let ids = slot_ids(&current);
        let lock_password = || NewSlot::Password {
            secret: SecretString::new(NEW_PASSWORD),
            label: "lock password".into(),
        };
        let new_recovery = |byte| NewSlot::RecoveryKey {
            key: recovery(byte),
            label: "recovery key".into(),
        };
        let proof = |i: usize, secret: KeptMaterial| current.keep(ids[i], secret, FAST).unwrap();
        let other_id = |new_slots: Vec<SlotId>| RotationIds {
            vault_id: VaultId([0x5b; 16]),
            new_slots,
        };
        let refused = |vault: &UnlockedVault,
                       rotation: &RotationIds,
                       keep: Vec<KeptSlot>,
                       new: Vec<NewSlot>,
                       writer: &str| {
            let before = derivations();
            let result = rotate(
                vault,
                rotation,
                keep,
                new,
                writer,
                FAST,
                &mut ScriptedRandom::new(Vec::new()),
            );
            assert_eq!(derivations(), before, "nothing derived");
            result.map(|_| ()).unwrap_err()
        };
        // A rotation under way: that one is finished first (§5.4).
        let under_way = rotated(&current).vault;
        let rk = under_way
            .keep(under_way.header().slots[2].id, kept_recovery(0x11), FAST)
            .unwrap();
        assert!(matches!(
            refused(&under_way, &other_id(vec![]), vec![rk], vec![], WRITER),
            VaultError::InvalidInput(InputWhat::RotationUnfinished)
        ));
        // A new vault id; one minted id per new slot.
        let same = RotationIds {
            vault_id: current.header().vault_id,
            new_slots: vec![],
        };
        assert!(matches!(
            refused(
                &current,
                &same,
                vec![proof(3, kept_recovery(0x11))],
                vec![],
                WRITER
            ),
            VaultError::InvalidInput(InputWhat::SameVaultId)
        ));
        assert!(matches!(
            refused(
                &current,
                &given_ids(0),
                vec![proof(3, kept_recovery(0x11))],
                vec![lock_password()],
                WRITER
            ),
            VaultError::InvalidInput(InputWhat::SlotIds)
        ));
        // A proof of another vault, of a slot gone since, of another record under its id,
        // or given twice.
        let (_, other) = store();
        let foreign = other
            .keep(slot_ids(&other)[3], kept_recovery(0x11), FAST)
            .unwrap();
        assert!(matches!(
            refused(&current, &given_ids(0), vec![foreign], vec![], WRITER),
            VaultError::KeyStale
        ));
        let mut gone = file.open(&recovery_secret(0x11), FAST).unwrap();
        gone.remove_slot(ids[0], FAST).unwrap();
        assert!(matches!(
            refused(
                &gone,
                &given_ids(0),
                vec![proof(0, kept_password(PASSWORD)), proof(3, kept_recovery(0x11))],
                vec![],
                WRITER
            ),
            VaultError::SlotNotFound { slot } if slot == ids[0]
        ));
        let mut forged = file.open(&recovery_secret(0x11), FAST).unwrap();
        let mut swapped = forged.header.slots[3].clone();
        swapped.id = ids[0];
        forged.header.slots[0] = swapped;
        assert!(matches!(
            refused(
                &forged,
                &given_ids(0),
                vec![
                    proof(0, kept_password(PASSWORD)),
                    proof(3, kept_recovery(0x11))
                ],
                vec![],
                WRITER
            ),
            VaultError::KeyStale
        ));
        // Another record under its id: the other computer's account password under this
        // computer's slot id (critic round, 6).
        let mut moved = file.open(&recovery_secret(0x11), FAST).unwrap();
        let mut there = moved.header.slots[2].clone();
        there.id = ids[1];
        moved.header.slots[1] = there;
        assert!(matches!(
            refused(
                &moved,
                &given_ids(0),
                vec![proof(1, kept_account(HERE)), proof(3, kept_recovery(0x11))],
                vec![],
                WRITER
            ),
            VaultError::KeyStale
        ));
        assert!(matches!(
            refused(
                &current,
                &given_ids(0),
                vec![proof(3, kept_recovery(0x11)), proof(3, kept_recovery(0x11))],
                vec![],
                WRITER
            ),
            VaultError::SlotIdTaken { slot } if slot == ids[3]
        ));
        // A minted id any slot of the vault has — kept or dropped — or another new slot has.
        assert!(matches!(
            refused(
                &current,
                &other_id(vec![ids[2]]),
                vec![proof(3, kept_recovery(0x11))],
                vec![lock_password()],
                WRITER
            ),
            VaultError::SlotIdTaken { slot } if slot == ids[2]
        ));
        let twice = SlotId([0x5c; 16]);
        assert!(matches!(
            refused(
                &current,
                &other_id(vec![twice, twice]),
                vec![proof(3, kept_recovery(0x11))],
                vec![lock_password(), lock_password()],
                WRITER
            ),
            VaultError::SlotIdTaken { slot } if slot == twice
        ));
        // Exactly one recovery-key slot (ADR 0068 §3).
        assert!(matches!(
            refused(
                &current,
                &given_ids(0),
                vec![proof(0, kept_password(PASSWORD))],
                vec![],
                WRITER
            ),
            VaultError::InvalidInput(InputWhat::OneRecoverySlot)
        ));
        assert!(matches!(
            refused(
                &current,
                &given_ids(1),
                vec![proof(3, kept_recovery(0x11))],
                vec![new_recovery(0x22)],
                WRITER
            ),
            VaultError::InvalidInput(InputWhat::OneRecoverySlot)
        ));
        // One account-password slot per computer (B77): this computer's kept and a new one.
        assert!(matches!(
            refused(
                &current,
                &given_ids(1),
                vec![proof(1, kept_account(HERE)), proof(3, kept_recovery(0x11))],
                vec![account_slot(HERE)],
                WRITER
            ),
            VaultError::InvalidInput(InputWhat::OneSlotPerComputer)
        ));
        // At most sixteen slots.
        let many: Vec<NewSlot> = (0..16).map(|_| lock_password()).collect();
        assert!(matches!(
            refused(
                &current,
                &given_ids(16),
                vec![proof(3, kept_recovery(0x11))],
                many,
                WRITER
            ),
            VaultError::TooLarge {
                what: SizeWhat::Slots,
                ..
            }
        ));
        // A new slot's own checks, and the writer's.
        let empty = NewSlot::Password {
            secret: SecretString::new(""),
            label: "lock password".into(),
        };
        assert!(matches!(
            refused(
                &current,
                &given_ids(1),
                vec![proof(3, kept_recovery(0x11))],
                vec![empty],
                WRITER
            ),
            VaultError::PasswordRefused(_)
        ));
        let bell = NewSlot::Password {
            secret: SecretString::new(NEW_PASSWORD),
            label: "bell\u{7}".into(),
        };
        assert!(matches!(
            refused(
                &current,
                &given_ids(1),
                vec![proof(3, kept_recovery(0x11))],
                vec![bell],
                WRITER
            ),
            VaultError::InvalidInput(InputWhat::Label)
        ));
        assert!(matches!(
            refused(
                &current,
                &given_ids(0),
                vec![proof(3, kept_recovery(0x11))],
                vec![],
                "bell\u{7}"
            ),
            VaultError::InvalidInput(InputWhat::Writer)
        ));
    }

    /// F10, B63: the rotated payload carries one entry more — the retiring store key, 98 bytes
    /// (1 + 21 + 2 + 74), the new key's text as long as the current one's. A payload too large
    /// for it is refused before anything is drawn; one that just fits rotates to 1 MiB.
    #[test]
    fn a_payload_too_large_to_rotate_is_refused_before_anything_is_drawn() {
        let vault_of = |len: usize| {
            let slots = vec![NewSlot::RecoveryKey {
                key: recovery(0x11),
                label: "recovery key".into(),
            }];
            let payload = super::super::tests::payload_of_len(len);
            create(STORE, WRITER, payload, slots, FAST, &mut OsRandom)
                .unwrap()
                .1
        };
        let too_big = vault_of(PAYLOAD_MAX - 97);
        let kept = too_big
            .keep(slot_ids(&too_big)[0], kept_recovery(0x11), FAST)
            .unwrap();
        let mut nothing = ScriptedRandom::new(Vec::new());
        assert!(matches!(
            rotate(
                &too_big,
                &given_ids(0),
                vec![kept],
                Vec::new(),
                WRITER,
                FAST,
                &mut nothing
            ),
            Err(VaultError::TooLarge {
                what: SizeWhat::Payload,
                ..
            })
        ));
        let fits = vault_of(PAYLOAD_MAX - 98);
        let kept = fits
            .keep(slot_ids(&fits)[0], kept_recovery(0x11), FAST)
            .unwrap();
        let done = rotate(
            &fits,
            &given_ids(0),
            vec![kept],
            Vec::new(),
            WRITER,
            FAST,
            &mut OsRandom,
        )
        .unwrap();
        assert_eq!(done.vault.payload().check_size().unwrap(), PAYLOAD_MAX);
    }

    /// B68, B13: a kept slot is proven by its own key check — never PAM — and only for a slot
    /// its secret can open here.
    #[test]
    fn a_kept_slot_is_proven_by_its_own_key_check() {
        let (_, current) = store();
        let ids = slot_ids(&current);
        let proven =
            |i: usize, secret: KeptMaterial| current.keep(ids[i], secret, FAST).unwrap().id();
        assert_eq!(proven(0, kept_password(PASSWORD)), ids[0]);
        assert_eq!(proven(1, kept_account(HERE)), ids[1]);
        assert_eq!(proven(3, kept_recovery(0x11)), ids[3]);
        // A wrong secret is a wrong secret of that slot's kind, never "kept".
        let wrong_account = KeptMaterial::AccountPassword(
            SecretString::new("hunter3 is not a password"),
            machine(HERE),
        );
        for (id, secret, kind) in [
            (
                ids[0],
                kept_password("correct horse battery stapler"),
                SlotKind::Password,
            ),
            (ids[1], wrong_account, SlotKind::OsPassword),
            (ids[3], kept_recovery(0x12), SlotKind::RecoveryKey),
        ] {
            match current.keep(id, secret, FAST) {
                Err(VaultError::WrongSecret { kinds }) => assert_eq!(kinds, [kind]),
                other => panic!("{:?}", other.map(|_| "kept")),
            }
        }
        // An over-long password of either kind is a wrong secret, refused unhashed, as `open`
        // refuses it (critic round, 2).
        let too_long = "a".repeat(MAX_PASSWORD_BYTES + 1);
        let long_account =
            KeptMaterial::AccountPassword(SecretString::new(too_long.clone()), machine(HERE));
        let unhashed = derivations();
        for (id, secret, kind) in [
            (ids[0], kept_password(&too_long), SlotKind::Password),
            (ids[1], long_account, SlotKind::OsPassword),
        ] {
            match current.keep(id, secret, FAST) {
                Err(VaultError::WrongSecret { kinds }) => assert_eq!(kinds, [kind]),
                other => panic!("{:?}", other.map(|_| "kept")),
            }
        }
        assert_eq!(
            derivations(),
            unhashed,
            "an over-long password is never hashed"
        );
        // A slot its secret cannot open here derives nothing: another computer's account
        // password (B68 drops it), another kind, a slot kept opaque; no such slot; the
        // policy's refusal of the slot's costs (T22).
        let before = derivations();
        for (id, secret) in [
            (ids[2], kept_account(HERE)),
            (ids[0], kept_account(HERE)),
            (ids[1], kept_password(ACCOUNT)),
            (ids[3], kept_password(PASSWORD)),
            (ids[4], kept_recovery(0x11)),
            (ids[7], kept_account(HERE)),
        ] {
            assert!(
                matches!(
                    current.keep(id, secret, FAST),
                    Err(VaultError::NoUsableSlot { .. })
                ),
                "{id}"
            );
        }
        assert!(matches!(
            current.keep(SlotId([0x99; 16]), kept_recovery(0x11), FAST),
            Err(VaultError::SlotNotFound { .. })
        ));
        assert!(matches!(
            current.keep(ids[0], kept_password(PASSWORD), &KdfPolicy::RELEASE),
            Err(VaultError::KdfTooWeak { .. })
        ));
        assert_eq!(derivations(), before);
    }

    /// A slot whose key check passes and whose wrap does not open to this vault's data key is
    /// damaged: a damaged wrap, or another vault's data key wrapped under this vault's ids.
    #[test]
    fn a_kept_slot_must_wrap_this_vaults_data_key() {
        let (file, _) = store();
        let mut damaged = file.open(&recovery_secret(0x11), FAST).unwrap();
        let target = damaged.header.slots[0].id;
        if let SlotBody::Password { wrapped_dk, .. } = &mut damaged.header.slots[0].body {
            wrapped_dk[0] ^= 1;
        }
        assert!(matches!(
            damaged.keep(target, kept_password(PASSWORD), FAST),
            Err(VaultError::SlotCorrupt { slot }) if slot == target
        ));
        let mut twinned = file.open(&recovery_secret(0x11), FAST).unwrap();
        let ids = VaultIds {
            vault_id: twinned.header.vault_id,
            slots: vec![target, SlotId([0x42; 16])],
        };
        let slots = vec![
            NewSlot::Password {
                secret: SecretString::new(PASSWORD),
                label: "lock password".into(),
            },
            NewSlot::RecoveryKey {
                key: recovery(0x11),
                label: "recovery key".into(),
            },
        ];
        let (_, twin) =
            create_with(&ids, STORE, WRITER, payload(), slots, FAST, &mut OsRandom).unwrap();
        twinned.header.slots[0] = twin.header.slots[0].clone();
        assert!(matches!(
            twinned.keep(target, kept_password(PASSWORD), FAST),
            Err(VaultError::SlotCorrupt { slot }) if slot == target
        ));
    }

    #[test]
    fn rotation_ids_are_drawn_vault_id_first() {
        let mut rng = ScriptedRandom::new(vec![seq(0xc0, 16), seq(0xd0, 16)]);
        assert_eq!(RotationIds::draw(1, &mut rng).unwrap(), given_ids(1));
        assert!(matches!(
            RotationIds::draw(1, &mut ScriptedRandom::failing_at(1, vec![seq(0xc0, 16)])),
            Err(VaultError::RandomUnavailable { .. })
        ));
    }

    /// R4 (B63), acceptance 2: the retired vault is the rotated one — the same data key, vault
    /// id, slots and labels, no slot re-made, one draw — without `age-identity-retiring`,
    /// every other entry as it was; a vault that retires nothing is refused before drawing.
    #[test]
    fn retiring_removes_only_the_retiring_store_key() {
        let (_, current) = store();
        let done = rotated(&current);
        let before = derivations();
        let mut rng = ScriptedRandom::new(vec![seq(0xd1, 24)]);
        let bytes = done.vault.retire_identity(WRITER, &mut rng).unwrap();
        assert_eq!((rng.drawn(), derivations()), (1, before));
        let file = VaultFile::parse(&bytes).unwrap();
        assert_eq!(file.header(), done.vault.header());
        assert_eq!(file.payload_nonce, seq_array::<24>(0xd1));
        let retired = file
            .open_with_key(done.vault.data_key(), done.vault.header().vault_id)
            .unwrap();
        let rest = |p: &VaultPayload| -> Vec<(String, Vec<u8>)> {
            p.keys()
                .filter(|key| *key != "age-identity-retiring")
                .map(|key| (key.to_string(), p.raw(key).unwrap().to_vec()))
                .collect()
        };
        assert!(retired.payload().retiring_identity().unwrap().is_none());
        assert!(retired.payload().raw("age-identity-retiring").is_none());
        assert_eq!(rest(retired.payload()), rest(done.vault.payload()));
        // Every slot the rotation kept still opens it.
        for secret in [password(PASSWORD), account(HERE), recovery_secret(0x11)] {
            assert!(file.open(&secret, FAST).is_ok(), "{secret:?}");
        }
        // The rotated vault in memory still retires its key; retiring nothing is refused.
        assert!(done.vault.payload().retiring_identity().unwrap().is_some());
        for vault in [&retired, &current] {
            assert!(matches!(
                vault.retire_identity(WRITER, &mut ScriptedRandom::new(Vec::new())),
                Err(VaultError::InvalidInput(InputWhat::NothingToRetire))
            ));
        }
    }

    /// B78, §4.5, §5.4: both of a rotation's vaults go through `vault::file::replace`, durable
    /// before it returns, and read back as written. From R2 on the vault id on disk is the new
    /// one and R4 keeps it: only the opened payload's retiring key tells R2's vault from R4's.
    #[test]
    fn a_rotation_is_written_and_retired_through_the_durable_replace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let lock = StoreLock::exclusive(&TargetStorePaths::for_root(root.to_path_buf())).unwrap();
        let path = root.join(VAULT_FILE);
        let slots = vec![
            NewSlot::Password {
                secret: SecretString::new(PASSWORD),
                label: "lock password".into(),
            },
            NewSlot::RecoveryKey {
                key: recovery(0x11),
                label: "recovery key".into(),
            },
        ];
        let (made, current) = create(STORE, WRITER, payload(), slots, FAST, &mut OsRandom).unwrap();
        file::write_new(&path, &made, &lock).unwrap();
        assert_eq!(
            vault_id_on_disk(root).unwrap(),
            Some(current.header().vault_id)
        );
        // R2: the rotated vault replaces the current one.
        let ids = slot_ids(&current);
        let keep = vec![
            current.keep(ids[0], kept_password(PASSWORD), FAST).unwrap(),
            current.keep(ids[1], kept_recovery(0x11), FAST).unwrap(),
        ];
        let rotation = RotationIds::draw(0, &mut OsRandom).unwrap();
        let done = rotate(
            &current,
            &rotation,
            keep,
            Vec::new(),
            WRITER,
            FAST,
            &mut OsRandom,
        )
        .unwrap();
        file::replace(&path, &done.bytes, &lock).unwrap();
        assert_eq!(vault_id_on_disk(root).unwrap(), Some(rotation.vault_id));
        let r2 = VaultFile::parse(&file::read(&path).unwrap())
            .unwrap()
            .open(&password(PASSWORD), FAST)
            .unwrap();
        assert_eq!(r2.header(), done.vault.header());
        assert!(r2.payload().retiring_identity().unwrap().is_some());
        // R4: the retired vault replaces it, under the same vault id.
        let retired = r2.retire_identity(WRITER, &mut OsRandom).unwrap();
        file::replace(&path, &retired, &lock).unwrap();
        assert_eq!(vault_id_on_disk(root).unwrap(), Some(rotation.vault_id));
        let r4 = VaultFile::parse(&file::read(&path).unwrap())
            .unwrap()
            .open_with_key(r2.data_key(), rotation.vault_id)
            .unwrap();
        assert_eq!(r4.header(), r2.header());
        assert!(r4.payload().retiring_identity().unwrap().is_none());
        assert_eq!(
            r4.payload().raw("age-identity"),
            r2.payload().raw("age-identity")
        );
    }

    #[test]
    fn nothing_a_rotation_prints_shows_a_secret() {
        let fold = |text: &str| text.to_lowercase().replace(['-', '_', ' ', '\n'], "");
        let (_, current) = store();
        let ids = slot_ids(&current);
        let done = rotated(&current);
        let new_key = done.vault.payload().age_identity().unwrap();
        let shown = recovery(0x11).to_display();
        let secrets = [
            fold(PASSWORD),
            fold(ACCOUNT),
            fold(IDENTITY),
            fold(new_key.to_string().expose_secret()),
            fold(&shown.expose()["APPRAFTER-RECOVERY-KEY-1\n".len()..]),
            hex(done.vault.data_key().expose()),
            hex(current.data_key().expose()),
            hex(&[HERE; 16]),
        ];
        let outputs = [
            format!("{done:?}"),
            format!(
                "{:?}",
                current.keep(ids[0], kept_password(PASSWORD), FAST).unwrap()
            ),
            format!(
                "{:?}",
                current.keep(ids[1], kept_account(HERE), FAST).unwrap()
            ),
            format!("{:?}", kept_password(PASSWORD)),
            format!("{:?}", kept_account(HERE)),
            format!("{:?}", kept_recovery(0x11)),
            format!("{:?}", given_ids(1)),
        ];
        for output in &outputs {
            let folded = fold(output);
            for secret in &secrets {
                assert!(!folded.contains(secret.as_str()), "{output} shows a secret");
            }
        }
    }
}
