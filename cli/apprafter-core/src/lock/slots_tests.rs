// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `lock slot add` (d4b-c1 Task 6) and `lock slot remove` (Task 7) on scratch stores, under
//! the fast KDF policy and its permit (`Context::with_kdf_policy`, `with_insecure_kdf_root`):
//! no test derives a release key (T23). Each write is shown built from the vault as it is
//! under the store lock, read back, and opened by nothing but a fresh secret: the client's
//! access — which would hand out a holder's key, or the app's — is never asked (`NoKeys`,
//! which counts every way a client could hand one out: critic round, finding 6).

use std::path::Path;
use std::sync::Arc;

use cli_core::secrets::Identity;
use cli_core::{
    AccountCheckUnavailable, AccountPasswordRefusal, AccountPasswordUnusable, CliError, LockPhase,
    SlotRefusal, StoreLock,
};

use super::facts::{FixedFacts, HostOs, MachineFact};
use super::limits::{LimitClient, SshFacts};
use super::on_tests::{files_store, lock, opened, token_of, PASSWORD};
use super::*;
use crate::report::{Event, Reporter};
use crate::secrets::testing::{locked_store, LockedTestStore};
use crate::secrets::{
    read_store_file, store_locked, KeySession, LockSlotKind, VaultAccess, VaultRequest,
    VaultSession,
};
use crate::vault::{
    KdfPolicy, MachineId, NewSlot, OsRandom, RecoveryKey, SlotInput, VaultPayload, VAULT_FILE,
};
use crate::{
    CancellationToken, ChangeAction, Context, CoreError, NullReporter, Outcome, PlanClass,
    SecretString,
};

const TOKEN: &str = "abababababababababababababababababababababababababababababababab";
const TOKEN_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const ACCOUNT_A: &str = "the account password on box a";
const ACCOUNT_B: &str = "the account password on box b";
const NEW_LOCK_PASSWORD: &str = "a second lock password, long enough";

/// A store the secret store's kit locked — one target, `prod`, and the recovery key — its
/// context under the fast policy and its permit, and a client that counts every way it could
/// hand out a key ([`NoKeys`]): none of these flows may take one (B19).
struct Kit {
    _dir: tempfile::TempDir,
    ctx: Context,
    made: LockedTestStore,
    asked: Arc<NoKeys>,
}

