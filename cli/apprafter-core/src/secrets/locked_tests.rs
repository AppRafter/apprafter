// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A target store's lock state (D.4a c2): `store.yaml`, the credentials marker, and what the
//! secret store answers on a locked, a locking and an unlocking store, on scratch stores. The
//! vault itself is Task 3's (`secrets::testing::locked_store`): here no vault is ever opened.

use cli_core::secrets::CachedSecret;
use cli_core::{CliError, LockPhase, LockedWhy, SecretsModeKind, StoreLock};

use super::marker::{classify, write_marker, Marker, MARKER_COMMENT};
use super::*;
use crate::error::CoreError;
use crate::target::testkit::{store, TOKEN_A, TOKEN_B};
use crate::target::TokenPresence;
use crate::target_ref::TargetRef;
use crate::vault::{VaultError, VaultResult, VaultRng, VAULT_FILE};
use crate::Context;

const ID: &str = "0123456789abcdef";
const OTHER: &str = "fedcba9876543210";
const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";

fn id(text: &str) -> StoreId {
    StoreId::parse(text).unwrap()
}

/// `store.yaml` saying `secrets`, with `with_id`, written as a lock flow writes it.
fn lock_state(ctx: &Context, secrets: SecretsMode, with_id: Option<&str>) {
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    write_store_file(
        ctx.config_root(),
        &StoreFile {
            format: STORE_FILE_FORMAT,
            secrets,
            id: with_id.map(id),
            slots: vec![SlotInfo {
                kind: LockSlotKind::RecoveryKey,
                label: "recovery key".into(),
            }],
            unlock_timeout_min: Some(5),
        },
        &held,
    )
    .unwrap();
}

fn hetzner(age: Option<&str>, yaml: Option<&str>) -> HetznerCloudState {
    serde_json::from_value(serde_json::json!({
        "server_id": 1, "server_name": "n", "kubeconfig_age": age, "kubeconfig_yaml": yaml,
    }))
    .unwrap()
}

#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// T31: the lock state round-trips, names nothing secret, warns against its deletion, 0644.
#[test]
fn the_lock_state_round_trips_and_says_never_to_delete_it() {
    let (_d, ctx) = store(&["prod"], None);
    lock_state(&ctx, SecretsMode::Locked, Some(ID));
    assert_eq!(
        read_store_file(ctx.config_root()).unwrap().unwrap(),
        StoreFile {
            format: 1,
            secrets: SecretsMode::Locked,
            id: Some(id(ID)),
            slots: vec![SlotInfo {
                kind: LockSlotKind::RecoveryKey,
                label: "recovery key".into()
            }],
            unlock_timeout_min: Some(5),
        }
    );
    let path = ctx.config_root().join(STORE_FILE);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("# The lock state of this AppRafter target store"),
        "{text}"
    );
    assert!(
        text.contains("secrets: locked\n") && text.contains(&format!("id: {ID}\n")),
        "{text}"
    );
    assert_eq!(ctx.secrets().mode().unwrap(), SecretsMode::Locked);
    #[cfg(unix)]
    assert_eq!(mode_of(&path), 0o644);
    lock_state(&ctx, SecretsMode::Files, None);
    assert!(!std::fs::read_to_string(&path).unwrap().contains("id:"));
    assert_eq!(ctx.secrets().mode().unwrap(), SecretsMode::Files);
}

/// T31: no `store.yaml` and no vault is a store in files, and reading it creates nothing.
#[test]
fn no_lock_state_is_files_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:1")
        .with_home_dir(Some(dir.path().join("home")));
    assert_eq!(read_store_file(ctx.config_root()).unwrap(), None);
    assert_eq!(ctx.secrets().mode().unwrap(), SecretsMode::Files);
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

/// T31: a vault beside no lock state, or beside one that says files, is never read as files:
/// deleting or editing `store.yaml` cannot turn a locked store into one that honours
/// `HCLOUD_TOKEN` or its clear files.
#[test]
fn a_vault_beside_no_lock_is_refused_never_files() {
    let (_d, ctx) = store(&["prod"], None);
    let path = ctx.config_root().join(STORE_FILE);
    std::fs::write(ctx.config_root().join(VAULT_FILE), b"x").unwrap();
    let unreadable =
        |e: CliError| matches!(e, CliError::StoreFileUnreadable { path: ref p, .. } if *p == path);
    assert!(unreadable(read_store_file(ctx.config_root()).unwrap_err()));
    assert!(unreadable(ctx.secrets().mode().unwrap_err()));
    assert!(unreadable(ctx.secrets().stored_token("prod").unwrap_err()));
    lock_state(&ctx, SecretsMode::Files, None);
    assert!(unreadable(read_store_file(ctx.config_root()).unwrap_err()));
    assert!(unreadable(
        ctx.secrets().token_presence("prod").unwrap_err()
    ));
}

