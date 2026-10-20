// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Vault format v1 (D.4a overview §3.7; ADR 0068 §2): a strict, bounded codec for the plain
//! header and its slot records. No crypto here.
//!
//! Layout, little-endian, explicit lengths, no trailing bytes, at most [`VAULT_FILE_MAX`]:
//!
//! ```text
//! magic (8) ‖ format_version u16 ‖ writer_len u8 ‖ writer ‖ flags u16 ‖ vault_id (16) ‖
//! store_id_len u8 ‖ store_id ‖ slot_count u8 ‖ slot records ‖ payload_nonce (24) ‖
//! payload_len u32 ‖ payload_ct
//! slot record = record_len u16 ‖ slot_id (16) ‖ kind u8 ‖ body_len u16 ‖ body ‖ label_len u8 ‖ label
//! ```
//!
//! The payload's associated data is every byte before `payload_ct`.
//!
//! **Version rule (critic S8).** The magic, the version and the writer keep their place in
//! every version, so this reader can name the writer of a newer file. A version that changes
//! what a slot record or the payload means also changes [`SLOT_AAD_DOMAIN`] and the HKDF info
//! strings (`kdf.rs`), so rewriting a newer file's version byte can never make this reader
//! accept one of its slots; lowering `store.yaml`'s `format` is harmless, because this header
//! still refuses. Within v1 a slot of an unknown kind and a payload entry of an unknown key are
//! kept byte for byte: the version changes only when an existing byte changes meaning.
//!
//! The header is parsed before anything authenticates it. [`VaultFile::header`] is what
//! `lock status`, doctor and the desktop may show without a secret; nothing in it is trusted
//! until a slot or a held data key opens the payload.

use std::collections::HashSet;
use std::fmt;

use super::error::{InputWhat, MalformedWhat, SizeWhat, VaultError, VaultResult};
use super::kdf::Argon2idParams;

/// `89 41 52 56 0D 0A 1A 0A`, PNG-style: a text-mode copy (`\r\n` rewritten, bit 7 cleared)
/// reads as not a vault rather than as a tampered one. Frozen across versions.
pub const MAGIC: [u8; 8] = *b"\x89ARV\r\n\x1a\n";
/// The one version this build reads and writes.
pub const FORMAT_V1: u16 = 1;
/// The read cap and the largest file a write may produce: 2 MiB.
pub const VAULT_FILE_MAX: usize = 2 * 1024 * 1024;
/// The largest payload plaintext: 1 MiB.
pub const PAYLOAD_MAX: usize = 1024 * 1024;
/// The writer field: `apprafter <version>` or `apprafter-desktop <version>`, at most 64 bytes.
pub const WRITER_MAX: usize = 64;
/// The store id field: `store.yaml`'s `id`, at most 64 bytes.
pub const STORE_ID_MAX: usize = 64;
/// At most 16 slots.
pub const SLOT_MAX: usize = 16;
/// A slot label: at most 64 bytes.
pub const LABEL_MAX: usize = 64;
/// The prefix of every slot's wrap associated data (§3.7). Changes with any version that
/// changes what a slot means (the version rule above).
pub const SLOT_AAD_DOMAIN: &[u8] = b"apprafter-vault/v1/slot-wrap\0";

pub(crate) const NONCE_LEN: usize = 24;
pub(crate) const TAG_LEN: usize = 16;
pub(crate) const SALT_LEN: usize = 16;
pub(crate) const KEY_CHECK_LEN: usize = 32;
pub(crate) const WRAPPED_DK_LEN: usize = 48;
pub(crate) const PASSWORD_BODY_LEN: usize = 133;
pub(crate) const RECOVERY_BODY_LEN: usize = 121;
const KDF_NONE: u8 = 0x00;
const KDF_ARGON2ID: u8 = 0x01;

/// Random at creation; a new data key means a new vault and a new id.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VaultId(pub [u8; 16]);

/// Random, immutable and unique in its vault: what a slot's wrap is bound to, never its position.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotId(pub [u8; 16]);

fn hex16(bytes: &[u8; 16], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    bytes.iter().try_for_each(|b| write!(f, "{b:02x}"))
}

impl fmt::Display for VaultId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        hex16(&self.0, f)
    }
}

impl fmt::Debug for VaultId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VaultId({self})")
    }
}

impl fmt::Display for SlotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        hex16(&self.0, f)
    }
}

impl fmt::Debug for SlotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SlotId({self})")
    }
}

/// A slot's kind byte. D.4a creates and opens `Password` (`0x01`) and `RecoveryKey` (`0x02`);
/// `0x03` to `0x05` are reserved for D.4b and later; every other byte is unknown. A reserved
/// or unknown slot is kept byte for byte and never opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotKind {
    Password,
    RecoveryKey,
    OsPassword,
    WindowsHello,
    TouchId,
    Unknown(u8),
}