impl Kit {
    fn new() -> Kit {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = locked_store(&root, &[("prod", Some(TOKEN))]);
        let asked = Arc::new(NoKeys::default());
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

    /// The recovery key as `lock on` showed it.
    fn recovery(&self) -> String {
        self.made.recovery_key().expose().to_string()
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
        label: account_password_label("walker", &format!("box-{which:x}")),
        enrolled_at: 1_760_000_000,
        machine: machine(which),
    }
}

fn lock_password(text: &str) -> NewSlot {
    NewSlot::Password {
        secret: SecretString::new(text),
        label: "lock password".into(),
    }
}

/// `secret` typed at a prompt on the computer `on` (`None`: its machine id is not known),
/// opened as a client opens the vault for a slot flow: `lock::open_at`, in this process.
fn open_with(ctx: &Context, secret: &str, on: Option<u8>) -> CoreResult<OpenedVault> {
    let file = read_store_file(ctx.config_root()).unwrap().unwrap();
    open_at(
        ctx.config_root(),
        file.id.as_ref().unwrap(),
        SlotInput::from_input(SecretString::new(secret), on.map(machine)),
        &KdfPolicy::INSECURE_FAST,
    )
}

fn opens(ctx: &Context, secret: &str, on: Option<u8>) -> bool {
    open_with(ctx, secret, on).is_ok()
}

fn completed<T: std::fmt::Debug>(outcome: CoreResult<Outcome<T>>) -> T {
    match outcome.unwrap() {
        Outcome::Completed { result } => result,
        other => panic!("{other:?}"),
    }
}

/// `lock slot add <kind>` to its end: planned with the facts of the computer `on`, opened
/// with `opener` typed on `opener_on`.
fn add(
    ctx: &Context,
    kind: SlotAddKind,
    on: u8,
    opener: &str,
    opener_on: Option<u8>,
    new: NewSlot,
) -> SlotAdded {
    let plan = plan_slot_add(ctx, kind, &FixedFacts::on(on)).unwrap();
    let opened = open_with(ctx, opener, opener_on).unwrap();
    completed(execute_slot_add(
        ctx,
        plan,
        opened,
        new,
        &NullReporter,
        &CancellationToken::new(),
    ))
}

fn vault_bytes(ctx: &Context) -> Vec<u8> {
    std::fs::read(ctx.config_root().join(VAULT_FILE)).unwrap()
}

fn mirrored(ctx: &Context) -> Vec<(LockSlotKind, String)> {
    read_store_file(ctx.config_root())
        .unwrap()
        .unwrap()
        .slots
        .into_iter()
        .map(|s| (s.kind, s.label))
        .collect()
}

fn quiet() -> LimitClient {
    LimitClient::Cli {
        terminal: None,
        ssh: SshFacts::default(),
    }
}

/// A client that hands out nothing and counts every way it could (critic round, finding 6):
/// a session (the CLI's prompt), an open one (a terminal's window, its holder; the unlocked
/// app's reads) and a write key. `CountingAccess` counts the first and the last only.
#[derive(Debug, Default)]
struct NoKeys(std::sync::atomic::AtomicUsize);

impl NoKeys {
    fn asked(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn count(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl VaultAccess for NoKeys {
    fn session(&self, req: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>> {
        self.count();
        Err(store_locked(req.store_root))
    }

    fn open_session(
        &self,
        _: &VaultRequest<'_>,
    ) -> cli_core::Result<Option<Arc<dyn VaultSession>>> {
        self.count();
        Ok(None)
    }

    fn write_key(&self, req: &VaultRequest<'_>) -> cli_core::Result<KeySession> {
        self.count();
        Err(store_locked(req.store_root))
    }
}

/// Another command on the locked store — `target add <target> --renew` — on a thread of its
/// own (critic round, finding 2): it holds the store lock, and stores `token` only once the
/// flow reports waiting for that lock ([`FirstNotice`], which it returns), so the write lands
/// after every read the flow makes before its lock and before every read it makes under it —
/// no sleep, no race (`File::try_lock` is per open file, so two threads conflict). Join the
/// handle after the flow. A flow that fails before it waits drops the reporter, whose sender
/// then ends the writer's wait with an error: never a hang.
fn other_writer(
    other: Context,
    target: &'static str,
    token: &'static str,
) -> (FirstNotice, std::thread::JoinHandle<()>) {
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let secrets = other.secrets();
        let key = secrets.prepare_write().unwrap();
        let held = StoreLock::exclusive(&other.store()).unwrap();
        held_tx.send(()).unwrap();
        go_rx
            .recv()
            .expect("the flow never waited for the store lock");
        secrets
            .put_token(&key, &held, target, &SecretString::new(token))
            .unwrap();
    });
    held_rx.recv().unwrap();
    (FirstNotice(std::sync::Mutex::new(Some(go_tx))), handle)
}

/// Lets [`other_writer`] go at the first notice: the store lock's "waiting for another
/// AppRafter process", the only notice a slot flow reports.
struct FirstNotice(std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>);

impl Reporter for FirstNotice {
    fn report(&self, event: Event) {
        let Event::Notice { .. } = event else {
            return;
        };
        if let Some(go) = self.0.lock().unwrap().take() {
            let _ = go.send(());
        }
    }
}

/// B20, B60 (WI-498 acceptance 1, 2): a lock password is added from the recovery key,
/// opened in this process; the plan needs no secret and names the new slot; the outcome
/// names the slot that allowed it; the new password opens the store; `store.yaml` mirrors
/// the vault; the client was never asked for a key.
#[test]
fn a_lock_password_is_added_from_a_fresh_secret_and_opens_the_store() {
    let k = Kit::new();
    let plan = plan_slot_add(&k.ctx, SlotAddKind::Password, &UnknownFacts).unwrap();
    assert_eq!(plan.class, PlanClass::Bounded);
    assert_eq!(
        plan.changes
            .iter()
            .map(|c| (c.kind.as_str(), c.object.as_str(), c.action))
            .collect::<Vec<_>>(),
        [("Slot", "lock password", ChangeAction::Create)]
    );
    assert!(plan.payload.notes().is_empty() && plan.payload.replaces().is_empty());
    let opened = open_with(&k.ctx, &k.recovery(), None).unwrap();
    let done = completed(execute_slot_add(
        &k.ctx,
        plan,
        opened,
        lock_password(NEW_LOCK_PASSWORD),
        &NullReporter,
        &CancellationToken::new(),
    ));
    assert_eq!(
        (done.added.kind, done.added.label.as_str()),
        (LockSlotKind::Password, "lock password")
    );
    assert_eq!(done.opened_with.kind, LockSlotKind::RecoveryKey);
    assert!(done.replaced.is_empty());
    assert!(opens(&k.ctx, NEW_LOCK_PASSWORD, None) && opens(&k.ctx, &k.recovery(), None));
    assert_eq!(
        mirrored(&k.ctx),
        [
            (LockSlotKind::RecoveryKey, "recovery key".to_string()),
            (LockSlotKind::Password, "lock password".to_string())
        ]
    );
    assert_eq!(
        k.asked.asked(),
        0,
        "a slot write never asks the client for a key (B19)"
    );
}

/// B18: a second lock password is refused by the plan, before anything is asked, and again
/// under the store lock when another process added one after the plan — nothing written.
#[test]
fn a_second_lock_password_is_refused_before_the_prompt_and_under_the_lock() {
    let k = Kit::new();
    let plan = plan_slot_add(&k.ctx, SlotAddKind::Password, &UnknownFacts).unwrap();
    let opened = open_with(&k.ctx, &k.recovery(), None).unwrap();
    add(
        &k.ctx,
        SlotAddKind::Password,
        0xa,
        &k.recovery(),
        None,
        lock_password(NEW_LOCK_PASSWORD),
    );
    let between = vault_bytes(&k.ctx);
    assert!(matches!(
        execute_slot_add(
            &k.ctx,
            plan,
            opened,
            lock_password("a third lock password, long enough"),
            &NullReporter,
            &CancellationToken::new()
        ),
        Err(CoreError::Cli(CliError::SlotRefused {
            reason: SlotRefusal::AlreadyHasPassword,
            ..
        }))
    ));
    assert_eq!(vault_bytes(&k.ctx), between, "nothing written");
    assert!(matches!(
        plan_slot_add(&k.ctx, SlotAddKind::Password, &UnknownFacts),
        Err(CoreError::Cli(CliError::SlotRefused {
            reason: SlotRefusal::AlreadyHasPassword,
            ..
        }))
    ));
    assert_eq!(k.asked.asked(), 0);
}

/// B77 (WI-498 acceptance 3): the account password gets one slot per computer — adding it
/// on B keeps A's; on A only A's is tried; adding it on A again replaces A's own in the same
/// write (the plan says which, with what the slot costs) and keeps B's.
#[test]
fn the_account_password_is_added_on_each_computer_and_replaces_only_its_own() {
    let k = Kit::new();
    let on_a = add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xa,
        &k.recovery(),
        None,
        account(ACCOUNT_A, 0xa),
    );
    assert_eq!(
        (on_a.added.kind, on_a.added.label.as_str()),
        (
            LockSlotKind::OsPassword,
            "account password of walker on box-a"
        )
    );
    let on_b = add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xb,
        &k.recovery(),
        None,
        account(ACCOUNT_B, 0xb),
    );
    assert!(on_b.replaced.is_empty(), "adding it on B keeps A's slot");
    assert!(opens(&k.ctx, ACCOUNT_A, Some(0xa)) && opens(&k.ctx, ACCOUNT_B, Some(0xb)));
    assert!(
        !opens(&k.ctx, ACCOUNT_B, Some(0xa)),
        "on A, only A's slot is tried"
    );
    let plan = plan_slot_add(&k.ctx, SlotAddKind::OsPassword, &FixedFacts::on(0xa)).unwrap();
    assert_eq!(plan.payload.replaces(), std::slice::from_ref(&on_a.added));
    assert_eq!(
        plan.changes.iter().map(|c| c.action).collect::<Vec<_>>(),
        [ChangeAction::Create, ChangeAction::Replace]
    );
    assert_eq!(
        plan.payload.notes(),
        [SlotNote::AccountPasswordOffline, SlotNote::ManagesStore]
    );
    let opened = open_with(&k.ctx, &k.recovery(), None).unwrap();
    let again = completed(execute_slot_add(
        &k.ctx,
        plan,
        opened,
        account("a new password on box a", 0xa),
        &NullReporter,
        &CancellationToken::new(),
    ));
    assert_eq!(again.replaced, [on_a.added]);
    assert!(!opens(&k.ctx, ACCOUNT_A, Some(0xa)));
    assert!(opens(&k.ctx, "a new password on box a", Some(0xa)));
    assert!(opens(&k.ctx, ACCOUNT_B, Some(0xb)), "B's slot stays");
    assert_eq!(k.asked.asked(), 0);
}

/// B76: in a container (the client says its machine id may change) the plan says so.
#[test]
fn a_machine_id_that_may_change_is_said_before_the_password_is_asked() {
    let k = Kit::new();
    let container = FixedFacts {
        may_change: Some(true),
        ..FixedFacts::on(0xa)
    };
    let plan = plan_slot_add(&k.ctx, SlotAddKind::OsPassword, &container).unwrap();
    assert_eq!(
        plan.payload.notes(),
        [
            SlotNote::AccountPasswordOffline,
            SlotNote::ManagesStore,
            SlotNote::MachineIdMayChange
        ]
    );
}

/// B69 (WI-498 acceptance 2): after an account-password change the slot still opens with
/// the previous password, which may open the re-enrolment (O-b8); the new one then opens
/// the store and the previous one no longer does.
#[test]
fn the_previous_account_password_opens_the_re_enrolment() {
    let k = Kit::new();
    let first = add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xa,
        &k.recovery(),
        None,
        account(ACCOUNT_A, 0xa),
    );
    let done = add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xa,
        ACCOUNT_A,
        Some(0xa),
        account("the changed account password", 0xa),
    );
    assert_eq!(
        done.opened_with, first.added,
        "the previous password opened it"
    );
    assert_eq!(done.replaced, [first.added]);
    assert!(!opens(&k.ctx, ACCOUNT_A, Some(0xa)));
    assert!(opens(&k.ctx, "the changed account password", Some(0xa)));
}

