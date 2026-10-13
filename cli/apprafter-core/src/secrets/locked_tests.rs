// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A target store's lock state (D.4a c2): `store.yaml`, the credentials marker, and what the
//! secret store answers on a locked, a locking and an unlocking store, on scratch stores. The
//! vault itself is Task 3's (`secrets::testing::locked_store`): here no vault is ever opened.

use std::path::Path;
use std::sync::Arc;

use cli_core::secrets::{
    encrypt_for_recipient, load_or_create_identity, CacheKeyKind, UndecryptableWhy,
};
use cli_core::{DamageKind, RefusedInput, VaultProblem, VaultWriteProblem};

use super::locked::VAULT_WRITER;
use super::testing::{locked_store, LockedTestStore};
use crate::vault::{DataKey, KdfPolicy, RecoveryKey, SlotSecret, VaultFile, VaultId};

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
/// beside it, never another store's — and what needs the vault says the vault is missing, here
/// where `store.yaml` says locked and no vault exists.
#[test]
fn a_locked_store_never_hands_out_a_clear_token() {
    let (_d, ctx) = store(&["prod", "bare", "copied", "none"], None);
    lock_state(&ctx, SecretsMode::Locked, Some(ID));
    write_marker(&ctx.store(), "prod", &id(ID)).unwrap();
    write_marker(&ctx.store(), "copied", &id(OTHER)).unwrap();
    std::fs::remove_file(ctx.secrets().credentials_file("none")).unwrap();
    let s = ctx.secrets();
    let vault = ctx.config_root().join(VAULT_FILE);
    let missing = |e: CliError| {
        matches!(&e, CliError::VaultUnreadable { path, problem: VaultProblem::Missing, .. }
            if *path == vault)
    };
    assert!(missing(s.stored_token("prod").unwrap_err()));
    assert!(matches!(s.stored_token("bare"),
        Err(CliError::TokenOutsideVault { target, path })
            if target == "bare" && path == s.credentials_file("bare")));
    assert!(matches!(
        s.stored_token("copied"),
        Err(CliError::TokenInAnotherVault { .. })
    ));
    assert!(s.stored_token("none").unwrap().is_none());
    assert!(missing(s.cache_key("prod").unwrap_err()));
    assert!(missing(s.cache_key_for_new_secret("prod").unwrap_err()));
    assert!(missing(s.prepare_write().unwrap_err()));
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

// ---- The vault behind the markers (D.4a c2, the vault backend). ----

/// A locked store at a fresh root with `targets`, and a context that opens it with its key.
fn locked(targets: &[(&str, Option<&str>)]) -> (tempfile::TempDir, Context, LockedTestStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let made = locked_store(&root, targets);
    let ctx = Context::for_desktop(root, "http://127.0.0.1:1")
        .with_home_dir(Some(dir.path().join("home")))
        .with_vault_access(made.access());
    (dir, ctx, made)
}

/// `ctx`'s store, with no way to open its vault: the default of every context.
fn without_access(ctx: &Context) -> Context {
    Context::for_desktop(ctx.config_root().to_path_buf(), "http://127.0.0.1:1")
        .with_home_dir(ctx.home_dir().map(Path::to_path_buf))
}

fn vault_bytes(ctx: &Context) -> Vec<u8> {
    std::fs::read(ctx.config_root().join(VAULT_FILE)).unwrap()
}

/// `target`'s token as the vault holds it now, read with the test store's key.
fn entry(ctx: &Context, made: &LockedTestStore, target: &str) -> Option<String> {
    VaultFile::parse(&vault_bytes(ctx))
        .unwrap()
        .open_with_key(made.data_key(), made.vault_id)
        .unwrap()
        .payload()
        .hetzner_token(target)
        .map(|token| token.expose().to_string())
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// T12: a locked store's tokens open through the access its context was given.
#[test]
fn a_locked_store_opens_its_tokens_through_its_access() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A)), ("bare", None)]);
    let s = ctx.secrets();
    assert_eq!(s.mode().unwrap(), SecretsMode::Locked);
    assert_eq!(s.token_presence("prod").unwrap(), StoredToken::InVault);
    assert_eq!(s.stored_token("prod").unwrap().unwrap().expose(), TOKEN_A);
    assert_eq!(s.token_presence("bare").unwrap(), StoredToken::None);
    assert!(s.stored_token("bare").unwrap().is_none());
    let t = TargetRef::named(&ctx, "prod").unwrap();
    assert_eq!(
        crate::target::hetzner_token(&ctx, &t).unwrap().expose(),
        TOKEN_A
    );
}

