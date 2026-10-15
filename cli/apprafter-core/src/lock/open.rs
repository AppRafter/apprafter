// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Choosing a lock password and typing a recovery key (D.4a §3.9; O2, T25, T62): what the core
//! refuses, and why. `open`, `check_password` and `set_unlock_timeout` arrive with d4a-d.
//!
//! The password rules are the vault's, which `create` and `add_slot` enforce (c1's Deviation
//! 6): [`PasswordProblem`] (empty, too long, shaped like a recovery key — no minimum length,
//! O2), [`MAX_PASSWORD_BYTES`] and [`RECOMMENDED_PASSWORD_CHARS`] are re-exported from there,
//! one copy of each, and this module adds only their wire name and their words.

pub use crate::vault::{PasswordProblem, MAX_PASSWORD_BYTES, RECOMMENDED_PASSWORD_CHARS};

use crate::vault::RecoveryKeyError;
use serde::Serialize;

/// Advice on a new lock password, never a refusal: below [`RECOMMENDED_PASSWORD_CHARS`] Unicode
/// scalar values (D.4a O2: anyone who copies the vault guesses it offline, where Argon2id only
/// slows each guess) the CLI asks `Use it anyway? [y/N]`, the desktop shows its warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PasswordAdvice {
    /// `chars` Unicode scalar values, fewer than `recommended`.
    Short { chars: u32, recommended: u32 },
}

/// The wire name and the words of the vault's refusal of a new lock password; an inherent
/// `impl` of the vault's type, in the same crate.
impl PasswordProblem {
    /// Its name on the wire (`fields.reason`).
    pub fn as_str(self) -> &'static str {
        match self {
            PasswordProblem::Empty => "empty",
            PasswordProblem::TooLong { .. } => "too_long",
            PasswordProblem::RecoveryKeyShape => "recovery_key_shape",
        }
    }

    /// The refusal's message, which names no password.
    pub fn message(self) -> String {
        match self {
            PasswordProblem::Empty => "a lock password cannot be empty".to_string(),
            PasswordProblem::TooLong { max } => {
                format!("a lock password can be at most {max} bytes long")
            }
            PasswordProblem::RecoveryKeyShape => {
                "that password has the shape of a recovery key, and a lock password must not"
                    .to_string()
            }
        }
    }
}

/// Why a typed recovery key was refused: the vault's parse and comparison errors, and a lock-on
/// plan executed before its key was typed back (T25, critic C5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryKeyProblem {
    Empty,
    WrongPrefix,
    /// A character recovery keys do not use. `position` counts the 58 characters after the
    /// prefix from 0, dashes and spaces left out (c1's count; words show `position + 1`);
    /// `hint` is the look-alike a person most likely meant (`l`, `0` or `8`), never the
    /// character typed (T24).
    InvalidChar {
        position: usize,
        hint: Option<char>,
    },
    Checksum {
        group: Option<u8>,
    },
    WrongLength,
    Mismatch,
    NotConfirmed,
}

impl From<RecoveryKeyError> for RecoveryKeyProblem {
    fn from(e: RecoveryKeyError) -> Self {
        match e {
            RecoveryKeyError::Empty => RecoveryKeyProblem::Empty,
            RecoveryKeyError::WrongPrefix => RecoveryKeyProblem::WrongPrefix,
            RecoveryKeyError::InvalidChar {
                position,
                lookalike,
            } => RecoveryKeyProblem::InvalidChar {
                position,
                hint: lookalike.and_then(meant),
            },
            RecoveryKeyError::Checksum { group } => RecoveryKeyProblem::Checksum { group },
            RecoveryKeyError::WrongLength => RecoveryKeyProblem::WrongLength,
            RecoveryKeyError::Mismatch => RecoveryKeyProblem::Mismatch,
        }
    }
}

/// The character a person most likely meant when they typed `typed`, one of the four Bech32
/// leaves out — c1's `lookalike`, which is `Some` only for `1`, `b`, `i` and `o`, lower-cased
/// (its Deviation 7); the same pairs as c1's `lookalike_hint`.
fn meant(typed: char) -> Option<char> {
    match typed {
        '1' | 'i' => Some('l'),
        'o' => Some('0'),
        'b' => Some('8'),
        _ => None,
    }
}

