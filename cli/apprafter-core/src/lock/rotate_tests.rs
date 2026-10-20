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
use std::sync::Arc;

use cli_core::secrets::{encrypt_for_recipient, Recipient};
use cli_core::{AccountCheckUnavailable, AccountPasswordRefusal};
use cli_state::{State, StatePaths};
use serde_json::json;

use super::facts::{FixedFacts, MachineFact};
use crate::secrets::testing::CountingAccess;
use crate::secrets::{vault_path, LockSlotKind, SlotInfo};
use crate::vault::{
    self, MachineId, NewSlot, OsRandom, SlotBody, SlotKind, SlotRecord, VaultFile, VaultHeader,
};
use crate::{Plan, PlanClass, SecretString};
use std::path::PathBuf;
use std::sync::Mutex;

use cli_core::secrets::{decrypt_with_identity_reason, CachedSecret, Identity};
use cli_state::{HetznerCloudState, SealedTo};

use super::facts::{HelloKeyDeleted, HelloKeys, SlotFacts};
use super::off::HelloKeyGone;
use crate::secrets::{StoreFile, StoreId};
use crate::vault::{KeptMaterial, RecoveryKey, RotationIds, SlotSecret};
use crate::{CancellationToken, NullReporter, Outcome};

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

const LOCK_PASSWORD: &str = "the lock password of a 0.2.80 store";
const ACCOUNT_A: &str = "the account password on box a";
const ACCOUNT_B: &str = "the account password on box b";
const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";
const PW: &str = "argocd-admin-password";
const WRITER: &str = "apprafter rotate-test";
/// The Windows Hello and Touch ID slots a roamed store carries: bodies this build keeps
/// opaque (d4b-b1), put in through the header.
const HELLO_SLOT: SlotId = SlotId([0x44; 16]);
const TOUCH_SLOT: SlotId = SlotId([0x55; 16]);

/// The ways in a test store is made with: B68's three store shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// AppRafter 0.2.80's: the recovery key and a lock password.
    V0280,
    /// D.4b's default (O-b3): the recovery key and this computer's (`a`) account password.
    Default,
    /// A store used on two computers: the recovery key, the account passwords of `a` and
    /// `b`, a Windows Hello slot and a Touch ID slot.
    Roamed,
}

/// A store of a shape — `prod` with a token, `staging` without — its context under the fast
/// policy and its permit, and an access that counts every key it is asked for.
struct Kit {
    _dir: tempfile::TempDir,
    ctx: Context,
    made: LockedTestStore,
    asked: Arc<CountingAccess>,
    hello: Arc<HelloRecorder>,
}

impl Kit {
    fn new(shape: Shape) -> Kit {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN)), ("staging", None)]);
        add_ways(&root, &made, shape);
        let asked = Arc::new(CountingAccess::default());
        let hello = Arc::new(HelloRecorder::new(&root));
        let ctx = Context::for_desktop(root, "http://127.0.0.1:1")
            .with_kdf_policy(&KdfPolicy::INSECURE_FAST)
            .with_insecure_kdf_root(dir.path().to_path_buf())
            .with_vault_access(asked.clone())
            .with_hello_keys(hello.clone());
        Kit {
            _dir: dir,
            ctx,
            made,
            asked,
            hello,
        }
    }

    fn root(&self) -> &Path {
        self.ctx.config_root()
    }

    /// The vault on disk now: its header, read without a key.
    fn header(&self) -> VaultHeader {
        let bytes = vault::file::read(&vault_path(self.root())).unwrap();
        VaultFile::parse(&bytes).unwrap().header().clone()
    }

    /// `folder`'s state, its `hetzner_cloud` as given.
    fn seed(&self, folder: &str, hetzner: serde_json::Value) {
        State {
            hetzner_cloud: Some(serde_json::from_value(hetzner).unwrap()),
            ..Default::default()
        }
        .save(&StatePaths::for_active_target(&self.ctx.store(), folder))
        .unwrap();
    }
}

fn machine(which: u8) -> MachineId {
    MachineId::new(&[which; 16]).unwrap()
}

/// The account password on the computer `which`, as the os-slots glue builds it once the
/// system checked it (d4b-d's G-OS6: only the glue, and tests, build this slot).
fn account(password: &str, which: u8) -> NewSlot {
    NewSlot::OsPassword {
        secret: SecretString::new(password),
        label: account_password_label("walker", &format!("box-{}", which as char)),
        enrolled_at: 1_760_000_000,
        machine: machine(which),
    }
}

fn lock_password(text: &str) -> NewSlot {
    NewSlot::Password {
        secret: SecretString::new(text),
        label: PASSWORD_LABEL.into(),
    }
}

fn sealed(to: &Recipient, plain: &str) -> String {
    encrypt_for_recipient(plain, to).unwrap()
}

/// The store's vault opened with its data key, changed by `edit` as a write of this build
/// would change it, sealed, written over the old one under the store lock, and mirrored in
/// `store.yaml`.
fn rewrite(root: &Path, made: &LockedTestStore, edit: impl FnOnce(&mut vault::UnlockedVault)) {
    let path = vault_path(root);
    let bytes = vault::file::read(&path).unwrap();
    let mut opened = VaultFile::parse(&bytes)
        .unwrap()
        .open_with_key(made.data_key(), made.vault_id)
        .unwrap();
    edit(&mut opened);
    let sealed = opened.seal(WRITER, &mut OsRandom).unwrap();
    let store = cli_core::target::TargetStorePaths::for_root(root.to_path_buf());
    let held = StoreLock::exclusive(&store).unwrap();
    vault::file::replace(&path, &sealed, &held).unwrap();
    let mut file = read_store_file(root).unwrap().unwrap();
    file.slots = opened
        .header()
        .slots
        .iter()
        .map(|slot| SlotInfo {
            kind: slot.kind.into(),
            label: slot.label.clone(),
        })
        .collect();
    write_store_file(root, &file, &held).unwrap();
}

/// `shape`'s ways in beside the kit's recovery key.
fn add_ways(root: &Path, made: &LockedTestStore, shape: Shape) {
    let fast = &KdfPolicy::INSECURE_FAST;
    rewrite(root, made, |vault| match shape {
        Shape::V0280 => {
            vault
                .add_slot(lock_password(LOCK_PASSWORD), fast, &mut OsRandom)
                .unwrap();
        }
        Shape::Default => {
            vault
                .add_slot(account(ACCOUNT_A, b'a'), fast, &mut OsRandom)
                .unwrap();
        }
        Shape::Roamed => {
            for (password, which) in [(ACCOUNT_A, b'a'), (ACCOUNT_B, b'b')] {
                vault
                    .add_slot(account(password, which), fast, &mut OsRandom)
                    .unwrap();
            }
            for (id, kind, label) in [
                (
                    HELLO_SLOT,
                    SlotKind::WindowsHello,
                    "Windows Hello on DESK-1",
                ),
                (TOUCH_SLOT, SlotKind::TouchId, "Touch ID on mac-1"),
            ] {
                vault.header_mut().slots.push(SlotRecord {
                    id,
                    kind,
                    body: SlotBody::Opaque(vec![0x01; 193]),
                    label: label.into(),
                });
            }
        }
    });
}

fn fates_of(plan: &Plan<RotatePayload>) -> Vec<(LockSlotKind, SlotFate)> {
    plan.payload
        .rows()
        .iter()
        .map(|row| (row.slot.kind, row.fate))
        .collect()
}

fn details(plan: &Plan<RotatePayload>) -> Vec<String> {
    plan.changes
        .iter()
        .filter_map(|change| change.detail.clone())
        .collect()
}

fn dropped(why: DropWhy) -> SlotFate {
    SlotFate::Dropped { why }
}

