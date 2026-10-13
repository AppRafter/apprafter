// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The vault backend (D.4a §3.4): a locked store's `credentials.yaml` markers, read without
//! opening anything; its vault, read for a session and checked to be this store's before any
//! key is asked for (T29); and its writes, each under the held write lock as a full
//! read-modify-write with the data key, read back and checked (T28, T41), and synced to disk
//! before a marker or the caller's directory move relies on it. Only [`super::SecretStore`]
//! calls in.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli_core::target::{parse_target_credentials, read_target_credentials_bytes, TargetStorePaths};
use cli_core::{
    CliError, DamageKind, RefusedInput, Result, SecretsModeKind, SlotRefusal, StoreLock,
    VaultProblem, VaultWriteProblem,
};
use zeroize::Zeroizing;

use super::access::KeySession;
use super::files::under_temp;
use super::marker::{classify, write_marker, Marker};
use super::store_file::StoreId;
use crate::context::SecretString;
use crate::vault::{
    self, OsRandom, PayloadWhat, UnlockedVault, VaultError, VaultFile, FORMAT_V1, VAULT_FILE,
};

/// The core's `writer` (§3.7): `apprafter <version>`, the default of
/// [`VaultAccess::writer`](super::VaultAccess::writer), which a client that is not the CLI
/// overrides with its own name.
pub(crate) const VAULT_WRITER: &str = concat!("apprafter ", env!("CARGO_PKG_VERSION"));

