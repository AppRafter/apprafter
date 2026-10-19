// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock off` (D.4a T37, §3.9; rule 4) on scratch stores `lock on` locked, under the fast KDF
//! policy of `on_tests`' fixtures: every interruption loses nothing and a run again finishes,
//! an interrupted `lock on` is backed out, and no clear token is ever overwritten.

use cli_core::secrets::{encrypt_for_recipient, CachedSecret, Identity};
use cli_core::target::{save_target_credentials, TargetCredentials};
use cli_core::{CliError, LockPhase, StoreLock, VaultProblem};

use super::on_tests::{
    files_store, hetzner_of, lock, new_input, opened, seed, token_of, CancelAt, Files, PASSWORD,
    PW, YAML,
};
use super::*;
use crate::secrets::marker::{classify, write_marker, Marker};
use crate::secrets::{read_store_file, LockSlotKind, SecretsMode};
use crate::target::testkit::{TOKEN_A, TOKEN_B};
use crate::vault::{KdfPolicy, SlotInput, VAULT_FILE};
use crate::{
    CancellationToken, ChangeAction, Context, CoreError, NullReporter, Outcome, PlanClass,
    SecretString,
};

const TOKEN_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

/// `ctx`'s vault opened with `secret`, as the CLI opens it for `lock off`.
fn open_with(ctx: &Context, secret: &str) -> OpenedVault {
    let file = read_store_file(ctx.config_root()).unwrap().unwrap();
    open_at(
        ctx.config_root(),
        file.id.as_ref().unwrap(),
        SlotInput::from_input(SecretString::new(secret), None),
        &KdfPolicy::INSECURE_FAST,
    )
    .unwrap()
}

/// `lock off` run to its end, with `secret` when the plan needs one.
fn unlock(ctx: &Context, secret: &str) -> LockedOff {
    let plan = plan_off(ctx).unwrap();
    let opened = plan.payload.needs_secret().then(|| open_with(ctx, secret));
    match execute_off(ctx, plan, opened, &NullReporter, &CancellationToken::new()).unwrap() {
        Outcome::Completed { result } => result,
        other => panic!("{other:?}"),
    }
}

/// The store as a whole `lock off` leaves it: in files, every token in clear, the caches
/// under the shared key at `ctx.age_key_path()`, no vault.
fn assert_files_whole(ctx: &Context) {
    assert!(!ctx.config_root().join(VAULT_FILE).exists());
    let s = ctx.secrets();
    assert_eq!(s.mode().unwrap(), SecretsMode::Files);
    for (target, token) in [("prod", TOKEN_A), ("staging", TOKEN_B)] {
        let bytes = std::fs::read(ctx.store().target_credentials_file(target)).unwrap();
        assert_eq!(classify(&bytes), Marker::ClearToken, "{target}");
        assert_eq!(s.stored_token(target).unwrap().unwrap().expose(), token);
    }
    let prod = hetzner_of(ctx, "prod");
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
    assert_eq!(
        s.cached_kubeconfig(&staging, "staging")
            .unwrap()
            .unwrap()
            .as_str(),
        YAML
    );
    // F13: under the shared key, which records nothing. staging's password, which no key
    // here opens, is either gone with its record or left with it (a back-out).
    assert_eq!(
        (
            &prod.kubeconfig_sealed_to,
            &prod.argocd_admin_password_sealed_to,
            &staging.kubeconfig_sealed_to
        ),
        (&None, &None, &None)
    );
    if staging.argocd_admin_password_age.is_none() {
        assert_eq!(staging.argocd_admin_password_sealed_to, None);
    }
}

#[test]
fn lock_off_puts_every_secret_back_in_files_and_drops_the_vault() {
    let f = files_store();
    let shared = std::fs::read(f.ctx.age_key_path()).unwrap();
    lock(&f.ctx);
    let done = unlock(&f.ctx, PASSWORD);
    assert_eq!(
        done.tokens_restored,
        vec!["prod".to_string(), "staging".to_string()]
    );
    assert!(
        done.tokens_kept.is_empty() && done.caches_dropped.is_empty(),
        "{done:?}"
    );
    assert_eq!(done.caches_resealed, 3);
    assert!(!done.age_key_created && !done.not_locked && !done.backed_out);
    assert_files_whole(&f.ctx);
    assert_eq!(
        std::fs::read(f.ctx.age_key_path()).unwrap(),
        shared,
        "the shared key is used, never replaced"
    );
}

