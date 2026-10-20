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
use crate::vault::MachineId;
use crate::vault::{DataKey, KdfPolicy, SlotInput, SlotSecret, VaultError, VaultFile, VaultId};

use super::SlotRef;

/// A vault opened in this process from one prompt's input (D.4a §3.9): its data key, for the
/// vault it opens, and the slot whose secret opened it. Its `Debug` shows no key (`DataKey`'s
/// own). Only [`open_at`] makes one (`opened_with` is private): a flow given an `OpenedVault`
/// knows a fresh secret opened the vault in this process — never a holder's key, never the
/// key an unlocked app holds (D.4b B19, B60).
#[derive(Debug)]
pub struct OpenedVault {
    pub key: Arc<DataKey>,
    pub vault_id: VaultId,
    /// The store's default unlock window, from the vault (T33).
    pub unlock_timeout_min: u32,
    opened_with: SlotRef,
}

impl OpenedVault {
    /// The slot whose secret opened the vault (B60: an outcome names it).
    pub fn opened_with(&self) -> &SlotRef {
        &self.opened_with
    }
}

/// Opens the vault of the store at `root`, whose `store.yaml` names `id`, with one prompt's
/// `input` under `policy`, in this process (T62): a recovery key tries the recovery slots; a
/// lock password the lock-password slots; the account password this computer's
/// account-password slot (B54); typed text the lock-password slot, then this computer's
/// account-password slot (B27, the prompt's input from d4b-c1 on); recovery-key-shaped input
/// whose checksum fails is tried as a lock password too and, when that fails, is
/// `recovery_key_invalid` naming its group. A secret
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
        SlotInput::AccountPassword(password, machine) => {
            parsed.open(&SlotSecret::AccountPassword(password, machine), policy)
        }
        SlotInput::Typed(typed, machine) => parsed.open_typed(&typed, machine.as_ref(), policy),
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
    let opened_with = opened
        .opened_by()
        .and_then(|id| opened.header().slots.iter().find(|slot| slot.id == id))
        .map(SlotRef::from)
        .ok_or_else(|| {
            CoreError::Cli(CliError::Other(
                "the vault opened, and no slot says it opened it".to_string(),
            ))
        })?;
    Ok(OpenedVault {
        key: opened.data_key().clone(),
        vault_id: opened.header().vault_id,
        unlock_timeout_min,
        opened_with,
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
/// [`open_slot_at`], so its kind is the slot that opened — the vault names it
/// (`OpenedVault::opened_with`, D.4b B56) — never a caller's claim.
#[derive(Debug)]
pub struct SlotOpened {
    opened: OpenedVault,
}

impl SlotOpened {
    /// The kind of slot that opened the vault.
    pub fn kind(&self) -> crate::secrets::LockSlotKind {
        self.opened.opened_with().kind
    }

    /// The vault it opened: its data key, for that vault.
    pub fn opened(&self) -> &OpenedVault {
        &self.opened
    }
}

/// Opens the vault of the store at `root` (`id`) with `secret` as the secret of a `kind` slot,
/// and of no other kind: a lock password tries the lock-password slot only, so a recovery key
/// or an account password typed there opens nothing; the account password tries this
/// computer's account-password slot only — with its machine id, `machine` (D.4b B56; without
/// one it is `account_password_refused`, never a try) — and asks the system nothing (B13); a
/// recovery key is parsed first (`recovery_key_invalid` naming its group, one attempt) and
/// tries the recovery slots only. A secret no slot of that kind takes is `unlock_failed`, one
/// attempt, for the caller to count. Any other kind is the caller's mistake: `usage_refused`.
pub fn open_slot_at(
    root: &Path,
    id: &StoreId,
    kind: crate::secrets::LockSlotKind,
    secret: crate::context::SecretString,
    machine: Option<MachineId>,
    policy: &KdfPolicy,
) -> CoreResult<SlotOpened> {
    use crate::secrets::LockSlotKind;
    let input = match kind {
        LockSlotKind::Password => SlotInput::Password(secret),
        LockSlotKind::OsPassword => {
            SlotInput::AccountPassword(secret, machine.ok_or_else(no_machine_id)?)
        }
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
        opened: open_at(root, id, input, policy)?,
    })
}

/// D.4b B54: an account-password slot opens only with this computer's machine id, which the
/// client could not read here.
fn no_machine_id() -> CoreError {
    CoreError::Cli(CliError::AccountPasswordRefused {
        reason: cli_core::AccountPasswordRefusal::Unavailable {
            what: cli_core::AccountCheckUnavailable::NoMachineId,
        },
        messages: Vec::new(),
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
    use crate::secrets::testing::{locked_store, LockedTestStore};
    use crate::vault::{MachineId, NewSlot, OsRandom, VAULT_FILE};
    use cli_core::StoreLock;

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
        let input = SlotInput::from_input(made.recovery_key().clone(), None);
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
        let input = SlotInput::from_input(SecretString::new("not the password"), None);
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
            SlotInput::from_input(typed, None),
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
        let input = SlotInput::from_input(made_b.recovery_key().clone(), None);
        assert!(matches!(
            open_at(&a, &made_a.store_id, input, KdfPolicy::active()).unwrap_err(),
            CoreError::Cli(CliError::VaultUnreadable {
                problem: cli_core::VaultProblem::OtherStore,
                ..
            })
        ));
        std::fs::remove_file(a.join("vault")).unwrap();
        let input = SlotInput::from_input(made_a.recovery_key().clone(), None);
        assert!(matches!(
            open_at(&a, &made_a.store_id, input, KdfPolicy::active()).unwrap_err(),
            CoreError::Cli(CliError::VaultUnreadable {
                problem: cli_core::VaultProblem::Missing,
                ..
            })
        ));
    }

    const ACCOUNT: &str = "hunter2 is not a password";

    fn here() -> MachineId {
        MachineId::new(&[0x0a; 16]).unwrap()
    }

    /// `made`'s vault with this computer's account-password slot added (fast costs), as d4b-c1's
    /// `lock slot add os-password` will add one.
    fn with_account_password(root: &Path, made: &LockedTestStore) {
        let path = root.join(VAULT_FILE);
        let bytes = crate::vault::file::read(&path).unwrap();
        let mut opened = VaultFile::parse(&bytes)
            .unwrap()
            .open_with_key(made.data_key(), made.vault_id)
            .unwrap();
        opened
            .add_slot(
                NewSlot::OsPassword {
                    secret: SecretString::new(ACCOUNT),
                    label: "account password of walker on box-a".into(),
                    enrolled_at: 1_760_000_000,
                    machine: here(),
                },
                &KdfPolicy::INSECURE_FAST,
                &mut OsRandom,
            )
            .unwrap();
        let sealed = opened.seal("apprafter 0.0.0-test", &mut OsRandom).unwrap();
        let held = StoreLock::exclusive(&TargetStorePaths::for_root(root.to_path_buf())).unwrap();
        crate::vault::file::replace(&path, &sealed, &held).unwrap();
    }

    fn one_failed_attempt(result: CoreResult<OpenedVault>) {
        match result.unwrap_err() {
            CoreError::Cli(CliError::UnlockFailed { attempts: 1, .. }) => {}
            e => panic!("expected one failed attempt, got {e:?}"),
        }
    }

    /// B27 (the prompt's input from d4b-c1 on): typed text opens this computer's
    /// account-password slot; without its machine id, or with another computer's, it is one
    /// failed attempt.
    #[test]
    fn typed_text_opens_this_computers_account_password_slot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN))]);
        with_account_password(&root, &made);
        let fast = &KdfPolicy::INSECURE_FAST;
        let typed = || SecretString::new(ACCOUNT);
        let input = SlotInput::Typed(typed(), Some(here()));
        let opened = open_at(&root, &made.store_id, input, fast).unwrap();
        assert_eq!(opened.key.expose(), made.data_key().expose());
        one_failed_attempt(open_at(
            &root,
            &made.store_id,
            SlotInput::Typed(typed(), None),
            fast,
        ));
        let elsewhere = MachineId::new(&[0x0b; 16]).unwrap();
        let input = SlotInput::Typed(typed(), Some(elsewhere));
        one_failed_attempt(open_at(&root, &made.store_id, input, fast));
    }

    /// Critic-plan M1, B56: a lock password's input never opens an account-password slot; the
    /// account password's opens this computer's slot only.
    #[test]
    fn the_account_password_input_opens_only_this_computers_slot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN))]);
        with_account_password(&root, &made);
        let fast = &KdfPolicy::INSECURE_FAST;
        let typed = || SecretString::new(ACCOUNT);
        one_failed_attempt(open_at(
            &root,
            &made.store_id,
            SlotInput::Password(typed()),
            fast,
        ));
        let input = SlotInput::AccountPassword(typed(), here());
        let opened = open_at(&root, &made.store_id, input, fast).unwrap();
        assert_eq!(opened.vault_id, made.vault_id);
        let elsewhere = MachineId::new(&[0x0b; 16]).unwrap();
        let input = SlotInput::AccountPassword(typed(), elsewhere);
        one_failed_attempt(open_at(&root, &made.store_id, input, fast));
    }

    /// B27 (d4b-c1): the prompt's input — whatever a client's prompt or field gets — opens
    /// this computer's account-password slot when the client knows the machine id, and is
    /// one failed attempt without it, as before.
    #[test]
    fn the_prompts_input_reaches_this_computers_account_password_slot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN))]);
        with_account_password(&root, &made);
        let fast = &KdfPolicy::INSECURE_FAST;
        let typed = || SecretString::new(ACCOUNT);
        let input = SlotInput::from_input(typed(), Some(here()));
        let opened = open_at(&root, &made.store_id, input, fast).unwrap();
        assert_eq!(opened.vault_id, made.vault_id);
        let input = SlotInput::from_input(typed(), None);
        one_failed_attempt(open_at(&root, &made.store_id, input, fast));
    }

    /// B60: an opened vault names the slot whose secret opened it — the recovery key's, this
    /// computer's account password's — by its id, kind and label.
    #[test]
    fn an_opened_vault_names_the_slot_whose_secret_opened_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN))]);
        with_account_password(&root, &made);
        let bytes = crate::vault::file::read(&root.join(VAULT_FILE)).unwrap();
        let ids: Vec<String> = VaultFile::parse(&bytes)
            .unwrap()
            .header()
            .slots
            .iter()
            .map(|slot| slot.id.to_string())
            .collect();
        let fast = &KdfPolicy::INSECURE_FAST;
        let input = SlotInput::from_input(made.recovery_key().clone(), None);
        let by_key = open_at(&root, &made.store_id, input, fast).unwrap();
        assert_eq!(
            by_key.opened_with(),
            &SlotRef {
                id: ids[0].clone(),
                kind: crate::secrets::LockSlotKind::RecoveryKey,
                label: "recovery key".into(),
            }
        );
        let input = SlotInput::from_input(SecretString::new(ACCOUNT), Some(here()));
        let by_account = open_at(&root, &made.store_id, input, fast).unwrap();
        assert_eq!(
            by_account.opened_with(),
            &SlotRef {
                id: ids[1].clone(),
                kind: crate::secrets::LockSlotKind::OsPassword,
                label: "account password of walker on box-a".into(),
            }
        );
    }
}
