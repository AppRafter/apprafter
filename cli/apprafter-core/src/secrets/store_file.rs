// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `<root>/store.yaml`: where a target store's secrets are (ADR 0068 §2; D.4a T31, T32, §3.5).
//!
//! No file is a store in files — and "no" means `ErrorKind::NotFound` only. Any other way it
//! cannot be read, a format newer than this build, an unknown `secrets:` value, a lock without
//! an id, or a `vault` beside a store that does not say it is locked, is
//! `CliError::StoreFileUnreadable`: the store is then treated as locked and none of its secrets
//! is used, so deleting or editing this file never turns a locked store into one that honours
//! `HCLOUD_TOKEN`. Written only under the store's held write lock, atomically. The slot list
//! and the timeout are display copies: the vault is authoritative (T29, T33). Parsed into a
//! private raw struct, never `Deserialize` on a report type (D.3 §3.10).

use std::fmt;
use std::io;
use std::path::Path;

use cli_core::{CliError, StoreLock};
use serde::{Deserialize, Serialize};

use super::files::under_temp;
use super::SecretsMode;
use crate::vault::{SlotId, VaultId};
use crate::vault::{VaultResult, VaultRng, VAULT_FILE};

/// The file's name under the store root.
pub const STORE_FILE: &str = "store.yaml";
/// The format this build reads and writes.
pub const STORE_FILE_FORMAT: u32 = 1;

/// The first lines of every `store.yaml` this build writes.
const HEADER: &str = "# The lock state of this AppRafter target store (ADR 0068). Never delete \
                      this file while\n# the `vault` beside it exists: without it the store's \
                      secrets cannot be used.\n";

/// A store's id: 16 lowercase hex characters, minted at every `lock on` from files (T32).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoreId(String);

impl StoreId {
    /// Its length in characters.
    pub const LEN: usize = 16;

    /// `text` when it is exactly [`Self::LEN`] lowercase hex characters.
    pub fn parse(text: &str) -> Option<StoreId> {
        (text.len() == Self::LEN && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| StoreId(text.to_string()))
    }

    /// A fresh id from `rng` (the operating system's, but in tests).
    pub fn generate(rng: &mut dyn VaultRng) -> VaultResult<StoreId> {
        let mut bytes = [0u8; 8];
        rng.fill(&mut bytes)?;
        Ok(StoreId(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One slot, as `store.yaml` mirrors it for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SlotInfo {
    pub kind: LockSlotKind,
    pub label: String,
}

/// What opens a slot. D.4a makes `Password` and `RecoveryKey`; the others are D.4b's and later,
/// shown when found; any other value reads as `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LockSlotKind {
    Password,
    RecoveryKey,
    OsPassword,
    WindowsHello,
    TouchId,
    Unknown,
}

impl LockSlotKind {
    /// Its name in `store.yaml` and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            LockSlotKind::Password => "password",
            LockSlotKind::RecoveryKey => "recovery_key",
            LockSlotKind::OsPassword => "os_password",
            LockSlotKind::WindowsHello => "windows_hello",
            LockSlotKind::TouchId => "touch_id",
            LockSlotKind::Unknown => "unknown",
        }
    }

    fn parse(text: &str) -> LockSlotKind {
        match text {
            "password" => LockSlotKind::Password,
            "recovery_key" => LockSlotKind::RecoveryKey,
            "os_password" => LockSlotKind::OsPassword,
            "windows_hello" => LockSlotKind::WindowsHello,
            "touch_id" => LockSlotKind::TouchId,
            _ => LockSlotKind::Unknown,
        }
    }
}

/// A vault slot's kind as `store.yaml` and the reports name it; a reserved or unknown byte is
/// `Unknown`.
impl From<crate::vault::SlotKind> for LockSlotKind {
    fn from(k: crate::vault::SlotKind) -> Self {
        use crate::vault::SlotKind;
        match k {
            SlotKind::Password => LockSlotKind::Password,
            SlotKind::RecoveryKey => LockSlotKind::RecoveryKey,
            SlotKind::OsPassword => LockSlotKind::OsPassword,
            SlotKind::WindowsHello => LockSlotKind::WindowsHello,
            SlotKind::TouchId => LockSlotKind::TouchId,
            SlotKind::Unknown(_) => LockSlotKind::Unknown,
        }
    }
}

/// `store.yaml`, read and checked: `id` is set whenever `secrets` is not `Files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreFile {
    pub format: u32,
    pub secrets: SecretsMode,
    pub id: Option<StoreId>,
    pub slots: Vec<SlotInfo>,
    pub unlock_timeout_min: Option<u32>,
    /// A key change's fields, with `secrets: rotating` only (D.4b B63); `None` in every other
    /// state.
    pub rotation: Option<Rotation>,
}