/// Rule 3: without a way to open it (`NoVaultAccess`, every context's default) a locked store
/// refuses `store_locked` — never an empty answer — and its markers still read.
#[test]
fn without_access_a_locked_store_refuses_and_never_reads_as_empty() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let bare = without_access(&ctx);
    let s = bare.secrets();
    for e in [
        s.stored_token("prod").map(|_| ()).unwrap_err(),
        s.cache_key("prod").map(|_| ()).unwrap_err(),
        s.cache_key_for_new_secret("prod").map(|_| ()).unwrap_err(),
        s.prepare_write().map(|_| ()).unwrap_err(),
    ] {
        assert!(
            matches!(
                &e,
                CliError::StoreLocked {
                    reason: LockedWhy::NoTerminal,
                    stopped_before: None,
                    ..
                }
            ),
            "{e:?}"
        );
    }
    assert_eq!(s.token_presence("prod").unwrap(), StoredToken::InVault);
}

/// T7: reading a token record never opens the vault — here a directory any read would fail on —
/// and an unreadable vault is an error where the token is needed, never "no token".
#[test]
fn the_marker_read_never_opens_the_vault() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let vault = ctx.config_root().join(VAULT_FILE);
    std::fs::remove_file(&vault).unwrap();
    std::fs::create_dir(&vault).unwrap();
    assert_eq!(
        ctx.secrets().token_presence("prod").unwrap(),
        StoredToken::InVault
    );
    let t = TargetRef::named(&ctx, "prod").unwrap();
    assert_eq!(
        crate::target::show(&ctx, &t).unwrap().token,
        TokenPresence {
            set: true,
            chars: None
        }
    );
    assert!(ctx.secrets().stored_token("prod").is_err());
}

/// Rule 3: a key that does not open the payload, for the right vault, is damage — never "no token".
#[test]
fn a_wrong_key_is_damage_and_never_an_absent_token() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let wrong = ctx
        .clone()
        .with_vault_access(Arc::new(FixedVaultAccess::new(
            Arc::new(DataKey::from_bytes(&[7; 32])),
            made.vault_id,
        )));
    let e = wrong.secrets().stored_token("prod").unwrap_err();
    assert!(
        matches!(
            &e,
            CliError::VaultUnreadable {
                problem: VaultProblem::Damaged {
                    what: DamageKind::Tampered
                },
                ..
            }
        ),
        "{e:?}"
    );
}

/// Rule 3, crypto §11: a key for another vault (the lock turned off and on meanwhile) is a
/// changed store, never damage.
#[test]
fn a_key_for_another_vault_is_a_changed_store_never_damage() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let stale = ctx
        .clone()
        .with_vault_access(Arc::new(FixedVaultAccess::new(
            made.data_key().clone(),
            VaultId([9; 16]),
        )));
    let e = stale.secrets().stored_token("prod").unwrap_err();
    assert!(
        matches!(
            &e,
            CliError::StoreChanged {
                prepared: SecretsModeKind::Locked,
                now: SecretsModeKind::Locked,
                ..
            }
        ),
        "{e:?}"
    );
}

/// T29: a vault naming another store, or none, is said so before any key is asked for.
#[test]
fn a_vault_of_another_store_or_none_is_said_so_before_any_key() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let other = tempfile::tempdir().unwrap();
    locked_store(&other.path().join("store"), &[("prod", Some(TOKEN_B))]);
    std::fs::copy(
        other.path().join("store").join(VAULT_FILE),
        ctx.config_root().join(VAULT_FILE),
    )
    .unwrap();
    for c in [&ctx, &without_access(&ctx)] {
        let e = c.secrets().stored_token("prod").unwrap_err();
        assert!(
            matches!(
                &e,
                CliError::VaultUnreadable {
                    problem: VaultProblem::OtherStore,
                    ..
                }
            ),
            "{e:?}"
        );
    }
    std::fs::remove_file(ctx.config_root().join(VAULT_FILE)).unwrap();
    let e = ctx.secrets().stored_token("prod").unwrap_err();
    assert!(
        matches!(
            &e,
            CliError::VaultUnreadable {
                problem: VaultProblem::Missing,
                ..
            }
        ),
        "{e:?}"
    );
}

