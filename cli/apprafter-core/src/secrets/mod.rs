// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The one door to a target store's secrets (ADR 0068 §5; D.4a overview §3.4).
//!
//! Every Hetzner token and every cached secret a core operation or a CLI arm reaches goes
//! through a [`SecretStore`], which [`Context::secrets`](crate::Context::secrets) builds per
//! call and which touches nothing until one of its methods runs.
//!
//! Each method first reads `<root>/store.yaml` ([`store_file`]), so a `lock on` in another
//! terminal is seen by the next call (D.4a T12, T31). A store without one keeps its secrets in
//! files: each target's `targets/<t>/credentials.yaml`, and the one shared age key
//! (`APPRAFTER_AGE_KEY`, else `<home>/.config/apprafter/age.key`) the caches in `state.json` are
//! encrypted to. A `locked` store keeps them in its vault: each `credentials.yaml` that held a
//! token is a [`marker`], read without opening anything, and what needs the vault refuses with
//! `store_locked` here (d4a-c2's next commit opens it through a `VaultAccess`). A `locking` or
//! `unlocking` store refuses every secret (`lock_incomplete`).
//!
//! Rules: no core code outside this module reads or writes a credentials file, reads the age
//! key, or opens or seals a cache, and nothing here seals a cache to a recipient that did not
//! come from a [`CacheKey`] (`tests/guards.rs`, D.4a T16); a read never creates the age key
//! (GOTCHA-120); a failure to decrypt is an error naming the key, never "none cached"; a locked
//! store takes no token from a clear file or another store's marker (T42, T34).

use std::fmt;
use std::path::{Path, PathBuf};

use cli_core::secrets::{CachedSecret, Identity, Recipient};
use cli_core::{CliError, LockPhase, LockedWhy, SecretsModeKind, StoreLock, TargetStorePaths};
use cli_state::HetznerCloudState;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::context::SecretString;

mod cache;
mod files;
mod locked;
#[cfg(test)]
mod locked_tests;
pub mod marker;
pub mod store_file;
#[cfg(test)]
mod tests;

pub use files::{Held, Holding};
pub use store_file::{
    read_store_file, write_store_file, LockSlotKind, SlotInfo, StoreFile, StoreId, STORE_FILE,
    STORE_FILE_FORMAT,
};

/// Where a store's secrets are; read from `<root>/store.yaml` on every call, never cached. No
/// `store.yaml` is `Files` (D.4a T31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SecretsMode {
    /// Each target's `credentials.yaml` and the shared age key.
    Files,
    /// An earlier `lock on` did not finish: every secret read refuses.
    Locking,
    /// In the store's vault, opened by a slot's secret.
    Locked,
    /// An earlier `lock off` did not finish: every secret read refuses.
    Unlocking,
}

impl SecretsMode {
    /// Its name in `store.yaml` and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            SecretsMode::Files => "files",
            SecretsMode::Locking => "locking",
            SecretsMode::Locked => "locked",
            SecretsMode::Unlocking => "unlocking",
        }
    }

    /// `text` as `store.yaml` spells it; `None` for anything else.
    pub fn parse(text: &str) -> Option<SecretsMode> {
        match text {
            "files" => Some(SecretsMode::Files),
            "locking" => Some(SecretsMode::Locking),
            "locked" => Some(SecretsMode::Locked),
            "unlocking" => Some(SecretsMode::Unlocking),
            _ => None,
        }
    }
}

impl From<SecretsMode> for SecretsModeKind {
    fn from(mode: SecretsMode) -> Self {
        match mode {
            SecretsMode::Files => SecretsModeKind::Files,
            SecretsMode::Locking => SecretsModeKind::Locking,
            SecretsMode::Locked => SecretsModeKind::Locked,
            SecretsMode::Unlocking => SecretsModeKind::Unlocking,
        }
    }
}