impl RecoveryKeyProblem {
    /// Its name on the wire (`fields.reason`).
    pub fn as_str(self) -> &'static str {
        match self {
            RecoveryKeyProblem::Empty => "empty",
            RecoveryKeyProblem::WrongPrefix => "wrong_prefix",
            RecoveryKeyProblem::InvalidChar { .. } => "invalid_char",
            RecoveryKeyProblem::Checksum { .. } => "checksum",
            RecoveryKeyProblem::WrongLength => "wrong_length",
            RecoveryKeyProblem::Mismatch => "mismatch",
            RecoveryKeyProblem::NotConfirmed => "not_confirmed",
        }
    }

    /// The refusal's message, which never repeats what was typed.
    pub fn message(self) -> String {
        match self {
            RecoveryKeyProblem::Empty => "no recovery key was typed".to_string(),
            RecoveryKeyProblem::WrongPrefix => {
                "that does not start like a recovery key".to_string()
            }
            RecoveryKeyProblem::InvalidChar { position, .. } => format!(
                "character {} of the recovery key is not one a recovery key uses",
                position + 1
            ),
            RecoveryKeyProblem::Checksum { group: Some(group) } => {
                format!("the recovery key is mistyped near group {group}")
            }
            RecoveryKeyProblem::Checksum { group: None } => {
                "the recovery key is mistyped".to_string()
            }
            RecoveryKeyProblem::WrongLength => {
                "that is too short or too long for a recovery key".to_string()
            }
            RecoveryKeyProblem::Mismatch => "that is not the recovery key shown".to_string(),
            RecoveryKeyProblem::NotConfirmed => {
                "the recovery key was not typed back, so nothing was locked".to_string()
            }
        }
    }
}

use std::path::Path;
use std::sync::Arc;

use cli_core::target::TargetStorePaths;
use cli_core::CliError;

use crate::error::{CoreError, CoreResult};
use crate::secrets::{read_vault, vault_error, StoreId};
use crate::vault::{DataKey, KdfPolicy, SlotInput, SlotSecret, VaultError, VaultFile, VaultId};

/// A vault opened in this process from one prompt's input (D.4a §3.9): its data key, for the
/// vault it opens. Its `Debug` shows no key (`DataKey`'s own).
#[derive(Debug)]
pub struct OpenedVault {
    pub key: Arc<DataKey>,
    pub vault_id: VaultId,
    /// The store's default unlock window, from the vault (T33).
    pub unlock_timeout_min: u32,
}

/// Opens the vault of the store at `root`, whose `store.yaml` names `id`, with one prompt's
/// `input` under `policy`, in this process (T62): a recovery key tries the recovery slots,
/// anything else the password slots, and recovery-key-shaped input whose checksum fails is tried
/// as a password too and, when that fails, is `recovery_key_invalid` naming its group. A secret
/// no slot takes is one failed attempt (`unlock_failed`, `attempts: 1`), for the caller to
/// count. The vault is read and checked to be this store's first (`vault_unreadable`, T29).
pub fn open_at(
    root: &Path,
    id: &StoreId,
    input: SlotInput,
    policy: &KdfPolicy,
) -> CoreResult<OpenedVault> {
    let bytes = read_vault(&TargetStorePaths::for_root(root.to_path_buf()), id)?;
    let parsed = VaultFile::parse(&bytes).map_err(|e| failed(root, e))?;
    let opened = match input {
        SlotInput::RecoveryKey(key) => parsed.open(&SlotSecret::RecoveryKey(key), policy),
        SlotInput::Password(password) => parsed.open(&SlotSecret::Password(password), policy),
        SlotInput::MistypedRecoveryKey { as_password, error } => {
            match parsed.open(&SlotSecret::Password(as_password), policy) {
                Err(VaultError::WrongSecret { .. } | VaultError::NoUsableSlot { .. }) => {
                    return Err(CoreError::RecoveryKeyInvalid {
                        reason: error.into(),
                    })
                }
                other => other,
            }
        }
    }
    .map_err(|e| failed(root, e))?;
    let unlock_timeout_min = opened
        .payload()
        .unlock_timeout_min()
        .map_err(|e| failed(root, e))?;
    Ok(OpenedVault {
        key: opened.data_key().clone(),
        vault_id: opened.header().vault_id,
        unlock_timeout_min,
    })
}

/// A vault failure as the secret store words it, but a kind of secret the vault has no slot
/// for is a wrong secret (one attempt): the person typed something that opens nothing here.
fn failed(root: &Path, e: VaultError) -> CoreError {
    match e {
        VaultError::NoUsableSlot { .. } => CoreError::Cli(CliError::UnlockFailed {
            store: root.to_path_buf(),
            attempts: 1,
            stopped_before: None,
        }),
        other => CoreError::Cli(vault_error(root, other)),
    }
}