/// Rule 3: a vault in a format this build does not read is "newer" only when its number is
/// higher; a lower one (c1 refuses any version but 1) is damage, never "upgrade AppRafter".
#[test]
fn only_a_higher_vault_format_is_newer() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let path = ctx.config_root().join(VAULT_FILE);
    let good = std::fs::read(&path).unwrap();
    // The format version: a little-endian u16 right after the 8-byte magic (c1's `format.rs`).
    assert_eq!(&good[8..10], &[1, 0]);
    for low_byte in [2u8, 0] {
        let mut bytes = good.clone();
        bytes[8] = low_byte;
        std::fs::write(&path, &bytes).unwrap();
        let e = ctx.secrets().stored_token("prod").unwrap_err();
        let expected = match low_byte {
            2 => VaultProblem::NewerFormat { version: 2 },
            _ => VaultProblem::Damaged {
                what: DamageKind::Malformed,
            },
        };
        assert!(
            matches!(&e, CliError::VaultUnreadable { problem, .. } if *problem == expected),
            "format {low_byte}: {e:?}"
        );
    }
}

/// Rule 3: one bit flipped in the payload is tampering.
#[test]
fn a_flipped_bit_in_the_vault_is_tampering() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let path = ctx.config_root().join(VAULT_FILE);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();
    let e = ctx.secrets().stored_token("prod").unwrap_err();
    assert!(
        matches!(
            &e,
            CliError::VaultUnreadable {
                problem: VaultProblem::Damaged {
                    what: DamageKind::Tampered
                },
                ..
            }
        ),
        "{e:?}"
    );
}

/// T43: a marker whose vault has no entry for its target is token_missing_from_vault.
#[test]
fn a_marker_without_an_entry_is_token_missing_from_vault() {
    let (_d, ctx, made) = locked(&[("prod", None)]);
    write_marker(&ctx.store(), "prod", &made.store_id).unwrap();
    let e = ctx.secrets().stored_token("prod").unwrap_err();
    assert!(
        matches!(&e, CliError::TokenMissingFromVault { target, .. } if target == "prod"),
        "{e:?}"
    );
}

/// T26, T38: the store's own identity opens its caches and gives new ones their recipient; a
/// cache under the shared age key — there, and readable — is never opened on a locked store.
#[test]
fn the_store_identity_opens_its_caches_and_the_shared_key_never_does() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let s = ctx.secrets();
    let key = s.cache_key("prod").unwrap();
    assert_eq!(key.kind(), CacheKeyView::StoreVault);
    assert_eq!(
        key.recipient().unwrap().to_string(),
        made.recipient().to_string()
    );
    let sealed = encrypt_for_recipient(YAML, &key.recipient().unwrap()).unwrap();
    let opened = s
        .cached_kubeconfig(&hetzner(Some(&sealed), Some("clusters: [stale]\n")), "prod")
        .unwrap()
        .unwrap();
    assert_eq!(opened.as_str(), YAML);
    let shared = load_or_create_identity(ctx.age_key_path()).unwrap();
    let foreign = encrypt_for_recipient(YAML, &shared.to_public()).unwrap();
    let e = s
        .cached_kubeconfig(&hetzner(Some(&foreign), None), "prod")
        .unwrap_err();
    assert!(
        matches!(
            &e,
            CliError::CacheUndecryptable {
                key: CacheKeyKind::StoreVault,
                reason: UndecryptableWhy::NotForThisKey,
                ..
            }
        ),
        "{e:?}"
    );
    assert!(matches!(s.cache_key_for_new_secret("prod").unwrap(),
        NewCacheKey::Ready(k) if k.kind() == CacheKeyView::StoreVault));
    let shown = format!("{key:?}");
    assert!(
        shown.contains("<in the vault>") && !shown.contains("AGE-SECRET-KEY"),
        "{shown}"
    );
}

