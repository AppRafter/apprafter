// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Re-sealing a target's cached secrets to a store key (D.4a T36): `lock on`'s step 4, which
//! moves each cache from the shared age key to the store's own. Moved here from `lock::on`
//! (D.4b B85) so a key change re-seals through the same steps. One target at a time, under the
//! store's held write lock, its `state.json` read again there and only its caches changed
//! (WI-487, `State::update`). Each cache sealed to the store's key — a skipped one too —
//! records it (F13, B86: the store's id and the vault's); a dropped one loses its record; a
//! left one keeps whatever it had.

use cli_core::secrets::{CachedSecret, Identity};
use cli_core::target::TargetStorePaths;
use cli_core::StoreLock;
use cli_state::{State, StatePaths, Write};
use zeroize::Zeroizing;

use super::on::{dropped, sealed_and_checked, CacheDrop, DroppedCache};
use crate::error::{CoreError, CoreResult};
use crate::secrets::{transfer, CacheKey};

/// The keys a cache the store's key does not open is tried with.
pub(crate) enum OldKeys<'a> {
    /// `lock on`'s: the shared age keys, read when a cache first needs them (D.4a f Task 3) —
    /// never when every cache is the store's already (a resume), or none is.
    Shared(&'a transfer::SharedKeys<'a>),
}

impl OldKeys<'_> {
    fn get(&self) -> CoreResult<&[Identity]> {
        let OldKeys::Shared(keys) = self;
        Ok(keys.get()?)
    }
}

/// What a re-seal does with a cache no key it has opens, and with a plaintext kubeconfig.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ResealMode<'a> {
    /// `lock on`'s step 4 (T36): a cache no key opens is dropped when the plan listed it
    /// (`agreed`), or on a resume (whose plan says so), else left and named; a winning
    /// plaintext kubeconfig is sealed, a shadowed one dropped.
    LockOn {
        agreed: &'a [CacheDrop],
        resume: bool,
    },
}

/// What a re-seal did, over every target it was given.
#[derive(Debug, Default)]
pub(crate) struct Resealed {
    /// Caches re-sealed to the store's key and checked.
    pub(crate) resealed: u32,
    /// Caches no key opens that were dropped.
    pub(crate) dropped: Vec<DroppedCache>,
    /// Caches no key opens that were left as they were, each named.
    pub(crate) left: Vec<DroppedCache>,
}

/// Re-seals `target`'s cached secrets to `key` under `held` (module docs): a cache under an
/// old key re-sealed and checked; one already the key's skipped (a resume) and recorded; one no
/// key opens dropped or left as `mode` says; a plaintext kubeconfig sealed or dropped.
pub(crate) fn reseal(
    store: &TargetStorePaths,
    held: &StoreLock,
    target: &str,
    key: &CacheKey,
    old: &OldKeys<'_>,
    mode: ResealMode<'_>,
    done: &mut Resealed,
) -> CoreResult<()> {
    let paths = StatePaths::for_active_target(store, target);
    // F13, B86: the store's vault the key is in, by its store id and vault id.
    let sealed_to = key.record()?;
    State::update::<CoreError>(&paths, held, |state| {
        let Some(mut h) = state.hetzner_cloud.clone() else {
            return Ok(Write::Leave);
        };
        let shadowed = h.kubeconfig_age.is_some();
        let mut changed = false;
        for secret in [CachedSecret::Kubeconfig, CachedSecret::ArgocdPassword] {
            let (slot, record) = h.cache_mut(secret);
            let Some(armored) = slot.clone() else {
                continue;
            };
            if transfer::opens(key, secret, &armored).is_some() {
                // A resume: sealed to the store's key already; recorded so, once.
                if *record != sealed_to {
                    *record = sealed_to.clone();
                    changed = true;
                }
                continue;
            }
            match transfer::open_with_any(&armored, old.get()?) {
                Some(plain) => {
                    *slot = Some(sealed_and_checked(store, key, secret, target, &plain)?);
                    *record = sealed_to.clone();
                    changed = true;
                    done.resealed += 1;
                }
                None => {
                    let ResealMode::LockOn { agreed, resume } = mode;
                    let listed = agreed.contains(&CacheDrop {
                        target: target.to_string(),
                        secret,
                    });
                    if resume || listed {
                        *slot = None;
                        *record = None;
                        changed = true;
                        done.dropped.push(dropped(
                            target,
                            secret,
                            "no age key this run read decrypts it",
                        ));
                    } else {
                        done.left.push(dropped(
                            target,
                            secret,
                            "it stopped decrypting after the plan was made; `--refresh` \
                             fetches it again",
                        ));
                    }
                }
            }
        }
        if let Some(plain) = h.kubeconfig_yaml.take().map(Zeroizing::new) {
            changed = true;
            if shadowed {
                done.dropped.push(dropped(
                    target,
                    CachedSecret::Kubeconfig,
                    "a plaintext copy beside the encrypted one",
                ));
            } else {
                h.kubeconfig_age = Some(sealed_and_checked(
                    store,
                    key,
                    CachedSecret::Kubeconfig,
                    target,
                    &plain,
                )?);
                h.kubeconfig_sealed_to = sealed_to.clone();
                done.resealed += 1;
            }
        }
        if !changed {
            return Ok(Write::Leave);
        }
        state.hetzner_cloud = Some(h);
        Ok(Write::Save)
    })?;
    Ok(())
}
