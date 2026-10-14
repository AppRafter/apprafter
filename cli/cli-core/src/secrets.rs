// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Thin wrapper around the `age` crate for AppRafter's at-rest
//! secret encryption.
//!
//! The CLI uses one X25519 identity per workstation for every target store
//! whose secrets are in files. The private key is stored bech32-encoded at
//! `$APPRAFTER_AGE_KEY` (default `~/.config/apprafter/age.key`, mode 0600;
//! [`age_key_path_from`], which the shared core's `Context` resolves it
//! with). Ciphertext is ASCII-armored so it stays human-eyeable inside
//! `state.json`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::armor::{ArmoredReader, ArmoredWriter, Format};
use age::secrecy::ExposeSecret;
/// The age identity and recipient, re-exported so a crate that names them (the shared core's
/// secret store) needs no `age` dependency of its own.
pub use age::x25519::{Identity, Recipient};

use crate::{CliError, Result};

/// The variable that overrides the age key path.
pub const AGE_KEY_ENV: &str = "APPRAFTER_AGE_KEY";

/// A secret a target's state caches age-encrypted, in `state.json`'s `hetzner_cloud`. Every
/// one of them opens only with the key it was cached under: a new key reads none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedSecret {
    /// `kubeconfig_age`: the k3s kubeconfig, fetched from the node over SSH.
    Kubeconfig,
    /// `argocd_admin_password_age`: the Argo CD admin password, read from the cluster.
    ArgocdPassword,
}

impl CachedSecret {
    /// What it is, as a sentence names it.
    pub fn noun(self) -> &'static str {
        match self {
            CachedSecret::Kubeconfig => "kubeconfig",
            CachedSecret::ArgocdPassword => "Argo CD admin password",
        }
    }

    /// Its name on the wire (`fields.secret`).
    pub fn as_str(self) -> &'static str {
        match self {
            CachedSecret::Kubeconfig => "kubeconfig",
            CachedSecret::ArgocdPassword => "argocd_password",
        }
    }
}

/// The key a cached secret was tried with (D.4a T26, T38): the one shared age key file every
/// target store in files uses, or a locked store's own identity, which only its vault holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKeyKind {
    SharedFile,
    StoreVault,
}

impl CacheKeyKind {
    /// Its name on the wire (`fields.key`).
    pub fn as_str(self) -> &'static str {
        match self {
            CacheKeyKind::SharedFile => "shared_file",
            CacheKeyKind::StoreVault => "store_vault",
        }
    }
}

/// Why a cached secret did not open (D.4a §3.12): it was not encrypted to the key it was tried
/// with (age's `NoMatchingKeys`, or a passphrase-protected file), or it is damaged (anything
/// else age refuses: an unreadable header, a wrong MAC, a broken payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndecryptableWhy {
    NotForThisKey,
    Damaged,
}

impl UndecryptableWhy {
    /// Its name on the wire (`fields.reason`).
    pub fn as_str(self) -> &'static str {
        match self {
            UndecryptableWhy::NotForThisKey => "not_for_this_key",
            UndecryptableWhy::Damaged => "damaged",
        }
    }
}

/// A cached secret that did not open: why, and the age library's account, worded exactly as
/// [`decrypt_with_identity`]'s error has always worded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undecryptable {
    pub why: UndecryptableWhy,
    pub detail: String,
}

/// The age key path from explicit inputs: the override verbatim (even empty), else
/// `<home>/.config/apprafter/age.key`, else `/.config/apprafter/age.key`.
pub fn age_key_path_from(override_path: Option<&Path>, home: Option<&Path>) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    home.unwrap_or(Path::new("/"))
        .join(".config")
        .join("apprafter")
        .join("age.key")
}

/// The identity at `path`, or `None` when the file is absent. Never creates anything: a read
/// (doctor, a kubeconfig probe) must not mint a key that matches no ciphertext. On Windows a key
/// whose DACL is not owner-only (written before WI-472, or copied in) is made owner-only on the
/// handle it is read through ([`crate::owner_only::read_secret`], WI-473).
pub fn load_identity(path: &Path) -> Result<Option<Identity>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = crate::owner_only::read_secret(path)
        .and_then(|bytes| {
            String::from_utf8(bytes).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stream did not contain valid UTF-8",
                )
            })
        })
        .map_err(|e| CliError::Other(format!("read age key {path:?}: {e}")))?;
    Identity::from_str(raw.trim())
        .map(Some)
        .map_err(|e| CliError::Other(format!("parse age key {path:?}: {e}")))
}

