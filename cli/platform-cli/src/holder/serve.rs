// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! One request, served (D.4a §3.10, T48, T53, T56). OS-neutral: the window first — an expired or
//! slept one refuses and ends the holder — then the store: `store.yaml`, re-read at every
//! request through the core's reader, must still say `locked` with this holder's id, else the
//! request is refused as an ended window and the holder ends (`watch`; team decision
//! 2026-10-13) — then the vault the client read, opened with a
//! per-request copy of the key through the core's `KeySession::for_store`, which refuses a
//! vault whose header names another store before it opens anything. The holder names no vault
//! door of its own (c2's T16 guard over the CLI; d4a-d's
//! `only_the_secret_store_and_the_lock_open_a_vault`, which bans `VaultFile` and `vault::file`
//! from `platform-cli/src`). Each failure is one of §3.10's refusals; none says a vault is
//! damaged for good — only an in-process open may (rule 3), so the client ends the holder and
//! prompts (C3).

use std::sync::Arc;
use std::time::Duration;

use apprafter_core::lock::HolderHardening;
use apprafter_core::secrets::{KeySession, VaultSession};
use apprafter_core::vault::{DataKey, VAULT_FILE_MAX};
use apprafter_core::Zeroizing;
use cli_core::error::VaultProblem;
use cli_core::CliError;

use super::protocol::{
    HolderItem, HolderStatus, InitHeader, Refusal, Reply, Request, MAX_ARMORED_CACHE, PROTOCOL,
    VERSION,
};
use super::watch::store_holds;
use super::window::{HolderClock, Window, WindowState};

/// What a holder holds while it serves: its key (in its locked page), its init, its protections.
pub(crate) struct Held<'a> {
    pub(crate) key: &'a [u8; 32],
    pub(crate) init: &'a InitHeader,
    pub(crate) hardening: HolderHardening,
}

/// A reply, and whether the holder ends once it is sent.
#[derive(Debug)]
pub(crate) struct Served {
    pub(crate) reply: Reply,
    pub(crate) end: bool,
}

fn keep(reply: Reply) -> Served {
    Served { reply, end: false }
}

pub(crate) fn handle(
    req: Request,
    held: &Held<'_>,
    window: &mut Window,
    clock: &impl HolderClock,
) -> Served {
    let left = match window.check(clock) {
        WindowState::Open { left } => left,
        WindowState::Expired | WindowState::Slept => {
            return Served {
                reply: Reply::Refused(Refusal::WindowEnded),
                end: true,
            }
        }
    };
    // T56, team decision 2026-10-13: the store is re-read before anything is served. One no
    // longer locked with this holder's id — `lock off` ran, another store's `store.yaml` was
    // copied over, the file is gone — gets nothing, and the holder ends as an ended window does.
    if !store_holds(&held.init.store) {
        return Served {
            reply: Reply::Refused(Refusal::WindowEnded),
            end: true,
        };
    }
    match req {
        Request::Status => keep(Reply::Status(held.status(left))),
        Request::End => Served {
            reply: Reply::Ended,
            end: true,
        },
        Request::OpenItem {
            vault,
            item: HolderItem::HetznerToken { target },
        } => match held.session(&vault) {
            Err(r) => keep(Reply::Refused(r)),
            Ok(s) => match s.hetzner_token(&vault, &target) {
                Ok(Some(token)) => {
                    window.touch(clock);
                    keep(Reply::Item(Zeroizing::new(
                        token.expose().as_bytes().to_vec(),
                    )))
                }
                Ok(None) => {
                    window.touch(clock);
                    keep(Reply::Absent)
                }
                Err(e) => keep(Reply::Refused(refusal_of(&e))),
            },
        },
        Request::OpenCache {
            vault,
            secret,
            target,
            armored,
        } => {
            if armored.len() > MAX_ARMORED_CACHE {
                return keep(Reply::Refused(Refusal::Malformed));
            }
            let Ok(armored) = String::from_utf8(armored) else {
                return keep(Reply::Refused(Refusal::Malformed));
            };
            match held.session(&vault) {
                Err(r) => keep(Reply::Refused(r)),
                Ok(s) => match s.open_cache(&vault, secret, &target, &armored) {
                    Ok(plain) => {
                        window.touch(clock);
                        keep(Reply::Item(Zeroizing::new(plain.as_bytes().to_vec())))
                    }
                    Err(e) => keep(Reply::Refused(refusal_of(&e))),
                },
            }
        }
        Request::Recipient { vault } => match held.session(&vault) {
            Err(r) => keep(Reply::Refused(r)),
            Ok(s) => match s.recipient(&vault) {
                Ok(r) => keep(Reply::Recipient(r.to_string())),
                Err(e) => keep(Reply::Refused(refusal_of(&e))),
            },
        },
    }
}

