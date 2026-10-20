// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The holder's wire protocol, version 1 (ADR 0068 §4; D.4a §3.10, T58).
//!
//! A frame is `"ARH"` ‖ version u8 ‖ kind u8 ‖ flags u8 (`0`) ‖ body length u32 ‖ body, and the
//! body is fields, each `tag` u16 ‖ length u32 ‖ bytes; every integer is little-endian, as in the
//! vault, and a reader skips a tag it does not know. A body is at most [`MAX_FRAME`] bytes: a
//! request carries the whole vault (read-capped at 2 MiB) and an armored cache (capped at
//! [`MAX_ARMORED_CACHE`]).
//!
//! Two kinds carry a secret — `Init`, the data key on the inherited init pipe only, and
//! `Secret`, one opened item to a served client — and only [`write_init`], [`read_init`],
//! [`write_secret`] and [`read_reply`] touch them (HG5): the key is written from its own buffer
//! and read straight into the caller's, an item read into an exact-size `Zeroizing` buffer,
//! never through a whole-body `Vec` (S6). Every other frame is plain data. `End` and the
//! `Unsupported` refusal are frozen across versions, so any client can end any holder.

use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use apprafter_core::lock::{HolderHardening, HolderUnavailableReason};
use apprafter_core::vault::{DataKey, VaultId};
use apprafter_core::Zeroizing;
use cli_core::secrets::{CachedSecret, UndecryptableWhy};

pub const MAGIC: [u8; 3] = *b"ARH";
pub const PROTOCOL: u8 = 1;
pub const MAX_FRAME: u32 = 4 << 20;
pub const MAX_ARMORED_CACHE: usize = 1 << 20;
/// An init frame holds no vault: anything longer is not one.
const MAX_INIT: u32 = 64 << 10;
/// The version a holder of this build reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const HEADER_LEN: usize = 10;
const FIELD_LEN: usize = 6;
const ITEM_HETZNER_TOKEN: u8 = 0x01;
const UNSUPPORTED: u8 = 0x06;

/// Field tags.
pub mod tag {
    pub const PROTOCOL: u16 = 0x0001;
    pub const VERSION: u16 = 0x0002;
    pub const STORE_ID: u16 = 0x0003;
    pub const ROOT: u16 = 0x0004;
    pub const BASE: u16 = 0x0005;
    pub const VAULT_ID: u16 = 0x0006;
    pub const MINUTES: u16 = 0x0007;
    pub const WINDOW_SECS: u16 = 0x0008;
    pub const KEY: u16 = 0x0009;
    pub const PID: u16 = 0x000A;
    pub const ACK: u16 = 0x000B;
    pub const REASON: u16 = 0x000C;
    pub const CODE: u16 = 0x000D;
    pub const LEFT_SECS: u16 = 0x000E;
    pub const HARDENING: u16 = 0x000F;
    pub const VAULT: u16 = 0x0010;
    pub const ITEM: u16 = 0x0011;
    pub const TARGET: u16 = 0x0012;
    pub const ARMORED: u16 = 0x0013;
    pub const SECRET_KIND: u16 = 0x0014;
    pub const SECRET: u16 = 0x0015;
    pub const RECIPIENT: u16 = 0x0016;
    pub const REFUSAL: u16 = 0x0017;
    pub const DETAIL: u16 = 0x0018;
    pub const POLL_LEADER: u16 = 0x0019;
    pub const LIMIT: u16 = 0x001A;
}

/// What a frame is: its fifth byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Status = 0x01,
    OpenItem = 0x10,
    OpenCache = 0x11,
    Recipient = 0x12,
    /// Frozen across versions: any client ends any holder.
    End = 0x7F,
    /// The data key, CLI → holder, on the inherited init pipe only (S6).
    Init = 0x40,
    Ack = 0x41,
    StatusReply = 0x81,
    /// One opened item, holder → a served client (S6).
    Secret = 0x90,
    Absent = 0x91,
    RecipientReply = 0x92,
    Refused = 0xE0,
    Ended = 0xFF,
}

impl Kind {
    /// The kind `byte` names, if any.
    pub fn from_byte(byte: u8) -> Option<Kind> {
        Some(match byte {
            0x01 => Kind::Status,
            0x10 => Kind::OpenItem,
            0x11 => Kind::OpenCache,
            0x12 => Kind::Recipient,
            0x7F => Kind::End,
            0x40 => Kind::Init,
            0x41 => Kind::Ack,
            0x81 => Kind::StatusReply,
            0x90 => Kind::Secret,
            0x91 => Kind::Absent,
            0x92 => Kind::RecipientReply,
            0xE0 => Kind::Refused,
            0xFF => Kind::Ended,
            _ => return None,
        })
    }

    /// Whether a frame of this kind carries a secret (HG5).
    pub fn carries_secret(self) -> bool {
        matches!(self, Kind::Init | Kind::Secret)
    }
}

/// The store a holder serves: `store.yaml`'s id and the canonical store root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreBinding {
    pub id: String,
    pub root: PathBuf,
}

/// The init frame without its key (the key is read straight into the holder's locked page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitHeader {
    pub store: StoreBinding,
    /// Unix: the directory of the holder's socket (`Context::holder_base`); Windows: where it
    /// records its pid (`Context::runtime_dir`).
    pub base: PathBuf,
    pub vault_id: VaultId,
    pub minutes: u32,
    /// Tests only: a window of seconds. A release CLI never sends it (T58).
    pub window_secs: Option<u32>,
    /// Tests only: watch the session leader by polling, as without `pidfd_open` (risk 6).
    pub poll_leader: bool,
    /// The process that started the holder: on Windows its console must list it.
    pub client_pid: u32,
}

/// What a client may ask a holder to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderItem {
    HetznerToken { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Status,
    OpenItem {
        vault: Vec<u8>,
        item: HolderItem,
    },
    OpenCache {
        vault: Vec<u8>,
        secret: CachedSecret,
        target: String,
        armored: Vec<u8>,
    },
    Recipient {
        vault: Vec<u8>,
    },
    End,
}

/// A holder's answer. `Item` is one opened item; its `Debug` never shows it.
#[derive(PartialEq, Eq)]
pub enum Reply {
    Status(HolderStatus),
    Item(Zeroizing<Vec<u8>>),
    Absent,
    Recipient(String),
    Ended,
    Refused(Refusal),
}

impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reply::Status(s) => f.debug_tuple("Status").field(s).finish(),
            Reply::Item(_) => f.write_str("Item(..)"),
            Reply::Absent => f.write_str("Absent"),
            Reply::Recipient(r) => f.debug_tuple("Recipient").field(r).finish(),
            Reply::Ended => f.write_str("Ended"),
            Reply::Refused(r) => f.debug_tuple("Refused").field(r).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderStatus {
    pub protocol: u8,
    pub version: String,
    pub store_id: String,
    pub vault_id: VaultId,
    pub left_secs: u64,
    pub minutes: u32,
    pub hardening: HolderHardening,
}

/// Why a holder did not serve a request (§3.10; the client's reaction to each is T53's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotThisTerminal,
    WrongStore,
    KeyMismatch,
    /// The window expired or the machine slept; the holder exits after replying.
    WindowEnded,
    Malformed,
    /// Frozen across versions.
    Unsupported {
        holder_protocol: u8,
    },
    VaultDamaged,
    /// The cache does not open with the store's key: a verdict on the cache, never the vault.
    CacheRefused {
        reason: UndecryptableWhy,
        detail: String,
    },
}

impl Refusal {
    pub fn code(&self) -> u8 {
        match self {
            Refusal::NotThisTerminal => 0x01,
            Refusal::WrongStore => 0x02,
            Refusal::KeyMismatch => 0x03,
            Refusal::WindowEnded => 0x04,
            Refusal::Malformed => 0x05,
            Refusal::Unsupported { .. } => UNSUPPORTED,
            Refusal::VaultDamaged => 0x07,
            Refusal::CacheRefused { .. } => 0x08,
        }
    }

    /// Its name, as tests and notes show it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Refusal::NotThisTerminal => "not_this_terminal",
            Refusal::WrongStore => "wrong_store",
            Refusal::KeyMismatch => "key_mismatch",
            Refusal::WindowEnded => "window_ended",
            Refusal::Malformed => "malformed",
            Refusal::Unsupported { .. } => "unsupported",
            Refusal::VaultDamaged => "vault_damaged",
            Refusal::CacheRefused { .. } => "cache_refused",
        }
    }
}

/// Why no holder serves this terminal (T49–T52, T59).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderUnavailable {
    NoRuntimeDir,
    UnsafeRuntimeDir { path: PathBuf, reason: String },
    PathTooLong { len: usize, max: usize },
    NoSessionLeader,
    NoConsole { code: u32 },
    NotResponding { pid: Option<u32> },
    Timeout,
}

impl HolderUnavailable {
    /// The frozen reason a report carries.
    pub fn reason(&self) -> HolderUnavailableReason {
        match self {
            HolderUnavailable::NoRuntimeDir => HolderUnavailableReason::NoRuntimeDir,
            HolderUnavailable::UnsafeRuntimeDir { .. } => HolderUnavailableReason::UnsafeRuntimeDir,
            HolderUnavailable::PathTooLong { .. } => HolderUnavailableReason::PathTooLong,
            HolderUnavailable::NoSessionLeader => HolderUnavailableReason::NoSessionLeader,
            HolderUnavailable::NoConsole { .. } => HolderUnavailableReason::NoConsole,
            HolderUnavailable::NotResponding { .. } => HolderUnavailableReason::NotResponding,
            HolderUnavailable::Timeout => HolderUnavailableReason::Timeout,
        }
    }

    /// One clause for a person: why this terminal has no unlock window.
    pub fn describe(&self) -> String {
        match self {
            HolderUnavailable::NoRuntimeDir => {
                "no directory to keep it in (XDG_RUNTIME_DIR and TMPDIR are unset)".into()
            }
            HolderUnavailable::UnsafeRuntimeDir { path, reason } => {
                format!("{} is no private directory: {reason}", path.display())
            }
            HolderUnavailable::PathTooLong { len, max } => {
                format!("its socket path would be {len} bytes, and this OS allows {max}")
            }
            HolderUnavailable::NoSessionLeader => {
                "this terminal's session has no leader to bind it to".into()
            }
            HolderUnavailable::NoConsole { code } => {
                format!("there is no console to bind it to (error {code})")
            }
            HolderUnavailable::NotResponding { pid: Some(pid) } => {
                format!("its holder (pid {pid}) does not answer; `apprafter lock now` ends it")
            }
            HolderUnavailable::NotResponding { pid: None } => {
                "its holder does not answer; `apprafter lock now` ends it".into()
            }
            HolderUnavailable::Timeout => "the holder did not start within 5 s".into(),
        }
    }
}

/// A new holder's answer to its starter, on the ack pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ack {
    Ready {
        pid: u32,
        protocol: u8,
        version: String,
    },
    AlreadyRunning {
        pid: u32,
    },
    Unavailable(HolderUnavailable),
}

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    BadMagic,
    Version(u8),
    Flags(u8),
    UnknownKind(u8),
    Unexpected(u8),
    TooLarge(u32),
    Truncated,
    Field(u16),
    Missing(u16),
}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        FrameError::Io(e)
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "holder I/O: {e}"),
            FrameError::BadMagic => f.write_str("not a holder frame"),
            FrameError::Version(v) => {
                write!(f, "holder protocol {v}, this build speaks {PROTOCOL}")
            }
            FrameError::Flags(b) => write!(f, "frame flags {b:#04x}"),
            FrameError::UnknownKind(k) => write!(f, "unknown frame kind {k:#04x}"),
            FrameError::Unexpected(k) => write!(f, "frame kind {k:#04x} here"),
            FrameError::TooLarge(n) => write!(f, "a frame of {n} bytes"),
            FrameError::Truncated => f.write_str("a truncated frame"),
            FrameError::Field(t) => write!(f, "field {t:#06x} is malformed"),
            FrameError::Missing(t) => write!(f, "field {t:#06x} is missing"),
        }
    }
}

impl std::error::Error for FrameError {}

// ---------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------

fn header(kind: Kind, len: u32) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..3].copy_from_slice(&MAGIC);
    h[3] = PROTOCOL;
    h[4] = kind as u8;
    h[6..].copy_from_slice(&len.to_le_bytes());
    h
}