/// T37: the plan needs no secret (the context here cannot open the vault), writes nothing, and
/// lists each move.
#[test]
fn the_plan_needs_no_secret_and_lists_each_move() {
    let f = files_store();
    lock(&f.ctx);
    let vault = std::fs::read(f.ctx.config_root().join(VAULT_FILE)).unwrap();
    let plan = plan_off(&f.ctx).unwrap();
    assert_eq!(plan.class, PlanClass::Destructive);
    assert!(plan.payload.needs_secret());
    assert_eq!(
        plan.payload.store_id(),
        read_store_file(f.ctx.config_root())
            .unwrap()
            .unwrap()
            .id
            .as_ref()
    );
    assert!(matches!(
        plan.payload.start(),
        LockOffStart::Unlock {
            from: SecretsMode::Locked,
            ..
        }
    ));
    assert_eq!(plan.payload.dest(), f.ctx.age_key_path());
    assert!(plan.payload.dest_exists());
    let lines: Vec<(String, ChangeAction)> = plan
        .changes
        .iter()
        .map(|c| (format!("{} {}", c.kind, c.object), c.action))
        .collect();
    for want in [
        ("Credentials prod".to_string(), ChangeAction::Move),
        ("Credentials staging".to_string(), ChangeAction::Move),
        ("Cache prod".to_string(), ChangeAction::Update),
        ("Cache staging".to_string(), ChangeAction::Update),
        (
            format!("AgeKey {}", f.ctx.age_key_path().display()),
            ChangeAction::Keep,
        ),
        (
            format!("Vault {}", f.ctx.config_root().join(VAULT_FILE).display()),
            ChangeAction::Delete,
        ),
    ] {
        assert!(lines.contains(&want), "{want:?} in {lines:?}");
    }
    assert_eq!(
        std::fs::read(f.ctx.config_root().join(VAULT_FILE)).unwrap(),
        vault
    );
    assert_eq!(f.ctx.secrets().mode().unwrap(), SecretsMode::Locked);
}

/// Rule 4 and T37: an interruption at every step loses no token, readers in between refuse,
/// and a run again finishes — with the secret while the vault is there, without it after.
#[test]
fn an_interruption_at_every_step_loses_nothing_and_a_rerun_finishes() {
    for at in [
        FaultPoint::AfterStoreFileUnlocking,
        FaultPoint::AfterSharedKey,
        FaultPoint::AfterFilesTarget(0),
        FaultPoint::AfterFilesTarget(1),
        FaultPoint::AfterFilesVerified,
        FaultPoint::AfterVaultDeleted,
        FaultPoint::AfterFiles,
    ] {
        let f = files_store();
        let shown = lock(&f.ctx);
        let plan = plan_off(&f.ctx).unwrap();
        let opened = Some(open_with(&f.ctx, &shown));
        let e = execute_off(
            &f.ctx.clone().with_fault(at),
            plan,
            opened,
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("injected fault"), "{at:?}: {e}");
        for (target, token) in [("prod", TOKEN_A), ("staging", TOKEN_B)] {
            assert_eq!(token_of(&f.ctx, &shown, target), token, "{at:?}: {target}");
        }
        if at != FaultPoint::AfterFiles {
            assert!(
                matches!(
                    f.ctx.secrets().stored_token("prod").unwrap_err(),
                    CliError::LockIncomplete {
                        phase: LockPhase::Unlocking,
                        ..
                    }
                ),
                "{at:?}: readers refuse"
            );
        }
        let start = plan_off(&f.ctx).unwrap().payload.start().clone();
        match at {
            FaultPoint::AfterVaultDeleted => {
                assert!(
                    matches!(start, LockOffStart::FinishNoVault { .. }),
                    "{start:?}"
                )
            }
            FaultPoint::AfterFiles => assert_eq!(start, LockOffStart::NotLocked),
            _ => assert!(
                matches!(
                    start,
                    LockOffStart::Unlock {
                        from: SecretsMode::Unlocking,
                        ..
                    }
                ),
                "{at:?}: {start:?}"
            ),
        }
        unlock(&f.ctx, &shown);
        assert_files_whole(&f.ctx);
    }
}

