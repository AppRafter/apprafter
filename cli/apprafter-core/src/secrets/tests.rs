// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The files backend of the secret store (D.4a b) on a scratch store: today's
//! `credentials.yaml` and shared-age-key behaviour, behind one door. The cache cases moved here
//! from the CLI's `commands/age_cache.rs`.

use std::path::Path;

use cli_core::secrets::{
    encrypt_for_recipient, load_or_create_identity, CacheKeyKind, UndecryptableWhy,
};
use cli_core::CliError;

use super::*;
use crate::target::testkit::{edit, seed_state_raw, store, TOKEN_A, TOKEN_B};
use crate::Context;

const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";

fn hetzner(kubeconfig_age: Option<&str>, kubeconfig_yaml: Option<&str>) -> HetznerCloudState {
    serde_json::from_value(serde_json::json!({
        "server_id": 1, "server_name": "n",
        "kubeconfig_age": kubeconfig_age, "kubeconfig_yaml": kubeconfig_yaml,
    }))
    .unwrap()
}

/// The context's age key, made now, and `text` encrypted to it.
fn sealed(ctx: &Context, text: &str) -> String {
    let id = load_or_create_identity(ctx.age_key_path()).unwrap();
    encrypt_for_recipient(text, &id.to_public()).unwrap()
}

/// Nothing at `path`, nor its parent directory.
fn absent(path: &Path) -> bool {
    !path.exists() && !path.parent().is_some_and(Path::exists)
}

#[test]
fn building_the_store_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:1")
        .with_home_dir(Some(dir.path().join("home")));
    let secrets = ctx.secrets();
    assert_eq!(secrets.mode().unwrap(), SecretsMode::Files);
    assert_eq!(
        secrets.credentials_file("prod"),
        dir.path().join("store/targets/prod/credentials.yaml")
    );
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "neither the store nor the age key is created"
    );
}

#[test]
fn a_token_record_is_read_without_handing_out_the_token() {
    let (_d, ctx) = store(&["prod", "bare"], None);
    let s = ctx.secrets();
    assert_eq!(
        s.token_presence("prod").unwrap(),
        StoredToken::InFile { chars: 64 }
    );
    assert_eq!(s.stored_token("prod").unwrap().unwrap().expose(), TOKEN_A);
    let bare = s.credentials_file("bare");
    std::fs::remove_file(&bare).unwrap();
    assert_eq!(s.token_presence("bare").unwrap(), StoredToken::None);
    assert!(s.stored_token("bare").unwrap().is_none());
    std::fs::write(&bare, "{}\n").unwrap();
    assert_eq!(s.token_presence("bare").unwrap(), StoredToken::None);
}

/// D.3d review #5 holds through the store: the error names the file and where, never the text.
#[test]
fn an_unreadable_record_is_its_files_error_and_never_quotes_it() {
    let (_d, ctx) = store(&["prod"], None);
    let s = ctx.secrets();
    let creds = s.credentials_file("prod");
    std::fs::write(&creds, format!("hetzner_token:{TOKEN_A}\n")).unwrap();
    for err in [
        s.token_presence("prod").unwrap_err(),
        s.stored_token("prod").unwrap_err(),
    ] {
        assert!(!err.to_string().contains(TOKEN_A), "{err}");
        assert!(
            matches!(&err, CliError::InvalidTargetConfig { path, target, .. }
                if *path == creds && target.as_deref() == Some("prod")),
            "{err:?}"
        );
    }
}

