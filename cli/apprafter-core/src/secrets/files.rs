// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The files backend (D.4a b): each target's `targets/<t>/credentials.yaml`, and the one shared
//! age key (`APPRAFTER_AGE_KEY`, else `<home>/.config/apprafter/age.key`) the caches in
//! `state.json` are encrypted to. Only [`super::SecretStore`] calls in.

use std::path::Path;

use cli_core::secrets::{load_identity, load_or_create_identity_reporting, CachedSecret};
use cli_core::target::{
    list_target_names, parse_target_credentials, read_target_credentials_bytes,
    save_target_credentials, TargetCredentials, TargetStorePaths,
};
use cli_core::{CliError, Result};
use cli_state::{SealedTo, State, StatePaths};
use zeroize::Zeroizing;

use super::marker::{self, Marker};
use super::{CacheKey, KeyOf, NewCacheKey, StoredToken};
use crate::context::SecretString;

/// D.4a T6: in the core's own tests a door opens only what lies under the temp dir — the
/// deepest part of `path` that exists, canonicalised — so no core test reaches the account's
/// own store or age key, whatever its context. Compiled out of every other build (and skipped,
/// as a `cfg(test)` item, by the purity guard that forbids `std::env::temp_dir`).
#[cfg(test)]
pub(super) fn under_temp(path: &Path) {
    let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let existing = path
        .ancestors()
        .find(|a| a.exists())
        .and_then(|a| std::fs::canonicalize(a).ok());
    assert!(
        existing.is_some_and(|p| p.starts_with(&tmp)),
        "a core test opened {path:?}, outside the temp dir (D.4a T6)"
    );
}

#[cfg(not(test))]
pub(super) fn under_temp(_: &Path) {}

/// What `target`'s credentials file holds, without the token: its length only. A marker here,
/// on a store in files, names another store's vault (D.4a T34).
pub(super) fn token_presence(store: &TargetStorePaths, target: &str) -> Result<StoredToken> {
    under_temp(&store.target_credentials_file(target));
    let Some(bytes) = read_target_credentials_bytes(store, target)?.map(Zeroizing::new) else {
        return Ok(StoredToken::None);
    };
    if let Marker::Vault { id } = marker::classify(&bytes) {
        return Ok(StoredToken::InAnotherVault { id });
    }
    Ok(
        match parse_target_credentials(store, target, &bytes)?.hetzner_token {
            Some(token) => StoredToken::InFile {
                chars: token.len() as u32,
            },
            None => StoredToken::None,
        },
    )
}

pub(super) fn stored_token(store: &TargetStorePaths, target: &str) -> Result<Option<SecretString>> {
    under_temp(&store.target_credentials_file(target));
    let Some(bytes) = read_target_credentials_bytes(store, target)?.map(Zeroizing::new) else {
        return Ok(None);
    };
    if let Marker::Vault { id } = marker::classify(&bytes) {
        return Err(CliError::TokenInAnotherVault {
            target: target.to_string(),
            id: id.to_string(),
        });
    }
    Ok(parse_target_credentials(store, target, &bytes)?
        .hetzner_token
        .map(SecretString::new))
}

pub(super) fn put_token(
    store: &TargetStorePaths,
    target: &str,
    token: &SecretString,
) -> Result<()> {
    under_temp(&store.target_credentials_file(target));
    save_target_credentials(
        store,
        target,
        &TargetCredentials {
            hetzner_token: Some(token.expose().to_string()),
        },
    )
}

/// The key at `age_key_path`, which a read needs: never created.
pub(super) fn cache_key(age_key_path: &Path, target: &str) -> Result<CacheKey> {
    under_temp(age_key_path);
    let identity = load_identity(age_key_path)?.ok_or_else(|| missing(target, age_key_path))?;
    Ok(CacheKey(KeyOf::SharedFile {
        identity,
        path: age_key_path.to_path_buf(),
    }))
}

/// `age_cache::key_for_new_secret`'s rule (WI-457), moved: the key there is, a first use, or
/// the lost key's refusal.
pub(super) fn cache_key_for_new_secret(
    store: &TargetStorePaths,
    age_key_path: &Path,
    target: &str,
) -> Result<NewCacheKey> {
    under_temp(age_key_path);
    if let Some(identity) = load_identity(age_key_path)? {
        return Ok(NewCacheKey::Ready(CacheKey(KeyOf::SharedFile {
            identity,
            path: age_key_path.to_path_buf(),
        })));
    }
    let held = held(store)?;
    if !held.is_empty() {
        return Err(CliError::AgeKeyMissing {
            path: age_key_path.display().to_string(),
            target: target.to_string(),
            stopped: None,
            lost_vault: lost_vaults(&held).map(|ids| ids.join(", ")),
        });
    }
    Ok(NewCacheKey::FirstUse {
        path: age_key_path.to_path_buf(),
    })
}

