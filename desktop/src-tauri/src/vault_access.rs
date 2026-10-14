// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The key the app holds (D.4a §3.4, T74, T80; ADR 0068 §8). On a protected store the app is
//! unlocked exactly while it holds the store's data key: [`DesktopVaultAccess`] is the
//! desktop's `VaultAccess`. The app's unlock puts the key in ([`DesktopVaultAccess::keep`],
//! under the lock machine's lock, just before it unlocks), every lock takes it out
//! (`forget`, from the lock hook), and the core's reads and writes take a session over it —
//! for this store's id only. A read that finds no key for the store it asks about refuses
//! `store_locked` and tells the shell (`on_missing`, called holding nothing of this access's),
//! which re-reads the store and locks the app when the store was locked under it
//! (`LockReason::SecretsLocked`).
//!
//! **The epoch.** The one flow that ends holding a key it opened itself — Protect, which opens
//! the new vault with the recovery key the person just confirmed (T64) — takes the epoch when
//! it is planned and keeps the key only if no lock came since ([`DesktopVaultAccess::hold`]):
//! every `forget` moves the epoch on, so a key opened across a lock is dropped, never kept in
//! a locked app.
//!
//! **The gate.** Every secret typed into the app — the lock screen's field, a confirm dialog's,
//! Finish's two — opens through one [`VaultGate`]: one opening at a time (a key derivation is
//! 256 MiB and about 0.7 s; a second is `auth_busy`), behind the app's back-off over all of
//! them (`apprafter_os_auth::outcome::Backoff`: three failures, then 30 s refused, on the
//! monotonic clock). A secret no slot takes is `auth_failed`; a mistyped recovery key is the
//! core's `recovery_key_invalid`, whose fields say where it is wrong (never what was typed);
//! both count, and the failure that starts the back-off is `auth_failed` with its countdown;
//! anything else (an unreadable vault, another store's) is the core's own error and counts
//! nothing.
//!
//! **After a lock**, nothing reads: every session the core took is an [`EpochSession`], which
//! holds the key weakly and refuses `store_locked` once the epoch moved (ADR 0068
//! Consequences) — a read that runs on across a lock stops at its next secret, and the key is
//! zeroed when the lock drops it. A vault write confirmed before the lock finishes with the key
//! the core took for it before its store lock ([`VaultAccess::write_key`]), that key's page
//! still pinned until the write ends ([`WritePin`]) — or, still waiting for another program's
//! store lock [`STORE_WAIT_SLICE`] after the lock, is abandoned with nothing written
//! ([`VaultAccess::keep_waiting`]). The epoch moves only when a key is dropped
//! ([`DesktopVaultAccess::drop_key`], `keep_only`, `forget`).
//!
//! It never names the CLI's holder (HG4), never reads a key from anywhere but a typed secret,
//! and its `Debug` shows no key.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError, Weak};
use std::time::Duration;

use apprafter_core::lock::{self, OpenedVault, SlotOpened};
use apprafter_core::secrets::{
    KeySession, LockSlotKind, StoreId, VaultAccess, VaultRequest, VaultSession,
};
use apprafter_core::vault::{DataKey, KdfPolicy, SlotInput, VaultId};
use apprafter_core::{CoreError, CoreResult, SecretString};
use apprafter_os_auth::outcome::Backoff;
use cli_core::secrets::{CachedSecret, Recipient};
use zeroize::Zeroizing;

use crate::errors::{DesktopError, Refusal};
use crate::harden;
use crate::ops::{Clock, SecretGesture};

/// How far `forget` has moved on: a key opened before a lock is not kept after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEpoch(u64);

/// How long a change that holds a key waits for the target store's lock before it asks
/// whether it may go on ([`VaultAccess::keep_waiting`]): after a lock it is abandoned within
/// this, nothing written yet, and its key goes with it (the final verifier's finding 12; the
/// app's words say "30 s").
pub const STORE_WAIT_SLICE: Duration = Duration::from_secs(30);

struct HeldKey {
    store_id: StoreId,
    vault_id: VaultId,
    key: Arc<DataKey>,
    /// Its page is locked in memory, as the pager answered.
    page_locked: bool,
    /// Vault writes the core took this key for and has not finished ([`WritePin`]).
    writes: Arc<AtomicUsize>,
}

/// Held by a write's key (`KeySession::with_guard`, dropped with its last clone, before the
/// key): the write is counted on its key while it lives, and its end lets a key a lock
/// retired go ([`DesktopVaultAccess::write_ended`]).
struct WritePin {
    writes: Arc<AtomicUsize>,
    access: Weak<DesktopVaultAccess>,
}

impl Drop for WritePin {
    fn drop(&mut self) {
        if self.writes.fetch_sub(1, SeqCst) == 1 {
            if let Some(access) = self.access.upgrade() {
                access.write_ended();
            }
        }
    }
}

/// Locks a key's page in memory (T77): the OS's ([`OsPager`]), or a test's.
pub trait Pager: Send + Sync {
    /// Whether the page(s) under `bytes` are now locked.
    fn lock(&self, bytes: &[u8]) -> bool;
    fn unlock(&self, bytes: &[u8]);
}

/// `mlock` / `VirtualLock`, under the OS's limit (`harden`).
pub struct OsPager;

impl Pager for OsPager {
    fn lock(&self, bytes: &[u8]) -> bool {
        harden::lock_page(bytes)
    }

    fn unlock(&self, bytes: &[u8]) {
        harden::unlock_page(bytes);
    }
}