/// T31: what no AppRafter writes is unreadable, and the store then gives no secret; only
/// `NotFound` is "no file".
#[test]
fn a_lock_state_that_cannot_be_read_is_refused() {
    let (_d, ctx) = store(&["prod"], None);
    let path = ctx.config_root().join(STORE_FILE);
    for (body, says) in [
        ("format: 1\nsecrets: [\n", None),
        ("secrets: files\n", Some("format")),
        (
            "format: 2\nsecrets: files\n",
            Some("format 2 is newer than this AppRafter reads (1)"),
        ),
        ("format: 0\nsecrets: files\n", Some("format 0 is no format")),
        (
            "format: 1\nsecrets: sealed\n",
            Some("\"sealed\"` is none of files, locking, locked, unlocking"),
        ),
        (
            "format: 1\nsecrets: locked\n",
            Some("it says the secrets are locked, and names no id"),
        ),
        (
            "format: 1\nsecrets: locking\nid: XYZ\n",
            Some("\"XYZ\"` is not 16 lowercase hex characters"),
        ),
    ] {
        std::fs::write(&path, body).unwrap();
        match read_store_file(ctx.config_root()) {
            Err(CliError::StoreFileUnreadable { path: p, detail }) => {
                assert_eq!(p, path);
                assert!(
                    says.is_none_or(|s| detail.contains(s)),
                    "{body:?}: {detail}"
                );
            }
            other => panic!("{body:?}: {other:?}"),
        }
        assert!(ctx.secrets().stored_token("prod").is_err(), "{body:?}");
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        matches!(
            read_store_file(ctx.config_root()),
            Err(CliError::StoreFileUnreadable { .. })
        ),
        "a store.yaml that is a directory is no store.yaml that is absent"
    );
}

/// T28: the lock state is written only under the held write lock; nothing is written without.
#[test]
fn the_lock_state_needs_the_held_write_lock() {
    let (_d, ctx) = store(&["prod"], None);
    let unheld = StoreLock::unheld();
    let file = StoreFile {
        format: STORE_FILE_FORMAT,
        secrets: SecretsMode::Locked,
        id: Some(id(ID)),
        slots: Vec::new(),
        unlock_timeout_min: None,
    };
    let err = write_store_file(ctx.config_root(), &file, &unheld).unwrap_err();
    assert!(
        matches!(&err, CliError::VaultNeedsWriteLock { store } if store == ctx.config_root()),
        "{err:?}"
    );
    assert!(!ctx.config_root().join(STORE_FILE).exists());
}

/// T31: the writer never makes a file its own reader refuses — a lock state without an id is a
/// caller's bug, refused with nothing written, never a store that then reads as unreadable.
#[test]
fn a_lock_state_without_an_id_is_never_written() {
    let (_d, ctx) = store(&["prod"], None);
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    for secrets in [
        SecretsMode::Locking,
        SecretsMode::Locked,
        SecretsMode::Unlocking,
    ] {
        let file = StoreFile {
            format: STORE_FILE_FORMAT,
            secrets,
            id: None,
            slots: Vec::new(),
            unlock_timeout_min: None,
        };
        let err = write_store_file(ctx.config_root(), &file, &held).unwrap_err();
        assert!(
            matches!(&err, CliError::Other(text) if text.contains("nothing was written")),
            "{err:?}"
        );
    }
    drop(held);
    assert!(!ctx.config_root().join(STORE_FILE).exists());
    assert_eq!(ctx.secrets().mode().unwrap(), SecretsMode::Files);
}

