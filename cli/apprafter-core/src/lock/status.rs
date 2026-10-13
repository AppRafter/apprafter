// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The one lock report (D.4a T73): `lock status`, doctor's lock rows, `status`'s line and
//! Settings › Lock all read it, so they cannot disagree. It never prompts and never opens the
//! vault: the vault's header is parsed for its slots and for the refusal this build would give
//! each before deriving (T22); the client supplies its window and what only it sees.

use cli_core::error::VaultProblem;
use cli_core::CliError;

use crate::context::Context;
use crate::error::CoreResult;
use crate::lock::limits::{detect_limits, LimitClient};
use crate::lock::{LockReport, SlotRefusalView, SlotView, UnlockWindow};
use crate::secrets::{
    read_vault, vault_error, vault_on_disk, vault_path, LockSlotKind, SecretsMode, StoreFile,
};
use crate::vault::{KdfPolicy, VaultError, VaultFile, DEFAULT_UNLOCK_TIMEOUT_MIN};

pub fn status(ctx: &Context, window: UnlockWindow, client: &LimitClient) -> CoreResult<LockReport> {
    let secrets = ctx.secrets();
    let mode = secrets.mode()?;
    let file = secrets.store_file()?;
    let targets = cli_core::list_target_names(&ctx.store())?;
    let slots = match &file {
        Some(f) => slot_views(ctx, f)?,
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
fn slot_views(ctx: &Context, file: &StoreFile) -> CoreResult<Vec<SlotView>> {
    let root = ctx.config_root();
    let mirror = || {
        file.slots
            .iter()
            .map(|s| SlotView {
                kind: s.kind,
                label: s.label.clone(),
                refused: None,
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
    let verdicts = parsed.slot_policy(KdfPolicy::active());
    Ok(parsed
        .header()
        .slots
        .iter()
        .zip(verdicts)
        .map(|(slot, (_, verdict))| SlotView {
            kind: LockSlotKind::from(slot.kind),
            label: slot.label.clone(),
            refused: match verdict {
                Err(VaultError::KdfTooWeak { .. }) => Some(SlotRefusalView::KdfTooWeak),
                Err(VaultError::KdfOutOfRange { .. }) => Some(SlotRefusalView::KdfOutOfRange),
                _ => None,
            },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::lock::limits::{DisplayKind, LimitClient, SshFacts, TerminalFacts};
    use crate::lock::UnlockWindow;
    use crate::secrets::testing::locked_store;
    use crate::secrets::{LockSlotKind, SecretsMode};

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

    /// T73: a store in files reports files, its targets, no slots and no id.
    #[test]
    fn a_store_in_files_reports_files() {
        let dir = tempfile::tempdir().unwrap();
        let r = status(&ctx(dir.path()), UnlockWindow::Closed, &client()).unwrap();
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
        let r = status(&ctx(dir.path()), UnlockWindow::Closed, &client()).unwrap();
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
        let r = status(&ctx(dir.path()), UnlockWindow::Closed, &client()).unwrap();
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
        assert!(status(&ctx(dir.path()), UnlockWindow::Closed, &client()).is_err());
        std::fs::write(
            &yaml,
            text.replace("secrets: locked\n", "secrets: unlocking\n"),
        )
        .unwrap();
        let r = status(&ctx(dir.path()), UnlockWindow::Closed, &client()).unwrap();
        assert_eq!(r.mode, SecretsMode::Unlocking);
        assert_eq!(r.slots.len(), 1);
        assert_eq!(
            (r.slots[0].kind, r.slots[0].refused),
            (LockSlotKind::RecoveryKey, None)
        );
    }

    /// Rule 3: a vault that cannot be read is an error, never a report without slots.
    #[test]
    fn a_damaged_vault_is_an_error_not_an_empty_report() {
        let dir = tempfile::tempdir().unwrap();
        locked_store(dir.path(), &[("prod", None)]);
        std::fs::write(dir.path().join("vault"), b"not a vault").unwrap();
        assert!(status(&ctx(dir.path()), UnlockWindow::Closed, &client()).is_err());
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
        match status(&ctx(&mine), UnlockWindow::Closed, &client()) {
            Err(crate::error::CoreError::Cli(cli_core::CliError::VaultUnreadable {
                problem: cli_core::error::VaultProblem::OtherStore,
                ..
            })) => {}
            other => panic!("{other:?}"),
        }
    }
}
