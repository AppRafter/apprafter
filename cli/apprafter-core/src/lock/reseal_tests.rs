// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A key change's re-seal (D.4b B63, B78, B79, B85, B86) on scratch locked stores: the store's
//! vault rotated as R2 leaves it (`vault::rotate` over the vault on disk, written by the
//! durable replace), then R3 over every state folder. `lock on`'s re-seal is `on_tests`' and
//! `off_tests`' (unchanged by the move, B85). No test here derives a key: the test store's one
//! slot is a recovery key (`secrets::testing::locked_store`), kept through the rotation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli_core::secrets::{
    decrypt_with_identity_reason, encrypt_for_recipient, CachedSecret, Identity, Recipient,
};
use cli_core::target::TargetStorePaths;
use cli_core::{CliError, StoreLock};
use cli_state::{HetznerCloudState, SealedTo, State, StatePaths};
use serde_json::json;

use super::on::DroppedCache;
use super::reseal::{
    orphan_line, reseal, set_aside_line, state_folders, OldKeys, ResealMode, Resealed, SetAside,
    StateFolder,
};
use crate::error::{CoreError, CoreResult};
use crate::secrets::testing::{locked_store, LockedTestStore};
use crate::secrets::FixedVaultAccess;
use crate::secrets::{transfer, vault_path, CacheKey, KeySession};
use crate::target::testkit::TOKEN_A;
use crate::vault::{
    self, KdfPolicy, KeptMaterial, OsRandom, RecoveryKey, Rotated, RotationIds, VaultFile, VaultId,
};
use crate::Context;
use cli_core::secrets::{CacheKeyKind, UndecryptableWhy};

const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";
const PW: &str = "argocd-admin-password";
const WRITER: &str = "apprafter reseal-test";

/// A locked store as R2 leaves it: its vault rotated — a new data key, vault id and store key,
/// the old store key retiring in the payload, the recovery-key slot kept — and written over the
/// old one; `store.yaml` as it was (R1 and R5 are c2a's).
struct Mid {
    _dir: tempfile::TempDir,
    store: TargetStorePaths,
    made: LockedTestStore,
    rotated: Rotated,
}

fn mid(targets: &[(&str, Option<&str>)]) -> Mid {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let made = locked_store(&root, targets);
    let store = TargetStorePaths::for_root(root.clone());
    let bytes = vault::file::read(&vault_path(&root)).unwrap();
    let opened = VaultFile::parse(&bytes)
        .unwrap()
        .open_with_key(made.data_key(), made.vault_id)
        .unwrap();
    let recovery = RecoveryKey::parse(made.recovery_key().expose()).unwrap();
    let kept = opened
        .keep(
            opened.header().slots[0].id,
            KeptMaterial::RecoveryKey(recovery),
            KdfPolicy::active(),
        )
        .unwrap();
    let ids = RotationIds::draw(0, &mut OsRandom).unwrap();
    let rotated = vault::rotate(
        &opened,
        &ids,
        vec![kept],
        Vec::new(),
        WRITER,
        KdfPolicy::active(),
        &mut OsRandom,
    )
    .unwrap();
    let held = StoreLock::exclusive(&store).unwrap();
    vault::file::replace(&vault_path(&root), &rotated.bytes, &held).unwrap();
    Mid {
        _dir: dir,
        store,
        made,
        rotated,
    }
}

impl Mid {
    fn root(&self) -> &Path {
        self.store.root()
    }

    /// The new store key, as R3 is given it: a session over the rotated vault.
    fn key(&self) -> CacheKey {
        let vault = &self.rotated.vault;
        transfer::store_key(
            self.root(),
            KeySession::new(
                vault.data_key().clone(),
                vault.header().vault_id,
                self.root().to_path_buf(),
            ),
            Arc::from(self.rotated.bytes.clone()),
        )
    }

    /// The store key the change retires.
    fn retiring(&self) -> Identity {
        self.rotated
            .vault
            .payload()
            .retiring_identity()
            .unwrap()
            .unwrap()
    }

    /// The new store key.
    fn now(&self) -> Identity {
        self.rotated.vault.payload().age_identity().unwrap()
    }

    fn new_vault(&self) -> VaultId {
        self.rotated.vault.header().vault_id
    }

    /// What a cache sealed to the store key of this store's vault `vault` records (B86).
    fn record(&self, vault: VaultId) -> SealedTo {
        SealedTo::StoreVault {
            store_id: self.made.store_id.as_str().to_string(),
            vault_id: Some(vault.to_string()),
        }
    }