/// T32: an id is 16 lowercase hex characters from the generator it is given; a failing
/// generator is its own error, never a weaker id.
#[test]
fn a_store_id_is_sixteen_lowercase_hex_from_its_generator() {
    struct Fixed(u8);
    impl VaultRng for Fixed {
        fn fill(&mut self, buf: &mut [u8]) -> VaultResult<()> {
            buf.fill(self.0);
            Ok(())
        }
    }
    struct Broken;
    impl VaultRng for Broken {
        fn fill(&mut self, _: &mut [u8]) -> VaultResult<()> {
            Err(VaultError::RandomUnavailable {
                detail: "no entropy".into(),
            })
        }
    }
    assert_eq!(
        StoreId::generate(&mut Fixed(0xab)).unwrap().as_str(),
        "abababababababab"
    );
    assert!(matches!(
        StoreId::generate(&mut Broken),
        Err(VaultError::RandomUnavailable { .. })
    ));
    assert!(StoreId::parse(ID).is_some());
    for bad in [
        "",
        "0123456789abcde",
        "0123456789abcdef0",
        "0123456789ABCDEF",
        "0123456789abcdeg",
    ] {
        assert!(StoreId::parse(bad).is_none(), "{bad:?}");
    }
    let a = StoreId::generate(&mut crate::vault::OsRandom).unwrap();
    let b = StoreId::generate(&mut crate::vault::OsRandom).unwrap();
    assert_ne!(a, b);
}

/// T34, overview §8 risk 9 (UNVERIFIED until this runs): the marker names the store, reads back
/// as one, is 0600, and v0.2.79's reader — `TargetCredentials { hetzner_token: Option<String> }`
/// with `#[serde(default)]`, as `git show v0.2.79:cli/cli-core/src/target.rs` declares it —
/// fails on it, so a released CLI stops there with nothing written and nothing printed.
#[test]
fn the_marker_names_the_store_and_a_released_cli_stops_at_it() {
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct V0279Credentials {
        #[allow(dead_code)]
        hetzner_token: Option<String>,
    }
    let (_d, ctx) = store(&["prod"], None);
    write_marker(&ctx.store(), "prod", &id(ID)).unwrap();
    let path = ctx.secrets().credentials_file("prod");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        format!("{MARKER_COMMENT}hetzner_token:\n  vault: {ID}\n")
    );
    assert_eq!(classify(text.as_bytes()), Marker::Vault { id: id(ID) });
    let old = serde_yaml::from_str::<V0279Credentials>(&text)
        .err()
        .expect("a released CLI cannot read a marker");
    assert!(
        old.to_string()
            .contains("invalid type: map, expected a string"),
        "{old}"
    );
    let today = cli_core::target::load_target_credentials(&ctx.store(), "prod").unwrap_err();
    assert!(
        matches!(today, CliError::InvalidTargetConfig { .. }),
        "{today:?}"
    );
    #[cfg(unix)]
    assert_eq!(mode_of(&path), 0o600);
    // An id YAML would read as a number is written so that it reads back as the id.
    write_marker(&ctx.store(), "prod", &id("1234567890123456")).unwrap();
    assert_eq!(
        classify(&std::fs::read(&path).unwrap()),
        Marker::Vault {
            id: id("1234567890123456")
        }
    );
}

/// T34: every shape a `credentials.yaml` takes, told apart without keeping a token.
#[test]
fn classify_tells_every_shape_apart() {
    let marker = format!("{MARKER_COMMENT}hetzner_token:\n  vault: {ID}\n");
    let cases: Vec<(String, Marker)> = vec![
        ("{}\n".into(), Marker::Absent),
        ("other: 1\n".into(), Marker::Absent),
        ("hetzner_token: null\n".into(), Marker::Absent),
        ("hetzner_token: abc\n".into(), Marker::ClearToken),
        // serde_yaml 0.9 reads any scalar as a string (`de.rs` `deserialize_str`): to the
        // files backend these are tokens, so on a locked store they are clear tokens (T42).
        ("hetzner_token: 5\n".into(), Marker::ClearToken),
        ("hetzner_token: true\n".into(), Marker::ClearToken),
        (marker, Marker::Vault { id: id(ID) }),
        ("hetzner_token:\n  vault: XYZ\n".into(), Marker::Unreadable),
        (
            format!("hetzner_token:\n  vault: {ID}\n  extra: 1\n"),
            Marker::Unreadable,
        ),
        (
            format!("hetzner_token:\n  store: {ID}\n"),
            Marker::Unreadable,
        ),
        ("hetzner_token: [1]\n".into(), Marker::Unreadable),
        ("- a\n".into(), Marker::Unreadable),
        ("a: [\n".into(), Marker::Unreadable),
    ];
    for (text, want) in cases {
        assert_eq!(classify(text.as_bytes()), want, "{text:?}");
    }
}