/// B68 (`lock rekey`) and B80 on a store AppRafter 0.2.80 made: its lock password is
/// replaced, its recovery key kept if given, else a new one; the plan asks no key of the
/// client, says what the store key's change covers and what a key change leaves (B65), and
/// mints a vault id and a recovery slot id of its own, none the vault's, new at every plan.
#[test]
fn a_rekey_plan_of_a_0_2_80_store_replaces_its_lock_password() {
    use LockSlotKind::{Password, RecoveryKey};
    let kit = Kit::new(Shape::V0280);
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    assert_eq!(plan.class, PlanClass::Bounded);
    assert!(
        plan.title
            .starts_with("Replace the keys of the target store at"),
        "{}",
        plan.title
    );
    assert_eq!(
        fates_of(&plan),
        vec![
            (RecoveryKey, SlotFate::KeptOrNew),
            (Password, SlotFate::Replaced)
        ]
    );
    assert!(plan.payload.new_lock_password());
    let header = kit.header();
    assert_ne!(plan.payload.new_vault_id(), header.vault_id.to_string());
    assert!(header
        .slots
        .iter()
        .all(|slot| slot.id.to_string() != plan.payload.recovery_slot_id()));
    let again = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    assert_ne!(again.payload.new_vault_id(), plan.payload.new_vault_id());
    assert_ne!(
        again.payload.recovery_slot_id(),
        plan.payload.recovery_slot_id()
    );
    let words = details(&plan);
    assert!(
        words.contains(&"replaced: you set a new lock password".to_string()),
        "{words:?}"
    );
    assert!(
        words.contains(
            &"kept if you open the store with it now, or type it when asked; Enter makes a \
              new one, shown once and typed back"
                .to_string()
        ),
        "{words:?}"
    );
    assert!(plan
        .payload
        .leaves()
        .starts_with("Copies of the vault made before this"));
    assert_eq!(kit.asked.asked(), 0, "no key was asked of the client");
}

/// B68 (`lock recovery`) on a 0.2.80 store: a new recovery key; the lock password kept if
/// given — dropped only by typing `drop` (B87).
#[test]
fn a_recovery_plan_of_a_0_2_80_store_keeps_its_lock_password_if_given() {
    use LockSlotKind::{Password, RecoveryKey};
    let kit = Kit::new(Shape::V0280);
    let plan = plan_rotate(&kit.ctx, RotateKind::Recovery, &FixedFacts::on(b'a')).unwrap();
    assert!(plan
        .title
        .starts_with("Make a new recovery key for the target store at"));
    assert_eq!(
        fates_of(&plan),
        vec![
            (RecoveryKey, SlotFate::Replaced),
            (Password, SlotFate::KeptIfGiven)
        ]
    );
    assert!(!plan.payload.new_lock_password());
    assert!(details(&plan).contains(
        &"kept if you open the store with it now, or type it when asked; Enter asks again, \
          and only typing `drop` drops it"
            .to_string()
    ));
}

/// B68 on the default store (O-b3): this computer's account password is kept if given in
/// both key changes; `lock rekey` sets no lock password where the store has none (ADR 0069
/// §2 reading 2: a pure key change).
#[test]
fn the_plans_of_a_default_store_keep_its_account_password_if_given() {
    use LockSlotKind::{OsPassword, RecoveryKey};
    let kit = Kit::new(Shape::Default);
    let facts = FixedFacts::on(b'a');
    let rekey = plan_rotate(&kit.ctx, RotateKind::Rekey, &facts).unwrap();
    assert_eq!(
        fates_of(&rekey),
        vec![
            (RecoveryKey, SlotFate::KeptOrNew),
            (OsPassword, SlotFate::KeptIfGiven)
        ]
    );
    assert!(!rekey.payload.new_lock_password());
    let recovery = plan_rotate(&kit.ctx, RotateKind::Recovery, &facts).unwrap();
    assert_eq!(
        fates_of(&recovery),
        vec![
            (RecoveryKey, SlotFate::Replaced),
            (OsPassword, SlotFate::KeptIfGiven)
        ]
    );
}

/// B68, B77, F9: on a roamed store only this computer's account password can be kept;
/// another computer's is dropped (its machine secret is not here), and so are Windows Hello
/// (until d4b-w1) and Touch ID — each listed with why before anything is asked. On the
/// other computer the two account passwords swap; on a computer with no machine id neither
/// can be kept.
#[test]
fn a_roamed_store_drops_what_cannot_be_kept_on_this_computer() {
    use LockSlotKind::{OsPassword, RecoveryKey, TouchId, WindowsHello};
    let kit = Kit::new(Shape::Roamed);
    let on =
        |facts: &FixedFacts| fates_of(&plan_rotate(&kit.ctx, RotateKind::Rekey, facts).unwrap());
    let recovery = (RecoveryKey, SlotFate::KeptOrNew);
    let hello = (WindowsHello, dropped(DropWhy::GestureNotHere));
    let touch = (TouchId, dropped(DropWhy::NewerKind));
    let here = (OsPassword, SlotFate::KeptIfGiven);
    let there = (OsPassword, dropped(DropWhy::OtherComputer));
    assert_eq!(
        on(&FixedFacts::on(b'a')),
        vec![recovery, here, there, hello, touch]
    );
    assert_eq!(
        on(&FixedFacts::on(b'b')),
        vec![recovery, there, here, hello, touch]
    );
    let no_id = FixedFacts {
        machine: MachineFact::NoId,
        ..FixedFacts::on(b'a')
    };
    let none = (OsPassword, dropped(DropWhy::NoMachineId));
    assert_eq!(on(&no_id), vec![recovery, none, none, hello, touch]);
    let words = details(&plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap());
    for said in [
        "dropped: made on another computer, whose machine id this one does not have",
        "dropped: this AppRafter cannot confirm Windows Hello",
        "dropped: made by a newer AppRafter",
    ] {
        assert!(words.contains(&said.to_string()), "{said}: {words:?}");
    }
}

/// F12: facts the client did not ask are no guess — a vault with an account-password slot
/// is refused rather than have this computer's way in dropped; a vault without one needs no
/// machine id.
#[test]
fn unknown_facts_refuse_a_store_with_an_account_password_slot() {
    let kit = Kit::new(Shape::Default);
    let err = plan_rotate(&kit.ctx, RotateKind::Rekey, &UnknownFacts).unwrap_err();
    assert!(
        matches!(
            err,
            CoreError::Cli(CliError::AccountPasswordRefused {
                reason: AccountPasswordRefusal::Unavailable {
                    what: AccountCheckUnavailable::NoMachineId
                },
                ..
            })
        ),
        "{err:?}"
    );
    let old = Kit::new(Shape::V0280);
    assert!(plan_rotate(&old.ctx, RotateKind::Rekey, &UnknownFacts).is_ok());
}

/// B18, B77: a second slot of a typed kind — a forged or damaged header, which no prompt
/// tries — is dropped and listed; the first is the one the key change replaces or keeps.
#[test]
fn a_second_slot_of_a_typed_kind_is_dropped() {
    use LockSlotKind::{Password, RecoveryKey};
    let kit = Kit::new(Shape::V0280);
    rewrite(kit.root(), &kit.made, |vault| {
        let mut second = vault
            .header()
            .slots
            .iter()
            .find(|slot| slot.kind == SlotKind::Password)
            .unwrap()
            .clone();
        second.id = SlotId([0x66; 16]);
        vault.header_mut().slots.push(second);
    });
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    assert_eq!(
        fates_of(&plan),
        vec![
            (RecoveryKey, SlotFate::KeptOrNew),
            (Password, SlotFate::Replaced),
            (Password, dropped(DropWhy::SecondOfKind)),
        ]
    );
}