/// What the access shares with every session it hands out: the epoch, and the look a session
/// takes before each use.
#[derive(Default)]
struct Shared {
    /// Moved on when a key is dropped — a lock, the quit, a sleep, a re-read that found the
    /// store changed or no longer protected — under `held`'s lock; never by a re-read of an
    /// unchanged store (the Rust verifier's finding 2). Each session keeps the value it was
    /// taken at and refuses once it moved ([`EpochSession`]).
    epoch: AtomicU64,
    /// O4 between ticks (Task 15: the lock machine's `check_sleep`): asked before every
    /// session's use, holding nothing of this access's.
    before_use: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

/// The desktop's `VaultAccess` (module docs).
pub struct DesktopVaultAccess {
    held: Mutex<Option<HeldKey>>,
    /// Keys a lock took out while a write the core took them for still ran: pinned and alive
    /// until that write ends (the final verifier's finding 12). Locked after `held`.
    retired: Mutex<Vec<HeldKey>>,
    shared: Arc<Shared>,
    gate: VaultGate,
    pager: Arc<dyn Pager>,
    on_missing: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// Tests: every ask of the core's (T-lazy-3).
    #[cfg(test)]
    pub(crate) asks: std::sync::atomic::AtomicUsize,
    /// The shell's "may a change go on waiting for the store's lock?" (unlocked, not
    /// quitting); with none set, it may.
    may_wait: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
    /// This access, for the write pins it hands out.
    me: Weak<DesktopVaultAccess>,
}

impl DesktopVaultAccess {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Self::with_pager(clock, Arc::new(OsPager))
    }

    /// [`new`](Self::new), with another pager (the tests').
    pub fn with_pager(clock: Arc<dyn Clock>, pager: Arc<dyn Pager>) -> Arc<Self> {
        Arc::new_cyclic(|me| DesktopVaultAccess {
            held: Mutex::default(),
            retired: Mutex::default(),
            shared: Arc::default(),
            gate: VaultGate::new(clock),
            pager,
            on_missing: OnceLock::new(),
            #[cfg(test)]
            asks: std::sync::atomic::AtomicUsize::new(0),
            may_wait: OnceLock::new(),
            me: me.clone(),
        })
    }

    /// What a change waiting for the store's lock asks after each [`STORE_WAIT_SLICE`]: the
    /// shell's "unlocked and not quitting" (Task 15). Set once, by the shell.
    pub fn may_wait(&self, hook: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = self.may_wait.set(Box::new(hook));
    }

    /// What a read that finds no key calls, once this access's lock is released: the shell's
    /// re-read of the store. Set once, by the shell; a second call changes nothing.
    pub fn on_missing(&self, hook: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_missing.set(Box::new(hook));
    }

    /// What every session asks before each use, and every new session before it is made: the
    /// lock machine's O4 look between ticks (Task 15). Set once, by the shell.
    pub fn before_use(&self, hook: impl Fn() + Send + Sync + 'static) {
        let _ = self.shared.before_use.set(Box::new(hook));
    }

    pub fn epoch(&self) -> KeyEpoch {
        let _held = self.lock();
        KeyEpoch(self.shared.epoch.load(SeqCst))
    }

    /// Keep `opened`, the key of the store `store_id`, unless a key was dropped since `epoch`
    /// — then it is dropped. Whether it was kept.
    pub fn hold(&self, epoch: KeyEpoch, store_id: StoreId, opened: OpenedVault) -> bool {
        let mut held = self.lock();
        if self.shared.epoch.load(SeqCst) != epoch.0 {
            return false;
        }
        self.put(&mut held, store_id, opened);
        true
    }

    /// Keep `opened` now: the app's unlock, called under the lock machine's lock just before
    /// it unlocks, so the key is held exactly when the app is unlocked.
    pub fn keep(&self, store_id: StoreId, opened: OpenedVault) {
        let mut held = self.lock();
        self.put(&mut held, store_id, opened);
    }

