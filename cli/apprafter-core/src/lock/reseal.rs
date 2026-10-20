// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Re-sealing a target store's cached secrets to a store key, one state folder at a time,
//! under the store's held write lock, each `state.json` read again there and only its caches
//! changed (WI-487). Two runs use it (D.4b B85):
//!
//! - `lock on`'s step 4 (D.4a T36), from the shared age key to the store's own: a cache no
//!   key of the run opens is dropped when the plan listed it, or on a resume, else left; a
//!   winning plaintext kubeconfig is sealed, a shadowed one dropped; `state.json` written as
//!   every writer writes it (`State::update`).
//! - A key change's R3 (B63), from the store key it retires to the new one, forward and on a
//!   finish alike: a cache neither key opens is left and named with its refresh, never
//!   dropped; a plaintext kubeconfig is left as it is (T38: a locked store never seals one
//!   later) and named, a secret in clear (critic round, finding 15); a failure of the new
//!   key fails the step (rule 2); every `state.json` it walks is
//!   on disk durably before the step returns (B78), so the retiring key may leave the vault,
//!   and one that does not parse is renamed aside and named (B79). It walks every folder of
//!   `<root>/state/` — a target's, and one a best-effort `target remove` or `rename` left
//!   behind, which it re-seals and names all the same — a link to a folder included, as
//!   every reader and writer of a target's state goes through it.
//!
//! Each cache sealed to the key — a skipped one too — records it (F13, B86: the store's id and
//! the vault's); a dropped one loses its record; a left one keeps whatever it had.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the key change's half: d4b-c2a's R3 (WI-501) is its first caller"
    )
)]

use std::path::{Path, PathBuf};

use cli_core::secrets::{CachedSecret, Identity};
use cli_core::target::TargetStorePaths;
use cli_core::{CliError, StoreLock};
use cli_state::{DurableWrite, State, StatePaths, Write};
use zeroize::Zeroizing;

use super::on::{dropped, refresh, sealed_and_checked, CacheDrop, DroppedCache};
use crate::error::{CoreError, CoreResult};
use crate::secrets::transfer::{self, Opened};
use crate::secrets::CacheKey;

/// A cache neither store key of a key change opens, in a folder no target uses: there is no
/// target to refresh (B85).
const ORPHAN_LEFT: &str = "it is sealed to a key this key change did not have, or it is \
                           damaged; no target uses its folder";

/// A kubeconfig in clear in a folder no target uses: named, with no refresh to name (critic
/// round, finding 15).
const ORPHAN_PLAINTEXT: &str =
    "a copy in clear, which a locked store never seals; no target uses its folder";

/// The keys a cache the target key does not open is tried with.
pub(crate) enum OldKeys<'a> {
    /// `lock on`'s: the shared age keys, read when a cache first needs them (D.4a f Task 3)
    /// — never when every cache is the store's already (a resume), or none is.
    Shared(&'a transfer::SharedKeys<'a>),
    /// A key change's: the store key it retires (B63), the vault's `age-identity-retiring`.
    Retiring(&'a [Identity]),
}

impl OldKeys<'_> {
    fn get(&self) -> CoreResult<&[Identity]> {
        match self {
            OldKeys::Shared(keys) => Ok(keys.get()?),
            OldKeys::Retiring(keys) => Ok(*keys),
        }
    }
}

/// What a re-seal does with a cache no key it has opens, and how it writes (module docs).
#[derive(Debug, Clone, Copy)]
pub(crate) enum ResealMode<'a> {
    /// `lock on`'s step 4: `agreed` are the drops its plan listed; `resume` drops any cache
    /// no key opens, as the resume's plan says.
    LockOn {
        agreed: &'a [CacheDrop],
        resume: bool,
    },
    /// A key change's R3, forward and on a finish alike.
    Rotate,
}

