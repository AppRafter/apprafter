// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Verified provider tokens, held in Rust while the add wizard runs (design decision 6).
//!
//! The wizard sends its token across IPC once, to `op_start_verify_token`; once the provider
//! accepts it, it waits here as a draft named by a [`DraftId`]: the catalogue read and the add
//! plan take the id, never the token. A draft lives [`DRAFT_TTL_MS`], measured as plans are (a
//! suspend counts, a wall clock stepped back does not), until the plan that uses it takes it,
//! the page discards it, or the app locks or quits. The token is a [`SecretString`], wiped when
//! the draft goes. A verify under way when the app locked keeps nothing: it took the
//! [`DraftEpoch`] at its start, every lock moves the epoch on, and an insert under an old epoch
//! drops its token.
//!
//! Lock order: the lock machine's hook calls [`DraftStore::drop_all`] under the machine's lock;
//! nothing here calls back into the lock machine or the operation manager.

use std::collections::HashMap;
use std::fmt;
use std::mem;
use std::sync::{Arc, Mutex, MutexGuard};

use apprafter_core::SecretString;
use apprafter_desktop_ipc::DraftId;

use crate::errors::DesktopError;
use crate::ops::{Clock, Stamp, PLAN_TTL_MS};

/// How long a draft waits for its plan.
pub const DRAFT_TTL_MS: u64 = PLAN_TTL_MS;

/// The lock period a verify started in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftEpoch(u64);

pub struct DraftStore {
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    next: u64,
    epoch: u64,
    drafts: HashMap<DraftId, Draft>,
}

struct Draft {
    provider: String,
    token: SecretString,
    created: Stamp,
}

