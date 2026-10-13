// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A holder watches its store (D.4a T56; team decision 2026-10-13). It serves only while
//! `<root>/store.yaml` says the store is `locked` with the id the holder was started for, and
//! reads the file through the core's own reader (`secrets::read_store_file`), never a parser of
//! its own: at every request (`serve::handle`) and, between requests, at once after it binds
//! and then every [`STORE_LOOK`], on the window's clock ([`HolderClock`]: HG1 keeps `Instant`
//! out of `holder/`). A file that is gone or cannot be read, any `secrets:` but `locked` and
//! another id end the holder at once. So `lock off` in any terminal (whose first step writes
//! `unlocking`), a store copied over or a deleted `store.yaml` closes every window of that
//! store within about [`STORE_LOOK`], with no retired id kept in `store.yaml` and no request
//! from one terminal to another's holder. Until its next look a holder still holds the key of a
//! store no longer locked with it, and serves nothing: a request re-reads first.

use std::time::Duration;

use apprafter_core::secrets::{read_store_file, SecretsMode, StoreFile};

use super::protocol::StoreBinding;
use super::window::HolderClock;

/// How often a holder re-reads its `store.yaml` between requests.
pub const STORE_LOOK: Duration = Duration::from_secs(5);

/// Whether `read` — `store.yaml` as the core read it — says the store is still `locked` with
/// `id`. A file that is gone (`None`) or cannot be read, `files`, `locking`, `unlocking` and
/// another id each say no.
pub fn still_locked(read: &cli_core::Result<Option<StoreFile>>, id: &str) -> bool {
    match read {
        Ok(Some(file)) => {
            file.secrets == SecretsMode::Locked
                && file.id.as_ref().is_some_and(|own| own.as_str() == id)
        }
        Ok(None) | Err(_) => false,
    }
}

/// `store`'s `store.yaml`, read now: whether the store is still locked with its id.
pub fn store_holds(store: &StoreBinding) -> bool {
    still_locked(&read_store_file(&store.root), &store.id)
}

/// The holder loop's watch: [`store_holds`] again once [`STORE_LOOK`] has passed since its last
/// look, and at once the first time.
#[derive(Debug, Clone)]
pub struct StoreWatch {
    store: StoreBinding,
    /// The window clock's `with_sleep` at the last look; `None` before the first.
    last: Option<Duration>,
}

impl StoreWatch {
    pub fn new(store: &StoreBinding) -> StoreWatch {
        StoreWatch {
            store: store.clone(),
            last: None,
        }
    }

    /// How long the loop may wait before the next look is due: zero before the first one.
    pub fn until_next(&self, clock: &impl HolderClock) -> Duration {
        match self.last {
            None => Duration::ZERO,
            Some(at) => STORE_LOOK.saturating_sub(clock.with_sleep().saturating_sub(at)),
        }
    }