fn field_header(tag: u16, len: usize) -> [u8; FIELD_LEN] {
    let mut h = [0u8; FIELD_LEN];
    h[..2].copy_from_slice(&tag.to_le_bytes());
    h[2..].copy_from_slice(&(len as u32).to_le_bytes());
    h
}

fn frame_len(len: usize) -> io::Result<u32> {
    u32::try_from(len)
        .ok()
        .filter(|l| *l <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a holder frame over 4 MiB"))
}

/// Plain fields, in the order they are written.
#[derive(Default)]
struct Fields(Vec<(u16, Vec<u8>)>);

impl Fields {
    fn put(mut self, tag: u16, value: Vec<u8>) -> Self {
        self.0.push((tag, value));
        self
    }
    fn byte(self, tag: u16, v: u8) -> Self {
        self.put(tag, vec![v])
    }
    fn word(self, tag: u16, v: u32) -> Self {
        self.put(tag, v.to_le_bytes().to_vec())
    }
    fn quad(self, tag: u16, v: u64) -> Self {
        self.put(tag, v.to_le_bytes().to_vec())
    }
    fn text(self, tag: u16, v: &str) -> Self {
        self.put(tag, v.as_bytes().to_vec())
    }
    fn bytes(self, tag: u16, v: &[u8]) -> Self {
        self.put(tag, v.to_vec())
    }
    fn body_len(&self) -> usize {
        self.0.iter().map(|(_, v)| FIELD_LEN + v.len()).sum()
    }
}

/// A frame of plain data (never a secret-bearing kind), written in one call.
fn write_plain(w: &mut impl Write, kind: Kind, fields: &Fields) -> io::Result<()> {
    if kind.carries_secret() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a secret-bearing frame has its own writer",
        ));
    }
    let len = frame_len(fields.body_len())?;
    let mut out = Vec::with_capacity(HEADER_LEN + len as usize);
    out.extend_from_slice(&header(kind, len));
    for (t, v) in &fields.0 {
        out.extend_from_slice(&field_header(*t, v.len()));
        out.extend_from_slice(v);
    }
    w.write_all(&out)?;
    w.flush()
}

/// A path as bytes: Unix its bytes, Windows its UTF-16 little-endian.
#[cfg(unix)]
pub(crate) fn path_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
pub(crate) fn path_bytes(p: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(unix)]
fn path_from(bytes: &[u8], t: u16) -> Result<PathBuf, FrameError> {
    use std::os::unix::ffi::OsStrExt;
    let _ = t;
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(windows)]
fn path_from(bytes: &[u8], t: u16) -> Result<PathBuf, FrameError> {
    use std::os::windows::ffi::OsStringExt;
    let (pairs, odd) = bytes.as_chunks::<2>();
    if !odd.is_empty() {
        return Err(FrameError::Field(t));
    }
    let wide: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
}

fn secret_code(s: CachedSecret) -> u8 {
    match s {
        CachedSecret::Kubeconfig => 1,
        CachedSecret::ArgocdPassword => 2,
    }
}

fn secret_of(b: u8) -> Result<CachedSecret, FrameError> {
    match b {
        1 => Ok(CachedSecret::Kubeconfig),
        2 => Ok(CachedSecret::ArgocdPassword),
        _ => Err(FrameError::Field(tag::SECRET_KIND)),
    }
}

fn why_code(w: UndecryptableWhy) -> u8 {
    match w {
        UndecryptableWhy::NotForThisKey => 1,
        UndecryptableWhy::Damaged => 2,
        // D.4b B86: the secret store words a refusal so from the state's record after the
        // session answered; no holder sends it. Here so every reason has its own byte.
        UndecryptableWhy::RetiredKey => 3,
    }
}

fn why_of(b: u8) -> Result<UndecryptableWhy, FrameError> {
    match b {
        1 => Ok(UndecryptableWhy::NotForThisKey),
        2 => Ok(UndecryptableWhy::Damaged),
        3 => Ok(UndecryptableWhy::RetiredKey),
        _ => Err(FrameError::Field(tag::REASON)),
    }
}

fn reason_code(r: HolderUnavailableReason) -> u8 {
    match r {
        HolderUnavailableReason::NoRuntimeDir => 1,
        HolderUnavailableReason::UnsafeRuntimeDir => 2,
        HolderUnavailableReason::PathTooLong => 3,
        HolderUnavailableReason::NoSessionLeader => 4,
        HolderUnavailableReason::NoConsole => 5,
        HolderUnavailableReason::NotResponding => 6,
        HolderUnavailableReason::Timeout => 7,
    }
}

/// A holder's protections as two bitmasks — applied, applicable — bit 0 not dumpable, 1 deny
/// attach, 2 no core dumps, 3 memory locked, 4 process DACL.
fn hardening_bits(h: &HolderHardening) -> [u8; 2] {
    let bit = |v: Option<bool>, i: u8| (u8::from(v == Some(true)) << i, u8::from(v.is_some()) << i);
    [
        bit(h.non_dumpable, 0),
        bit(h.deny_attach, 1),
        bit(Some(h.core_off), 2),
        bit(Some(h.memory_locked), 3),
        bit(h.process_dacl, 4),
    ]
    .iter()
    .fold([0, 0], |[a, b], (x, y)| [a | x, b | y])
}

fn hardening_from(bits: &[u8]) -> Result<HolderHardening, FrameError> {
    let [applied, applicable] =
        <[u8; 2]>::try_from(bits).map_err(|_| FrameError::Field(tag::HARDENING))?;
    let get = |i: u8| (applicable >> i & 1 == 1).then_some(applied >> i & 1 == 1);
    Ok(HolderHardening {
        non_dumpable: get(0),
        deny_attach: get(1),
        core_off: applied >> 2 & 1 == 1,
        memory_locked: applied >> 3 & 1 == 1,
        process_dacl: get(4),
    })
}

// ---------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------

fn read_header(r: &mut impl Read) -> Result<(u8, u8, u32), FrameError> {
    let mut h = [0u8; HEADER_LEN];
    r.read_exact(&mut h)?;
    if h[..3] != MAGIC {
        return Err(FrameError::BadMagic);
    }
    if h[5] != 0 {
        return Err(FrameError::Flags(h[5]));
    }
    let len = u32::from_le_bytes([h[6], h[7], h[8], h[9]]);
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    Ok((h[3], h[4], len))
}

