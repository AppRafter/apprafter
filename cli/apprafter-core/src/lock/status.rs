// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The one lock report (D.4a T73): `lock status`, doctor's lock rows, `status`'s line and the
//! desktop's Security page all read it, so they cannot disagree. It never prompts and never
//! opens the vault: the vault's header is parsed for its slots and for the refusal this build
//! would give each before deriving (T22); the client supplies its window and what only it sees.

use cli_core::error::VaultProblem;
use cli_core::CliError;

use crate::context::Context;
use crate::error::CoreResult;
use crate::lock::facts::{MachineFact, SlotFacts};
use crate::lock::limits::{detect_limits, LimitClient};
use crate::lock::{LockReport, SlotNote, SlotRefusalView, SlotView, UnlockWindow};
use crate::secrets::{
    read_vault, vault_error, vault_on_disk, vault_path, LockSlotKind, SecretsMode, StoreFile,
};
use crate::vault::{
    SlotBody, SlotId, SlotKind, VaultError, VaultFile, VaultHeader, VaultResult,
    DEFAULT_UNLOCK_TIMEOUT_MIN,
};

/// The lock report (module docs): `facts` are what only the client can find out about this
/// computer — its OS and machine id — by which each slot's refusal and notes are told here
/// (D.4b B30); a fact it could not ask is said so, never a pass.
pub fn status(
    ctx: &Context,
    window: UnlockWindow,
    client: &LimitClient,
    facts: &dyn SlotFacts,
) -> CoreResult<LockReport> {
    let secrets = ctx.secrets();
    let mode = secrets.mode()?;
    let file = secrets.store_file()?;
    let targets = cli_core::list_target_names(&ctx.store())?;
    let slots = match &file {
        Some(f) => slot_views(ctx, f, facts)?,
        None => Vec::new(),
    };
    let window_minutes = file
        .as_ref()
        .and_then(|f| f.unlock_timeout_min)
        .unwrap_or(DEFAULT_UNLOCK_TIMEOUT_MIN);
    let limits = detect_limits(ctx, client, &window);
    Ok(LockReport {
        store: cli_core::paths::abbreviate_home(ctx.config_root(), ctx.home_dir()),
        store_id: file
            .as_ref()
            .and_then(|f| f.id.as_ref())
            .map(|id| id.as_str().to_string()),
        targets,
        mode,
        slots,
        window_minutes,
        window,
        limits,
    })
}

/// The vault header's slots (authoritative; `store.yaml` mirrors them), each with this build's
/// refusal. The vault is read as every command reads it — `read_vault`, which refuses one that
/// names another store (T32; d4a-f's critic B, M3, and e's verifier, finding 5) — so it is
/// never reported as this store's slots. An interrupted lock flow may have no vault: `lock on`
/// none yet, `lock off` none any more (d's step 5 removed it before step 6 rewrote
/// `store.yaml`) — then `store.yaml`'s mirror, so `lock status` names the state rather than
/// failing (§3.11; critic-d4a-e finding 15). Only a `locked` store's missing vault is an error.
/// D.4b: each with its id, its notes and its refusal on this computer ([`header_views`]); the
/// mirror's rows have no id, and say what their kind costs.
fn slot_views(ctx: &Context, file: &StoreFile, facts: &dyn SlotFacts) -> CoreResult<Vec<SlotView>> {
    let root = ctx.config_root();
    let mirror = || {
        file.slots
            .iter()
            .map(|s| SlotView {
                id: None,
                kind: s.kind,
                label: s.label.clone(),
                refused: None,
                notes: mirror_notes(s.kind),
            })
            .collect::<Vec<_>>()
    };
    // A `store.yaml` that names no id is a store in files, which has no vault of its own: a
    // vault beside it (a race with `lock on`) is refused, never reported unchecked.
    let Some(id) = &file.id else {
        return match vault_on_disk(root)? {
            false => Ok(mirror()),
            true => Err(CliError::VaultUnreadable {
                path: vault_path(root),
                problem: VaultProblem::OtherStore,
                detail: "store.yaml names no store id".into(),
            }
            .into()),
        };
    };
    let bytes = match read_vault(&ctx.store(), id) {
        Ok(b) => b,
        Err(CliError::VaultUnreadable {
            problem: VaultProblem::Missing,
            ..
        }) if file.secrets != SecretsMode::Locked => return Ok(mirror()),
        Err(e) => return Err(e.into()),
    };
    let parsed = VaultFile::parse(&bytes).map_err(|e| vault_error(root, e))?;
    // F4: the context's policy — this build's, unless a test overrides it.
    Ok(header_views(
        parsed.header(),
        parsed.slot_policy(super::kdf_policy(ctx)),
        facts,
    ))
}