    /// Whether the key of `store_id` is held.
    pub fn holds(&self, store_id: &StoreId) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|k| &k.store_id == store_id)
    }

    /// Whether the key held is locked in memory (T77's `memory_locked`, which `lock_info`
    /// reports); `false` with none.
    pub fn memory_locked(&self) -> bool {
        self.lock().as_ref().is_some_and(|k| k.page_locked)
    }

    /// The shell's re-read of a protected store (the final verifier's finding 8): whether the
    /// key held is `store_id`'s — then it stays — and otherwise any key held goes, the epoch
    /// moving on; decided and done in one step under `held`'s lock, so an unlock's `keep`
    /// never lands between them. The shell calls it under the machine's lock
    /// (`LockMachine::lock_unlocked_unless`), which that `keep` holds too.
    pub fn keep_only(&self, store_id: &StoreId) -> bool {
        let mut held = self.lock();
        if held.as_ref().is_some_and(|k| &k.store_id == store_id) {
            return true;
        }
        if let Some(old) = held.take() {
            self.shared.epoch.fetch_add(1, SeqCst);
            self.release(old);
        }
        false
    }

    /// The shell's re-read found the store changed or no longer protected: the key held goes,
    /// and the epoch moves on — **only when a key was held**. A re-read of a store in files, or
    /// of the store whose key is held, moves nothing, so a Protect plan waiting for its confirm
    /// across a focus or an `app_info` keeps its key at the end (T64; the Rust verifier's
    /// finding 2). Whether a key went.
    pub fn drop_key(&self) -> bool {
        let mut held = self.lock();
        let Some(old) = held.take() else {
            return false;
        };
        self.shared.epoch.fetch_add(1, SeqCst);
        self.release(old);
        true
    }

    /// Opens the vault of the store at `root` (`id`) with `secret`, typed into one of the
    /// app's fields — the lock password or the recovery key, whichever it is (T62) — through
    /// the gate. Blocks for a derivation: never on the main thread.
    pub fn open_typed(
        &self,
        root: &Path,
        id: &StoreId,
        secret: SecretString,
    ) -> Result<OpenedVault, Refusal> {
        self.gate
            .attempt(|| lock::open_at(root, id, SlotInput::from_input(secret), KdfPolicy::active()))
    }

    /// Opens one slot of `kind` with `secret`, through the gate (Finish, rule 4).
    pub fn open_slot(
        &self,
        root: &Path,
        id: &StoreId,
        kind: LockSlotKind,
        secret: SecretString,
    ) -> Result<SlotOpened, Refusal> {
        self.gate
            .attempt(|| lock::open_slot_at(root, id, kind, secret, KdfPolicy::active()))
    }

    /// The O4 look a session takes before its use, holding nothing of this access's.
    fn look(&self) {
        if let Some(look) = self.shared.before_use.get() {
            look();
        }
    }

    /// A session over the key held for `req`'s store: it holds the key **weakly** (the Rust
    /// verifier's finding 3), so the lock's drop of the access's one strong reference zeroes
    /// the key at once, whatever session an operation still holds.
    fn session_for(&self, req: &VaultRequest<'_>) -> Option<EpochSession> {
        let held = self.lock();
        let k = held.as_ref().filter(|k| &k.store_id == req.store_id)?;
        Some(EpochSession {
            key: Arc::downgrade(&k.key),
            vault_id: k.vault_id,
            root: req.store_root.to_path_buf(),
            taken: self.shared.epoch.load(SeqCst),
            shared: self.shared.clone(),
        })
    }

    /// No key for `req`'s store: the shell hears of it, holding nothing of ours.
    fn missing(&self, req: &VaultRequest<'_>) -> cli_core::CliError {
        if let Some(hook) = self.on_missing.get() {
            hook();
        }
        store_locked(req.store_root)
    }

    /// `opened` becomes the key held, its page locked; the one it replaces goes first, so an
    /// unlock of the old page (`munlock` does not nest) never unlocks the new one's.
    fn put(&self, held: &mut Option<HeldKey>, store_id: StoreId, opened: OpenedVault) {
        if let Some(old) = held.take() {
            self.release(old);
        }
        let page_locked = self.pager.lock(&opened.key.expose()[..]);
        *held = Some(HeldKey {
            store_id,
            vault_id: opened.vault_id,
            key: opened.key,
            page_locked,
            writes: Arc::default(),
        });
    }

    /// A key that goes, under `held`'s lock. While a vault write the core took it for before
    /// the lock still runs (`writes`), it stays pinned and alive in `retired` until that write
    /// ends ([`WritePin`]): the write finishes with a key locked in memory, and the key goes
    /// with it (the final verifier's finding 12). Otherwise its page is unlocked and the
    /// access's strong reference dropped — the key is zeroed then (`DataKey`'s own `Drop`),
    /// unless a session's call is in flight (it holds the key for that one call). A key's page
    /// is pinned exactly while this access holds the key, in `held` or in `retired`.
    fn release(&self, old: HeldKey) {
        if old.writes.load(SeqCst) > 0 {
            self.retired_keys().push(old);
            return;
        }
        self.unpin(&old);
        // `munlock` / `VirtualUnlock` do not nest: a key still writing may share its page.
        self.repin(None);
    }

    fn unpin(&self, k: &HeldKey) {
        if k.page_locked {
            self.pager.unlock(&k.key.expose()[..]);
        }
    }

    /// Lock again the pages of the keys still alive — `held`, and those still writing — after
    /// an unlock that may have shared a page with one of them.
    fn repin(&self, held: Option<&HeldKey>) {
        let retired = self.retired_keys();
        for k in held.into_iter().chain(retired.iter()) {
            if k.page_locked {
                self.pager.lock(&k.key.expose()[..]);
            }
        }
    }

    /// A write that took its key before a lock has ended ([`WritePin`]): every retired key no
    /// write uses any more goes now, its page unlocked, and the keys still alive are pinned
    /// again.
    fn write_ended(&self) {
        let held = self.lock();
        let done: Vec<HeldKey> = {
            let mut retired = self.retired_keys();
            let (done, still) = std::mem::take(&mut *retired)
                .into_iter()
                .partition(|k| k.writes.load(SeqCst) == 0);
            *retired = still;
            done
        };
        if done.is_empty() {
            return;
        }
        for k in &done {
            self.unpin(k);
        }
        self.repin(held.as_ref());
    }

    fn retired_keys(&self) -> MutexGuard<'_, Vec<HeldKey>> {
        self.retired.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock(&self) -> MutexGuard<'_, Option<HeldKey>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `store_locked` for the store at `root`: nothing in this app can open its vault now.
fn store_locked(root: &Path) -> cli_core::CliError {
    cli_core::CliError::StoreLocked {
        store: root.to_path_buf(),
        reason: cli_core::LockedWhy::NoTerminal,
        stopped_before: None,
    }
}

