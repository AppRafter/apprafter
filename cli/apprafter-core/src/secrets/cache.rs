// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The secrets a target's state caches (`state.json`'s `hetzner_cloud`), opened with a
//! [`CacheKey`] and decrypted nowhere else in the core (D.4a T16). Moved from the CLI's
//! `commands/age_cache.rs` (D.4a b).

use cli_core::secrets::UndecryptableWhy;
use cli_core::secrets::{decrypt_with_identity_reason, CacheKeyKind, CachedSecret};
use cli_core::{CliError, Result};
use cli_state::SealedTo;
use zeroize::Zeroizing;

use super::{CacheKey, KeyOf};

/// `armored`, `target`'s cached `secret`, opened with `key`. A failure says which key and why
/// (§3.12).
pub(super) fn open(
    key: &CacheKey,
    secret: CachedSecret,
    target: &str,
    armored: &str,
) -> Result<Zeroizing<String>> {
    match &key.0 {
        KeyOf::SharedFile { identity, path } => decrypt_with_identity_reason(armored, identity)
            .map(Zeroizing::new)
            .map_err(|u| CliError::CacheUndecryptable {
                secret,
                target: target.to_string(),
                path: path.display().to_string(),
                detail: u.detail,
                key: CacheKeyKind::SharedFile,
                reason: u.why,
            }),
        KeyOf::StoreVault { session, vault, .. } => {
            session.open_cache(vault, secret, target, armored)
        }
    }
}

/// `e`, a refusal of a cache `key` did not open, in the words of a store key a key change
/// retired when that is what happened (D.4b B86): the state records this store's vault under
/// a vault id other than the one `key`'s identity is in — a vault id names one store key, as
/// both change together. Anything else is returned as it is: a damaged cache, a record
/// without a vault id (AppRafter 0.2.80's), another store's vault, the shared age key.
pub(super) fn retired(e: CliError, record: Option<&SealedTo>, key: &CacheKey) -> CliError {
    match e {
        CliError::CacheUndecryptable {
            secret,
            target,
            path,
            detail,
            key: CacheKeyKind::StoreVault,
            reason: UndecryptableWhy::NotForThisKey,
        } if was_retired(record, key) => CliError::CacheUndecryptable {
            secret,
            target,
            path,
            detail,
            key: CacheKeyKind::StoreVault,
            reason: UndecryptableWhy::RetiredKey,
        },
        other => other,
    }
}

/// Whether `record` names this store's vault under another vault id than `key`'s.
fn was_retired(record: Option<&SealedTo>, key: &CacheKey) -> bool {
    let Some(SealedTo::StoreVault {
        store_id,
        vault_id: Some(sealed),
    }) = record
    else {
        return false;
    };
    let Ok(Some(SealedTo::StoreVault {
        store_id: here,
        vault_id: Some(now),
    })) = key.record()
    else {
        return false;
    };
    *store_id == here && *sealed != now
}
