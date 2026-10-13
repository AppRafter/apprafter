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
//! encrypted to. A `locked` store keeps them in its vault (`crate::vault`): each
//! `credentials.yaml` that held a token is a [`marker`], read without opening anything; the
//! tokens and the store's own age identity open through the [`VaultAccess`] the client gave its
//! `Context` — a held key, a prompt, a terminal's window, or `store_locked` — and every vault
//! write runs under the store's held write lock, after re-reading the lock state its
//! [`WriteKey`] was prepared for (T28, T41). A `locking` or `unlocking` store refuses every
//! secret (`lock_incomplete`).
//!
//! Rules: no core code outside this module reads or writes a credentials file, reads the age
//! key, or opens or seals a cache, and nothing here seals a cache to a recipient that did not
//! come from a [`CacheKey`] (`tests/guards.rs`, D.4a T16); a read never creates the age key
//! (GOTCHA-120); a failure to decrypt is an error naming the key, never "none cached"; a locked
//! store takes no token from a clear file or another store's marker (T42, T34), and its caches
//! open only with its own identity (T26, T38).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli_core::secrets::{CacheKeyKind, CachedSecret, Identity, Recipient};
use cli_core::{CliError, LockPhase, LockedWhy, SecretsModeKind, StoreLock, TargetStorePaths};
use cli_state::HetznerCloudState;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::context::SecretString;

mod access;
mod cache;
mod files;
mod locked;
#[cfg(test)]
mod locked_tests;
pub mod marker;
pub mod store_file;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "test-support"))]
pub use access::FixedVaultAccess;
pub use access::{KeySession, NoVaultAccess, VaultAccess, VaultRequest, VaultSession};
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

/// The key a target's cached secrets are read with, and new ones sealed to (D.4a T26): the
/// shared age identity and its file, or a locked store's session over the vault bytes read,
/// whose identity never leaves it. Its `Debug` shows neither.
pub struct CacheKey(KeyOf);

enum KeyOf {
    SharedFile {
        identity: Identity,
        path: PathBuf,
    },
    StoreVault {
        session: Arc<dyn VaultSession>,
        vault: Arc<[u8]>,
        path: PathBuf,
    },
}

impl CacheKey {
    /// The recipient a new cache under this key is encrypted to — the only kind of recipient a
    /// cache may be sealed to (`tests/guards.rs`).
    pub fn recipient(&self) -> cli_core::Result<Recipient> {
        match &self.0 {
            KeyOf::SharedFile { identity, .. } => Ok(identity.to_public()),
            KeyOf::StoreVault { session, vault, .. } => session.recipient(vault),
        }
    }

    /// Which key it is, for display (doctor, IPC).
    pub fn kind(&self) -> CacheKeyView {
        match &self.0 {
            KeyOf::SharedFile { .. } => CacheKeyView::SharedFile,
            KeyOf::StoreVault { .. } => CacheKeyView::StoreVault,
        }
    }
}

impl fmt::Debug for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            KeyOf::SharedFile { path, .. } => f
                .debug_struct("CacheKey")
                .field("path", path)
                .field("identity", &"<redacted>")
                .finish(),
            KeyOf::StoreVault { path, vault, .. } => f
                .debug_struct("CacheKey")
                .field("path", path)
                .field("vault_bytes", &vault.len())
                .field("identity", &"<in the vault>")
                .finish(),
        }
    }
}

/// Which key a cache opens with, as IPC shows it: the twin of cli-core's `CacheKeyKind`
/// (D.4a §3.4, §3.12), one to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CacheKeyView {
    SharedFile,
    StoreVault,
}