/// A folder of `<root>/state/`: a target's, or one no target uses (`orphan`, B85).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StateFolder {
    pub(crate) name: String,
    pub(crate) orphan: bool,
}

impl StateFolder {
    /// Target `name`'s folder, as `lock on` walks its targets.
    pub(crate) fn target(name: &str) -> StateFolder {
        StateFolder {
            name: name.to_string(),
            orphan: false,
        }
    }
}

/// A `state.json` a key change renamed aside (B79): its folder, and where it went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetAside {
    pub(crate) folder: StateFolder,
    pub(crate) to: PathBuf,
}

/// What a re-seal did, over every folder it was given.
#[derive(Debug, Default)]
pub(crate) struct Resealed {
    /// Caches re-sealed to the key and checked.
    pub(crate) resealed: u32,
    /// `lock on`: caches no key opens that were dropped.
    pub(crate) dropped: Vec<DroppedCache>,
    /// Caches no key opens that were left as they were, each named.
    pub(crate) left: Vec<DroppedCache>,
    /// A key change: the state folders of no target it re-sealed, by path (B85).
    pub(crate) orphans: Vec<PathBuf>,
    /// A key change: the `state.json` files that did not parse, renamed aside (B79).
    pub(crate) set_aside: Vec<SetAside>,
}

/// Every folder of `<root>/state/` a target could use, in name order — what a key change's
/// R3 walks (B85); a link to a folder is one ([`cli_core::list_state_folder_names`]). A
/// folder is a target's when `<root>/targets/<name>` is a folder, as the
/// file system answers: on a case-insensitive one (macOS, Windows) a name that differs from
/// its target's only in case is that target's, never an orphan whose words say "delete it".
/// A target without a state folder caches nothing: there is nothing of it to walk.
pub(crate) fn state_folders(store: &TargetStorePaths) -> CoreResult<Vec<StateFolder>> {
    let mut folders = Vec::new();
    for name in cli_core::list_state_folder_names(store)? {
        let orphan = !is_folder(&store.target_dir(&name))?;
        folders.push(StateFolder { name, orphan });
    }
    Ok(folders)
}

/// Whether `path` is a folder: `false` when nothing is there (or a file stands where a folder
/// above it would be); any other failure is the store's I/O error, naming the path.
fn is_folder(path: &Path) -> CoreResult<bool> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(source) => Err(CoreError::Cli(CliError::StorePathIo {
            path: path.to_path_buf(),
            source,
        })),
    }
}