/// B60 (WI-498 acceptance 2): a fresh secret of every kind opens an add — the recovery key,
/// the lock password, the account password — and the client's access, which would hand out
/// a holder's key or the app's, is never asked.
#[test]
fn every_kind_of_slot_opens_an_add_and_no_holder_is_asked() {
    let k = Kit::new();
    let by_recovery = add(
        &k.ctx,
        SlotAddKind::Password,
        0xa,
        &k.recovery(),
        None,
        lock_password(NEW_LOCK_PASSWORD),
    );
    let by_password = add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xa,
        NEW_LOCK_PASSWORD,
        None,
        account(ACCOUNT_A, 0xa),
    );
    let by_account = add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xb,
        ACCOUNT_A,
        Some(0xa),
        account(ACCOUNT_B, 0xb),
    );
    assert_eq!(
        [
            by_recovery.opened_with.kind,
            by_password.opened_with.kind,
            by_account.opened_with.kind
        ],
        [
            LockSlotKind::RecoveryKey,
            LockSlotKind::Password,
            LockSlotKind::OsPassword
        ]
    );
    assert_eq!(k.asked.asked(), 0);
}

/// S3, B54, S12, the order of GOTCHA-294: what this computer or this store refuses, the plan
/// refuses before any secret is asked — nothing written, the client never asked.
#[test]
fn the_plan_refuses_before_anything_is_asked() {
    let k = Kit::new();
    let before = vault_bytes(&k.ctx);
    let windows = FixedFacts {
        os: HostOs::Windows,
        ..FixedFacts::on(0xa)
    };
    assert!(matches!(
        plan_slot_add(&k.ctx, SlotAddKind::OsPassword, &windows),
        Err(CoreError::Cli(CliError::SlotRefused {
            reason: SlotRefusal::NotOnThisOs,
            ..
        }))
    ));
    for machine in [MachineFact::NoId, MachineFact::Unknown] {
        let facts = FixedFacts {
            machine,
            ..FixedFacts::on(0xa)
        };
        assert!(matches!(
            plan_slot_add(&k.ctx, SlotAddKind::OsPassword, &facts),
            Err(CoreError::Cli(CliError::AccountPasswordRefused {
                reason: AccountPasswordRefusal::Unavailable {
                    what: AccountCheckUnavailable::NoMachineId
                },
                ..
            }))
        ));
    }
    // T23: weak parameters outside the context's roots.
    let outside = Context::for_desktop(k.ctx.config_root().to_path_buf(), "http://127.0.0.1:1")
        .with_kdf_policy(&KdfPolicy::INSECURE_FAST);
    assert!(matches!(
        plan_slot_add(&outside, SlotAddKind::Password, &UnknownFacts),
        Err(CoreError::InsecureKdfRefused { .. })
    ));
    // An interrupted `lock on` is finished first.
    let yaml = k.ctx.config_root().join("store.yaml");
    let text = std::fs::read_to_string(&yaml).unwrap();
    std::fs::write(
        &yaml,
        text.replace("secrets: locked\n", "secrets: locking\n"),
    )
    .unwrap();
    assert!(matches!(
        plan_slot_add(&k.ctx, SlotAddKind::Password, &UnknownFacts),
        Err(CoreError::Cli(CliError::LockIncomplete {
            phase: LockPhase::Locking,
            ..
        }))
    ));
    std::fs::write(&yaml, text).unwrap();
    // A store in files has no ways in to change.
    let files = files_store();
    assert!(matches!(
        plan_slot_add(&files.ctx, SlotAddKind::Password, &UnknownFacts),
        Err(CoreError::Cli(CliError::UsageRefused { .. }))
    ));
    assert_eq!(vault_bytes(&k.ctx), before);
    assert_eq!(k.asked.asked(), 0);
}