fn read_body(r: &mut impl Read, len: u32) -> Result<Vec<u8>, FrameError> {
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// A plain body's fields, borrowed from it.
struct Parsed<'a>(Vec<(u16, &'a [u8])>);

fn parse_body(body: &[u8]) -> Result<Parsed<'_>, FrameError> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < body.len() {
        let head = body.get(at..at + FIELD_LEN).ok_or(FrameError::Truncated)?;
        let t = u16::from_le_bytes([head[0], head[1]]);
        let n = u32::from_le_bytes([head[2], head[3], head[4], head[5]]) as usize;
        let start = at + FIELD_LEN;
        let end = start.checked_add(n).ok_or(FrameError::Truncated)?;
        out.push((t, body.get(start..end).ok_or(FrameError::Truncated)?));
        at = end;
    }
    Ok(Parsed(out))
}

impl<'a> Parsed<'a> {
    fn get(&self, t: u16) -> Option<&'a [u8]> {
        self.0.iter().find(|(x, _)| *x == t).map(|(_, v)| *v)
    }
    fn need(&self, t: u16) -> Result<&'a [u8], FrameError> {
        self.get(t).ok_or(FrameError::Missing(t))
    }
    fn byte(&self, t: u16) -> Result<u8, FrameError> {
        match self.need(t)? {
            [b] => Ok(*b),
            _ => Err(FrameError::Field(t)),
        }
    }
    fn word(&self, t: u16) -> Result<u32, FrameError> {
        <[u8; 4]>::try_from(self.need(t)?)
            .map(u32::from_le_bytes)
            .map_err(|_| FrameError::Field(t))
    }
    fn opt_word(&self, t: u16) -> Result<Option<u32>, FrameError> {
        self.get(t)
            .map(|v| {
                <[u8; 4]>::try_from(v)
                    .map(u32::from_le_bytes)
                    .map_err(|_| FrameError::Field(t))
            })
            .transpose()
    }
    fn quad(&self, t: u16) -> Result<u64, FrameError> {
        <[u8; 8]>::try_from(self.need(t)?)
            .map(u64::from_le_bytes)
            .map_err(|_| FrameError::Field(t))
    }
    fn text(&self, t: u16) -> Result<String, FrameError> {
        String::from_utf8(self.need(t)?.to_vec()).map_err(|_| FrameError::Field(t))
    }
    fn bytes(&self, t: u16) -> Result<Vec<u8>, FrameError> {
        Ok(self.need(t)?.to_vec())
    }
    fn vault_id(&self) -> Result<VaultId, FrameError> {
        <[u8; 16]>::try_from(self.need(tag::VAULT_ID)?)
            .map(VaultId)
            .map_err(|_| FrameError::Field(tag::VAULT_ID))
    }
}

// ---------------------------------------------------------------------
// Requests (client → holder)
// ---------------------------------------------------------------------

pub fn write_request(w: &mut impl Write, req: &Request) -> io::Result<()> {
    let (kind, fields) = match req {
        Request::Status => (Kind::Status, Fields::default()),
        Request::End => (Kind::End, Fields::default()),
        Request::OpenItem {
            vault,
            item: HolderItem::HetznerToken { target },
        } => (
            Kind::OpenItem,
            Fields::default()
                .bytes(tag::VAULT, vault)
                .byte(tag::ITEM, ITEM_HETZNER_TOKEN)
                .text(tag::TARGET, target),
        ),
        Request::OpenCache {
            vault,
            secret,
            target,
            armored,
        } => (
            Kind::OpenCache,
            Fields::default()
                .bytes(tag::VAULT, vault)
                .byte(tag::SECRET_KIND, secret_code(*secret))
                .text(tag::TARGET, target)
                .bytes(tag::ARMORED, armored),
        ),
        Request::Recipient { vault } => {
            (Kind::Recipient, Fields::default().bytes(tag::VAULT, vault))
        }
    };
    write_plain(w, kind, &fields)
}

/// A holder reads one request. Another version is refused (`Version`) but for `End`, which any
/// version may send; a secret-bearing kind is never a request.
pub fn read_request(r: &mut impl Read) -> Result<Request, FrameError> {
    let (version, byte, len) = read_header(r)?;
    let kind = Kind::from_byte(byte).ok_or(FrameError::UnknownKind(byte))?;
    if version != PROTOCOL {
        if kind == Kind::End {
            read_body(r, len)?;
            return Ok(Request::End);
        }
        return Err(FrameError::Version(version));
    }
    if kind.carries_secret() {
        return Err(FrameError::Unexpected(byte));
    }
    let body = read_body(r, len)?;
    let p = parse_body(&body)?;
    match kind {
        Kind::Status => Ok(Request::Status),
        Kind::End => Ok(Request::End),
        Kind::OpenItem => match p.byte(tag::ITEM)? {
            ITEM_HETZNER_TOKEN => Ok(Request::OpenItem {
                vault: p.bytes(tag::VAULT)?,
                item: HolderItem::HetznerToken {
                    target: p.text(tag::TARGET)?,
                },
            }),
            _ => Err(FrameError::Field(tag::ITEM)),
        },
        Kind::OpenCache => Ok(Request::OpenCache {
            vault: p.bytes(tag::VAULT)?,
            secret: secret_of(p.byte(tag::SECRET_KIND)?)?,
            target: p.text(tag::TARGET)?,
            armored: p.bytes(tag::ARMORED)?,
        }),
        Kind::Recipient => Ok(Request::Recipient {
            vault: p.bytes(tag::VAULT)?,
        }),
        _ => Err(FrameError::Unexpected(byte)),
    }
}

// ---------------------------------------------------------------------
// Replies (holder → client)
// ---------------------------------------------------------------------

