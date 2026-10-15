// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock on` (D.4a T36, §3.9; rule 4) on scratch stores in files, under the fast KDF policy and
//! its permit (`Context::with_kdf_policy`, `with_insecure_kdf_root`): no test here derives a
//! release key (T23). A run is interrupted at each `FaultPoint`, and every interruption is shown
//! to lose nothing and to finish when run again.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli_core::secrets::{encrypt_for_recipient, load_or_create_identity, CachedSecret, Identity};
use cli_core::target::{save_target_credentials, TargetCredentials};
use cli_core::CliError;
use cli_state::{HetznerCloudState, State, StatePaths};

use super::*;
use crate::secrets::marker::{classify, Marker};
use crate::secrets::{
    read_store_file, write_store_file, FixedVaultAccess, LockSlotKind, SecretsMode,
};
use crate::target::testkit::{store, TOKEN_A, TOKEN_B};
use crate::vault::{
    KdfPolicy, NewSlot, OsRandom, RecoveryKey, SlotInput, SlotKind, SlotSecret, VaultFile,
    VAULT_FILE,
};
use crate::{
    CancellationToken, ChangeAction, Context, CoreError, NullReporter, Outcome, PlanClass,
    SecretString,
};

pub(super) const PASSWORD: &str = "correct horse battery staple";
pub(super) const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";
pub(super) const PW: &str = "argocd-admin-password";
const TOKEN_C: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

/// A store in files: `prod` (TOKEN_A) caches its kubeconfig and password under the shared key,
/// with a stale plaintext kubeconfig beside them; `staging` (TOKEN_B) caches a plaintext
/// kubeconfig and a password no key here opens, recorded as sealed to a vault no longer here
/// (F13). The context runs the flows under the fast policy, its permit the scratch dir.
pub(super) struct Files {
    pub dir: tempfile::TempDir,
    pub ctx: Context,
}

pub(super) fn files_store() -> Files {
    let (dir, ctx) = store(&["prod", "staging"], Some("prod"));
    let ctx = ctx
        .with_kdf_policy(&KdfPolicy::INSECURE_FAST)
        .with_insecure_kdf_root(dir.path().to_path_buf());
    save_target_credentials(
        &ctx.store(),
        "staging",
        &TargetCredentials {
            hetzner_token: Some(TOKEN_B.into()),
        },
    )
    .unwrap();
    let shared = load_or_create_identity(ctx.age_key_path()).unwrap();
    let foreign = Identity::generate();
    seed(
        &ctx,
        "prod",
        serde_json::json!({
            "server_id": 1, "server_name": "p",
            "kubeconfig_age": encrypt_for_recipient(YAML, &shared.to_public()).unwrap(),
            "kubeconfig_yaml": "clusters: [stale]\n",
            "argocd_admin_password_age": encrypt_for_recipient(PW, &shared.to_public()).unwrap(),
        }),
    );
    seed(
        &ctx,
        "staging",
        serde_json::json!({
            "server_id": 2, "server_name": "s",
            "kubeconfig_yaml": YAML,
            "argocd_admin_password_age": encrypt_for_recipient(PW, &foreign.to_public()).unwrap(),
            "argocd_admin_password_sealed_to": {"kind": "store_vault", "store_id": "0123456789abcdef"},
        }),
    );
    Files { dir, ctx }
}

/// `target`'s state, its `hetzner_cloud` as given (`off_tests` seeds with it too).
pub(super) fn seed(ctx: &Context, target: &str, hetzner: serde_json::Value) {
    State {
        hetzner_cloud: Some(serde_json::from_value(hetzner).unwrap()),
        ..Default::default()
    }
    .save(&StatePaths::for_active_target(&ctx.store(), target))
    .unwrap();
}

pub(super) fn hetzner_of(ctx: &Context, target: &str) -> HetznerCloudState {
    State::load_or_default(&StatePaths::for_active_target(&ctx.store(), target))
        .unwrap()
        .hetzner_cloud
        .unwrap()
}

/// A recovery key, generated and typed back in full; its text.
pub(super) fn confirmed() -> (crate::vault::ConfirmedRecoveryKey, String) {
    let key = RecoveryKey::generate(&mut OsRandom).unwrap();
    let shown = key.to_display().expose().to_string();
    (key.confirm(&shown).unwrap(), shown)
}

pub(super) fn new_input() -> (LockOnInput, String) {
    let (recovery, shown) = confirmed();
    (
        LockOnInput::New {
            password: SecretString::new(PASSWORD),
            recovery,
        },
        shown,
    )
}