#[cfg(all(test, unix))]
thread_local! {
    /// Tests: the next fsync of the store root fails.
    pub(super) static ROOT_SYNC_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

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

/// `<root>/vault`.
pub(crate) fn vault_path(root: &Path) -> PathBuf {
    root.join(VAULT_FILE)
}

/// The vault's bytes as they are now, checked to belong to the store `id` (its header names
/// the store, T29) before anyone is asked for a key. Read without the store lock (R3): a
/// session opens exactly these bytes.
pub(crate) fn read_vault(store: &TargetStorePaths, id: &StoreId) -> Result<Arc<[u8]>> {
    let path = vault_path(store.root());
    under_temp(&path);
    let bytes = vault::file::read(&path).map_err(|e| vault_error(store.root(), e))?;
    let parsed = VaultFile::parse(&bytes).map_err(|e| vault_error(store.root(), e))?;
    let named = parsed.header().store_id.as_str();
    if named != id.as_str() {
        // Not authenticated yet: shown only when it is a store id.
        let shown = StoreId::parse(named).map_or_else(
            || "an id that is no store id".to_string(),
            |n| n.to_string(),
        );
        return Err(CliError::VaultUnreadable {
            path,
            problem: VaultProblem::OtherStore,
            detail: format!("it names store {shown}, and store.yaml names {id}"),
        });
    }
    Ok(Arc::from(bytes))
}

/// A vault failure as the secret store's error (D.4a T69, §3.12: one namespace,
/// `apprafter::secrets::*`), for the store's sessions and writes — the boundary d4a-d's
/// prompt reuses. `root` is the store root. A failed authentication is never "no such
/// secret", and a key for another vault is never damage (rule 3). A setter's refusal of a
/// value is not this function's: [`vault_write_error`].
pub(crate) fn vault_error(root: &Path, e: VaultError) -> CliError {
    let path = vault_path(root);
    let detail = e.to_string();
    let unreadable = |problem: VaultProblem| CliError::VaultUnreadable {
        path: path.clone(),
        problem,
        detail: detail.clone(),
    };
    let slot = |reason: SlotRefusal| CliError::SlotRefused {
        store: root.to_path_buf(),
        reason,
    };
    match e {
        VaultError::NotAVault => unreadable(VaultProblem::NotAVault),
        VaultError::UnsupportedVersion { found, .. } if found > FORMAT_V1 => {
            unreadable(VaultProblem::NewerFormat { version: found })
        }
        // A number below the first format is no format: damage, never "upgrade AppRafter".
        VaultError::UnsupportedVersion { .. } => unreadable(VaultProblem::Damaged {
            what: DamageKind::Malformed,
        }),
        VaultError::Missing { .. } => unreadable(VaultProblem::Missing),
        VaultError::Malformed { .. } => unreadable(VaultProblem::Damaged {
            what: DamageKind::Malformed,
        }),
        VaultError::SlotCorrupt { .. } => unreadable(VaultProblem::Damaged {
            what: DamageKind::SlotCorrupt,
        }),
        VaultError::Tampered => unreadable(VaultProblem::Damaged {
            what: DamageKind::Tampered,
        }),
        VaultError::PayloadInvalid { .. } => unreadable(VaultProblem::Damaged {
            what: DamageKind::PayloadInvalid,
        }),
        VaultError::StoreMismatch { .. } => unreadable(VaultProblem::OtherStore),
        // A slot's secret is the prompt's: one that opens nothing is a failed attempt.
        VaultError::WrongSecret { .. } => CliError::UnlockFailed {
            store: root.to_path_buf(),
            attempts: 1,
            stopped_before: None,
        },
        VaultError::NoUsableSlot { .. } => slot(SlotRefusal::NoUsableSlot),
        VaultError::KdfTooWeak { .. } => slot(SlotRefusal::KdfTooWeak),
        VaultError::KdfOutOfRange { .. } => slot(SlotRefusal::KdfOutOfRange),
        VaultError::LastRecoverySlot => slot(SlotRefusal::LastRecoverySlot),
        VaultError::NoUsableSlotLeft => slot(SlotRefusal::NoUsableSlotLeft),
        // A key for another vault: the lock was turned off and on again meanwhile.
        VaultError::KeyStale => CliError::StoreChanged {
            store: root.to_path_buf(),
            prepared: SecretsModeKind::Locked,
            now: SecretsModeKind::Locked,
        },
        VaultError::RandomUnavailable { detail } => CliError::RandomUnavailable { detail },
        VaultError::TooLarge { limit, .. } => CliError::VaultUnwritable {
            path: path.clone(),
            problem: VaultWriteProblem::TooLarge { limit },
        },
        VaultError::NoReplaceUnsupported { dir, fs } => CliError::VaultUnwritable {
            path: dir,
            problem: VaultWriteProblem::NoReplaceUnsupported { fs },
        },
        // c1 refuses a write without the held lock itself (its Deviation 9); the store checks
        // first (`SecretStore::recheck`), so this is the same refusal, should it ever be met.
        VaultError::WriteLockNotHeld { .. } => CliError::VaultNeedsWriteLock {
            store: root.to_path_buf(),
        },
        // Never reached from the secret store, whose reads and writes take no slot secret, no
        // new password and no slot id: d4a-d's `lock::open` maps a typed recovery key's
        // refusal to `CoreError::RecoveryKeyInvalid` (T62: with its group) and
        // `lock::check_password` a password's to `CoreError::LockPasswordRefused` before the
        // vault sees either, and `InvalidInput` / `SlotNotFound` are a caller's bug (c1's
        // hand-off). Kept in c1's words, never as an unlock or a damage the person must act on.
        VaultError::RecoveryKey(_)
        | VaultError::PasswordRefused(_)
        | VaultError::InvalidInput(_)
        | VaultError::SlotNotFound { .. } => CliError::Other(detail.clone()),
        VaultError::Io { path: at, source } => CliError::Io(std::io::Error::new(
            source.kind(),
            format!("{}: {source}", at.display()),
        )),
    }
}

/// A setter's refusal of a value the vault cannot hold (c1's `PayloadInvalid` from
/// `set_hetzner_token` and `rename_target`), refused before anything is written:
/// `vault_unwritable{refused}`, never a damaged vault. Anything else is [`vault_error`]'s.
fn vault_write_error(root: &Path, e: VaultError) -> CliError {
    let refused = |what: RefusedInput| CliError::VaultUnwritable {
        path: vault_path(root),
        problem: VaultWriteProblem::Refused { what },
    };
    match e {
        VaultError::PayloadInvalid {
            what: PayloadWhat::TargetName,
        } => refused(RefusedInput::TargetName),
        VaultError::PayloadInvalid {
            what: PayloadWhat::HetznerToken,
        } => refused(RefusedInput::Token),
        VaultError::PayloadInvalid {
            what: PayloadWhat::TargetExists,
        } => refused(RefusedInput::NameTaken),
        other => vault_error(root, other),
    }
}

/// The store root's directory entries — the vault's rename among them — on disk before a
/// marker or the caller's directory move relies on them (overview §8 risk 10): c1's `replace`
/// syncs the directory best effort only. A failure is an error, and nothing that relies on the
/// vault is written. Unix; Windows has no directory fsync (c1's Risks 5).
fn sync_root(store: &TargetStorePaths) -> Result<()> {
    #[cfg(unix)]
    {
        let root = store.root();
        let failed = |e: std::io::Error| {
            CliError::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {e}", root.display()),
            ))
        };
        #[cfg(test)]
        if ROOT_SYNC_FAILS.with(|fail| fail.replace(false)) {
            return Err(failed(std::io::Error::other(
                "injected directory fsync failure",
            )));
        }
        std::fs::File::open(root)
            .and_then(|dir| dir.sync_all())
            .map_err(failed)?;
    }
    #[cfg(not(unix))]
    let _ = store;
    Ok(())
}

/// The vault as it is now, opened with `key`.
fn open_current(store: &TargetStorePaths, id: &StoreId, key: &KeySession) -> Result<UnlockedVault> {
    let bytes = read_vault(store, id)?;
    key.open(&bytes)
}

/// `opened`, sealed with a fresh nonce as `writer` (the client's, [`VaultAccess::writer`]) and
/// written over the vault under `held`.
///
/// [`VaultAccess::writer`]: super::VaultAccess::writer
fn write_back(
    store: &TargetStorePaths,
    opened: &UnlockedVault,
    held: &StoreLock,
    writer: &str,
) -> Result<()> {
    let sealed = opened
        .seal(writer, &mut OsRandom)
        .map_err(|e| vault_error(store.root(), e))?;
    vault::file::replace(&vault_path(store.root()), &sealed, held)
        .map_err(|e| vault_error(store.root(), e))
}

