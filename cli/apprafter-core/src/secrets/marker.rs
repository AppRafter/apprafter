// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The `credentials.yaml` vault marker (ADR 0068 §5; D.4a T34, §3.6). On a locked store each
//! target that has a token keeps this in place of the token, naming the store whose vault holds
//! it. A release without the lock fails to read it ("invalid type: map, expected a string"), so
//! it stops there with no write and no secret printed. It is classified, never parsed into
//! `TargetCredentials`, and no message quotes it.
//!
//! Best effort, stated: `classify` reads the file into a YAML value, whose copy of a clear token
//! is not zeroized (as `cli_core`'s own parse of the file is not); the bytes read are.

use cli_core::target::TargetStorePaths;
use cli_core::CliError;
use serde::Serialize;
use serde_yaml::Value;

use super::files::under_temp;
use super::store_file::StoreId;

/// The comment every marker starts with. It names no version.
pub const MARKER_COMMENT: &str = "# The Hetzner API token of this target is in this target \
                                  store's vault (`apprafter lock on`).\n# AppRafter releases \
                                  without `apprafter lock` cannot read this file: update \
                                  AppRafter.\n";

/// What a `credentials.yaml` holds, told without keeping a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    /// No token recorded: an empty map, no `hetzner_token`, or a null one. (No file is the
    /// caller's: it reads nothing.)
    Absent,
    /// A marker: the token is in the vault of store `id`.
    Vault { id: StoreId },
    /// A token in clear: any YAML scalar, as `TargetCredentials` and the files backend read one
    /// (serde_yaml takes a number or a boolean as a string there).
    ClearToken,
    /// Anything else: not YAML, not a map, a marker that is not exactly `{vault: <id>}`.
    Unreadable,
}

/// What `bytes`, a `credentials.yaml`, holds.
pub fn classify(bytes: &[u8]) -> Marker {
    let Ok(doc) = serde_yaml::from_slice::<Value>(bytes) else {
        return Marker::Unreadable;
    };
    let map = match doc {
        Value::Null => return Marker::Absent,
        Value::Mapping(map) => map,
        _ => return Marker::Unreadable,
    };
    match map.get("hetzner_token") {
        None | Some(Value::Null) => Marker::Absent,
        Some(Value::String(_) | Value::Number(_) | Value::Bool(_)) => Marker::ClearToken,
        Some(Value::Mapping(inner)) if inner.len() == 1 => match inner.get("vault") {
            Some(Value::String(text)) => {
                StoreId::parse(text).map_or(Marker::Unreadable, |id| Marker::Vault { id })
            }
            _ => Marker::Unreadable,
        },
        Some(_) => Marker::Unreadable,
    }
}

#[derive(Serialize)]
struct MarkerOut<'a> {
    hetzner_token: VaultRef<'a>,
}

#[derive(Serialize)]
struct VaultRef<'a> {
    vault: &'a str,
}

/// The bytes of a marker for store `id`: the comment, then YAML that quotes the id where a
/// reader would otherwise take it for a number.
pub fn marker_bytes(id: &StoreId) -> cli_core::Result<String> {
    let body = serde_yaml::to_string(&MarkerOut {
        hetzner_token: VaultRef { vault: id.as_str() },
    })?;
    Ok(format!("{MARKER_COMMENT}{body}"))
}

/// Write `target`'s marker for store `id` in place of its `credentials.yaml`: atomically, mode
/// 0600 on Unix (Windows ignores the mode; the marker holds no secret), the target's directory
/// created as the files backend creates it. The caller holds the store's write lock.
pub fn write_marker(store: &TargetStorePaths, target: &str, id: &StoreId) -> cli_core::Result<()> {
    let dest = store.target_credentials_file(target);
    under_temp(&dest);
    let bytes = marker_bytes(id)?;
    let io = |source: std::io::Error| CliError::TargetFileIo {
        path: dest.clone(),
        target: target.to_string(),
        source,
    };
    std::fs::create_dir_all(store.target_dir(target)).map_err(io)?;
    cli_core::atomic_replace(&dest, bytes.as_bytes(), ".credentials", Some(0o600)).map_err(io)
}
