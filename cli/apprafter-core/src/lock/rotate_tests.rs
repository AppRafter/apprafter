// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A key change (d4b-c2a; ADR 0069 §7; D.4b overview B63, B65, B68, B73, B79, B80, B84,
//! B85, B87) on scratch locked stores, under the fast KDF policy and its permit
//! (`Context::with_kdf_policy`, `with_insecure_kdf_root`): no test derives a release key
//! (T23). The secret store's kit makes each store (`locked_store`: one recovery-key slot); a
//! shape's other ways in are added to its vault as this build adds them. A key change never
//! asks the client's access for a key (B19): the kit's access counts every one it could hand
//! out.

use std::path::Path;

use cli_core::{CliError, LockPhase, StoreLock};

use super::facts::UnknownFacts;
use super::*;
use crate::secrets::testing::{locked_store, LockedTestStore};
use crate::secrets::{read_store_file, write_store_file, Rotation, SecretsMode, STORE_FILE};
use crate::vault::{KdfPolicy, SlotId, VaultId};
use crate::{Context, CoreError, UiError};

const TOKEN: &str = "abababababababababababababababababababababababababababababababab";

/// A store locked by the secret store's kit (`prod`, its token in the vault), its context
/// under the fast policy and its permit.
fn store_of() -> (tempfile::TempDir, Context, LockedTestStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let made = locked_store(&root, &[("prod", Some(TOKEN))]);
    let ctx = Context::for_desktop(root, "http://127.0.0.1:1")
        .with_kdf_policy(&KdfPolicy::INSECURE_FAST)
        .with_insecure_kdf_root(dir.path().to_path_buf());
    (dir, ctx, made)
}

/// `store.yaml` as R1 leaves it: `rotating` to `to`, dropping the Windows Hello slots
/// `hello_drop`.
fn say_rotating(root: &Path, to: VaultId, hello_drop: Vec<SlotId>) {
    let mut file = read_store_file(root).unwrap().unwrap();
    file.secrets = SecretsMode::Rotating;
    file.rotation = Some(Rotation { to, hello_drop });
    let store = cli_core::target::TargetStorePaths::for_root(root.to_path_buf());
    let held = StoreLock::exclusive(&store).unwrap();
    write_store_file(root, &file, &held).unwrap();
}

/// B63: `rotating` round-trips with the vault id it writes and the Windows Hello slots it
/// drops — `rotating_to` and `hello_drop` in the file — and every other state carries
/// neither.
#[test]
fn a_rotating_store_file_says_its_new_vault_and_the_hello_keys_it_drops() {
    let (_dir, ctx, made) = store_of();
    let root = ctx.config_root();
    let to = VaultId([0xa5; 16]);
    let hello_drop = vec![SlotId([0xab; 16]), SlotId([0xcd; 16])];
    say_rotating(root, to, hello_drop.clone());
    let file = read_store_file(root).unwrap().unwrap();
    assert_eq!(file.secrets, SecretsMode::Rotating);
    assert_eq!(file.id.as_ref(), Some(&made.store_id));
    assert_eq!(
        file.rotation,
        Some(Rotation {
            to,
            hello_drop: hello_drop.clone()
        })
    );
    let text = std::fs::read_to_string(root.join(STORE_FILE)).unwrap();
    assert!(text.contains("secrets: rotating\n"), "{text}");
    assert!(text.contains(&format!("rotating_to: {to}")), "{text}");
    for slot in &hello_drop {
        assert!(text.contains(&slot.to_string()), "{text}");
    }
    let store = cli_core::target::TargetStorePaths::for_root(root.to_path_buf());
    let held = StoreLock::exclusive(&store).unwrap();
    let locked = crate::secrets::StoreFile {
        secrets: SecretsMode::Locked,
        rotation: None,
        ..file
    };
    write_store_file(root, &locked, &held).unwrap();
    let text = std::fs::read_to_string(root.join(STORE_FILE)).unwrap();
    assert!(
        !text.contains("rotating_to") && !text.contains("hello_drop"),
        "{text}"
    );
    assert_eq!(read_store_file(root).unwrap().unwrap().rotation, None);
}

/// B63, T31: `rotating` names the vault it writes; without one — or with an id that is
/// none — `store.yaml` is unreadable: the store is treated as locked, none of its secrets
/// used.
#[test]
fn a_rotating_store_file_without_its_new_vault_is_unreadable() {
    let (_dir, ctx, made) = store_of();
    let path = ctx.config_root().join(STORE_FILE);
    let id = made.store_id.as_str();
    let to = VaultId([0xa5; 16]);
    for (body, says) in [
        (
            format!("format: 1\nsecrets: rotating\nid: {id}\n"),
            "names no new vault id",
        ),
        (
            format!("format: 1\nsecrets: rotating\nid: {id}\nrotating_to: a5a5\n"),
            "names no new vault id",
        ),
        (
            format!(
                "format: 1\nsecrets: rotating\nid: {id}\nrotating_to: {to}\nhello_drop: [nope]\n"
            ),
            "`hello_drop` names \"nope\"",
        ),
    ] {
        std::fs::write(&path, &body).unwrap();
        match read_store_file(ctx.config_root()) {
            Err(CliError::StoreFileUnreadable { path: p, detail }) => {
                assert_eq!(p, path);
                assert!(detail.contains(says), "{body:?}: {detail}");
            }
            other => panic!("{body:?}: {other:?}"),
        }
    }
}