pub fn write_reply(w: &mut impl Write, reply: &Reply) -> io::Result<()> {
    match reply {
        Reply::Item(item) => write_secret(w, item),
        Reply::Status(s) => write_plain(
            w,
            Kind::StatusReply,
            &Fields::default()
                .byte(tag::PROTOCOL, s.protocol)
                .text(tag::VERSION, &s.version)
                .text(tag::STORE_ID, &s.store_id)
                .bytes(tag::VAULT_ID, &s.vault_id.0)
                .quad(tag::LEFT_SECS, s.left_secs)
                .word(tag::MINUTES, s.minutes)
                .bytes(tag::HARDENING, &hardening_bits(&s.hardening)),
        ),
        Reply::Absent => write_plain(w, Kind::Absent, &Fields::default()),
        Reply::Recipient(r) => write_plain(
            w,
            Kind::RecipientReply,
            &Fields::default().text(tag::RECIPIENT, r),
        ),
        Reply::Ended => write_plain(w, Kind::Ended, &Fields::default()),
        Reply::Refused(r) => {
            let protocol = match r {
                Refusal::Unsupported { holder_protocol } => *holder_protocol,
                _ => PROTOCOL,
            };
            let mut f = Fields::default()
                .byte(tag::REFUSAL, r.code())
                .byte(tag::PROTOCOL, protocol);
            if let Refusal::CacheRefused { reason, detail } = r {
                f = f
                    .byte(tag::REASON, why_code(*reason))
                    .text(tag::DETAIL, detail);
            }
            write_plain(w, Kind::Refused, &f)
        }
    }
}

/// One opened item, written from the caller's buffer: header, field header, then the bytes.
pub fn write_secret(w: &mut impl Write, secret: &[u8]) -> io::Result<()> {
    let len = frame_len(FIELD_LEN + secret.len())?;
    w.write_all(&header(Kind::Secret, len))?;
    w.write_all(&field_header(tag::SECRET, secret.len()))?;
    w.write_all(secret)?;
    w.flush()
}

/// A client reads one reply: an item straight into an exact-size `Zeroizing` buffer; under
/// another version only the frozen `Unsupported` refusal.
pub fn read_reply(r: &mut impl Read) -> Result<Reply, FrameError> {
    let (version, byte, len) = read_header(r)?;
    let kind = Kind::from_byte(byte).ok_or(FrameError::UnknownKind(byte))?;
    if kind == Kind::Secret {
        if version != PROTOCOL {
            return Err(FrameError::Version(version));
        }
        let mut head = [0u8; FIELD_LEN];
        r.read_exact(&mut head)?;
        let t = u16::from_le_bytes([head[0], head[1]]);
        let n = u32::from_le_bytes([head[2], head[3], head[4], head[5]]);
        if t != tag::SECRET || n.checked_add(FIELD_LEN as u32) != Some(len) {
            return Err(FrameError::Field(tag::SECRET));
        }
        let mut item = Zeroizing::new(vec![0u8; n as usize]);
        r.read_exact(&mut item)?;
        return Ok(Reply::Item(item));
    }
    let body = read_body(r, len)?;
    let p = parse_body(&body)?;
    if version != PROTOCOL {
        if kind == Kind::Refused && p.byte(tag::REFUSAL)? == UNSUPPORTED {
            return Ok(Reply::Refused(Refusal::Unsupported {
                holder_protocol: p.byte(tag::PROTOCOL)?,
            }));
        }
        return Err(FrameError::Version(version));
    }
    match kind {
        Kind::StatusReply => Ok(Reply::Status(HolderStatus {
            protocol: p.byte(tag::PROTOCOL)?,
            version: p.text(tag::VERSION)?,
            store_id: p.text(tag::STORE_ID)?,
            vault_id: p.vault_id()?,
            left_secs: p.quad(tag::LEFT_SECS)?,
            minutes: p.word(tag::MINUTES)?,
            hardening: hardening_from(p.need(tag::HARDENING)?)?,
        })),
        Kind::Absent => Ok(Reply::Absent),
        Kind::RecipientReply => Ok(Reply::Recipient(p.text(tag::RECIPIENT)?)),
        Kind::Ended => Ok(Reply::Ended),
        Kind::Refused => Ok(Reply::Refused(match p.byte(tag::REFUSAL)? {
            0x01 => Refusal::NotThisTerminal,
            0x02 => Refusal::WrongStore,
            0x03 => Refusal::KeyMismatch,
            0x04 => Refusal::WindowEnded,
            0x05 => Refusal::Malformed,
            UNSUPPORTED => Refusal::Unsupported {
                holder_protocol: p.byte(tag::PROTOCOL)?,
            },
            0x07 => Refusal::VaultDamaged,
            0x08 => Refusal::CacheRefused {
                reason: why_of(p.byte(tag::REASON)?)?,
                detail: p.text(tag::DETAIL)?,
            },
            _ => return Err(FrameError::Field(tag::REFUSAL)),
        })),
        _ => Err(FrameError::Unexpected(byte)),
    }
}

// ---------------------------------------------------------------------
// The init and ack pipes (CLI ↔ the holder it starts)
// ---------------------------------------------------------------------

fn init_fields(init: &InitHeader) -> Fields {
    let mut f = Fields::default()
        .byte(tag::PROTOCOL, PROTOCOL)
        .text(tag::STORE_ID, &init.store.id)
        .put(tag::ROOT, path_bytes(&init.store.root))
        .put(tag::BASE, path_bytes(&init.base))
        .bytes(tag::VAULT_ID, &init.vault_id.0)
        .word(tag::MINUTES, init.minutes)
        .word(tag::PID, init.client_pid);
    if let Some(secs) = init.window_secs {
        f = f.word(tag::WINDOW_SECS, secs);
    }
    if init.poll_leader {
        f = f.byte(tag::POLL_LEADER, 1);
    }
    f
}

/// The init frame: its plain fields, then the key straight from its own buffer. `w` is the
/// unbuffered init pipe (a `BufWriter` would copy the key into its heap buffer).
pub fn write_init(w: &mut impl Write, init: &InitHeader, key: &DataKey) -> io::Result<()> {
    let fields = init_fields(init);
    let len = frame_len(fields.body_len() + FIELD_LEN + 32)?;
    w.write_all(&header(Kind::Init, len))?;
    for (t, v) in &fields.0 {
        w.write_all(&field_header(*t, v.len()))?;
        w.write_all(v)?;
    }
    w.write_all(&field_header(tag::KEY, 32))?;
    w.write_all(key.expose())?;
    w.flush()
}