/// A vault that, read back after a write, does not hold what was written.
fn not_kept(store: &TargetStorePaths, what: &str) -> CliError {
    CliError::VaultUnreadable {
        path: vault_path(store.root()),
        problem: VaultProblem::Damaged {
            what: DamageKind::PayloadInvalid,
        },
        detail: format!("read back after this write, it does not hold {what}"),
    }
}

/// `token` for `target` into the vault, then — once the vault is on disk — the target's
/// marker (T41: the vault first). The caller checked the lock state and that `held` is held.
pub(super) fn put_token(
    store: &TargetStorePaths,
    id: &StoreId,
    key: &KeySession,
    held: &StoreLock,
    writer: &str,
    target: &str,
    token: &SecretString,
) -> Result<()> {
    let mut opened = open_current(store, id, key)?;
    opened
        .payload_mut()
        .set_hetzner_token(target, token.clone())
        .map_err(|e| vault_write_error(store.root(), e))?;
    write_back(store, &opened, held, writer)?;
    let back = open_current(store, id, key)?;
    // c1 hands out owned copies (its Deviation 12): bind one, then compare.
    let kept = back.payload().hetzner_token(target);
    if kept.as_ref().map(SecretString::expose) != Some(token.expose()) {
        return Err(not_kept(store, "the token it was given"));
    }
    sync_root(store)?;
    write_marker(store, target, id)
}

/// Every entry under `target/<target>/` out of the vault; `false` when there was none (nothing
/// is written then). The caller removes the directory first, marker included (T41: a crash
/// between leaves an entry no directory names, harmless).
pub(super) fn remove_token(
    store: &TargetStorePaths,
    id: &StoreId,
    key: &KeySession,
    held: &StoreLock,
    writer: &str,
    target: &str,
) -> Result<bool> {
    let mut opened = open_current(store, id, key)?;
    if opened.payload_mut().remove_target(target) == 0 {
        return Ok(false);
    }
    write_back(store, &opened, held, writer)?;
    if open_current(store, id, key)?
        .payload()
        .hetzner_token(target)
        .is_some()
    {
        return Err(not_kept(store, "the removal"));
    }
    Ok(true)
}

/// T41's first step of a rename, before the caller moves the directories: `from`'s token also
/// under `to`, `from`'s kept, and on disk before the move relies on it — a crash after it
/// leaves both names, each readable. Nothing is written when `from` has no token (an older
/// AppRafter already renamed the directories alone, T43, or none was stored) or `to` holds
/// the same one (a re-run). A token an older AppRafter's `target remove` left under `to` (T43)
/// is overwritten: the caller refused an existing `to` directory under the same held lock.
pub(super) fn copy_token(
    store: &TargetStorePaths,
    id: &StoreId,
    key: &KeySession,
    held: &StoreLock,
    writer: &str,
    from: &str,
    to: &str,
) -> Result<()> {
    let mut opened = open_current(store, id, key)?;
    let Some(token) = opened.payload().hetzner_token(from) else {
        return Ok(());
    };
    if opened.payload().hetzner_token(to).as_ref() == Some(&token) {
        return Ok(());
    }
    opened
        .payload_mut()
        .set_hetzner_token(to, token.clone())
        .map_err(|e| vault_write_error(store.root(), e))?;
    write_back(store, &opened, held, writer)?;
    let back = open_current(store, id, key)?;
    let (from_now, to_now) = (
        back.payload().hetzner_token(from),
        back.payload().hetzner_token(to),
    );
    if from_now.as_ref() != Some(&token) || to_now.as_ref() != Some(&token) {
        return Err(not_kept(store, "the copy"));
    }
    sync_root(store)
}

/// T41's last step of a rename, after the caller moved the directories: every entry under
/// `from` — the token, and any entry a newer AppRafter keeps there (T21: c1's `rename_target`
/// moves them all) — onto `to`, replacing what `to` held ([`copy_token`]'s copy, or a token an
/// older AppRafter's `target remove` left, T43). A crash before it leaves `from`'s entries
/// behind, harmless. Nothing is written when `from` has no token (an older AppRafter renamed
/// the directories alone, T43, or none was stored).
pub(super) fn rename_token(
    store: &TargetStorePaths,
    id: &StoreId,
    key: &KeySession,
    held: &StoreLock,
    writer: &str,
    from: &str,
    to: &str,
) -> Result<()> {
    let mut opened = open_current(store, id, key)?;
    let Some(moved) = opened.payload().hetzner_token(from) else {
        return Ok(());
    };
    let payload = opened.payload_mut();
    payload.remove_target(to);
    payload
        .rename_target(from, to)
        .map_err(|e| vault_write_error(store.root(), e))?;
    write_back(store, &opened, held, writer)?;
    let back = open_current(store, id, key)?;
    let (from_now, to_now) = (
        back.payload().hetzner_token(from),
        back.payload().hetzner_token(to),
    );
    if from_now.is_some() || to_now.as_ref() != Some(&moved) {
        return Err(not_kept(store, "the rename"));
    }
    Ok(())
}