/// M3, B19 (WI-498 acceptance 4): the write is built from the vault as it is under the
/// store lock — a token another command stores after the prompt, while this one waits for
/// the lock, is kept (critic round, finding 2: stored before the flow started, the token
/// would be kept by a flow that read the vault before its lock too; t6m9 is that flow).
#[test]
fn a_token_stored_between_the_prompt_and_the_lock_is_kept() {
    let f = files_store();
    let shown = lock(&f.ctx);
    let plan = plan_slot_add(&f.ctx, SlotAddKind::OsPassword, &FixedFacts::on(0xa)).unwrap();
    let opener = open_with(&f.ctx, PASSWORD, None).unwrap();
    let (waiting, other) = other_writer(opened(&f.ctx, PASSWORD), "staging", TOKEN_C);
    completed(execute_slot_add(
        &f.ctx,
        plan,
        opener,
        account(ACCOUNT_A, 0xa),
        &waiting,
        &CancellationToken::new(),
    ));
    other.join().unwrap();
    assert_eq!(token_of(&f.ctx, &shown, "staging"), TOKEN_C);
    assert!(opens(&f.ctx, ACCOUNT_A, Some(0xa)));
}

/// B19: a vault replaced between the prompt and the lock — a key change — is
/// `store_changed`: the opener's key is for another vault, and nothing is written.
#[test]
fn a_vault_replaced_since_the_prompt_is_store_changed() {
    let k = Kit::new();
    let plan = plan_slot_add(&k.ctx, SlotAddKind::Password, &UnknownFacts).unwrap();
    let opener = open_with(&k.ctx, &k.recovery(), None).unwrap();
    let (other, _) = crate::vault::create(
        k.made.store_id.as_str(),
        "apprafter 0.0.0-test",
        VaultPayload::new(&Identity::generate()),
        vec![NewSlot::RecoveryKey {
            key: RecoveryKey::generate(&mut OsRandom).unwrap(),
            label: "recovery key".into(),
        }],
        &KdfPolicy::INSECURE_FAST,
        &mut OsRandom,
    )
    .unwrap();
    let held = StoreLock::exclusive(&k.ctx.store()).unwrap();
    crate::vault::file::replace(&k.ctx.config_root().join(VAULT_FILE), &other, &held).unwrap();
    drop(held);
    assert!(matches!(
        execute_slot_add(
            &k.ctx,
            plan,
            opener,
            lock_password(NEW_LOCK_PASSWORD),
            &NullReporter,
            &CancellationToken::new()
        ),
        Err(CoreError::Cli(CliError::StoreChanged { .. }))
    ));
    assert_eq!(vault_bytes(&k.ctx), other, "nothing written");
}

/// B51 (WI-498 acceptance 4): a store root that does not sync fails the write before its
/// outcome. Unix: Windows has no directory fsync, and there the vault's move is
/// write-through (d4b-b1).
#[cfg(unix)]
#[test]
fn a_root_that_does_not_sync_reports_no_outcome() {
    let k = Kit::new();
    let plan = plan_slot_add(&k.ctx, SlotAddKind::Password, &UnknownFacts).unwrap();
    let opener = open_with(&k.ctx, &k.recovery(), None).unwrap();
    crate::secrets::ROOT_SYNC_FAILS.with(|fail| fail.set(true));
    match execute_slot_add(
        &k.ctx,
        plan,
        opener,
        lock_password(NEW_LOCK_PASSWORD),
        &NullReporter,
        &CancellationToken::new(),
    ) {
        Err(CoreError::Cli(CliError::Io(e))) => {
            assert!(
                e.to_string().contains("injected directory fsync failure"),
                "{e}"
            )
        }
        other => panic!("{other:?}"),
    }
}

