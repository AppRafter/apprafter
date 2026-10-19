// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the vault refuses, typed (D.4a overview §3.8). No variant carries a secret, and none
//! has a miette code: the `secrets` boundary maps each one to a `cli_core::CliError` under
//! `apprafter::secrets::*` (d4a-c2, overview §3.12). A failed authentication is always one of
//! these, never "no such secret" (overview §4 rule 3).

use std::fmt;
use std::path::PathBuf;

use super::format::{SlotId, SlotKind};
use super::kdf::Argon2idParams;

/// The result of every fallible vault operation.
pub type VaultResult<T> = Result<T, VaultError>;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The first eight bytes are not the vault's magic: not a vault, or a copy a text-mode
    /// transfer mangled (`\r\n` rewritten) — never read as a tampered vault.
    #[error("not an AppRafter vault file")]
    NotAVault,
    /// A format version this build does not read; `writer` names the program that wrote it.
    #[error("vault format {found} was written by {writer}; this build reads format 1")]
    UnsupportedVersion { found: u16, writer: String },
    /// A field breaks the format: a length, a bound, the order, a reserved bit, the end.
    #[error("the vault file is malformed: {what} (byte {offset})")]
    Malformed { what: MalformedWhat, offset: usize },
    /// No vault file where one must be: never an empty vault.
    #[error("no vault file at {}", path.display())]
    Missing { path: PathBuf },
    /// Every slot tried failed its key check: the secret is not one of theirs. `kinds` are
    /// the kinds tried, in order — one for a secret of one kind; for typed text, the
    /// lock-password slot's, then this computer's account-password slot's (B27).
    #[error("this {} does not open the vault", kinds_or(kinds))]
    WrongSecret { kinds: Vec<SlotKind> },
    /// The secret passed the slot's key check, but the wrapped data key does not decrypt.
    #[error("vault slot {slot} is damaged: the secret is right, the slot does not open")]
    SlotCorrupt { slot: SlotId },
    /// The data key is right and the payload does not authenticate: bytes were changed.
    #[error("the vault was changed after it was written: its contents do not authenticate")]
    Tampered,
    /// No slot of the secret's kind; `kinds` are the slots the vault has.
    #[error(
        "the vault has no slot this secret can open (its slots: {})",
        kinds_text(kinds)
    )]
    NoUsableSlot { kinds: Vec<SlotKind> },
    /// Argon2id parameters below the policy's floor, refused on create and on open (T22).
    #[error(
        "key-derivation parameters m={} KiB, t={}, p={} are below this build's floor \
         (m ≥ {floor_m_cost_kib} KiB and m·t ≥ {floor_m_times_t})",
        params.m_cost_kib, params.t_cost, params.p_cost
    )]
    KdfTooWeak {
        params: Argon2idParams,
        floor_m_cost_kib: u32,
        floor_m_times_t: u64,
    },
    /// Parameters above the ceiling, or ones Argon2id cannot run: refused before allocating.
    #[error(
        "key-derivation parameters m={} KiB, t={}, p={} are out of range",
        params.m_cost_kib, params.t_cost, params.p_cost
    )]
    KdfOutOfRange { params: Argon2idParams },
    /// The vault names another store than the `store.yaml` beside it.
    #[error("this vault belongs to target store {vault_store_id}, not {store_id}")]
    StoreMismatch {
        vault_store_id: String,
        store_id: String,
    },
    /// A held data key is for another vault (the vault was replaced): unlock again.
    #[error("the held data key is for another vault: unlock again")]
    KeyStale,
    /// The payload authenticated, and breaks a rule of its own.
    #[error("the vault's contents are invalid: {what}")]
    PayloadInvalid { what: PayloadWhat },
    /// The operating system's random generator failed; nothing was written.
    #[error("the system's random number generator failed, nothing was written: {detail}")]
    RandomUnavailable { detail: String },
    /// A write over one of the format's limits, refused before anything is written (W7).
    #[error("{what} exceeds its limit of {limit}")]
    TooLarge { what: SizeWhat, limit: u64 },
    /// The directory's filesystem has no hard links (FAT, exFAT), so a first vault cannot be
    /// written without the risk of replacing one (T27).
    #[error(
        "{} cannot hold a vault: its filesystem{} has no hard links, so a new vault cannot be \
         written there without the risk of replacing one",
        dir.display(),
        fs.as_deref().map(|f| format!(" ({f})")).unwrap_or_default()
    )]
    NoReplaceUnsupported { dir: PathBuf, fs: Option<String> },
    /// The vault's only recovery-key slot (ADR 0068 §3: a recovery key is always present).
    #[error("the vault's last recovery-key slot cannot be removed")]
    LastRecoverySlot,
    /// Removing the slot would leave no slot this build's policy can open (T63).
    #[error("removing that slot would leave no slot this build can open")]
    NoUsableSlotLeft,
    /// No slot with that id.
    #[error("the vault has no slot {slot}")]
    SlotNotFound { slot: SlotId },
    /// One slot id given to two new slots (`create_with`, B57): a caller's bug.
    #[error("slot id {slot} is given to two new slots")]
    SlotIdTaken { slot: SlotId },
    /// A new lock password breaks one of O2's three rules.
    #[error("a new lock password is refused: {0}")]
    PasswordRefused(PasswordProblem),
    /// An argument a vault cannot be written with.
    #[error("{0}")]
    InvalidInput(InputWhat),
    /// A vault write without the target store's write lock held (T28).
    #[error(
        "writing {} needs the target store's write lock, which this process does not hold",
        path.display()
    )]
    WriteLockNotHeld { path: PathBuf },
    /// A file operation failed.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A recovery key that does not parse, or is not the one shown.
    #[error(transparent)]
    RecoveryKey(#[from] RecoveryKeyError),
}

