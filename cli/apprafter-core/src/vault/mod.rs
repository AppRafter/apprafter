// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The vault of a locked target store (ADR 0068 §2; D.4a overview §3.7, §3.8): its file
//! format, its crypto and its writes.
//!
//! One random 256-bit data key encrypts the payload — the store's own age identity, each
//! target's Hetzner token, the default unlock timeout — with XChaCha20-Poly1305. Each slot
//! wraps that key under a key derived from its own secret: Argon2id over a lock password, or
//! the 32 bytes of a recovery key, through HKDF-SHA256. The header is plain and parsed with a
//! bound on everything; nothing in it is trusted until a slot or a held data key opens the
//! payload, whose associated data is every byte before it.
//!
//! The module reads no environment, never prompts and takes every path as an argument. Plain
//! text exists only inside `seal` and `open`, in pre-sized `Zeroizing` buffers; no type here
//! derives `Serialize`, a secret is compared only in constant time, and Argon2's working
//! memory is ours and zeroed (`tests/guards.rs`). Copies made inside the crates it calls are
//! not wiped (T29's stated limit): the Blake2b state that absorbs a password and the stack
//! buffers of Argon2's first blocks and `blake2b_long`, the `hkdf` HMAC state, bech32's encode
//! buffer, and the old buffer `to_lowercase` leaves when typed recovery-key text grows. A
//! failed authentication is a typed refusal, never "no such secret". The format's version rule
//! is in `format.rs`.

mod error;
pub mod file;
mod format;
mod kdf;
mod keys;
mod password;
mod payload;
mod recovery;
mod rng;
mod seal;

#[cfg(test)]
mod kat;
#[cfg(test)]
mod tamper;
#[cfg(test)]
mod test_alloc;
#[cfg(test)]
mod testutil;
#[cfg(test)]
mod vectors;

pub use error::{
    InputWhat, MalformedWhat, PasswordProblem, PayloadWhat, RecoveryKeyError, SizeWhat, VaultError,
    VaultResult,
};
pub use format::{
    SlotBody, SlotId, SlotKind, SlotRecord, VaultFile, VaultHeader, VaultId, FORMAT_V1, LABEL_MAX,
    MAGIC, PAYLOAD_MAX, SLOT_AAD_DOMAIN, SLOT_MAX, STORE_ID_MAX, VAULT_FILE_MAX, WRITER_MAX,
};
pub use kdf::{Argon2idParams, KdfPolicy};
pub use keys::{DataKey, SlotInput, SlotSecret};
pub use password::{
    check_new_password, AcceptedPassword, MAX_PASSWORD_BYTES, RECOMMENDED_PASSWORD_CHARS,
};
pub use payload::{
    EntryKey, VaultPayload, DEFAULT_UNLOCK_TIMEOUT_MIN, ENTRY_MAX, KEY_MAX, MAX_UNLOCK_TIMEOUT_MIN,
    VALUE_MAX,
};
pub use recovery::{looks_like_recovery_key, ConfirmedRecoveryKey, RecoveryKey, RECOVERY_KEY_HRP};
#[cfg(any(test, feature = "test-support"))]
pub use rng::ScriptedRandom;
pub use rng::{OsRandom, VaultRng};
pub use seal::{create, NewSlot, UnlockedVault};

/// The vault's file name in a target store's root.
pub const VAULT_FILE: &str = "vault";