/// Load the identity at `path`, or generate a fresh one and persist
/// it (parent dir created, file mode 0600 on Unix) when the file
/// is absent.
pub fn load_or_create_identity(path: &Path) -> Result<Identity> {
    load_or_create_identity_reporting(path).map(|(identity, _)| identity)
}

/// [`load_or_create_identity`], and whether this call created the key (WI-457 review #10).
///
/// The key is written whole beside its name and then given it by a move that refuses a name
/// already taken (WI-475), so a key that appears between the check and the create — the lost
/// one put back, or another process's — is loaded, never overwritten, and `false` says this
/// call made none: a caller that reports "a new key was created" asks this. A key file that
/// does not parse is the error that names it, whoever wrote it: never used, never replaced.
pub fn load_or_create_identity_reporting(path: &Path) -> Result<(Identity, bool)> {
    if let Some(identity) = load_identity(path)? {
        return Ok((identity, false));
    }
    match create_identity(path)? {
        Some(identity) => Ok((identity, true)),
        None => {
            let identity = load_identity(path)?.ok_or_else(|| {
                CliError::Other(format!("age key {path:?} vanished while it was created"))
            })?;
            Ok((identity, false))
        }
    }
}

/// Generate an identity and write it to `path` whole (parent dir created, owner-only before its
/// first byte: [`write_secret_file`]), or `None` when a file is already there: it is left as it
/// was.
fn create_identity(path: &Path) -> Result<Option<Identity>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CliError::Other(format!("mkdir {parent:?}: {e}")))?;
    }
    let identity = Identity::generate();
    let serialised = identity.to_string();
    match write_secret_file(path, serialised.expose_secret().as_bytes()) {
        Ok(()) => Ok(Some(identity)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(e) => Err(CliError::Other(format!("create age key {path:?}: {e}"))),
    }
}

/// Write a new secret file whole, owner-only before its first byte, and never over another
/// (WI-475). The bytes go to a temp file beside `path` (`.<name>.<random>.tmp`), made by
/// [`crate::owner_only::create_new`] — mode 0600 at its open on Unix, the protected one-ACE DACL
/// on its handle on Windows — and synced; then the temp file takes `path`'s name by a move that
/// refuses a name already taken (tempfile's `persist_noclobber`: `renameat2(RENAME_NOREPLACE)`
/// on Linux, `renamex_np(RENAME_EXCL)` on macOS, else `link(2)`; `MoveFileExW` without
/// `MOVEFILE_REPLACE_EXISTING` on Windows, which keeps the temp file's DACL). `AlreadyExists`
/// when a file is there: it is left as it was. On any failure the temp file is removed and
/// nothing has `path`'s name, so a reader finds no key or a whole one, never part of one.
fn write_secret_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut tmp = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".tmp")
        .make_in(dir, crate::owner_only::create_new)?;
    #[cfg(all(test, windows))]
    crate::owner_only::testing::record_before_first_byte(tmp.as_file());
    tmp.write_all(bytes)?;
    tmp.write_all(b"\n")?;
    #[cfg(test)]
    if KEY_WRITE_FAILS.with(std::cell::Cell::take) {
        return Err(std::io::Error::other("a forced write error"));
    }
    tmp.as_file().sync_all()?;
    #[cfg(test)]
    SEEN_BEFORE_PLACED.with(|seen| seen.set(Some(path.exists())));
    // Closed first: the move opens the file again, by name. A refused move hands the temp path
    // back in its error, and dropping that removes the temp file.
    tmp.into_temp_path()
        .persist_noclobber(path)
        .map_err(|e| e.error)?;
    // The new name is durable once the directory entry is. Best effort: a filesystem that
    // cannot sync a directory still made the move.
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// A test's forced write error: this thread's next key write fails once its temp file holds
    /// the bytes, as a write to a full disk does.
    static KEY_WRITE_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the key's own name existed once its bytes were written and synced, just before
    /// the temp file took the name: recorded by this thread's last key write.
    static SEEN_BEFORE_PLACED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
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
    decrypt_with_identity_reason(armored, identity).map_err(|u| CliError::Other(u.detail))
}