impl Held<'_> {
    fn status(&self, left: Duration) -> HolderStatus {
        HolderStatus {
            protocol: PROTOCOL,
            version: VERSION.into(),
            store_id: self.init.store.id.clone(),
            vault_id: self.init.vault_id,
            left_secs: left.as_secs(),
            minutes: self.init.minutes,
            hardening: self.hardening,
        }
    }

    /// A session over a copy of the key for this one request, for this holder's store only: the
    /// core checks that the vault's header names it before it opens anything, so a copied
    /// store's vault never opens here (T50) — `vault_unreadable{other_store}`, which
    /// [`refusal_of`] turns into `WrongStore`; one that does not parse is `VaultDamaged`.
    fn session(&self, vault: &[u8]) -> Result<KeySession, Refusal> {
        if vault.len() > VAULT_FILE_MAX {
            return Err(Refusal::Malformed);
        }
        Ok(KeySession::for_store(
            Arc::new(DataKey::from_bytes(self.key)),
            self.init.vault_id,
            self.init.store.root.clone(),
            self.init.store.id.clone(),
        ))
    }
}

/// §3.10's refusal for what the core's session said: a key for another vault (the vault was
/// rekeyed, or turned off and on) is `KeyMismatch`; a cache for another key is that cache's
/// refusal; anything else `VaultDamaged`, which the client never reports as damage (C3).
fn refusal_of(e: &CliError) -> Refusal {
    match e {
        CliError::StoreChanged { .. } => Refusal::KeyMismatch,
        CliError::VaultUnreadable {
            problem: VaultProblem::OtherStore,
            ..
        } => Refusal::WrongStore,
        CliError::CacheUndecryptable { reason, detail, .. } => Refusal::CacheRefused {
            reason: *reason,
            detail: detail.clone(),
        },
        _ => Refusal::VaultDamaged,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use apprafter_core::secrets::testing::{locked_store, LockedTestStore};
    use apprafter_core::vault::VaultId;
    use cli_core::secrets::{encrypt_for_recipient, CachedSecret, Identity};

    use super::*;
    use crate::holder::protocol::StoreBinding;
    use crate::holder::watch::tests::change_store;

    const TOKEN: &str = "abababababababababababababababababababababababababababababababab";

    struct Fake(Cell<Duration>, Cell<Duration>);
    impl HolderClock for Fake {
        fn with_sleep(&self) -> Duration {
            self.0.get()
        }
        fn awake_only(&self) -> Duration {
            self.1.get()
        }
    }
    impl Fake {
        fn new() -> Fake {
            Fake(
                Cell::new(Duration::from_secs(100)),
                Cell::new(Duration::from_secs(100)),
            )
        }
        fn pass(&self, d: Duration) {
            self.0.set(self.0.get() + d);
            self.1.set(self.1.get() + d);
        }
    }

    struct World {
        _dir: tempfile::TempDir,
        store: LockedTestStore,
        init: InitHeader,
        vault: Vec<u8>,
    }

    fn world(targets: &[(&str, Option<&str>)]) -> World {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), targets);
        let init = InitHeader {
            store: StoreBinding {
                id: store.store_id.as_str().into(),
                root: dir.path().to_path_buf(),
            },
            base: dir.path().join("run"),
            vault_id: store.vault_id,
            minutes: 5,
            window_secs: None,
            poll_leader: false,
            client_pid: 1,
        };
        let vault = std::fs::read(dir.path().join("vault")).unwrap();
        World {
            _dir: dir,
            store,
            init,
            vault,
        }
    }

    fn held<'a>(init: &'a InitHeader, key: &'a [u8; 32]) -> Held<'a> {
        Held {
            key,
            init,
            hardening: HolderHardening {
                non_dumpable: None,
                deny_attach: None,
                core_off: true,
                memory_locked: true,
                process_dacl: None,
            },
        }
    }

    fn item(target: &str, vault: &[u8]) -> Request {
        Request::OpenItem {
            vault: vault.to_vec(),
            item: HolderItem::HetznerToken {
                target: target.into(),
            },
        }
    }

    fn left(w: &mut Window, c: &Fake) -> Duration {
        match w.check(c) {
            WindowState::Open { left } => left,
            other => panic!("{other:?}"),
        }
    }

    /// T48, T56: a served item is the vault's, and restarts the window; `Status` and a refusal
    /// restart nothing.
    #[test]
    fn a_served_item_restarts_the_window_and_a_status_does_not() {
        let w = world(&[("prod", Some(TOKEN))]);
        let key = *w.store.data_key().expose();
        let c = Fake::new();
        let mut win = Window::open(Duration::from_secs(60), &c);
        c.pass(Duration::from_secs(50));
        let s = handle(Request::Status, &held(&w.init, &key), &mut win, &c);
        assert!(
            matches!(s.reply, Reply::Status(ref st) if st.left_secs == 10 && st.minutes == 5)
                && !s.end
        );
        assert_eq!(
            left(&mut win, &c),
            Duration::from_secs(10),
            "Status restarts nothing"
        );
        let s = handle(item("prod", &w.vault), &held(&w.init, &key), &mut win, &c);
        assert_eq!(
            s.reply,
            Reply::Item(Zeroizing::new(TOKEN.as_bytes().to_vec()))
        );
        assert_eq!(
            left(&mut win, &c),
            Duration::from_secs(60),
            "a served item restarts it"
        );
        c.pass(Duration::from_secs(30));
        let mut tampered = w.vault.clone();
        let at = tampered.len() - 1;
        tampered[at] ^= 1;
        let s = handle(item("prod", &tampered), &held(&w.init, &key), &mut win, &c);
        assert_eq!(s.reply, Reply::Refused(Refusal::VaultDamaged));
        assert_eq!(
            left(&mut win, &c),
            Duration::from_secs(30),
            "a refusal restarts nothing"
        );
    }

    /// Rule 3: `Absent` only once the vault authenticated (it is served, so it restarts too).
    #[test]
    fn a_target_without_a_token_is_absent_after_authentication() {
        let w = world(&[("prod", Some(TOKEN)), ("staging", None)]);
        let key = *w.store.data_key().expose();
        let c = Fake::new();
        let mut win = Window::open(Duration::from_secs(60), &c);
        c.pass(Duration::from_secs(20));
        let s = handle(
            item("staging", &w.vault),
            &held(&w.init, &key),
            &mut win,
            &c,
        );
        assert_eq!(s.reply, Reply::Absent);
        assert_eq!(left(&mut win, &c), Duration::from_secs(60));
    }

    /// T53: another store's vault is `WrongStore`; this store's vault under a key for another
    /// vault (a rekeyed vault) is `KeyMismatch`; a damaged one `VaultDamaged`; an oversized one
    /// `Malformed`. None is ever an absent token.
    #[test]
    fn every_vault_the_holder_cannot_serve_is_refused_by_its_name() {
        let w = world(&[("prod", Some(TOKEN))]);
        let other = world(&[("prod", Some(TOKEN))]);
        let key = *w.store.data_key().expose();
        let c = Fake::new();
        let mut win = Window::open(Duration::from_secs(60), &c);
        let refused = |req, held: &Held<'_>, win: &mut Window| handle(req, held, win, &c).reply;
        assert_eq!(
            refused(item("prod", &other.vault), &held(&w.init, &key), &mut win),
            Reply::Refused(Refusal::WrongStore)
        );
        let mut rekeyed = w.init.clone();
        rekeyed.vault_id = VaultId([9; 16]);
        assert_eq!(
            refused(item("prod", &w.vault), &held(&rekeyed, &key), &mut win),
            Reply::Refused(Refusal::KeyMismatch)
        );
        assert_eq!(
            refused(item("prod", b"not a vault"), &held(&w.init, &key), &mut win),
            Reply::Refused(Refusal::VaultDamaged)
        );
        let huge = vec![0u8; apprafter_core::vault::VAULT_FILE_MAX + 1];
        assert_eq!(
            refused(item("prod", &huge), &held(&w.init, &key), &mut win),
            Reply::Refused(Refusal::Malformed)
        );
    }

    /// T48: a cache opens inside the holder with the store's identity; a cache for another key
    /// is refused with that reason (a verdict on the cache, never on the vault); the recipient
    /// is the store's.
    #[test]
    fn a_cache_opens_inside_the_holder_and_the_recipient_is_the_stores() {
        let w = world(&[("prod", Some(TOKEN))]);
        let key = *w.store.data_key().expose();
        let c = Fake::new();
        let mut win = Window::open(Duration::from_secs(60), &c);
        let sealed = encrypt_for_recipient("apiVersion: v1", w.store.recipient()).unwrap();
        let open = |armored: &str, win: &mut Window| {
            handle(
                Request::OpenCache {
                    vault: w.vault.clone(),
                    secret: CachedSecret::Kubeconfig,
                    target: "prod".into(),
                    armored: armored.as_bytes().to_vec(),
                },
                &held(&w.init, &key),
                win,
                &c,
            )
            .reply
        };
        assert_eq!(
            open(&sealed, &mut win),
            Reply::Item(Zeroizing::new(b"apiVersion: v1".to_vec()))
        );
        let foreign = encrypt_for_recipient("x", &Identity::generate().to_public()).unwrap();
        assert!(matches!(
            open(&foreign, &mut win),
            Reply::Refused(Refusal::CacheRefused {
                reason: cli_core::secrets::UndecryptableWhy::NotForThisKey,
                ..
            })
        ));
        let r = handle(
            Request::Recipient {
                vault: w.vault.clone(),
            },
            &held(&w.init, &key),
            &mut win,
            &c,
        );
        assert_eq!(r.reply, Reply::Recipient(w.store.recipient().to_string()));
        let big = "a".repeat(MAX_ARMORED_CACHE + 1);
        assert_eq!(open(&big, &mut win), Reply::Refused(Refusal::Malformed));
    }

    /// T56, O4: an expired or slept window refuses and ends the holder; `End` ends it too.
    #[test]
    fn an_ended_window_refuses_and_end_ends() {
        let w = world(&[("prod", Some(TOKEN))]);
        let key = *w.store.data_key().expose();
        let c = Fake::new();
        let mut win = Window::open(Duration::from_secs(10), &c);
        c.pass(Duration::from_secs(10));
        let s = handle(Request::Status, &held(&w.init, &key), &mut win, &c);
        assert_eq!(
            (s.reply, s.end),
            (Reply::Refused(Refusal::WindowEnded), true)
        );
        let mut win = Window::open(Duration::from_secs(600), &c);
        c.0.set(c.0.get() + Duration::from_secs(6));
        let s = handle(item("prod", &w.vault), &held(&w.init, &key), &mut win, &c);
        assert_eq!(
            (s.reply, s.end),
            (Reply::Refused(Refusal::WindowEnded), true),
            "slept"
        );
        let mut win = Window::open(Duration::from_secs(600), &c);
        let s = handle(Request::End, &held(&w.init, &key), &mut win, &c);
        assert_eq!((s.reply, s.end), (Reply::Ended, true));
    }

    /// T56, team decision 2026-10-13: every request re-reads `store.yaml` first. Once the store
    /// is no longer locked with this holder's id — `lock off` ran (the vault deleted, then
    /// `files`), another store's `store.yaml` was copied over, the file is gone — a request,
    /// `Status` and `End` included, is refused as an ended window and ends the holder.
    #[test]
    fn a_request_after_the_store_changed_is_refused_and_ends_the_holder() {
        for change in ["files", "another id", "removed"] {
            let w = world(&[("prod", Some(TOKEN))]);
            let key = *w.store.data_key().expose();
            let c = Fake::new();
            let mut win = Window::open(Duration::from_secs(600), &c);
            let s = handle(Request::Status, &held(&w.init, &key), &mut win, &c);
            assert!(
                matches!(s.reply, Reply::Status(_)) && !s.end,
                "{change}: before"
            );
            change_store(&w.init.store.root, change);
            for req in [Request::Status, item("prod", &w.vault), Request::End] {
                let s = handle(req, &held(&w.init, &key), &mut win, &c);
                assert_eq!(
                    (s.reply, s.end),
                    (Reply::Refused(Refusal::WindowEnded), true),
                    "{change}"
                );
            }
        }
    }
}
