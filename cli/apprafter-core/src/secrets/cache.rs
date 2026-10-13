// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The secrets a target's state caches (`state.json`'s `hetzner_cloud`), opened with a
//! [`CacheKey`] and decrypted nowhere else in the core (D.4a T16). Moved from the CLI's
//! `commands/age_cache.rs` (D.4a b).

use cli_core::secrets::{decrypt_with_identity_reason, CacheKeyKind, CachedSecret};
use cli_core::{CliError, Result};
use cli_state::HetznerCloudState;
use zeroize::Zeroizing;

use super::{CacheKey, SecretStore};

/// `target`'s cached kubeconfig; `Ok(None)` when neither slot is set.
///
/// PRECEDENCE IS LOAD-BEARING: the encrypted slot wins over the plaintext one. The plaintext
/// slot is a legacy fallback that older states may still carry beside a freshly written
/// encrypted one; preferring it would silently hand back a STALE kubeconfig for a cluster
/// that has since been re-provisioned.
pub(super) fn cached_kubeconfig(
    store: &SecretStore,
    h: &HetznerCloudState,
    target: &str,
) -> Result<Option<Zeroizing<String>>> {
    match (h.kubeconfig_age.as_deref(), h.kubeconfig_yaml.as_deref()) {
        (Some(armored), _) => {
            let key = store.cache_key(target)?;
            open(&key, CachedSecret::Kubeconfig, target, armored).map(Some)
        }
        (None, Some(plain)) => Ok(Some(Zeroizing::new(plain.to_string()))),
        (None, None) => Ok(None),
    }
}

/// `armored`, `target`'s cached `secret`, opened with `key`: the shared age key here (the
/// store's own key is d4a-c2's vault backend). A failure says which key and why (§3.12).
pub(super) fn open(
    key: &CacheKey,
    secret: CachedSecret,
    target: &str,
    armored: &str,
) -> Result<Zeroizing<String>> {
    decrypt_with_identity_reason(armored, &key.identity)
        .map(Zeroizing::new)
        .map_err(|u| CliError::CacheUndecryptable {
            secret,
            target: target.to_string(),
            path: key.path.display().to_string(),
            detail: u.detail,
            key: CacheKeyKind::SharedFile,
            reason: u.why,
        })
}
