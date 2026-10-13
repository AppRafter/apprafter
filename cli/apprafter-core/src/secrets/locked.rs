// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The vault backend (D.4a §3.4): a locked store's `credentials.yaml` markers, read without
//! opening anything. Only [`super::SecretStore`] calls in.

use cli_core::target::{parse_target_credentials, read_target_credentials_bytes, TargetStorePaths};
use cli_core::{CliError, Result};
use zeroize::Zeroizing;

use super::files::under_temp;
use super::marker::{classify, Marker};
use super::store_file::StoreId;

/// Where a locked store's `credentials.yaml` puts a target's token.
pub(super) enum Placed {
    /// No token recorded.
    Absent,
    /// A marker for this store's own vault.
    Own,
    /// A token in clear: never used (T42).
    Clear,
    /// A marker for another store's vault (T34).
    Other(StoreId),
}

/// What `target`'s `credentials.yaml` says on the locked store `id`, with no token handed out
/// and no vault opened. A file that classifies as nothing is that file's error, worded as the
/// files backend words it, never quoted.
pub(super) fn placed(store: &TargetStorePaths, id: &StoreId, target: &str) -> Result<Placed> {
    let path = store.target_credentials_file(target);
    under_temp(&path);
    let Some(bytes) = read_target_credentials_bytes(store, target)?.map(Zeroizing::new) else {
        return Ok(Placed::Absent);
    };
    Ok(match classify(&bytes) {
        Marker::Absent => Placed::Absent,
        Marker::Vault { id: named } if named == *id => Placed::Own,
        Marker::Vault { id: named } => Placed::Other(named),
        Marker::ClearToken => Placed::Clear,
        Marker::Unreadable => {
            parse_target_credentials(store, target, &bytes)?;
            return Err(CliError::InvalidTargetConfig {
                path,
                message: "not a valid target credentials map".to_string(),
                target: Some(target.to_string()),
            });
        }
    })
}