/// The vault header's slots as every surface lists them (D.4b B18, B30, B36, B61, B70, B77):
/// each with its id, its notes and this build's refusal on this computer, told without
/// deriving anything. A body this build cannot read is a newer AppRafter's; an
/// account-password slot is judged by the client's facts — the OS it plays and this
/// computer's machine id — never the OS itself, and a fact the client could not ask is said
/// so; a second slot of a typed kind is never tried (the prompt takes the first).
pub(super) fn header_views(
    header: &VaultHeader,
    verdicts: Vec<(SlotId, VaultResult<()>)>,
    facts: &dyn SlotFacts,
) -> Vec<SlotView> {
    let machine = facts.machine_id();
    let this_os = facts.os().has_account_password_slot();
    let may_change = facts.machine_id_may_change() == Some(true);
    let first_password = header
        .slots
        .iter()
        .position(|slot| slot.kind == SlotKind::Password);
    let first_here = machine
        .id()
        .and_then(|id| header.slots.iter().position(|slot| slot.made_on(id)));
    header
        .slots
        .iter()
        .zip(verdicts)
        .enumerate()
        .map(|(i, (slot, (_, verdict)))| {
            let mut notes = Vec::new();
            let mut refused = None;
            if matches!(slot.body, SlotBody::Opaque(_)) {
                refused = Some(SlotRefusalView::NewerKind);
            } else if slot.kind == SlotKind::OsPassword {
                notes.push(SlotNote::AccountPasswordOffline);
                notes.push(SlotNote::ManagesStore);
                match &machine {
                    _ if !this_os => refused = Some(SlotRefusalView::GestureNotHere),
                    MachineFact::Id(id) if slot.made_on(id) => {
                        if first_here != Some(i) {
                            refused = Some(SlotRefusalView::SecondOfKind);
                        }
                        if may_change {
                            notes.push(SlotNote::MachineIdMayChange);
                        }
                    }
                    MachineFact::Id(_) | MachineFact::NoId => {
                        refused = Some(SlotRefusalView::GestureNotHere);
                        notes.push(SlotNote::OsPasswordOtherComputer);
                    }
                    MachineFact::Unknown => notes.push(SlotNote::NotCheckedHere),
                }
            } else if slot.kind == SlotKind::Password && first_password != Some(i) {
                refused = Some(SlotRefusalView::SecondOfKind);
            }
            if refused.is_none() {
                refused = match verdict {
                    Err(VaultError::KdfTooWeak { .. }) => Some(SlotRefusalView::KdfTooWeak),
                    Err(VaultError::KdfOutOfRange { .. }) => Some(SlotRefusalView::KdfOutOfRange),
                    _ => None,
                };
            }
            SlotView {
                id: Some(slot.id.to_string()),
                kind: LockSlotKind::from(slot.kind),
                label: slot.label.clone(),
                refused,
                notes,
            }
        })
        .collect()
}