/// §9.2 c1: a fault before the replace leaves the vault as it was; one after it leaves the
/// new vault, whole and opening with the new slot, and `store.yaml`'s mirror as it was —
/// the drift doctor names, which the next write puts right (B42). The recovery key opens
/// either; the report reads the vault.
#[test]
fn a_fault_before_or_after_the_replace_leaves_one_whole_vault() {
    for (at, written) in [
        (FaultPoint::BeforeSlotWrite, false),
        (FaultPoint::AfterSlotWrite, true),
    ] {
        let k = Kit::new();
        let before = vault_bytes(&k.ctx);
        let mirror = mirrored(&k.ctx);
        let ctx = k.ctx.clone().with_fault(at);
        let plan = plan_slot_add(&ctx, SlotAddKind::Password, &UnknownFacts).unwrap();
        let opener = open_with(&ctx, &k.recovery(), None).unwrap();
        let result = execute_slot_add(
            &ctx,
            plan,
            opener,
            lock_password(NEW_LOCK_PASSWORD),
            &NullReporter,
            &CancellationToken::new(),
        );
        assert!(result.is_err(), "{at:?}");
        assert_eq!(vault_bytes(&k.ctx) != before, written, "{at:?}");
        assert_eq!(opens(&k.ctx, NEW_LOCK_PASSWORD, None), written, "{at:?}");
        assert!(opens(&k.ctx, &k.recovery(), None), "{at:?}");
        assert_eq!(
            mirrored(&k.ctx),
            mirror,
            "{at:?}: store.yaml is rewritten last"
        );
        let listed = status(&k.ctx, UnlockWindow::Closed, &quiet(), &UnknownFacts)
            .unwrap()
            .slots
            .len();
        assert_eq!(
            listed,
            1 + usize::from(written),
            "{at:?}: the report reads the vault"
        );
    }
}

/// B20: a vault that, read back, does not hold the slot just written is `vault_unreadable`
/// (not kept), and `store.yaml` is not rewritten.
#[test]
fn a_write_that_does_not_read_back_is_not_kept() {
    let k = Kit::new();
    let before = vault_bytes(&k.ctx);
    let mirror = mirrored(&k.ctx);
    let plan = plan_slot_add(&k.ctx, SlotAddKind::Password, &UnknownFacts).unwrap();
    let opener = open_with(&k.ctx, &k.recovery(), None).unwrap();
    super::slots::AFTER_SLOT_WRITE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |root: &Path| {
            std::fs::write(root.join(VAULT_FILE), &before).unwrap()
        }));
    });
    let result = execute_slot_add(
        &k.ctx,
        plan,
        opener,
        lock_password(NEW_LOCK_PASSWORD),
        &NullReporter,
        &CancellationToken::new(),
    );
    super::slots::AFTER_SLOT_WRITE.with(|hook| *hook.borrow_mut() = None);
    assert!(
        matches!(
            result,
            Err(CoreError::Cli(CliError::VaultUnreadable { .. }))
        ),
        "{result:?}"
    );
    assert_eq!(mirrored(&k.ctx), mirror);
}

/// The new slot is the plan's kind, or nothing is written (a caller's slip); an account
/// password no slot can hold is `account_password_refused`, never a lock password's words.
#[test]
fn a_slot_of_another_kind_or_one_no_slot_holds_is_refused_with_nothing_written() {
    let k = Kit::new();
    let before = vault_bytes(&k.ctx);
    let run = |kind: SlotAddKind, new: NewSlot| {
        let plan = plan_slot_add(&k.ctx, kind, &FixedFacts::on(0xa)).unwrap();
        let opener = open_with(&k.ctx, &k.recovery(), None).unwrap();
        execute_slot_add(
            &k.ctx,
            plan,
            opener,
            new,
            &NullReporter,
            &CancellationToken::new(),
        )
    };
    assert!(matches!(
        run(SlotAddKind::Password, account(ACCOUNT_A, 0xa)),
        Err(CoreError::Cli(CliError::Other(_)))
    ));
    assert!(matches!(
        run(SlotAddKind::OsPassword, account(ACCOUNT_A, 0xb)),
        Err(CoreError::Cli(CliError::Other(_)))
    ));
    // Critic round, finding 5: above, a slot bound to another machine id than the plan's.
    assert!(matches!(
        run(SlotAddKind::OsPassword, account("", 0xa)),
        Err(CoreError::Cli(CliError::AccountPasswordRefused {
            reason: AccountPasswordRefusal::NoPassword,
            ..
        }))
    ));
    assert_eq!(vault_bytes(&k.ctx), before);
}

/// B10, B75, B77: the glue's two helpers — the account password refused before the system
/// is asked when no slot could hold it (a short one is advice), and the default label, which
/// names the account and the computer, cut at a character boundary to the vault's 64 bytes,
/// with no control character.
#[test]
fn an_account_password_no_slot_holds_is_refused_before_the_check() {
    let check = |text: &str| check_account_password(&SecretString::new(text));
    let refused = |text: &str| match check(text) {
        Err(CoreError::Cli(CliError::AccountPasswordRefused { reason, .. })) => reason,
        other => panic!("{other:?}"),
    };
    assert_eq!(refused(""), AccountPasswordRefusal::NoPassword);
    assert_eq!(
        refused(&"a".repeat(1025)),
        AccountPasswordRefusal::Unusable {
            why: AccountPasswordUnusable::TooLong { max: 1024 }
        }
    );
    let shaped = RecoveryKey::generate(&mut OsRandom)
        .unwrap()
        .to_display()
        .expose()
        .to_string();
    assert_eq!(
        refused(&shaped),
        AccountPasswordRefusal::Unusable {
            why: AccountPasswordUnusable::RecoveryKeyShape
        }
    );
    assert_eq!(
        check("short").unwrap(),
        Some(PasswordAdvice::Short {
            chars: 5,
            recommended: 15
        })
    );
    assert_eq!(check(ACCOUNT_A).unwrap(), None);
    assert_eq!(
        account_password_label("walker", "box-a"),
        "account password of walker on box-a"
    );
    let long = account_password_label(&"é".repeat(40), "box-a");
    assert!(
        long.len() <= 64 && long.starts_with("account password of é"),
        "{long}"
    );
    assert_eq!(
        account_password_label("wal\nker", "box\u{7}"),
        "account password of wal?ker on box?"
    );
}