impl VaultAccess for DesktopVaultAccess {
    fn session(&self, req: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>> {
        #[cfg(test)]
        self.asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.look();
        match self.session_for(req) {
            Some(session) => Ok(Arc::new(session)),
            None => Err(self.missing(req)),
        }
    }

    /// A key for one vault write. The core takes it **before** its store lock
    /// (`SecretStore::prepare_write`), which may wait on another process, and writes with it
    /// after: a strong `KeySession`, the trait's type, carrying a [`WritePin`]. So a vault
    /// write the person confirmed before a lock — a token saved, renamed or removed — finishes
    /// with it, its page still pinned (`release` retires it), and the key goes when that write
    /// ends; one still waiting for the store's lock after a lock is abandoned within
    /// [`STORE_WAIT_SLICE`] (`keep_waiting`). It is never handed out after a lock (the Rust
    /// verifier's finding 3, the final verifier's 12; Deviations 25 and the guide).
    fn write_key(&self, req: &VaultRequest<'_>) -> cli_core::Result<KeySession> {
        #[cfg(test)]
        self.asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.look();
        let key = {
            let held = self.lock();
            held.as_ref()
                .filter(|k| &k.store_id == req.store_id)
                .map(|k| {
                    k.writes.fetch_add(1, SeqCst);
                    let pin = WritePin {
                        writes: k.writes.clone(),
                        access: self.me.clone(),
                    };
                    KeySession::new(k.key.clone(), k.vault_id, req.store_root.to_path_buf())
                        .with_guard(Arc::new(pin))
                })
        };
        key.ok_or_else(|| self.missing(req))
    }

    /// A change that holds a key waits for the store's lock in [`STORE_WAIT_SLICE`]s.
    fn store_wait_slice(&self) -> Option<Duration> {
        Some(STORE_WAIT_SLICE)
    }

    /// After a slice the store's lock is still another program's: the change goes on while the
    /// app is unlocked (the shell's [`may_wait`](DesktopVaultAccess::may_wait)), and is
    /// abandoned — it wrote nothing yet, and its key goes with it — once the app locked or
    /// quits (the final verifier's finding 12).
    fn keep_waiting(&self, store: &Path) -> cli_core::Result<()> {
        match self.may_wait.get() {
            Some(may) if !may() => Err(store_locked(store)),
            _ => Ok(()),
        }
    }

    /// The key held, for a read that goes on without one (e's `DiagnosticAccess`: doctor):
    /// never a refusal, never the shell told.
    fn open_session(
        &self,
        req: &VaultRequest<'_>,
    ) -> cli_core::Result<Option<Arc<dyn VaultSession>>> {
        #[cfg(test)]
        self.asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.look();
        Ok(self
            .session_for(req)
            .map(|session| Arc::new(session) as Arc<dyn VaultSession>))
    }

    fn writer(&self) -> String {
        format!("apprafter-desktop {}", env!("CARGO_PKG_VERSION"))
    }

    /// Every lock, the quit and a sleep (the lock hook): the epoch moves on — every session
    /// taken before refuses from now — and the key goes, its page unlocked, all under `held`'s
    /// lock.
    fn forget(&self) {
        let mut held = self.lock();
        self.shared.epoch.fetch_add(1, SeqCst);
        if let Some(old) = held.take() {
            self.release(old);
        }
    }
}

impl fmt::Debug for DesktopVaultAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let held = self.lock();
        f.debug_struct("DesktopVaultAccess")
            .field(
                "store",
                &held.as_ref().map(|k| k.store_id.as_str().to_string()),
            )
            .field("epoch", &self.shared.epoch.load(SeqCst))
            .field("key", &"<redacted>")
            .finish()
    }
}

/// A session over the key held, good until the next dropped key (ADR 0068 Consequences: while
/// the app is locked nothing in it decrypts a locked store's secrets). It holds the key
/// **weakly** and makes a `KeySession` for one call at a time, after O4's look and the epoch's
/// check: from `forget` on, every use refuses `store_locked`, and the key is already zeroed — no
/// running operation keeps it alive (the Rust verifier's finding 3). A call that began before
/// the lock ends as it began.
struct EpochSession {
    key: Weak<DataKey>,
    vault_id: VaultId,
    root: PathBuf,
    /// The epoch it was taken at.
    taken: u64,
    shared: Arc<Shared>,
}

impl fmt::Debug for EpochSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EpochSession")
            .field("root", &self.root)
            .field("taken", &self.taken)
            .field("key", &"<redacted>")
            .finish()
    }
}

impl EpochSession {
    /// The key for one call: after O4's look (which may lock the app, and so move the epoch),
    /// while no key was dropped since and the access still holds it.
    fn current(&self) -> cli_core::Result<KeySession> {
        if let Some(look) = self.shared.before_use.get() {
            look();
        }
        // The key first, then the epoch: a lock that lands before the check refuses this call
        // even while a write's retired key is still alive, so the check is the one point that
        // decides — a call that passed it began before the lock.
        let key = self.key.upgrade().ok_or_else(|| store_locked(&self.root))?;
        if self.shared.epoch.load(SeqCst) != self.taken {
            return Err(store_locked(&self.root));
        }
        Ok(KeySession::new(key, self.vault_id, self.root.clone()))
    }
}

impl VaultSession for EpochSession {
    fn hetzner_token(&self, vault: &[u8], target: &str) -> cli_core::Result<Option<SecretString>> {
        self.current()?.hetzner_token(vault, target)
    }