impl DraftStore {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            inner: Mutex::default(),
        }
    }

    /// Taken when a verify starts; [`insert`](Self::insert) keeps nothing under an old one.
    pub fn epoch(&self) -> DraftEpoch {
        DraftEpoch(self.lock().epoch)
    }

    /// Keep a verified token. `None` — the token dropped — when the app locked since `epoch`.
    pub fn insert(
        &self,
        epoch: DraftEpoch,
        provider: String,
        token: SecretString,
    ) -> Option<DraftId> {
        let created = Stamp::now(&*self.clock);
        let mut inner = self.lock();
        if inner.epoch != epoch.0 {
            return None;
        }
        inner.next += 1;
        let id = DraftId(inner.next);
        inner.drafts.insert(
            id,
            Draft {
                provider,
                token,
                created,
            },
        );
        Some(id)
    }

    /// The draft's provider and a copy of its token; the draft stays.
    pub fn get(&self, id: DraftId) -> Result<(String, SecretString), DesktopError> {
        let mut inner = self.lock();
        let draft = self.live(&mut inner, id)?;
        Ok((draft.provider.clone(), draft.token.clone()))
    }

    /// The draft's provider and token; the draft goes (its plan holds the token now).
    pub fn take(&self, id: DraftId) -> Result<(String, SecretString), DesktopError> {
        let mut inner = self.lock();
        self.live(&mut inner, id)?;
        let draft = inner
            .drafts
            .remove(&id)
            .expect("live() found it under this lock");
        Ok((draft.provider, draft.token))
    }

    /// The page is done with it (the wizard closed). An unknown id is no error.
    pub fn discard(&self, id: DraftId) {
        let gone = self.lock().drafts.remove(&id);
        drop(gone);
    }

    /// The app locked or is quitting: every draft goes, and so does any verify still running.
    pub fn drop_all(&self) {
        let mut inner = self.lock();
        inner.epoch += 1;
        let gone = mem::take(&mut inner.drafts);
        drop(inner);
        drop(gone);
    }

    /// The idle tick: the drafts past their time go.
    pub fn sweep(&self) {
        let clock = &*self.clock;
        let mut inner = self.lock();
        let gone: Vec<Draft> = inner
            .drafts
            .extract_if(|_, d| d.created.elapsed_ms(clock) > DRAFT_TTL_MS)
            .map(|(_, d)| d)
            .collect();
        drop(inner);
        drop(gone);
    }

    /// The draft, unless missing or expired (an expired one goes now).
    fn live<'a>(&self, inner: &'a mut Inner, id: DraftId) -> Result<&'a Draft, DesktopError> {
        let expired = match inner.drafts.get(&id) {
            None => return Err(DesktopError::DraftNotFound { draft_id: id }),
            Some(draft) => draft.created.elapsed_ms(&*self.clock) > DRAFT_TTL_MS,
        };
        if expired {
            inner.drafts.remove(&id);
            return Err(DesktopError::DraftExpired { draft_id: id });
        }
        Ok(&inner.drafts[&id])
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl fmt::Debug for DraftStore {
    /// How many drafts wait; never a provider or a token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DraftStore")
            .field("drafts", &self.lock().drafts.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apprafter_core::SecretString;
    use apprafter_desktop_ipc::DraftId;

    use super::{DraftStore, DRAFT_TTL_MS};
    use crate::errors::DesktopError;
    use crate::ops::test_clock::ManualClock;

    const T0: u64 = 1_700_000_000_000;

    /// Token-shaped, nobody's.
    fn token(c: char) -> SecretString {
        SecretString::new(c.to_string().repeat(64))
    }

    fn store() -> (Arc<ManualClock>, DraftStore) {
        let clock = Arc::new(ManualClock::at(T0));
        (clock.clone(), DraftStore::new(clock))
    }

    #[test]
    fn a_draft_holds_its_provider_and_token_until_taken() {
        let (_, drafts) = store();
        let id = drafts
            .insert(drafts.epoch(), "hetzner-cloud".into(), token('k'))
            .unwrap();
        let other = drafts
            .insert(drafts.epoch(), "hetzner-cloud".into(), token('m'))
            .unwrap();
        assert_ne!(id, other);
        let (provider, got) = drafts.get(id).unwrap();
        assert_eq!(
            (provider.as_str(), got.expose()),
            ("hetzner-cloud", "k".repeat(64).as_str())
        );
        assert!(drafts.get(id).is_ok(), "get leaves it");
        assert_eq!(drafts.take(id).unwrap().1.expose(), "k".repeat(64));
        assert!(
            matches!(drafts.take(id), Err(DesktopError::DraftNotFound { draft_id }) if draft_id == id)
        );
        assert_eq!(
            drafts.get(other).unwrap().1.expose(),
            "m".repeat(64),
            "taking one leaves the other"
        );
    }

    #[test]
    fn a_draft_expires_after_its_ttl_and_a_suspend_counts() {
        let (clock, drafts) = store();
        let id = drafts
            .insert(drafts.epoch(), "hetzner-cloud".into(), token('k'))
            .unwrap();
        clock.advance(DRAFT_TTL_MS);
        assert!(drafts.get(id).is_ok(), "alive for its whole time to live");
        clock.advance(1);
        assert!(matches!(
            drafts.get(id),
            Err(DesktopError::DraftExpired { draft_id }) if draft_id == id
        ));
        assert!(
            matches!(drafts.get(id), Err(DesktopError::DraftNotFound { .. })),
            "gone once expired"
        );
        let id = drafts
            .insert(drafts.epoch(), "hetzner-cloud".into(), token('m'))
            .unwrap();
        let now = T0 + DRAFT_TTL_MS + 1;
        clock.set_wall(now + DRAFT_TTL_MS + 10); // a night with the lid closed
        assert!(matches!(
            drafts.take(id),
            Err(DesktopError::DraftExpired { .. })
        ));
    }

    #[test]
    fn a_lock_drops_every_draft_and_a_verify_from_before_it_keeps_nothing() {
        let (_, drafts) = store();
        let before = drafts.epoch();
        let id = drafts
            .insert(before, "hetzner-cloud".into(), token('k'))
            .unwrap();
        drafts.drop_all();
        assert!(matches!(
            drafts.get(id),
            Err(DesktopError::DraftNotFound { .. })
        ));
        assert_eq!(
            drafts.insert(before, "hetzner-cloud".into(), token('m')),
            None
        );
        assert!(drafts
            .insert(drafts.epoch(), "hetzner-cloud".into(), token('m'))
            .is_some());
    }

    #[test]
    fn discard_and_sweep_drop_drafts_and_an_unknown_id_is_no_error() {
        let (clock, drafts) = store();
        let old = drafts
            .insert(drafts.epoch(), "hetzner-cloud".into(), token('k'))
            .unwrap();
        clock.advance(DRAFT_TTL_MS / 2);
        let fresh = drafts
            .insert(drafts.epoch(), "hetzner-cloud".into(), token('m'))
            .unwrap();
        clock.advance(DRAFT_TTL_MS / 2 + 1);
        drafts.sweep();
        assert_eq!(
            format!("{drafts:?}"),
            "DraftStore { drafts: 1, .. }",
            "swept"
        );
        assert!(matches!(
            drafts.get(old),
            Err(DesktopError::DraftNotFound { .. })
        ));
        assert!(drafts.get(fresh).is_ok());
        drafts.discard(fresh);
        drafts.discard(DraftId(999));
        assert!(matches!(
            drafts.get(fresh),
            Err(DesktopError::DraftNotFound { .. })
        ));
    }

    #[test]
    fn debug_never_shows_a_token() {
        let (_, drafts) = store();
        drafts
            .insert(drafts.epoch(), "hetzner-cloud".into(), token('k'))
            .unwrap();
        let shown = format!("{drafts:?}");
        assert!(!shown.contains(&"k".repeat(64)), "{shown}");
        assert!(!shown.contains("hetzner-cloud"), "{shown}");
        assert!(shown.contains("drafts: 1"), "{shown}");
    }
}