/// D.4a T35: `APPRAFTER_AGE_KEY` set on a locked store is refused by every cache-key path,
/// before the access is asked anything; in files the same variable is the key path (R4).
#[test]
fn apprafter_age_key_is_refused_on_a_locked_store_before_any_prompt() {
    let (dir, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let counting = Arc::new(super::testing::CountingAccess::default());
    let overridden = SecretStore::new(
        ctx.store(),
        dir.path().join("elsewhere").join("age.key"),
        counting.clone(),
        true,
    );
    let refused = |e: CliError| {
        assert!(
            matches!(&e, CliError::OverrideOnLockedStore { var, store }
                if *var == "APPRAFTER_AGE_KEY" && store == ctx.config_root()),
            "{e:?}"
        );
    };
    refused(overridden.cache_key("prod").unwrap_err());
    refused(overridden.cache_key_for_new_secret("prod").unwrap_err());
    refused(
        overridden
            .cached_kubeconfig(
                &hetzner(Some("-----BEGIN AGE ENCRYPTED FILE-----"), None),
                "prod",
            )
            .unwrap_err(),
    );
    assert_eq!(counting.asked(), 0, "refused before anything was asked");
    // The same variable on a store in files names the key the caches are read with.
    let (_d, files) = store(&["prod"], None);
    let in_files = SecretStore::new(
        files.store(),
        dir.path().join("elsewhere").join("age.key"),
        counting.clone(),
        true,
    );
    assert!(matches!(
        in_files.cache_key("prod").unwrap_err(),
        CliError::AgeKeyMissing { path, .. } if path.ends_with("age.key") && path.contains("elsewhere")
    ));
}

/// T28, T34, rule 2: a write goes into the vault under the held lock and leaves a 0600 marker;
/// no file of the store holds a token in clear, the vault included.
#[test]
fn writes_go_to_the_vault_and_leave_a_marker_and_no_clear_token() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    s.put_token(&key, &held, "prod", &SecretString::new(TOKEN_B))
        .unwrap();
    s.put_token(&key, &held, "staging", &SecretString::new(TOKEN_A))
        .unwrap();
    drop(held);
    assert_eq!(s.stored_token("prod").unwrap().unwrap().expose(), TOKEN_B);
    assert_eq!(
        s.stored_token("staging").unwrap().unwrap().expose(),
        TOKEN_A
    );
    assert_eq!(
        classify(&std::fs::read(s.credentials_file("staging")).unwrap()),
        Marker::Vault {
            id: made.store_id.clone()
        }
    );
    for file in walk(ctx.config_root()) {
        let bytes = std::fs::read(&file).unwrap();
        for token in [TOKEN_A, TOKEN_B] {
            assert!(
                !bytes.windows(token.len()).any(|w| w == token.as_bytes()),
                "{} holds a token in clear",
                file.display()
            );
        }
    }
    #[cfg(unix)]
    assert_eq!(mode_of(&s.credentials_file("staging")), 0o600);
    // c2's writes are the first callers of c1's `replace`: the vault stays owner-only (the
    // Windows DACL is c1's `windows-latest` test).
    #[cfg(unix)]
    assert_eq!(mode_of(&ctx.config_root().join(VAULT_FILE)), 0o600);
}

/// T28: a vault write without the held write lock is refused, and the vault is unchanged —
/// refused by the store before anything is read, even where the write would change nothing
/// (`absent` has no entry, so c1's own check in `replace` is never reached).
#[test]
fn a_vault_write_needs_the_held_write_lock() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    let before = vault_bytes(&ctx);
    let unheld = StoreLock::unheld();
    let refused = |r: cli_core::Result<()>| matches!(r, Err(CliError::VaultNeedsWriteLock { .. }));
    assert!(refused(s.put_token(
        &key,
        &unheld,
        "prod",
        &SecretString::new(TOKEN_B)
    )));
    assert!(refused(s.remove_token(&key, &unheld, "prod").map(|_| ())));
    assert!(refused(s.remove_token(&key, &unheld, "absent").map(|_| ())));
    assert!(refused(s.copy_token(&key, &unheld, "prod", "next")));
    assert!(refused(s.rename_token(&key, &unheld, "prod", "next")));
    drop(unheld);
    assert_eq!(vault_bytes(&ctx), before);
}