    fn open_cache(
        &self,
        vault: &[u8],
        secret: CachedSecret,
        target: &str,
        armored: &str,
    ) -> cli_core::Result<Zeroizing<String>> {
        self.current()?.open_cache(vault, secret, target, armored)
    }

    fn recipient(&self, vault: &[u8]) -> cli_core::Result<Recipient> {
        self.current()?.recipient(vault)
    }
}

/// One opening at a time, behind the app's back-off (module docs).
pub struct VaultGate {
    derive: Mutex<()>,
    backoff: Mutex<Backoff>,
    clock: Arc<dyn Clock>,
}

impl VaultGate {
    fn new(clock: Arc<dyn Clock>) -> Self {
        VaultGate {
            derive: Mutex::new(()),
            backoff: Mutex::new(Backoff::new()),
            clock,
        }
    }

    /// `open` — a vault opening with a typed secret — as one attempt: refused at once while
    /// another runs (`auth_busy`) or while the back-off refuses (`auth_failed`, `exhausted`,
    /// with how long it still will); a secret no slot takes and a mistyped recovery key each
    /// count as a failure. The failure that starts a refusal says so, with its length; before
    /// that a secret no slot takes is `auth_failed`, and a mistyped recovery key is the core's
    /// `recovery_key_invalid`, whose fields say where (the page words it with
    /// `recoveryKeyLine`, as Protect's confirm does: one wording, critic A finding 26). A
    /// success clears the count; any other error is the core's and counts nothing.
    fn attempt<T>(&self, open: impl FnOnce() -> CoreResult<T>) -> Result<T, Refusal> {
        let _one = match self.derive.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(DesktopError::AuthBusy.into()),
        };
        let now = self.clock.monotonic_ms();
        if let Err(refused) = self.backoff().allow(now) {
            return Err(failed(true, Some(refused.remaining_ms)));
        }
        match open() {
            Ok(opened) => {
                self.backoff().record_success();
                Ok(opened)
            }
            Err(e) => match wrong(&e) {
                Some(kind) => {
                    let now = self.clock.monotonic_ms();
                    let mut backoff = self.backoff();
                    backoff.record_failure(now);
                    match backoff.allow(now).err().map(|r| r.remaining_ms) {
                        // The back-off just started: its countdown is what to say.
                        Some(wait) => Err(failed(true, Some(wait))),
                        None if kind == Wrong::Mistyped => Err(DesktopError::Core(e).into()),
                        None => Err(failed(false, None)),
                    }
                }
                None => Err(DesktopError::Core(e).into()),
            },
        }
    }

    fn backoff(&self) -> MutexGuard<'_, Backoff> {
        self.backoff.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A wrong secret, which the back-off counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wrong {
    /// No slot takes it: the page's own words.
    NoSlot,
    /// A recovery key's shape with a typo: the core's fields say where, never what.
    Mistyped,
}

fn wrong(e: &CoreError) -> Option<Wrong> {
    match e {
        CoreError::Cli(cli_core::CliError::UnlockFailed { .. }) => Some(Wrong::NoSlot),
        CoreError::RecoveryKeyInvalid { .. } => Some(Wrong::Mistyped),
        _ => None,
    }
}

fn failed(exhausted: bool, retry_in_ms: Option<u64>) -> Refusal {
    DesktopError::AuthFailed {
        exhausted,
        retry_in_ms,
    }
    .into()
}

/// Where a confirmed plan finds the vault its gesture opened: Stop protecting's `execute_off`
/// takes it.
pub type OpenedSlot = Arc<Mutex<Option<OpenedVault>>>;

/// The store's secret as a plan's gesture (D.4a T76): opened through the gate against the
/// store `id`; what it opened goes to `slot` when the plan wants it, else it is dropped.
pub struct StoreSecretGesture {
    vault: Arc<DesktopVaultAccess>,
    root: PathBuf,
    id: StoreId,
    slot: Option<OpenedSlot>,
}

impl StoreSecretGesture {
    pub fn new(
        vault: Arc<DesktopVaultAccess>,
        root: PathBuf,
        id: StoreId,
        slot: Option<OpenedSlot>,
    ) -> Self {
        StoreSecretGesture {
            vault,
            root,
            id,
            slot,
        }
    }
}

