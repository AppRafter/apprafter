// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Where a locked store's secrets open (ADR 0068 §5; D.4a T12, T41, T48, §3.4). The core never
//! prompts (ADR 0067 §2): each client injects a [`VaultAccess`] into its `Context`, which
//! decides how a locked store opens there — a key this process holds, a prompt, a terminal's
//! unlock window, or a refusal.
//!
//! A [`VaultSession`] opens items of the vault bytes its caller read (lockless, R3): it never
//! reads a file and never takes the store lock, so what it answers is what the caller read.
//! Vault writes never go through a session (T41, O11): they take a [`KeySession`] from
//! [`VaultAccess::write_key`] — a key this process holds — and write under the held write lock.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli_core::secrets::{
    decrypt_with_identity_reason, CacheKeyKind, CachedSecret, Identity, Recipient,
};
use cli_core::CliError;
use zeroize::Zeroizing;

use super::locked::{vault_error, vault_path, VAULT_WRITER};
use super::store_file::{SlotInfo, StoreId};
use super::store_locked;
use crate::context::SecretString;
use crate::vault::{DataKey, UnlockedVault, VaultFile, VaultId};

/// What a client is asked to open: the store, by its root and its id, and its slots as
/// `store.yaml` mirrors them (for a prompt to name).
#[derive(Debug, Clone, Copy)]
pub struct VaultRequest<'a> {
    pub store_root: &'a Path,
    pub store_id: &'a StoreId,
    pub slots: &'a [SlotInfo],
}