#[test]
fn a_token_is_written_as_save_target_wrote_it_and_nothing_else() {
    let (_d, ctx) = store(&["prod"], None);
    let (_e, reference) = store(&["prod"], None);
    edit(&reference, "prod", |t| {
        t.credentials.hetzner_token = Some(TOKEN_B.into())
    });
    let config = std::fs::read(ctx.store().target_config_file("prod")).unwrap();
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    s.put_token(&key, &held, "prod", &SecretString::new(TOKEN_B))
        .unwrap();
    drop(held);
    // A literal first: the reference below is written through the same cli-core writer.
    assert_eq!(
        std::fs::read_to_string(s.credentials_file("prod")).unwrap(),
        format!("hetzner_token: {TOKEN_B}\n")
    );
    assert_eq!(
        std::fs::read(s.credentials_file("prod")).unwrap(),
        std::fs::read(reference.secrets().credentials_file("prod")).unwrap(),
        "the bytes cli_core::save_target writes"
    );
    assert_eq!(
        std::fs::read(ctx.store().target_config_file("prod")).unwrap(),
        config,
        "config.yaml is the caller's"
    );
    assert_eq!(s.stored_token("prod").unwrap().unwrap().expose(), TOKEN_B);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(s.credentials_file("prod"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
    }
}

/// GOTCHA-120: a read never creates the age key.
#[test]
fn a_read_with_no_key_is_age_key_missing_and_creates_no_key() {
    let (_d, ctx) = store(&["prod"], None);
    let key = ctx.age_key_path().to_path_buf();
    let s = ctx.secrets();
    let err = s
        .cached_kubeconfig(&hetzner(Some("armored"), None), "prod")
        .unwrap_err();
    assert!(
        matches!(&err, CliError::AgeKeyMissing { path, target, stopped: None }
            if *path == key.display().to_string() && target == "prod"),
        "{err:?}"
    );
    assert!(matches!(
        s.cache_key("prod"),
        Err(CliError::AgeKeyMissing { .. })
    ));
    assert!(absent(&key), "a read never writes");
}

#[test]
fn a_key_that_does_not_open_the_cache_says_which_secret_and_whose() {
    let (d, ctx) = store(&["prod"], None);
    load_or_create_identity(ctx.age_key_path()).unwrap();
    let other = load_or_create_identity(&d.path().join("other.key")).unwrap();
    let armored = encrypt_for_recipient(YAML, &other.to_public()).unwrap();
    let s = ctx.secrets();
    let err = s
        .cached_kubeconfig(&hetzner(Some(&armored), None), "prod")
        .unwrap_err();
    assert!(
        matches!(&err, CliError::CacheUndecryptable {
                secret: CachedSecret::Kubeconfig, target, path,
                key: CacheKeyKind::SharedFile, reason: UndecryptableWhy::NotForThisKey, ..
            }
            if target == "prod" && *path == ctx.age_key_path().display().to_string()),
        "{err:?}"
    );
    let key = s.cache_key("prod").unwrap();
    let err = s
        .open_cache(&key, CachedSecret::ArgocdPassword, "prod", &armored)
        .unwrap_err();
    assert!(
        matches!(
            &err,
            CliError::CacheUndecryptable {
                secret: CachedSecret::ArgocdPassword,
                ..
            }
        ),
        "{err:?}"
    );
}

/// PRECEDENCE IS LOAD-BEARING (moved with the code): the encrypted slot wins over a plaintext
/// one beside it; the plaintext slot alone needs no key; none is `Ok(None)`.
#[test]
fn the_encrypted_slot_wins_and_plaintext_needs_no_key() {
    let (_d, ctx) = store(&["prod"], None);
    let s = ctx.secrets();
    let plain = s
        .cached_kubeconfig(&hetzner(None, Some(YAML)), "prod")
        .unwrap()
        .unwrap();
    assert_eq!(plain.as_str(), YAML);
    assert!(absent(ctx.age_key_path()), "a plaintext slot needs no key");
    assert!(s
        .cached_kubeconfig(&hetzner(None, None), "prod")
        .unwrap()
        .is_none());
    let armored = sealed(&ctx, "clusters: [current]\n");
    let picked = s
        .cached_kubeconfig(
            &hetzner(Some(&armored), Some("clusters: [stale]\n")),
            "prod",
        )
        .unwrap()
        .unwrap();
    assert_eq!(picked.as_str(), "clusters: [current]\n");
}

#[test]
fn a_cache_key_opens_what_its_recipient_seals_and_never_shows_itself() {
    let (_d, ctx) = store(&["prod"], None);
    let id = load_or_create_identity(ctx.age_key_path()).unwrap();
    let s = ctx.secrets();
    let key = s.cache_key("prod").unwrap();
    assert_eq!(
        key.recipient().unwrap().to_string(),
        id.to_public().to_string()
    );
    let armored = encrypt_for_recipient("pw", &key.recipient().unwrap()).unwrap();
    assert_eq!(
        s.open_cache(&key, CachedSecret::ArgocdPassword, "prod", &armored)
            .unwrap()
            .as_str(),
        "pw"
    );
    let shown = format!("{key:?}");
    assert!(
        shown.contains("<redacted>") && !shown.contains("AGE-SECRET-KEY"),
        "{shown}"
    );
}

#[test]
fn held_lists_every_targets_ciphertexts_in_order_and_an_unreadable_state() {
    let (_d, ctx) = store(&["a", "b", "c", "d"], None);
    seed_state_raw(
        &ctx,
        "b",
        r#"{"hetzner_cloud":{"server_id":2,"server_name":"b","kubeconfig_age":"x","argocd_admin_password_age":"y"}}"#,
    );
    seed_state_raw(
        &ctx,
        "a",
        r#"{"hetzner_cloud":{"server_id":1,"server_name":"a","kubeconfig_yaml":"plain"}}"#,
    );
    seed_state_raw(&ctx, "d", "not json");
    let held = ctx.secrets().held().unwrap();
    let secret = |t: &str, s| Held {
        target: t.into(),
        what: Holding::Secret(s),
    };
    assert_eq!(held.len(), 3, "{held:?}");
    assert_eq!(held[0], secret("b", CachedSecret::Kubeconfig));
    assert_eq!(held[1], secret("b", CachedSecret::ArgocdPassword));
    assert!(
        matches!(&held[2], Held { target, what: Holding::Unreadable(_) } if target == "d"),
        "{held:?}"
    );
}

/// WI-457: a first use has no key yet (the caller creates it once it has something to cache);
/// a key there is ready; anything cached means the key is lost, and that refuses.
#[test]
fn a_first_use_has_no_key_yet_and_anything_cached_refuses_naming_the_target() {
    let (_d, ctx) = store(&["prod", "staging"], None);
    seed_state_raw(
        &ctx,
        "prod",
        r#"{"hetzner_cloud":{"server_id":1,"server_name":"p"}}"#,
    );
    let s = ctx.secrets();
    let key = ctx.age_key_path().to_path_buf();
    match s.cache_key_for_new_secret("prod").unwrap() {
        NewCacheKey::FirstUse { path } => assert_eq!(path, key),
        other => panic!("{other:?}"),
    }
    assert!(absent(&key), "never created here");
    let made = load_or_create_identity(&key).unwrap();
    match s.cache_key_for_new_secret("prod").unwrap() {
        NewCacheKey::Ready(k) => assert_eq!(
            k.recipient().unwrap().to_string(),
            made.to_public().to_string()
        ),
        other => panic!("{other:?}"),
    }
    std::fs::remove_file(&key).unwrap();
    seed_state_raw(
        &ctx,
        "staging",
        r#"{"hetzner_cloud":{"server_id":2,"server_name":"s","kubeconfig_age":"x"}}"#,
    );
    let err = s.cache_key_for_new_secret("prod").unwrap_err();
    assert!(
        matches!(&err, CliError::AgeKeyMissing { target, stopped: None, .. } if target == "prod"),
        "{err:?}"
    );
    assert!(!key.exists(), "a lost key is never replaced here");
}

/// D.4a T6: in the core's own tests a door refuses a store outside the temp dir before it
/// reads anything, so no core test reaches the account's own store, whatever its context.
#[test]
#[should_panic(expected = "outside the temp dir")]
fn a_store_outside_the_temp_dir_is_refused_in_tests() {
    let _ = Context::for_desktop("/nonexistent/apprafter".into(), "http://127.0.0.1:1")
        .secrets()
        .token_presence("prod");
}

/// D.4a T6: the same for the age key, with the store itself in a tempdir.
#[test]
#[should_panic(expected = "outside the temp dir")]
fn an_age_key_outside_the_temp_dir_is_refused_in_tests() {
    let dir = tempfile::tempdir().unwrap();
    let _ = Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:1")
        .with_home_dir(Some("/nonexistent/home".into()))
        .secrets()
        .cache_key("prod");
}