impl SecretGesture for StoreSecretGesture {
    fn check(&self, secret: Zeroizing<String>) -> Result<(), Refusal> {
        let opened = self
            .vault
            .open_typed(&self.root, &self.id, SecretString::from(secret))?;
        if let Some(slot) = &self.slot {
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(opened);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::{Arc, Mutex};

    use apprafter_core::error::codes;
    use apprafter_core::secrets::testing::{locked_store, LockedTestStore};
    use apprafter_core::secrets::{StoreId, VaultAccess, VaultRequest};
    use apprafter_core::vault::VAULT_FILE;
    use apprafter_core::{CoreError, SecretString};

    use super::{DesktopVaultAccess, Pager};
    use crate::errors::DesktopError;
    use crate::ops::test_clock::ManualClock;

    const T0: u64 = 1_700_000_000_000;

    fn access() -> (Arc<ManualClock>, Arc<DesktopVaultAccess>) {
        let clock = Arc::new(ManualClock::at(T0));
        (clock.clone(), DesktopVaultAccess::new(clock))
    }

    fn req<'a>(root: &'a Path, id: &'a StoreId) -> VaultRequest<'a> {
        VaultRequest {
            store_root: root,
            store_id: id,
            slots: &[],
        }
    }

    fn key(store: &LockedTestStore) -> SecretString {
        store.recovery_key().clone()
    }

    fn other_id() -> StoreId {
        StoreId::parse("0123456789abcdef").unwrap()
    }

    #[test]
    fn the_recovery_key_opens_its_store_and_the_held_key_serves_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let (_, a) = access();
        let opened = a
            .open_typed(dir.path(), &store.store_id, key(&store))
            .unwrap();
        assert!(opened.vault_id == store.vault_id);
        assert!(a.hold(a.epoch(), store.store_id.clone(), opened));
        assert!(a.holds(&store.store_id));
        assert!(a.session(&req(dir.path(), &store.store_id)).is_ok());
        assert!(a.write_key(&req(dir.path(), &store.store_id)).is_ok());
        assert!(a
            .open_session(&req(dir.path(), &store.store_id))
            .unwrap()
            .is_some());
        // Another store's id (re-locked from a terminal: off, then on): this key is not its.
        assert!(!a.holds(&other_id()));
        assert!(a.session(&req(dir.path(), &other_id())).is_err());
        assert!(a
            .open_session(&req(dir.path(), &other_id()))
            .unwrap()
            .is_none());
    }

    #[test]
    fn with_no_key_a_read_is_store_locked_and_the_shell_is_told() {
        let (_, a) = access();
        let told = Arc::new(AtomicUsize::new(0));
        {
            let told = told.clone();
            a.on_missing(move || {
                told.fetch_add(1, SeqCst);
            });
        }
        let root = Path::new("/s");
        let e = a.session(&req(root, &other_id())).unwrap_err();
        assert!(matches!(e, cli_core::CliError::StoreLocked { .. }), "{e:?}");
        assert!(a.write_key(&req(root, &other_id())).is_err());
        assert_eq!(told.load(SeqCst), 2);
        // open_session asks nobody: a read that can go on without the key (doctor) does.
        assert!(a.open_session(&req(root, &other_id())).unwrap().is_none());
        assert_eq!(told.load(SeqCst), 2);
    }

    #[test]
    fn forget_drops_the_key_and_a_key_opened_before_it_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let (_, a) = access();
        let id = store.store_id.clone();
        let opened = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        let before = a.epoch();
        assert!(a.hold(before, id.clone(), opened));
        a.forget();
        assert!(!a.holds(&id) && !a.memory_locked());
        // Opened before the lock: not kept, whatever it opens.
        let opened = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        assert!(!a.hold(before, id.clone(), opened));
        assert!(!a.holds(&id));
        // The unlock's own keep (under the lock machine's lock) is not an epoch's.
        let opened = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        a.keep(id.clone(), opened);
        assert!(a.holds(&id));
    }

