// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The one door to a target store's secrets (ADR 0068 §5; D.4a overview §3.4).
//!
//! Every Hetzner token and every cached secret a core operation or a CLI arm reaches goes
//! through a [`SecretStore`], which [`Context::secrets`](crate::Context::secrets) builds per
//! call and which touches nothing until one of its methods runs.
//!
//! D.4a b ships the files backend only ([`SecretsMode::Files`]): each target's
//! `targets/<t>/credentials.yaml`, and the one shared age key (`APPRAFTER_AGE_KEY`, else
//! `<home>/.config/apprafter/age.key`) the caches in `state.json` are encrypted to — what every
//! store is today. d4a-c adds `store.yaml`, the vault and the modes it reads from them; the
//! methods keep their shape.
//!
//! Rules: no core code outside this module reads or writes a credentials file, reads the age
//! key, or opens or seals a cache, and nothing here seals a cache to a recipient that did not
//! come from a [`CacheKey`] (`tests/guards.rs`, D.4a T16); a read never creates the age key
//! (GOTCHA-120); a failure to decrypt is an error naming the key, never "none cached".

use std::fmt;
use std::path::PathBuf;

use cli_core::secrets::{CachedSecret, Identity, Recipient};
use cli_core::{StoreLock, TargetStorePaths};
use cli_state::HetznerCloudState;
use zeroize::Zeroizing;

use crate::context::SecretString;

mod cache;
mod files;
#[cfg(test)]
mod tests;

pub use files::{Held, Holding};

/// Where a store's secrets are. D.4a b knows one place; d4a-c adds `Locking`, `Locked` and
/// `Unlocking` with `store.yaml`, and every `match` on this then decides each of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretsMode {
    /// Each target's `credentials.yaml` and the shared age key.
    Files,
}

/// What a target's token record says, read without handing out the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredToken {
    /// No token: no credentials file, or one without `hetzner_token`.
    None,
    /// A token in the target's `credentials.yaml`, `chars` bytes long (tokens are ASCII).
    InFile { chars: u32 },
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
/// is taken: a slow step (d4a-c's prompt) never runs under the lock. It is for the mode the
/// store was in when it was prepared.
#[derive(Debug)]
pub struct WriteKey {
    mode: SecretsMode,
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

    /// Where this store's secrets are. D.4a b: always [`SecretsMode::Files`], read from
    /// nothing; d4a-c reads `<root>/store.yaml` here, on every call.
    pub fn mode(&self) -> cli_core::Result<SecretsMode> {
        Ok(SecretsMode::Files)
    }

    /// The file that records `target`'s token, for display and file checks (`target show`,
    /// doctor's credentials row). Nothing is read through this path.
    pub fn credentials_file(&self, target: &str) -> PathBuf {
        self.store.target_credentials_file(target)
    }

    /// What is stored for `target`, without the token. A record that cannot be read is that
    /// file's error (`InvalidTargetConfig`, which never quotes it, or `TargetFileIo`).
    pub fn token_presence(&self, target: &str) -> cli_core::Result<StoredToken> {
        match self.mode()? {
            SecretsMode::Files => files::token_presence(&self.store, target),
        }
    }

    /// The token stored for `target` — never an override: whether `HCLOUD_TOKEN` outranks it
    /// is the caller's rule (`target::hetzner_token`, R4). `None`: no token stored.
    pub fn stored_token(&self, target: &str) -> cli_core::Result<Option<SecretString>> {
        match self.mode()? {
            SecretsMode::Files => files::stored_token(&self.store, target),
        }
    }

    /// What a token write needs. Call it BEFORE taking the store lock. Files: nothing to ask.
    pub fn prepare_write(&self) -> cli_core::Result<WriteKey> {
        Ok(WriteKey { mode: self.mode()? })
    }

    /// Store `token` for `target` under the store lock, which `_held` is the caller's guard of
    /// (on a filesystem without locks it holds nothing, and files are written as every edit
    /// is there). Files: `credentials.yaml`, mode 0600, written atomically, the bytes
    /// `cli_core::save_target` wrote; `config.yaml` is the caller's to write.
    pub fn put_token(
        &self,
        key: &WriteKey,
        _held: &StoreLock,
        target: &str,
        token: &SecretString,
    ) -> cli_core::Result<()> {
        match key.mode {
            SecretsMode::Files => files::put_token(&self.store, target, token),
        }
    }

    /// The key `target`'s cached secrets are read with. Never creates one (GOTCHA-120): no key
    /// is `CliError::AgeKeyMissing` naming its path and the target.
    pub fn cache_key(&self, target: &str) -> cli_core::Result<CacheKey> {
        files::cache_key(&self.age_key_path, target)
    }

    /// The key a NEW cached secret of `target` is written under, settled before the secret is
    /// read so that a refusal costs no read: the key there is; else, with nothing cached
    /// anywhere, a first use ([`NewCacheKey::FirstUse`]); else the key everything was cached
    /// under is lost, which is `CliError::AgeKeyMissing` — only `apprafter kubeconfig
    /// --refresh` replaces a lost key, after it asks (WI-457).
    pub fn cache_key_for_new_secret(&self, target: &str) -> cli_core::Result<NewCacheKey> {
        files::cache_key_for_new_secret(&self.store, &self.age_key_path, target)
    }

    /// `target`'s cached kubeconfig from its state's `hetzner_cloud`; `Ok(None)` when none is
    /// cached. The encrypted slot wins over the legacy plaintext one, which needs no key.
    pub fn cached_kubeconfig(
        &self,
        hetzner: &HetznerCloudState,
        target: &str,
    ) -> cli_core::Result<Option<Zeroizing<String>>> {
        cache::cached_kubeconfig(self, hetzner, target)
    }

    /// `armored`, `target`'s cached `secret`, opened with `key`. A key that does not open it
    /// is `CliError::CacheUndecryptable` naming the secret, the target and the key's file.
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