/// What a target's token record says, read without handing out the token or opening a vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredToken {
    /// No token: no credentials file, or one without `hetzner_token`.
    None,
    /// A token in the target's `credentials.yaml`, `chars` bytes long (tokens are ASCII).
    InFile { chars: u32 },
    /// A locked store's marker for its own vault.
    InVault,
    /// A token in clear in a locked store's `credentials.yaml`: never used (T42).
    OutsideVault,
    /// A marker for another store's vault: the target's directory came from it (T34).
    InAnotherVault { id: StoreId },
}

/// The key a target's cached secrets are read with, and new ones sealed to: the shared age
/// identity and the file it was read from. Its `Debug` never shows the identity.
pub struct CacheKey {
    identity: Identity,
    path: PathBuf,
}

impl CacheKey {
    /// The recipient a new cache under this key is encrypted to — the only kind of recipient a
    /// cache may be sealed to (`tests/guards.rs`).
    pub fn recipient(&self) -> cli_core::Result<Recipient> {
        Ok(self.identity.to_public())
    }
}

impl fmt::Debug for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CacheKey")
            .field("path", &self.path)
            .field("identity", &"<redacted>")
            .finish()
    }
}

/// The key a new cached secret of a target is written under.
#[derive(Debug)]
pub enum NewCacheKey {
    /// The key there is.
    Ready(CacheKey),
    /// No key, and nothing is cached anywhere: a first use. The caller (the CLI) creates the
    /// key at `path` once it has something to cache; the core never creates one.
    FirstUse { path: PathBuf },
}

/// What a token write needs, from [`SecretStore::prepare_write`], called BEFORE the store lock
/// is taken: a slow step (a prompt) never runs under the lock. It records the lock state it was
/// prepared for; a write re-reads `store.yaml` under the lock and refuses another
/// (`store_changed`, D.4a T41, critic G3).
#[derive(Debug)]
pub struct WriteKey(WriteFor);

#[derive(Debug)]
enum WriteFor {
    Files,
}

impl WriteFor {
    fn kind(&self) -> SecretsModeKind {
        match self {
            WriteFor::Files => SecretsModeKind::Files,
        }
    }
}

/// `store.yaml` as one call read it.
enum StoreState {
    Files,
    Locked { id: StoreId },
    Incomplete(LockPhase),
}

impl StoreState {
    fn kind(&self) -> SecretsModeKind {
        match self {
            StoreState::Files => SecretsModeKind::Files,
            StoreState::Locked { .. } => SecretsModeKind::Locked,
            StoreState::Incomplete(LockPhase::Locking) => SecretsModeKind::Locking,
            StoreState::Incomplete(LockPhase::Unlocking) => SecretsModeKind::Unlocking,
        }
    }
}

/// `store_locked` for the store at `root`: nothing in this process can open its vault.
pub(crate) fn store_locked(root: &Path) -> CliError {
    CliError::StoreLocked {
        store: root.to_path_buf(),
        reason: LockedWhy::NoTerminal,
        stopped_before: None,
    }
}

/// The one door to a target store's secrets. Cheap to build and to clone; it touches nothing
/// until a method runs, and every method reads the files again (nothing is cached).
#[derive(Debug, Clone)]
pub struct SecretStore {
    store: TargetStorePaths,
    age_key_path: PathBuf,
}

impl SecretStore {
    pub(crate) fn new(store: TargetStorePaths, age_key_path: PathBuf) -> Self {
        SecretStore {
            store,
            age_key_path,
        }
    }

    /// `store.yaml`, read now.
    fn state(&self) -> cli_core::Result<StoreState> {
        let Some(file) = read_store_file(self.store.root())? else {
            return Ok(StoreState::Files);
        };
        Ok(match (file.secrets, file.id) {
            (SecretsMode::Files, _) => StoreState::Files,
            (SecretsMode::Locking, _) => StoreState::Incomplete(LockPhase::Locking),
            (SecretsMode::Unlocking, _) => StoreState::Incomplete(LockPhase::Unlocking),
            (SecretsMode::Locked, Some(id)) => StoreState::Locked { id },
            // `read_store_file` refuses this; the same refusal, should it ever not.
            (SecretsMode::Locked, None) => {
                return Err(CliError::StoreFileUnreadable {
                    path: self.store.root().join(STORE_FILE),
                    detail: "it says the secrets are locked, and names no id".into(),
                })
            }
        })
    }