/// What `store.yaml` says of a key change that did not finish (D.4b B63, ADR 0069 §7):
/// fields only `secrets: rotating` has, which AppRafter 0.2.80 never reads — it refuses the
/// state first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rotation {
    /// `rotating_to`: the id of the vault the key change writes. A finish tells from it
    /// whether the change was written (d4b-c2b).
    pub to: VaultId,
    /// `hello_drop`: the Windows Hello slots the key change drops, whose keys on this
    /// computer are handed to the client before `store.yaml` forgets them (R4b, B84).
    pub hello_drop: Vec<SlotId>,
}

#[derive(Deserialize)]
struct RawStoreFile {
    format: u32,
    secrets: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    slots: Vec<RawSlot>,
    #[serde(default)]
    unlock_timeout_min: Option<u32>,
    #[serde(default)]
    rotating_to: Option<String>,
    #[serde(default)]
    hello_drop: Vec<String>,
}

#[derive(Deserialize)]
struct RawSlot {
    kind: String,
    #[serde(default)]
    label: String,
}

#[derive(Serialize)]
struct RawOut<'a> {
    format: u32,
    secrets: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    slots: Vec<RawSlotOut<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unlock_timeout_min: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rotating_to: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    hello_drop: Vec<String>,
}

#[derive(Serialize)]
struct RawSlotOut<'a> {
    kind: &'a str,
    label: &'a str,
}

/// `<root>/store.yaml`: `None` for a store in files (no file, and no vault). Every other case
/// the module docs list is `StoreFileUnreadable`. Of the vault it reads the directory entry
/// only, never the file.
pub fn read_store_file(root: &Path) -> cli_core::Result<Option<StoreFile>> {
    let path = root.join(STORE_FILE);
    under_temp(&path);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return match vault_exists(root)? {
                false => Ok(None),
                true => Err(unreadable(
                    &path,
                    "it is missing, and the vault beside it exists".into(),
                )),
            };
        }
        Err(e) => return Err(unreadable(&path, e.to_string())),
    };
    let raw: RawStoreFile =
        serde_yaml::from_slice(&bytes).map_err(|e| unreadable(&path, e.to_string()))?;
    let file = check(&path, raw)?;
    if file.secrets == SecretsMode::Files && vault_exists(root)? {
        return Err(unreadable(
            &path,
            "it says the secrets are in files, and a vault exists beside it".into(),
        ));
    }
    Ok(Some(file))
}

/// Whether `<root>/vault` exists; "no" is `NotFound` only (T31). A dangling link exists.
pub(crate) fn vault_exists(root: &Path) -> cli_core::Result<bool> {
    let vault = root.join(VAULT_FILE);
    match std::fs::symlink_metadata(&vault) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(unreadable(
            &root.join(STORE_FILE),
            format!("the vault {} cannot be checked: {e}", vault.display()),
        )),
    }
}

fn check(path: &Path, raw: RawStoreFile) -> cli_core::Result<StoreFile> {
    if raw.format > STORE_FILE_FORMAT {
        return Err(unreadable(
            path,
            format!(
                "format {} is newer than this AppRafter reads ({STORE_FILE_FORMAT})",
                raw.format
            ),
        ));
    }
    if raw.format == 0 {
        return Err(unreadable(path, "format 0 is no format".into()));
    }
    let secrets = SecretsMode::parse(&raw.secrets).ok_or_else(|| {
        unreadable(
            path,
            format!(
                "`secrets: {:?}` is none of files, locking, locked, unlocking, rotating",
                raw.secrets
            ),
        )
    })?;
    let id = match raw.id.as_deref() {
        None => None,
        Some(text) => Some(StoreId::parse(text).ok_or_else(|| {
            unreadable(
                path,
                format!("`id: {text:?}` is not 16 lowercase hex characters"),
            )
        })?),
    };
    if secrets != SecretsMode::Files && id.is_none() {
        return Err(unreadable(
            path,
            format!(
                "it says the secrets are {}, and names no id",
                secrets.as_str()
            ),
        ));
    }
    let rotation = rotation_of(path, secrets, &raw)?;
    Ok(StoreFile {
        format: raw.format,
        secrets,
        id,
        slots: raw
            .slots
            .into_iter()
            .map(|s| SlotInfo {
                kind: LockSlotKind::parse(&s.kind),
                label: s.label,
            })
            .collect(),
        unlock_timeout_min: raw.unlock_timeout_min,
        rotation,
    })
}