fn kinds_or(kinds: &[SlotKind]) -> String {
    kinds
        .iter()
        .map(SlotKind::to_string)
        .collect::<Vec<_>>()
        .join(" or ")
}

fn kinds_text(kinds: &[SlotKind]) -> String {
    kinds
        .iter()
        .map(SlotKind::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Which rule of the format a file breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedWhat {
    TooLarge,
    Truncated,
    TrailingBytes,
    WriterLength,
    WriterText,
    Flags,
    StoreIdLength,
    StoreIdText,
    SlotCount,
    RecordLength,
    BodyLength,
    KdfId,
    LabelLength,
    LabelText,
    DuplicateSlotId,
    PayloadLength,
}

impl fmt::Display for MalformedWhat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "the file is larger than 2 MiB",
            Self::Truncated => "the file ends early",
            Self::TrailingBytes => "bytes after the payload",
            Self::WriterLength => "the writer's length",
            Self::WriterText => "the writer is not printable ASCII",
            Self::Flags => "a reserved flag is set",
            Self::StoreIdLength => "the store id's length",
            Self::StoreIdText => "the store id is not printable ASCII",
            Self::SlotCount => "the slot count",
            Self::RecordLength => "a slot record's length",
            Self::BodyLength => "a slot body's length",
            Self::KdfId => "a slot's key-derivation id",
            Self::LabelLength => "a slot label's length",
            Self::LabelText => "a slot label is not text",
            Self::DuplicateSlotId => "two slots share an id",
            Self::PayloadLength => "the payload's length",
        })
    }
}

/// Which rule of the payload an authenticated payload breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadWhat {
    Truncated,
    TrailingBytes,
    EntryCount,
    KeyLength,
    KeyText,
    KeyOrder,
    ValueLength,
    AgeIdentityMissing,
    AgeIdentity,
    RetiringIdentity,
    HetznerToken,
    UnlockTimeout,
    TargetName,
    TargetExists,
}

impl fmt::Display for PayloadWhat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "the entries end early",
            Self::TrailingBytes => "bytes after the last entry",
            Self::EntryCount => "the entry count",
            Self::KeyLength => "an entry key's length",
            Self::KeyText => "an entry key's characters",
            Self::KeyOrder => "the entries are not in strictly ascending order",
            Self::ValueLength => "an entry value's length",
            Self::AgeIdentityMissing => "no age identity",
            Self::AgeIdentity => "the age identity does not parse",
            Self::RetiringIdentity => "the retiring age identity does not parse",
            Self::HetznerToken => "a Hetzner token is empty, too long or not text",
            Self::UnlockTimeout => "the unlock timeout is not a whole number of minutes, 0 to 1440",
            Self::TargetName => "a target name breaks the target-name rule",
            Self::TargetExists => "the new target name already has entries",
        })
    }
}