/// A holder reads its init frame field by field, the key straight into `key` (its locked page).
pub fn read_init(r: &mut impl Read, key: &mut [u8; 32]) -> Result<InitHeader, FrameError> {
    let (version, byte, len) = read_header(r)?;
    if Kind::from_byte(byte) != Some(Kind::Init) {
        return Err(FrameError::Unexpected(byte));
    }
    if version != PROTOCOL {
        return Err(FrameError::Version(version));
    }
    if len > MAX_INIT {
        return Err(FrameError::TooLarge(len));
    }
    let mut left = len as usize;
    let mut plain: Vec<(u16, Vec<u8>)> = Vec::new();
    let mut keyed = false;
    while left > 0 {
        if left < FIELD_LEN {
            return Err(FrameError::Truncated);
        }
        let mut head = [0u8; FIELD_LEN];
        r.read_exact(&mut head)?;
        left -= FIELD_LEN;
        let t = u16::from_le_bytes([head[0], head[1]]);
        let n = u32::from_le_bytes([head[2], head[3], head[4], head[5]]) as usize;
        if n > left {
            return Err(FrameError::Truncated);
        }
        if t == tag::KEY {
            if n != 32 || keyed {
                return Err(FrameError::Field(tag::KEY));
            }
            r.read_exact(key)?;
            keyed = true;
        } else {
            let mut v = vec![0u8; n];
            r.read_exact(&mut v)?;
            plain.push((t, v));
        }
        left -= n;
    }
    if !keyed {
        return Err(FrameError::Missing(tag::KEY));
    }
    let p = Parsed(plain.iter().map(|(t, v)| (*t, v.as_slice())).collect());
    let protocol = p.byte(tag::PROTOCOL)?;
    if protocol != PROTOCOL {
        return Err(FrameError::Version(protocol));
    }
    Ok(InitHeader {
        store: StoreBinding {
            id: p.text(tag::STORE_ID)?,
            root: path_from(p.need(tag::ROOT)?, tag::ROOT)?,
        },
        base: path_from(p.need(tag::BASE)?, tag::BASE)?,
        vault_id: p.vault_id()?,
        minutes: p.word(tag::MINUTES)?,
        window_secs: p.opt_word(tag::WINDOW_SECS)?,
        poll_leader: p.get(tag::POLL_LEADER) == Some(&[1][..]),
        client_pid: p.word(tag::PID)?,
    })
}

pub fn write_ack(w: &mut impl Write, ack: &Ack) -> io::Result<()> {
    let fields = match ack {
        Ack::Ready {
            pid,
            protocol,
            version,
        } => Fields::default()
            .byte(tag::ACK, 0)
            .word(tag::PID, *pid)
            .byte(tag::PROTOCOL, *protocol)
            .text(tag::VERSION, version),
        Ack::AlreadyRunning { pid } => Fields::default().byte(tag::ACK, 1).word(tag::PID, *pid),
        Ack::Unavailable(u) => {
            let f = Fields::default()
                .byte(tag::ACK, 2)
                .byte(tag::REASON, reason_code(u.reason()));
            match u {
                HolderUnavailable::UnsafeRuntimeDir { path, reason } => {
                    f.put(tag::ROOT, path_bytes(path)).text(tag::DETAIL, reason)
                }
                HolderUnavailable::PathTooLong { len, max } => {
                    f.word(tag::CODE, *len as u32).word(tag::LIMIT, *max as u32)
                }
                HolderUnavailable::NoConsole { code } => f.word(tag::CODE, *code),
                HolderUnavailable::NotResponding { pid: Some(pid) } => f.word(tag::PID, *pid),
                _ => f,
            }
        }
    };
    write_plain(w, Kind::Ack, &fields)
}