/// Critic G5: `lock off` backs out of a `lock on` interrupted at any of its seven `locking`
/// points — without the secret when no vault was written, with it after — and never drops a
/// cache `lock on` had not touched (critic finding 10).
#[test]
fn lock_off_backs_out_of_an_interrupted_lock_on() {
    for at in [
        FaultPoint::AfterStoreFileLocking,
        FaultPoint::AfterVaultWritten,
        FaultPoint::AfterVaultVerified,
        FaultPoint::AfterCachesTarget(0),
        FaultPoint::AfterCachesTarget(1),
        FaultPoint::AfterMarkerTarget(0),
        FaultPoint::AfterMarkerTarget(1),
    ] {
        let f = files_store();
        let plan = plan_on(&f.ctx, None).unwrap();
        let (input, shown) = new_input();
        execute_on(
            &f.ctx.clone().with_fault(at),
            plan,
            input,
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap_err();
        let start = plan_off(&f.ctx).unwrap().payload.start().clone();
        if at == FaultPoint::AfterStoreFileLocking {
            assert_eq!(start, LockOffStart::BackOutNoVault);
        } else {
            assert!(
                matches!(
                    start,
                    LockOffStart::Unlock {
                        from: SecretsMode::Locking,
                        ..
                    }
                ),
                "{at:?}: {start:?}"
            );
        }
        let done = unlock(&f.ctx, &shown);
        assert!(done.backed_out, "{at:?}");
        assert!(
            done.caches_dropped.is_empty(),
            "{at:?}: a back-out drops nothing: {done:?}"
        );
        assert_files_whole(&f.ctx);
        // staging's password cache, sealed to a key no run reads, was dropped only where
        // `lock on` itself re-sealed staging (target 1) — its plan listed it; before that the
        // back-out leaves it as it was, and says so.
        let dropped_by_lock_on = matches!(
            at,
            FaultPoint::AfterCachesTarget(1)
                | FaultPoint::AfterMarkerTarget(0)
                | FaultPoint::AfterMarkerTarget(1)
        );
        assert_eq!(
            hetzner_of(&f.ctx, "staging")
                .argocd_admin_password_age
                .is_none(),
            dropped_by_lock_on,
            "{at:?}"
        );
        // F13: a cache the back-out leaves keeps its record; a dropped one lost it.
        assert_eq!(
            hetzner_of(&f.ctx, "staging").argocd_admin_password_sealed_to,
            (!dropped_by_lock_on).then(|| cli_state::SealedTo::StoreVault {
                store_id: "0123456789abcdef".into()
            }),
            "{at:?}"
        );
        let left: Vec<(&str, &str)> = done
            .caches_left
            .iter()
            .map(|c| (c.target.as_str(), c.secret.as_str()))
            .collect();
        let vault_written = at != FaultPoint::AfterStoreFileLocking;
        assert_eq!(
            left,
            if vault_written && !dropped_by_lock_on {
                vec![("staging", "argocd_password")]
            } else {
                Vec::new()
            },
            "{at:?}"
        );
    }
}

/// T37, T42: on a locked store a clear token in a credentials file stops the plan — it is
/// renewed into the vault or removed first, never adopted and never silently dropped.
#[test]
fn a_clear_token_on_a_locked_store_stops_the_plan() {
    let f = files_store();
    lock(&f.ctx);
    save_target_credentials(
        &f.ctx.store(),
        "staging",
        &TargetCredentials {
            hetzner_token: Some(TOKEN_C.into()),
        },
    )
    .unwrap();
    match plan_off(&f.ctx) {
        Err(CoreError::Cli(CliError::TokenOutsideVault { target, .. })) => {
            assert_eq!(target, "staging")
        }
        other => panic!("{other:?}"),
    }
}

/// Critic G2's mirror rule: a resume never overwrites a clear token that differs from the
/// vault's (written while the run was interrupted, the person's newer intent): kept, named.
#[test]
fn a_resume_never_overwrites_a_clear_token_it_did_not_put() {
    let f = files_store();
    let shown = lock(&f.ctx);
    let plan = plan_off(&f.ctx).unwrap();
    execute_off(
        &f.ctx
            .clone()
            .with_fault(FaultPoint::AfterStoreFileUnlocking),
        plan,
        Some(open_with(&f.ctx, &shown)),
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    save_target_credentials(
        &f.ctx.store(),
        "staging",
        &TargetCredentials {
            hetzner_token: Some(TOKEN_C.into()),
        },
    )
    .unwrap();
    let done = unlock(&f.ctx, &shown);
    assert_eq!(done.tokens_kept, vec!["staging".to_string()]);
    assert_eq!(done.tokens_restored, vec!["prod".to_string()]);
    let s = f.ctx.secrets();
    assert_eq!(
        s.stored_token("staging").unwrap().unwrap().expose(),
        TOKEN_C
    );
    assert_eq!(s.stored_token("prod").unwrap().unwrap().expose(), TOKEN_A);
    assert!(!f.ctx.config_root().join(VAULT_FILE).exists());
}

/// T43: a marker whose vault holds no token stops the run before anything is written.
#[test]
fn a_marker_without_its_token_in_the_vault_stops_lock_off_before_any_write() {
    let f = files_store();
    let shown = lock(&f.ctx);
    let reader = opened(&f.ctx, &shown);
    let s = reader.secrets();
    let key = s.prepare_write().unwrap();
    let held = StoreLock::exclusive(&reader.store()).unwrap();
    assert!(s.remove_token(&key, &held, "prod").unwrap());
    drop(held);
    let plan = plan_off(&f.ctx).unwrap();
    let opened = Some(open_with(&f.ctx, &shown));
    match execute_off(
        &f.ctx,
        plan,
        opened,
        &NullReporter,
        &CancellationToken::new(),
    ) {
        Err(CoreError::Cli(CliError::TokenMissingFromVault { target, .. })) => {
            assert_eq!(target, "prod")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(f.ctx.secrets().mode().unwrap(), SecretsMode::Locked);
    assert!(f.ctx.config_root().join(VAULT_FILE).exists());
    let bytes = std::fs::read(f.ctx.store().target_credentials_file("staging")).unwrap();
    assert!(matches!(classify(&bytes), Marker::Vault { .. }));
}

/// O13: the caches go to the shared key this context names (`APPRAFTER_AGE_KEY`, else the
/// default), created when there is none; the key elsewhere is left as it was.
#[test]
fn lock_off_seals_the_caches_to_the_key_this_context_names() {
    let f = files_store();
    let first = std::fs::read(f.ctx.age_key_path()).unwrap();
    lock(&f.ctx);
    let elsewhere = f
        .ctx
        .clone()
        .with_home_dir(Some(f.dir.path().join("home2")));
    assert!(!elsewhere.age_key_path().exists());
    let plan = plan_off(&elsewhere).unwrap();
    assert!(!plan.payload.dest_exists());
    assert!(plan.changes.iter().any(|c| c.kind == "AgeKey"
        && c.object == elsewhere.age_key_path().display().to_string()
        && c.action == ChangeAction::Create));
    let done = unlock(&elsewhere, PASSWORD);
    assert!(done.age_key_created);
    assert_eq!(
        done.age_key,
        Some(elsewhere.age_key_path().display().to_string())
    );
    assert_files_whole(&elsewhere);
    assert_eq!(std::fs::read(f.ctx.age_key_path()).unwrap(), first);
    let prod = hetzner_of(&f.ctx, "prod");
    assert!(matches!(
        f.ctx
            .secrets()
            .cached_kubeconfig(&prod, "prod")
            .unwrap_err(),
        CliError::CacheUndecryptable { .. }
    ));
}

/// A run that lost the race to another finds the store in files and writes nothing.
#[test]
fn a_run_that_lost_the_race_writes_nothing() {
    let f = files_store();
    let shown = lock(&f.ctx);
    let late = plan_off(&f.ctx).unwrap();
    let late_key = Some(open_with(&f.ctx, &shown));
    unlock(&f.ctx, &shown);
    let before = std::fs::read(f.ctx.store().target_credentials_file("prod")).unwrap();
    match execute_off(
        &f.ctx,
        late,
        late_key,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap()
    {
        Outcome::Completed { result } => assert!(result.not_locked, "{result:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        std::fs::read(f.ctx.store().target_credentials_file("prod")).unwrap(),
        before
    );
    assert_files_whole(&f.ctx);
}

/// Rule 4: every token written is read back before the vault goes; a file that does not
/// hold it stops the run with the vault kept.
#[test]
fn the_files_are_read_back_before_the_vault_goes() {
    let f = files_store();
    lock(&f.ctx);
    let plan = plan_off(&f.ctx).unwrap();
    let opened = Some(open_with(&f.ctx, PASSWORD));
    super::off::AFTER_FILES_WRITE.with(|h| {
        *h.borrow_mut() = Some(Box::new(|store: &cli_core::TargetStorePaths| {
            save_target_credentials(
                store,
                "prod",
                &TargetCredentials {
                    hetzner_token: Some(TOKEN_C.into()),
                },
            )
            .unwrap();
        }))
    });
    let e = execute_off(
        &f.ctx,
        plan,
        opened,
        &NullReporter,
        &CancellationToken::new(),
    );
    super::off::AFTER_FILES_WRITE.with(|h| *h.borrow_mut() = None);
    let e = e.unwrap_err();
    assert!(e.to_string().contains("the vault at"), "{e}");
    // d's hand-off: its own code, never the catch-all whose help says to file an issue.
    assert!(
        matches!(&e, CoreError::Cli(CliError::FilesNotKept { what, .. }) if what.contains("`prod`")),
        "{e:?}"
    );
    assert_eq!(
        crate::error::UiError::from(&e).code.as_deref(),
        Some(crate::error::codes::FILES_NOT_KEPT)
    );
    assert!(f.ctx.config_root().join(VAULT_FILE).exists());
}

#[test]
fn in_files_lock_off_has_nothing_to_do() {
    let f = files_store();
    let plan = plan_off(&f.ctx).unwrap();
    assert_eq!(plan.payload.start(), &LockOffStart::NotLocked);
    assert!(!plan.payload.needs_secret() && plan.changes.is_empty());
    match execute_off(&f.ctx, plan, None, &NullReporter, &CancellationToken::new()).unwrap() {
        Outcome::Completed { result } => assert!(result.not_locked),
        other => panic!("{other:?}"),
    }
    assert!(read_store_file(f.ctx.config_root()).unwrap().is_none());
}

/// An interrupted `lock off` whose vault someone deleted while a marker still names it: the
/// plan stops (the token is gone with the vault) instead of calling the store files.
#[test]
fn a_vault_deleted_by_hand_stops_the_finish() {
    let f = files_store();
    let shown = lock(&f.ctx);
    let plan = plan_off(&f.ctx).unwrap();
    execute_off(
        &f.ctx
            .clone()
            .with_fault(FaultPoint::AfterStoreFileUnlocking),
        plan,
        Some(open_with(&f.ctx, &shown)),
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    std::fs::remove_file(f.ctx.config_root().join(VAULT_FILE)).unwrap();
    assert!(matches!(
        plan_off(&f.ctx).unwrap_err(),
        CoreError::Cli(CliError::VaultUnreadable {
            problem: VaultProblem::Missing,
            ..
        })
    ));
}

/// A cancel before any file changed puts `store.yaml` back: the store is locked as before.
#[test]
fn a_cancel_before_the_files_leaves_the_store_locked() {
    let f = files_store();
    let shown = lock(&f.ctx);
    let plan = plan_off(&f.ctx).unwrap();
    let token = CancellationToken::new();
    let reporter = CancelAt {
        at: 1,
        token: token.clone(),
    };
    match execute_off(
        &f.ctx,
        plan,
        Some(open_with(&f.ctx, &shown)),
        &reporter,
        &token,
    )
    .unwrap()
    {
        Outcome::Cancelled { left, .. } => assert!(left.is_empty(), "{left:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(f.ctx.secrets().mode().unwrap(), SecretsMode::Locked);
    assert!(f.ctx.config_root().join(VAULT_FILE).exists());
    for target in ["prod", "staging"] {
        let bytes = std::fs::read(f.ctx.store().target_credentials_file(target)).unwrap();
        assert!(matches!(classify(&bytes), Marker::Vault { .. }), "{target}");
    }
}

/// T37, critic finding 10: on a locked store a cache neither the vault's key nor the
/// destination opens is dropped — and the plan named that loss before the person agreed; a
/// damaged one is said to be damaged, never "no key opens it" (rule 3).
#[test]
fn an_unlock_drops_a_cache_no_key_opens_and_the_plan_names_it() {
    let f = files_store();
    let shown = lock(&f.ctx);
    let mut prod = hetzner_of(&f.ctx, "prod");
    prod.argocd_admin_password_age =
        Some(encrypt_for_recipient(PW, &Identity::generate().to_public()).unwrap());
    seed(&f.ctx, "prod", serde_json::to_value(&prod).unwrap());
    // staging's kubeconfig, sealed to the store's own key, its payload broken: the key it
    // names does not decrypt it.
    let mut staging = hetzner_of(&f.ctx, "staging");
    let recipient = opened(&f.ctx, &shown)
        .secrets()
        .cache_key("staging")
        .unwrap()
        .recipient()
        .unwrap();
    let sealed = encrypt_for_recipient(YAML, &recipient).unwrap();
    let mut lines: Vec<String> = sealed.lines().map(str::to_string).collect();
    let last = lines.len() - 2; // the payload's last base64 line, before the END line
    let first = if lines[last].starts_with('A') {
        "B"
    } else {
        "A"
    };
    lines[last].replace_range(0..1, first);
    staging.kubeconfig_age = Some(format!("{}\n", lines.join("\n")));
    seed(&f.ctx, "staging", serde_json::to_value(&staging).unwrap());
    let plan = plan_off(&f.ctx).unwrap();
    let named = |target: &str, noun: &str| {
        plan.changes.iter().any(|c| {
            c.kind == "Cache"
                && c.object == target
                && c.detail.as_deref().is_some_and(|d| {
                    d.contains(noun) && d.contains("dropped if the vault's age key cannot open it")
                })
        })
    };
    assert!(
        named("prod", "Argo CD admin password"),
        "{:?}",
        plan.changes
    );
    assert!(named("staging", "kubeconfig"), "{:?}", plan.changes);
    let done = unlock(&f.ctx, PASSWORD);
    let dropped: Vec<(&str, &str, bool)> = done
        .caches_dropped
        .iter()
        .map(|c| {
            (
                c.target.as_str(),
                c.secret.as_str(),
                c.why.contains("damaged"),
            )
        })
        .collect();
    assert_eq!(
        dropped,
        vec![
            ("prod", "argocd_password", false),
            ("staging", "kubeconfig", true)
        ],
        "{done:?}"
    );
    assert!(done.caches_left.is_empty(), "{done:?}");
    assert!(hetzner_of(&f.ctx, "prod")
        .argocd_admin_password_age
        .is_none());
    assert!(hetzner_of(&f.ctx, "staging").kubeconfig_age.is_none());
    // F13: a dropped cache takes its record with it.
    assert_eq!(
        hetzner_of(&f.ctx, "prod").argocd_admin_password_sealed_to,
        None
    );
    assert_eq!(hetzner_of(&f.ctx, "staging").kubeconfig_sealed_to, None);
}

/// F13: a cache under the shared key already — an older `lock off` re-sealed it and was
/// interrupted, or another program did — is skipped, and its record of the vault goes: the
/// shared key records nothing.
#[test]
fn a_cache_under_the_shared_key_already_loses_its_vault_record() {
    let f = files_store();
    lock(&f.ctx);
    let shared = cli_core::secrets::load_or_create_identity(f.ctx.age_key_path()).unwrap();
    let mut prod = hetzner_of(&f.ctx, "prod");
    assert!(prod.kubeconfig_sealed_to.is_some());
    prod.kubeconfig_age = Some(encrypt_for_recipient(YAML, &shared.to_public()).unwrap());
    seed(&f.ctx, "prod", serde_json::to_value(&prod).unwrap());
    unlock(&f.ctx, PASSWORD);
    assert_files_whole(&f.ctx);
}

/// Critic finding 16 (b): a `lock on` interrupted before its vault, whose store holds a marker
/// of its id (the vault deleted by hand after step 5), is not called files — the token is gone
/// with the vault; the plan and the run both stop (`vault_gone`).
#[test]
fn a_back_out_with_a_marker_and_no_vault_stops() {
    let f = files_store();
    let (input, _) = new_input();
    execute_on(
        &f.ctx.clone().with_fault(FaultPoint::AfterMarkerTarget(0)),
        plan_on(&f.ctx, None).unwrap(),
        input,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    std::fs::remove_file(f.ctx.config_root().join(VAULT_FILE)).unwrap();
    let gone = |e: CoreError| {
        assert!(
            matches!(
                &e,
                CoreError::Cli(CliError::VaultUnreadable {
                    problem: VaultProblem::Missing,
                    ..
                })
            ),
            "{e:?}"
        )
    };
    gone(plan_off(&f.ctx).unwrap_err());
    // The run re-reads under the lock: a marker written after a clean plan stops it too.
    let f = files_store();
    execute_on(
        &f.ctx.clone().with_fault(FaultPoint::AfterStoreFileLocking),
        plan_on(&f.ctx, None).unwrap(),
        new_input().0,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    let plan = plan_off(&f.ctx).unwrap();
    assert_eq!(plan.payload.start(), &LockOffStart::BackOutNoVault);
    let id = read_store_file(f.ctx.config_root())
        .unwrap()
        .unwrap()
        .id
        .unwrap();
    write_marker(&f.ctx.store(), "prod", &id).unwrap();
    gone(execute_off(&f.ctx, plan, None, &NullReporter, &CancellationToken::new()).unwrap_err());
    assert_eq!(
        read_store_file(f.ctx.config_root())
            .unwrap()
            .unwrap()
            .secrets,
        SecretsMode::Locking,
        "nothing written"
    );
}

/// A store in files with two targets' tokens (`prod` TOKEN_A, `staging` TOKEN_B) and no cache
/// at all: no state file. The context runs the flows under the fast policy.
fn bare_store() -> Files {
    let (dir, ctx) = crate::target::testkit::store(&["prod", "staging"], Some("prod"));
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
    Files { dir, ctx }
}

/// What no AppRafter parses as an age key: a flow that reads the file fails on it, so a
/// round trip that leaves it in place and succeeds read nothing (a read-counter the CLI's
/// flows cannot get past).
const NOT_A_KEY: &[u8] = b"not an age key\n";

fn plant_unreadable_key(ctx: &Context) {
    let path = ctx.age_key_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, NOT_A_KEY).unwrap();
}

/// D.4a f Task 3 (d4a-h's critic, blocker 1): a store with no cache moves none, so `lock on`
/// and `lock off` neither create nor read the shared age key, and neither plan names it:
/// absent, it stays absent; unreadable, it is never read.
#[test]
fn a_store_with_no_cache_never_reads_or_creates_the_shared_key() {
    let f = bare_store();
    assert!(!f.ctx.age_key_path().exists());
    let plan = plan_on(&f.ctx, None).unwrap();
    assert!(plan.payload.keys().is_empty(), "{:?}", plan.payload.keys());
    assert!(
        plan.changes
            .iter()
            .all(|c| !c.detail.as_deref().unwrap_or("").contains("age.key")),
        "{:?}",
        plan.changes
    );
    lock(&f.ctx);
    assert!(
        !f.ctx.age_key_path().exists(),
        "lock on created the shared key"
    );
    let plan = plan_off(&f.ctx).unwrap();
    assert!(!plan.payload.needs_key());
    assert!(
        plan.changes.iter().all(|c| c.kind != "AgeKey"),
        "{:?}",
        plan.changes
    );
    let done = unlock(&f.ctx, PASSWORD);
    assert!(
        !f.ctx.age_key_path().exists(),
        "lock off created the shared key"
    );
    assert_eq!(
        (done.age_key.as_deref(), done.age_key_created),
        (None, false)
    );
    assert_eq!(
        done.tokens_restored,
        vec!["prod".to_string(), "staging".to_string()]
    );
    // Unreadable: every read of it would fail the flow.
    let f = bare_store();
    plant_unreadable_key(&f.ctx);
    lock(&f.ctx);
    unlock(&f.ctx, PASSWORD);
    assert_eq!(std::fs::read(f.ctx.age_key_path()).unwrap(), NOT_A_KEY);
    assert_eq!(f.ctx.secrets().mode().unwrap(), SecretsMode::Files);
}

/// D.4a f Task 3, rule 4: on a store with no cache, no interruption of `lock on` or of
/// `lock off`, and no run that backs the one out or finishes the other, reads or creates the
/// shared key; every token is where a whole run leaves it.
#[test]
fn no_interrupted_run_of_a_store_with_no_cache_touches_the_shared_key() {
    for at in [
        FaultPoint::AfterStoreFileLocking,
        FaultPoint::AfterVaultWritten,
        FaultPoint::AfterVaultVerified,
        FaultPoint::AfterMarkerTarget(0),
    ] {
        let f = bare_store();
        plant_unreadable_key(&f.ctx);
        let plan = plan_on(&f.ctx, None).unwrap();
        let (input, shown) = new_input();
        execute_on(
            &f.ctx.clone().with_fault(at),
            plan,
            input,
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap_err();
        let done = unlock(&f.ctx, &shown);
        assert!(done.backed_out, "{at:?}");
        assert_eq!(
            std::fs::read(f.ctx.age_key_path()).unwrap(),
            NOT_A_KEY,
            "{at:?}"
        );
    }
    for at in [
        FaultPoint::AfterStoreFileUnlocking,
        FaultPoint::AfterSharedKey,
        FaultPoint::AfterFilesTarget(0),
        FaultPoint::AfterFilesVerified,
        FaultPoint::AfterVaultDeleted,
    ] {
        let f = bare_store();
        lock(&f.ctx);
        let plan = plan_off(&f.ctx).unwrap();
        let opened = open_with(&f.ctx, PASSWORD);
        execute_off(
            &f.ctx.clone().with_fault(at),
            plan,
            Some(opened),
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap_err();
        unlock(&f.ctx, PASSWORD);
        assert!(
            !f.ctx.age_key_path().exists(),
            "{at:?}: the shared key was created"
        );
        for (target, token) in [("prod", TOKEN_A), ("staging", TOKEN_B)] {
            assert_eq!(
                f.ctx
                    .secrets()
                    .stored_token(target)
                    .unwrap()
                    .unwrap()
                    .expose(),
                token,
                "{at:?}"
            );
        }
    }
}

/// D.4a f Task 3: a resume of `lock on` reads the shared key only for a cache still sealed to
/// it. Interrupted once every cache was re-sealed to the store's key, it finishes with the
/// shared key unreadable — it never reads it; interrupted before, it still needs it (and
/// reads it: the unreadable file stops it, nothing lost).
#[test]
fn a_resume_reads_the_shared_key_only_for_a_cache_still_sealed_to_it() {
    for (at, reads) in [
        (FaultPoint::AfterCachesTarget(0), true),
        (FaultPoint::AfterCachesTarget(1), false),
        (FaultPoint::AfterMarkerTarget(0), false),
    ] {
        let f = files_store();
        let plan = plan_on(&f.ctx, None).unwrap();
        let (input, shown) = new_input();
        execute_on(
            &f.ctx.clone().with_fault(at),
            plan,
            input,
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap_err();
        std::fs::write(f.ctx.age_key_path(), NOT_A_KEY).unwrap();
        let plan = plan_on(&f.ctx, None).unwrap();
        let LockOnStart::Resume { id } = plan.payload.start().clone() else {
            panic!("{at:?}: {:?}", plan.payload.start())
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
        let run = execute_on(
            &f.ctx,
            plan,
            LockOnInput::Resume {
                password: slot(LockSlotKind::Password, PASSWORD),
                recovery: slot(LockSlotKind::RecoveryKey, shown.as_str()),
            },
            &NullReporter,
            &CancellationToken::new(),
        );
        assert_eq!(run.is_err(), reads, "{at:?}: {run:?}");
        assert_eq!(
            std::fs::read(f.ctx.age_key_path()).unwrap(),
            NOT_A_KEY,
            "{at:?}"
        );
        let mode = f.ctx.secrets().mode().unwrap();
        assert_eq!(
            mode,
            if reads {
                SecretsMode::Locking
            } else {
                SecretsMode::Locked
            },
            "{at:?}"
        );
    }
}

/// d4a-h F1: `lock off` on a locked store another account can change is refused when it is
/// planned, before the lock password is asked; a store in files is still only "not locked".
#[test]
fn a_store_another_account_can_change_is_refused_before_its_secret_is_asked() {
    let f = files_store();
    let state = f.ctx.config_root().join("state");
    {
        let _foreign = cli_core::owner_only::pretend::foreign_owner(&state);
        let plan = plan_off(&f.ctx).unwrap();
        assert!(matches!(plan.payload.start(), LockOffStart::NotLocked));
    }
    lock(&f.ctx);
    let _foreign = cli_core::owner_only::pretend::foreign_owner(&state);
    match plan_off(&f.ctx) {
        Err(CoreError::Cli(CliError::StoreNotPrivate { path, .. })) => assert_eq!(path, state),
        other => panic!("{:?}", other.map(|p| p.title)),
    }
}

/// WI-475: `lock off` on a store another account can change is refused before anything is
/// written: every token stays in the vault, and the vault stays.
#[test]
fn a_store_another_account_can_change_is_not_unlocked() {
    let f = files_store();
    lock(&f.ctx);
    let plan = plan_off(&f.ctx).unwrap();
    let opened = plan
        .payload
        .needs_secret()
        .then(|| open_with(&f.ctx, PASSWORD));
    let before = crate::target::testkit::snapshot(f.ctx.config_root());
    let state = f.ctx.config_root().join("state");
    let _foreign = cli_core::owner_only::pretend::foreign_owner(&state);
    match execute_off(
        &f.ctx,
        plan,
        opened,
        &NullReporter,
        &CancellationToken::new(),
    ) {
        Err(CoreError::Cli(CliError::StoreNotPrivate { path, .. })) => assert_eq!(path, state),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        crate::target::testkit::snapshot(f.ctx.config_root()),
        before
    );
}

/// WI-487: `lock off` re-seals each target's caches through `State::update` — its state read
/// again while the run holds the store lock — and keeps every field a re-seal does not own.
#[test]
fn each_reseal_back_reads_the_state_again_under_the_held_lock_and_keeps_the_rest() {
    let f = files_store();
    lock(&f.ctx);
    let paths = cli_state::StatePaths::for_active_target(&f.ctx.store(), "prod");
    let mut state = cli_state::State::load_or_default(&paths).unwrap();
    state.region = Some("fsn1".into());
    let h = state.hetzner_cloud.as_mut().unwrap();
    h.ssh_key_ids = vec![7];
    h.network_id = Some(100);
    h.server_type = Some("cx22".into());
    state.save(&paths).unwrap();
    let at = crate::target::testkit::LockAtRereads::arm(&f.ctx.store());
    unlock(&f.ctx, PASSWORD);
    assert_eq!(
        at.held(),
        vec![true, true],
        "prod's and staging's states, each re-read under the lock"
    );
    let after = cli_state::State::load_or_default(&paths).unwrap();
    assert_eq!(after.region.as_deref(), Some("fsn1"));
    let h = after.hetzner_cloud.unwrap();
    assert_eq!(
        (h.ssh_key_ids, h.network_id, h.server_type.as_deref()),
        (vec![7], Some(100), Some("cx22"))
    );
    assert_eq!(h.kubeconfig_sealed_to, None, "back under the shared key");
}