/// `rotating_to` and `hello_drop` as `secrets: rotating` carries them (D.4b B63): its new
/// vault id is required, every slot id must be one. In every other state the fields mean
/// nothing and are not read.
fn rotation_of(
    path: &Path,
    secrets: SecretsMode,
    raw: &RawStoreFile,
) -> cli_core::Result<Option<Rotation>> {
    if secrets != SecretsMode::Rotating {
        return Ok(None);
    }
    let to = raw
        .rotating_to
        .as_deref()
        .and_then(hex16)
        .map(VaultId)
        .ok_or_else(|| {
            unreadable(
                path,
                "it says a key change did not finish, and names no new vault id \
                 (`rotating_to`: 32 lowercase hex characters)"
                    .into(),
            )
        })?;
    let hello_drop = raw
        .hello_drop
        .iter()
        .map(|text| {
            hex16(text).map(SlotId).ok_or_else(|| {
                unreadable(
                    path,
                    format!(
                        "`hello_drop` names {text:?}, which is no slot id (32 lowercase hex \
                         characters)"
                    ),
                )
            })
        })
        .collect::<cli_core::Result<Vec<_>>>()?;
    Ok(Some(Rotation { to, hello_drop }))
}

/// `text` as 16 bytes when it is exactly 32 lowercase hex characters — a vault or slot id
/// as its `Display` writes it.
fn hex16(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn unreadable(path: &Path, detail: String) -> CliError {
    CliError::StoreFileUnreadable {
        path: path.to_path_buf(),
        detail,
    }
}

/// Write `<root>/store.yaml` (mode 0644: it holds nothing secret), atomically, under the
/// store's write lock, which `held` must hold: every step of a lock flow is a
/// read-modify-write (T28). A lock state other than `files` without an id is a caller's bug,
/// refused before anything is written: [`read_store_file`] would refuse the file it made.
///
/// Durability: the rename is synced to disk best effort only (`cli_core::atomic_replace`
/// ignores a failed directory fsync). A lock flow whose next step relies on this file being on
/// disk — d4a-d's `lock on` / `lock off` (overview §8 risk 10) — syncs `root` itself after
/// this call and fails when that fails, as `secrets::locked` does before a marker.
pub fn write_store_file(root: &Path, file: &StoreFile, held: &StoreLock) -> cli_core::Result<()> {
    let path = root.join(STORE_FILE);
    under_temp(&path);
    if !held.is_held() {
        return Err(CliError::VaultNeedsWriteLock {
            store: root.to_path_buf(),
        });
    }
    if file.secrets != SecretsMode::Files && file.id.is_none() {
        return Err(CliError::Other(format!(
            "{}: a lock state other than `files` is written only with the store's id; nothing \
             was written",
            path.display()
        )));
    }
    // D.4b B63: `rotating` is written with its key change's fields, and no other state is.
    if (file.secrets == SecretsMode::Rotating) != file.rotation.is_some() {
        return Err(CliError::Other(format!(
            "{}: `secrets: rotating` is written with its key change's new vault id, and no \
             other lock state is; nothing was written",
            path.display()
        )));
    }
    let out = RawOut {
        format: file.format,
        secrets: file.secrets.as_str(),
        id: file.id.as_ref().map(StoreId::as_str),
        slots: file
            .slots
            .iter()
            .map(|s| RawSlotOut {
                kind: s.kind.as_str(),
                label: &s.label,
            })
            .collect(),
        unlock_timeout_min: file.unlock_timeout_min,
        rotating_to: file.rotation.as_ref().map(|r| r.to.to_string()),
        hello_drop: file
            .rotation
            .as_ref()
            .map(|r| r.hello_drop.iter().map(ToString::to_string).collect())
            .unwrap_or_default(),
    };
    let body = serde_yaml::to_string(&out)?;
    cli_core::atomic_replace(
        &path,
        format!("{HEADER}{body}").as_bytes(),
        ".store-",
        Some(0o644),
    )
    .map_err(|e| CliError::Io(io::Error::new(e.kind(), format!("{}: {e}", path.display()))))
}
