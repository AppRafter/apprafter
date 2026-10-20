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
}

impl Kit {
    fn new(shape: Shape) -> Kit {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN)), ("staging", None)]);
        add_ways(&root, &made, shape);
        let asked = Arc::new(CountingAccess::default());
        let ctx = Context::for_desktop(root, "http://127.0.0.1:1")
            .with_kdf_policy(&KdfPolicy::INSECURE_FAST)
            .with_insecure_kdf_root(dir.path().to_path_buf())
            .with_vault_access(asked.clone());
        Kit {
            _dir: dir,
            ctx,
            made,
            asked,
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