/// B24: a slot whose key settings this build's policy refuses cannot be kept — dropped,
/// listed — where the key change does not replace it anyway.
#[test]
fn a_slot_this_build_refuses_is_not_kept() {
    use LockSlotKind::{OsPassword, RecoveryKey};
    let kit = Kit::new(Shape::Default);
    let strict = kit.ctx.clone().with_kdf_policy(&KdfPolicy::RELEASE);
    let plan = plan_rotate(&strict, RotateKind::Recovery, &FixedFacts::on(b'a')).unwrap();
    assert_eq!(
        fates_of(&plan),
        vec![
            (RecoveryKey, SlotFate::Replaced),
            (OsPassword, dropped(DropWhy::KdfRefused)),
        ]
    );
}

/// B1, d4b-b1's Deviation 10 (critic round, finding 12): an account-password slot of a
/// layout this build cannot read — a newer AppRafter's — is dropped as a newer kind, listed,
/// never read as another computer's.
#[test]
fn an_account_password_slot_of_a_newer_layout_is_dropped() {
    use LockSlotKind::{OsPassword, Password, RecoveryKey};
    let kit = Kit::new(Shape::V0280);
    rewrite(kit.root(), &kit.made, |vault| {
        vault.header_mut().slots.push(SlotRecord {
            id: SlotId([0x77; 16]),
            kind: SlotKind::OsPassword,
            body: SlotBody::Opaque(vec![0x02; 160]),
            label: "account password of walker on box-z".into(),
        });
    });
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    assert_eq!(
        fates_of(&plan),
        vec![
            (RecoveryKey, SlotFate::KeptOrNew),
            (Password, SlotFate::Replaced),
            (OsPassword, dropped(DropWhy::NewerKind)),
        ]
    );
}

/// B79: a state file that does not load — a target's, or one in a folder no target uses —
/// refuses the plan before anything is asked, naming it (`state::corrupt`).
#[test]
fn the_plan_refuses_a_state_file_that_does_not_load() {
    for folder in ["staging", "old"] {
        let kit = Kit::new(Shape::V0280);
        let paths = StatePaths::for_active_target(&kit.ctx.store(), folder);
        paths.create_state_dir().unwrap();
        std::fs::write(paths.state_file(), b"{ not json").unwrap();
        match plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')) {
            Err(CoreError::Cli(CliError::InvalidState { path, .. })) => {
                assert_eq!(path, paths.state_file(), "{folder}")
            }
            other => panic!("{folder}: {other:?}"),
        }
        assert_eq!(kit.asked.asked(), 0);
    }
}

/// B65, B85: the plan says what the store key's change covers — every cache sealed to a
/// key, in the targets' state folders and in a folder no target uses; a kubeconfig in clear
/// is no cache it re-encrypts.
#[test]
fn the_plan_counts_what_the_store_key_reencrypts() {
    let kit = Kit::new(Shape::V0280);
    let to = kit.made.recipient();
    kit.seed(
        "prod",
        json!({
            "server_id": 1, "server_name": "p",
            "kubeconfig_age": sealed(to, YAML), "argocd_admin_password_age": sealed(to, PW),
        }),
    );
    kit.seed(
        "staging",
        json!({
            "server_id": 2, "server_name": "s", "kubeconfig_yaml": YAML,
            "argocd_admin_password_age": sealed(to, PW),
        }),
    );
    kit.seed(
        "old",
        json!({"server_id": 3, "server_name": "o", "kubeconfig_age": sealed(to, YAML)}),
    );
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    assert_eq!(
        plan.payload.store_key(),
        StoreKeyLine {
            caches: 4,
            targets: 2,
            orphans: 1
        }
    );
    assert!(details(&plan).contains(
        &"replaced; 4 cached secrets re-encrypted, of 2 targets and of 1 state folder no \
          target uses"
            .to_string()
    ));
}

/// ADR 0068 §5: a store whose secrets are in files has no key to change; a store whose key
/// change did not finish is finished, never planned over (d4b-c2b's finish).
#[test]
fn a_store_not_locked_has_no_key_to_change() {
    let dir = tempfile::tempdir().unwrap();
    let files = Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:1");
    assert!(matches!(
        plan_rotate(&files, RotateKind::Rekey, &FixedFacts::on(b'a')),
        Err(CoreError::Cli(CliError::UsageRefused { .. }))
    ));
    let kit = Kit::new(Shape::V0280);
    say_rotating(kit.root(), VaultId([0xa5; 16]), Vec::new());
    assert!(matches!(
        plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')),
        Err(CoreError::Cli(CliError::LockIncomplete {
            phase: LockPhase::Rotating,
            ..
        }))
    ));
}

const NEW_LOCK_PASSWORD: &str = "a new lock password, long enough";
const TOKEN_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

/// This computer's Windows Hello keys, played (B15): each call recorded with the slot's id
/// and whether `store.yaml` at that moment still said `rotating` and listed the slot in
/// `hello_drop` — read from the file system at the call, never from timing.
#[derive(Debug)]
struct HelloRecorder {
    root: PathBuf,
    calls: Mutex<Vec<(String, bool)>>,
}