/// O-b8, B60: the account password opens `lock off` too — every vault write takes any slot's
/// fresh secret.
#[test]
fn the_account_password_opens_lock_off_too() {
    let k = Kit::new();
    add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xa,
        &k.recovery(),
        None,
        account(ACCOUNT_A, 0xa),
    );
    let plan = plan_off(&k.ctx).unwrap();
    let opener = open_with(&k.ctx, ACCOUNT_A, Some(0xa)).unwrap();
    let done = completed(execute_off(
        &k.ctx,
        plan,
        Some(opener),
        &NullReporter,
        &CancellationToken::new(),
    ));
    assert_eq!(done.tokens_restored, ["prod"]);
    assert!(!k.ctx.config_root().join(VAULT_FILE).exists());
}

/// `lock slot remove <given>` to its end, planned with the facts of computer 0xa and opened
/// with `opener` typed on `on`.
fn remove(ctx: &Context, given: &str, opener: &str, on: Option<u8>) -> SlotRemoved {
    let plan = plan_slot_remove(ctx, given, &FixedFacts::on(0xa)).unwrap();
    let opened = open_with(ctx, opener, on).unwrap();
    completed(execute_slot_remove(
        ctx,
        plan,
        opened,
        &NullReporter,
        &CancellationToken::new(),
    ))
}

/// The ids of `ctx`'s slots, from the report.
fn ids(ctx: &Context) -> Vec<String> {
    status(ctx, UnlockWindow::Closed, &quiet(), &UnknownFacts)
        .unwrap()
        .slots
        .into_iter()
        .map(|s| s.id.unwrap())
        .collect()
}

/// B21 (WI-498 acceptance 1): a slot goes by a kind the store has one of, or by 8 of its id's
/// characters; the plan (Destructive) says the keys stay and what opens the store after it;
/// the data key is unchanged; `store.yaml` mirrors the vault; no client is asked.
#[test]
fn a_slot_is_removed_by_its_kind_or_its_id_and_the_keys_stay() {
    let k = Kit::new();
    let password = add(
        &k.ctx,
        SlotAddKind::Password,
        0xa,
        &k.recovery(),
        None,
        lock_password(NEW_LOCK_PASSWORD),
    );
    add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xa,
        NEW_LOCK_PASSWORD,
        None,
        account(ACCOUNT_A, 0xa),
    );
    let plan = plan_slot_remove(&k.ctx, "os-password", &FixedFacts::on(0xa)).unwrap();
    assert_eq!(plan.class, PlanClass::Destructive);
    assert_eq!(
        plan.changes.iter().map(|c| c.action).collect::<Vec<_>>(),
        [ChangeAction::Delete, ChangeAction::Keep, ChangeAction::Keep]
    );
    let keeps = plan.changes[1].detail.as_deref().unwrap();
    assert!(
        keeps.contains("`apprafter lock rekey` replaces the keys")
            && keeps.contains("test guesses of that account password"),
        "{keeps}"
    );
    assert_eq!(
        plan.changes[2].detail.as_deref(),
        Some("the store then opens with: recovery key, lock password")
    );
    let opener = open_with(&k.ctx, NEW_LOCK_PASSWORD, None).unwrap();
    let done = completed(execute_slot_remove(
        &k.ctx,
        plan,
        opener,
        &NullReporter,
        &CancellationToken::new(),
    ));
    assert_eq!(done.removed.kind, LockSlotKind::OsPassword);
    assert_eq!(done.opened_with.kind, LockSlotKind::Password);
    assert_eq!(
        done.remains.iter().map(|s| s.kind).collect::<Vec<_>>(),
        [LockSlotKind::RecoveryKey, LockSlotKind::Password]
    );
    assert!(!opens(&k.ctx, ACCOUNT_A, Some(0xa)));
    // The data key did not change: the key held from before still reads the token.
    let reader = Context::for_desktop(k.ctx.config_root().to_path_buf(), "http://127.0.0.1:1")
        .with_vault_access(k.made.access());
    assert_eq!(
        reader
            .secrets()
            .stored_token("prod")
            .unwrap()
            .unwrap()
            .expose(),
        TOKEN
    );
    let plan = plan_slot_remove(&k.ctx, &password.added.id[..8], &FixedFacts::on(0xa)).unwrap();
    assert_eq!(plan.payload.slot(), &password.added);
    assert_eq!(
        plan.changes[2].detail.as_deref(),
        Some(
            "on this computer the store then opens only with the recovery key; it opens with: \
             recovery key"
        )
    );
    let opener = open_with(&k.ctx, &k.recovery(), None).unwrap();
    completed(execute_slot_remove(
        &k.ctx,
        plan,
        opener,
        &NullReporter,
        &CancellationToken::new(),
    ));
    assert_eq!(
        mirrored(&k.ctx),
        [(LockSlotKind::RecoveryKey, "recovery key".to_string())]
    );
    assert_eq!(k.asked.asked(), 0);
}