/// `lock on` run to its end; the recovery key it was given.
pub(super) fn lock(ctx: &Context) -> String {
    let plan = plan_on(ctx, None).unwrap();
    let (input, shown) = new_input();
    let done = execute_on(ctx, plan, input, &NullReporter, &CancellationToken::new()).unwrap();
    assert!(matches!(done, Outcome::Completed { .. }), "{done:?}");
    shown
}

/// `ctx`'s locked store, opened with `secret` (either kind) into a context that reads it.
pub(super) fn opened(ctx: &Context, secret: &str) -> Context {
    let file = read_store_file(ctx.config_root()).unwrap().unwrap();
    let key = open_at(
        ctx.config_root(),
        file.id.as_ref().unwrap(),
        SlotInput::from_input(SecretString::new(secret)),
        &KdfPolicy::INSECURE_FAST,
    )
    .unwrap();
    Context::for_desktop(ctx.config_root().to_path_buf(), "http://127.0.0.1:1")
        .with_home_dir(ctx.home_dir().map(Path::to_path_buf))
        .with_vault_access(Arc::new(FixedVaultAccess::new(key.key, key.vault_id)))
}

/// `target`'s token wherever it is: in clear in its file, or in the vault behind its marker
/// (opened with the recovery key `shown`).
pub(super) fn token_of(ctx: &Context, shown: &str, target: &str) -> String {
    let bytes = std::fs::read(ctx.store().target_credentials_file(target)).unwrap();
    match classify(&bytes) {
        Marker::ClearToken => serde_yaml::from_slice::<serde_yaml::Value>(&bytes).unwrap()
            ["hetzner_token"]
            .as_str()
            .unwrap()
            .to_string(),
        Marker::Vault { id } => {
            let o = open_at(
                ctx.config_root(),
                &id,
                SlotInput::from_input(SecretString::new(shown)),
                &KdfPolicy::INSECURE_FAST,
            )
            .unwrap();
            let vault = std::fs::read(ctx.config_root().join(VAULT_FILE)).unwrap();
            VaultFile::parse(&vault)
                .unwrap()
                .open_with_key(&o.key, o.vault_id)
                .unwrap()
                .payload()
                .hetzner_token(target)
                .unwrap()
                .expose()
                .to_string()
        }
        other => panic!("{target}: {other:?}"),
    }
}

/// Every file under `root` holding `needle`.
pub(super) fn anywhere(root: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if String::from_utf8_lossy(&std::fs::read(&path).unwrap()).contains(needle) {
                out.push(path);
            }
        }
    }
    out
}

/// The locked store as a whole run leaves it: every token in the vault behind its marker, the
/// caches under the store's key, no plaintext anywhere (rule 2).
pub(super) fn assert_locked_whole(ctx: &Context, secret: &str) {
    let root = ctx.config_root();
    let file = read_store_file(root).unwrap().unwrap();
    assert_eq!(file.secrets, SecretsMode::Locked);
    let reader = opened(ctx, secret);
    let s = reader.secrets();
    for (target, token) in [("prod", TOKEN_A), ("staging", TOKEN_B)] {
        assert_eq!(s.stored_token(target).unwrap().unwrap().expose(), token);
        assert_eq!(
            classify(&std::fs::read(ctx.store().target_credentials_file(target)).unwrap()),
            Marker::Vault {
                id: file.id.clone().unwrap()
            }
        );
    }
    let prod = hetzner_of(ctx, "prod");
    assert!(
        prod.kubeconfig_yaml.is_none(),
        "the shadowed plaintext went"
    );
    assert_eq!(
        s.cached_kubeconfig(&prod, "prod")
            .unwrap()
            .unwrap()
            .as_str(),
        YAML
    );
    let key = s.cache_key("prod").unwrap();
    let pw = prod.argocd_admin_password_age.as_deref().unwrap();
    assert_eq!(
        s.open_cache(&key, CachedSecret::ArgocdPassword, "prod", pw)
            .unwrap()
            .as_str(),
        PW
    );
    let staging = hetzner_of(ctx, "staging");
    assert!(staging.kubeconfig_yaml.is_none());
    assert_eq!(
        s.cached_kubeconfig(&staging, "staging")
            .unwrap()
            .unwrap()
            .as_str(),
        YAML
    );
    assert!(
        staging.argocd_admin_password_age.is_none(),
        "the cache no key opens was dropped"
    );
    // F13: each cache sealed to the store's key says so; the dropped one has no record.
    let vault = Some(cli_state::SealedTo::StoreVault {
        store_id: file.id.as_ref().unwrap().as_str().to_string(),
    });
    assert_eq!(
        (
            &prod.kubeconfig_sealed_to,
            &prod.argocd_admin_password_sealed_to
        ),
        (&vault, &vault),
        "prod"
    );
    assert_eq!(
        (
            &staging.kubeconfig_sealed_to,
            &staging.argocd_admin_password_sealed_to
        ),
        (&vault, &None),
        "staging: its plaintext kubeconfig sealed, its password dropped"
    );
    for needle in [TOKEN_A, TOKEN_B, "apiVersion: v1", "clusters: [stale]", PW] {
        assert!(
            anywhere(root, needle).is_empty(),
            "{needle:?} in clear: {:?}",
            anywhere(root, needle)
        );
    }
}