/// Which limit a write would break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeWhat {
    Payload,
    Entries,
    Key,
    Slots,
    File,
}

impl fmt::Display for SizeWhat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Payload => "the payload",
            Self::Entries => "the number of entries",
            Self::Key => "an entry key",
            Self::Slots => "the number of slots",
            Self::File => "the vault file",
        })
    }
}

/// An argument a vault cannot be written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputWhat {
    Label,
    StoreId,
    Writer,
    NoRecoverySlot,
    MachineId,
    OneSlotPerComputer,
    SlotIds,
    FirstSlots,
    RotationUnfinished,
    SameVaultId,
    OneRecoverySlot,
}

impl fmt::Display for InputWhat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Label => "a slot label is at most 64 bytes of text without control characters",
            Self::StoreId => "a store id is 1 to 64 printable ASCII characters without spaces",
            Self::Writer => "a vault's writer is 1 to 64 printable ASCII characters",
            Self::NoRecoverySlot => "a new vault needs a recovery-key slot",
            Self::MachineId => {
                "a machine id is 16 raw bytes or 32 lower-case hex characters, never all zero"
            }
            Self::OneSlotPerComputer => {
                "a vault holds at most one account-password slot per computer"
            }
            Self::SlotIds => "a vault made at plan time needs one minted id per new slot",
            Self::FirstSlots => {
                "a vault made at plan time holds exactly one recovery-key slot and another slot"
            }
            Self::RotationUnfinished => {
                "the vault already retires a store key: that key change is finished first"
            }
            Self::SameVaultId => "a rotated vault needs a vault id of its own",
            Self::OneRecoverySlot => "a rotated vault holds exactly one recovery-key slot",
        })
    }
}

/// The three rules a new lock password must meet (owner decision O2: no minimum length).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordProblem {
    Empty,
    TooLong { max: usize },
    RecoveryKeyShape,
}

impl fmt::Display for PasswordProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("it is empty"),
            Self::TooLong { max } => write!(f, "it is longer than {max} bytes"),
            Self::RecoveryKeyShape => {
                f.write_str("it has the shape of a recovery key, and a prompt would read it as one")
            }
        }
    }
}

/// Why typed recovery-key text is refused (T24, T25). A position counts the 58 data characters
/// after the prefix, from 0, with dashes and spaces left out; a group is 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryKeyError {
    #[error("no recovery key was entered")]
    Empty,
    #[error(
        "this is not an AppRafter recovery key: it does not start with APPRAFTER-RECOVERY-KEY-1"
    )]
    WrongPrefix,
    #[error(
        "character {} of the recovery key (group {}) is not used in recovery keys{}",
        position + 1,
        position / 5 + 1,
        lookalike_hint(*lookalike)
    )]
    InvalidChar {
        position: usize,
        /// The character typed, lower-cased, only when it is one of the four Bech32 leaves out
        /// (`1`, `b`, `i`, `o`): any other character of the typed secret is never repeated.
        lookalike: Option<char>,
    },
    #[error(
        "the recovery key is mistyped{}",
        group.map(|g| format!(" near group {g}")).unwrap_or_default()
    )]
    Checksum { group: Option<u8> },
    #[error("a recovery key has 58 characters after its prefix, in 12 groups")]
    WrongLength,
    #[error("that is not the recovery key shown")]
    Mismatch,
}

/// The look-alike typed and the character a person most likely meant: Bech32 leaves out `1`,
/// `b`, `i` and `o`.
fn lookalike_hint(lookalike: Option<char>) -> String {
    let Some(typed) = lookalike else {
        return String::new();
    };
    let meant = match typed {
        '1' | 'i' => 'l',
        'o' => '0',
        'b' => '8',
        _ => return String::new(),
    };
    format!(": `{typed}`; did you mean `{meant}`?")
}
