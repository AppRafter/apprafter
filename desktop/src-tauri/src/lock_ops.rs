// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Security page's commands (D.4a §3.14): the report, and the store lock's three flows —
//! Protect (`lock on`), Finish (an interrupted `lock on` whose vault is written: rule 4, both
//! slots opened first; team decision 2026-10-13), Stop protecting (`lock off`, the store's
//! secret its gesture, O11). Each is a plan the page confirms, its secrets held in Rust: the
//! password in the plan's executor, the new recovery key in [`LockOnTable`] until the plan runs
//! or goes. The key reaches the page once, for display (`LockOnPlan`, T79), comes back only as
//! typed ([`confirm_recovery_key`], T25), and is copied by Rust ([`copy_recovery_key`], O12).
//!
//! Each flow's run takes the shell's `FlowGuard` as its executor's first statement — never at
//! plan time: a confirm left open must not switch off the re-read — and re-reads the store as
//! it ends: Protect then keeps the new vault's key, opened with the confirmed recovery key (T64;
//! not kept when a lock came between, the epoch), and Stop protecting leaves the store in
//! files, which the re-read follows (the key and the pins go).
//!
//! The new key is copied only by Rust and only until "I saved it" ([`recovery_key_saved`]):
//! then it comes off the clipboard and is typed back from where the person saved it, which is
//! the only state [`confirm_recovery_key`] accepts it in (O12).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use apprafter_core::lock::open::RecoveryKeyProblem;
use apprafter_core::lock::{self, HolderHardening, LockOnInput, LockOnStart, LockedOn};
use apprafter_core::secrets::{LockSlotKind, StoreId};
use apprafter_core::vault::{ConfirmedRecoveryKey, KdfPolicy, RecoveryKey, SlotInput};
use apprafter_core::{CoreError, PlanClass, SecretString};
use apprafter_desktop_ipc::{LockOffPlan, LockOnPlan, LockSectionInfo, OpId, PlanView};

use crate::app::Shell;
use crate::errors::{DesktopError, Refusal};
use crate::ops::{Executor, PlanParts, SecretGesture};
use crate::target_ops::json_outcome;
use crate::vault_access::{DesktopVaultAccess, KeyEpoch, OpenedSlot, StoreSecretGesture};

/// The new recovery keys of the Protect plans waiting to run, by plan. Held weakly: the plan's
/// executor holds each, so a plan that goes — run, discarded, expired, dropped by a lock —
/// takes its key with it (zeroed on drop).
#[derive(Default)]
pub struct LockOnTable(Mutex<HashMap<OpId, Weak<NewKey>>>);

impl LockOnTable {
    fn insert(&self, id: OpId, key: &Arc<NewKey>) {
        let mut keys = self.lock();
        keys.retain(|_, key| key.strong_count() > 0);
        keys.insert(id, Arc::downgrade(key));
    }