impl From<CacheKeyKind> for CacheKeyView {
    fn from(kind: CacheKeyKind) -> Self {
        match kind {
            CacheKeyKind::SharedFile => CacheKeyView::SharedFile,
            CacheKeyKind::StoreVault => CacheKeyView::StoreVault,
        }
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
/// prepared for — on a locked store the store id and the key — and a write re-reads
/// `store.yaml` under the lock and refuses another (`store_changed`, D.4a T41, critic G3).
#[derive(Debug)]
pub struct WriteKey(WriteFor);

#[derive(Debug)]
enum WriteFor {
    Files,
    Locked { id: StoreId, key: KeySession },
}

impl WriteFor {
    fn kind(&self) -> SecretsModeKind {
        match self {
            WriteFor::Files => SecretsModeKind::Files,
            WriteFor::Locked { .. } => SecretsModeKind::Locked,
        }
    }
}

/// `store.yaml` as one call read it.
enum StoreState {
    Files,
    Locked { file: StoreFile, id: StoreId },
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

/// The vault bytes one call read, and the session that opens items of exactly those bytes (R3).
type Opened = (Arc<[u8]>, Arc<dyn VaultSession>);

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
    access: Arc<dyn VaultAccess>,
}

impl SecretStore {
    pub(crate) fn new(
        store: TargetStorePaths,
        age_key_path: PathBuf,
        access: Arc<dyn VaultAccess>,
    ) -> Self {
        SecretStore {
            store,
            age_key_path,
            access,
        }
    }

    /// `store.yaml`, read now.
    fn state(&self) -> cli_core::Result<StoreState> {
        let Some(file) = read_store_file(self.store.root())? else {
            return Ok(StoreState::Files);
        };
        Ok(match file.secrets {
            SecretsMode::Files => StoreState::Files,
            SecretsMode::Locking => StoreState::Incomplete(LockPhase::Locking),
            SecretsMode::Unlocking => StoreState::Incomplete(LockPhase::Unlocking),
            SecretsMode::Locked => match file.id.clone() {
                Some(id) => StoreState::Locked { file, id },
                // `read_store_file` refuses this; the same refusal, should it ever not.
                None => {
                    return Err(CliError::StoreFileUnreadable {
                        path: self.store.root().join(STORE_FILE),
                        detail: "it says the secrets are locked, and names no id".into(),
                    })
                }
            },
        })
    }

    /// What a client is asked to open.
    fn request<'a>(&'a self, file: &'a StoreFile, id: &'a StoreId) -> VaultRequest<'a> {
        VaultRequest {
            store_root: self.store.root(),
            store_id: id,
            slots: &file.slots,
        }
    }