/// §3.12: a value the vault cannot hold is this write's refusal — never "the vault is
/// damaged, restore it from a backup" — and nothing is written.
#[test]
fn a_token_the_vault_cannot_hold_is_refused_and_nothing_is_written() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    let before = vault_bytes(&ctx);
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    for token in [String::new(), "x".repeat(16_385)] {
        let e = s
            .put_token(&key, &held, "prod", &SecretString::new(token))
            .unwrap_err();
        assert!(
            matches!(
                &e,
                CliError::VaultUnwritable {
                    problem: VaultWriteProblem::Refused {
                        what: RefusedInput::Token
                    },
                    ..
                }
            ),
            "{e:?}"
        );
    }
    drop(held);
    assert_eq!(vault_bytes(&ctx), before);
    assert_eq!(s.stored_token("prod").unwrap().unwrap().expose(), TOKEN_A);
}

/// Overview §8 risk 10: a marker is written only once the vault it points into is on disk
/// (c1's `replace` syncs the directory best effort): a failed fsync of the store root writes
/// no marker.
#[cfg(unix)]
#[test]
fn no_marker_is_written_before_the_vault_is_durable() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    super::locked::ROOT_SYNC_FAILS.with(|fail| fail.set(true));
    let e = s
        .put_token(&key, &held, "staging", &SecretString::new(TOKEN_B))
        .unwrap_err();
    drop(held);
    assert!(matches!(&e, CliError::Io(_)), "{e:?}");
    assert!(!s.credentials_file("staging").exists());
}

/// §3.7: a vault write records its client as the writer — the core's `apprafter <version>`
/// unless the client's access names itself, as the desktop's will (`apprafter-desktop
/// <version>`, d4a-g).
#[test]
fn a_vault_write_records_the_clients_writer() {
    #[derive(Debug)]
    struct Desktop(Arc<dyn VaultAccess>);
    impl VaultAccess for Desktop {
        fn session(&self, req: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>> {
            self.0.session(req)
        }
        fn write_key(&self, req: &VaultRequest<'_>) -> cli_core::Result<KeySession> {
            self.0.write_key(req)
        }
        fn writer(&self) -> String {
            "apprafter-desktop 9.9.9".into()
        }
    }
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let writer = |c: &Context| {
        VaultFile::parse(&vault_bytes(c))
            .unwrap()
            .header()
            .writer
            .clone()
    };
    assert_eq!(writer(&ctx), VAULT_WRITER);
    let desktop = ctx
        .clone()
        .with_vault_access(Arc::new(Desktop(made.access())));
    let s = desktop.secrets();
    let key = s.prepare_write().unwrap();
    let held = StoreLock::exclusive(&desktop.store()).unwrap();
    s.put_token(&key, &held, "prod", &SecretString::new(TOKEN_B))
        .unwrap();
    drop(held);
    assert_eq!(writer(&desktop), "apprafter-desktop 9.9.9");
}

/// T41, critic G3: a write prepared for one vault is refused once the store was locked again
/// (a new store id) or put back in files under it.
#[test]
fn a_write_prepared_before_the_lock_changed_is_refused() {
    let (_d, ctx, _made) = locked(&[("prod", Some(TOKEN_A))]);
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    std::fs::remove_file(ctx.config_root().join(VAULT_FILE)).unwrap();
    let _again = locked_store(ctx.config_root(), &[("prod", Some(TOKEN_A))]);
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    let e = s
        .put_token(&key, &held, "prod", &SecretString::new(TOKEN_B))
        .unwrap_err();
    drop(held);
    assert!(
        matches!(
            &e,
            CliError::StoreChanged {
                prepared: SecretsModeKind::Locked,
                now: SecretsModeKind::Locked,
                ..
            }
        ),
        "{e:?}"
    );
    std::fs::remove_file(ctx.config_root().join(VAULT_FILE)).unwrap();
    lock_state(&ctx, SecretsMode::Files, None);
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    let e = s
        .put_token(&key, &held, "prod", &SecretString::new(TOKEN_B))
        .unwrap_err();
    drop(held);
    assert!(
        matches!(
            &e,
            CliError::StoreChanged {
                prepared: SecretsModeKind::Locked,
                now: SecretsModeKind::Files,
                ..
            }
        ),
        "{e:?}"
    );
}

/// T41, T43: a rename puts the new entry (`copy_token`), the caller moves the directories,
/// then the old entry goes (`rename_token`) — the store reads at every step, so a crash between
/// them leaves both names, harmless — and a vault an older AppRafter left with only the new
/// name's entry (it renamed the directories alone) is left as it is; remove deletes the entry
/// once.
#[test]
fn remove_and_rename_move_the_vault_entries_and_tolerate_an_older_rename() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A)), ("staging", Some(TOKEN_B))]);
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    s.copy_token(&key, &held, "prod", "eu").unwrap();
    assert_eq!(entry(&ctx, &made, "eu").as_deref(), Some(TOKEN_A));
    assert_eq!(
        s.stored_token("prod").unwrap().unwrap().expose(),
        TOKEN_A,
        "a crash here: prod reads"
    );
    let copied = vault_bytes(&ctx);
    s.copy_token(&key, &held, "prod", "eu").unwrap();
    assert_eq!(
        vault_bytes(&ctx),
        copied,
        "copied already: a re-run writes nothing"
    );
    cli_core::rename_target(&ctx.store(), "prod", "eu").unwrap();
    assert_eq!(
        s.stored_token("eu").unwrap().unwrap().expose(),
        TOKEN_A,
        "a crash here: eu reads"
    );
    s.rename_token(&key, &held, "prod", "eu").unwrap();
    assert_eq!(s.stored_token("eu").unwrap().unwrap().expose(), TOKEN_A);
    assert_eq!(entry(&ctx, &made, "prod"), None);
    let before = vault_bytes(&ctx);
    s.copy_token(&key, &held, "prod", "eu").unwrap();
    s.rename_token(&key, &held, "prod", "eu").unwrap();
    assert_eq!(
        vault_bytes(&ctx),
        before,
        "already under the new name: nothing is written"
    );
    assert!(s.remove_token(&key, &held, "staging").unwrap());
    assert!(
        !s.remove_token(&key, &held, "staging").unwrap(),
        "nothing left to remove"
    );
    drop(held);
    assert!(matches!(
        s.stored_token("staging"),
        Err(CliError::TokenMissingFromVault { .. })
    ));
}

