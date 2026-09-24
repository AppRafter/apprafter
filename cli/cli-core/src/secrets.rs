// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Thin wrapper around the `age` crate for AppRafter's at-rest
//! secret encryption.
//!
//! The CLI uses one X25519 identity per workstation. The private
//! key is stored bech32-encoded at `$APPRAFTER_AGE_KEY` (default
//! `~/.config/apprafter/age.key`, mode 0600). Ciphertext is
//! ASCII-armored so it stays human-eyeable inside `state.json`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::armor::{ArmoredReader, ArmoredWriter, Format};
use age::secrecy::ExposeSecret;
use age::x25519::{Identity, Recipient};

use crate::{CliError, Result};

/// Resolve the on-disk path for the age private key. Honours
/// `APPRAFTER_AGE_KEY`; falls back to `$HOME/.config/apprafter/age.key`.
pub fn default_age_key_path() -> PathBuf {
    if let Ok(p) = std::env::var("APPRAFTER_AGE_KEY") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("/"));
    Path::new(&home)
        .join(".config")
        .join("apprafter")
        .join("age.key")
}

/// Load the identity at `path`, or generate a fresh one and persist
/// it (parent dir created, file mode 0600 on Unix) when the file
/// is absent.
pub fn load_or_create_identity(path: &Path) -> Result<Identity> {
    if path.exists() {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| CliError::Other(format!("read age key {path:?}: {e}")))?;
        return Identity::from_str(raw.trim())
            .map_err(|e| CliError::Other(format!("parse age key {path:?}: {e}")));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CliError::Other(format!("mkdir {parent:?}: {e}")))?;
    }
    let identity = Identity::generate();
    let serialised = identity.to_string();
    write_secret_file(path, serialised.expose_secret().as_bytes())?;
    Ok(identity)
}

#[cfg(unix)]
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| CliError::Other(format!("create age key {path:?}: {e}")))?;
    f.write_all(bytes)
        .map_err(|e| CliError::Other(format!("write age key {path:?}: {e}")))?;
    f.write_all(b"\n").ok();
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::File::create(path)
        .map_err(|e| CliError::Other(format!("create age key {path:?}: {e}")))?;
    f.write_all(bytes)
        .map_err(|e| CliError::Other(format!("write age key {path:?}: {e}")))?;
    f.write_all(b"\n").ok();
    Ok(())
}

/// Encrypt `plaintext` for a single recipient. Returns ASCII-armored
/// ciphertext (`-----BEGIN AGE ENCRYPTED FILE-----` … `-----END …`).
pub fn encrypt_for_recipient(plaintext: &str, recipient: &Recipient) -> Result<String> {
    // age 0.11 changed `with_recipients` to take an iterator of `&dyn
    // Recipient` and to validate eagerly, returning Result instead of Option.
    // The recipient list here is a one-element literal, so the error arm is
    // unreachable in practice — it is mapped rather than unwrapped so a future
    // multi-recipient caller inherits the check instead of a panic.
    let recipient_ref: &dyn age::Recipient = recipient;
    let encryptor = age::Encryptor::with_recipients(std::iter::once(recipient_ref))
        .map_err(|e| CliError::Other(format!("age encryptor: {e}")))?;
    let mut out: Vec<u8> = Vec::new();
    let armored = ArmoredWriter::wrap_output(&mut out, Format::AsciiArmor)
        .map_err(|e| CliError::Other(format!("age armor: {e}")))?;
    let mut writer = encryptor
        .wrap_output(armored)
        .map_err(|e| CliError::Other(format!("age wrap: {e}")))?;
    writer
        .write_all(plaintext.as_bytes())
        .map_err(|e| CliError::Other(format!("age write: {e}")))?;
    let armored = writer
        .finish()
        .map_err(|e| CliError::Other(format!("age finish wrap: {e}")))?;
    armored
        .finish()
        .map_err(|e| CliError::Other(format!("age finish armor: {e}")))?;
    String::from_utf8(out).map_err(|e| CliError::Other(format!("age armor not utf-8: {e}")))
}

/// Decrypt ASCII-armored ciphertext with the given identity.
pub fn decrypt_with_identity(armored: &str, identity: &Identity) -> Result<String> {
    let reader = ArmoredReader::new(armored.as_bytes());
    // age 0.11 collapsed the `Decryptor::{Recipients,Passphrase}` enum into an
    // opaque struct; passphrase mode is now asked about rather than matched on.
    // `is_scrypt()` is the same question the old `Passphrase(_)` arm answered —
    // an scrypt recipient stanza IS the passphrase mode.
    let decryptor = age::Decryptor::new(reader)
        .map_err(|e| CliError::Other(format!("age decryptor init: {e}")))?;
    if decryptor.is_scrypt() {
        return Err(CliError::Other(
            "age ciphertext is passphrase-protected; AppRafter expects recipient mode".to_string(),
        ));
    }
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| CliError::Other(format!("age decrypt: {e}")))?;
    let mut out = String::new();
    reader
        .read_to_string(&mut out)
        .map_err(|e| CliError::Other(format!("age read plaintext: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_encrypts_and_decrypts() {
        let identity = Identity::generate();
        let recipient = identity.to_public();
        let armored = encrypt_for_recipient("hello apprafter", &recipient).unwrap();
        assert!(
            armored.starts_with("-----BEGIN AGE ENCRYPTED FILE-----"),
            "{armored}"
        );
        let plain = decrypt_with_identity(&armored, &identity).unwrap();
        assert_eq!(plain, "hello apprafter");
    }

    #[test]
    fn decrypt_with_wrong_identity_errors() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let armored = encrypt_for_recipient("for alice", &alice.to_public()).unwrap();
        let err = decrypt_with_identity(&armored, &bob).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("decrypt"), "{msg}");
    }

    #[test]
    fn load_or_create_generates_and_persists_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("age.key");
        let id1 = load_or_create_identity(&path).expect("create");
        assert!(path.exists());
        let id2 = load_or_create_identity(&path).expect("load");
        assert_eq!(id1.to_public().to_string(), id2.to_public().to_string());
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_writes_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("age.key");
        load_or_create_identity(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "got {mode:o}");
    }

    #[test]
    fn default_age_key_path_honours_env_override() {
        std::env::set_var("APPRAFTER_AGE_KEY", "/tmp/custom-age");
        assert_eq!(default_age_key_path(), PathBuf::from("/tmp/custom-age"));
        std::env::remove_var("APPRAFTER_AGE_KEY");
        let p = default_age_key_path();
        assert!(p.ends_with(".config/apprafter/age.key"), "{p:?}");
    }

    #[test]
    fn identity_serialises_and_parses_via_bech32() {
        let id = Identity::generate();
        let s = id.to_string();
        assert!(s.expose_secret().starts_with("AGE-SECRET-KEY-"));
        let parsed = Identity::from_str(s.expose_secret()).unwrap();
        assert_eq!(parsed.to_public().to_string(), id.to_public().to_string());
    }
}