/// ADR 0068 §3, S2 (WI-498 acceptance 1): the store's last recovery key is never removed —
/// by its kind or by its id — refused by the plan, before anything is asked.
#[test]
fn the_recovery_key_is_never_removed() {
    let k = Kit::new();
    let before = vault_bytes(&k.ctx);
    let id = ids(&k.ctx).remove(0);
    for given in ["recovery-key", &id[..8]] {
        assert!(
            matches!(
                plan_slot_remove(&k.ctx, given, &UnknownFacts),
                Err(CoreError::Cli(CliError::SlotRefused {
                    reason: SlotRefusal::LastRecoverySlot,
                    ..
                }))
            ),
            "{given}"
        );
    }
    assert_eq!(vault_bytes(&k.ctx), before);
    assert_eq!(k.asked.asked(), 0);
}

/// B21, B29: a slot the store does not have — an id no slot begins with, fewer than 4
/// characters, a word that is no kind — or a kind it has two of is `slot_not_found`, listing
/// what it could mean.
#[test]
fn a_slot_the_store_lacks_or_two_it_could_mean_is_slot_not_found() {
    let k = Kit::new();
    let on_a = add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xa,
        &k.recovery(),
        None,
        account(ACCOUNT_A, 0xa),
    );
    let on_b = add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xb,
        &k.recovery(),
        None,
        account(ACCOUNT_B, 0xb),
    );
    let not_found = |given: &str| match plan_slot_remove(&k.ctx, given, &FixedFacts::on(0xa)) {
        Err(CoreError::Cli(CliError::SlotNotFound { matches, .. })) => matches,
        other => panic!("{given}: {other:?}"),
    };
    assert!(not_found("lock").is_empty());
    assert!(
        not_found(&on_a.added.id[..3]).is_empty(),
        "fewer than 4 characters name no slot"
    );
    let all = ids(&k.ctx);
    let unused = (0..=0xffffu32)
        .map(|n| format!("{n:04x}"))
        .find(|p| all.iter().all(|id| !id.starts_with(p.as_str())))
        .unwrap();
    assert!(not_found(&unused).is_empty());
    let mut both = not_found("os-password");
    both.sort();
    let mut want = vec![on_a.added.id.clone(), on_b.added.id.clone()];
    want.sort();
    assert_eq!(both, want);
    assert_eq!(k.asked.asked(), 0);
}

/// T63, B29 (WI-498 acceptance 1): on a hand-made vault without its recovery key, removing
/// its only lock password would leave no slot this build opens — `no_usable_slot_left`,
/// refused by the plan, before anything is asked.
#[test]
fn a_removal_that_leaves_no_slot_this_build_opens_is_refused_by_the_plan() {
    let k = Kit::new();
    add(
        &k.ctx,
        SlotAddKind::Password,
        0xa,
        &k.recovery(),
        None,
        lock_password(NEW_LOCK_PASSWORD),
    );
    let mut vault = crate::vault::VaultFile::parse(&vault_bytes(&k.ctx))
        .unwrap()
        .open_with_key(k.made.data_key(), k.made.vault_id)
        .unwrap();
    vault
        .header_mut()
        .slots
        .retain(|slot| slot.kind != crate::vault::SlotKind::RecoveryKey);
    let sealed = vault.seal("apprafter 0.0.0-test", &mut OsRandom).unwrap();
    let held = StoreLock::exclusive(&k.ctx.store()).unwrap();
    crate::vault::file::replace(&k.ctx.config_root().join(VAULT_FILE), &sealed, &held).unwrap();
    drop(held);
    assert!(matches!(
        plan_slot_remove(&k.ctx, "password", &UnknownFacts),
        Err(CoreError::Cli(CliError::SlotRefused {
            reason: SlotRefusal::NoUsableSlotLeft,
            ..
        }))
    ));
    assert_eq!(k.asked.asked(), 0);
}

/// B60 (WI-498 acceptance 2): a fresh secret of every kind opens a removal — the account
/// password, the lock password, the recovery key — and no holder is ever asked.
#[test]
fn every_kind_of_slot_opens_a_removal_and_no_holder_is_asked() {
    let k = Kit::new();
    add(
        &k.ctx,
        SlotAddKind::Password,
        0xa,
        &k.recovery(),
        None,
        lock_password(NEW_LOCK_PASSWORD),
    );
    add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xa,
        &k.recovery(),
        None,
        account(ACCOUNT_A, 0xa),
    );
    let on_b = add(
        &k.ctx,
        SlotAddKind::OsPassword,
        0xb,
        &k.recovery(),
        None,
        account(ACCOUNT_B, 0xb),
    );
    let by_account = remove(&k.ctx, &on_b.added.id[..8], ACCOUNT_A, Some(0xa));
    let by_password = remove(&k.ctx, "os-password", NEW_LOCK_PASSWORD, None);
    let by_recovery = remove(&k.ctx, "password", &k.recovery(), None);
    assert_eq!(
        [
            by_account.opened_with.kind,
            by_password.opened_with.kind,
            by_recovery.opened_with.kind
        ],
        [
            LockSlotKind::OsPassword,
            LockSlotKind::Password,
            LockSlotKind::RecoveryKey
        ]
    );
    assert_eq!(k.asked.asked(), 0);
}