/// B63: `rotating` is written with its key change's fields, and no other state is: either
/// mismatch is a caller's bug, refused before anything is written.
#[test]
fn only_rotating_carries_a_key_change_s_fields() {
    let (_dir, ctx, _made) = store_of();
    let root = ctx.config_root();
    let before = std::fs::read(root.join(STORE_FILE)).unwrap();
    let file = read_store_file(root).unwrap().unwrap();
    let store = cli_core::target::TargetStorePaths::for_root(root.to_path_buf());
    let held = StoreLock::exclusive(&store).unwrap();
    let bare = crate::secrets::StoreFile {
        secrets: SecretsMode::Rotating,
        rotation: None,
        ..file.clone()
    };
    let stray = crate::secrets::StoreFile {
        rotation: Some(Rotation {
            to: VaultId([0xa5; 16]),
            hello_drop: Vec::new(),
        }),
        ..file
    };
    for wrong in [bare, stray] {
        assert!(
            matches!(
                write_store_file(root, &wrong, &held),
                Err(CliError::Other(_))
            ),
            "{wrong:?}"
        );
    }
    assert_eq!(std::fs::read(root.join(STORE_FILE)).unwrap(), before);
}

/// B63, rule 10, F2, F13: every read of a store whose key change did not finish refuses it
/// as such — never a key, never "locked" — and so do the lock flows until it is finished:
/// `lock on` would plan a new vault over it, `lock off` would leave caches sealed to a key
/// no vault holds (d4b-c2b brings `lock off` from it), a slot write would carry the retiring
/// store key on.
#[test]
fn a_rotating_store_refuses_every_read_and_every_lock_flow() {
    let (_dir, ctx, made) = store_of();
    say_rotating(ctx.config_root(), VaultId([0xa5; 16]), Vec::new());
    let ctx = ctx.with_vault_access(made.access());
    let rotating = |e: CoreError| {
        matches!(
            e,
            CoreError::Cli(CliError::LockIncomplete {
                phase: LockPhase::Rotating,
                ..
            })
        )
    };
    assert_eq!(ctx.secrets().mode().unwrap(), SecretsMode::Rotating);
    assert!(rotating(CoreError::Cli(
        ctx.secrets().stored_token("prod").unwrap_err()
    )));
    assert!(rotating(
        plan_on(&ctx, None, WayAsked::LockPassword).unwrap_err()
    ));
    assert!(rotating(plan_off(&ctx).unwrap_err()));
    assert!(rotating(
        plan_slot_add(&ctx, SlotAddKind::Password, &UnknownFacts).unwrap_err()
    ));
}

/// B63, F2 (critic round, finding 5): a `lock off` planned while the store was `locked`
/// and run after a key change stopped over it refuses under the store lock — D.4a's steps
/// never run over an unfinished key change (d4b-c2b brings `lock off` from it) — and
/// writes nothing.
#[test]
fn a_lock_off_planned_before_a_key_change_stopped_refuses_it() {
    let (_dir, ctx, made) = store_of();
    let plan = plan_off(&ctx).unwrap();
    let opened = open_at(
        ctx.config_root(),
        &made.store_id,
        crate::vault::SlotInput::from_input(made.recovery_key().clone(), None),
        &KdfPolicy::INSECURE_FAST,
    )
    .unwrap();
    say_rotating(ctx.config_root(), VaultId([0xa5; 16]), Vec::new());
    let before = std::fs::read(ctx.config_root().join(STORE_FILE)).unwrap();
    let err = execute_off(
        &ctx,
        plan,
        Some(opened),
        &crate::NullReporter,
        &crate::CancellationToken::new(),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            CoreError::Cli(CliError::LockIncomplete {
                phase: LockPhase::Rotating,
                ..
            })
        ),
        "{err:?}"
    );
    assert_eq!(
        std::fs::read(ctx.config_root().join(STORE_FILE)).unwrap(),
        before
    );
}

/// The words of an interrupted key change: both commands that finish it, and `lock off`;
/// the desktop's names the Security page.
#[test]
fn an_interrupted_key_change_says_how_it_ends() {
    let e = CliError::LockIncomplete {
        store: "/s".into(),
        phase: LockPhase::Rotating,
    };
    assert_eq!(
        e.to_string(),
        "an earlier `lock rekey` or `lock recovery` on the target store at /s did not finish"
    );
    assert_eq!(
        UiError::from(&CoreError::Cli(e)).help.as_deref(),
        Some(
            "Changing this target store's keys was interrupted. Finish it, or stop \
             protecting the secrets, on the Security page."
        )
    );
}