    /// R3, as c2a runs it: every state folder re-sealed to the new key under the store lock.
    fn reseal_all(&self) -> CoreResult<Resealed> {
        let held = StoreLock::exclusive(&self.store).unwrap();
        let (key, retiring) = (self.key(), [self.retiring()]);
        let mut done = Resealed::default();
        for folder in state_folders(&self.store)? {
            reseal(
                &self.store,
                &held,
                &folder,
                &key,
                &OldKeys::Retiring(&retiring),
                ResealMode::Rotate,
                &mut done,
            )?;
        }
        Ok(done)
    }
}

/// `folder`'s state, its `hetzner_cloud` as given, saved as a test saves it.
fn seed(store: &TargetStorePaths, folder: &str, hetzner: serde_json::Value) {
    State {
        hetzner_cloud: Some(serde_json::from_value(hetzner).unwrap()),
        ..Default::default()
    }
    .save(&StatePaths::for_active_target(store, folder))
    .unwrap();
}

fn hetzner_of(store: &TargetStorePaths, folder: &str) -> HetznerCloudState {
    State::load_or_default(&StatePaths::for_active_target(store, folder))
        .unwrap()
        .hetzner_cloud
        .unwrap()
}

fn state_file(store: &TargetStorePaths, folder: &str) -> PathBuf {
    StatePaths::for_active_target(store, folder).state_file()
}

fn cache(h: &HetznerCloudState, secret: CachedSecret) -> &str {
    match secret {
        CachedSecret::Kubeconfig => h.kubeconfig_age.as_deref(),
        CachedSecret::ArgocdPassword => h.argocd_admin_password_age.as_deref(),
    }
    .unwrap()
}

fn sealed(to: &Recipient, plain: &str) -> String {
    encrypt_for_recipient(plain, to).unwrap()
}

fn opens(with: &Identity, armored: &str) -> Option<String> {
    decrypt_with_identity_reason(armored, with).ok()
}

/// B63, B86: every cache sealed to the retiring store key is re-sealed to the new one, checked,
/// and recorded with the new vault's id — an older record (none) as well; the retiring key
/// opens none of them afterwards; every field the re-seal does not own stays.
#[test]
fn a_key_change_reseals_every_cache_to_the_new_key_and_records_its_vault() {
    let m = mid(&[("prod", Some(TOKEN_A)), ("staging", None)]);
    let old = m.made.recipient().clone();
    let before = serde_json::to_value(m.record(m.made.vault_id)).unwrap();
    seed(
        &m.store,
        "prod",
        json!({
            "server_id": 1, "server_name": "p", "network_id": 100,
            "kubeconfig_age": sealed(&old, YAML), "kubeconfig_sealed_to": before,
            "argocd_admin_password_age": sealed(&old, PW),
            "argocd_admin_password_sealed_to": before,
        }),
    );
    seed(
        &m.store,
        "staging",
        json!({"server_id": 2, "server_name": "s", "argocd_admin_password_age": sealed(&old, PW)}),
    );
    let done = m.reseal_all().unwrap();
    assert_eq!(done.resealed, 3);
    assert!(done.left.is_empty() && done.dropped.is_empty(), "{done:?}");
    assert!(
        done.orphans.is_empty() && done.set_aside.is_empty(),
        "{done:?}"
    );
    let (now, retiring, new) = (m.now(), m.retiring(), m.record(m.new_vault()));
    for (folder, secret, plain) in [
        ("prod", CachedSecret::Kubeconfig, YAML),
        ("prod", CachedSecret::ArgocdPassword, PW),
        ("staging", CachedSecret::ArgocdPassword, PW),
    ] {
        let h = hetzner_of(&m.store, folder);
        assert_eq!(
            opens(&now, cache(&h, secret)).as_deref(),
            Some(plain),
            "{folder} {secret:?}"
        );
        assert_eq!(
            opens(&retiring, cache(&h, secret)),
            None,
            "{folder} {secret:?}"
        );
        assert_eq!(h.sealed_to(secret), Some(&new), "{folder} {secret:?}");
    }
    let prod = hetzner_of(&m.store, "prod");
    assert_eq!((prod.server_id, prod.network_id), (1, Some(100)));
}