impl SlotKind {
    pub fn from_byte(byte: u8) -> SlotKind {
        match byte {
            0x01 => SlotKind::Password,
            0x02 => SlotKind::RecoveryKey,
            0x03 => SlotKind::OsPassword,
            0x04 => SlotKind::WindowsHello,
            0x05 => SlotKind::TouchId,
            other => SlotKind::Unknown(other),
        }
    }

    pub fn byte(self) -> u8 {
        match self {
            SlotKind::Password => 0x01,
            SlotKind::RecoveryKey => 0x02,
            SlotKind::OsPassword => 0x03,
            SlotKind::WindowsHello => 0x04,
            SlotKind::TouchId => 0x05,
            SlotKind::Unknown(byte) => byte,
        }
    }
}

impl fmt::Display for SlotKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlotKind::Password => f.write_str("lock password"),
            SlotKind::RecoveryKey => f.write_str("recovery key"),
            SlotKind::OsPassword => f.write_str("OS password"),
            SlotKind::WindowsHello => f.write_str("Windows Hello"),
            SlotKind::TouchId => f.write_str("Touch ID"),
            SlotKind::Unknown(byte) => write!(f, "slot of kind {byte:#04x}"),
        }
    }
}

/// A slot's body. Nothing here is secret: the salt and the parameters are public, `key_check`
/// is a commitment, `wrapped_dk` is ciphertext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotBody {
    /// 133 bytes: `0x01` (Argon2id v0x13) ‖ m u32 ‖ t u32 ‖ p u32 ‖ salt ‖ wrap_nonce ‖ key_check ‖ wrapped_dk.
    Password {
        kdf: Argon2idParams,
        salt: [u8; SALT_LEN],
        wrap_nonce: [u8; NONCE_LEN],
        key_check: [u8; KEY_CHECK_LEN],
        wrapped_dk: [u8; WRAPPED_DK_LEN],
    },
    /// 121 bytes: `0x00` (no KDF) ‖ salt ‖ wrap_nonce ‖ key_check ‖ wrapped_dk.
    RecoveryKey {
        salt: [u8; SALT_LEN],
        wrap_nonce: [u8; NONCE_LEN],
        key_check: [u8; KEY_CHECK_LEN],
        wrapped_dk: [u8; WRAPPED_DK_LEN],
    },
    /// A reserved or unknown kind's body, kept byte for byte.
    Opaque(Vec<u8>),
}

impl SlotBody {
    /// The body up to the end of its salt: what a slot's wrap associated data binds (§3.7).
    /// Empty for an opaque body, which is never wrapped or opened here.
    pub(crate) fn kdf_block(&self) -> Vec<u8> {
        match self {
            SlotBody::Password { kdf, salt, .. } => {
                let mut out = Vec::with_capacity(1 + 12 + SALT_LEN);
                out.push(KDF_ARGON2ID);
                out.extend_from_slice(&kdf.m_cost_kib.to_le_bytes());
                out.extend_from_slice(&kdf.t_cost.to_le_bytes());
                out.extend_from_slice(&kdf.p_cost.to_le_bytes());
                out.extend_from_slice(salt);
                out
            }
            SlotBody::RecoveryKey { salt, .. } => {
                let mut out = Vec::with_capacity(1 + SALT_LEN);
                out.push(KDF_NONE);
                out.extend_from_slice(salt);
                out
            }
            SlotBody::Opaque(_) => Vec::new(),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            SlotBody::Password {
                wrap_nonce,
                key_check,
                wrapped_dk,
                ..
            }
            | SlotBody::RecoveryKey {
                wrap_nonce,
                key_check,
                wrapped_dk,
                ..
            } => {
                out.extend_from_slice(&self.kdf_block());
                out.extend_from_slice(wrap_nonce);
                out.extend_from_slice(key_check);
                out.extend_from_slice(wrapped_dk);
            }
            SlotBody::Opaque(bytes) => out.extend_from_slice(bytes),
        }
    }

    fn encoded_len(&self) -> usize {
        match self {
            SlotBody::Password { .. } => PASSWORD_BODY_LEN,
            SlotBody::RecoveryKey { .. } => RECOVERY_BODY_LEN,
            SlotBody::Opaque(bytes) => bytes.len(),
        }
    }
}

/// One slot as the file holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRecord {
    pub id: SlotId,
    pub kind: SlotKind,
    pub body: SlotBody,
    pub label: String,
}

/// The plain part of a vault: who wrote it, which vault and store it is, its slots in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultHeader {
    pub format_version: u16,
    pub writer: String,
    pub vault_id: VaultId,
    pub store_id: String,
    pub slots: Vec<SlotRecord>,
}

impl VaultHeader {
    /// Refuses a vault that belongs to another store: `expected` is `store.yaml`'s `id` (T32).
    pub fn check_store_id(&self, expected: &str) -> VaultResult<()> {
        if self.store_id == expected {
            Ok(())
        } else {
            Err(VaultError::StoreMismatch {
                vault_store_id: self.store_id.clone(),
                store_id: expected.to_string(),
            })
        }
    }