    fn incomplete(&self, phase: LockPhase) -> CliError {
        CliError::LockIncomplete {
            store: self.store.root().to_path_buf(),
            phase,
        }
    }

    /// Where this store's secrets are, read from `store.yaml` now.
    pub fn mode(&self) -> cli_core::Result<SecretsMode> {
        Ok(match self.state()? {
            StoreState::Files => SecretsMode::Files,
            StoreState::Locked { .. } => SecretsMode::Locked,
            StoreState::Incomplete(LockPhase::Locking) => SecretsMode::Locking,
            StoreState::Incomplete(LockPhase::Unlocking) => SecretsMode::Unlocking,
        })
    }

    /// `store.yaml`, read and checked; `None` for a store in files.
    pub fn store_file(&self) -> cli_core::Result<Option<StoreFile>> {
        read_store_file(self.store.root())
    }

    /// The file that records `target`'s token, for display and file checks (`target show`,
    /// doctor's credentials row). Nothing is read through this path.
    pub fn credentials_file(&self, target: &str) -> PathBuf {
        self.store.target_credentials_file(target)
    }

    /// What is stored for `target`, without the token and without a vault: in files, the
    /// record and its length; on a locked store, what its marker says. A record that cannot be
    /// read is that file's error (`InvalidTargetConfig`, which never quotes it, or
    /// `TargetFileIo`).
    pub fn token_presence(&self, target: &str) -> cli_core::Result<StoredToken> {
        match self.state()? {
            StoreState::Files => files::token_presence(&self.store, target),
            StoreState::Locked { id } => Ok(match locked::placed(&self.store, &id, target)? {
                locked::Placed::Absent => StoredToken::None,
                locked::Placed::Own => StoredToken::InVault,
                locked::Placed::Clear => StoredToken::OutsideVault,
                locked::Placed::Other(id) => StoredToken::InAnotherVault { id },
            }),
            StoreState::Incomplete(phase) => Err(self.incomplete(phase)),
        }
    }

    /// The token stored for `target` — never an override: whether `HCLOUD_TOKEN` outranks it
    /// is the caller's rule (`target::hetzner_token`, R4). `None`: no token stored. On a locked
    /// store never a clear token (T42) nor another store's.
    pub fn stored_token(&self, target: &str) -> cli_core::Result<Option<SecretString>> {
        match self.state()? {
            StoreState::Files => files::stored_token(&self.store, target),
            StoreState::Locked { id } => match locked::placed(&self.store, &id, target)? {
                locked::Placed::Absent => Ok(None),
                locked::Placed::Own => Err(store_locked(self.store.root())),
                locked::Placed::Clear => Err(CliError::TokenOutsideVault {
                    target: target.to_string(),
                    path: self.credentials_file(target),
                }),
                locked::Placed::Other(id) => Err(CliError::TokenInAnotherVault {
                    target: target.to_string(),
                    id: id.to_string(),
                }),
            },
            StoreState::Incomplete(phase) => Err(self.incomplete(phase)),
        }
    }

    /// What a token write needs. Call it BEFORE taking the store lock. Files: nothing to ask.
    pub fn prepare_write(&self) -> cli_core::Result<WriteKey> {
        match self.state()? {
            StoreState::Files => Ok(WriteKey(WriteFor::Files)),
            StoreState::Locked { .. } => Err(store_locked(self.store.root())),
            StoreState::Incomplete(phase) => Err(self.incomplete(phase)),
        }
    }

    /// Store `token` for `target` under the store lock, which `_held` is the caller's guard of,
    /// once the store is still in the state `key` was prepared for (`store_changed` else,
    /// G3). Files: `credentials.yaml`, mode 0600, written atomically, the bytes
    /// `cli_core::save_target` wrote; `config.yaml` is the caller's to write.
    pub fn put_token(
        &self,
        key: &WriteKey,
        _held: &StoreLock,
        target: &str,
        token: &SecretString,
    ) -> cli_core::Result<()> {
        self.recheck(key)?;
        match key.0 {
            WriteFor::Files => files::put_token(&self.store, target, token),
        }
    }