/// T34: a marker in a store in files names another store's vault (the target's directory came
/// from a locked store) and is never read as a token; the other targets read as before.
#[test]
fn a_marker_in_a_store_in_files_names_the_other_store() {
    let (_d, ctx) = store(&["prod", "staging"], None);
    write_marker(&ctx.store(), "prod", &id(OTHER)).unwrap();
    let s = ctx.secrets();
    assert_eq!(
        s.token_presence("prod").unwrap(),
        StoredToken::InAnotherVault { id: id(OTHER) }
    );
    assert!(matches!(s.stored_token("prod"),
        Err(CliError::TokenInAnotherVault { target, id }) if target == "prod" && id == OTHER));
    let t = TargetRef::named(&ctx, "prod").unwrap();
    assert!(matches!(
        crate::target::hetzner_token(&ctx, &t),
        Err(CoreError::Cli(CliError::TokenInAnotherVault { .. }))
    ));
    assert_eq!(
        s.stored_token("staging").unwrap().unwrap().expose(),
        TOKEN_A
    );
}

/// T34, T42: on a locked store each record is read from its marker alone — no vault exists here
/// at all.
#[test]
fn a_locked_store_reads_its_markers_without_a_vault() {
    let (_d, ctx) = store(&["prod", "bare", "copied", "none"], None);
    lock_state(&ctx, SecretsMode::Locked, Some(ID));
    write_marker(&ctx.store(), "prod", &id(ID)).unwrap();
    write_marker(&ctx.store(), "copied", &id(OTHER)).unwrap();
    std::fs::remove_file(ctx.secrets().credentials_file("none")).unwrap();
    let s = ctx.secrets();
    assert_eq!(s.token_presence("prod").unwrap(), StoredToken::InVault);
    assert_eq!(s.token_presence("bare").unwrap(), StoredToken::OutsideVault);
    assert_eq!(
        s.token_presence("copied").unwrap(),
        StoredToken::InAnotherVault { id: id(OTHER) }
    );
    assert_eq!(s.token_presence("none").unwrap(), StoredToken::None);
    assert!(!ctx.config_root().join(VAULT_FILE).exists());
}

/// T42, rule 3: a locked store hands out no token but its vault's — never the clear one planted
/// beside it, never another store's — and what needs the vault refuses while nothing here can
/// open it.
#[test]
fn a_locked_store_never_hands_out_a_clear_token() {
    let (_d, ctx) = store(&["prod", "bare", "copied", "none"], None);
    lock_state(&ctx, SecretsMode::Locked, Some(ID));
    write_marker(&ctx.store(), "prod", &id(ID)).unwrap();
    write_marker(&ctx.store(), "copied", &id(OTHER)).unwrap();
    std::fs::remove_file(ctx.secrets().credentials_file("none")).unwrap();
    let s = ctx.secrets();
    let root = ctx.config_root().to_path_buf();
    assert!(matches!(s.stored_token("prod"),
        Err(CliError::StoreLocked { store, reason: LockedWhy::NoTerminal, stopped_before: None })
            if store == root));
    assert!(matches!(s.stored_token("bare"),
        Err(CliError::TokenOutsideVault { target, path })
            if target == "bare" && path == s.credentials_file("bare")));
    assert!(matches!(
        s.stored_token("copied"),
        Err(CliError::TokenInAnotherVault { .. })
    ));
    assert!(s.stored_token("none").unwrap().is_none());
    assert!(matches!(
        s.cache_key("prod"),
        Err(CliError::StoreLocked { .. })
    ));
    assert!(matches!(
        s.cache_key_for_new_secret("prod"),
        Err(CliError::StoreLocked { .. })
    ));
    assert!(matches!(
        s.prepare_write(),
        Err(CliError::StoreLocked { .. })
    ));
    let t = TargetRef::named(&ctx, "bare").unwrap();
    assert!(matches!(
        crate::target::hetzner_token(&ctx, &t),
        Err(CoreError::Cli(CliError::TokenOutsideVault { .. }))
    ));
}