    /// Every byte before the payload ciphertext — the payload's associated data — for a payload
    /// of `payload_ct_len` bytes (ciphertext and tag). It checks every per-field rule of
    /// `parse`; whole-header rules (two slots with one id) are enforced by `seal`'s parse-back,
    /// so [`VaultFile::to_bytes`] is only for headers `parse` produced.
    pub(crate) fn encode_aad(
        &self,
        payload_nonce: &[u8; NONCE_LEN],
        payload_ct_len: usize,
    ) -> VaultResult<Vec<u8>> {
        check_writer(self.writer.as_bytes())
            .map_err(|_| VaultError::InvalidInput(InputWhat::Writer))?;
        check_store_id(self.store_id.as_bytes())
            .map_err(|_| VaultError::InvalidInput(InputWhat::StoreId))?;
        if self.slots.is_empty() {
            return Err(malformed(MalformedWhat::SlotCount, 0));
        }
        if self.slots.len() > SLOT_MAX {
            return Err(VaultError::TooLarge {
                what: SizeWhat::Slots,
                limit: SLOT_MAX as u64,
            });
        }
        if !(TAG_LEN..=PAYLOAD_MAX + TAG_LEN).contains(&payload_ct_len) {
            return Err(VaultError::TooLarge {
                what: SizeWhat::Payload,
                limit: PAYLOAD_MAX as u64,
            });
        }
        let mut out = Vec::with_capacity(256 + self.slots.len() * 256);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_V1.to_le_bytes());
        out.push(self.writer.len() as u8);
        out.extend_from_slice(self.writer.as_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.vault_id.0);
        out.push(self.store_id.len() as u8);
        out.extend_from_slice(self.store_id.as_bytes());
        out.push(self.slots.len() as u8);
        for slot in &self.slots {
            encode_slot(slot, &mut out)?;
        }
        out.extend_from_slice(payload_nonce);
        out.extend_from_slice(&(payload_ct_len as u32).to_le_bytes());
        Ok(out)
    }
}

fn encode_slot(slot: &SlotRecord, out: &mut Vec<u8>) -> VaultResult<()> {
    // `Unknown(b)` for a byte `from_byte` names (0x01 to 0x05) would be read back as that kind.
    if SlotKind::from_byte(slot.kind.byte()) != slot.kind {
        return Err(VaultError::Malformed {
            what: MalformedWhat::BodyLength,
            offset: out.len(),
        });
    }
    let body_matches_kind = match (&slot.kind, &slot.body) {
        (SlotKind::Password, SlotBody::Password { .. }) => true,
        (SlotKind::RecoveryKey, SlotBody::RecoveryKey { .. }) => true,
        (SlotKind::Password | SlotKind::RecoveryKey, _) => false,
        (_, body) => matches!(body, SlotBody::Opaque(_)),
    };
    if !body_matches_kind {
        return Err(VaultError::Malformed {
            what: MalformedWhat::BodyLength,
            offset: out.len(),
        });
    }
    check_label(slot.label.as_bytes()).map_err(|_| VaultError::InvalidInput(InputWhat::Label))?;
    let body_len = slot.body.encoded_len();
    let record_len = 16 + 1 + 2 + body_len + 1 + slot.label.len();
    let (Ok(record_len), Ok(body_len)) = (u16::try_from(record_len), u16::try_from(body_len))
    else {
        return Err(VaultError::TooLarge {
            what: SizeWhat::File,
            limit: VAULT_FILE_MAX as u64,
        });
    };
    out.extend_from_slice(&record_len.to_le_bytes());
    out.extend_from_slice(&slot.id.0);
    out.push(slot.kind.byte());
    out.extend_from_slice(&body_len.to_le_bytes());
    slot.body.encode_into(out);
    out.push(slot.label.len() as u8);
    out.extend_from_slice(slot.label.as_bytes());
    Ok(())
}

/// A parsed vault, **not** authenticated: the header to show, and the payload to open.
pub struct VaultFile {
    header: VaultHeader,
    pub(super) payload_aad: Vec<u8>,
    pub(super) payload_nonce: [u8; NONCE_LEN],
    pub(super) payload_ct: Vec<u8>,
}

impl fmt::Debug for VaultFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultFile")
            .field("header", &self.header)
            .field("payload_ct_len", &self.payload_ct.len())
            .finish_non_exhaustive()
    }
}