pub fn read_ack(r: &mut impl Read) -> Result<Ack, FrameError> {
    let (version, byte, len) = read_header(r)?;
    if Kind::from_byte(byte) != Some(Kind::Ack) {
        return Err(FrameError::Unexpected(byte));
    }
    if version != PROTOCOL {
        return Err(FrameError::Version(version));
    }
    let body = read_body(r, len)?;
    let p = parse_body(&body)?;
    match p.byte(tag::ACK)? {
        0 => Ok(Ack::Ready {
            pid: p.word(tag::PID)?,
            protocol: p.byte(tag::PROTOCOL)?,
            version: p.text(tag::VERSION)?,
        }),
        1 => Ok(Ack::AlreadyRunning {
            pid: p.word(tag::PID)?,
        }),
        2 => Ok(Ack::Unavailable(match p.byte(tag::REASON)? {
            1 => HolderUnavailable::NoRuntimeDir,
            2 => HolderUnavailable::UnsafeRuntimeDir {
                path: path_from(p.need(tag::ROOT)?, tag::ROOT)?,
                reason: p.text(tag::DETAIL)?,
            },
            3 => HolderUnavailable::PathTooLong {
                len: p.word(tag::CODE)? as usize,
                max: p.word(tag::LIMIT)? as usize,
            },
            4 => HolderUnavailable::NoSessionLeader,
            5 => HolderUnavailable::NoConsole {
                code: p.word(tag::CODE)?,
            },
            6 => HolderUnavailable::NotResponding {
                pid: p.opt_word(tag::PID)?,
            },
            7 => HolderUnavailable::Timeout,
            _ => return Err(FrameError::Field(tag::REASON)),
        })),
        _ => Err(FrameError::Field(tag::ACK)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn written(f: impl FnOnce(&mut Vec<u8>) -> io::Result<()>) -> Vec<u8> {
        let mut out = Vec::new();
        f(&mut out).unwrap();
        out
    }

    // `$E/goldens.py` (Task 2 Step 1): an independent encoder of D.4a §3.10.
    const STATUS: &str = "41524801010000000000";
    const END: &str = "415248017f0000000000";
    const ENDED: &str = "41524801ff0000000000";
    const ABSENT: &str = "41524801910000000000";
    const OPEN_ITEM: &str =
        "4152480110001c0000001000050000005641554c541100010000000112000400000070726f64";
    const OPEN_CACHE: &str =
        "4152480111001f000000100001000000561400010000000112000400000070726f6413000100000041";
    const RECIPIENT: &str = "4152480112000700000010000100000056";
    const REFUSED_NOT_THIS_TERMINAL: &str = "41524801e0000e0000001700010000000101000100000001";
    const REFUSED_CACHE: &str =
        "41524801e0001d00000017000100000008010001000000010c0001000000011800020000006e6f";
    const SECRET: &str = "41524801900009000000150003000000746f6b";
    const RECIPIENT_REPLY: &str = "4152480192000b0000001600050000006167653178";
    const STATUS_REPLY: &str = concat!(
        "415248018100630000000100010000000102000a000000302e302e302d746573740300100000003031323334",
        "3536373839616263646566060010000000111111111111111111111111111111110e0008000000b400000000",
        "000000070004000000050000000f0002000000050d",
    );
    const ACK_READY: &str = concat!(
        "415248014100280000000b0001000000000a0004000000921000000100010000000102000a000000302e302e",
        "302d74657374",
    );
    const ACK_ALREADY: &str = "415248014100110000000b0001000000010a000400000092100000";
    const ACK_TOO_LONG: &str =
        "415248014100220000000b0001000000020c0001000000030d0004000000780000001a000400000068000000";
    #[cfg(unix)]
    const INIT_UNIX: &str = concat!(
        "4152480140007d00000001000100000001030010000000303132333435363738396162636465660400020000",
        "002f730500020000002f7206001000000022222222222222222222222222222222070004000000050000000a",
        "0004000000070000000900200000005a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
        "5a5a5a",
    );
    // Hand-built frames (the same Python, other versions and shapes).
    const END_V9: &str = "415248097f0000000000";
    const STATUS_V9: &str = "41524809010000000000";
    const UNSUPPORTED_V9: &str = "41524809e0000e0000001700010000000601000100000009";
    const NOT_THIS_TERMINAL_V9: &str = "41524809e0000e0000001700010000000101000100000009";
    const KEYLESS_INIT: &str = "4152480140000700000001000100000001";
    const OPEN_ITEM_UNKNOWN_TAG: &str = concat!(
        "415248011000240000001000050000005641554c547777020000007a7a110001000000011200040000007072",
        "6f64",
    );

    fn status() -> HolderStatus {
        HolderStatus {
            protocol: 1,
            version: "0.0.0-test".into(),
            store_id: "0123456789abcdef".into(),
            vault_id: VaultId([0x11; 16]),
            left_secs: 180,
            minutes: 5,
            hardening: HolderHardening {
                non_dumpable: Some(true),
                deny_attach: None,
                core_off: true,
                memory_locked: false,
                process_dacl: None,
            },
        }
    }

    fn requests() -> Vec<(&'static str, Request)> {
        vec![
            (STATUS, Request::Status),
            (END, Request::End),
            (
                OPEN_ITEM,
                Request::OpenItem {
                    vault: b"VAULT".to_vec(),
                    item: HolderItem::HetznerToken {
                        target: "prod".into(),
                    },
                },
            ),
            (
                OPEN_CACHE,
                Request::OpenCache {
                    vault: b"V".to_vec(),
                    secret: CachedSecret::Kubeconfig,
                    target: "prod".into(),
                    armored: b"A".to_vec(),
                },
            ),
            (
                RECIPIENT,
                Request::Recipient {
                    vault: b"V".to_vec(),
                },
            ),
        ]
    }

    fn replies() -> Vec<(&'static str, Reply)> {
        vec![
            (ENDED, Reply::Ended),
            (ABSENT, Reply::Absent),
            (SECRET, Reply::Item(Zeroizing::new(b"tok".to_vec()))),
            (RECIPIENT_REPLY, Reply::Recipient("age1x".into())),
            (
                REFUSED_NOT_THIS_TERMINAL,
                Reply::Refused(Refusal::NotThisTerminal),
            ),
            (
                REFUSED_CACHE,
                Reply::Refused(Refusal::CacheRefused {
                    reason: UndecryptableWhy::NotForThisKey,
                    detail: "no".into(),
                }),
            ),
            (STATUS_REPLY, Reply::Status(status())),
        ]
    }

    fn acks() -> Vec<(&'static str, Ack)> {
        vec![
            (
                ACK_READY,
                Ack::Ready {
                    pid: 4242,
                    protocol: 1,
                    version: "0.0.0-test".into(),
                },
            ),
            (ACK_ALREADY, Ack::AlreadyRunning { pid: 4242 }),
            (
                ACK_TOO_LONG,
                Ack::Unavailable(HolderUnavailable::PathTooLong { len: 120, max: 104 }),
            ),
        ]
    }

    /// D.4a T58: every v1 frame is frozen byte for byte — a holder one release started serves
    /// the next release's commands, and `End` reaches any of them.
    #[test]
    fn every_v1_frame_has_its_golden_bytes() {
        for (golden, req) in requests() {
            assert_eq!(written(|w| write_request(w, &req)), hex(golden), "{req:?}");
        }
        for (golden, reply) in replies() {
            assert_eq!(
                written(|w| write_reply(w, &reply)),
                hex(golden),
                "{reply:?}"
            );
        }
        for (golden, ack) in acks() {
            assert_eq!(written(|w| write_ack(w, &ack)), hex(golden), "{ack:?}");
        }
    }

    #[test]
    fn every_v1_frame_reads_back_as_written() {
        for (golden, req) in requests() {
            assert_eq!(read_request(&mut &hex(golden)[..]).unwrap(), req);
        }
        for (golden, reply) in replies() {
            assert_eq!(read_reply(&mut &hex(golden)[..]).unwrap(), reply);
        }
        for (golden, ack) in acks() {
            assert_eq!(read_ack(&mut &hex(golden)[..]).unwrap(), ack);
        }
    }

    fn init() -> InitHeader {
        InitHeader {
            store: StoreBinding {
                id: "0123456789abcdef".into(),
                root: PathBuf::from("/s"),
            },
            base: PathBuf::from("/r"),
            vault_id: VaultId([0x22; 16]),
            minutes: 5,
            window_secs: None,
            poll_leader: false,
            client_pid: 7,
        }
    }

    /// S6: the key is written from its own buffer and read straight into the caller's.
    #[cfg(unix)]
    #[test]
    fn the_init_frame_is_golden_and_its_key_lands_where_the_reader_says() {
        let key = DataKey::from_bytes(&[0x5a; 32]);
        let bytes = written(|w| write_init(w, &init(), &key));
        assert_eq!(bytes, hex(INIT_UNIX));
        let mut got = [0u8; 32];
        let back = read_init(&mut &bytes[..], &mut got).unwrap();
        assert_eq!((back, got), (init(), [0x5a; 32]));
    }

    #[test]
    fn the_test_only_init_fields_round_trip_and_a_keyless_init_is_refused() {
        let mut with = init();
        with.window_secs = Some(3);
        with.poll_leader = true;
        let key = DataKey::from_bytes(&[7; 32]);
        let bytes = written(|w| write_init(w, &with, &key));
        let mut got = [0u8; 32];
        assert_eq!(read_init(&mut &bytes[..], &mut got).unwrap(), with);
        assert_eq!(got, [7; 32]);
        let mut untouched = [0u8; 32];
        assert!(matches!(
            read_init(&mut &hex(KEYLESS_INIT)[..], &mut untouched),
            Err(FrameError::Missing(tag::KEY))
        ));
    }

    #[test]
    fn a_reader_skips_a_field_it_does_not_know() {
        assert_eq!(
            read_request(&mut &hex(OPEN_ITEM_UNKNOWN_TAG)[..]).unwrap(),
            requests()[2].1
        );
    }

    #[test]
    fn a_truncated_oversized_or_foreign_frame_is_refused() {
        let mut short = hex(STATUS_REPLY);
        short.pop();
        assert!(matches!(
            read_reply(&mut &short[..]),
            Err(FrameError::Io(_))
        ));
        let mut huge = hex(STATUS);
        huge[6..10].copy_from_slice(&(MAX_FRAME + 1).to_le_bytes());
        assert!(matches!(
            read_request(&mut &huge[..]),
            Err(FrameError::TooLarge(_))
        ));
        let mut magic = hex(STATUS);
        magic[2] = b'X';
        assert!(matches!(
            read_request(&mut &magic[..]),
            Err(FrameError::BadMagic)
        ));
        let mut flags = hex(STATUS);
        flags[5] = 1;
        assert!(matches!(
            read_request(&mut &flags[..]),
            Err(FrameError::Flags(1))
        ));
        let mut kind = hex(STATUS);
        kind[4] = 0x55;
        assert!(matches!(
            read_request(&mut &kind[..]),
            Err(FrameError::UnknownKind(0x55))
        ));
        // A field that claims more than the body holds.
        let mut field = hex(RECIPIENT);
        field[12] = 0xff;
        assert!(matches!(
            read_request(&mut &field[..]),
            Err(FrameError::Truncated)
        ));
    }

    /// T58: `End` and the `Unsupported` refusal are frozen across versions; nothing else is.
    #[test]
    fn end_and_the_unsupported_refusal_read_under_any_version() {
        assert_eq!(read_request(&mut &hex(END_V9)[..]).unwrap(), Request::End);
        assert!(matches!(
            read_request(&mut &hex(STATUS_V9)[..]),
            Err(FrameError::Version(9))
        ));
        assert_eq!(
            read_reply(&mut &hex(UNSUPPORTED_V9)[..]).unwrap(),
            Reply::Refused(Refusal::Unsupported { holder_protocol: 9 })
        );
        assert!(matches!(
            read_reply(&mut &hex(NOT_THIS_TERMINAL_V9)[..]),
            Err(FrameError::Version(9))
        ));
    }

    /// S6: a secret-bearing kind is never a request, and never goes through the plain writer.
    #[test]
    fn a_secret_bearing_frame_is_never_a_request_or_plain_data() {
        // Refused at its header: the body (a key, an item) is never read into the request
        // reader's plain buffer — the reader is left right after the header.
        for (frame, kind) in [(SECRET, 0x90), (KEYLESS_INIT, 0x40)] {
            let bytes = hex(frame);
            let mut left = &bytes[..];
            assert!(
                matches!(read_request(&mut left), Err(FrameError::Unexpected(k)) if k == kind),
                "{frame}"
            );
            assert_eq!(
                left.len(),
                bytes.len() - HEADER_LEN,
                "{frame}: its body was read"
            );
        }
        for byte in [0x40, 0x90] {
            let kind = Kind::from_byte(byte).unwrap();
            assert!(kind.carries_secret());
            let mut out = Vec::new();
            assert!(write_plain(&mut out, kind, &Fields::default()).is_err());
            assert!(out.is_empty());
        }
    }

    /// Rule 9: an opened item never shows in a `Debug` output.
    #[test]
    fn a_reply_never_shows_its_item_in_debug() {
        let shown = format!("{:?}", Reply::Item(Zeroizing::new(b"tok".to_vec())));
        assert!(!shown.contains("tok") && !shown.contains("116"), "{shown}");
    }

    /// D.4b B86: every reason a cache refusal can carry has a byte of its own, read back as
    /// itself; no other byte is a reason.
    #[test]
    fn every_undecryptable_reason_has_a_byte_of_its_own() {
        for why in [
            UndecryptableWhy::NotForThisKey,
            UndecryptableWhy::Damaged,
            UndecryptableWhy::RetiredKey,
        ] {
            assert_eq!(why_of(why_code(why)).unwrap(), why);
        }
        assert!(why_of(0).is_err() && why_of(4).is_err());
    }

    #[test]
    fn hardening_and_every_unavailability_round_trip() {
        let opts = [None, Some(false), Some(true)];
        for nd in opts {
            for da in opts {
                for dacl in opts {
                    for (core, mem) in [(false, true), (true, false)] {
                        let h = HolderHardening {
                            non_dumpable: nd,
                            deny_attach: da,
                            core_off: core,
                            memory_locked: mem,
                            process_dacl: dacl,
                        };
                        assert_eq!(hardening_from(&hardening_bits(&h)).unwrap(), h);
                    }
                }
            }
        }
        for u in [
            HolderUnavailable::NoRuntimeDir,
            HolderUnavailable::UnsafeRuntimeDir {
                path: PathBuf::from("/r/apprafter"),
                reason: "its mode 755 lets others in".into(),
            },
            HolderUnavailable::PathTooLong { len: 120, max: 104 },
            HolderUnavailable::NoSessionLeader,
            HolderUnavailable::NoConsole { code: 6 },
            HolderUnavailable::NotResponding { pid: Some(42) },
            HolderUnavailable::NotResponding { pid: None },
            HolderUnavailable::Timeout,
        ] {
            let ack = Ack::Unavailable(u.clone());
            assert_eq!(
                read_ack(&mut &written(|w| write_ack(w, &ack))[..]).unwrap(),
                ack
            );
            assert!(!u.describe().is_empty());
        }
    }
}