/// T31: an interrupted lock flow refuses every secret, and its mode says which.
#[test]
fn an_interrupted_lock_refuses_every_secret() {
    for (mode, phase) in [
        (SecretsMode::Locking, LockPhase::Locking),
        (SecretsMode::Unlocking, LockPhase::Unlocking),
    ] {
        let (_d, ctx) = store(&["prod"], None);
        lock_state(&ctx, mode, Some(ID));
        let s = ctx.secrets();
        assert_eq!(s.mode().unwrap(), mode);
        let incomplete =
            |e: CliError| matches!(e, CliError::LockIncomplete { phase: p, .. } if p == phase);
        assert!(incomplete(s.token_presence("prod").unwrap_err()));
        assert!(
            incomplete(s.stored_token("prod").unwrap_err()),
            "never the clear token beside it"
        );
        assert!(incomplete(s.cache_key("prod").unwrap_err()));
        assert!(incomplete(s.prepare_write().unwrap_err()));
        assert!(incomplete(
            s.cached_kubeconfig(&hetzner(None, Some(YAML)), "prod")
                .unwrap_err()
        ));
    }
}

/// T38: on a locked store a plaintext kubeconfig is refused, never used; none cached is none.
#[test]
fn a_plaintext_kubeconfig_on_a_locked_store_is_not_sealed() {
    let (_d, ctx) = store(&["prod"], None);
    lock_state(&ctx, SecretsMode::Locked, Some(ID));
    let s = ctx.secrets();
    assert!(
        matches!(s.cached_kubeconfig(&hetzner(None, Some(YAML)), "prod"),
        Err(CliError::CacheNotSealed { secret: CachedSecret::Kubeconfig, target }) if target == "prod")
    );
    assert!(s
        .cached_kubeconfig(&hetzner(None, None), "prod")
        .unwrap()
        .is_none());
}

/// T41, critic G3: a write prepared while the store was in files is refused once a `lock on`
/// finished under it — its clear token would land on the marker — and the marker stays.
#[test]
fn a_write_prepared_for_files_is_refused_once_the_store_is_locked() {
    let (_d, ctx) = store(&["prod"], None);
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    lock_state(&ctx, SecretsMode::Locked, Some(ID));
    write_marker(&ctx.store(), "prod", &id(ID)).unwrap();
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    let err = s
        .put_token(&key, &held, "prod", &SecretString::new(TOKEN_B))
        .unwrap_err();
    drop(held);
    assert!(
        matches!(
            &err,
            CliError::StoreChanged {
                prepared: SecretsModeKind::Files,
                now: SecretsModeKind::Locked,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        classify(&std::fs::read(s.credentials_file("prod")).unwrap()),
        Marker::Vault { id: id(ID) }
    );
}

/// `target show` on a locked store: a token in the vault is set, its length unknown; a clear or
/// another store's one is no token this store uses.
#[test]
fn show_says_a_vault_token_is_set_and_a_foreign_or_clear_one_is_not() {
    let (_d, ctx) = store(&["prod", "bare"], None);
    lock_state(&ctx, SecretsMode::Locked, Some(ID));
    write_marker(&ctx.store(), "prod", &id(ID)).unwrap();
    let token = |name: &str| {
        crate::target::show(&ctx, &TargetRef::named(&ctx, name).unwrap())
            .unwrap()
            .token
    };
    assert_eq!(
        token("prod"),
        TokenPresence {
            set: true,
            chars: None
        }
    );
    assert_eq!(
        token("bare"),
        TokenPresence {
            set: false,
            chars: None
        }
    );
}

/// c1's frozen v1 `store.yaml` (`tests/vault_vectors/v1/fast-params/`, its SHA-256 pinned by
/// `vault::vectors`) reads as the lock state it records: the reader and the frozen fixture
/// agree, so a reader change that would misread a released store fails here.
#[test]
fn the_frozen_v1_store_yaml_reads_as_its_lock_state() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(STORE_FILE),
        include_bytes!("../../tests/vault_vectors/v1/fast-params/store.yaml"),
    )
    .unwrap();
    assert_eq!(
        read_store_file(dir.path()).unwrap().unwrap(),
        StoreFile {
            format: 1,
            secrets: SecretsMode::Locked,
            id: Some(id(ID)),
            slots: vec![
                SlotInfo {
                    kind: LockSlotKind::Password,
                    label: "lock password".into()
                },
                SlotInfo {
                    kind: LockSlotKind::RecoveryKey,
                    label: "recovery key".into()
                },
            ],
            unlock_timeout_min: Some(15),
        }
    );
}