/// What `store.yaml`'s mirror can say of a slot without its vault: an account-password
/// slot's standing costs, and that whether it opens here was not checked.
fn mirror_notes(kind: LockSlotKind) -> Vec<SlotNote> {
    match kind {
        LockSlotKind::OsPassword => vec![
            SlotNote::AccountPasswordOffline,
            SlotNote::ManagesStore,
            SlotNote::NotCheckedHere,
        ],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::context::SecretString;
    use crate::lock::facts::{FixedFacts, HostOs};
    use crate::lock::limits::{DisplayKind, LimitClient, SshFacts, TerminalFacts};
    use crate::lock::UnlockWindow;
    use crate::lock::{SlotNote, UnknownFacts};
    use crate::secrets::testing::locked_store;
    use crate::secrets::testing::LockedTestStore;
    use crate::secrets::{LockSlotKind, SecretsMode};
    use crate::vault::{KdfPolicy as Policy, MachineId, NewSlot, OsRandom, SlotInput, VAULT_FILE};
    use cli_core::target::TargetStorePaths;
    use cli_core::StoreLock;

    fn client() -> LimitClient {
        LimitClient::Cli {
            terminal: Some(TerminalFacts {
                multiplexer: None,
                x_client: None,
                display: DisplayKind::None,
            }),
            ssh: SshFacts::default(),
        }
    }

    /// A context over the scratch store `root`, built from nothing of the environment: G3 counts
    /// `from_cli_env` in a test as a real-environment builder, and these tests open the store
    /// (critic-d4a-e finding 1); `status` needs no holder base.
    fn ctx(root: &Path) -> Context {
        Context::for_desktop(root.to_path_buf(), "http://127.0.0.1:1")
    }

    /// [`ctx`] under the fast KDF policy: the slots a test adds read as this build's.
    fn fast(root: &Path) -> Context {
        ctx(root).with_kdf_policy(&Policy::INSECURE_FAST)
    }

    fn account(password: &str, which: u8) -> NewSlot {
        NewSlot::OsPassword {
            secret: SecretString::new(password),
            label: format!("account password of walker on box-{which:x}"),
            enrolled_at: 1_760_000_000,
            machine: MachineId::new(&[which; 16]).unwrap(),
        }
    }

    fn lock_password(text: &str) -> NewSlot {
        NewSlot::Password {
            secret: SecretString::new(text),
            label: "lock password".into(),
        }
    }

    /// `made`'s vault (a recovery-key store at `root`) with `slots` added, fast costs, and
    /// `forged` pushed as they are — a header no write of the core makes (B18, B77).
    fn with_slots(root: &Path, made: &LockedTestStore, slots: Vec<NewSlot>, forged: Vec<NewSlot>) {
        let path = root.join(VAULT_FILE);
        let bytes = crate::vault::file::read(&path).unwrap();
        let mut opened = VaultFile::parse(&bytes)
            .unwrap()
            .open_with_key(made.data_key(), made.vault_id)
            .unwrap();
        for slot in slots {
            opened
                .add_slot(slot, &Policy::INSECURE_FAST, &mut OsRandom)
                .unwrap();
        }
        for slot in forged {
            let record = opened.forge_record(slot, &Policy::INSECURE_FAST).unwrap();
            opened.header_mut().slots.push(record);
        }
        let sealed = opened.seal("apprafter 0.0.0-test", &mut OsRandom).unwrap();
        let held = StoreLock::exclusive(&TargetStorePaths::for_root(root.to_path_buf())).unwrap();
        crate::vault::file::replace(&path, &sealed, &held).unwrap();
    }

    /// T73: a store in files reports files, its targets, no slots and no id.
    #[test]
    fn a_store_in_files_reports_files() {
        let dir = tempfile::tempdir().unwrap();
        let r = status(
            &ctx(dir.path()),
            UnlockWindow::Closed,
            &client(),
            &UnknownFacts,
        )
        .unwrap();
        assert_eq!(
            (r.mode, r.store_id, r.slots.len(), r.window_minutes),
            (SecretsMode::Files, None, 0, 5)
        );
        assert_eq!(r.window, UnlockWindow::Closed);
    }

    /// T73: a locked store reports its id, its targets and the vault header's slots, the window
    /// the client gave and the store's default; the vault is parsed, never opened.
    #[test]
    fn a_locked_store_reports_its_slots_from_the_vault_header() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(
            dir.path(),
            &[(
                "prod",
                Some("abababababababababababababababababababababababababababababababab"),
            )],
        );
        let r = status(
            &ctx(dir.path()),
            UnlockWindow::Closed,
            &client(),
            &UnknownFacts,
        )
        .unwrap();
        assert_eq!(r.mode, SecretsMode::Locked);
        assert_eq!(r.store_id.as_deref(), Some(store.store_id.as_str()));
        assert_eq!(r.targets, ["prod"]);
        assert_eq!(r.slots.len(), 1);
        assert_eq!(
            (r.slots[0].kind, r.slots[0].refused),
            (LockSlotKind::RecoveryKey, None)
        );
    }

    /// T22, L3: a password slot this build refuses says so before any derivation — the frozen
    /// fast-parameter vector under the release policy (a plain-feature test run).
    #[cfg(not(feature = "insecure-fast-kdf"))]
    #[test]
    fn a_slot_this_build_refuses_is_named_without_deriving() {
        let dir = tempfile::tempdir().unwrap();
        let v1 = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vault_vectors/v1/fast-params");
        for f in ["store.yaml", "vault"] {
            std::fs::copy(v1.join(f), dir.path().join(f)).unwrap();
        }
        let r = status(
            &ctx(dir.path()),
            UnlockWindow::Closed,
            &client(),
            &UnknownFacts,
        )
        .unwrap();
        assert_eq!(r.store_id.as_deref(), Some("0123456789abcdef"));
        let password = r
            .slots
            .iter()
            .find(|s| s.kind == LockSlotKind::Password)
            .unwrap();
        assert_eq!(password.refused, Some(SlotRefusalView::KdfTooWeak));
        assert_eq!(r.window_minutes, 15, "store.yaml's mirror");
    }

    /// §3.11, T73 (critic-d4a-e finding 15): an interrupted `lock off` that already removed the
    /// vault (`secrets: unlocking`, no vault) is reported from `store.yaml`'s mirror, as an
    /// interrupted `lock on` with no vault yet is — `lock status` names the state, it never fails
    /// on it; a `locked` store without its vault still does.
    #[test]
    fn an_interrupted_lock_without_its_vault_reports_the_mirror() {
        let dir = tempfile::tempdir().unwrap();
        locked_store(dir.path(), &[("prod", None)]);
        let yaml = dir.path().join("store.yaml");
        let text = std::fs::read_to_string(&yaml).unwrap();
        assert!(text.contains("secrets: locked\n"), "{text}");
        std::fs::remove_file(dir.path().join("vault")).unwrap();
        assert!(status(
            &ctx(dir.path()),
            UnlockWindow::Closed,
            &client(),
            &UnknownFacts
        )
        .is_err());
        std::fs::write(
            &yaml,
            text.replace("secrets: locked\n", "secrets: unlocking\n"),
        )
        .unwrap();
        let r = status(
            &ctx(dir.path()),
            UnlockWindow::Closed,
            &client(),
            &UnknownFacts,
        )
        .unwrap();
        assert_eq!(r.mode, SecretsMode::Unlocking);
        assert_eq!(
            r.slots[0].id, None,
            "a mirror row has no id, so no action (B36)"
        );
        assert_eq!(r.slots.len(), 1);
        assert_eq!(
            (r.slots[0].kind, r.slots[0].refused),
            (LockSlotKind::RecoveryKey, None)
        );
    }

    /// D.4b B30, B36, B61, B70 (WI-498 acceptance 3): every slot with its id, its notes and
    /// its refusal on this computer — from the client's facts, never the OS; a fact the client
    /// could not ask is said so.
    #[test]
    fn every_slot_is_listed_with_its_id_notes_and_refusal_here() {
        let dir = tempfile::tempdir().unwrap();
        let made = locked_store(dir.path(), &[("prod", None)]);
        with_slots(
            dir.path(),
            &made,
            vec![account("on a", 0xa), account("on b", 0xb)],
            Vec::new(),
        );
        let bytes = std::fs::read(dir.path().join(VAULT_FILE)).unwrap();
        let ids: Vec<String> = VaultFile::parse(&bytes)
            .unwrap()
            .header()
            .slots
            .iter()
            .map(|slot| slot.id.to_string())
            .collect();
        let at = |facts: &dyn SlotFacts| {
            status(&fast(dir.path()), UnlockWindow::Closed, &client(), facts)
                .unwrap()
                .slots
        };
        let here = at(&FixedFacts::on(0xa));
        assert_eq!(
            here.iter()
                .map(|s| s.id.clone().unwrap())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(
            (here[0].kind, here[0].refused),
            (LockSlotKind::RecoveryKey, None)
        );
        assert!(here[0].notes.is_empty());
        assert_eq!(
            here[1].notes,
            [SlotNote::AccountPasswordOffline, SlotNote::ManagesStore]
        );
        assert_eq!(here[1].refused, None, "this computer's slot opens here");
        assert_eq!(
            here[2].notes,
            [
                SlotNote::AccountPasswordOffline,
                SlotNote::ManagesStore,
                SlotNote::OsPasswordOtherComputer
            ]
        );
        assert_eq!(here[2].refused, Some(SlotRefusalView::GestureNotHere));
        let unknown = at(&FixedFacts {
            machine: MachineFact::Unknown,
            ..FixedFacts::on(0xa)
        });
        for s in &unknown[1..] {
            assert_eq!(
                (s.refused, s.notes.last()),
                (None, Some(&SlotNote::NotCheckedHere)),
                "{s:?}"
            );
        }
        let windows = at(&FixedFacts {
            os: HostOs::Windows,
            ..FixedFacts::on(0xa)
        });
        assert!(windows[1..]
            .iter()
            .all(|s| s.refused == Some(SlotRefusalView::GestureNotHere)));
        let container = at(&FixedFacts {
            may_change: Some(true),
            ..FixedFacts::on(0xa)
        });
        assert_eq!(
            container[1].notes.last(),
            Some(&SlotNote::MachineIdMayChange)
        );
        assert!(!container[2].notes.contains(&SlotNote::MachineIdMayChange));
        let no_id = at(&FixedFacts {
            machine: MachineFact::NoId,
            ..FixedFacts::on(0xa)
        });
        assert!(no_id[1..]
            .iter()
            .all(|s| s.refused == Some(SlotRefusalView::GestureNotHere)
                && s.notes.contains(&SlotNote::OsPasswordOtherComputer)));
    }

    /// B18, B77 (WI-498 acceptance 3): a second lock-password slot, and a second slot with
    /// this computer's tag — a header no write of the core makes — are `second_of_kind`: the
    /// prompt on this computer never tries them, and another computer's slot is no second.
    #[test]
    fn a_second_slot_of_a_typed_kind_is_said_never_to_be_tried() {
        let dir = tempfile::tempdir().unwrap();
        let made = locked_store(dir.path(), &[("prod", None)]);
        with_slots(
            dir.path(),
            &made,
            vec![
                lock_password("first"),
                account("on a", 0xa),
                account("on b", 0xb),
            ],
            vec![lock_password("second"), account("again on a", 0xa)],
        );
        let s = status(
            &fast(dir.path()),
            UnlockWindow::Closed,
            &client(),
            &FixedFacts::on(0xa),
        )
        .unwrap()
        .slots;
        assert_eq!(
            s.iter().map(|v| v.refused).collect::<Vec<_>>(),
            [
                None,
                None,
                None,
                Some(SlotRefusalView::GestureNotHere),
                Some(SlotRefusalView::SecondOfKind),
                Some(SlotRefusalView::SecondOfKind),
            ]
        );
        for never in ["second", "again on a"] {
            let input = SlotInput::from_input(
                SecretString::new(never),
                Some(MachineId::new(&[0xa; 16]).unwrap()),
            );
            assert!(
                matches!(
                    crate::lock::open_at(dir.path(), &made.store_id, input, &Policy::INSECURE_FAST),
                    Err(crate::error::CoreError::Cli(
                        cli_core::CliError::UnlockFailed { .. }
                    ))
                ),
                "{never} is never tried"
            );
        }
    }

    /// Rule 3: a vault that cannot be read is an error, never a report without slots.
    #[test]
    fn a_damaged_vault_is_an_error_not_an_empty_report() {
        let dir = tempfile::tempdir().unwrap();
        locked_store(dir.path(), &[("prod", None)]);
        std::fs::write(dir.path().join("vault"), b"not a vault").unwrap();
        assert!(status(
            &ctx(dir.path()),
            UnlockWindow::Closed,
            &client(),
            &UnknownFacts
        )
        .is_err());
    }

    /// §3.13, T32 (d4a-f's critic B, M3): a vault that names another store is
    /// `vault_unreadable{other_store}`, as every command refuses it — never a report whose
    /// slots doctor and `status` would show as "locked".
    #[test]
    fn a_vault_of_another_store_is_an_error_not_a_report() {
        let dir = tempfile::tempdir().unwrap();
        let (mine, other) = (dir.path().join("mine"), dir.path().join("other"));
        locked_store(&mine, &[("prod", None)]);
        locked_store(&other, &[("prod", None)]);
        std::fs::copy(other.join("vault"), mine.join("vault")).unwrap();
        match status(&ctx(&mine), UnlockWindow::Closed, &client(), &UnknownFacts) {
            Err(crate::error::CoreError::Cli(cli_core::CliError::VaultUnreadable {
                problem: cli_core::error::VaultProblem::OtherStore,
                ..
            })) => {}
            other => panic!("{other:?}"),
        }
    }
}