    /// Under the write lock: the store must still be in the state `key` was prepared for.
    fn recheck(&self, key: &WriteKey) -> cli_core::Result<()> {
        let now = self.state()?;
        if matches!((&key.0, &now), (WriteFor::Files, StoreState::Files)) {
            return Ok(());
        }
        Err(CliError::StoreChanged {
            store: self.store.root().to_path_buf(),
            prepared: key.0.kind(),
            now: now.kind(),
        })
    }

    /// The key `target`'s cached secrets are read with. Never creates one (GOTCHA-120): in
    /// files, no key is `CliError::AgeKeyMissing` naming its path and the target.
    pub fn cache_key(&self, target: &str) -> cli_core::Result<CacheKey> {
        match self.state()? {
            StoreState::Files => files::cache_key(&self.age_key_path, target),
            StoreState::Locked { .. } => Err(store_locked(self.store.root())),
            StoreState::Incomplete(phase) => Err(self.incomplete(phase)),
        }
    }

    /// The key a NEW cached secret of `target` is written under, settled before the secret is
    /// read so that a refusal costs no read. In files: the key there is; else, with nothing
    /// cached anywhere, a first use ([`NewCacheKey::FirstUse`]); else the key everything was
    /// cached under is lost, which is `CliError::AgeKeyMissing` — only `apprafter kubeconfig
    /// --refresh` replaces a lost key, after it asks (WI-457).
    pub fn cache_key_for_new_secret(&self, target: &str) -> cli_core::Result<NewCacheKey> {
        match self.state()? {
            StoreState::Files => {
                files::cache_key_for_new_secret(&self.store, &self.age_key_path, target)
            }
            StoreState::Locked { .. } => Err(store_locked(self.store.root())),
            StoreState::Incomplete(phase) => Err(self.incomplete(phase)),
        }
    }

    /// `target`'s cached kubeconfig from its state's `hetzner_cloud`; `Ok(None)` when none is
    /// cached. PRECEDENCE IS LOAD-BEARING: the encrypted slot wins over the legacy plaintext
    /// one, which an older state may still carry beside a fresh encrypted one (preferring it
    /// would hand back a stale kubeconfig). The plaintext one needs no key in files, and is
    /// refused on a locked store (`cache_not_sealed`, T38: an older AppRafter's write, or a
    /// forgery; never sealed later).
    pub fn cached_kubeconfig(
        &self,
        hetzner: &HetznerCloudState,
        target: &str,
    ) -> cli_core::Result<Option<Zeroizing<String>>> {
        match (
            hetzner.kubeconfig_age.as_deref(),
            hetzner.kubeconfig_yaml.as_deref(),
        ) {
            (None, None) => Ok(None),
            (Some(armored), _) => {
                let key = self.cache_key(target)?;
                self.open_cache(&key, CachedSecret::Kubeconfig, target, armored)
                    .map(Some)
            }
            (None, Some(plain)) => match self.state()? {
                StoreState::Files => Ok(Some(Zeroizing::new(plain.to_string()))),
                StoreState::Locked { .. } => Err(CliError::CacheNotSealed {
                    secret: CachedSecret::Kubeconfig,
                    target: target.to_string(),
                }),
                StoreState::Incomplete(phase) => Err(self.incomplete(phase)),
            },
        }
    }

    /// `armored`, `target`'s cached `secret`, opened with `key`. A key that does not open it
    /// is `CliError::CacheUndecryptable` naming the secret, the target and the key.
    pub fn open_cache(
        &self,
        key: &CacheKey,
        secret: CachedSecret,
        target: &str,
        armored: &str,
    ) -> cli_core::Result<Zeroizing<String>> {
        cache::open(key, secret, target, armored)
    }

    /// Everything the store's targets cache under the shared age key, by target name, the
    /// kubeconfig before the password; a state that cannot be read counts as holding
    /// something. Read-only. What the CLI lists before it replaces a lost key (WI-457).
    pub fn held(&self) -> cli_core::Result<Vec<Held>> {
        files::held(&self.store)
    }
}