/// B85: a cache neither key opens is left as it was, with its record, and named with its
/// refresh — never dropped, forward or on a finish.
#[test]
fn a_key_change_never_drops_a_cache_no_key_opens() {
    let m = mid(&[("prod", Some(TOKEN_A))]);
    let before = serde_json::to_value(m.record(m.made.vault_id)).unwrap();
    let foreign = sealed(&Identity::generate().to_public(), YAML);
    seed(
        &m.store,
        "prod",
        json!({
            "server_id": 1, "server_name": "p",
            "kubeconfig_age": foreign.clone(), "kubeconfig_sealed_to": before,
            "argocd_admin_password_age": sealed(m.made.recipient(), PW),
        }),
    );
    let left = DroppedCache {
        target: "prod".into(),
        secret: "kubeconfig".into(),
        why: "it is sealed to a key this key change did not have, or it is damaged: \
              `apprafter kubeconfig --refresh --target prod` fetches it again"
            .into(),
    };
    for pass in ["forward", "a finish"] {
        let done = m.reseal_all().unwrap();
        assert_eq!(done.left, vec![left.clone()], "{pass}");
        assert!(done.dropped.is_empty(), "{pass}");
        let h = hetzner_of(&m.store, "prod");
        assert_eq!(
            h.kubeconfig_age.as_deref(),
            Some(foreign.as_str()),
            "{pass}: never dropped"
        );
        assert_eq!(
            h.sealed_to(CachedSecret::Kubeconfig),
            Some(&m.record(m.made.vault_id)),
            "{pass}: its record stays"
        );
    }
}

/// B63 (a finish): what a pass re-sealed the next one skips — nothing re-sealed twice, the
/// file's bytes as they were — and a cache the new key opens whose record names another vault
/// is recorded anew, once.
#[test]
fn a_finish_skips_what_is_resealed_and_records_it_once() {
    let m = mid(&[("prod", Some(TOKEN_A))]);
    let old = m.made.recipient().clone();
    seed(
        &m.store,
        "prod",
        json!({
            "server_id": 1, "server_name": "p",
            "kubeconfig_age": sealed(&old, YAML), "argocd_admin_password_age": sealed(&old, PW),
        }),
    );
    assert_eq!(m.reseal_all().unwrap().resealed, 2);
    let first = std::fs::read(state_file(&m.store, "prod")).unwrap();
    assert_eq!(m.reseal_all().unwrap().resealed, 0);
    assert_eq!(
        std::fs::read(state_file(&m.store, "prod")).unwrap(),
        first,
        "nothing re-sealed twice"
    );
    let mut h = hetzner_of(&m.store, "prod");
    h.argocd_admin_password_sealed_to = Some(m.record(m.made.vault_id));
    seed(&m.store, "prod", serde_json::to_value(&h).unwrap());
    assert_eq!(m.reseal_all().unwrap().resealed, 0);
    assert_eq!(
        hetzner_of(&m.store, "prod").sealed_to(CachedSecret::ArgocdPassword),
        Some(&m.record(m.new_vault()))
    );
}

/// B85: a state folder of no target — a best-effort `target remove` or `rename` left it — is
/// re-sealed like a target's and named, in B85's words; a cache in it no key opens is left,
/// with words that name no refresh (there is no target to refresh), and so is a kubeconfig in
/// clear beside the sealed one (critic round, finding 15).
#[test]
fn a_state_folder_of_no_target_is_resealed_and_named() {
    let m = mid(&[("prod", Some(TOKEN_A))]);
    seed(
        &m.store,
        "old",
        json!({
            "server_id": 9, "server_name": "o", "kubeconfig_yaml": YAML,
            "kubeconfig_age": sealed(m.made.recipient(), YAML),
            "argocd_admin_password_age": sealed(&Identity::generate().to_public(), PW),
        }),
    );
    let done = m.reseal_all().unwrap();
    assert_eq!(done.orphans, vec![m.store.state_dir("old")]);
    assert_eq!(done.resealed, 1);
    let h = hetzner_of(&m.store, "old");
    assert_eq!(
        opens(&m.now(), cache(&h, CachedSecret::Kubeconfig)).as_deref(),
        Some(YAML)
    );
    assert_eq!(
        done.left,
        vec![
            DroppedCache {
                target: "old".into(),
                secret: "argocd_password".into(),
                why: "it is sealed to a key this key change did not have, or it is damaged; \
                      no target uses its folder"
                    .into(),
            },
            DroppedCache {
                target: "old".into(),
                secret: "kubeconfig".into(),
                why: "a copy in clear, which a locked store never seals; no target uses its \
                      folder"
                    .into(),
            },
        ]
    );
    assert_eq!(
        orphan_line(&done.orphans[0]),
        format!(
            "a state folder left by an earlier target remove or rename: {} — delete it unless \
             a target will use that name",
            m.store.state_dir("old").display()
        )
    );
}

