// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The files backend (D.4a b): each target's `targets/<t>/credentials.yaml`, and the one shared
//! age key (`APPRAFTER_AGE_KEY`, else `<home>/.config/apprafter/age.key`) the caches in
//! `state.json` are encrypted to. Only [`super::SecretStore`] calls in.

use std::path::Path;

use cli_core::secrets::{load_identity, CachedSecret};
use cli_core::target::{
    list_target_names, load_target_credentials, save_target_credentials, TargetCredentials,
    TargetStorePaths,
};
use cli_core::{CliError, Result};
use cli_state::{State, StatePaths};

use super::{CacheKey, NewCacheKey, StoredToken};
use crate::context::SecretString;

/// D.4a T6: in the core's own tests a door opens only what lies under the temp dir — the
/// deepest part of `path` that exists, canonicalised — so no core test reaches the account's
/// own store or age key, whatever its context. Compiled out of every other build (and skipped,
/// as a `cfg(test)` item, by the purity guard that forbids `std::env::temp_dir`).
#[cfg(test)]
fn under_temp(path: &Path) {
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
fn under_temp(_: &Path) {}

/// What `target`'s credentials file holds, without the token: its length only.
pub(super) fn token_presence(store: &TargetStorePaths, target: &str) -> Result<StoredToken> {
    under_temp(&store.target_credentials_file(target));
    Ok(
        match load_target_credentials(store, target)?.hetzner_token {
            Some(token) => StoredToken::InFile {
                chars: token.len() as u32,
            },
            None => StoredToken::None,
        },
    )
}

pub(super) fn stored_token(store: &TargetStorePaths, target: &str) -> Result<Option<SecretString>> {
    under_temp(&store.target_credentials_file(target));
    Ok(load_target_credentials(store, target)?
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
    Ok(CacheKey {
        identity,
        path: age_key_path.to_path_buf(),
    })
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
        return Ok(NewCacheKey::Ready(CacheKey {
            identity,
            path: age_key_path.to_path_buf(),
        }));
    }
    if !held(store)?.is_empty() {
        return Err(missing(target, age_key_path));
    }
    Ok(NewCacheKey::FirstUse {
        path: age_key_path.to_path_buf(),
    })
}

/// The missing-key error for `target`'s cache.
fn missing(target: &str, age_key_path: &Path) -> CliError {
    CliError::AgeKeyMissing {
        path: age_key_path.display().to_string(),
        target: target.to_string(),
        stopped: None,
    }
}

/// One thing a target's state holds under the age key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub target: String,
    pub what: Holding,
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
                        });
                    }
                }
            }
            Err(e) => out.push(Held {
                target,
                what: Holding::Unreadable(e.to_string()),
            }),
        }
    }
    Ok(out)
}