/// T43, §3.12: renaming onto a name an older AppRafter removed — its directory gone, its vault
/// entry left — replaces the leftover: never "the new name already has entries", never a
/// damaged vault.
#[test]
fn a_rename_onto_a_name_an_older_apprafter_removed_replaces_its_leftover() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A)), ("eu", Some(TOKEN_B))]);
    std::fs::remove_dir_all(ctx.store().target_dir("eu")).unwrap();
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    s.copy_token(&key, &held, "prod", "eu").unwrap();
    cli_core::rename_target(&ctx.store(), "prod", "eu").unwrap();
    s.rename_token(&key, &held, "prod", "eu").unwrap();
    drop(held);
    assert_eq!(s.stored_token("eu").unwrap().unwrap().expose(), TOKEN_A);
    assert_eq!(entry(&ctx, &made, "prod"), None);
}

/// In files, remove and rename are the caller's directory moves: nothing for the store to do.
#[test]
fn in_files_remove_and_rename_leave_the_files_to_the_caller() {
    let (_d, ctx) = store(&["prod"], None);
    let s = ctx.secrets();
    let key = s.prepare_write().unwrap();
    let held = StoreLock::exclusive(&ctx.store()).unwrap();
    let before = std::fs::read(s.credentials_file("prod")).unwrap();
    assert!(!s.remove_token(&key, &held, "prod").unwrap());
    s.copy_token(&key, &held, "prod", "eu").unwrap();
    s.rename_token(&key, &held, "prod", "eu").unwrap();
    drop(held);
    assert_eq!(std::fs::read(s.credentials_file("prod")).unwrap(), before);
    assert!(!s.credentials_file("eu").exists());
}

/// The test store d4a-d's spawned tests open: one recovery-key slot, its key as shown opens it,
/// `store.yaml` mirrors it.
#[test]
fn the_test_store_opens_with_its_recovery_key() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let bytes = vault_bytes(&ctx);
    let parsed = VaultFile::parse(&bytes).unwrap();
    assert_eq!(parsed.header().slots.len(), 1);
    assert_eq!(parsed.header().store_id, made.store_id.as_str());
    let key = RecoveryKey::parse(made.recovery_key().expose()).unwrap();
    let opened = parsed
        .open(&SlotSecret::RecoveryKey(key), KdfPolicy::active())
        .unwrap();
    assert_eq!(
        opened.payload().hetzner_token("prod").unwrap().expose(),
        TOKEN_A
    );
    let file = read_store_file(ctx.config_root()).unwrap().unwrap();
    assert_eq!(file.id.as_ref(), Some(&made.store_id));
    assert_eq!(
        file.slots,
        vec![SlotInfo {
            kind: LockSlotKind::RecoveryKey,
            label: "recovery key".into()
        }]
    );
}

