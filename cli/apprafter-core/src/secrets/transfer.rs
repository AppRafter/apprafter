// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What `lock on` and `lock off` move between a target store's files and its vault (D.4a §3.9,
//! T36, T37): a target's credentials file, the shared age keys, the caches and their seals —
//! the doors T16 keeps inside `secrets/`. The flows that use them are `crate::lock::{on, off}`;
//! nothing here takes the store lock or writes the vault.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli_core::secrets::{
    decrypt_with_identity_reason, encrypt_for_recipient, load_identity,
    load_or_create_identity_reporting, CachedSecret, Identity, UndecryptableWhy,
};
use cli_core::target::{parse_target_credentials, read_target_credentials_bytes, TargetStorePaths};
use cli_core::Result;
use zeroize::Zeroizing;

use super::access::KeySession;
use super::files::under_temp;
use super::locked::vault_path;
use super::marker::{classify, Marker};
use super::store_file::StoreId;
use super::{CacheKey, KeyOf};
use crate::context::SecretString;

/// What a target's `credentials.yaml` holds, as a lock flow reads it.
pub(crate) enum Credentials {
    /// No file, or no token in it.
    Absent,
    /// A token in clear.
    Clear(SecretString),
    /// A marker for the vault of store `id`.
    Marker(StoreId),
}

/// `target`'s credentials file. One that is neither a token nor a marker is that file's error:
/// it may hold the only copy of a token, and the flow stops at it (T36).
pub(crate) fn credentials(store: &TargetStorePaths, target: &str) -> Result<Credentials> {
    under_temp(&store.target_credentials_file(target));
    let Some(bytes) = read_target_credentials_bytes(store, target)?.map(Zeroizing::new) else {
        return Ok(Credentials::Absent);
    };
    Ok(match classify(&bytes) {
        Marker::Absent => Credentials::Absent,
        Marker::Vault { id } => Credentials::Marker(id),
        Marker::ClearToken | Marker::Unreadable => {
            match parse_target_credentials(store, target, &bytes)?.hetzner_token {
                Some(token) => Credentials::Clear(SecretString::new(token)),
                None => Credentials::Absent,
            }
        }
    })
}

/// The shared age identities at `paths`, read and never created (GOTCHA-120); a path with no
/// key is skipped.
pub(crate) fn shared_identities(paths: &[PathBuf]) -> Result<Vec<Identity>> {
    let mut out = Vec::new();
    for path in paths {
        under_temp(path);
        if let Some(identity) = load_identity(path)? {
            out.push(identity);
        }
    }
    Ok(out)
}

/// The shared age identities at `paths`, read once, when first asked for (D.4a f Task 3): a
/// run that meets no cache sealed to them never reads them.
pub(crate) struct SharedKeys<'a> {
    paths: &'a [PathBuf],
    read: std::cell::OnceCell<Vec<Identity>>,
}

impl<'a> SharedKeys<'a> {
    pub(crate) fn at(paths: &'a [PathBuf]) -> Self {
        SharedKeys {
            paths,
            read: std::cell::OnceCell::new(),
        }
    }

    /// The identities, read now if they were not yet ([`shared_identities`]).
    pub(crate) fn get(&self) -> Result<&[Identity]> {
        if let Some(keys) = self.read.get() {
            return Ok(keys.as_slice());
        }
        let keys = shared_identities(self.paths)?;
        Ok(self.read.get_or_init(|| keys).as_slice())
    }
}

/// `armored` opened with the first of `keys` that opens it.
pub(crate) fn open_with_any(armored: &str, keys: &[Identity]) -> Option<Zeroizing<String>> {
    keys.iter().find_map(|id| {
        decrypt_with_identity_reason(armored, id)
            .ok()
            .map(Zeroizing::new)
    })
}

/// The key of the vault `vault` of the store at `root`, opened with `session`'s data key.
pub(crate) fn store_key(root: &Path, session: KeySession, vault: Arc<[u8]>) -> CacheKey {
    CacheKey(KeyOf::StoreVault {
        session: Arc::new(session),
        vault,
        path: vault_path(root),
    })
}

/// `plaintext` sealed to `key` — T16's one form of a seal.
pub(crate) fn seal(key: &CacheKey, plaintext: &str) -> Result<String> {
    encrypt_for_recipient(plaintext, &key.recipient()?)
}

/// `armored` opened with `key`, or `None` when the key does not open it.
pub(crate) fn opens(
    key: &CacheKey,
    secret: CachedSecret,
    armored: &str,
) -> Option<Zeroizing<String>> {
    super::cache::open(key, secret, "", armored).ok()
}

/// The shared age key at `path` — `lock off`'s destination (O13) — loaded, or created when
/// none is there (never replaced); whether this call created it.
pub(crate) fn shared_identity_at(path: &Path) -> Result<(Identity, bool)> {
    under_temp(path);
    load_or_create_identity_reporting(path)
}

/// The shared age key `identity`, kept at `path`, as a cache key.
pub(crate) fn shared_key(identity: Identity, path: PathBuf) -> CacheKey {
    CacheKey(KeyOf::SharedFile { identity, path })
}

/// The refusal of a clear token in a locked store's credentials file (T42), naming that file.
pub(crate) fn outside_vault(store: &TargetStorePaths, target: &str) -> cli_core::CliError {
    cli_core::CliError::TokenOutsideVault {
        target: target.to_string(),
        path: store.target_credentials_file(target),
    }
}

/// `token` in clear in `target`'s credentials file (mode 0600, atomically), as the files
/// backend writes it.
pub(crate) fn write_clear_token(
    store: &TargetStorePaths,
    target: &str,
    token: &SecretString,
) -> Result<()> {
    super::files::put_token(store, target, token)
}

/// What a key makes of a cached secret (rule 3: a failed decryption is never "absent").
pub(crate) enum Opened {
    /// The key opens it.
    Plain(Zeroizing<String>),
    /// It is sealed to another key.
    NotForThisKey,
    /// It names this key and does not decrypt: damaged.
    Damaged,
}

/// `armored` as `key` opens it: the plaintext, or why not. Any other failure (a vault key
/// that cannot be used) is the error.
pub(crate) fn opened_by(key: &CacheKey, secret: CachedSecret, armored: &str) -> Result<Opened> {
    match super::cache::open(key, secret, "", armored) {
        Ok(plain) => Ok(Opened::Plain(plain)),
        Err(cli_core::CliError::CacheUndecryptable {
            reason: UndecryptableWhy::Damaged,
            ..
        }) => Ok(Opened::Damaged),
        Err(cli_core::CliError::CacheUndecryptable { .. }) => Ok(Opened::NotForThisKey),
        Err(e) => Err(e),
    }
}