impl VaultFile {
    /// Parses a vault file: magic, then the frozen prefix (version and writer), then the v1
    /// header, every bound checked before anything is allocated from it.
    pub fn parse(bytes: &[u8]) -> VaultResult<VaultFile> {
        if bytes.len() > VAULT_FILE_MAX {
            return Err(malformed(MalformedWhat::TooLarge, VAULT_FILE_MAX));
        }
        if bytes.len() < MAGIC.len() || bytes[..MAGIC.len()] != MAGIC[..] {
            return Err(VaultError::NotAVault);
        }
        let mut r = Reader::new(bytes, MAGIC.len());
        let format_version = r.u16()?;
        let writer_at = r.pos;
        let writer_len = usize::from(r.u8()?);
        let writer =
            check_writer(r.take(writer_len)?).map_err(|what| malformed(what, writer_at))?;
        if format_version != FORMAT_V1 {
            return Err(VaultError::UnsupportedVersion {
                found: format_version,
                writer,
            });
        }
        let flags_at = r.pos;
        if r.u16()? != 0 {
            return Err(malformed(MalformedWhat::Flags, flags_at));
        }
        let vault_id = VaultId(r.array()?);
        let store_at = r.pos;
        let store_len = usize::from(r.u8()?);
        let store_id =
            check_store_id(r.take(store_len)?).map_err(|what| malformed(what, store_at))?;
        let count_at = r.pos;
        let slot_count = usize::from(r.u8()?);
        if slot_count == 0 || slot_count > SLOT_MAX {
            return Err(malformed(MalformedWhat::SlotCount, count_at));
        }
        let mut slots = Vec::with_capacity(slot_count);
        let mut ids = HashSet::with_capacity(slot_count);
        for _ in 0..slot_count {
            let at = r.pos;
            let slot = parse_slot(&mut r)?;
            if !ids.insert(slot.id) {
                return Err(malformed(MalformedWhat::DuplicateSlotId, at));
            }
            slots.push(slot);
        }
        let payload_nonce = r.array()?;
        let len_at = r.pos;
        let payload_len = r.u32()? as usize;
        if !(TAG_LEN..=PAYLOAD_MAX + TAG_LEN).contains(&payload_len) {
            return Err(malformed(MalformedWhat::PayloadLength, len_at));
        }
        let aad_end = r.pos;
        let payload_ct = r.take(payload_len)?.to_vec();
        if r.pos != bytes.len() {
            return Err(malformed(MalformedWhat::TrailingBytes, r.pos));
        }
        Ok(VaultFile {
            header: VaultHeader {
                format_version,
                writer,
                vault_id,
                store_id,
                slots,
            },
            payload_aad: bytes[..aad_end].to_vec(),
            payload_nonce,
            payload_ct,
        })
    }

    /// The header as read, not yet authenticated.
    pub fn header(&self) -> &VaultHeader {
        &self.header
    }

    /// The file re-encoded from what `parse` read: byte for byte the input of every file
    /// `parse` accepts (one encoding per vault). No parse-back: the header is the one `parse`
    /// produced, so the whole-header rules already hold.
    pub fn to_bytes(&self) -> VaultResult<Vec<u8>> {
        let mut out = self
            .header
            .encode_aad(&self.payload_nonce, self.payload_ct.len())?;
        out.extend_from_slice(&self.payload_ct);
        Ok(out)
    }
}

fn malformed(what: MalformedWhat, offset: usize) -> VaultError {
    VaultError::Malformed { what, offset }
}

/// A bounds-checked cursor over the file.
struct Reader<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(input: &'a [u8], pos: usize) -> Self {
        Reader { input, pos }
    }

    fn take(&mut self, n: usize) -> VaultResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.input.len())
            .ok_or_else(|| malformed(MalformedWhat::Truncated, self.input.len()))?;
        let out = &self.input[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> VaultResult<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> VaultResult<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> VaultResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn array<const N: usize>(&mut self) -> VaultResult<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
}

fn parse_slot(r: &mut Reader<'_>) -> VaultResult<SlotRecord> {
    let start = r.pos;
    let record_len = usize::from(r.u16()?);
    let record_at = r.pos;
    let record = r.take(record_len)?;
    // Running out of bytes inside a record means its own length is wrong.
    let short = |_: VaultError| malformed(MalformedWhat::RecordLength, start);
    let mut s = Reader::new(record, 0);
    let id = SlotId(s.array().map_err(short)?);
    let kind = SlotKind::from_byte(s.u8().map_err(short)?);
    let body_len = usize::from(s.u16().map_err(short)?);
    let body_at = record_at + s.pos;
    let body = s.take(body_len).map_err(short)?;
    let label_at = record_at + s.pos;
    let label_len = usize::from(s.u8().map_err(short)?);
    let label = s.take(label_len).map_err(short)?;
    if s.pos != record.len() {
        return Err(malformed(MalformedWhat::RecordLength, start));
    }
    let label = check_label(label).map_err(|what| malformed(what, label_at))?;
    let body = match kind {
        SlotKind::Password => parse_password_body(body, body_at)?,
        SlotKind::RecoveryKey => parse_recovery_body(body, body_at)?,
        _ => SlotBody::Opaque(body.to_vec()),
    };
    Ok(SlotRecord {
        id,
        kind,
        body,
        label,
    })
}