/// Rule 9: no Debug output shows the data key or the recovery key.
#[test]
fn no_key_shows_in_a_debug_output() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let hex: String = made
        .data_key()
        .expose()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let groups = made
        .recovery_key()
        .expose()
        .lines()
        .nth(1)
        .unwrap()
        .trim()
        .to_string();
    let session = KeySession::new(
        made.data_key().clone(),
        made.vault_id,
        ctx.config_root().to_path_buf(),
    );
    for shown in [
        format!("{session:?}"),
        format!(
            "{:?}",
            FixedVaultAccess::new(made.data_key().clone(), made.vault_id)
        ),
        format!("{made:?}"),
        format!("{:?}", ctx.secrets().prepare_write().unwrap()),
        format!("{:?}", ctx.secrets().cache_key("prod").unwrap()),
    ] {
        assert!(!shown.contains(&hex) && !shown.contains(&groups), "{shown}");
    }
}

/// T41: a rename on a locked store moves the target's vault entry with its directories.
#[test]
fn a_rename_on_a_locked_store_moves_its_vault_entry() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let from = TargetRef::named(&ctx, "prod").unwrap();
    let plan = crate::target::plan_rename(&ctx, &from, "staging").unwrap();
    crate::target::execute_rename(
        &ctx,
        plan,
        &crate::NullReporter,
        &crate::CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(entry(&ctx, &made, "staging").as_deref(), Some(TOKEN_A));
    assert_eq!(entry(&ctx, &made, "prod"), None);
    assert_eq!(
        ctx.secrets()
            .stored_token("staging")
            .unwrap()
            .unwrap()
            .expose(),
        TOKEN_A
    );
}

/// T41: without the store's key a rename refuses before it moves anything.
#[test]
fn a_rename_on_a_locked_store_without_its_key_moves_nothing() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let bare = without_access(&ctx);
    let from = TargetRef::named(&bare, "prod").unwrap();
    let plan = crate::target::plan_rename(&bare, &from, "staging").unwrap();
    let e = crate::target::execute_rename(
        &bare,
        plan,
        &crate::NullReporter,
        &crate::CancellationToken::new(),
    )
    .unwrap_err();
    assert!(
        matches!(&e, CoreError::Cli(CliError::StoreLocked { .. })),
        "{e:?}"
    );
    assert!(ctx.store().target_dir("prod").exists());
    assert!(!ctx.store().target_dir("staging").exists());
    assert_eq!(entry(&ctx, &made, "prod").as_deref(), Some(TOKEN_A));
}

/// T41: a remove on a locked store deletes the target's vault entry after its directories.
#[test]
fn a_remove_on_a_locked_store_drops_its_vault_entry() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A)), ("staging", Some(TOKEN_B))]);
    let target = TargetRef::named(&ctx, "prod").unwrap();
    let plan = crate::target::plan_remove(&ctx, &target).unwrap();
    crate::target::execute_remove(
        &ctx,
        plan,
        &crate::NullReporter,
        &crate::CancellationToken::new(),
    )
    .unwrap();
    assert!(!ctx.store().target_dir("prod").exists());
    assert_eq!(entry(&ctx, &made, "prod"), None);
    assert_eq!(entry(&ctx, &made, "staging").as_deref(), Some(TOKEN_B));
}

/// T41: without the store's key a remove refuses before it deletes anything.
#[test]
fn a_remove_on_a_locked_store_without_its_key_deletes_nothing() {
    let (_d, ctx, made) = locked(&[("prod", Some(TOKEN_A))]);
    let bare = without_access(&ctx);
    let target = TargetRef::named(&bare, "prod").unwrap();
    let plan = crate::target::plan_remove(&bare, &target).unwrap();
    assert!(matches!(
        crate::target::execute_remove(
            &bare,
            plan,
            &crate::NullReporter,
            &crate::CancellationToken::new()
        )
        .unwrap_err(),
        CoreError::Cli(CliError::StoreLocked { .. })
    ));
    assert!(ctx.store().target_dir("prod").exists());
    assert_eq!(entry(&ctx, &made, "prod").as_deref(), Some(TOKEN_A));
}