    /// The vault as it is now, and a session to open it with: the vault is read and checked
    /// first, so a broken or foreign one is said so before anyone is asked for a key.
    fn session(&self, file: &StoreFile, id: &StoreId) -> cli_core::Result<Opened> {
        let vault = locked::read_vault(&self.store, id)?;
        let session = self.access.session(&self.request(file, id))?;
        Ok((vault, session))
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

    /// What is stored for `target`, without the token and without the vault: in files, the
    /// record and its length; on a locked store, what its marker says (T7: no vault is opened).
    /// A record that cannot be read is that file's error (`InvalidTargetConfig`, which never
    /// quotes it, or `TargetFileIo`).
    pub fn token_presence(&self, target: &str) -> cli_core::Result<StoredToken> {
        match self.state()? {
            StoreState::Files => files::token_presence(&self.store, target),
            StoreState::Locked { id, .. } => Ok(match locked::placed(&self.store, &id, target)? {
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
    /// store, through a session (which may prompt, in the CLI); never a clear token (T42) nor
    /// another store's; a marker with no vault entry is `token_missing_from_vault` (T43).
    pub fn stored_token(&self, target: &str) -> cli_core::Result<Option<SecretString>> {
        match self.state()? {
            StoreState::Files => files::stored_token(&self.store, target),
            StoreState::Locked { file, id } => match locked::placed(&self.store, &id, target)? {
                locked::Placed::Absent => Ok(None),
                locked::Placed::Own => {
                    let (vault, session) = self.session(&file, &id)?;
                    match session.hetzner_token(&vault, target)? {
                        Some(token) => Ok(Some(token)),
                        None => Err(CliError::TokenMissingFromVault {
                            target: target.to_string(),
                            store: self.store.root().to_path_buf(),
                        }),
                    }
                }
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

    /// What a token write needs. Call it BEFORE taking the store lock: on a locked store it
    /// asks the client for the key (which may prompt), after checking that the vault reads.
    pub fn prepare_write(&self) -> cli_core::Result<WriteKey> {
        match self.state()? {
            StoreState::Files => Ok(WriteKey(WriteFor::Files)),
            StoreState::Locked { file, id } => {
                locked::read_vault(&self.store, &id)?;
                let key = self.access.write_key(&self.request(&file, &id))?;
                Ok(WriteKey(WriteFor::Locked { id, key }))
            }
            StoreState::Incomplete(phase) => Err(self.incomplete(phase)),
        }
    }

    /// Store `token` for `target` under the store lock `held`. In files: `credentials.yaml`,
    /// mode 0600, written atomically, the bytes `cli_core::save_target` wrote; on a locked
    /// store: the vault (`held` must hold the lock), then the marker. `config.yaml` is the
    /// caller's to write.
    pub fn put_token(
        &self,
        key: &WriteKey,
        held: &StoreLock,
        target: &str,
        token: &SecretString,
    ) -> cli_core::Result<()> {
        self.recheck(key, held)?;
        match &key.0 {
            WriteFor::Files => files::put_token(&self.store, target, token),
            WriteFor::Locked { id, key } => {
                let writer = self.access.writer();
                locked::put_token(&self.store, id, key, held, &writer, target, token)
            }
        }
    }

    /// Remove `target`'s token, after the caller removed the target's directory (T41). In
    /// files it is the target's credentials file, which that removal deleted: `false`, nothing
    /// done. On a locked store every vault entry of the target goes; `false` when there was
    /// none.
    pub fn remove_token(
        &self,
        key: &WriteKey,
        held: &StoreLock,
        target: &str,
    ) -> cli_core::Result<bool> {
        self.recheck(key, held)?;
        match &key.0 {
            WriteFor::Files => Ok(false),
            WriteFor::Locked { id, key } => {
                let writer = self.access.writer();
                locked::remove_token(&self.store, id, key, held, &writer, target)
            }
        }
    }

    /// A rename's first step (T41), before the caller moves the directories
    /// (`cli_core::rename_target`), which it does only after refusing an existing `to` under
    /// the same held lock: on a locked store `from`'s token also under `to`, `from`'s kept, on
    /// disk before the move. In files the token moves with the directory: nothing to do.
    pub fn copy_token(
        &self,
        key: &WriteKey,
        held: &StoreLock,
        from: &str,
        to: &str,
    ) -> cli_core::Result<()> {
        self.recheck(key, held)?;
        match &key.0 {
            WriteFor::Files => Ok(()),
            WriteFor::Locked { id, key } => {
                let writer = self.access.writer();
                locked::copy_token(&self.store, id, key, held, &writer, from, to)
            }
        }
    }

    /// A rename's last step (T41), after the caller moved the directories: on a locked store
    /// `from`'s entries replace `to`'s, so a crash between the steps leaves both names,
    /// harmless; a vault an older AppRafter left with `to`'s entry only (it renamed the
    /// directories alone, T43) is left as it is. In files: nothing to do.
    pub fn rename_token(
        &self,
        key: &WriteKey,
        held: &StoreLock,
        from: &str,
        to: &str,
    ) -> cli_core::Result<()> {
        self.recheck(key, held)?;
        match &key.0 {
            WriteFor::Files => Ok(()),
            WriteFor::Locked { id, key } => {
                let writer = self.access.writer();
                locked::rename_token(&self.store, id, key, held, &writer, from, to)
            }
        }
    }

    /// Before any write: a vault write needs the held write lock (T28), and every write the lock
    /// state its key was prepared for, read again now, under that lock (T41, critic G3).
    fn recheck(&self, key: &WriteKey, held: &StoreLock) -> cli_core::Result<()> {
        if matches!(key.0, WriteFor::Locked { .. }) && !held.is_held() {
            return Err(CliError::VaultNeedsWriteLock {
                store: self.store.root().to_path_buf(),
            });
        }
        let now = self.state()?;
        let same = match (&key.0, &now) {
            (WriteFor::Files, StoreState::Files) => true,
            (WriteFor::Locked { id, .. }, StoreState::Locked { id: now_id, .. }) => id == now_id,
            _ => false,
        };
        if same {
            return Ok(());
        }
        Err(CliError::StoreChanged {
            store: self.store.root().to_path_buf(),
            prepared: key.0.kind(),
            now: now.kind(),
        })
    }

    /// The key `target`'s cached secrets are read with. Never creates one (GOTCHA-120). In
    /// files: the shared key, and no key is `CliError::AgeKeyMissing` naming its path and the
    /// target. On a locked store: its own identity, through a session — never the shared key,
    /// whatever `APPRAFTER_AGE_KEY` says (T35; d4a-d refuses the variable there).
    pub fn cache_key(&self, target: &str) -> cli_core::Result<CacheKey> {
        match self.state()? {
            StoreState::Files => files::cache_key(&self.age_key_path, target),
            StoreState::Locked { file, id } => {
                let (vault, session) = self.session(&file, &id)?;
                Ok(CacheKey(KeyOf::StoreVault {
                    session,
                    vault,
                    path: locked::vault_path(self.store.root()),
                }))
            }
            StoreState::Incomplete(phase) => Err(self.incomplete(phase)),
        }
    }

    /// The key a NEW cached secret of `target` is written under, settled before the secret is
    /// read so that a refusal costs no read. In files: the key there is; else, with nothing
    /// cached anywhere, a first use ([`NewCacheKey::FirstUse`]); else the key everything was
    /// cached under is lost, which is `CliError::AgeKeyMissing` — only `apprafter kubeconfig
    /// --refresh` replaces a lost key, after it asks (WI-457). On a locked store: always the
    /// store's own key (T26).
    pub fn cache_key_for_new_secret(&self, target: &str) -> cli_core::Result<NewCacheKey> {
        match self.state()? {
            StoreState::Files => {
                files::cache_key_for_new_secret(&self.store, &self.age_key_path, target)
            }
            StoreState::Locked { .. } => self.cache_key(target).map(NewCacheKey::Ready),
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