/// B85: R3's walk is every folder of `<root>/state/` a target could use, in name order; a
/// folder is a target's when `<root>/targets/<name>` is a folder. Files, dot-folders and a
/// target with no state folder are not walked.
#[test]
fn the_state_folders_are_every_folder_of_state_in_name_order() {
    let m = mid(&[("prod", None), ("staging", None), ("fresh", None)]);
    for name in ["staging", "old", "prod", "gone", ".scratch"] {
        std::fs::create_dir_all(m.store.state_dir(name)).unwrap();
    }
    std::fs::write(m.store.state_dir("notes.txt"), b"not a folder").unwrap();
    std::fs::write(
        m.store.target_dir("gone"),
        b"a file where a target's folder goes",
    )
    .unwrap();
    let folder = |name: &str, orphan| StateFolder {
        name: name.into(),
        orphan,
    };
    assert_eq!(
        state_folders(&m.store).unwrap(),
        vec![
            folder("gone", true),
            folder("old", true),
            folder("prod", false),
            folder("staging", false),
        ]
    );
}

/// B79: a `state.json` that does not parse is renamed aside — never deleted — and named; the
/// other folders are re-sealed all the same.
#[test]
fn an_unparsable_state_is_set_aside_and_named() {
    let m = mid(&[("prod", Some(TOKEN_A)), ("staging", None)]);
    seed(
        &m.store,
        "prod",
        json!({
            "server_id": 1, "server_name": "p",
            "kubeconfig_age": sealed(m.made.recipient(), YAML),
        }),
    );
    let staging = StatePaths::for_active_target(&m.store, "staging");
    staging.create_state_dir().unwrap();
    std::fs::write(staging.state_file(), b"{ not json").unwrap();
    let done = m.reseal_all().unwrap();
    assert_eq!(done.resealed, 1);
    assert_eq!(done.set_aside.len(), 1, "{done:?}");
    let aside = &done.set_aside[0];
    assert_eq!(
        aside.folder,
        StateFolder {
            name: "staging".into(),
            orphan: false
        }
    );
    assert_eq!(aside.to.parent(), Some(staging.state_dir().as_path()));
    let name = aside.to.file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.starts_with("state.json.corrupt-"), "{name}");
    assert_eq!(
        std::fs::read(&aside.to).unwrap(),
        b"{ not json",
        "renamed, never deleted"
    );
    assert!(!staging.state_file().exists());
    let to = aside.to.display();
    assert_eq!(
        set_aside_line(aside),
        format!(
            "the state file of target `staging` did not parse and was renamed to {to}: \
             `apprafter import --force --target staging` rebuilds it, then `apprafter kubeconfig \
             --refresh --target staging` and `apprafter argocd-password --refresh --target \
             staging` fetch its cached secrets again — they were sealed to the store key this key \
             change retired"
        )
    );
    let orphan = SetAside {
        folder: StateFolder {
            name: "old".into(),
            orphan: true,
        },
        to: PathBuf::from("/s/state/old/.apprafter/state.json.corrupt-7"),
    };
    assert_eq!(
        set_aside_line(&orphan),
        "a state file in a folder no target uses did not parse and was renamed to \
         /s/state/old/.apprafter/state.json.corrupt-7"
    );
}

/// B78 (Unix): every `state.json` R3 walks is on disk durably before the step returns — the one
/// it re-seals, and the one an earlier pass re-sealed and this one keeps: a folder that cannot
/// be synced fails the step, naming the file; the finish then completes.
#[cfg(unix)]
#[test]
fn every_state_file_a_key_change_walks_is_durable() {
    for earlier_pass in [false, true] {
        let m = mid(&[("prod", Some(TOKEN_A))]);
        seed(
            &m.store,
            "prod",
            json!({
                "server_id": 1, "server_name": "p",
                "kubeconfig_age": sealed(m.made.recipient(), YAML),
            }),
        );
        if earlier_pass {
            m.reseal_all().unwrap();
        }
        cli_core::fsutil::testing::fail_next_dir_sync();
        let err = m.reseal_all().unwrap_err();
        let file = state_file(&m.store, "prod").display().to_string();
        assert!(err.to_string().contains(&file), "{earlier_pass}: {err}");
        let done = m.reseal_all().unwrap();
        assert_eq!(
            done.resealed, 0,
            "{earlier_pass}: the moved file holds the new bytes"
        );
        let h = hetzner_of(&m.store, "prod");
        assert_eq!(
            opens(&m.now(), cache(&h, CachedSecret::Kubeconfig)).as_deref(),
            Some(YAML)
        );
    }
}