/// O2 (decided 2026-10-13): a new lock password is refused only when it is empty, longer than
/// [`MAX_PASSWORD_BYTES`] or shaped like a recovery key (`password_refused`); below
/// [`RECOMMENDED_PASSWORD_CHARS`] Unicode scalar values it is advice ([`PasswordAdvice::Short`]),
/// which the CLI asks about and the desktop shows — never a refusal.
pub fn check_password(
    password: &crate::context::SecretString,
) -> CoreResult<Option<PasswordAdvice>> {
    let accepted = crate::vault::check_new_password(password)
        .map_err(|reason| CoreError::LockPasswordRefused { reason })?;
    Ok(accepted.is_short().then(|| PasswordAdvice::Short {
        chars: u32::try_from(accepted.chars()).unwrap_or(u32::MAX),
        recommended: RECOMMENDED_PASSWORD_CHARS as u32,
    }))
}

/// `lock on --timeout <minutes>` on a locked store (D.4a T33, O11): the store's default unlock
/// window, written to its vault with a key this process opens — it asks even where a window is
/// open — under the store's write lock. The minutes set.
pub fn set_unlock_timeout(
    ctx: &crate::context::Context,
    minutes: u32,
    reporter: &dyn crate::report::Reporter,
) -> CoreResult<u32> {
    super::check_timeout(minutes)?;
    let secrets = ctx.secrets();
    let key = secrets.prepare_write()?;
    let held = crate::target::lock_store(ctx, reporter)?;
    secrets.set_unlock_timeout(&key, &held, minutes)?;
    Ok(minutes)
}

/// One slot of a store's vault, opened in this process by a secret of that slot's own kind
/// (D.4a rule 4; team decision 2026-10-13): what a resumed `lock on` needs twice — the
/// password slot and the recovery slot — before any clear datum goes. Made only by
/// [`open_slot_at`], so its kind is the slot that opened, never a caller's claim.
#[derive(Debug)]
pub struct SlotOpened {
    kind: crate::secrets::LockSlotKind,
    opened: OpenedVault,
}

impl SlotOpened {
    /// The kind of slot that opened the vault.
    pub fn kind(&self) -> crate::secrets::LockSlotKind {
        self.kind
    }

    /// The vault it opened: its data key, for that vault.
    pub fn opened(&self) -> &OpenedVault {
        &self.opened
    }
}

/// Opens the vault of the store at `root` (`id`) with `secret` as the secret of a `kind` slot,
/// and of no other kind: a lock password tries the password slots only, so a recovery key typed
/// there opens nothing; a recovery key is parsed first (`recovery_key_invalid` naming its group,
/// one attempt) and tries the recovery slots only. A secret no slot of that kind takes is
/// `unlock_failed`, one attempt, for the caller to count. Any other kind is the caller's
/// mistake: `usage_refused`.
pub fn open_slot_at(
    root: &Path,
    id: &StoreId,
    kind: crate::secrets::LockSlotKind,
    secret: crate::context::SecretString,
    policy: &KdfPolicy,
) -> CoreResult<SlotOpened> {
    use crate::secrets::LockSlotKind;
    let input = match kind {
        LockSlotKind::Password => SlotInput::Password(secret),
        LockSlotKind::RecoveryKey => SlotInput::RecoveryKey(
            crate::vault::RecoveryKey::parse(secret.expose())
                .map_err(|e| CoreError::RecoveryKeyInvalid { reason: e.into() })?,
        ),
        other => {
            return Err(CoreError::Cli(CliError::UsageRefused {
                message: format!(
                    "a `{}` slot is not opened with a typed secret",
                    other.as_str()
                ),
                help: "Open the vault with the lock password or the recovery key.".to_string(),
            }))
        }
    };
    Ok(SlotOpened {
        kind,
        opened: open_at(root, id, input, policy)?,
    })
}

#[cfg(test)]
mod check_password_tests {
    use super::*;
    use crate::context::SecretString;

    fn advice(text: &str) -> CoreResult<Option<PasswordAdvice>> {
        check_password(&SecretString::new(text))
    }

    fn a_recovery_key() -> String {
        crate::vault::RecoveryKey::generate(&mut crate::vault::OsRandom)
            .unwrap()
            .to_display()
            .expose()
            .to_string()
    }