/// [`decrypt_with_identity`], telling a ciphertext for another key from a damaged one
/// ([`UndecryptableWhy`]). Only the shared core's secret store calls it (D.4a T16,
/// `apprafter-core/tests/guards.rs`).
pub fn decrypt_with_identity_reason(
    armored: &str,
    identity: &Identity,
) -> std::result::Result<String, Undecryptable> {
    let damaged = |detail: String| Undecryptable {
        why: UndecryptableWhy::Damaged,
        detail,
    };
    let reader = ArmoredReader::new(armored.as_bytes());
    // age 0.11 collapsed the `Decryptor::{Recipients,Passphrase}` enum into an
    // opaque struct; passphrase mode is now asked about rather than matched on.
    // `is_scrypt()` is the same question the old `Passphrase(_)` arm answered —
    // an scrypt recipient stanza IS the passphrase mode.
    let decryptor =
        age::Decryptor::new(reader).map_err(|e| damaged(format!("age decryptor init: {e}")))?;
    if decryptor.is_scrypt() {
        return Err(Undecryptable {
            why: UndecryptableWhy::NotForThisKey,
            detail: "age ciphertext is passphrase-protected; AppRafter expects recipient mode"
                .to_string(),
        });
    }
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| Undecryptable {
            why: match &e {
                age::DecryptError::NoMatchingKeys => UndecryptableWhy::NotForThisKey,
                _ => UndecryptableWhy::Damaged,
            },
            detail: format!("age decrypt: {e}"),
        })?;
    let mut out = String::new();
    reader
        .read_to_string(&mut out)
        .map_err(|e| damaged(format!("age read plaintext: {e}")))?;
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

    /// D.4a §3.12: a cache encrypted to another key is "not for this key", and its words are the
    /// ones `decrypt_with_identity` has always given (the goldens that show them stay as they are).
    #[test]
    fn a_cache_for_another_key_is_not_for_this_key_and_reads_as_before() {
        let a = Identity::generate();
        let b = Identity::generate();
        let armored = encrypt_for_recipient("x", &a.to_public()).unwrap();
        let why = decrypt_with_identity_reason(&armored, &b).unwrap_err();
        assert_eq!(why.why, UndecryptableWhy::NotForThisKey);
        assert!(why.detail.starts_with("age decrypt: "), "{}", why.detail);
        match decrypt_with_identity(&armored, &b) {
            Err(CliError::Other(text)) => assert_eq!(text, why.detail),
            other => panic!("{other:?}"),
        }
        assert_eq!(decrypt_with_identity_reason(&armored, &a).unwrap(), "x");
    }

    /// Anything age refuses but a missing key is damage: a file that is not age, and a payload
    /// with one character of its armour changed.
    #[test]
    fn a_damaged_cache_is_damaged() {
        let a = Identity::generate();
        let not_age = decrypt_with_identity_reason("not an age file", &a).unwrap_err();
        assert_eq!(not_age.why, UndecryptableWhy::Damaged);
        assert!(
            not_age.detail.starts_with("age decryptor init: "),
            "{}",
            not_age.detail
        );
        let armored = encrypt_for_recipient(&"x".repeat(600), &a.to_public()).unwrap();
        let mut lines: Vec<String> = armored.lines().map(str::to_string).collect();
        // The last armour line before `-----END …-----` is deep in the payload.
        let at = lines.len() - 2;
        let first = lines[at].remove(0);
        lines[at].insert(0, if first == 'A' { 'B' } else { 'A' });
        let broken = format!("{}\n", lines.join("\n"));
        let e = decrypt_with_identity_reason(&broken, &a).unwrap_err();
        assert_eq!(e.why, UndecryptableWhy::Damaged, "{}", e.detail);
    }

    /// A passphrase-protected file was not written by AppRafter: not for this key.
    #[test]
    fn a_passphrase_cache_is_not_for_this_key() {
        let mut r = age::scrypt::Recipient::new(age::secrecy::SecretString::from("pw".to_string()));
        r.set_work_factor(1);
        let enc =
            age::Encryptor::with_recipients(std::iter::once(&r as &dyn age::Recipient)).unwrap();
        let mut out = Vec::new();
        let mut w = enc
            .wrap_output(ArmoredWriter::wrap_output(&mut out, Format::AsciiArmor).unwrap())
            .unwrap();
        w.write_all(b"x").unwrap();
        w.finish().unwrap().finish().unwrap();
        let e =
            decrypt_with_identity_reason(std::str::from_utf8(&out).unwrap(), &Identity::generate())
                .unwrap_err();
        assert_eq!(e.why, UndecryptableWhy::NotForThisKey);
        assert_eq!(
            e.detail,
            "age ciphertext is passphrase-protected; AppRafter expects recipient mode"
        );
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

    /// WI-472: on Windows the age key is owner-only by its DACL — protected, one ACE, this
    /// user — before the key's first byte is written, whatever the folder hands down.
    #[cfg(windows)]
    #[test]
    fn load_or_create_writes_the_owner_only_dacl_before_the_key() {
        use crate::owner_only::testing;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("age.key");
        let _ = testing::take_before_first_byte();
        load_or_create_identity(&path).unwrap();
        testing::take_before_first_byte()
            .expect("the write recorded the key file's DACL")
            .unwrap()
            .assert_owner_only_file("age.key before its first byte");
        testing::of_path(&path)
            .unwrap()
            .assert_owner_only_file("age.key");
    }

    /// WI-473: on Windows an age key other accounts can read — written before WI-472, or
    /// copied in from another computer — is owner-only once read, and loads the same key.
    #[cfg(windows)]
    #[test]
    fn a_wide_age_key_is_owner_only_once_loaded() {
        use crate::owner_only::testing;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("age.key");
        let created = load_or_create_identity(&path).unwrap();
        testing::widen(&path).unwrap();
        assert!(
            !testing::of_path(&path).unwrap().is_owner_only(),
            "the test's premise"
        );
        let loaded = load_identity(&path).unwrap().expect("the key is there");
        assert_eq!(
            loaded.to_public().to_string(),
            created.to_public().to_string()
        );
        testing::of_path(&path)
            .unwrap()
            .assert_owner_only_file("age.key, loaded");
    }

    #[test]
    fn the_age_key_path_is_the_override_or_under_the_home() {
        assert_eq!(
            age_key_path_from(Some(Path::new("/k/age.key")), Some(Path::new("/h"))),
            PathBuf::from("/k/age.key")
        );
        assert_eq!(
            age_key_path_from(None, Some(Path::new("/h"))),
            PathBuf::from("/h/.config/apprafter/age.key")
        );
        assert_eq!(
            age_key_path_from(None, None),
            PathBuf::from("/.config/apprafter/age.key")
        );
        assert_eq!(
            age_key_path_from(Some(Path::new("")), None),
            PathBuf::from("")
        );
    }

    #[test]
    fn loading_an_absent_identity_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/age.key");
        assert!(load_identity(&path).unwrap().is_none());
        assert!(
            !path.exists() && !path.parent().unwrap().exists(),
            "a read never writes"
        );
    }

    #[test]
    fn a_present_identity_loads_as_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("age.key");
        let created = load_or_create_identity(&path).unwrap();
        let loaded = load_identity(&path).unwrap().expect("present");
        assert_eq!(
            loaded.to_public().to_string(),
            created.to_public().to_string()
        );
    }

    /// WI-457 review #10: whether the call created the key, for a caller that reports it.
    #[test]
    fn load_or_create_reports_whether_it_created_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k/age.key");
        let (made, created) = load_or_create_identity_reporting(&path).unwrap();
        assert!(created && path.exists());
        let (loaded, created) = load_or_create_identity_reporting(&path).unwrap();
        assert!(!created, "a key already there is loaded, not created");
        assert_eq!(loaded.to_public().to_string(), made.to_public().to_string());
    }

    /// The create itself never replaces a key that appeared after the check (`create_new`): it
    /// reports `None`, and the key there is left as it was.
    #[test]
    fn creating_where_a_key_appeared_leaves_it_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("age.key");
        let there = load_or_create_identity(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(create_identity(&path).unwrap().is_none());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let (loaded, created) = load_or_create_identity_reporting(&path).unwrap();
        assert!(!created);
        assert_eq!(
            loaded.to_public().to_string(),
            there.to_public().to_string()
        );
    }

    /// Names in `dir`.
    fn names_in(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// WI-475: a key whose write fails leaves no key behind, whole or partial, and no temp file;
    /// the next first use makes a key that loads.
    #[test]
    fn a_failed_key_write_leaves_no_key_and_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("age.key");
        KEY_WRITE_FAILS.with(|fails| fails.set(true));
        let Err(err) = load_or_create_identity_reporting(&path) else {
            panic!("a key whose write failed was handed out");
        };
        assert!(err.to_string().contains("a forced write error"), "{err}");
        assert!(!path.exists(), "a partial key was left at {path:?}");
        assert!(
            names_in(dir.path()).is_empty(),
            "{:?}",
            names_in(dir.path())
        );
        let (made, created) = load_or_create_identity_reporting(&path).unwrap();
        assert!(created);
        assert_eq!(
            load_identity(&path)
                .unwrap()
                .unwrap()
                .to_public()
                .to_string(),
            made.to_public().to_string()
        );
        assert_eq!(names_in(dir.path()), ["age.key"]);
    }

    /// WI-475: the key's name appears only once the key is whole and on disk: until then the
    /// bytes are in a temp file beside it.
    #[test]
    fn the_key_has_its_name_only_once_it_is_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("age.key");
        SEEN_BEFORE_PLACED.with(|seen| seen.set(None));
        load_or_create_identity(&path).unwrap();
        assert_eq!(
            SEEN_BEFORE_PLACED.with(|seen| seen.take()),
            Some(false),
            "the key's name existed before the key was whole (or the write site recorded nothing)"
        );
        assert_eq!(names_in(dir.path()), ["age.key"]);
    }

    /// WI-475: a key file that does not parse — a partial key, an empty file — is an error that
    /// names it, and it is left as it was: never reused as a key, never replaced by a new one.
    #[test]
    fn a_key_that_does_not_parse_is_reported_and_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let whole = Identity::generate().to_string();
        let partial = &whole.expose_secret()[..30];
        for (name, bytes) in [("partial", partial.as_bytes()), ("empty", b"".as_slice())] {
            let path = dir.path().join(name).join("age.key");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            let Err(err) = load_or_create_identity_reporting(&path) else {
                panic!("{name}: a key that does not parse was used");
            };
            let err = err.to_string();
            assert!(
                err.contains("parse age key") && err.contains(&format!("{path:?}")),
                "{name}: {err}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}: changed");
            assert!(load_identity(&path).is_err(), "{name}");
        }
    }

    /// WI-475: first uses that race all end with the one key one of them created: none sees a
    /// key half written, and none replaces another's.
    #[test]
    fn first_uses_that_race_agree_on_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("age.key");
        let start = std::sync::Arc::new(std::sync::Barrier::new(8));
        let runs: Vec<_> = (0..8)
            .map(|_| {
                let (path, start) = (path.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    load_or_create_identity_reporting(&path)
                        .map(|(id, created)| (id.to_public().to_string(), created))
                        .map_err(|e| e.to_string())
                })
            })
            .collect();
        let results: Vec<_> = runs.into_iter().map(|r| r.join().unwrap()).collect();
        let on_disk = load_identity(&path)
            .unwrap()
            .unwrap()
            .to_public()
            .to_string();
        for r in &results {
            assert_eq!(r.as_ref().map(|(key, _)| key), Ok(&on_disk), "{results:?}");
        }
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Ok((_, true))))
                .count(),
            1,
            "{results:?}"
        );
        assert_eq!(names_in(dir.path()), ["age.key"]);
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