fn parse_password_body(body: &[u8], at: usize) -> VaultResult<SlotBody> {
    if body.len() != PASSWORD_BODY_LEN {
        return Err(malformed(MalformedWhat::BodyLength, at));
    }
    let mut b = Reader::new(body, 0);
    if b.u8()? != KDF_ARGON2ID {
        return Err(malformed(MalformedWhat::KdfId, at));
    }
    let kdf = Argon2idParams {
        m_cost_kib: b.u32()?,
        t_cost: b.u32()?,
        p_cost: b.u32()?,
    };
    Ok(SlotBody::Password {
        kdf,
        salt: b.array()?,
        wrap_nonce: b.array()?,
        key_check: b.array()?,
        wrapped_dk: b.array()?,
    })
}

fn parse_recovery_body(body: &[u8], at: usize) -> VaultResult<SlotBody> {
    if body.len() != RECOVERY_BODY_LEN {
        return Err(malformed(MalformedWhat::BodyLength, at));
    }
    let mut b = Reader::new(body, 0);
    if b.u8()? != KDF_NONE {
        return Err(malformed(MalformedWhat::KdfId, at));
    }
    Ok(SlotBody::RecoveryKey {
        salt: b.array()?,
        wrap_nonce: b.array()?,
        key_check: b.array()?,
        wrapped_dk: b.array()?,
    })
}

/// 1..=64 printable ASCII bytes (space included).
pub(crate) fn check_writer(bytes: &[u8]) -> Result<String, MalformedWhat> {
    if bytes.is_empty() || bytes.len() > WRITER_MAX {
        return Err(MalformedWhat::WriterLength);
    }
    if !bytes.iter().all(|b| (0x20..=0x7e).contains(b)) {
        return Err(MalformedWhat::WriterText);
    }
    Ok(String::from_utf8(bytes.to_vec()).expect("printable ASCII is UTF-8"))
}

/// 1..=64 printable ASCII bytes without spaces.
pub(crate) fn check_store_id(bytes: &[u8]) -> Result<String, MalformedWhat> {
    if bytes.is_empty() || bytes.len() > STORE_ID_MAX {
        return Err(MalformedWhat::StoreIdLength);
    }
    if !bytes.iter().all(|b| (0x21..=0x7e).contains(b)) {
        return Err(MalformedWhat::StoreIdText);
    }
    Ok(String::from_utf8(bytes.to_vec()).expect("printable ASCII is UTF-8"))
}