impl HelloRecorder {
    fn new(root: &Path) -> HelloRecorder {
        HelloRecorder {
            root: root.to_path_buf(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, bool)> {
        self.calls.lock().unwrap().clone()
    }
}

impl HelloKeys for HelloRecorder {
    fn delete(&self, _store: &StoreId, slot: &SlotRecord) -> HelloKeyDeleted {
        let listed = read_store_file(&self.root)
            .ok()
            .flatten()
            .and_then(|file| file.rotation)
            .is_some_and(|rotation| rotation.hello_drop.contains(&slot.id));
        self.calls
            .lock()
            .unwrap()
            .push((slot.id.to_string(), listed));
        HelloKeyDeleted::Deleted
    }
}

impl Kit {
    /// The recovery key as `lock on` showed it.
    fn recovery(&self) -> String {
        self.made.recovery_key().expose().to_string()
    }

    /// The vault on disk now, opened with `secret`.
    fn open(&self, secret: SlotSecret) -> vault::VaultResult<vault::UnlockedVault> {
        let bytes = vault::file::read(&vault_path(self.root())).unwrap();
        VaultFile::parse(&bytes)
            .unwrap()
            .open(&secret, &KdfPolicy::INSECURE_FAST)
    }

    /// The store key of the vault on disk now, opened with `secret`.
    fn store_key(&self, secret: SlotSecret) -> Identity {
        self.open(secret).unwrap().payload().age_identity().unwrap()
    }

    fn hetzner(&self, folder: &str) -> HetznerCloudState {
        State::load_or_default(&StatePaths::for_active_target(&self.ctx.store(), folder))
            .unwrap()
            .hetzner_cloud
            .unwrap()
    }

    fn store_file(&self) -> StoreFile {
        read_store_file(self.root()).unwrap().unwrap()
    }
}

fn typed(text: &str) -> RotateAnswer {
    RotateAnswer::Typed(SecretString::new(text))
}

fn recovery_secret(text: &str) -> SlotSecret {
    SlotSecret::RecoveryKey(RecoveryKey::parse(text).unwrap())
}

fn cache(h: &HetznerCloudState, secret: CachedSecret) -> &str {
    match secret {
        CachedSecret::Kubeconfig => h.kubeconfig_age.as_deref(),
        CachedSecret::ArgocdPassword => h.argocd_admin_password_age.as_deref(),
    }
    .unwrap()
}

/// Plans `kind` with `facts`, opens with `opener` and answers every question with `answer`
/// — a new recovery key typed back as it was shown — returning the plan, its material and
/// the key shown, if one was: nothing written yet.
fn asked(
    kit: &Kit,
    kind: RotateKind,
    facts: &dyn SlotFacts,
    opener: &str,
    mut answer: impl FnMut(&RotateQuestion) -> String,
) -> (Plan<RotatePayload>, RotateMaterial, Option<String>) {
    let plan = plan_rotate(&kit.ctx, kind, facts).unwrap();
    let mut material = begin_rotate(&kit.ctx, &plan, SecretString::new(opener)).unwrap();
    let mut shown = None;
    while let Some(question) = material.question() {
        let text = match &question {
            RotateQuestion::NewRecoveryKey { shown: key, .. } => {
                shown = Some(key.expose().to_string());
                key.expose().to_string()
            }
            other => answer(other),
        };
        match material.answer(typed(&text)).unwrap() {
            Answered::Taken | Answered::TakenShort(_) => {}
            Answered::Again(why) => panic!("{question:?} asked again: {why:?}"),
        }
    }
    (plan, material, shown)
}

/// The run of `plan` with `material`, to its end.
fn run(kit: &Kit, plan: Plan<RotatePayload>, material: RotateMaterial) -> KeysRotated {
    match execute_rotate(
        &kit.ctx,
        plan,
        material,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap()
    {
        Outcome::Completed { result } => result,
        other => panic!("{other:?}"),
    }
}

/// [`asked`], then [`run`].
fn rotated(
    kit: &Kit,
    kind: RotateKind,
    facts: &dyn SlotFacts,
    opener: &str,
    answer: impl FnMut(&RotateQuestion) -> String,
) -> (KeysRotated, Option<String>) {
    let (plan, material, shown) = asked(kit, kind, facts, opener, answer);
    (run(kit, plan, material), shown)
}

/// B68, B80, F11: `lock rekey` of a 0.2.80 store opened with its lock password — which the
/// change replaces, so it is not kept — asks a new lock password, then the recovery key,
/// last; Enter there makes a new one, shown with the slot id the plan minted and typed back.
#[test]
fn rekey_asks_a_new_lock_password_then_the_recovery_key_last() {
    let kit = Kit::new(Shape::V0280);
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    let mut m = begin_rotate(&kit.ctx, &plan, SecretString::new(LOCK_PASSWORD)).unwrap();
    assert!(
        matches!(m.question(), Some(RotateQuestion::NewLockPassword)),
        "{:?}",
        m.question()
    );
    assert_eq!(m.answer(typed(NEW_LOCK_PASSWORD)).unwrap(), Answered::Taken);
    match m.question() {
        Some(RotateQuestion::KeepRecoveryKey { slot }) => {
            assert_eq!(slot.kind, LockSlotKind::RecoveryKey)
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(m.answer(typed("")).unwrap(), Answered::Taken);
    let shown = match m.question() {
        Some(RotateQuestion::NewRecoveryKey { shown, slot_id }) => {
            assert_eq!(slot_id, plan.payload.recovery_slot_id());
            shown
        }
        other => panic!("{other:?}"),
    };
    assert!(RecoveryKey::parse(shown.expose()).is_ok());
    assert_eq!(
        m.answer(RotateAnswer::Typed(shown)).unwrap(),
        Answered::Taken
    );
    assert!(m.question().is_none() && m.is_complete());
    assert_eq!(kit.asked.asked(), 0);
}

/// B68: the slot whose secret opened the store is kept without its question — this
/// computer's account password at `lock recovery`, the recovery key at `lock rekey`.
#[test]
fn the_opener_is_kept_without_its_question() {
    let kit = Kit::new(Shape::Default);
    let facts = FixedFacts::on(b'a');
    let plan = plan_rotate(&kit.ctx, RotateKind::Recovery, &facts).unwrap();
    let m = begin_rotate(&kit.ctx, &plan, SecretString::new(ACCOUNT_A)).unwrap();
    assert!(
        matches!(m.question(), Some(RotateQuestion::NewRecoveryKey { .. })),
        "{:?}",
        m.question()
    );
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &facts).unwrap();
    let mut m = begin_rotate(&kit.ctx, &plan, SecretString::new(kit.recovery())).unwrap();
    match m.question() {
        Some(RotateQuestion::Keep { slot, .. }) => {
            assert_eq!(slot.kind, LockSlotKind::OsPassword)
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(m.answer(typed(ACCOUNT_A)).unwrap(), Answered::Taken);
    assert!(
        m.is_complete(),
        "the recovery key opened the store: no question for it"
    );
}

/// B87, ADR 0069 §2 reading 4 (acceptance 4): at a typed way in's question Enter asks
/// again — never a drop — and only the word `drop` drops it; the question says first that
/// dropping it leaves only the recovery key (B68); the outcome lists it.
#[test]
fn enter_asks_again_and_only_drop_drops_a_typed_way_in() {
    let kit = Kit::new(Shape::Default);
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    let mut m = begin_rotate(&kit.ctx, &plan, SecretString::new(kit.recovery())).unwrap();
    let ask = |m: &RotateMaterial| match m.question() {
        Some(RotateQuestion::Keep {
            slot,
            only_recovery_after_drop,
        }) => (slot.kind, only_recovery_after_drop),
        other => panic!("{other:?}"),
    };
    assert_eq!(ask(&m), (LockSlotKind::OsPassword, true));
    assert_eq!(m.answer(typed("")).unwrap(), Answered::Again(Again::Empty));
    assert_eq!(
        ask(&m),
        (LockSlotKind::OsPassword, true),
        "the same question again"
    );
    assert_eq!(m.answer(typed(DROP)).unwrap(), Answered::Taken);
    assert!(m.is_complete());
    let done = run(&kit, plan, m);
    assert_eq!(
        done.dropped
            .iter()
            .map(|d| (d.slot.kind, d.why))
            .collect::<Vec<_>>(),
        vec![(LockSlotKind::OsPassword, DropWhy::Declined)]
    );
}

/// B13, B68: the secret typed to keep a way in is checked against its slot at once (its key
/// check, never PAM): a wrong one is asked again, never kept under the slot.
#[test]
fn a_wrong_secret_is_asked_again_never_kept() {
    let kit = Kit::new(Shape::Default);
    let plan = plan_rotate(&kit.ctx, RotateKind::Recovery, &FixedFacts::on(b'a')).unwrap();
    let mut m = begin_rotate(&kit.ctx, &plan, SecretString::new(kit.recovery())).unwrap();
    assert!(matches!(m.question(), Some(RotateQuestion::Keep { .. })));
    assert_eq!(
        m.answer(typed(ACCOUNT_B)).unwrap(),
        Answered::Again(Again::Wrong)
    );
    assert!(matches!(m.question(), Some(RotateQuestion::Keep { .. })));
    assert_eq!(m.answer(typed(ACCOUNT_A)).unwrap(), Answered::Taken);
    assert!(matches!(
        m.question(),
        Some(RotateQuestion::NewRecoveryKey { .. })
    ));
}

/// O2, S21: a new lock password is refused only by O2's rules — an empty one is asked
/// again — and a short one is taken with its advice.
#[test]
fn a_new_lock_password_takes_o2_s_rules() {
    let kit = Kit::new(Shape::V0280);
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    let mut m = begin_rotate(&kit.ctx, &plan, SecretString::new(LOCK_PASSWORD)).unwrap();
    assert_eq!(
        m.answer(typed("")).unwrap(),
        Answered::Again(Again::PasswordRefused(PasswordProblem::Empty))
    );
    assert!(matches!(
        m.question(),
        Some(RotateQuestion::NewLockPassword)
    ));
    assert!(matches!(
        m.answer(typed("short")).unwrap(),
        Answered::TakenShort(PasswordAdvice::Short { chars: 5, .. })
    ));
}

/// T25, O12, B80: a new recovery key counts only once it is typed back in full; another key
/// typed back asks again, the same key shown with the same slot id.
#[test]
fn a_new_recovery_key_counts_once_typed_back() {
    let kit = Kit::new(Shape::Default);
    let plan = plan_rotate(&kit.ctx, RotateKind::Recovery, &FixedFacts::on(b'a')).unwrap();
    let mut m = begin_rotate(&kit.ctx, &plan, SecretString::new(ACCOUNT_A)).unwrap();
    let shown = |m: &RotateMaterial| match m.question() {
        Some(RotateQuestion::NewRecoveryKey { shown, slot_id }) => {
            (shown.expose().to_string(), slot_id)
        }
        other => panic!("{other:?}"),
    };
    let first = shown(&m);
    let another = RecoveryKey::generate(&mut OsRandom).unwrap().to_display();
    assert!(matches!(
        m.answer(RotateAnswer::Typed(another)).unwrap(),
        Answered::Again(Again::NotTheKeyShown(_))
    ));
    assert_eq!(shown(&m), first, "the same key, the same slot id");
    assert_eq!(m.answer(typed(&first.0)).unwrap(), Answered::Taken);
    assert!(m.is_complete());
}

/// B68: at `lock rekey` the recovery key typed is checked against its slot: text that is no
/// recovery key, and another recovery key, ask again; the store's own is kept, and no new
/// one is made.
#[test]
fn keeping_the_recovery_key_checks_it() {
    let kit = Kit::new(Shape::V0280);
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    let mut m = begin_rotate(&kit.ctx, &plan, SecretString::new(LOCK_PASSWORD)).unwrap();
    assert_eq!(m.answer(typed(NEW_LOCK_PASSWORD)).unwrap(), Answered::Taken);
    assert!(matches!(
        m.answer(typed("not a key")).unwrap(),
        Answered::Again(Again::NotARecoveryKey(_))
    ));
    let another = RecoveryKey::generate(&mut OsRandom).unwrap().to_display();
    assert_eq!(
        m.answer(RotateAnswer::Typed(another)).unwrap(),
        Answered::Again(Again::Wrong)
    );
    assert_eq!(m.answer(typed(&kit.recovery())).unwrap(), Answered::Taken);
    assert!(m.is_complete(), "kept: no new key is made");
}

/// B87, F9 (acceptance 4): a cancelled confirmation drops a Windows Hello way in only where
/// another way besides the recovery key remains; a typed way in always takes the word
/// `drop` — at a typed question a cancel asks for it.
#[test]
fn a_cancelled_confirmation_drops_only_where_another_way_remains() {
    assert!(drop_takes_the_word(LockSlotKind::WindowsHello, true));
    assert!(!drop_takes_the_word(LockSlotKind::WindowsHello, false));
    for kind in [LockSlotKind::Password, LockSlotKind::OsPassword] {
        for last in [true, false] {
            assert!(drop_takes_the_word(kind, last), "{kind:?} {last}");
        }
    }
    let kit = Kit::new(Shape::Default);
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    let mut m = begin_rotate(&kit.ctx, &plan, SecretString::new(kit.recovery())).unwrap();
    assert_eq!(
        m.answer(RotateAnswer::Cancelled).unwrap(),
        Answered::Again(Again::TypeDrop)
    );
    assert!(matches!(m.question(), Some(RotateQuestion::Keep { .. })));
}

/// B80 (acceptance 4): a run stopped after a new recovery key was shown and typed back, and
/// before R1, changed nothing — `store.yaml` and the vault as they were, the current
/// recovery key opens the store, the key shown opens nothing, its slot id is in no vault.
#[test]
fn a_key_shown_and_never_used_opens_nothing() {
    let kit = Kit::new(Shape::Default);
    let store_file = std::fs::read(kit.root().join(STORE_FILE)).unwrap();
    let vault_bytes = vault::file::read(&vault_path(kit.root())).unwrap();
    let (plan, material, shown) = asked(
        &kit,
        RotateKind::Recovery,
        &FixedFacts::on(b'a'),
        ACCOUNT_A,
        |q| panic!("{q:?}"),
    );
    let shown = shown.expect("a new recovery key was shown");
    let slot_id = plan.payload.recovery_slot_id();
    drop((plan, material));
    assert_eq!(
        std::fs::read(kit.root().join(STORE_FILE)).unwrap(),
        store_file
    );
    assert_eq!(
        vault::file::read(&vault_path(kit.root())).unwrap(),
        vault_bytes
    );
    assert!(kit.open(recovery_secret(&kit.recovery())).is_ok());
    assert!(kit.open(recovery_secret(&shown)).is_err());
    assert!(kit
        .header()
        .slots
        .iter()
        .all(|slot| slot.id.to_string() != slot_id));
}

/// B19: a vault written between the plan and the questions — a slot added — is
/// `store_changed`: the plan's fates no longer describe it.
#[test]
fn the_questions_refuse_a_vault_written_since_the_plan() {
    let kit = Kit::new(Shape::Default);
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    rewrite(kit.root(), &kit.made, |vault| {
        vault
            .add_slot(
                lock_password(LOCK_PASSWORD),
                &KdfPolicy::INSECURE_FAST,
                &mut OsRandom,
            )
            .unwrap();
    });
    let err = begin_rotate(&kit.ctx, &plan, SecretString::new(kit.recovery())).unwrap_err();
    assert!(
        matches!(err, CoreError::Cli(CliError::StoreChanged { .. })),
        "{err:?}"
    );
}

/// F13, d4b-b2's hand-off: a `locked` store whose vault still retires a store key — a
/// `store.yaml` put back by hand mid-change (§4.5) — is an unfinished key change, which
/// d4b-c2b finishes, never the start of another.
#[test]
fn the_questions_refuse_a_vault_that_still_retires_a_key() {
    let kit = Kit::new(Shape::Default);
    let path = vault_path(kit.root());
    let opened = VaultFile::parse(&vault::file::read(&path).unwrap())
        .unwrap()
        .open_with_key(kit.made.data_key(), kit.made.vault_id)
        .unwrap();
    let fast = &KdfPolicy::INSECURE_FAST;
    let recovery = RecoveryKey::parse(&kit.recovery()).unwrap();
    let kept = opened
        .keep(
            opened.header().slots[0].id,
            KeptMaterial::RecoveryKey(recovery),
            fast,
        )
        .unwrap();
    let ids = RotationIds::draw(0, &mut OsRandom).unwrap();
    let rotated = vault::rotate(
        &opened,
        &ids,
        vec![kept],
        Vec::new(),
        WRITER,
        fast,
        &mut OsRandom,
    )
    .unwrap();
    let held = StoreLock::exclusive(&kit.ctx.store()).unwrap();
    vault::file::replace(&path, &rotated.bytes, &held).unwrap();
    drop(held);
    let plan = plan_rotate(&kit.ctx, RotateKind::Rekey, &FixedFacts::on(b'a')).unwrap();
    let err = begin_rotate(&kit.ctx, &plan, SecretString::new(kit.recovery())).unwrap_err();
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
}

/// B63, B65, B68, B85, B86, §5.6 (acceptance 1) on a store AppRafter 0.2.80 made: `lock
/// rekey` opened with its lock password, a new one set, the recovery key typed to keep it.
/// The new vault has a new vault id under the same store id; it opens with the new lock
/// password and with the kept recovery key — under its own slot id — and with nothing old:
/// not the old lock password, not the old data key. Every cache is re-sealed to the new
/// store key and records the new vault; the retired key opens none; a folder no target uses
/// is re-sealed and named; tokens and markers are as they were; `store.yaml` is `locked`,
/// its slots the new vault's; no key was asked of the client.
#[test]
fn a_rekey_of_a_0_2_80_store_replaces_every_key_and_keeps_what_was_given() {
    let kit = Kit::new(Shape::V0280);
    let recovery = kit.recovery();
    let old_key = kit.store_key(recovery_secret(&recovery));
    let to = kit.made.recipient();
    kit.seed(
        "prod",
        json!({
            "server_id": 1, "server_name": "p",
            "kubeconfig_age": sealed(to, YAML), "argocd_admin_password_age": sealed(to, PW),
        }),
    );
    kit.seed(
        "staging",
        json!({"server_id": 2, "server_name": "s", "argocd_admin_password_age": sealed(to, PW)}),
    );
    kit.seed(
        "old",
        json!({"server_id": 3, "server_name": "o", "kubeconfig_age": sealed(to, YAML)}),
    );
    let before = kit.header();
    let (done, shown) = rotated(
        &kit,
        RotateKind::Rekey,
        &FixedFacts::on(b'a'),
        LOCK_PASSWORD,
        |q| match q {
            RotateQuestion::NewLockPassword => NEW_LOCK_PASSWORD.into(),
            RotateQuestion::KeepRecoveryKey { .. } => recovery.clone(),
            other => panic!("{other:?}"),
        },
    );
    assert!(shown.is_none(), "the recovery key was kept");
    let header = kit.header();
    assert_eq!(header.store_id, before.store_id);
    assert_ne!(header.vault_id, before.vault_id);
    assert_eq!(done.vault_id, header.vault_id.to_string());
    let old_recovery = &before.slots[0];
    assert_eq!(done.kept, vec![SlotRef::from(old_recovery)]);
    assert_eq!(kinds_of(&done.replaced), vec![LockSlotKind::Password]);
    assert_eq!(kinds_of(&done.added), vec![LockSlotKind::Password]);
    assert!(done.dropped.is_empty());
    assert_eq!(done.opened_with.kind, LockSlotKind::Password);
    let by_key = kit.open(recovery_secret(&recovery)).unwrap();
    assert_eq!(by_key.opened_by(), Some(old_recovery.id));
    assert!(kit
        .open(SlotSecret::Password(SecretString::new(NEW_LOCK_PASSWORD)))
        .is_ok());
    assert!(kit
        .open(SlotSecret::Password(SecretString::new(LOCK_PASSWORD)))
        .is_err());
    let disk = vault::file::read(&vault_path(kit.root())).unwrap();
    assert!(VaultFile::parse(&disk)
        .unwrap()
        .open_with_key(kit.made.data_key(), header.vault_id)
        .is_err());
    let new_key = by_key.payload().age_identity().unwrap();
    assert!(by_key.payload().retiring_identity().unwrap().is_none());
    assert_ne!(
        new_key.to_public().to_string(),
        old_key.to_public().to_string()
    );
    let record = SealedTo::StoreVault {
        store_id: kit.made.store_id.to_string(),
        vault_id: Some(done.vault_id.clone()),
    };
    for (folder, secret, plain) in [
        ("prod", CachedSecret::Kubeconfig, YAML),
        ("prod", CachedSecret::ArgocdPassword, PW),
        ("staging", CachedSecret::ArgocdPassword, PW),
        ("old", CachedSecret::Kubeconfig, YAML),
    ] {
        let h = kit.hetzner(folder);
        let armored = cache(&h, secret);
        assert_eq!(
            decrypt_with_identity_reason(armored, &new_key)
                .ok()
                .as_deref(),
            Some(plain),
            "{folder} {secret:?}"
        );
        assert!(
            decrypt_with_identity_reason(armored, &old_key).is_err(),
            "{folder} {secret:?}"
        );
        assert_eq!(h.sealed_to(secret), Some(&record), "{folder} {secret:?}");
    }
    assert_eq!(done.caches_resealed, 4);
    assert!(done.caches_left.is_empty());
    assert_eq!(
        done.orphans,
        vec![super::reseal::orphan_line(
            &kit.ctx.store().state_dir("old")
        )]
    );
    assert_eq!(
        by_key
            .payload()
            .hetzner_token("prod")
            .map(|t| t.expose().to_string()),
        Some(TOKEN.to_string())
    );
    assert!(matches!(
        crate::secrets::transfer::credentials(&kit.ctx.store(), "prod").unwrap(),
        crate::secrets::transfer::Credentials::Marker(id) if id == kit.made.store_id
    ));
    let file = kit.store_file();
    assert_eq!((file.secrets, file.rotation), (SecretsMode::Locked, None));
    assert_eq!(
        file.slots
            .iter()
            .map(|s| s.label.clone())
            .collect::<Vec<_>>(),
        header
            .slots
            .iter()
            .map(|s| s.label.clone())
            .collect::<Vec<_>>()
    );
    assert!(plan_leaves_and_words());
    assert_eq!(kit.asked.asked(), 0, "no key was asked of the client");
}

/// Each slot's kind, in order.
fn kinds_of(slots: &[SlotRef]) -> Vec<LockSlotKind> {
    slots.iter().map(|slot| slot.kind).collect()
}

/// B65, B51, B80: the words a key change says, from one place (`lock::text`).
fn plan_leaves_and_words() -> bool {
    text::KEY_CHANGE_LEAVES.contains("delete earlier copies you can")
        && text::KEEP_PREVIOUS_RECOVERY_KEY.starts_with("Keep the previous recovery key")
        && text::NEW_KEY_VALID_WHEN_DONE.contains("only when AppRafter says Done")
}

/// B68 on the default store (O-b3): `lock recovery` opened with this computer's account
/// password keeps it; the new recovery key opens the store under the slot id the plan
/// minted, the old one opens nothing.
#[test]
fn a_recovery_of_a_default_store_keeps_its_account_password() {
    let kit = Kit::new(Shape::Default);
    let old_recovery = kit.recovery();
    let (plan, material, shown) = asked(
        &kit,
        RotateKind::Recovery,
        &FixedFacts::on(b'a'),
        ACCOUNT_A,
        |q| panic!("{q:?}"),
    );
    let slot_id = plan.payload.recovery_slot_id();
    let done = run(&kit, plan, material);
    let shown = shown.expect("a new recovery key");
    assert_eq!(kinds_of(&done.kept), vec![LockSlotKind::OsPassword]);
    assert_eq!(kinds_of(&done.replaced), vec![LockSlotKind::RecoveryKey]);
    assert_eq!(
        done.added.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        vec![slot_id.clone()]
    );
    assert_eq!(
        kit.open(recovery_secret(&shown))
            .unwrap()
            .opened_by()
            .map(|id| id.to_string()),
        Some(slot_id)
    );
    assert!(kit.open(recovery_secret(&old_recovery)).is_err());
    assert!(kit
        .open(SlotSecret::AccountPassword(
            SecretString::new(ACCOUNT_A),
            machine(b'a')
        ))
        .is_ok());
}

/// B68, B77, R4b (acceptance 1): `lock rekey` of a roamed store on computer `a`, opened with
/// the recovery key, keeps it and `a`'s account password (typed) and drops `b`'s, Windows
/// Hello and Touch ID, each listed with why; this computer's key of the Hello slot is handed
/// to the client; `store.yaml` lists what the vault holds; `b`'s password opens nothing.
#[test]
fn a_roamed_store_keeps_this_computer_s_way_and_lists_what_it_dropped() {
    let kit = Kit::new(Shape::Roamed);
    let before = kit.header();
    let (done, _) = rotated(
        &kit,
        RotateKind::Rekey,
        &FixedFacts::on(b'a'),
        &kit.recovery(),
        |q| match q {
            RotateQuestion::Keep {
                only_recovery_after_drop: true,
                ..
            } => ACCOUNT_A.into(),
            other => panic!("{other:?}"),
        },
    );
    let slot = |i: usize| SlotRef::from(&before.slots[i]);
    assert_eq!(done.kept, vec![slot(0), slot(1)]);
    assert_eq!(
        done.dropped,
        vec![
            DroppedSlot {
                slot: slot(2),
                why: DropWhy::OtherComputer
            },
            DroppedSlot {
                slot: slot(3),
                why: DropWhy::GestureNotHere
            },
            DroppedSlot {
                slot: slot(4),
                why: DropWhy::NewerKind
            },
        ]
    );
    assert_eq!(
        done.hello_keys,
        vec![HelloKeyGone {
            slot: HELLO_SLOT.to_string(),
            label: "Windows Hello on DESK-1".into(),
            deleted: HelloKeyDeleted::Deleted,
        }]
    );
    assert_eq!(
        kit.store_file()
            .slots
            .iter()
            .map(|s| s.kind)
            .collect::<Vec<_>>(),
        vec![LockSlotKind::RecoveryKey, LockSlotKind::OsPassword]
    );
    assert!(kit
        .open(SlotSecret::AccountPassword(
            SecretString::new(ACCOUNT_B),
            machine(b'b')
        ))
        .is_err());
    assert!(kit
        .open(SlotSecret::AccountPassword(
            SecretString::new(ACCOUNT_A),
            machine(b'a')
        ))
        .is_ok());
}

/// §5.6, acceptance 1 (critic round, finding 8: the overview asks the invariants as
/// property tests; with no property-test crate in either lock, a table over every store
/// shape, both key changes and both kinds of opener stands in): the store id and the
/// marker unchanged, exactly one recovery-key slot, every kept slot under its own id, the
/// recovery key now — kept or shown — opening the store, the old data key opening nothing,
/// the cache opening with the new store key only and recording the new vault, `store.yaml`
/// mirroring the vault, no key asked of the client. One store at a time, the fast policy.
#[test]
fn the_invariants_hold_for_every_shape_key_change_and_opener() {
    for shape in [Shape::V0280, Shape::Default, Shape::Roamed] {
        for kind in [RotateKind::Rekey, RotateKind::Recovery] {
            for by_recovery in [true, false] {
                let case = format!("{shape:?} {kind:?} by_recovery={by_recovery}");
                let kit = Kit::new(shape);
                let to = kit.made.recipient();
                kit.seed(
                    "prod",
                    json!({"server_id": 1, "server_name": "p", "kubeconfig_age": sealed(to, YAML)}),
                );
                let recovery = kit.recovery();
                let old_key = kit.store_key(recovery_secret(&recovery));
                let typed = if shape == Shape::V0280 {
                    LOCK_PASSWORD
                } else {
                    ACCOUNT_A
                };
                let opener = if by_recovery {
                    recovery.clone()
                } else {
                    typed.to_string()
                };
                let before = kit.header();
                let (done, shown) =
                    rotated(&kit, kind, &FixedFacts::on(b'a'), &opener, |q| match q {
                        RotateQuestion::NewLockPassword => NEW_LOCK_PASSWORD.into(),
                        RotateQuestion::KeepRecoveryKey { .. } => recovery.clone(),
                        RotateQuestion::Keep { slot, .. }
                            if slot.kind == LockSlotKind::Password =>
                        {
                            LOCK_PASSWORD.into()
                        }
                        RotateQuestion::Keep { .. } => ACCOUNT_A.into(),
                        other => panic!("{other:?}"),
                    });
                let header = kit.header();
                assert_eq!(header.store_id, before.store_id, "{case}");
                assert_eq!(
                    header
                        .slots
                        .iter()
                        .filter(|s| s.kind == SlotKind::RecoveryKey)
                        .count(),
                    1,
                    "{case}"
                );
                for kept in &done.kept {
                    assert!(
                        before.slots.iter().any(|s| s.id.to_string() == kept.id),
                        "{case}: {kept:?}"
                    );
                }
                let key_now = shown.clone().unwrap_or_else(|| recovery.clone());
                let by_key = kit.open(recovery_secret(&key_now)).unwrap();
                let disk = vault::file::read(&vault_path(kit.root())).unwrap();
                assert!(
                    VaultFile::parse(&disk)
                        .unwrap()
                        .open_with_key(kit.made.data_key(), header.vault_id)
                        .is_err(),
                    "{case}"
                );
                let new_key = by_key.payload().age_identity().unwrap();
                let h = kit.hetzner("prod");
                let armored = cache(&h, CachedSecret::Kubeconfig);
                assert_eq!(
                    decrypt_with_identity_reason(armored, &new_key)
                        .ok()
                        .as_deref(),
                    Some(YAML),
                    "{case}"
                );
                assert!(
                    decrypt_with_identity_reason(armored, &old_key).is_err(),
                    "{case}"
                );
                assert_eq!(
                    h.sealed_to(CachedSecret::Kubeconfig),
                    Some(&SealedTo::StoreVault {
                        store_id: kit.made.store_id.to_string(),
                        vault_id: Some(done.vault_id.clone()),
                    }),
                    "{case}"
                );
                assert!(
                    matches!(
                        crate::secrets::transfer::credentials(&kit.ctx.store(), "prod").unwrap(),
                        crate::secrets::transfer::Credentials::Marker(id) if id == kit.made.store_id
                    ),
                    "{case}"
                );
                assert_eq!(
                    kit.store_file()
                        .slots
                        .iter()
                        .map(|s| s.kind)
                        .collect::<Vec<_>>(),
                    header
                        .slots
                        .iter()
                        .map(|s| LockSlotKind::from(s.kind))
                        .collect::<Vec<_>>(),
                    "{case}"
                );
                assert_eq!(kit.asked.asked(), 0, "{case}");
            }
        }
    }
}

/// What the disk says after a run that stopped at a step.
#[derive(Debug, PartialEq, Eq)]
struct OnDisk {
    secrets: SecretsMode,
    new_vault: bool,
    retiring: bool,
    prod: bool,
    staging: bool,
    hello: bool,
}

/// §5.3 (acceptance 1): R1–R6 land in order — a run stopped at each step (a crash: nothing
/// runs after a fault point) leaves exactly the steps before it on disk.
#[test]
fn every_step_lands_in_order() {
    use FaultPoint as F;
    let at = |secrets, new_vault, retiring, prod, staging, hello| OnDisk {
        secrets,
        new_vault,
        retiring,
        prod,
        staging,
        hello,
    };
    let r = SecretsMode::Rotating;
    for (point, want) in [
        (
            F::AfterStoreFileRotating,
            at(r, false, false, false, false, false),
        ),
        (F::AfterVaultRotated, at(r, true, true, false, false, false)),
        (
            F::AfterRotatedVerified,
            at(r, true, true, false, false, false),
        ),
        (
            F::AfterCachesRekeyed(0),
            at(r, true, true, true, false, false),
        ),
        (
            F::AfterCachesRekeyed(1),
            at(r, true, true, true, true, false),
        ),
        (
            F::AfterRetiringDropped,
            at(r, true, false, true, true, false),
        ),
        (
            F::AfterHelloKeysHandedOver,
            at(r, true, false, true, true, true),
        ),
        (
            F::AfterRotated,
            at(SecretsMode::Locked, true, false, true, true, true),
        ),
    ] {
        let kit = Kit::new(Shape::Roamed);
        let to = kit.made.recipient();
        for folder in ["prod", "staging"] {
            kit.seed(
                folder,
                json!({"server_id": 1, "server_name": folder, "kubeconfig_age": sealed(to, YAML)}),
            );
        }
        let recovery = kit.recovery();
        let (plan, material, _) = asked(
            &kit,
            RotateKind::Rekey,
            &FixedFacts::on(b'a'),
            &recovery,
            |_| ACCOUNT_A.into(),
        );
        let new_id = plan.payload.new_vault_id();
        let crashing = kit.ctx.clone().with_fault(point);
        let err = execute_rotate(
            &crashing,
            plan,
            material,
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("injected fault"),
            "{point:?}: {err}"
        );
        let opened = kit.open(recovery_secret(&recovery)).unwrap();
        let new_vault = kit.header().vault_id.to_string() == new_id;
        let key = opened.payload().age_identity().unwrap();
        let resealed = |folder: &str| {
            new_vault
                && decrypt_with_identity_reason(
                    cache(&kit.hetzner(folder), CachedSecret::Kubeconfig),
                    &key,
                )
                .is_ok()
        };
        let got = OnDisk {
            secrets: kit.store_file().secrets,
            new_vault,
            retiring: opened.payload().retiring_identity().unwrap().is_some(),
            prod: resealed("prod"),
            staging: resealed("staging"),
            hello: !kit.hello.calls().is_empty(),
        };
        assert_eq!(got, want, "{point:?}");
    }
}

/// B84 (a) (acceptance 3): this computer's Windows Hello key of a dropped slot is handed to
/// the client after R4 and before R5 — while `store.yaml` still says `rotating` and lists it
/// in `hello_drop` — so no key is left once `store.yaml` forgets which were dropped.
#[test]
fn hello_keys_are_handed_over_while_store_yaml_still_lists_them() {
    let kit = Kit::new(Shape::Roamed);
    rotated(
        &kit,
        RotateKind::Rekey,
        &FixedFacts::on(b'a'),
        &kit.recovery(),
        |_| ACCOUNT_A.into(),
    );
    assert_eq!(kit.hello.calls(), vec![(HELLO_SLOT.to_string(), true)]);
}

/// B85 (acceptance 1): a cache neither store key opens is left as it was — never dropped —
/// and named with its refresh.
#[test]
fn a_cache_neither_key_opens_is_left_and_named() {
    let kit = Kit::new(Shape::Default);
    let foreign = sealed(&Identity::generate().to_public(), YAML);
    kit.seed(
        "prod",
        json!({"server_id": 1, "server_name": "p", "kubeconfig_age": foreign.clone()}),
    );
    let (done, _) = rotated(
        &kit,
        RotateKind::Recovery,
        &FixedFacts::on(b'a'),
        ACCOUNT_A,
        |q| panic!("{q:?}"),
    );
    assert_eq!(
        done.caches_left,
        vec![DroppedCache {
            target: "prod".into(),
            secret: "kubeconfig".into(),
            why: "it is sealed to a key this key change did not have, or it is damaged: \
                  `apprafter kubeconfig --refresh --target prod` fetches it again"
                .into(),
        }]
    );
    assert_eq!(
        kit.hetzner("prod").kubeconfig_age.as_deref(),
        Some(foreign.as_str())
    );
}

/// B79 (acceptance 3): a state file that stops parsing after the plan is renamed aside
/// during R3 — never deleted — and named in the outcome; the change finishes.
#[test]
fn a_state_file_that_stops_parsing_after_the_plan_is_set_aside_and_named() {
    let kit = Kit::new(Shape::Default);
    let (plan, material, _) = asked(
        &kit,
        RotateKind::Recovery,
        &FixedFacts::on(b'a'),
        ACCOUNT_A,
        |q| panic!("{q:?}"),
    );
    let paths = StatePaths::for_active_target(&kit.ctx.store(), "staging");
    paths.create_state_dir().unwrap();
    std::fs::write(paths.state_file(), b"{ not json").unwrap();
    let done = run(&kit, plan, material);
    assert_eq!(done.set_aside.len(), 1, "{done:?}");
    assert!(
        done.set_aside[0]
            .starts_with("the state file of target `staging` did not parse and was renamed to "),
        "{}",
        done.set_aside[0]
    );
    assert!(!paths.state_file().exists());
    assert_eq!(kit.store_file().secrets, SecretsMode::Locked);
}

/// B19, M3, §5.6: a token written between the questions and the lock — another command's,
/// under the same data key — is in the new vault: its payload is the one read under the
/// lock.
#[test]
fn a_token_written_between_the_questions_and_the_lock_is_kept() {
    let kit = Kit::new(Shape::Default);
    let (plan, material, _) = asked(
        &kit,
        RotateKind::Recovery,
        &FixedFacts::on(b'a'),
        ACCOUNT_A,
        |q| panic!("{q:?}"),
    );
    rewrite(kit.root(), &kit.made, |vault| {
        vault
            .payload_mut()
            .set_hetzner_token("staging", SecretString::new(TOKEN_C))
            .unwrap();
    });
    run(&kit, plan, material);
    let opened = kit
        .open(SlotSecret::AccountPassword(
            SecretString::new(ACCOUNT_A),
            machine(b'a'),
        ))
        .unwrap();
    assert_eq!(
        opened
            .payload()
            .hetzner_token("staging")
            .map(|t| t.expose().to_string()),
        Some(TOKEN_C.to_string())
    );
}

/// B19: a slot written between the questions and the lock is `store_changed`, nothing
/// written: `store.yaml` stays `locked`, the vault the one that write left.
#[test]
fn a_slot_written_between_the_questions_and_the_lock_refuses_the_change() {
    let kit = Kit::new(Shape::Default);
    let (plan, material, _) = asked(
        &kit,
        RotateKind::Recovery,
        &FixedFacts::on(b'a'),
        ACCOUNT_A,
        |q| panic!("{q:?}"),
    );
    rewrite(kit.root(), &kit.made, |vault| {
        vault
            .add_slot(
                lock_password(LOCK_PASSWORD),
                &KdfPolicy::INSECURE_FAST,
                &mut OsRandom,
            )
            .unwrap();
    });
    let written = vault::file::read(&vault_path(kit.root())).unwrap();
    let err = execute_rotate(
        &kit.ctx,
        plan,
        material,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(
        matches!(err, CoreError::Cli(CliError::StoreChanged { .. })),
        "{err:?}"
    );
    assert_eq!(kit.store_file().secrets, SecretsMode::Locked);
    assert_eq!(vault::file::read(&vault_path(kit.root())).unwrap(), written);
}

/// B80: the answers given for one plan never run another — a recovery key shown with one
/// plan's slot id is never written under another's.
#[test]
fn the_answers_of_one_plan_never_run_another() {
    let kit = Kit::new(Shape::Default);
    let (_plan, material, _) = asked(
        &kit,
        RotateKind::Recovery,
        &FixedFacts::on(b'a'),
        ACCOUNT_A,
        |q| panic!("{q:?}"),
    );
    let other = plan_rotate(&kit.ctx, RotateKind::Recovery, &FixedFacts::on(b'a')).unwrap();
    let err = execute_rotate(
        &kit.ctx,
        other,
        material,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("the answers of another plan"),
        "{err}"
    );
    assert_eq!(kit.store_file().secrets, SecretsMode::Locked);
}

/// Rule 5: R2's write is read back and opened with every slot it kept or made; a vault on
/// disk that is not the one written stops the change, named, before any cache moves.
#[test]
fn a_vault_on_disk_that_is_not_what_r2_wrote_stops_the_change() {
    let kit = Kit::new(Shape::Default);
    let to = kit.made.recipient();
    kit.seed(
        "prod",
        json!({"server_id": 1, "server_name": "p", "kubeconfig_age": sealed(to, YAML)}),
    );
    let state = StatePaths::for_active_target(&kit.ctx.store(), "prod").state_file();
    let cached = std::fs::read(&state).unwrap();
    let path = vault_path(kit.root());
    let old = vault::file::read(&path).unwrap();
    let (plan, material, _) = asked(
        &kit,
        RotateKind::Recovery,
        &FixedFacts::on(b'a'),
        ACCOUNT_A,
        |q| panic!("{q:?}"),
    );
    super::rotate::AFTER_ROTATED_WRITE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |_: &Path| {
            std::fs::write(&path, &old).unwrap();
        }));
    });
    let err = execute_rotate(
        &kit.ctx,
        plan,
        material,
        &NullReporter,
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("does not hold the vault this key change wrote"),
        "{err}"
    );
    assert_eq!(std::fs::read(&state).unwrap(), cached, "no cache moved");
}

/// A cancel before the store lock writes nothing.
#[test]
fn a_cancel_before_the_lock_writes_nothing() {
    let kit = Kit::new(Shape::Default);
    let (plan, material, _) = asked(
        &kit,
        RotateKind::Recovery,
        &FixedFacts::on(b'a'),
        ACCOUNT_A,
        |q| panic!("{q:?}"),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        execute_rotate(&kit.ctx, plan, material, &NullReporter, &cancel).unwrap(),
        Outcome::Cancelled { .. }
    ));
    assert_eq!(kit.store_file().secrets, SecretsMode::Locked);
}