/// B85 (critic round, finding 1): a target's state folder that is a link to a folder
/// elsewhere — every reader and writer of its state goes through the link — is re-sealed like
/// any other, and is no orphan.
#[cfg(unix)]
#[test]
fn a_state_folder_reached_through_a_link_is_resealed() {
    let m = mid(&[("prod", Some(TOKEN_A))]);
    let elsewhere = m._dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir_all(m.store.state_dir("prod").parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, m.store.state_dir("prod")).unwrap();
    seed(
        &m.store,
        "prod",
        json!({
            "server_id": 1, "server_name": "p",
            "kubeconfig_age": sealed(m.made.recipient(), YAML),
        }),
    );
    let done = m.reseal_all().unwrap();
    assert_eq!(done.resealed, 1, "{done:?}");
    assert!(done.orphans.is_empty(), "{done:?}");
    let h = hetzner_of(&m.store, "prod");
    assert_eq!(
        opens(&m.now(), cache(&h, CachedSecret::Kubeconfig)).as_deref(),
        Some(YAML)
    );
}

/// B78 (Windows): every `state.json` R3 walks moves with write-through — the one it re-seals
/// and, on a finish, the one it keeps.
#[cfg(windows)]
#[test]
fn every_state_file_a_key_change_walks_moves_with_write_through() {
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    let m = mid(&[("prod", Some(TOKEN_A)), ("staging", None)]);
    for folder in ["prod", "staging"] {
        seed(
            &m.store,
            folder,
            json!({
                "server_id": 1, "server_name": "p",
                "kubeconfig_age": sealed(m.made.recipient(), YAML),
            }),
        );
    }
    // Critic round, finding 10: a busy retry records the same flags again, so a pass is at
    // least one move per folder, every one with write-through.
    let pass = |why: &str| {
        m.reseal_all().unwrap();
        let moves = cli_core::fsutil::testing::take_moves();
        assert!(
            moves.len() >= 2 && moves.iter().all(|f| *f == flags),
            "{why}: {moves:?}"
        );
    };
    let _ = cli_core::fsutil::testing::take_moves();
    pass("forward");
    pass("a finish keeps each, durably");
}

/// T38: a key change never seals a plaintext kubeconfig a locked store's state holds (an older
/// AppRafter's write, or a forgery): it is left as it is, and named with the refresh that
/// replaces it (critic round, finding 15: never masked).
#[test]
fn a_key_change_leaves_a_plaintext_kubeconfig_as_it_is() {
    let m = mid(&[("prod", Some(TOKEN_A))]);
    seed(
        &m.store,
        "prod",
        json!({
            "server_id": 1, "server_name": "p", "kubeconfig_yaml": YAML,
            "argocd_admin_password_age": sealed(m.made.recipient(), PW),
        }),
    );
    let done = m.reseal_all().unwrap();
    assert_eq!(done.resealed, 1);
    let h = hetzner_of(&m.store, "prod");
    assert_eq!(
        (h.kubeconfig_yaml.as_deref(), h.kubeconfig_age.as_deref()),
        (Some(YAML), None)
    );
    assert_eq!(
        done.left,
        vec![DroppedCache {
            target: "prod".into(),
            secret: "kubeconfig".into(),
            why: "a copy in clear, which a locked store never seals: `apprafter kubeconfig \
                  --refresh --target prod` replaces it with a sealed one"
                .into(),
        }]
    );
}

