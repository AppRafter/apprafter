// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The secrets a target's state caches (`state.json`'s `hetzner_cloud`), opened with a
//! [`CacheKey`] and decrypted nowhere else in the core (D.4a T16). Moved from the CLI's
//! `commands/age_cache.rs` (D.4a b).

use cli_core::secrets::{decrypt_with_identity_reason, CacheKeyKind, CachedSecret};
use cli_core::{CliError, Result};
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