#[test]
fn lock_on_puts_every_secret_in_the_vault_and_none_stays_in_clear() {
    let f = files_store();
    let shared_before = std::fs::read(f.ctx.age_key_path()).unwrap();
    let shown = lock(&f.ctx);
    let file = read_store_file(f.ctx.config_root()).unwrap().unwrap();
    assert_eq!(file.slots.len(), 2);
    assert_eq!(file.unlock_timeout_min, Some(5));
    assert_locked_whole(&f.ctx, PASSWORD);
    assert_locked_whole(&f.ctx, &shown);
    assert_eq!(
        std::fs::read(f.ctx.age_key_path()).unwrap(),
        shared_before,
        "the shared key stays for other stores"
    );
}

/// T36: the plan reads, writes nothing, and lists each move and each drop.
#[test]
fn the_plan_lists_each_move_and_writes_nothing() {
    let f = files_store();
    let before = anywhere(f.ctx.config_root(), "");
    let plan = plan_on(&f.ctx, Some(15)).unwrap();
    assert_eq!(plan.class, PlanClass::Bounded);
    assert_eq!(plan.payload.start(), &LockOnStart::New { restart: false });
    assert_eq!(plan.payload.timeout_min(), 15);
    let lines: Vec<(String, String, ChangeAction)> = plan
        .changes
        .iter()
        .map(|c| (c.kind.clone(), c.object.clone(), c.action))
        .collect();
    for want in [
        ("Credentials", "prod", ChangeAction::Move),
        ("Credentials", "staging", ChangeAction::Move),
        ("Cache", "prod", ChangeAction::Update),
        ("Cache", "prod", ChangeAction::Delete),
        ("Cache", "staging", ChangeAction::Update),
        ("Cache", "staging", ChangeAction::Delete),
        ("AgeKey", "this store", ChangeAction::Create),
    ] {
        assert!(
            lines.contains(&(want.0.to_string(), want.1.to_string(), want.2)),
            "{want:?} in {lines:?}"
        );
    }
    let drop = plan
        .changes
        .iter()
        .find(|c| c.object == "staging" && c.action == ChangeAction::Delete)
        .unwrap();
    assert!(
        drop.detail
            .as_deref()
            .unwrap()
            .contains("`apprafter argocd-password --refresh --target staging`"),
        "{drop:?}"
    );
    assert_eq!(anywhere(f.ctx.config_root(), ""), before, "nothing written");
    assert!(read_store_file(f.ctx.config_root()).unwrap().is_none());
}

/// T36: a credentials file or a state that cannot be read may hold the only copy of a secret:
/// the plan stops at it.
#[test]
fn an_unreadable_record_stops_the_plan() {
    let f = files_store();
    std::fs::write(
        f.ctx.store().target_credentials_file("prod"),
        "hetzner_token: [",
    )
    .unwrap();
    assert!(matches!(
        plan_on(&f.ctx, None).unwrap_err(),
        CoreError::Cli(CliError::InvalidTargetConfig { .. })
    ));
    let f = files_store();
    std::fs::write(
        StatePaths::for_active_target(&f.ctx.store(), "staging").state_file(),
        "{ not json",
    )
    .unwrap();
    assert!(plan_on(&f.ctx, None).is_err());
}

