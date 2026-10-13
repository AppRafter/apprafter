// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A locked target store for tests (D.4a §3.2), in a directory the test owns: one recovery-key
//! slot and no key derivation, so it costs nothing in either build (with `insecure-fast-kdf` or
//! without, T23), and it opens with the access it returns or with its recovery key, as shown.
//! Never in a shipped build (`test-support`, which only `[dev-dependencies]` enable).

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use cli_core::secrets::{Identity, Recipient};
use cli_core::target::{TargetConfig, TargetStorePaths};
use cli_core::StoreLock;

use super::access::{FixedVaultAccess, VaultAccess};
use super::files::under_temp;
use super::locked::{vault_path, VAULT_WRITER};
use super::marker::write_marker;
use super::store_file::{
    write_store_file, LockSlotKind, SlotInfo, StoreFile, StoreId, STORE_FILE_FORMAT,
};
use super::SecretsMode;
use crate::context::SecretString;
use crate::vault::{
    self, DataKey, KdfPolicy, NewSlot, OsRandom, RecoveryKey, VaultId, VaultPayload,
};

/// The label of the test store's one slot.
pub const RECOVERY_LABEL: &str = "recovery key";

/// What [`locked_store`] made.
pub struct LockedTestStore {
    /// `store.yaml`'s id, which the vault names too.
    pub store_id: StoreId,
    pub vault_id: VaultId,
    key: Arc<DataKey>,
    recovery: SecretString,
    recipient: Recipient,
}

impl LockedTestStore {
    /// A `VaultAccess` that opens this store: its data key, for its vault.
    pub fn access(&self) -> Arc<dyn VaultAccess> {
        Arc::new(FixedVaultAccess::new(self.key.clone(), self.vault_id))
    }

    pub fn data_key(&self) -> &Arc<DataKey> {
        &self.key
    }

    /// The recovery key as `lock on` shows it (two lines).
    pub fn recovery_key(&self) -> &SecretString {
        &self.recovery
    }

    /// The store identity's recipient, to seal a test cache to.
    pub fn recipient(&self) -> &Recipient {
        &self.recipient
    }
}

impl fmt::Debug for LockedTestStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LockedTestStore")
            .field("store_id", &self.store_id)
            .field("key", &"<redacted>")
            .field("recovery", &"<redacted>")
            .finish()
    }
}

/// A store at `root`, locked: each `(name, token)` a target (`hetzner-cloud`, `nbg1`) whose
/// token, when given, is in the vault behind its marker; a fresh store id and age identity;
/// the vault written first and `store.yaml` last, as `lock on` ends. Panics on any failure:
/// test setup.
pub fn locked_store(root: &Path, targets: &[(&str, Option<&str>)]) -> LockedTestStore {
    under_temp(root);
    let store = TargetStorePaths::for_root(root.to_path_buf());
    for (name, _) in targets {
        let config = TargetConfig {
            provider: "hetzner-cloud".into(),
            region: Some("nbg1".into()),
            ..Default::default()
        };
        cli_core::target::save_target_config(&store, name, &config)
            .expect("a test target's config.yaml");
    }
    let store_id = StoreId::generate(&mut OsRandom).expect("a store id");
    let identity = Identity::generate();
    let mut payload = VaultPayload::new(&identity);
    for (name, token) in targets {
        if let Some(token) = token {
            payload
                .set_hetzner_token(name, SecretString::new(*token))
                .expect("a test token fits the vault");
        }
    }
    let recovery = RecoveryKey::generate(&mut OsRandom).expect("a recovery key");
    let shown = recovery.to_display();
    let slot = NewSlot::RecoveryKey {
        key: recovery,
        label: RECOVERY_LABEL.into(),
    };
    let (bytes, unlocked) = vault::create(
        store_id.as_str(),
        VAULT_WRITER,
        payload,
        vec![slot],
        KdfPolicy::active(),
        &mut OsRandom,
    )
    .expect("a test vault");
    let held = StoreLock::exclusive(&store).expect("the test store's write lock");
    // c1's first write takes the held lock too (its Deviation 9).
    vault::file::write_new(&vault_path(root), &bytes, &held).expect("the test vault, written");
    for (name, token) in targets {
        if token.is_some() {
            write_marker(&store, name, &store_id).expect("a marker");
        }
    }
    let file = StoreFile {
        format: STORE_FILE_FORMAT,
        secrets: SecretsMode::Locked,
        id: Some(store_id.clone()),
        slots: vec![SlotInfo {
            kind: LockSlotKind::RecoveryKey,
            label: RECOVERY_LABEL.into(),
        }],
        unlock_timeout_min: Some(5),
    };
    write_store_file(root, &file, &held).expect("store.yaml");
    drop(held);
    LockedTestStore {
        store_id,
        vault_id: unlocked.header().vault_id,
        key: unlocked.data_key().clone(),
        recovery: shown,
        recipient: identity.to_public(),
    }
}

/// A `VaultAccess` that opens nothing and counts how often it was asked (D.4a T35): a refusal
/// that must come before any prompt leaves the count at 0. Every question is `store_locked`.
#[derive(Debug, Default)]
pub struct CountingAccess {
    asked: std::sync::atomic::AtomicUsize,
}

impl CountingAccess {
    /// How many sessions and write keys were asked for.
    pub fn asked(&self) -> usize {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl VaultAccess for CountingAccess {
    fn session(
        &self,
        req: &super::access::VaultRequest<'_>,
    ) -> cli_core::Result<Arc<dyn super::access::VaultSession>> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(super::store_locked(req.store_root))
    }

    fn write_key(
        &self,
        req: &super::access::VaultRequest<'_>,
    ) -> cli_core::Result<super::access::KeySession> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(super::store_locked(req.store_root))
    }
}