    /// ADR 0068 Consequences (critic B finding 9, the Rust verifier's finding 3): a session an
    /// operation took before a lock decrypts nothing after it — it refuses `store_locked`, as a
    /// read with no key does — even while a write key the core took before the lock still holds
    /// the key; and no session keeps the key alive: once that write is done it is gone.
    #[test]
    fn a_session_taken_before_a_lock_decrypts_nothing_after_it_and_keeps_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[("prod", Some("the-hcloud-token"))]);
        let (_, a) = access();
        let id = store.store_id.clone();
        let opened = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        let watch = Arc::downgrade(&opened.key);
        a.keep(id.clone(), opened);
        let session = a.session(&req(dir.path(), &id)).unwrap();
        let diagnostic = a.open_session(&req(dir.path(), &id)).unwrap().unwrap();
        let vault = std::fs::read(dir.path().join(VAULT_FILE)).unwrap();
        let token = session.hetzner_token(&vault, "prod").unwrap().unwrap();
        assert_eq!(token.expose(), "the-hcloud-token");
        // A write the core took before the lock (`prepare_write`) still holds the key.
        let writer = a.write_key(&req(dir.path(), &id)).unwrap();
        a.forget();
        for s in [&session, &diagnostic] {
            let e = s.hetzner_token(&vault, "prod").unwrap_err();
            assert!(matches!(e, cli_core::CliError::StoreLocked { .. }), "{e:?}");
            assert!(s.recipient(&vault).is_err());
        }
        drop(writer);
        assert!(
            watch.upgrade().is_none(),
            "a session kept the key alive after the lock"
        );
    }

    /// The Rust verifier's finding 2: the epoch moves only when a key is dropped — a re-read of
    /// a store in files (no key held) moves nothing, so a Protect plan keeps its key (T64).
    #[test]
    fn drop_key_moves_the_epoch_only_when_a_key_was_held() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let (_, a) = access();
        let id = store.store_id.clone();
        let before = a.epoch();
        assert!(!a.drop_key());
        assert_eq!(a.epoch(), before, "a re-read with no key moved the epoch");
        let opened = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        a.keep(id.clone(), opened);
        assert!(a.drop_key());
        assert!(a.epoch() != before && !a.holds(&id));
    }

    /// O4 between ticks: every session's use, and every new session or write key, asks the
    /// shell's look first (Task 15 wires the lock machine's `check_sleep` to it).
    #[test]
    fn every_use_looks_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let (_, a) = access();
        let looks = Arc::new(AtomicUsize::new(0));
        {
            let looks = looks.clone();
            a.before_use(move || {
                looks.fetch_add(1, SeqCst);
            });
        }
        let id = store.store_id.clone();
        let opened = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        a.keep(id.clone(), opened);
        let session = a.session(&req(dir.path(), &id)).unwrap();
        assert_eq!(looks.load(SeqCst), 1);
        let vault = std::fs::read(dir.path().join(VAULT_FILE)).unwrap();
        session.recipient(&vault).unwrap();
        assert_eq!(looks.load(SeqCst), 2);
        a.open_session(&req(dir.path(), &id)).unwrap();
        a.write_key(&req(dir.path(), &id)).unwrap();
        assert_eq!(looks.load(SeqCst), 4);
    }

    /// A pager that answers `takes` and records what it was asked, in order.
    struct FakePager {
        takes: bool,
        asked: Mutex<Vec<(&'static str, usize)>>,
    }

    impl Pager for FakePager {
        fn lock(&self, bytes: &[u8]) -> bool {
            self.asked
                .lock()
                .unwrap()
                .push(("lock", bytes.as_ptr() as usize));
            self.takes
        }

        fn unlock(&self, bytes: &[u8]) {
            self.asked
                .lock()
                .unwrap()
                .push(("unlock", bytes.as_ptr() as usize));
        }
    }

    fn paged(takes: bool) -> (Arc<FakePager>, Arc<DesktopVaultAccess>) {
        let pager = Arc::new(FakePager {
            takes,
            asked: Mutex::new(Vec::new()),
        });
        let a = DesktopVaultAccess::with_pager(Arc::new(ManualClock::at(T0)), pager.clone());
        (pager, a)
    }

    /// T77's `memory_locked` is what the OS answered for the key held, and nothing with no key
    /// (critic A finding 6, B 5); the old page goes before the new one locks, since `munlock`
    /// does not nest (critic A finding 20).
    #[test]
    fn memory_locked_is_the_held_key_s_page_as_the_pager_answered() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let id = store.store_id.clone();
        let (pager, a) = paged(true);
        assert!(!a.memory_locked());
        let first = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        let at_first = first.key.expose().as_ptr() as usize;
        a.keep(id.clone(), first);
        assert!(a.memory_locked());
        let second = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        let at_second = second.key.expose().as_ptr() as usize;
        a.keep(id.clone(), second);
        assert_eq!(
            *pager.asked.lock().unwrap(),
            vec![
                ("lock", at_first),
                ("unlock", at_first),
                ("lock", at_second)
            ]
        );
        a.forget();
        assert!(!a.memory_locked());
        assert_eq!(
            pager.asked.lock().unwrap().last(),
            Some(&("unlock", at_second))
        );
        // Where the OS refuses, it is said: no key page claimed locked.
        let (_, refused) = paged(false);
        let opened = refused.open_typed(dir.path(), &id, key(&store)).unwrap();
        refused.keep(id, opened);
        assert!(!refused.memory_locked());
    }

    /// A password typed where the store has only a recovery-key slot: one failed attempt each,
    /// with no derivation.
    #[test]
    fn three_wrong_secrets_start_the_back_off_and_it_ends_on_the_monotonic_clock() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let (clock, a) = access();
        let id = &store.store_id;
        let wrong = || SecretString::new("not the lock password");
        for i in 0..2 {
            let r = a.open_typed(dir.path(), id, wrong()).unwrap_err();
            assert!(
                matches!(
                    *r.error,
                    DesktopError::AuthFailed {
                        exhausted: false,
                        retry_in_ms: None
                    }
                ),
                "{i}: {r:?}"
            );
        }
        let third = a.open_typed(dir.path(), id, wrong()).unwrap_err();
        assert!(
            matches!(
                *third.error,
                DesktopError::AuthFailed {
                    exhausted: true,
                    retry_in_ms: Some(30_000)
                }
            ),
            "{third:?}"
        );
        // Turned away unchecked, the right key too, saying how long it still refuses.
        clock.advance(10_000);
        let held = a.open_typed(dir.path(), id, key(&store)).unwrap_err();
        assert!(
            matches!(
                *held.error,
                DesktopError::AuthFailed {
                    exhausted: true,
                    retry_in_ms: Some(20_000)
                }
            ),
            "{held:?}"
        );
        clock.advance(20_000);
        assert!(a.open_typed(dir.path(), id, key(&store)).is_ok());
        // A success starts the count over.
        let after = a.open_typed(dir.path(), id, wrong()).unwrap_err();
        assert!(matches!(
            *after.error,
            DesktopError::AuthFailed {
                exhausted: false,
                ..
            }
        ));
    }

    /// One character changed in the key's last line: the checksum fails, the password slots
    /// (none here) refuse it too, and the refusal is the core's `recovery_key_invalid`, whose
    /// fields say where and never what was typed (the page words it: critic A finding 26). It
    /// counts as a failure.
    #[test]
    fn a_mistyped_recovery_key_says_where_never_what() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let (_, a) = access();
        let shown = store.recovery_key().expose().to_string();
        let mut chars: Vec<char> = shown.chars().collect();
        let at = shown.rfind('\n').map_or(0, |i| i + 3);
        chars[at] = if chars[at] == 'Q' { 'P' } else { 'Q' };
        let typed: String = chars.into_iter().collect();
        let r = a
            .open_typed(
                dir.path(),
                &store.store_id,
                SecretString::new(typed.clone()),
            )
            .unwrap_err();
        assert!(
            matches!(
                &*r.error,
                DesktopError::Core(CoreError::RecoveryKeyInvalid { .. })
            ),
            "{r:?}"
        );
        let ui = r.to_ui();
        assert_eq!(ui.code.as_deref(), Some(codes::RECOVERY_KEY_INVALID));
        assert!(ui.fields.contains_key("reason"), "{ui:?}");
        let last = typed.lines().last().unwrap();
        assert!(!format!("{ui:?}").contains(last), "{ui:?}");
        // It counted: the second wrong secret after it starts the back-off.
        let wrong = || SecretString::new("not the lock password");
        let id = &store.store_id;
        let second = a.open_typed(dir.path(), id, wrong()).unwrap_err();
        assert!(
            matches!(
                *second.error,
                DesktopError::AuthFailed {
                    exhausted: false,
                    ..
                }
            ),
            "{second:?}"
        );
        let third = a.open_typed(dir.path(), id, wrong()).unwrap_err();
        assert!(
            matches!(
                *third.error,
                DesktopError::AuthFailed {
                    exhausted: true,
                    ..
                }
            ),
            "{third:?}"
        );
    }

    #[test]
    fn one_opening_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let (_, a) = access();
        let _deriving = a.gate.derive.lock().unwrap();
        let r = a
            .open_typed(dir.path(), &store.store_id, key(&store))
            .unwrap_err();
        assert!(matches!(*r.error, DesktopError::AuthBusy), "{r:?}");
    }

    #[test]
    fn the_writer_names_the_desktop_and_debug_shows_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let (_, a) = access();
        let opened = a
            .open_typed(dir.path(), &store.store_id, key(&store))
            .unwrap();
        let hex: String = opened
            .key
            .expose()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        a.keep(store.store_id.clone(), opened);
        let shown = format!("{a:?}");
        assert!(
            !shown.contains(&hex) && shown.contains("<redacted>"),
            "{shown}"
        );
        assert_eq!(
            a.writer(),
            format!("apprafter-desktop {}", env!("CARGO_PKG_VERSION"))
        );
    }

    /// The final verifier's finding 8: the re-read keeps the key only if it is the store's,
    /// and drops another's, in one step; the epoch moves only when a key went.
    #[test]
    fn keep_only_keeps_the_store_s_key_and_drops_another_s() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let (_, a) = access();
        let id = store.store_id.clone();
        let before = a.epoch();
        assert!(!a.keep_only(&id));
        assert_eq!(a.epoch(), before, "nothing held, nothing moved");
        let opened = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        a.keep(id.clone(), opened);
        assert!(a.keep_only(&id) && a.holds(&id));
        assert_eq!(a.epoch(), before);
        assert!(!a.keep_only(&other_id()));
        assert!(!a.holds(&id) && a.epoch() != before);
    }

    /// The final verifier's finding 12: a write the core took a key for before a lock keeps
    /// that key's page pinned until the write ends — the lock does not unlock it — and the key
    /// goes with the write.
    #[test]
    fn a_write_taken_before_a_lock_keeps_its_key_pinned_until_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let id = store.store_id.clone();
        let (pager, a) = paged(true);
        let opened = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        let at = opened.key.expose().as_ptr() as usize;
        let watch = Arc::downgrade(&opened.key);
        a.keep(id.clone(), opened);
        let writer = a.write_key(&req(dir.path(), &id)).unwrap();
        a.forget();
        assert!(!a.holds(&id) && !a.memory_locked());
        assert_eq!(
            *pager.asked.lock().unwrap(),
            vec![("lock", at)],
            "unpinned under a write"
        );
        assert!(watch.upgrade().is_some());
        drop(writer);
        assert_eq!(pager.asked.lock().unwrap().last(), Some(&("unlock", at)));
        assert!(watch.upgrade().is_none(), "the key outlived its write");
    }

    /// `munlock` does not nest: when a write's key goes after the app unlocked again, the page
    /// of the key held now is locked again (the two may share it).
    #[test]
    fn the_key_held_now_is_pinned_again_when_a_write_s_key_goes() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let id = store.store_id.clone();
        let (pager, a) = paged(true);
        let first = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        let at_first = first.key.expose().as_ptr() as usize;
        a.keep(id.clone(), first);
        let writer = a.write_key(&req(dir.path(), &id)).unwrap();
        a.forget();
        let second = a.open_typed(dir.path(), &id, key(&store)).unwrap();
        let at_second = second.key.expose().as_ptr() as usize;
        a.keep(id.clone(), second);
        drop(writer);
        let asked = pager.asked.lock().unwrap().clone();
        assert_eq!(
            asked[asked.len() - 2..],
            [("unlock", at_first), ("lock", at_second)],
            "{asked:?}"
        );
        assert!(a.memory_locked() && a.holds(&id));
    }

    /// The final verifier's finding 12: a change waiting for the store's lock asks after each
    /// slice; it goes on while the shell allows, and is abandoned `store_locked` once not.
    #[test]
    fn a_change_waits_for_the_store_only_while_the_shell_allows() {
        let (_, a) = access();
        let root = Path::new("/s");
        assert_eq!(a.store_wait_slice(), Some(super::STORE_WAIT_SLICE));
        assert!(a.keep_waiting(root).is_ok(), "with no hook it may wait");
        let unlocked = Arc::new(std::sync::atomic::AtomicBool::new(true));
        {
            let unlocked = unlocked.clone();
            a.may_wait(move || unlocked.load(SeqCst));
        }
        assert!(a.keep_waiting(root).is_ok());
        unlocked.store(false, SeqCst);
        let e = a.keep_waiting(root).unwrap_err();
        assert!(matches!(e, cli_core::CliError::StoreLocked { .. }), "{e:?}");
    }
}