/// How a locked store opens in this client. A refusal is `store_locked`, `unlock_failed` or the
/// vault's own problem — never an empty session (rule 3).
pub trait VaultAccess: Send + Sync + fmt::Debug {
    /// A read session for `req`'s store. A CLI asks at most once per process and keeps it
    /// (d4a-d).
    fn session(&self, req: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>>;

    /// The key a vault write uses, never a holder's (T41): the CLI's from a prompt in this
    /// process, the desktop's while it is unlocked.
    fn write_key(&self, req: &VaultRequest<'_>) -> cli_core::Result<KeySession>;

    /// What a vault write records as its writer (§3.7: printable ASCII, at most 64 bytes):
    /// the core's `apprafter <version>` unless the client names itself — the desktop's access
    /// returns `apprafter-desktop <version>` (d4a-g). The core cannot know which client links
    /// it.
    fn writer(&self) -> String {
        VAULT_WRITER.to_string()
    }

    /// The command's secret phase ended: zeroize an in-process key, keep the items opened
    /// (d4a-d; T46, critic S5).
    fn end_phase(&self) {}

    /// Drop whatever this process holds (the desktop's app lock, d4a-g).
    fn forget(&self) {}
}

/// Items of one vault, opened from bytes the caller read.
pub trait VaultSession: Send + Sync + fmt::Debug {
    /// `target`'s token; `None` only once the vault authenticated and holds none for it.
    fn hetzner_token(&self, vault: &[u8], target: &str) -> cli_core::Result<Option<SecretString>>;

    /// `armored`, `target`'s cached `secret`, opened with the store's identity, which never
    /// leaves the session. A failure names the secret and the target (`cache_undecryptable`).
    fn open_cache(
        &self,
        vault: &[u8],
        secret: CachedSecret,
        target: &str,
        armored: &str,
    ) -> cli_core::Result<Zeroizing<String>>;

    /// The recipient of the store's identity, derived from the authenticated identity, for
    /// new caches (T26: written nowhere).
    fn recipient(&self, vault: &[u8]) -> cli_core::Result<Recipient>;
}

/// A session over a data key this process holds — the CLI's after a prompt, the desktop's while
/// unlocked, a test's — for the vault it came from (`vault_id`): a key for another vault is
/// `store_changed`, never damage (rule 3). Its `Debug` never shows the key.
#[derive(Clone)]
pub struct KeySession {
    key: Arc<DataKey>,
    vault_id: VaultId,
    root: PathBuf,
}

impl KeySession {
    /// What `key` opens: the vault `vault_id` of the store at `root`.
    pub fn new(key: Arc<DataKey>, vault_id: VaultId, root: PathBuf) -> Self {
        KeySession {
            key,
            vault_id,
            root,
        }
    }

    /// The vault this key opens.
    pub fn vault_id(&self) -> VaultId {
        self.vault_id
    }

    /// `vault`, authenticated with this key.
    pub(super) fn open(&self, vault: &[u8]) -> cli_core::Result<UnlockedVault> {
        VaultFile::parse(vault)
            .and_then(|parsed| parsed.open_with_key(&self.key, self.vault_id))
            .map_err(|e| vault_error(&self.root, e))
    }

    /// The store's own age identity, from the authenticated payload.
    fn identity(&self, vault: &[u8]) -> cli_core::Result<Identity> {
        self.open(vault)?
            .payload()
            .age_identity()
            .map_err(|e| vault_error(&self.root, e))
    }
}

impl fmt::Debug for KeySession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeySession")
            .field("vault_id", &hex(&self.vault_id.0))
            .field("root", &self.root)
            .field("key", &"<redacted>")
            .finish()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl VaultSession for KeySession {
    fn hetzner_token(&self, vault: &[u8], target: &str) -> cli_core::Result<Option<SecretString>> {
        // c1's `hetzner_token` hands out an owned, zeroed copy (its Deviation 12).
        Ok(self.open(vault)?.payload().hetzner_token(target))
    }

    fn open_cache(
        &self,
        vault: &[u8],
        secret: CachedSecret,
        target: &str,
        armored: &str,
    ) -> cli_core::Result<Zeroizing<String>> {
        let identity = self.identity(vault)?;
        decrypt_with_identity_reason(armored, &identity)
            .map(Zeroizing::new)
            .map_err(|u| CliError::CacheUndecryptable {
                secret,
                target: target.to_string(),
                path: vault_path(&self.root).display().to_string(),
                detail: u.detail,
                key: CacheKeyKind::StoreVault,
                reason: u.why,
            })
    }

    fn recipient(&self, vault: &[u8]) -> cli_core::Result<Recipient> {
        Ok(self.identity(vault)?.to_public())
    }
}

/// Every context's default: no key, and nothing here can ask for one. Each locked-store secret
/// refuses `store_locked{no_terminal}`, so a locked-store test without a key fails closed.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoVaultAccess;

impl VaultAccess for NoVaultAccess {
    fn session(&self, req: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>> {
        Err(store_locked(req.store_root))
    }

    fn write_key(&self, req: &VaultRequest<'_>) -> cli_core::Result<KeySession> {
        Err(store_locked(req.store_root))
    }
}

/// A fixed data key, for a locked store's tests (`secrets::testing::locked_store` hands one
/// out). Never in a shipped build. Its `Debug` never shows the key.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
pub struct FixedVaultAccess {
    key: Arc<DataKey>,
    vault_id: VaultId,
}

#[cfg(any(test, feature = "test-support"))]
impl FixedVaultAccess {
    /// `key`, which opens the vault `vault_id`.
    pub fn new(key: Arc<DataKey>, vault_id: VaultId) -> Self {
        FixedVaultAccess { key, vault_id }
    }

    fn key_session(&self, root: &Path) -> KeySession {
        KeySession::new(self.key.clone(), self.vault_id, root.to_path_buf())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl fmt::Debug for FixedVaultAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FixedVaultAccess")
            .field("vault_id", &hex(&self.vault_id.0))
            .field("key", &"<redacted>")
            .finish()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl VaultAccess for FixedVaultAccess {
    fn session(&self, req: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>> {
        Ok(Arc::new(self.key_session(req.store_root)))
    }

    fn write_key(&self, req: &VaultRequest<'_>) -> cli_core::Result<KeySession> {
        Ok(self.key_session(req.store_root))
    }
}