/// The shared key at `age_key_path`, created when none is there — a first use, or a lost
/// key's replacement the caller agreed to (WI-457): whether this call created it. A key that
/// appeared meanwhile is used, never replaced (a no-replace move, WI-475). Files only: a locked
/// store never gets one (c2's hand-off).
pub(super) fn create_shared_key(age_key_path: &Path) -> Result<bool> {
    under_temp(age_key_path);
    load_or_create_identity_reporting(age_key_path).map(|(_, created)| created)
}

/// The missing-key error for `target`'s cache, naming no vault: [`name_lost_vault`] does.
fn missing(target: &str, age_key_path: &Path) -> CliError {
    CliError::AgeKeyMissing {
        path: age_key_path.display().to_string(),
        target: target.to_string(),
        stopped: None,
        lost_vault: None,
    }
}

/// d4a-h F13's residue: an `age_key_missing` names the target store vaults what the store
/// caches records it was encrypted to, when each cached secret records one ([`lost_vaults`], the
/// rule `kubeconfig --refresh`'s explanation follows): no age key opens them, so its help names
/// the vault, never the key file. Any other error, or a store whose caches cannot all be read,
/// is left as it is: the general words never claim to know.
pub(super) fn name_lost_vault(store: &TargetStorePaths, e: CliError) -> CliError {
    match e {
        CliError::AgeKeyMissing {
            path,
            target,
            stopped,
            lost_vault: None,
        } => CliError::AgeKeyMissing {
            path,
            target,
            stopped,
            lost_vault: held(store)
                .ok()
                .and_then(|held| lost_vaults(&held).map(|ids| ids.join(", "))),
        },
        other => other,
    }
}

/// F13: the ids of the target store vaults whose keys sealed what is `held`, when the state
/// records one for every cached secret — `lock on` re-sealed them, and that vault is no longer
/// here (the shared key is looked for only on a store in files). `None` when nothing is held,
/// or any held item records nothing or its state cannot be read: then the general words, which
/// never claim to know.
pub fn lost_vaults(held: &[Held]) -> Option<Vec<&str>> {
    let mut ids: Vec<&str> = Vec::new();
    for h in held {
        match (&h.what, &h.sealed_to) {
            (Holding::Secret(_), Some(SealedTo::StoreVault { store_id, .. })) => {
                if !ids.contains(&store_id.as_str()) {
                    ids.push(store_id);
                }
            }
            _ => return None,
        }
    }
    (!ids.is_empty()).then_some(ids)
}

/// One thing a target's state holds under the age key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub target: String,
    pub what: Holding,
    /// What the state records sealed it, when not the shared age key (F13); `None` for an
    /// unreadable state, and for a cache that records nothing.
    pub sealed_to: Option<SealedTo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holding {
    Secret(CachedSecret),
    /// A state that cannot be read, which may hold either: counted as if it did, so a key is
    /// never created on the guess that it holds nothing.
    Unreadable(String),
}

/// Everything the store's targets hold encrypted, by target name, the kubeconfig before the
/// password. Read-only.
pub(super) fn held(store: &TargetStorePaths) -> Result<Vec<Held>> {
    under_temp(store.root());
    let mut out = Vec::new();
    for target in list_target_names(store)? {
        let paths = StatePaths::for_active_target(store, &target);
        match State::load_or_default(&paths) {
            Ok(state) => {
                let Some(h) = state.hetzner_cloud else {
                    continue;
                };
                for (slot, secret) in [
                    (&h.kubeconfig_age, CachedSecret::Kubeconfig),
                    (&h.argocd_admin_password_age, CachedSecret::ArgocdPassword),
                ] {
                    if slot.is_some() {
                        out.push(Held {
                            target: target.clone(),
                            what: Holding::Secret(secret),
                            sealed_to: h.sealed_to(secret).cloned(),
                        });
                    }
                }
            }
            Err(e) => out.push(Held {
                target,
                what: Holding::Unreadable(e.to_string()),
                sealed_to: None,
            }),
        }
    }
    Ok(out)
}