/// Rule 4 and T36: an interruption at every step loses no token, readers in between refuse,
/// and a run again finishes — a resume with both slots' secrets, each opening its own slot,
/// when a vault is on disk (team decision 2026-10-13), else a new run.
#[test]
fn an_interruption_at_every_step_loses_nothing_and_a_rerun_finishes() {
    for at in [
        FaultPoint::AfterStoreFileLocking,
        FaultPoint::AfterVaultWritten,
        FaultPoint::AfterVaultVerified,
        FaultPoint::AfterCachesTarget(0),
        FaultPoint::AfterCachesTarget(1),
        FaultPoint::AfterMarkerTarget(0),
        FaultPoint::AfterMarkerTarget(1),
        FaultPoint::AfterLocked,
    ] {
        let f = files_store();
        let plan = plan_on(&f.ctx, None).unwrap();
        let (input, shown) = new_input();
        let e = execute_on(
            &f.ctx.clone().with_fault(at),
            plan,
            input,
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("injected fault"), "{at:?}: {e}");
        for (target, token) in [("prod", TOKEN_A), ("staging", TOKEN_B)] {
            assert_eq!(token_of(&f.ctx, &shown, target), token, "{at:?}: {target}");
        }
        if at != FaultPoint::AfterLocked {
            assert!(
                matches!(
                    f.ctx.secrets().stored_token("prod").unwrap_err(),
                    CliError::LockIncomplete {
                        phase: cli_core::LockPhase::Locking,
                        ..
                    }
                ),
                "{at:?}: readers refuse"
            );
        }
        let plan = plan_on(&f.ctx, None).unwrap();
        let secret = match plan.payload.start().clone() {
            LockOnStart::Resume { id } => {
                let slot = |kind, secret: &str| {
                    open_slot_at(
                        f.ctx.config_root(),
                        &id,
                        kind,
                        SecretString::new(secret),
                        &KdfPolicy::INSECURE_FAST,
                    )
                    .unwrap()
                };
                execute_on(
                    &f.ctx,
                    plan,
                    LockOnInput::Resume {
                        password: slot(LockSlotKind::Password, PASSWORD),
                        recovery: slot(LockSlotKind::RecoveryKey, shown.as_str()),
                    },
                    &NullReporter,
                    &CancellationToken::new(),
                )
                .unwrap();
                shown
            }
            LockOnStart::New { restart: true } => {
                assert_eq!(at, FaultPoint::AfterStoreFileLocking);
                let (input, again) = new_input();
                execute_on(
                    &f.ctx,
                    plan,
                    input,
                    &NullReporter,
                    &CancellationToken::new(),
                )
                .unwrap();
                again
            }
            LockOnStart::AlreadyLocked => {
                assert_eq!(at, FaultPoint::AfterLocked);
                shown
            }
            other => panic!("{at:?}: {other:?}"),
        };
        assert_locked_whole(&f.ctx, &secret);
    }
}