/// Rule 2: a new key that cannot open — here a session for another vault id — fails the step;
/// it never reads as "no key opens this cache", which would leave it, named.
#[test]
fn a_key_that_cannot_open_fails_the_step_and_leaves_nothing() {
    let m = mid(&[("prod", Some(TOKEN_A))]);
    seed(
        &m.store,
        "prod",
        json!({
            "server_id": 1, "server_name": "p",
            "kubeconfig_age": sealed(&Identity::generate().to_public(), YAML),
        }),
    );
    let before = std::fs::read(state_file(&m.store, "prod")).unwrap();
    let stale = transfer::store_key(
        m.root(),
        KeySession::new(
            m.rotated.vault.data_key().clone(),
            m.made.vault_id,
            m.root().to_path_buf(),
        ),
        Arc::from(m.rotated.bytes.clone()),
    );
    let held = StoreLock::exclusive(&m.store).unwrap();
    let retiring = [m.retiring()];
    let mut done = Resealed::default();
    let err = reseal(
        &m.store,
        &held,
        &StateFolder::target("prod"),
        &stale,
        &OldKeys::Retiring(&retiring),
        ResealMode::Rotate,
        &mut done,
    )
    .unwrap_err();
    assert!(
        matches!(err, CoreError::Cli(CliError::StoreChanged { .. })),
        "{err:?}"
    );
    assert!(done.left.is_empty(), "{done:?}");
    assert_eq!(std::fs::read(state_file(&m.store, "prod")).unwrap(), before);
}

/// B86: after a key change, a cache sealed to the store key it retired says so — its record
/// names this store's earlier vault — for both cached secrets; a record without a vault id
/// (AppRafter 0.2.80's), another store's, or the vault on disk keeps the words as they were,
/// and a damaged cache stays damaged.
#[test]
fn a_cache_sealed_to_a_retired_store_key_says_so() {
    let m = mid(&[("prod", Some(TOKEN_A))]);
    // R4: the retiring key leaves the vault.
    let retired = m
        .rotated
        .vault
        .retire_identity(WRITER, &mut OsRandom)
        .unwrap();
    let held = StoreLock::exclusive(&m.store).unwrap();
    vault::file::replace(&vault_path(m.root()), &retired, &held).unwrap();
    drop(held);
    let ctx = Context::for_desktop(m.root().to_path_buf(), "http://127.0.0.1:1")
        .with_home_dir(Some(m._dir.path().join("home")))
        .with_vault_access(Arc::new(FixedVaultAccess::new(
            m.rotated.vault.data_key().clone(),
            m.new_vault(),
        )));
    let s = ctx.secrets();
    let read = |record: serde_json::Value, armored: &str| {
        let h: HetznerCloudState = serde_json::from_value(json!({
            "server_id": 1, "server_name": "p",
            "kubeconfig_age": armored, "kubeconfig_sealed_to": record,
            "argocd_admin_password_age": armored, "argocd_admin_password_sealed_to": record,
        }))
        .unwrap();
        (
            s.cached_kubeconfig(&h, "prod").unwrap_err(),
            s.cached_argocd_password(&h, "prod").unwrap_err(),
        )
    };
    let why = |e: &CliError| match e {
        CliError::CacheUndecryptable {
            key: CacheKeyKind::StoreVault,
            reason,
            ..
        } => *reason,
        other => panic!("{other:?}"),
    };
    let old = sealed(m.made.recipient(), YAML);
    let earlier = serde_json::to_value(m.record(m.made.vault_id)).unwrap();
    let (kube, pw) = read(earlier.clone(), &old);
    assert_eq!(
        (why(&kube), why(&pw)),
        (UndecryptableWhy::RetiredKey, UndecryptableWhy::RetiredKey)
    );
    assert!(
        kube.to_string()
            .contains("is recorded as sealed to the store key of an earlier key change"),
        "{kube}"
    );
    // Critic round, finding 3: a cache R3 left (neither key opened it) keeps its record, so
    // it reads with the same reason — and the words say only what the record says.
    let left = sealed(&Identity::generate().to_public(), YAML);
    assert_eq!(
        why(&read(earlier.clone(), &left).0),
        UndecryptableWhy::RetiredKey
    );
    let older = json!({"kind": "store_vault", "store_id": m.made.store_id.as_str()});
    assert_eq!(
        why(&read(older, &old).0),
        UndecryptableWhy::NotForThisKey,
        "0.2.80's record"
    );
    let other = json!({
        "kind": "store_vault", "store_id": "0123456789abcdef",
        "vault_id": m.made.vault_id.to_string(),
    });
    assert_eq!(
        why(&read(other, &old).0),
        UndecryptableWhy::NotForThisKey,
        "another store's"
    );
    let foreign = sealed(&Identity::generate().to_public(), YAML);
    let here = serde_json::to_value(m.record(m.new_vault())).unwrap();
    assert_eq!(
        why(&read(here, &foreign).0),
        UndecryptableWhy::NotForThisKey,
        "the vault on disk"
    );
    assert_eq!(
        why(&read(earlier, "not an age file").0),
        UndecryptableWhy::Damaged,
        "damaged stays damaged"
    );
}