    /// O2 (decided 2026-10-13): no minimum — under 15 Unicode scalar values is advice, never a
    /// refusal; refused only empty, over 1024 bytes, or shaped like a recovery key. The same
    /// table as the desktop's bun test (an emoji is one, a combining mark one more).
    #[test]
    fn a_short_password_is_advice_and_three_shapes_are_refused() {
        let short = |chars: u32| {
            Some(PasswordAdvice::Short {
                chars,
                recommended: 15,
            })
        };
        assert_eq!(advice(&"a".repeat(14)).unwrap(), short(14));
        assert_eq!(advice(&"a".repeat(15)).unwrap(), None);
        assert_eq!(advice(&"\u{1F600}".repeat(14)).unwrap(), short(14));
        assert_eq!(advice(&"e\u{301}".repeat(7)).unwrap(), short(14));
        assert_eq!(advice(&format!("{}x", "e\u{301}".repeat(7))).unwrap(), None);
        assert_eq!(advice("a").unwrap(), short(1));
        for (text, reason) in [
            (String::new(), PasswordProblem::Empty),
            ("a".repeat(1025), PasswordProblem::TooLong { max: 1024 }),
            (a_recovery_key(), PasswordProblem::RecoveryKeyShape),
        ] {
            assert!(
                matches!(advice(&text).unwrap_err(), CoreError::LockPasswordRefused { reason: r } if r == reason),
                "{reason:?}"
            );
        }
    }
}

#[cfg(test)]
mod open_at_tests {
    use super::*;
    use crate::context::SecretString;
    use crate::secrets::testing::locked_store;

    const TOKEN: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    /// The shown recovery key with its first data character swapped for another of the charset:
    /// still recovery-key shaped, its checksum broken.
    fn mistyped(shown: &str) -> String {
        let (prefix, data) = shown.split_at(shown.find('\n').unwrap() + 1);
        let mut chars: Vec<char> = data.chars().collect();
        chars[0] = if chars[0] == 'Q' { 'P' } else { 'Q' };
        format!("{prefix}{}", chars.into_iter().collect::<String>())
    }

    #[test]
    fn the_recovery_key_opens_the_vault_of_its_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN))]);
        let input = SlotInput::from_input(made.recovery_key().clone());
        let opened = open_at(&root, &made.store_id, input, KdfPolicy::active()).unwrap();
        assert_eq!(opened.vault_id, made.vault_id);
        assert_eq!(opened.key.expose(), made.data_key().expose());
        assert_eq!(opened.unlock_timeout_min, 5);
    }

    /// T62: a secret no slot takes — a wrong password, or a kind the vault has no slot of — is
    /// one failed attempt, for the caller to count; never damage, never "absent".
    #[test]
    fn a_wrong_secret_is_one_failed_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN))]);
        let input = SlotInput::from_input(SecretString::new("not the password"));
        match open_at(&root, &made.store_id, input, KdfPolicy::active()).unwrap_err() {
            CoreError::Cli(CliError::UnlockFailed {
                attempts, store, ..
            }) => assert_eq!((attempts, store), (1, root.clone())),
            e => panic!("expected one failed attempt, got {e:?}"),
        }
    }

    /// T62 (critic G4): a recovery key with a broken checksum names its group; it is one
    /// attempt too.
    #[test]
    fn a_mistyped_recovery_key_names_its_group() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN))]);
        let typed = SecretString::new(mistyped(made.recovery_key().expose()));
        match open_at(
            &root,
            &made.store_id,
            SlotInput::from_input(typed),
            KdfPolicy::active(),
        )
        .unwrap_err()
        {
            CoreError::RecoveryKeyInvalid {
                reason: RecoveryKeyProblem::Checksum { group: Some(_) },
            } => {}
            e => panic!("expected the group of the mistake, got {e:?}"),
        }
    }

    /// T29: the vault is checked to be this store's before any secret is tried.
    #[test]
    fn another_stores_vault_or_none_is_said_so_first() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let made_a = locked_store(&a, &[("prod", Some(TOKEN))]);
        let made_b = locked_store(&b, &[("prod", Some(TOKEN))]);
        std::fs::copy(b.join("vault"), a.join("vault")).unwrap();
        let input = SlotInput::from_input(made_b.recovery_key().clone());
        assert!(matches!(
            open_at(&a, &made_a.store_id, input, KdfPolicy::active()).unwrap_err(),
            CoreError::Cli(CliError::VaultUnreadable {
                problem: cli_core::VaultProblem::OtherStore,
                ..
            })
        ));
        std::fs::remove_file(a.join("vault")).unwrap();
        let input = SlotInput::from_input(made_a.recovery_key().clone());
        assert!(matches!(
            open_at(&a, &made_a.store_id, input, KdfPolicy::active()).unwrap_err(),
            CoreError::Cli(CliError::VaultUnreadable {
                problem: cli_core::VaultProblem::Missing,
                ..
            })
        ));
    }
}