/// B19: a slot another process removed between the plan and the lock is `slot_not_found`,
/// read under the lock; nothing is written.
#[test]
fn a_slot_removed_meanwhile_is_slot_not_found_under_the_lock() {
    let k = Kit::new();
    add(
        &k.ctx,
        SlotAddKind::Password,
        0xa,
        &k.recovery(),
        None,
        lock_password(NEW_LOCK_PASSWORD),
    );
    let plan = plan_slot_remove(&k.ctx, "password", &UnknownFacts).unwrap();
    let opener = open_with(&k.ctx, &k.recovery(), None).unwrap();
    remove(&k.ctx, "password", &k.recovery(), None);
    let between = vault_bytes(&k.ctx);
    assert!(matches!(
        execute_slot_remove(
            &k.ctx,
            plan,
            opener,
            &NullReporter,
            &CancellationToken::new()
        ),
        Err(CoreError::Cli(CliError::SlotNotFound { .. }))
    ));
    assert_eq!(vault_bytes(&k.ctx), between);
}

/// M3, B19 for a removal (critic round, finding 2): a token another command stores while
/// the removal waits for the store lock is kept.
#[test]
fn a_token_stored_while_a_removal_waits_for_the_lock_is_kept() {
    let k = Kit::new();
    add(
        &k.ctx,
        SlotAddKind::Password,
        0xa,
        &k.recovery(),
        None,
        lock_password(NEW_LOCK_PASSWORD),
    );
    let plan = plan_slot_remove(&k.ctx, "password", &UnknownFacts).unwrap();
    let opener = open_with(&k.ctx, &k.recovery(), None).unwrap();
    let (waiting, other) = other_writer(opened(&k.ctx, &k.recovery()), "prod", TOKEN_C);
    completed(execute_slot_remove(
        &k.ctx,
        plan,
        opener,
        &waiting,
        &CancellationToken::new(),
    ));
    other.join().unwrap();
    assert_eq!(token_of(&k.ctx, &k.recovery(), "prod"), TOKEN_C);
    assert!(!opens(&k.ctx, NEW_LOCK_PASSWORD, None));
}

/// §5.6, ADR 0069 §10 (critic round, finding 7; Deviation 23): a second recovery-key slot —
/// which no write of the core makes — goes by its id, leaving the store's one recovery key;
/// `recovery-key` names two slots until then (`slot_not_found`); the last one never goes.
#[test]
fn a_second_recovery_slot_goes_by_its_id_and_the_last_stays() {
    let k = Kit::new();
    let mut vault = crate::vault::VaultFile::parse(&vault_bytes(&k.ctx))
        .unwrap()
        .open_with_key(k.made.data_key(), k.made.vault_id)
        .unwrap();
    let spare = vault
        .forge_record(
            NewSlot::RecoveryKey {
                key: RecoveryKey::generate(&mut OsRandom).unwrap(),
                label: "spare".into(),
            },
            &KdfPolicy::INSECURE_FAST,
        )
        .unwrap();
    let spare_id = spare.id.to_string();
    vault.header_mut().slots.push(spare);
    let sealed = vault.seal("apprafter 0.0.0-test", &mut OsRandom).unwrap();
    let held = StoreLock::exclusive(&k.ctx.store()).unwrap();
    crate::vault::file::replace(&k.ctx.config_root().join(VAULT_FILE), &sealed, &held).unwrap();
    drop(held);
    assert!(matches!(
        plan_slot_remove(&k.ctx, "recovery-key", &UnknownFacts),
        Err(CoreError::Cli(CliError::SlotNotFound { ref matches, .. })) if matches.len() == 2
    ));
    remove(&k.ctx, &spare_id[..8], &k.recovery(), None);
    assert!(matches!(
        plan_slot_remove(&k.ctx, "recovery-key", &UnknownFacts),
        Err(CoreError::Cli(CliError::SlotRefused {
            reason: SlotRefusal::LastRecoverySlot,
            ..
        }))
    ));
    assert!(opens(&k.ctx, &k.recovery(), None));
}

/// B20, §9.2 c1: a fault before a removal's replace leaves the vault as it was; a removal
/// that does not read back is not kept.
#[test]
fn a_removal_that_faults_first_or_does_not_read_back_is_not_reported() {
    let k = Kit::new();
    add(
        &k.ctx,
        SlotAddKind::Password,
        0xa,
        &k.recovery(),
        None,
        lock_password(NEW_LOCK_PASSWORD),
    );
    let before = vault_bytes(&k.ctx);
    let ctx = k.ctx.clone().with_fault(FaultPoint::BeforeSlotWrite);
    let plan = plan_slot_remove(&ctx, "password", &UnknownFacts).unwrap();
    let opener = open_with(&ctx, &k.recovery(), None).unwrap();
    assert!(
        execute_slot_remove(&ctx, plan, opener, &NullReporter, &CancellationToken::new()).is_err()
    );
    assert_eq!(vault_bytes(&k.ctx), before);
    let old = before.clone();
    super::slots::AFTER_SLOT_WRITE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |root: &Path| {
            std::fs::write(root.join(VAULT_FILE), &old).unwrap()
        }));
    });
    let plan = plan_slot_remove(&k.ctx, "password", &UnknownFacts).unwrap();
    let opener = open_with(&k.ctx, &k.recovery(), None).unwrap();
    let result = execute_slot_remove(
        &k.ctx,
        plan,
        opener,
        &NullReporter,
        &CancellationToken::new(),
    );
    super::slots::AFTER_SLOT_WRITE.with(|hook| *hook.borrow_mut() = None);
    assert!(
        matches!(
            result,
            Err(CoreError::Cli(CliError::VaultUnreadable { .. }))
        ),
        "{result:?}"
    );
    assert!(
        opens(&k.ctx, NEW_LOCK_PASSWORD, None),
        "the vault on disk is the one from before"
    );
}