    /// The loop's look: the store re-read when a look is due; until then the last look's
    /// answer stands, and it was yes (a no ends the holder).
    pub fn holds(&mut self, clock: &impl HolderClock) -> bool {
        if !self.until_next(clock).is_zero() {
            return true;
        }
        self.last = Some(clock.with_sleep());
        store_holds(&self.store)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;
    use std::path::Path;

    use apprafter_core::secrets::testing::locked_store;
    use apprafter_core::secrets::{write_store_file, StoreId, STORE_FILE, STORE_FILE_FORMAT};
    use apprafter_core::vault::{OsRandom, VAULT_FILE};
    use cli_core::{CliError, StoreLock, TargetStorePaths};

    use super::*;

    const S: Duration = Duration::from_secs(1);
    const OWN: &str = "0123456789abcdef";
    const OTHER: &str = "fedcba9876543210";

    /// The window's clock, by hand: no sleep, so both readings move together.
    struct Fake(Cell<Duration>);

    impl HolderClock for Fake {
        fn with_sleep(&self) -> Duration {
            self.0.get()
        }
        fn awake_only(&self) -> Duration {
            self.0.get()
        }
    }

    impl Fake {
        fn pass(&self, d: Duration) {
            self.0.set(self.0.get() + d);
        }
    }

    /// `root`'s `store.yaml` as `change` leaves it (team decision 2026-10-13): `files` is `lock
    /// off`'s last two steps (the vault deleted, then `files`); `another id` another store's
    /// `store.yaml` copied over it; `removed` no file at all.
    pub(crate) fn change_store(root: &Path, change: &str) {
        if change == "removed" {
            std::fs::remove_file(root.join(STORE_FILE)).unwrap();
            return;
        }
        let mut file = read_store_file(root).unwrap().expect("a locked store");
        match change {
            "files" => {
                std::fs::remove_file(root.join(VAULT_FILE)).unwrap();
                file = StoreFile {
                    secrets: SecretsMode::Files,
                    id: None,
                    slots: Vec::new(),
                    unlock_timeout_min: None,
                    ..file
                };
            }
            "another id" => file.id = Some(StoreId::generate(&mut OsRandom).unwrap()),
            other => panic!("no change {other}"),
        }
        let held = StoreLock::exclusive(&TargetStorePaths::for_root(root.to_path_buf())).unwrap();
        write_store_file(root, &file, &held).unwrap();
    }

    fn read(secrets: SecretsMode, id: Option<&str>) -> cli_core::Result<Option<StoreFile>> {
        Ok(Some(StoreFile {
            format: STORE_FILE_FORMAT,
            secrets,
            id: id.map(|i| StoreId::parse(i).unwrap()),
            slots: Vec::new(),
            unlock_timeout_min: None,
        }))
    }

    /// Team decision 2026-10-13: a holder serves only a store that `store.yaml` says is
    /// `locked` with the holder's own id — gone, unreadable, `files` (even one naming the id),
    /// `locking`, `unlocking` and another id each end it.
    #[test]
    fn still_locked_only_while_locked_with_its_own_id() {
        assert!(still_locked(&read(SecretsMode::Locked, Some(OWN)), OWN));
        for (store_yaml, why) in [
            (read(SecretsMode::Locked, Some(OTHER)), "another id"),
            (read(SecretsMode::Files, Some(OWN)), "files naming the id"),
            (read(SecretsMode::Files, None), "files"),
            (read(SecretsMode::Locking, Some(OWN)), "locking"),
            (read(SecretsMode::Unlocking, Some(OWN)), "unlocking"),
            (Ok(None), "gone"),
            (Err(CliError::Other("unreadable".into())), "unreadable"),
        ] {
            assert!(!still_locked(&store_yaml, OWN), "{why}");
        }
    }

    /// The loop's look (team decision 2026-10-13): due at once, then every [`STORE_LOOK`] on
    /// the window's clock and never in between; the look that is due after the store changed
    /// sees it, through the core's reader.
    #[test]
    fn the_loop_looks_at_once_then_every_five_seconds() {
        for change in ["files", "another id", "removed"] {
            // No socket here, and these tests run on every OS (Windows has no `/tmp`): the
            // system's temporary directory.
            let dir = tempfile::tempdir().unwrap();
            let made = locked_store(dir.path(), &[("prod", None)]);
            let store = StoreBinding {
                id: made.store_id.as_str().into(),
                root: dir.path().to_path_buf(),
            };
            let c = Fake(Cell::new(Duration::from_secs(100)));
            let mut w = StoreWatch::new(&store);
            assert_eq!(
                w.until_next(&c),
                Duration::ZERO,
                "the first look is due at once"
            );
            assert!(w.holds(&c), "{change}: locked with its id");
            assert_eq!(w.until_next(&c), STORE_LOOK);
            change_store(dir.path(), change);
            assert!(!store_holds(&store), "{change}: a read now sees it");
            c.pass(STORE_LOOK - S);
            assert_eq!(w.until_next(&c), S);
            assert!(w.holds(&c), "{change}: no look before one is due");
            c.pass(S);
            assert!(!w.holds(&c), "{change}: the look that is due sees it");
        }
    }
}