    fn get(&self, id: OpId) -> Result<Arc<NewKey>, DesktopError> {
        self.lock()
            .get(&id)
            .and_then(Weak::upgrade)
            .ok_or(DesktopError::PlanNotFound { op_id: id })
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<OpId, Weak<NewKey>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A new recovery key: shown (and copied, by Rust, on request), then saved — off the clipboard
/// and copied no more — then confirmed by being typed back in full (T25, O12), then spent by
/// its run. No `Debug`, no `Clone`.
pub struct NewKey(Mutex<KeyState>);

enum KeyState {
    Shown(RecoveryKey),
    Saved(RecoveryKey),
    Confirmed(ConfirmedRecoveryKey),
    Spent,
}

impl NewKey {
    fn new(key: RecoveryKey) -> Arc<Self> {
        Arc::new(NewKey(Mutex::new(KeyState::Shown(key))))
    }

    /// "I saved it": copied no more, and typed back from now on.
    fn saved(&self) {
        let mut state = self.lock();
        *state = match std::mem::replace(&mut *state, KeyState::Spent) {
            KeyState::Shown(key) => KeyState::Saved(key),
            other => other,
        };
    }

    /// `typed` is the key, in full, once it is saved: then it is confirmed. Before "I saved
    /// it" nothing is taken (O12: the app's own Copy must not answer for the person's copy). A
    /// refusal says why (`mismatch`, the group of a `checksum`), never what was typed, and the
    /// key stays saved. A retype after the confirm is checked as the first one was, and a wrong
    /// one leaves the key unconfirmed: the last retype decides, and no answer is `Ok` for a key
    /// that was not typed. A key its run took answers as a plan that is gone (`op_id`'s).
    fn confirm(&self, op_id: OpId, typed: &str) -> Result<(), DesktopError> {
        let mut state = self.lock();
        let key = match std::mem::replace(&mut *state, KeyState::Spent) {
            KeyState::Shown(key) => {
                *state = KeyState::Shown(key);
                return Err(refused(
                    "the recovery key is typed back only once it is saved",
                    "Choose I saved it, then type it from where you saved it.",
                ));
            }
            KeyState::Saved(key) => key,
            KeyState::Confirmed(confirmed) => confirmed.into_key(),
            KeyState::Spent => return Err(DesktopError::PlanNotFound { op_id }),
        };
        match key.confirm(typed) {
            Ok(confirmed) => {
                *state = KeyState::Confirmed(confirmed);
                Ok(())
            }
            Err((key, error)) => {
                *state = KeyState::Saved(key);
                Err(CoreError::RecoveryKeyInvalid {
                    reason: error.into(),
                }
                .into())
            }
        }
    }

    /// `f` over the key's text while it is shown, its state held all along (O12): "I saved it"
    /// waits for a Copy under way (the Rust verifier's finding 8). `None` once it is saved,
    /// confirmed or spent.
    fn with_shown<T>(&self, f: impl FnOnce(&SecretString) -> T) -> Option<T> {
        let state = self.lock();
        match &*state {
            KeyState::Shown(key) => Some(f(&key.to_display())),
            KeyState::Saved(_) | KeyState::Confirmed(_) | KeyState::Spent => None,
        }
    }

    /// The confirmed key, for the run; `None` until it is confirmed.
    fn take_confirmed(&self) -> Option<ConfirmedRecoveryKey> {
        let mut state = self.lock();
        match std::mem::replace(&mut *state, KeyState::Spent) {
            KeyState::Confirmed(key) => Some(key),
            other => {
                *state = other;
                None
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, KeyState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `lock_info`: the store re-read first — `locked` when that re-read locked the app (critic A
/// finding 21) — then the one report (T73) with this app's window and limit client, and the
/// process's own protections (T77), `memory_locked` the held key's page as the OS answered.
/// On the blocking pool: the limit rows may ask the ssh-agent (2 s) and wait for the session
/// watch.
///
/// F20: when the report fails while that re-read found the store's own problem — the one the
/// page shows in its own panel (`AppInfo.secrets.problem`: a lost vault, a store others can
/// change) — the answer is no report and this computer's limits all the same (rule 1; the
/// core's doctor shows them beside an unreadable vault too). Any other failure of the report is
/// the section's, said where the limits go: never masked (critic C-M-5).
pub fn lock_info(shell: &Shell) -> Result<LockSectionInfo, DesktopError> {
    let now = shell.recheck();
    if shell.lock.is_locked() {
        return Err(DesktopError::Locked);
    }
    let window = shell.window(&now);
    let client = shell.limit_client_later()();
    let facts = crate::vault_access::slot_facts();
    let (report, limits) = match lock::status(&shell.context, window.clone(), &client, &*facts) {
        Ok(report) => {
            let limits = report.limits.clone();
            (Some(report), limits)
        }
        Err(_) if now.problem.is_some() => (
            None,
            lock::limits::detect_limits(&shell.context, &client, &window),
        ),
        Err(e) => return Err(e.into()),
    };
    Ok(LockSectionInfo {
        report,
        limits,
        app: HolderHardening {
            memory_locked: shell.vault.memory_locked(),
            ..shell.app_hardening()
        },
    })
}

/// A refusal in words the page shows as they are.
fn refused(message: &str, help: &str) -> DesktopError {
    DesktopError::Core(CoreError::Cli(cli_core::CliError::UsageRefused {
        message: message.to_string(),
        help: help.to_string(),
    }))
}

/// Plan Protect (Bounded): the password checked (O2: only empty, too long or shaped like a
/// recovery key refuse; a short one is the page's warning), the plan read, a new recovery key
/// made. Refused where Protect cannot start over: a vault is written (Finish) or the store is
/// protected already.
pub fn plan_lock_on(
    shell: &Arc<Shell>,
    password: SecretString,
    timeout_minutes: Option<u32>,
) -> Result<LockOnPlan, DesktopError> {
    // A key left from before a terminal's `lock off` goes now, before the epoch is read below:
    // a later re-read would drop it, move the epoch, and cost Protect its new key (the final
    // verifier's finding 11).
    shell.recheck();
    lock::check_password(&password)?;
    // D.4a's Protect — a separate lock password — until d4b-g2b offers the ways (B58).
    let mut plan = lock::plan_on(
        &shell.context,
        timeout_minutes,
        lock::WayAsked::LockPassword,
    )?;
    match plan.payload.start() {
        LockOnStart::New { .. } => {}
        LockOnStart::Resume { .. } => {
            return Err(refused(
                "an earlier Protect wrote this target store's vault",
                "Finish it with the lock password and the recovery key, or stop protecting.",
            ))
        }
        LockOnStart::AlreadyLocked => {
            return Err(refused(
                "this target store's secrets are protected already",
                "The Security page shows what opens them.",
            ))
        }
    }
    // D.4b B56: the lock password checked as the first slot, and the recovery key drawn
    // only then — the same two steps Protect has always taken (until d4b-g2b's ways).
    let step = lock::first_slot(&mut plan, lock::FirstSecret::LockPassword(password))?;
    let (first, key) = match step {
        lock::FirstStep::Ready { first, recovery } => (first, recovery),
        lock::FirstStep::Mistake(e) => return Err(e.into()),
        // Only an account password's empty entry falls back (d4b-g2b asks it).
        lock::FirstStep::FellBack => {
            return Err(refused(
                "Protect's plan left the lock password while it was checked",
                "Start Protect again.",
            ));
        }
    };
    let shown = key.to_display();
    let new_key = NewKey::new(key);
    let epoch = shell.vault.epoch();
    let parts = PlanParts {
        changes: plan.changes.clone(),
        ..PlanParts::new(plan.class, plan.title.clone(), "protect the secrets of")
    };
    let ctx = shell.context.clone();
    let (vault, held, weak) = (shell.vault.clone(), new_key.clone(), Arc::downgrade(shell));
    let exec: Executor = Box::new(move |reporter, cancel| {
        // The run's guard, not the plan's (critic B finding 4).
        let guard = weak.upgrade().map(|shell| shell.flow());
        let result = (|| {
            let recovery = held.take_confirmed().ok_or(CoreError::RecoveryKeyInvalid {
                reason: RecoveryKeyProblem::NotConfirmed,
            })?;
            let again = recovery.to_display();
            let outcome = lock::execute_on(
                &ctx,
                plan,
                LockOnInput::New { first, recovery },
                reporter,
                cancel,
            )?;
            if let apprafter_core::Outcome::Completed { result } = &outcome {
                keep_new_key(&ctx, &vault, epoch, result, &again);
            }
            Ok(json_outcome(outcome))
        })();
        drop(guard);
        if let Some(shell) = weak.upgrade() {
            shell.recheck();
        }
        result
    });
    let view = shell.register_plan(parts, exec);
    shell.lock_ons.insert(view.op_id, &new_key);
    Ok(LockOnPlan {
        plan: view,
        recovery_key: shown.expose().to_string(),
    })
}

/// T64: the app keeps the key of the vault Protect just wrote, opened with the recovery key
/// the person confirmed (a recovery slot: no derivation) — unless a lock came since the plan
/// (`epoch`), or another run locked the store first (its vault is not this key's).
fn keep_new_key(
    ctx: &apprafter_core::Context,
    vault: &DesktopVaultAccess,
    epoch: KeyEpoch,
    done: &LockedOn,
    shown: &SecretString,
) {
    if done.already_locked {
        return;
    }
    let Some(id) = done.store_id.as_deref().and_then(StoreId::parse) else {
        return;
    };
    match keep_key_of(ctx, vault, epoch, id, shown) {
        NewVaultKey::Kept | NewVaultKey::NotOpened => {}
        NewVaultKey::LockedSince | NewVaultKey::Dropped => {
            tracing::info!("the app locked while Protect ran: the new vault's key is not kept");
        }
    }
}

/// What became of the new vault's key.
#[derive(Debug, PartialEq, Eq)]
enum NewVaultKey {
    Kept,
    /// A lock came since the plan: the vault was not even opened — no key is unwrapped only
    /// to be dropped (the final verifier's finding 13).
    LockedSince,
    /// Opened, and a lock came between the check and the hold: dropped at once.
    Dropped,
    NotOpened,
}

/// [`keep_new_key`] for the store `id`: the epoch is checked before the vault is opened, and
/// again under the access's lock as it is held (`hold`).
fn keep_key_of(
    ctx: &apprafter_core::Context,
    vault: &DesktopVaultAccess,
    epoch: KeyEpoch,
    id: StoreId,
    shown: &SecretString,
) -> NewVaultKey {
    if vault.epoch() != epoch {
        return NewVaultKey::LockedSince;
    }
    let opened = RecoveryKey::parse(shown.expose())
        .map_err(|e| CoreError::RecoveryKeyInvalid { reason: e.into() })
        .and_then(|key| {
            lock::open_at(
                ctx.config_root(),
                &id,
                SlotInput::RecoveryKey(key),
                KdfPolicy::active(),
            )
        });
    match opened {
        Ok(opened) => {
            if vault.hold(epoch, id, opened) {
                NewVaultKey::Kept
            } else {
                NewVaultKey::Dropped
            }
        }
        Err(e) => {
            tracing::warn!("the new vault did not open with its recovery key: {e}");
            NewVaultKey::NotOpened
        }
    }
}

/// The recovery key of plan `op_id`, typed back in full (T25).
pub fn confirm_recovery_key(
    shell: &Shell,
    op_id: OpId,
    typed: SecretString,
) -> Result<(), DesktopError> {
    shell.lock_ons.get(op_id)?.confirm(op_id, typed.expose())
}

/// Copy plan `op_id`'s recovery key from Rust (O12): only until it is saved. The lock count is
/// read first and the key's state held across the copy (the Rust verifier's finding 8): a lock
/// that lands meanwhile clears what this copies at once, and "I saved it" waits for it, then
/// takes it off.
pub fn copy_recovery_key(shell: &Shell, op_id: OpId) -> Result<(), DesktopError> {
    let locks = shell.clipboard.locks();
    let key = shell.lock_ons.get(op_id)?;
    key.with_shown(|shown| shell.clipboard.copy_since(shown, locks))
        .unwrap_or_else(|| {
            Err(refused(
                "the recovery key is not copied again once it is saved",
                "Type it from where you saved it.",
            ))
        })
}

/// "I saved it" for plan `op_id` (O12, critic B finding 3): the key comes off the clipboard if
/// it is still there, is copied no more, and is typed back from where the person saved it.
pub fn recovery_key_saved(shell: &Shell, op_id: OpId) -> Result<(), DesktopError> {
    shell.lock_ons.get(op_id)?.saved();
    shell.clipboard.clear_now();
    Ok(())
}

/// Plan finishing an interrupted Protect (Bounded): its vault is written, so both slots open
/// first, each by its own kind of secret, through the gate (rule 4).
pub fn plan_lock_finish(
    shell: &Arc<Shell>,
    password: SecretString,
    recovery: SecretString,
) -> Result<PlanView, Refusal> {
    let plan = lock::plan_on(&shell.context, None, lock::WayAsked::LockPassword)
        .map_err(DesktopError::from)?;
    let LockOnStart::Resume { id } = plan.payload.start().clone() else {
        return Err(refused(
            "there is no interrupted Protect to finish in this target store",
            "The Security page shows where its secrets are.",
        )
        .into());
    };
    let root = shell.context.config_root().to_path_buf();
    let password = shell
        .vault
        .open_slot(&root, &id, LockSlotKind::Password, password)?;
    let recovery = shell
        .vault
        .open_slot(&root, &id, LockSlotKind::RecoveryKey, recovery)?;
    let parts = PlanParts {
        changes: plan.changes.clone(),
        ..PlanParts::new(
            plan.class,
            plan.title.clone(),
            "finish protecting the secrets of",
        )
    };
    let ctx = shell.context.clone();
    let weak = Arc::downgrade(shell);
    let exec: Executor = Box::new(move |reporter, cancel| {
        let guard = weak.upgrade().map(|shell| shell.flow());
        let result = lock::execute_on(
            &ctx,
            plan,
            LockOnInput::Resume { password, recovery },
            reporter,
            cancel,
        );
        drop(guard);
        if let Some(shell) = weak.upgrade() {
            shell.recheck();
        }
        result.map(json_outcome)
    });
    Ok(shell.register_plan(parts, exec))
}

/// Plan Stop protecting. Destructive, its gesture the store's secret, whenever a vault must open
/// (O11: a fresh slot secret, never a window), and what the gesture opens is what
/// `execute_off` uses. A plan with nothing to open — a back-out with no vault, a finish past
/// the vault's deletion — moves nothing secret: a plain confirm (Bounded), as the CLI asks
/// nothing there, and never a plan a computer with no OS authenticator cannot confirm (critic
/// B finding 25).
pub fn plan_lock_off(shell: &Arc<Shell>) -> Result<LockOffPlan, DesktopError> {
    let plan = lock::plan_off(&shell.context)?;
    // The shared age key is read or created only for a cache (d4a-f Task 3): the dialog says so
    // only then.
    let needs_key = plan.payload.needs_key();
    let slot: OpenedSlot = Arc::default();
    let secret = if plan.payload.needs_secret() {
        let id = plan.payload.store_id().cloned().ok_or_else(|| {
            DesktopError::Internal("a lock-off plan that needs a secret names no store".into())
        })?;
        Some(Arc::new(StoreSecretGesture::new(
            shell.vault.clone(),
            shell.context.config_root().to_path_buf(),
            id,
            Some(slot.clone()),
        )) as Arc<dyn SecretGesture>)
    } else {
        None
    };
    let class = if secret.is_some() {
        plan.class
    } else {
        PlanClass::Bounded
    };
    let parts = PlanParts {
        changes: plan.changes.clone(),
        ..PlanParts::new(class, plan.title.clone(), "stop protecting the secrets of")
    };
    let ctx = shell.context.clone();
    let weak = Arc::downgrade(shell);
    let exec: Executor = Box::new(move |reporter, cancel| {
        let guard = weak.upgrade().map(|shell| shell.flow());
        let opened = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        let result = lock::execute_off(&ctx, plan, opened, reporter, cancel);
        drop(guard);
        if let Some(shell) = weak.upgrade() {
            shell.recheck();
        }
        result.map(json_outcome)
    });
    let plan = match secret {
        Some(secret) => shell.register_plan_with(parts, exec, Some(secret)),
        None => shell.register_plan(parts, exec),
    };
    Ok(LockOffPlan { plan, needs_key })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use apprafter_core::lock::{MachineLimit, UnlockWindow};
    use apprafter_core::secrets::testing::locked_store;
    use apprafter_core::secrets::{SecretsMode, VaultAccess};
    use apprafter_core::vault::OsRandom;
    use apprafter_core::{Context, Outcome, PathSource, SecretString};
    use apprafter_desktop_ipc::{errors, Gesture, LockReason, OpEvent, PlanView, Settings};
    use cli_core::{Target, TargetConfig, TargetCredentials};
    use zeroize::Zeroizing;

    use super::*;
    use crate::app::{Shell, ShellExtras};
    use crate::auth::NoAuthenticator;
    use crate::env::ToolSearchPath;
    use crate::ops::{EventSink, SystemClock};
    use crate::protection::Protection;
    use crate::settings::SettingsStore;

    const TOKEN: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
    const PASSWORD: &str = "correct horse battery staple";

    struct Rig {
        _dir: tempfile::TempDir,
        shell: Arc<Shell>,
    }

    /// The shell, unlocked, over a store at `<tmp>/store` — in files with target `prod` and
    /// its token, unless `prepare` makes something else there. Its home is the tempdir, so
    /// `lock off`'s shared age key is `<tmp>/.config/apprafter/age.key` (critic A finding 5);
    /// its weak-KDF fence is the tempdir too, unless `fenced` is false (only a test build
    /// creates a password slot under it).
    fn rig_in(prepare: impl FnOnce(&Path), extras: ShellExtras, fenced: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = Context::for_desktop(root.clone(), "http://127.0.0.1:9")
            .with_home_dir(Some(dir.path().to_path_buf()));
        let ctx = if fenced {
            ctx.with_insecure_kdf_root(dir.path().to_path_buf())
        } else {
            ctx
        };
        let target = Target {
            name: "prod".into(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                ..Default::default()
            },
            credentials: TargetCredentials {
                hetzner_token: Some(TOKEN.into()),
            },
        };
        cli_core::save_target(&ctx.store(), &target).unwrap();
        prepare(&root);
        let settings = SettingsStore::load(dir.path(), &SystemClock);
        settings
            .set(Settings {
                lock_on_start: false,
                ..Settings::default()
            })
            .unwrap();
        let shell = Shell::new_with(
            settings,
            Arc::new(NoAuthenticator),
            Arc::new(SystemClock),
            ctx,
            ToolSearchPath::known(Default::default(), PathSource::Explicit),
            false,
            extras,
            |_| {},
        );
        Rig { _dir: dir, shell }
    }

    fn rig_with(prepare: impl FnOnce(&Path), extras: ShellExtras) -> Rig {
        rig_in(prepare, extras, true)
    }

    fn rig() -> Rig {
        rig_with(|_| {}, ShellExtras::default())
    }

    /// A pager that says every page locked: the held key's page, as lock_info reports it.
    struct Takes;

    impl crate::vault_access::Pager for Takes {
        fn lock(&self, _: &[u8]) -> bool {
            true
        }
        fn unlock(&self, _: &[u8]) {}
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<OpEvent>>);

    impl EventSink for Recorder {
        fn send(&self, event: &OpEvent) -> bool {
            self.0.lock().unwrap().push(event.clone());
            true
        }
        fn webview(&self) -> &str {
            "main"
        }
    }

    /// Runs `view`, with the store's secret `secret` when its gesture asks it; its last event.
    fn run(shell: &Shell, view: &PlanView, secret: Option<&str>) -> OpEvent {
        let sink = Arc::new(Recorder::default());
        shell
            .execute_with(
                view.op_id,
                sink.clone(),
                secret.map(|s| Zeroizing::new(s.to_owned())),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(end @ (OpEvent::Finished { .. } | OpEvent::Failed { .. })) =
                sink.0.lock().unwrap().last().cloned()
            {
                return end;
            }
            assert!(
                Instant::now() < deadline,
                "operation {:?} did not end",
                view.op_id
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn completed(event: OpEvent) -> serde_json::Value {
        match event {
            OpEvent::Finished {
                outcome: Outcome::Completed { result },
            } => result,
            other => panic!("not completed: {other:?}"),
        }
    }

    #[test]
    fn lock_info_reads_the_store_and_says_what_the_app_did() {
        let r = rig();
        let info = lock_info(&r.shell).unwrap();
        let report = info.report.expect("a store in files has its report");
        assert_eq!(report.mode, SecretsMode::Files);
        assert_eq!(report.window, UnlockWindow::Closed);
        assert_eq!(info.limits, report.limits);
        assert_eq!(info.app, crate::harden::not_applied());
    }

    /// WI-476: Protect refused on a protected store points to where what opens it is shown — the
    /// Security page. (The page shows the message only: the projection gives `UsageRefused` no
    /// help, the hand-off's F1; the words are checked where they live, in the diagnostic.)
    #[test]
    fn protect_on_a_protected_store_says_where_what_opens_it_is() {
        let mut made = None;
        let r = rig_with(
            |root| made = Some(locked_store(root, &[])),
            ShellExtras::default(),
        );
        let store = made.unwrap();
        r.shell
            .unlock_with_password(Zeroizing::new(store.recovery_key().expose().into()))
            .unwrap();
        let e = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap_err();
        assert!(
            e.to_ui().message.contains("protected already"),
            "{:?}",
            e.to_ui()
        );
        let help = miette::Diagnostic::help(&e).map(|h| h.to_string());
        assert_eq!(
            help.as_deref(),
            Some("The Security page shows what opens them.")
        );
    }

    #[test]
    fn lock_info_names_the_app_s_window_while_it_holds_the_key() {
        let mut made = None;
        let r = rig_with(
            |root| made = Some(locked_store(root, &[])),
            ShellExtras::default(),
        );
        let store = made.unwrap();
        r.shell
            .unlock_with_password(Zeroizing::new(store.recovery_key().expose().into()))
            .unwrap();
        let report = lock_info(&r.shell).unwrap().report.unwrap();
        assert_eq!(report.mode, SecretsMode::Locked);
        assert_eq!(report.window, UnlockWindow::App);
    }

    /// T77 (critic A finding 6, B 5): `memory_locked` is the held key's page as the OS answered
    /// — here a pager that says it took — never the process report's `false`.
    #[test]
    fn lock_info_reports_the_held_key_s_page() {
        let mut made = None;
        let r = rig_with(
            |root| made = Some(locked_store(root, &[])),
            ShellExtras {
                pager: Arc::new(Takes),
                ..ShellExtras::default()
            },
        );
        let store = made.unwrap();
        r.shell
            .unlock_with_password(Zeroizing::new(store.recovery_key().expose().into()))
            .unwrap();
        assert!(lock_info(&r.shell).unwrap().app.memory_locked);
    }

    /// F20 (D-9): a protected store whose vault was moved aside after the unlock. The page
    /// shows that problem in its own panel (`AppInfo.secrets.problem`); lock_info answers no
    /// report, and this computer's limits all the same (rule 1, as the core's doctor shows them
    /// beside an unreadable vault) — never the vault's error a second time.
    #[test]
    fn lock_info_shows_this_computer_s_limits_beside_a_lost_vault() {
        let mut made = None;
        let r = rig_with(
            |root| made = Some(locked_store(root, &[])),
            ShellExtras::default(),
        );
        let store = made.unwrap();
        r.shell
            .unlock_with_password(Zeroizing::new(store.recovery_key().expose().into()))
            .unwrap();
        let root = r.shell.context.config_root().to_path_buf();
        std::fs::rename(
            root.join(apprafter_core::vault::VAULT_FILE),
            root.join("vault.aside"),
        )
        .unwrap();
        let problem = Protection::read(&root, None).problem;
        assert!(problem.is_some(), "the page's own panel says why");
        let info = lock_info(&r.shell).expect("the limits are shown beside a lost vault");
        assert!(info.report.is_none(), "{:?}", info.report);
        assert!(
            info.limits.contains(&MachineLimit::SameUserPrograms)
                && info.limits.contains(&MachineLimit::Hibernation),
            "{:?}",
            info.limits
        );
    }

    /// F20 (critic C-M-5): a report that fails for anything but the store's own problem is the
    /// section's error, shown where the limits go as before — never masked. Here the vault is
    /// still there but no longer reads as one after the unlock: the store read (its directory
    /// entry only) finds no problem, and the report's slots cannot be read.
    #[test]
    fn lock_info_keeps_any_other_failure_of_the_report() {
        let mut made = None;
        let r = rig_with(
            |root| made = Some(locked_store(root, &[])),
            ShellExtras::default(),
        );
        let store = made.unwrap();
        r.shell
            .unlock_with_password(Zeroizing::new(store.recovery_key().expose().into()))
            .unwrap();
        let root = r.shell.context.config_root().to_path_buf();
        std::fs::write(root.join(apprafter_core::vault::VAULT_FILE), b"not a vault").unwrap();
        assert_eq!(Protection::read(&root, None).problem, None);
        let e = lock_info(&r.shell).expect_err("the report's failure is the section's");
        assert!(!matches!(e, DesktopError::Locked), "{e:?}");
    }

    /// Critic A finding 21: when its re-read locks the app, lock_info answers `locked`, not a
    /// report for an app that is now locked.
    #[test]
    fn lock_info_refuses_once_its_re_read_locked_the_app() {
        let r = rig();
        locked_store(r.shell.context.config_root(), &[]);
        let e = lock_info(&r.shell).unwrap_err();
        assert!(matches!(e, DesktopError::Locked), "{e:?}");
    }

    /// Critic B finding 4: a flow's guard is its run's — while a Protect plan waits for its
    /// confirm, a store locked from a terminal still locks the app.
    #[test]
    fn a_plan_waiting_for_its_confirm_does_not_stop_the_re_read() {
        let r = rig();
        let _plan = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap();
        locked_store(r.shell.context.config_root(), &[]);
        r.shell.app_info();
        assert_eq!(r.shell.lock.state().reason, Some(LockReason::SecretsLocked));
    }

    /// T25, critic C5: the plan runs only with its key typed back; until then nothing moves.
    #[test]
    fn a_protect_plan_runs_only_with_its_recovery_key_typed_back() {
        let r = rig();
        let plan = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap();
        assert!(plan.recovery_key.starts_with("APPRAFTER-RECOVERY-KEY-1"));
        assert_eq!(plan.plan.gesture, Gesture::None);
        let event = run(&r.shell, &plan.plan, None);
        let OpEvent::Failed { error } = event else {
            panic!("ran: {event:?}")
        };
        assert_eq!(
            error.code.as_deref(),
            Some("apprafter::secrets::recovery_key_invalid")
        );
        assert_eq!(error.fields["reason"], "not_confirmed");
        let root = r.shell.context.config_root();
        assert_eq!(Protection::read(root, None).mode, SecretsMode::Files);
    }

    #[test]
    fn the_key_is_confirmed_only_as_shown_once_saved_and_goes_with_its_plan() {
        let r = rig();
        let plan = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap();
        let id = plan.plan.op_id;
        // Before "I saved it", not even the right key is taken (O12).
        let early =
            confirm_recovery_key(&r.shell, id, SecretString::new(plan.recovery_key.clone()))
                .unwrap_err();
        assert!(
            early.to_ui().message.contains("only once it is saved"),
            "{:?}",
            early.to_ui()
        );
        recovery_key_saved(&r.shell, id).unwrap();
        let wrong =
            confirm_recovery_key(&r.shell, id, SecretString::new("not the key")).unwrap_err();
        assert_eq!(
            wrong.to_ui().code.as_deref(),
            Some("apprafter::secrets::recovery_key_invalid")
        );
        confirm_recovery_key(&r.shell, id, SecretString::new(plan.recovery_key.clone())).unwrap();
        r.shell.ops.discard(id);
        let gone = confirm_recovery_key(&r.shell, id, SecretString::new(plan.recovery_key.clone()))
            .unwrap_err();
        assert_eq!(gone.to_ui().code.as_deref(), Some(errors::PLAN_NOT_FOUND));
        assert!(copy_recovery_key(&r.shell, id).is_err());
    }

    #[test]
    fn an_empty_or_key_shaped_password_is_refused_and_a_short_one_is_not() {
        let r = rig();
        for refused in ["", "APPRAFTER-RECOVERY-KEY-1 QQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK"] {
            let e = plan_lock_on(&r.shell, SecretString::new(refused), None).unwrap_err();
            assert_eq!(
                e.to_ui().code.as_deref(),
                Some("apprafter::secrets::password_refused"),
                "{refused:?}"
            );
        }
        assert!(plan_lock_on(&r.shell, SecretString::new("short"), None).is_ok());
    }

    /// O12 (critic B finding 3): Copy is Rust's; "I saved it" takes the key off the clipboard
    /// and ends the copies, so what is typed back comes from where the person saved it.
    #[test]
    fn copy_sends_the_key_from_rust_and_saved_takes_it_off_for_good() {
        let board = Arc::new(Mutex::new(None::<String>));
        let maker: crate::secret_clip::ClipboardMaker = {
            let board = board.clone();
            Arc::new(move || {
                struct Board(Arc<Mutex<Option<String>>>);
                impl crate::secret_clip::ClipboardBackend for Board {
                    fn set_concealed(&mut self, text: &str) -> Result<(), String> {
                        *self.0.lock().unwrap() = Some(text.into());
                        Ok(())
                    }
                    fn text(&mut self) -> Result<Option<Zeroizing<String>>, String> {
                        Ok(self.0.lock().unwrap().clone().map(Zeroizing::new))
                    }
                    fn clear(&mut self) -> Result<(), String> {
                        *self.0.lock().unwrap() = None;
                        Ok(())
                    }
                }
                Ok(Box::new(Board(board.clone())) as Box<dyn crate::secret_clip::ClipboardBackend>)
            })
        };
        let r = rig_with(
            |_| {},
            ShellExtras {
                clipboard: Some(maker),
                ..ShellExtras::default()
            },
        );
        let plan = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap();
        copy_recovery_key(&r.shell, plan.plan.op_id).unwrap();
        assert_eq!(
            board.lock().unwrap().as_deref(),
            Some(plan.recovery_key.as_str())
        );
        recovery_key_saved(&r.shell, plan.plan.op_id).unwrap();
        assert_eq!(
            *board.lock().unwrap(),
            None,
            "saved left the key on the clipboard"
        );
        let again = copy_recovery_key(&r.shell, plan.plan.op_id).unwrap_err();
        assert!(
            again.to_ui().message.contains("not copied again"),
            "{:?}",
            again.to_ui()
        );
        assert_eq!(*board.lock().unwrap(), None);
    }

    /// Critic B finding 25: a back-out with nothing to open — an interrupted Protect that wrote
    /// no vault — is a plain confirm: nothing secret moves, and no OS authenticator is needed.
    #[test]
    fn a_back_out_with_nothing_to_open_is_a_plain_confirm() {
        let r = rig_with(
            |root| {
                locked_store(root, &[]);
                std::fs::remove_file(root.join("vault")).unwrap();
                let file = root.join("store.yaml");
                let text = std::fs::read_to_string(&file).unwrap();
                std::fs::write(
                    &file,
                    text.replacen("secrets: locked", "secrets: locking", 1),
                )
                .unwrap();
            },
            ShellExtras::default(),
        );
        let off = plan_lock_off(&r.shell).unwrap().plan;
        assert_eq!(off.gesture, Gesture::None);
        completed(run(&r.shell, &off, None));
        assert_eq!(
            Protection::read(r.shell.context.config_root(), None).mode,
            SecretsMode::Files
        );
    }

    #[test]
    fn finish_is_refused_where_nothing_was_interrupted_and_opens_both_slots_first() {
        let r = rig();
        let e = plan_lock_finish(
            &r.shell,
            SecretString::new(PASSWORD),
            SecretString::new("k"),
        )
        .unwrap_err();
        assert!(
            e.to_ui().message.contains("no interrupted Protect"),
            "{:?}",
            e.to_ui()
        );
        // WI-476: where the page shows the store's state. The help lives in the diagnostic: the
        // projection gives `UsageRefused` none (the hand-off's F1).
        let help = miette::Diagnostic::help(&*e.error).map(|h| h.to_string());
        assert_eq!(
            help.as_deref(),
            Some("The Security page shows where its secrets are.")
        );
        // An interrupted Protect with its vault: the test store has a recovery slot only, so
        // the lock password opens nothing — refused through the gate, before any plan.
        let mut made = None;
        let r = rig_with(
            |root| {
                made = Some(locked_store(root, &[]));
                let file = root.join("store.yaml");
                let text = std::fs::read_to_string(&file).unwrap();
                assert!(text.contains("secrets: locked"), "{text}");
                std::fs::write(
                    &file,
                    text.replacen("secrets: locked", "secrets: locking", 1),
                )
                .unwrap();
            },
            ShellExtras::default(),
        );
        let store = made.unwrap();
        r.shell
            .unlock_with_password(Zeroizing::new(store.recovery_key().expose().into()))
            .unwrap();
        let e = plan_lock_finish(
            &r.shell,
            SecretString::new(PASSWORD),
            store.recovery_key().clone(),
        )
        .unwrap_err();
        assert!(
            matches!(*e.error, crate::errors::DesktopError::AuthFailed { .. }),
            "{e:?}"
        );
    }

    /// T64, O11: Protect keeps the new vault's key and the app stays unlocked; Stop protecting
    /// asks the store's secret and puts everything back. The fast KDF only (a password slot).
    #[cfg(feature = "test-build")]
    #[test]
    fn protect_then_stop_protecting_end_to_end() {
        let r = rig();
        let plan = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap();
        recovery_key_saved(&r.shell, plan.plan.op_id).unwrap();
        // The person went to save the key: the window lost and regained the focus, the page
        // read app_info. Re-reads of the unchanged store move no epoch (the Rust verifier's
        // finding 2), so the new key is still kept below.
        r.shell.app_info();
        r.shell.recheck();
        confirm_recovery_key(
            &r.shell,
            plan.plan.op_id,
            SecretString::new(plan.recovery_key.clone()),
        )
        .unwrap();
        completed(run(&r.shell, &plan.plan, None));
        let root = r.shell.context.config_root().to_path_buf();
        let now = Protection::read(&root, None);
        assert!(now.protected && now.mode == SecretsMode::Locked);
        assert!(
            r.shell.vault.holds(now.id.as_ref().unwrap()),
            "T64: the new key is kept"
        );
        assert!(!r.shell.lock.is_locked() && r.shell.lock.protected());

        let off = plan_lock_off(&r.shell).unwrap();
        // No cached secret in this store: no shared age key is used (d4a-f Task 3).
        assert!(!off.needs_key);
        let off = off.plan;
        assert_eq!(off.gesture, Gesture::StoreSecret);
        let refused = r.shell.execute_with(
            off.op_id,
            Arc::new(Recorder::default()),
            Some(Zeroizing::new("not it".into())),
        );
        assert!(refused.is_err(), "a wrong secret runs nothing");
        let done = completed(run(&r.shell, &off, Some(PASSWORD)));
        let now = Protection::read(&root, None);
        assert_eq!(now.mode, SecretsMode::Files);
        assert!(!r.shell.lock.protected());
        // With no cache the shared age key is neither read nor created (d4a-f Task 3), and the
        // outcome names none; where it would go is the rig's home, never outside the tempdir.
        assert!(done["ageKey"].is_null(), "{done}");
        assert!(!r._dir.path().join(".config/apprafter/age.key").exists());
        assert_eq!(
            r.shell.context.age_key_path(),
            r._dir.path().join(".config/apprafter/age.key")
        );
    }

    /// The last event of operation `id` once it has ended, from its replay: a lock drops every
    /// page's subscription, so a run the lock passed tells no sink; the page reads the replay
    /// after the unlock.
    #[cfg(feature = "test-build")]
    fn ended_in_replay(shell: &Shell, id: OpId) -> OpEvent {
        let deadline = Instant::now() + Duration::from_secs(30);
        while shell
            .ops
            .list()
            .iter()
            .any(|s| s.op_id == id && s.state == apprafter_desktop_ipc::OpState::Running)
        {
            assert!(Instant::now() < deadline, "operation {id:?} did not end");
            std::thread::sleep(Duration::from_millis(5));
        }
        let replay = shell
            .ops
            .subscribe(id, Arc::new(Recorder::default()))
            .expect("the ended operation is kept for the page")
            .replay;
        replay
            .last()
            .cloned()
            .expect("an ended operation has its final event")
    }

    /// D.4a F18 (owner O15, A1; the owner walk's D-7 runs 1 and 4): Stop protecting confirmed
    /// with its secret, and Lock now while the gate still derives that secret's key. The lock
    /// does not refuse the typed secret: judged on its answer, the change runs with the vault its
    /// gesture opened and finishes — the store is in files, its vault gone — and its end waits
    /// for the page in the operation's replay. The lock is forced inside the derivation (the
    /// gate's test seam), never sampled.
    #[cfg(feature = "test-build")]
    #[test]
    fn stop_protecting_confirmed_before_a_lock_finishes_after_it() {
        let r = rig();
        let plan = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap();
        recovery_key_saved(&r.shell, plan.plan.op_id).unwrap();
        confirm_recovery_key(
            &r.shell,
            plan.plan.op_id,
            SecretString::new(plan.recovery_key.clone()),
        )
        .unwrap();
        completed(run(&r.shell, &plan.plan, None));
        let root = r.shell.context.config_root().to_path_buf();
        let off = plan_lock_off(&r.shell).unwrap().plan;
        assert_eq!(off.gesture, Gesture::StoreSecret);
        let (began_tx, began) = std::sync::mpsc::channel();
        let (go, held) = std::sync::mpsc::channel::<()>();
        let (began_tx, held) = (Mutex::new(began_tx), Mutex::new(held));
        r.shell.vault.hold_openings(move || {
            let _ = began_tx.lock().unwrap().send(());
            let _ = held.lock().unwrap().recv_timeout(Duration::from_secs(30));
        });
        let (shell, id) = (r.shell.clone(), off.op_id);
        let confirmed = std::thread::spawn(move || {
            shell
                .execute_with(
                    id,
                    Arc::new(Recorder::default()),
                    Some(Zeroizing::new(PASSWORD.into())),
                )
                .map(drop)
                .map_err(|refusal| refusal.to_ui())
        });
        began
            .recv_timeout(Duration::from_secs(30))
            .expect("the gate derives the typed secret's key");
        assert!(r.shell.lock_now().locked, "Lock now locks the app");
        go.send(()).unwrap();
        confirmed
            .join()
            .unwrap()
            .expect("the typed secret is judged, and the plan runs");
        completed(ended_in_replay(&r.shell, id));
        assert_eq!(Protection::read(&root, None).mode, SecretsMode::Files);
        assert!(
            !root.join(apprafter_core::vault::VAULT_FILE).exists(),
            "the vault is gone"
        );
    }

    /// L5's desktop half (T23, critic B finding 11): a test build without its fence refuses
    /// Protect before anything is written.
    #[cfg(feature = "test-build")]
    #[test]
    fn without_its_fence_a_test_build_refuses_protect() {
        let r = rig_in(|_| {}, ShellExtras::default(), false);
        let e = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap_err();
        assert_eq!(
            e.to_ui().code.as_deref(),
            Some("apprafter::secrets::insecure_kdf_refused")
        );
        let root = r.shell.context.config_root();
        assert!(!root.join("vault").exists() && !root.join("store.yaml").exists());
    }

    /// The final verifier's finding 11: a key the app still holds from before a terminal's
    /// `lock off` goes before Protect reads the epoch, so the re-read that follows (a focus,
    /// `app_info`) cannot cost Protect its new key (T64).
    #[cfg(feature = "test-build")]
    #[test]
    fn a_key_left_from_a_terminal_s_lock_off_goes_before_protect_reads_the_epoch() {
        let mut made = None;
        let r = rig_with(
            |root| made = Some(locked_store(root, &[])),
            ShellExtras::default(),
        );
        let store = made.unwrap();
        r.shell
            .unlock_with_password(Zeroizing::new(store.recovery_key().expose().into()))
            .unwrap();
        // A terminal's `lock off` the app has not re-read yet: the store is in files again.
        let root = r.shell.context.config_root().to_path_buf();
        std::fs::remove_file(root.join(apprafter_core::vault::VAULT_FILE)).unwrap();
        std::fs::remove_file(root.join("store.yaml")).unwrap();
        assert!(
            r.shell.vault.holds(&store.store_id),
            "the stale key is still held"
        );
        let plan = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap();
        recovery_key_saved(&r.shell, plan.plan.op_id).unwrap();
        r.shell.app_info();
        confirm_recovery_key(
            &r.shell,
            plan.plan.op_id,
            SecretString::new(plan.recovery_key.clone()),
        )
        .unwrap();
        completed(run(&r.shell, &plan.plan, None));
        let now = Protection::read(&root, None);
        assert!(
            r.shell.vault.holds(now.id.as_ref().unwrap()),
            "T64: the new key is kept"
        );
        assert!(!r.shell.lock.is_locked());
    }

    /// The final verifier's finding 13: after a lock, Protect's run does not even open the new
    /// vault — its key would only be dropped.
    #[test]
    fn after_a_lock_protect_does_not_open_the_new_vault() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[]);
        let ctx = Context::for_desktop(dir.path().to_path_buf(), "http://127.0.0.1:9");
        let vault = DesktopVaultAccess::new(Arc::new(SystemClock));
        let shown = store.recovery_key().clone();
        let epoch = vault.epoch();
        vault.forget();
        assert_eq!(
            keep_key_of(&ctx, &vault, epoch, store.store_id.clone(), &shown),
            NewVaultKey::LockedSince
        );
        assert!(!vault.holds(&store.store_id));
        let epoch = vault.epoch();
        assert_eq!(
            keep_key_of(&ctx, &vault, epoch, store.store_id.clone(), &shown),
            NewVaultKey::Kept
        );
        assert!(vault.holds(&store.store_id));
    }

    /// Rule 4: an interrupted Protect finishes with both slots opened first.
    #[cfg(feature = "test-build")]
    #[test]
    fn an_interrupted_protect_finishes_with_both_secrets() {
        let r = rig();
        let plan = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap();
        let key = plan.recovery_key.clone();
        recovery_key_saved(&r.shell, plan.plan.op_id).unwrap();
        confirm_recovery_key(&r.shell, plan.plan.op_id, SecretString::new(key.clone())).unwrap();
        completed(run(&r.shell, &plan.plan, None));
        let file = r.shell.context.config_root().join("store.yaml");
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(
            &file,
            text.replacen("secrets: locked", "secrets: locking", 1),
        )
        .unwrap();
        let finish = plan_lock_finish(
            &r.shell,
            SecretString::new(PASSWORD),
            SecretString::new(key),
        )
        .unwrap();
        completed(run(&r.shell, &finish, None));
        assert_eq!(
            Protection::read(r.shell.context.config_root(), None).mode,
            SecretsMode::Locked
        );
    }

    /// O12, T25 (beyond the plan): a retype after the confirm is checked as the first one was —
    /// a wrong one is refused and leaves the key unconfirmed, so the last retype decides and the
    /// plan does not run on an earlier one.
    #[test]
    fn a_retype_after_the_confirm_is_checked_again() {
        let r = rig();
        let plan = plan_lock_on(&r.shell, SecretString::new(PASSWORD), None).unwrap();
        let id = plan.plan.op_id;
        recovery_key_saved(&r.shell, id).unwrap();
        confirm_recovery_key(&r.shell, id, SecretString::new(plan.recovery_key.clone())).unwrap();
        let wrong =
            confirm_recovery_key(&r.shell, id, SecretString::new("not the key")).unwrap_err();
        assert_eq!(
            wrong.to_ui().code.as_deref(),
            Some("apprafter::secrets::recovery_key_invalid")
        );
        let event = run(&r.shell, &plan.plan, None);
        let OpEvent::Failed { error } = event else {
            panic!("ran: {event:?}")
        };
        assert_eq!(error.fields["reason"], "not_confirmed");
    }

    /// Beyond the plan: a key its run has taken is typed back no more — the plan is no longer
    /// waiting, as `confirm` says of a plan that is gone.
    #[test]
    fn a_key_its_run_took_is_typed_back_no_more() {
        let key = RecoveryKey::generate(&mut OsRandom).unwrap();
        let shown = key.to_display();
        let new_key = NewKey::new(key);
        new_key.saved();
        new_key.confirm(OpId(7), shown.expose()).unwrap();
        assert!(new_key.take_confirmed().is_some());
        let e = new_key.confirm(OpId(7), shown.expose()).unwrap_err();
        assert!(
            matches!(e, DesktopError::PlanNotFound { op_id: OpId(7) }),
            "{e:?}"
        );
    }
}