/// At most 64 bytes of UTF-8 without control characters.
pub(crate) fn check_label(bytes: &[u8]) -> Result<String, MalformedWhat> {
    if bytes.len() > LABEL_MAX {
        return Err(MalformedWhat::LabelLength);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| MalformedWhat::LabelText)?;
    if text.chars().any(char::is_control) {
        return Err(MalformedWhat::LabelText);
    }
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::testutil::{hex, seq, seq_array, unhex};

    /// The fast-params vector's header as `gen_v1_vectors.py` (libsodium, libargon2, Python)
    /// encoded it: every byte before its 328-byte payload ciphertext.
    const FAST_PARAMS_AAD: &str = "\
        894152560d0a1a0a01001661707072616674657220302e302e302d766563746f720000606162636465\
        666768696a6b6c6d6e6f103031323334353637383961626364656602a600707172737475767778797a\
        7b7c7d7e7f01850001200000000100000001000000808182838485868788898a8b8c8d8e8f90919293\
        9495969798999a9b9c9d9e9fa0a1a2a3a4a5a6a7da4ce96dc431ff7760669d76972a74b70060fa5f0c\
        7ddd39ed39a46e28e9d8beab17cb2e6c13f98a25a1196112a6e13bdf447d66cb05f2af842bc21e09bd\
        2a6e1ed51121f98d4c9c4dade66ec6aa06540d6c6f636b2070617373776f72649900a8a9aaabacadae\
        afb0b1b2b3b4b5b6b702790000b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3\
        d4d5d6d7d8d9dadbdcdddedfaf94e6ba00de085ec7093f25c8089817fded1dec500590a2e35311015c\
        27d99bb415d2c5ae9eb5368c04bb9473809045d6c873734cd8e70b4bbbe6bc034c652dfc6e4d0ea589\
        e2b829578ee56e41d9de0c7265636f76657279206b6579e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1\
        f2f3f4f5f6f748010000";

    fn fast_params_header() -> VaultHeader {
        let array = |text: &str| -> Vec<u8> { unhex(text) };
        VaultHeader {
            format_version: FORMAT_V1,
            writer: "apprafter 0.0.0-vector".into(),
            vault_id: VaultId(seq_array(0x60)),
            store_id: "0123456789abcdef".into(),
            slots: vec![
                SlotRecord {
                    id: SlotId(seq_array(0x70)),
                    kind: SlotKind::Password,
                    body: SlotBody::Password {
                        kdf: Argon2idParams {
                            m_cost_kib: 32,
                            t_cost: 1,
                            p_cost: 1,
                        },
                        salt: seq_array(0x80),
                        wrap_nonce: seq_array(0x90),
                        key_check: array(
                            "da4ce96dc431ff7760669d76972a74b70060fa5f0c7ddd39ed39a46e28e9d8be",
                        )
                        .try_into()
                        .unwrap(),
                        wrapped_dk: array(
                            "ab17cb2e6c13f98a25a1196112a6e13bdf447d66cb05f2af842bc21e09bd2a6e\
                             1ed51121f98d4c9c4dade66ec6aa0654",
                        )
                        .try_into()
                        .unwrap(),
                    },
                    label: "lock password".into(),
                },
                SlotRecord {
                    id: SlotId(seq_array(0xa8)),
                    kind: SlotKind::RecoveryKey,
                    body: SlotBody::RecoveryKey {
                        salt: seq_array(0xb8),
                        wrap_nonce: seq_array(0xc8),
                        key_check: array(
                            "af94e6ba00de085ec7093f25c8089817fded1dec500590a2e35311015c27d99b",
                        )
                        .try_into()
                        .unwrap(),
                        wrapped_dk: array(
                            "b415d2c5ae9eb5368c04bb9473809045d6c873734cd8e70b4bbbe6bc034c652d\
                             fc6e4d0ea589e2b829578ee56e41d9de",
                        )
                        .try_into()
                        .unwrap(),
                    },
                    label: "recovery key".into(),
                },
            ],
        }
    }

    /// A header with every kind of slot: password, recovery key, and an unknown kind.
    fn three_slot_header() -> VaultHeader {
        let mut header = fast_params_header();
        header.writer = "apprafter 0.0.0-test".into();
        header.slots.push(SlotRecord {
            id: SlotId([0x30; 16]),
            kind: SlotKind::Unknown(0x7f),
            body: SlotBody::Opaque(vec![0x5a; 40]),
            label: "future slot".into(),
        });
        header
    }

    /// `header` followed by a 40-byte stand-in for the payload ciphertext.
    fn file(header: &VaultHeader) -> Vec<u8> {
        let mut out = header.encode_aad(&[0xe0; 24], 40).unwrap();
        out.extend_from_slice(&[0xcc; 40]);
        out
    }

    fn malformed_what(result: VaultResult<VaultFile>) -> MalformedWhat {
        match result {
            Err(VaultError::Malformed { what, .. }) => what,
            other => panic!("expected Malformed, got {:?}", other.map(|_| "a vault")),
        }
    }

    /// Offsets in [`three_slot_header`]'s file (writer 20 bytes, store id 16, labels 13/12/11).
    const WRITER_AT: usize = 11;
    const FLAGS_AT: usize = 31;
    const STORE_ID_AT: usize = 50;
    const SLOT_COUNT_AT: usize = 66;
    const SLOT1_AT: usize = 67;
    const SLOT1_KDF_AT: usize = SLOT1_AT + 2 + 16 + 1 + 2;
    const SLOT1_LABEL_AT: usize = SLOT1_KDF_AT + 133 + 1;
    const SLOT2_AT: usize = SLOT1_AT + 2 + 166;
    const SLOT2_KIND_AT: usize = SLOT2_AT + 2 + 16;
    const SLOT2_KDF_AT: usize = SLOT2_KIND_AT + 1 + 2;

    #[test]
    fn the_header_is_encoded_as_the_independent_generator_wrote_it() {
        let aad = fast_params_header()
            .encode_aad(&seq_array(0xe0), 328)
            .unwrap();
        assert_eq!(hex(&aad), hex(&unhex(FAST_PARAMS_AAD)));
    }

    #[test]
    fn a_header_round_trips_byte_for_byte() {
        let header = three_slot_header();
        let bytes = file(&header);
        let parsed = VaultFile::parse(&bytes).unwrap();
        assert_eq!(parsed.header(), &header);
        assert_eq!(parsed.to_bytes().unwrap(), bytes);
        assert_eq!(parsed.payload_aad, bytes[..bytes.len() - 40]);
        assert_eq!(parsed.payload_nonce, [0xe0; 24]);
        assert_eq!(parsed.payload_ct, vec![0xcc; 40]);
        // the offsets the other tests poke at
        assert_eq!(&bytes[WRITER_AT..WRITER_AT + 20], b"apprafter 0.0.0-test");
        assert_eq!(&bytes[FLAGS_AT..FLAGS_AT + 2], &[0, 0]);
        assert_eq!(&bytes[STORE_ID_AT..STORE_ID_AT + 16], b"0123456789abcdef");
        assert_eq!(bytes[SLOT_COUNT_AT], 3);
        assert_eq!(bytes[SLOT1_KDF_AT], 0x01);
        assert_eq!(
            &bytes[SLOT1_LABEL_AT..SLOT1_LABEL_AT + 13],
            b"lock password"
        );
        assert_eq!(bytes[SLOT2_KIND_AT], 0x02);
        assert_eq!(bytes[SLOT2_KDF_AT], 0x00);
    }

    #[test]
    fn a_copy_mangled_in_transfer_is_not_a_vault() {
        let bytes = file(&three_slot_header());
        // git autocrlf style: the magic's CR LF becomes LF.
        let mut crlf = bytes.clone();
        crlf.remove(4);
        assert!(matches!(
            VaultFile::parse(&crlf),
            Err(VaultError::NotAVault)
        ));
        // a 7-bit transfer: bit 7 of the first byte cleared.
        let mut seven_bit = bytes.clone();
        seven_bit[0] &= 0x7f;
        assert!(matches!(
            VaultFile::parse(&seven_bit),
            Err(VaultError::NotAVault)
        ));
        for text in [&b""[..], &b"vault"[..], &b"secrets: locked\n"[..]] {
            assert!(matches!(VaultFile::parse(text), Err(VaultError::NotAVault)));
        }
    }

    #[test]
    fn a_newer_version_is_refused_and_names_its_writer() {
        let mut bytes = file(&three_slot_header());
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
        match VaultFile::parse(&bytes) {
            Err(VaultError::UnsupportedVersion { found, writer }) => {
                assert_eq!(found, 2);
                assert_eq!(writer, "apprafter 0.0.0-test");
            }
            other => panic!("{:?}", other.map(|_| "a vault")),
        }
    }

    #[test]
    fn every_truncation_and_a_trailing_byte_are_refused() {
        let bytes = file(&three_slot_header());
        for len in 0..bytes.len() {
            match VaultFile::parse(&bytes[..len]) {
                Err(VaultError::NotAVault) => assert!(len < MAGIC.len(), "length {len}"),
                Err(VaultError::Malformed { .. }) => assert!(len >= MAGIC.len(), "length {len}"),
                other => panic!("length {len}: {:?}", other.map(|_| "a vault")),
            }
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(
            malformed_what(VaultFile::parse(&longer)),
            MalformedWhat::TrailingBytes
        );
    }

    #[test]
    fn a_file_over_2_mib_is_refused_unread() {
        let mut big = MAGIC.to_vec();
        big.resize(VAULT_FILE_MAX + 1, 0);
        assert_eq!(
            malformed_what(VaultFile::parse(&big)),
            MalformedWhat::TooLarge
        );
    }

    #[test]
    fn each_field_rule_is_checked() {
        let bytes = file(&three_slot_header());
        let poke = |at: usize, value: &[u8]| {
            let mut b = bytes.clone();
            b[at..at + value.len()].copy_from_slice(value);
            malformed_what(VaultFile::parse(&b))
        };
        assert_eq!(poke(10, &[0]), MalformedWhat::WriterLength);
        assert_eq!(poke(10, &[65]), MalformedWhat::WriterLength);
        assert_eq!(poke(WRITER_AT, &[0x07]), MalformedWhat::WriterText);
        assert_eq!(poke(FLAGS_AT, &[1, 0]), MalformedWhat::Flags);
        assert_eq!(poke(FLAGS_AT, &[0, 0x80]), MalformedWhat::Flags);
        assert_eq!(poke(STORE_ID_AT - 1, &[0]), MalformedWhat::StoreIdLength);
        assert_eq!(poke(STORE_ID_AT, b" "), MalformedWhat::StoreIdText);
        assert_eq!(poke(SLOT_COUNT_AT, &[0]), MalformedWhat::SlotCount);
        assert_eq!(poke(SLOT_COUNT_AT, &[17]), MalformedWhat::SlotCount);
        assert_eq!(
            poke(SLOT1_AT, &167u16.to_le_bytes()),
            MalformedWhat::RecordLength
        );
        assert_eq!(
            poke(SLOT1_AT, &165u16.to_le_bytes()),
            MalformedWhat::RecordLength
        );
        assert_eq!(poke(SLOT1_KDF_AT, &[0x02]), MalformedWhat::KdfId);
        assert_eq!(poke(SLOT2_KDF_AT, &[0x01]), MalformedWhat::KdfId);
        // a recovery-key body under a password kind byte: 121 bytes where 133 belong
        assert_eq!(poke(SLOT2_KIND_AT, &[0x01]), MalformedWhat::BodyLength);
        assert_eq!(poke(SLOT1_LABEL_AT, &[0x07]), MalformedWhat::LabelText);
        assert_eq!(poke(SLOT1_LABEL_AT, &[0xff]), MalformedWhat::LabelText);
        let len_at = bytes.len() - 40 - 4;
        assert_eq!(
            poke(len_at, &15u32.to_le_bytes()),
            MalformedWhat::PayloadLength
        );
        assert_eq!(
            poke(len_at, &((PAYLOAD_MAX + TAG_LEN + 1) as u32).to_le_bytes()),
            MalformedWhat::PayloadLength
        );
        assert_eq!(check_label(&[b'a'; 65]), Err(MalformedWhat::LabelLength));
    }

    #[test]
    fn two_slots_with_one_id_are_refused() {
        let mut header = three_slot_header();
        header.slots[2].id = header.slots[0].id;
        assert_eq!(
            malformed_what(VaultFile::parse(&file(&header))),
            MalformedWhat::DuplicateSlotId
        );
    }

    #[test]
    fn reserved_and_unknown_kinds_are_kept_as_bytes() {
        for kind in [0x00u8, 0x03, 0x04, 0x05, 0x7f, 0xff] {
            let mut header = three_slot_header();
            header.slots[2].kind = SlotKind::from_byte(kind);
            header.slots[2].body = SlotBody::Opaque(seq(kind, 7));
            let bytes = file(&header);
            let parsed = VaultFile::parse(&bytes).unwrap();
            assert_eq!(parsed.header().slots[2].kind.byte(), kind);
            assert_eq!(
                parsed.header().slots[2].body,
                SlotBody::Opaque(seq(kind, 7))
            );
            assert_eq!(parsed.to_bytes().unwrap(), bytes);
        }
        for byte in 0..=255u8 {
            assert_eq!(SlotKind::from_byte(byte).byte(), byte);
        }
    }

    #[test]
    fn the_encoder_refuses_what_the_parser_would() {
        let encode = |header: &VaultHeader, ct_len: usize| header.encode_aad(&[0; 24], ct_len);
        let mut h = three_slot_header();
        h.writer = String::new();
        assert!(matches!(
            encode(&h, 40),
            Err(VaultError::InvalidInput(InputWhat::Writer))
        ));
        h.writer = "two\nlines".into();
        assert!(matches!(
            encode(&h, 40),
            Err(VaultError::InvalidInput(InputWhat::Writer))
        ));
        let mut h = three_slot_header();
        h.store_id = "has space".into();
        assert!(matches!(
            encode(&h, 40),
            Err(VaultError::InvalidInput(InputWhat::StoreId))
        ));
        let mut h = three_slot_header();
        h.slots[0].label = "bell\u{7}".into();
        assert!(matches!(
            encode(&h, 40),
            Err(VaultError::InvalidInput(InputWhat::Label))
        ));
        let mut h = three_slot_header();
        h.slots[0].kind = SlotKind::RecoveryKey;
        assert!(matches!(
            encode(&h, 40),
            Err(VaultError::Malformed {
                what: MalformedWhat::BodyLength,
                ..
            })
        ));
        // A known kind's byte spelled as `Unknown`: read back, it is a password slot's.
        let mut h = three_slot_header();
        h.slots[0].kind = SlotKind::Unknown(0x01);
        h.slots[0].body = SlotBody::Opaque(vec![0; 7]);
        assert!(matches!(
            encode(&h, 40),
            Err(VaultError::Malformed {
                what: MalformedWhat::BodyLength,
                ..
            })
        ));
        let mut h = three_slot_header();
        h.slots.clear();
        assert!(matches!(
            encode(&h, 40),
            Err(VaultError::Malformed {
                what: MalformedWhat::SlotCount,
                ..
            })
        ));
        let mut h = three_slot_header();
        h.slots = (0..17u8)
            .map(|i| SlotRecord {
                id: SlotId([i; 16]),
                kind: SlotKind::Unknown(0x7f),
                body: SlotBody::Opaque(vec![i]),
                label: String::new(),
            })
            .collect();
        assert!(matches!(
            encode(&h, 40),
            Err(VaultError::TooLarge {
                what: SizeWhat::Slots,
                ..
            })
        ));
        let h = three_slot_header();
        assert!(matches!(
            encode(&h, 15),
            Err(VaultError::TooLarge {
                what: SizeWhat::Payload,
                ..
            })
        ));
        assert!(matches!(
            encode(&h, PAYLOAD_MAX + TAG_LEN + 1),
            Err(VaultError::TooLarge {
                what: SizeWhat::Payload,
                ..
            })
        ));
    }

    #[test]
    fn a_vault_of_another_store_is_refused() {
        let header = three_slot_header();
        assert!(header.check_store_id("0123456789abcdef").is_ok());
        match header.check_store_id("fedcba9876543210") {
            Err(VaultError::StoreMismatch {
                vault_store_id,
                store_id,
            }) => {
                assert_eq!(vault_store_id, "0123456789abcdef");
                assert_eq!(store_id, "fedcba9876543210");
            }
            other => panic!("{other:?}"),
        }
    }
}