/// Re-seals `folder`'s cached secrets to `key` under `held` (module docs); what it did goes
/// into `done`.
pub(crate) fn reseal(
    store: &TargetStorePaths,
    held: &StoreLock,
    folder: &StateFolder,
    key: &CacheKey,
    old: &OldKeys<'_>,
    mode: ResealMode<'_>,
    done: &mut Resealed,
) -> CoreResult<()> {
    let paths = StatePaths::for_active_target(store, &folder.name);
    // F13, B86: the store's vault the key is in, by its store id and vault id.
    let sealed_to = key.record()?;
    let target = folder.name.as_str();
    let change = |state: &mut State| -> CoreResult<Write> {
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
            if opens_with(key, secret, &armored, mode)? {
                // A resume or a finish: sealed to the key already; recorded so, once.
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
                None => match mode {
                    ResealMode::LockOn { agreed, resume } => {
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
                    // B85: a key change never drops a cache, forward or on a finish.
                    ResealMode::Rotate => {
                        let why = left_why(secret, folder);
                        done.left.push(dropped(target, secret, &why));
                    }
                },
            }
        }
        // Critic round, finding 15: a key change never seals a kubeconfig in clear (T38),
        // and names it, never masked (rule 1) — shadowed by an encrypted one or not.
        if matches!(mode, ResealMode::Rotate) && h.kubeconfig_yaml.is_some() {
            done.left.push(dropped(
                target,
                CachedSecret::Kubeconfig,
                &plaintext_why(folder),
            ));
        }
        // A plaintext kubeconfig: `lock on` seals a winning one and drops a shadowed one; a
        // key change leaves it as it is (T38: a locked store never seals one later).
        let plaintext = match mode {
            ResealMode::LockOn { .. } => h.kubeconfig_yaml.take().map(Zeroizing::new),
            ResealMode::Rotate => None,
        };
        if let Some(plain) = plaintext {
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
    };
    match mode {
        ResealMode::LockOn { .. } => {
            State::update::<CoreError>(&paths, held, change)?;
        }
        ResealMode::Rotate => {
            let wrote = State::update_durable::<CoreError>(&paths, held, change)?;
            if let DurableWrite::SetAside(to) = wrote {
                done.set_aside.push(SetAside {
                    folder: folder.clone(),
                    to,
                });
            }
        }
    }
    if folder.orphan {
        done.orphans.push(store.state_dir(&folder.name));
    }
    Ok(())
}

/// Whether `key` opens `armored`. For `lock on`, as D.4a wrote it: any failure is "it does
/// not". For a key change, a cache the key does not open goes on to the retiring key, and
/// any other failure of the key — a vault it cannot open, a session that refuses — fails the
/// step (rule 2: a key that cannot open is not a cache to leave).
fn opens_with(
    key: &CacheKey,
    secret: CachedSecret,
    armored: &str,
    mode: ResealMode<'_>,
) -> CoreResult<bool> {
    if let ResealMode::LockOn { .. } = mode {
        return Ok(transfer::opens(key, secret, armored).is_some());
    }
    let opened = transfer::opened_by(key, secret, armored)?;
    Ok(matches!(opened, Opened::Plain(_)))
}

/// Why a key change left a cache no key it has opens (B85): never dropped; neither key's
/// failure tells a cache sealed to another key from a damaged one, so the words say both,
/// and name the refresh where a target can be refreshed.
fn left_why(secret: CachedSecret, folder: &StateFolder) -> String {
    if folder.orphan {
        return ORPHAN_LEFT.to_string();
    }
    format!(
        "it is sealed to a key this key change did not have, or it is damaged: `{}` fetches \
         it again",
        refresh(secret, &folder.name)
    )
}

/// Why a key change left a kubeconfig in clear (T38: a locked store never seals one later):
/// named, never masked; a refresh replaces it with a sealed copy (`write_cache` clears the
/// plaintext slot), where a target can be refreshed (critic round, finding 15).
fn plaintext_why(folder: &StateFolder) -> String {
    if folder.orphan {
        return ORPHAN_PLAINTEXT.to_string();
    }
    format!(
        "a copy in clear, which a locked store never seals: `{}` replaces it with a sealed one",
        refresh(CachedSecret::Kubeconfig, &folder.name)
    )
}

/// B85's words for a state folder of no target a key change re-sealed, for its outcome.
pub(crate) fn orphan_line(path: &Path) -> String {
    format!(
        "a state folder left by an earlier target remove or rename: {} — delete it unless a \
         target will use that name",
        path.display()
    )
}

/// B79's words for a `state.json` a key change renamed aside, for its outcome.
pub(crate) fn set_aside_line(aside: &SetAside) -> String {
    let to = aside.to.display();
    if aside.folder.orphan {
        return format!(
            "a state file in a folder no target uses did not parse and was renamed to {to}"
        );
    }
    let name = &aside.folder.name;
    // Critic round, finding 4: with `state.json` set aside both refreshes stop at "state
    // has no hetzner_cloud section", so the rebuild is named first.
    format!(
        "the state file of target `{name}` did not parse and was renamed to {to}: `apprafter \
         import --force --target {name}` rebuilds it, then `{}` and `{}` fetch its cached \
         secrets again — they were sealed to the store key this key change retired",
        refresh(CachedSecret::Kubeconfig, name),
        refresh(CachedSecret::ArgocdPassword, name)
    )
}