/// F13: a resume meets caches sealed to the store's key already — by the interrupted run, or by
/// an older AppRafter that recorded nothing — and records them so all the same.
#[test]
fn a_resume_records_caches_already_sealed_to_the_store_s_key() {
    let f = files_store();
    let plan = plan_on(&f.ctx, None).unwrap();
    let (input, shown) = new_input();
    let e = execute_on(
        &f.ctx.clone().with_fault(FaultPoint::AfterCachesTarget(1)),
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("injected fault"), "{e}");
    for target in ["prod", "staging"] {
        let mut h = hetzner_of(&f.ctx, target);
        assert!(h.kubeconfig_age.is_some(), "{target}");
        h.kubeconfig_sealed_to = None;
        h.argocd_admin_password_sealed_to = None;
        seed(&f.ctx, target, serde_json::to_value(&h).unwrap());
    }
    let plan = plan_on(&f.ctx, None).unwrap();
    let LockOnStart::Resume { id } = plan.payload.start().clone() else {
        panic!("{:?}", plan.payload.start());
    };
    let slot = |kind, secret: &str| {
        open_slot_at(
            f.ctx.config_root(),
            &id,
            kind,
            SecretString::new(secret),
            &KdfPolicy::INSECURE_FAST,
        )
        .unwrap()
    };
    execute_on(
        &f.ctx,
        plan,
        LockOnInput::Resume {
            password: slot(LockSlotKind::Password, PASSWORD),
            recovery: slot(LockSlotKind::RecoveryKey, shown.as_str()),
        },
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_locked_whole(&f.ctx, &shown);
}

/// Critic G2: a resume never writes a marker over a clear token the vault does not hold — one an
/// older AppRafter wrote while the run was interrupted is left, and named.
#[test]
fn a_resume_never_replaces_a_clear_token_it_did_not_vault() {
    let f = files_store();
    let plan = plan_on(&f.ctx, None).unwrap();
    let (input, shown) = new_input();
    execute_on(
        &f.ctx.clone().with_fault(FaultPoint::AfterVaultVerified),
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    let store = f.ctx.store();
    save_target_credentials(
        &store,
        "staging",
        &TargetCredentials {
            hetzner_token: Some(TOKEN_C.into()),
        },
    )
    .unwrap();
    let plan = plan_on(&f.ctx, None).unwrap();
    let LockOnStart::Resume { id } = plan.payload.start().clone() else {
        panic!("a resume")
    };
    let slot = |kind, secret: &str| {
        open_slot_at(
            f.ctx.config_root(),
            &id,
            kind,
            SecretString::new(secret),
            &KdfPolicy::INSECURE_FAST,
        )
        .unwrap()
    };
    let done = match execute_on(
        &f.ctx,
        plan,
        LockOnInput::Resume {
            password: slot(LockSlotKind::Password, PASSWORD),
            recovery: slot(LockSlotKind::RecoveryKey, shown.as_str()),
        },
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap()
    {
        Outcome::Completed { result } => result,
        other => panic!("{other:?}"),
    };
    assert_eq!(done.tokens_left, vec!["staging".to_string()]);
    assert_eq!(
        token_of(&f.ctx, &shown, "staging"),
        TOKEN_C,
        "left in clear, not replaced"
    );
    assert!(matches!(
        opened_secrets(&f.ctx, &shown)
            .stored_token("staging")
            .unwrap_err(),
        CliError::TokenOutsideVault { .. }
    ));
}

fn opened_secrets(ctx: &Context, secret: &str) -> crate::secrets::SecretStore {
    opened(ctx, secret).secrets()
}

/// Rule 4 (team decision 2026-10-13): a resume opens the vault with the password slot AND the
/// recovery slot, each by a secret of its own kind, before any clear datum goes. A recovery key
/// typed for the lock password opens no slot, nor a password typed for the key; the lock
/// password given twice, or a recovery key that opens another store's vault, finishes nothing —
/// every token stays in its file and `store.yaml` says `locking`; both right, the run finishes.
#[test]
fn a_resume_opens_the_vault_with_both_slots_before_any_clear_data_goes() {
    let f = files_store();
    let plan = plan_on(&f.ctx, None).unwrap();
    let (input, shown) = new_input();
    execute_on(
        &f.ctx.clone().with_fault(FaultPoint::AfterVaultVerified),
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    let root = f.ctx.config_root().to_path_buf();
    let id = read_store_file(&root).unwrap().unwrap().id.unwrap();
    let slot = |kind: LockSlotKind, secret: &str| {
        open_slot_at(
            &root,
            &id,
            kind,
            SecretString::new(secret),
            &KdfPolicy::INSECURE_FAST,
        )
    };
    assert!(
        matches!(
            slot(LockSlotKind::Password, &shown),
            Err(CoreError::Cli(CliError::UnlockFailed { attempts: 1, .. }))
        ),
        "a recovery key typed for the lock password opens no slot"
    );
    assert!(
        matches!(
            slot(LockSlotKind::RecoveryKey, PASSWORD),
            Err(CoreError::RecoveryKeyInvalid { .. })
        ),
        "a password typed for the recovery key is no key"
    );
    let elsewhere = tempfile::tempdir().unwrap();
    let foreign =
        crate::secrets::testing::locked_store(elsewhere.path(), &[("prod", Some(TOKEN_A))]);
    let foreign_key = || {
        open_slot_at(
            elsewhere.path(),
            &foreign.store_id,
            LockSlotKind::RecoveryKey,
            SecretString::new(foreign.recovery_key().expose()),
            &KdfPolicy::INSECURE_FAST,
        )
        .unwrap()
    };
    let resume = |recovery: SlotOpened| {
        let plan = plan_on(&f.ctx, None).unwrap();
        assert!(
            matches!(plan.payload.start(), LockOnStart::Resume { .. }),
            "{:?}",
            plan.payload.start()
        );
        execute_on(
            &f.ctx,
            plan,
            LockOnInput::Resume {
                password: slot(LockSlotKind::Password, PASSWORD).unwrap(),
                recovery,
            },
            &NullReporter,
            &CancellationToken::new(),
        )
    };
    let untouched = |case: &str| {
        for (target, token) in [("prod", TOKEN_A), ("staging", TOKEN_B)] {
            let bytes = std::fs::read(f.ctx.store().target_credentials_file(target)).unwrap();
            assert_eq!(
                classify(&bytes),
                Marker::ClearToken,
                "{case}: {target} kept in clear"
            );
            assert!(
                String::from_utf8_lossy(&bytes).contains(token),
                "{case}: {target}"
            );
        }
        let file = read_store_file(&root).unwrap().unwrap();
        assert_eq!(file.secrets, SecretsMode::Locking, "{case}");
    };
    assert!(
        matches!(
            resume(slot(LockSlotKind::Password, PASSWORD).unwrap()),
            Err(CoreError::Cli(CliError::LockIncomplete {
                phase: cli_core::LockPhase::Locking,
                ..
            }))
        ),
        "the lock password twice"
    );
    untouched("the lock password twice");
    assert!(
        matches!(
            resume(foreign_key()),
            Err(CoreError::Cli(CliError::StoreChanged { .. }))
        ),
        "another store's recovery key"
    );
    untouched("another store's recovery key");
    let done = resume(slot(LockSlotKind::RecoveryKey, &shown).unwrap()).unwrap();
    assert!(matches!(done, Outcome::Completed { .. }), "{done:?}");
    assert_locked_whole(&f.ctx, PASSWORD);
    assert_locked_whole(&f.ctx, &shown);
}

/// Critic G13: a run interrupted before its vault starts over — a new id, a new recovery key.
#[test]
fn a_run_without_a_vault_starts_over_with_a_new_store_id() {
    let f = files_store();
    let plan = plan_on(&f.ctx, None).unwrap();
    let (input, _) = new_input();
    execute_on(
        &f.ctx.clone().with_fault(FaultPoint::AfterStoreFileLocking),
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    let first = read_store_file(f.ctx.config_root()).unwrap().unwrap().id;
    let plan = plan_on(&f.ctx, None).unwrap();
    assert_eq!(plan.payload.start(), &LockOnStart::New { restart: true });
    let (input, shown) = new_input();
    let done = execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(matches!(done, Outcome::Completed { result } if result.restarted));
    assert_ne!(
        read_store_file(f.ctx.config_root()).unwrap().unwrap().id,
        first
    );
    assert_locked_whole(&f.ctx, &shown);
}

/// Critic G13: a run that lost the race to another finds the store locked and writes nothing.
#[test]
fn a_run_that_lost_the_race_writes_nothing() {
    let f = files_store();
    let late = plan_on(&f.ctx, None).unwrap();
    lock(&f.ctx);
    let vault = std::fs::read(f.ctx.config_root().join(VAULT_FILE)).unwrap();
    let (input, _) = new_input();
    let done = execute_on(
        &f.ctx,
        late,
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(matches!(done, Outcome::Completed { result } if result.already_locked));
    assert_eq!(
        std::fs::read(f.ctx.config_root().join(VAULT_FILE)).unwrap(),
        vault
    );
}

/// Rule 4: the vault is read back from disk and opened with BOTH slots, and every entry
/// compared, before any clear datum goes. The hook rewrites the run's own vault (same vault
/// id, same data key) right after step 2: once with its recovery slot swapped for another
/// key's, once without `prod`'s token. Either stops the run with every file intact.
#[test]
fn the_vault_is_checked_with_both_slots_before_any_clear_data_goes() {
    for spoil in ["another recovery key", "no token"] {
        let f = files_store();
        let plan = plan_on(&f.ctx, None).unwrap();
        let (input, _) = new_input();
        let rewrite = move |path: &Path| {
            let fast = &KdfPolicy::INSECURE_FAST;
            let mut vault = VaultFile::parse(&std::fs::read(path).unwrap())
                .unwrap()
                .open(&SlotSecret::Password(SecretString::new(PASSWORD)), fast)
                .unwrap();
            if spoil == "another recovery key" {
                let shown = vault
                    .header()
                    .slots
                    .iter()
                    .find(|slot| slot.kind == SlotKind::RecoveryKey)
                    .unwrap()
                    .id;
                vault
                    .add_slot(
                        NewSlot::RecoveryKey {
                            key: RecoveryKey::generate(&mut OsRandom).unwrap(),
                            label: RECOVERY_LABEL.into(),
                        },
                        fast,
                        &mut OsRandom,
                    )
                    .unwrap();
                vault.remove_slot(shown, fast).unwrap();
            } else {
                assert_eq!(vault.payload_mut().remove_target("prod"), 1);
            }
            std::fs::write(path, vault.seal("test", &mut OsRandom).unwrap()).unwrap();
        };
        super::on::AFTER_VAULT_WRITE.with(|h| *h.borrow_mut() = Some(Box::new(rewrite)));
        let e = execute_on(
            &f.ctx,
            plan,
            input,
            &NullReporter,
            &CancellationToken::new(),
        );
        super::on::AFTER_VAULT_WRITE.with(|h| *h.borrow_mut() = None);
        assert!(
            matches!(e, Err(CoreError::Cli(CliError::VaultUnreadable { .. }))),
            "{spoil}: {e:?}"
        );
        for (target, token) in [("prod", TOKEN_A), ("staging", TOKEN_B)] {
            let bytes = std::fs::read(f.ctx.store().target_credentials_file(target)).unwrap();
            assert_eq!(
                classify(&bytes),
                Marker::ClearToken,
                "{spoil}: {target} kept in clear"
            );
            assert!(String::from_utf8_lossy(&bytes).contains(token));
        }
    }
}

/// T36: a cache that stops opening between the plan and the run is left — the person agreed
/// to drop only what the plan listed — and named; a read later says `cache_undecryptable`.
#[test]
fn a_cache_that_stops_opening_after_the_plan_is_left() {
    let f = files_store();
    let plan = plan_on(&f.ctx, None).unwrap();
    let mut prod = hetzner_of(&f.ctx, "prod");
    prod.kubeconfig_age =
        Some(encrypt_for_recipient(YAML, &Identity::generate().to_public()).unwrap());
    seed(&f.ctx, "prod", serde_json::to_value(&prod).unwrap());
    let (input, _) = new_input();
    let done = match execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap()
    {
        Outcome::Completed { result } => result,
        other => panic!("{other:?}"),
    };
    assert_eq!(done.caches_left.len(), 1);
    assert_eq!(done.caches_left[0].target, "prod");
    assert!(hetzner_of(&f.ctx, "prod").kubeconfig_age.is_some());
}

/// T23, critic S2 (L5 in process): a weak policy creates no password slot for a store outside
/// its roots — refused at the plan, before anything is asked.
#[test]
fn a_weak_policy_plans_nothing_outside_its_roots() {
    let (dir, ctx) = store(&["prod"], Some("prod"));
    let weak = ctx.clone().with_kdf_policy(&KdfPolicy::INSECURE_FAST);
    assert!(matches!(
        plan_on(&weak, None).unwrap_err(),
        CoreError::InsecureKdfRefused { root } if root == ctx.config_root()
    ));
    let elsewhere = tempfile::tempdir().unwrap();
    let wrong_root = weak
        .clone()
        .with_insecure_kdf_root(elsewhere.path().to_path_buf());
    assert!(plan_on(&wrong_root, None).is_err());
    assert!(insecure_kdf_permitted(
        &weak.with_insecure_kdf_root(dir.path().to_path_buf())
    ));
    assert!(insecure_kdf_permitted(
        &ctx.with_kdf_policy(&KdfPolicy::RELEASE)
    ));
}

#[test]
fn a_locked_or_unlocking_store_is_not_planned_again() {
    let f = files_store();
    lock(&f.ctx);
    let plan = plan_on(&f.ctx, None).unwrap();
    assert_eq!(plan.payload.start(), &LockOnStart::AlreadyLocked);
    let (input, _) = new_input();
    assert!(matches!(
        execute_on(&f.ctx, plan, input, &NullReporter, &CancellationToken::new()).unwrap(),
        Outcome::Completed { result } if result.already_locked
    ));
    let mut file = read_store_file(f.ctx.config_root()).unwrap().unwrap();
    file.secrets = SecretsMode::Unlocking;
    let held = cli_core::StoreLock::exclusive(&f.ctx.store()).unwrap();
    write_store_file(f.ctx.config_root(), &file, &held).unwrap();
    drop(held);
    assert!(matches!(
        plan_on(&f.ctx, None).unwrap_err(),
        CoreError::Cli(CliError::LockIncomplete {
            phase: cli_core::LockPhase::Unlocking,
            ..
        })
    ));
}

#[test]
fn a_cancelled_run_writes_nothing() {
    let f = files_store();
    let plan = plan_on(&f.ctx, None).unwrap();
    let (input, _) = new_input();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        execute_on(&f.ctx, plan, input, &NullReporter, &cancel).unwrap(),
        Outcome::Cancelled { .. }
    ));
    assert!(read_store_file(f.ctx.config_root()).unwrap().is_none());
    assert!(!f.ctx.config_root().join(VAULT_FILE).exists());
}

/// A reporter that cancels `token` when stage `at` begins: a Ctrl-C at that moment.
pub(super) struct CancelAt {
    pub at: u32,
    pub token: CancellationToken,
}

impl crate::Reporter for CancelAt {
    fn report(&self, event: crate::Event) {
        if matches!(event, crate::Event::Stage { index, .. } if index == self.at) {
            self.token.cancel();
        }
    }
}

/// A cancel before the caches (step 4) undoes what the run wrote: its vault goes, `store.yaml`
/// says files again, and nothing had left the files.
#[test]
fn a_cancel_before_the_caches_undoes_what_the_run_wrote() {
    for (at, wrote_vault) in [(1, false), (2, true)] {
        let f = files_store();
        let plan = plan_on(&f.ctx, None).unwrap();
        let (input, _) = new_input();
        let token = CancellationToken::new();
        let reporter = CancelAt {
            at,
            token: token.clone(),
        };
        let cleaned = match execute_on(&f.ctx, plan, input, &reporter, &token).unwrap() {
            Outcome::Cancelled { cleaned, left } => {
                assert!(left.is_empty(), "{left:?}");
                cleaned
            }
            other => panic!("stage {at}: {other:?}"),
        };
        let vault = f.ctx.config_root().join(VAULT_FILE);
        assert_eq!(
            cleaned.contains(&vault.display().to_string()),
            wrote_vault,
            "stage {at}: {cleaned:?}"
        );
        assert!(!vault.exists(), "stage {at}");
        let file = read_store_file(f.ctx.config_root()).unwrap().unwrap();
        assert_eq!(file.secrets, SecretsMode::Files);
        assert_eq!(
            f.ctx
                .secrets()
                .stored_token("prod")
                .unwrap()
                .unwrap()
                .expose(),
            TOKEN_A
        );
        let prod = hetzner_of(&f.ctx, "prod");
        assert_eq!(
            f.ctx
                .secrets()
                .cached_kubeconfig(&prod, "prod")
                .unwrap()
                .unwrap()
                .as_str(),
            YAML
        );
    }
}

/// T33: the window lives in the vault (store.yaml shows a copy); changing it is a vault write.
#[test]
fn the_window_lives_in_the_vault_and_changes_there() {
    let f = files_store();
    let plan = plan_on(&f.ctx, Some(15)).unwrap();
    let (input, shown) = new_input();
    execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap();
    let reader = opened(&f.ctx, &shown);
    let id = read_store_file(f.ctx.config_root())
        .unwrap()
        .unwrap()
        .id
        .unwrap();
    let o = open_at(
        f.ctx.config_root(),
        &id,
        SlotInput::from_input(SecretString::new(shown.as_str())),
        &KdfPolicy::INSECURE_FAST,
    )
    .unwrap();
    assert_eq!(o.unlock_timeout_min, 15);
    assert_eq!(set_unlock_timeout(&reader, 30, &NullReporter).unwrap(), 30);
    let o = open_at(
        f.ctx.config_root(),
        &id,
        SlotInput::from_input(SecretString::new(shown.as_str())),
        &KdfPolicy::INSECURE_FAST,
    )
    .unwrap();
    assert_eq!(o.unlock_timeout_min, 30);
    assert_eq!(
        read_store_file(f.ctx.config_root())
            .unwrap()
            .unwrap()
            .unlock_timeout_min,
        Some(30)
    );
    assert!(matches!(
        set_unlock_timeout(&reader, 1441, &NullReporter).unwrap_err(),
        CoreError::Cli(CliError::UsageRefused { .. })
    ));
}

/// d4a-h F1: `lock on` over a store another account can change is refused when it is planned,
/// before the CLI asks for a new lock password or shows a recovery key it could never write;
/// on a locked store too, where `--timeout` would change `store.yaml`.
#[test]
fn a_store_another_account_can_change_is_refused_before_anything_is_asked() {
    let f = files_store();
    let staging = f.ctx.store().target_dir("staging");
    {
        let _foreign = cli_core::owner_only::pretend::foreign_owner(&staging);
        for timeout in [None, Some(15)] {
            match plan_on(&f.ctx, timeout) {
                Err(CoreError::Cli(CliError::StoreNotPrivate { path, .. })) => {
                    assert_eq!(path, staging)
                }
                other => panic!("{timeout:?}: {:?}", other.map(|p| p.title)),
            }
        }
    }
    lock(&f.ctx);
    let _foreign = cli_core::owner_only::pretend::foreign_owner(&staging);
    match plan_on(&f.ctx, Some(15)) {
        Err(CoreError::Cli(CliError::StoreNotPrivate { path, .. })) => assert_eq!(path, staging),
        other => panic!("{:?}", other.map(|p| p.title)),
    }
}

/// WI-475: `lock on` over a store another account can change is refused before anything is
/// written — no vault, no `store.yaml`, every clear file as it was — naming the folder.
#[test]
fn a_store_another_account_can_change_is_not_locked() {
    let f = files_store();
    let plan = plan_on(&f.ctx, None).unwrap();
    let before = crate::target::testkit::snapshot(f.ctx.config_root());
    let staging = f.ctx.store().target_dir("staging");
    let _foreign = cli_core::owner_only::pretend::foreign_owner(&staging);
    let (input, _) = new_input();
    match execute_on(
        &f.ctx,
        plan,
        input,
        &NullReporter,
        &CancellationToken::new(),
    ) {
        Err(CoreError::Cli(CliError::StoreNotPrivate { path, .. })) => assert_eq!(path, staging),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        crate::target::testkit::snapshot(f.ctx.config_root()),
        before
    );
}
