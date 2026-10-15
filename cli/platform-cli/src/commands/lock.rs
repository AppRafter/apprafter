// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter lock on` and `apprafter lock off` (ADR 0068 §5; D.4a T36, T37, T60; overview
//! §3.11): the two leaves over `apprafter_core::lock`. The core plans and executes; this module
//! shows the plan, holds the dialogue at the controlling terminal and prints the result. Every
//! secret is asked, and the recovery key shown, at the terminal itself ([`Asker`]) — never on
//! stdout or stderr, which a log or a pipe may keep: the new lock password twice (O2), the
//! recovery key once and typed back whole (O12), the lock password and the recovery key for a
//! resume, each opening its own slot (rule 4; team decision 2026-10-13), one slot's secret for
//! `lock off` (asked fresh, O11).

use std::path::Path;

use apprafter_core::lock::text;
use apprafter_core::lock::{
    self, check_password, HolderUnavailableReason, LockOffPayload, LockOffStart, LockOnInput,
    LockOnPayload, LockOnStart, LockReport, LockedOff, LockedOn, MachineLimit, PasswordAdvice,
    PasswordProblem, RecoveryKeyProblem, SlotOpened, SlotRefusalView, SlotView, UnlockWindow,
};
use apprafter_core::secrets::{LockSlotKind, SecretsMode, StoreId, STORE_FILE};
use apprafter_core::vault::{
    ConfirmedRecoveryKey, KdfPolicy, OsRandom, RecoveryKey, DEFAULT_UNLOCK_TIMEOUT_MIN,
};
use apprafter_core::{
    CancellationToken, ChangeAction, Context, CoreError, CoreResult, Event, Plan, PlannedChange,
    Reporter, SecretString, Zeroizing,
};
use cli_core::{CliError, LockPhase, LockedWhy};

use crate::cli::LockCommand;
use crate::commands::target::completed;
use crate::context::{cli_context, diagnostic_context, ProcessEnv};
use crate::holder::protocol::HolderUnavailable;
use crate::holder::spawn::HolderStarted;
use crate::prompt::{Asker, PromptError, Terminal};
use crate::render::core_error::report;
use crate::render::reporter::CliReporter;
use crate::vault_access::{ask_and_open, prompt_refusal, sentence, ATTEMPTS};
use crate::{holder_window, limits_client};

/// C11.
const REPEAT: &str = "Repeat the lock password: ";
/// C12.
const DIFFER: &str = "The two passwords differ. Try again.\n";
/// O2: the two refusals that replace a minimum.
const EMPTY: &str = "The lock password cannot be empty.\n";
const KEY_SHAPED_REFUSED: &str = "That has the shape of a recovery key, so it would never be \
                                  tried as a password. Choose another.\n";
/// O2: after C13.
const ANYWAY: &str = "Use it anyway? [y/N] ";
/// C15, as §3.11 amends it (O12).
const CONFIRM: &str = "Type the recovery key to confirm you saved it (the \
                       `APPRAFTER-RECOVERY-KEY-1` prefix is optional): ";
/// A resume's answer that is not the slot's secret (C29's resume form).
const NOT_THE_PASSWORD: &str = "That is not the lock password of this store.\n";
const NOT_THE_KEY: &str = "That is not the recovery key of this store.\n";
/// A resume without one of its two secrets: the way out (team decision 2026-10-13).
const WAY_OUT: &str = "This `apprafter lock on` finishes only with both the lock password and \
                       the recovery key. Without one of them, `apprafter lock off` backs out of \
                       it with the other, and a new `apprafter lock on` makes a new vault and a \
                       new recovery key.\n";
/// C8.
const AGENT_NOTE: &str = "This stops an AI agent that runs AppRafter as you from reading \
                          these secrets or using them without you. It does not stop a program \
                          that sets out to capture what you type: for that, run agents under a \
                          separate OS account or in a sandbox (guide: Protecting secrets from \
                          agents).\n";
/// C23.
const CLEAR_AGAIN: &str = "The secrets will be on disk in clear again: any program running \
                           as you, an AI agent included, can read them.\n";

pub(crate) fn run(action: LockCommand) -> miette::Result<()> {
    match action {
        LockCommand::On { timeout } => on(&Terminal, timeout),
        LockCommand::Off => off(&Terminal),
        LockCommand::Now => now(),
        LockCommand::Status => status(),
    }
}

/// The store root as the person reads it, and its targets in words.
fn named(ctx: &Context) -> (String, String) {
    let shown = cli_core::paths::abbreviate_home(ctx.config_root(), ctx.home_dir());
    let names = cli_core::list_target_names(&ctx.store()).unwrap_or_default();
    let targets = if names.is_empty() {
        "no targets".to_string()
    } else {
        format!("targets: {}", names.join(", "))
    };
    (shown, targets)
}

/// The progress of a lock flow (surface C16): each stage as it starts, on stderr; notices and
/// warnings as every command prints them.
struct Progress;

impl Reporter for Progress {
    fn report(&self, event: Event) {
        match &event {
            Event::Stage { title, .. } => eprintln!("  {title}"),
            _ => CliReporter.report(event),
        }
    }
}

fn on(asker: &dyn Asker, timeout: Option<u32>) -> miette::Result<()> {
    crate::vault_access::insecure_kdf_note();
    let ctx = crate::context::cli_context()?;
    let root = ctx.config_root().to_path_buf();
    let (shown, targets) = named(&ctx);
    let plan = lock::plan_on(&ctx, timeout).map_err(report)?;
    if let LockOnStart::AlreadyLocked = plan.payload.start() {
        match timeout {
            None => println!("The target store at {shown} is already locked."),
            Some(minutes) => {
                let set = lock::set_unlock_timeout(&ctx, minutes, &CliReporter).map_err(report)?;
                println!("Unlock windows of {shown} now last {set} min (0: every command asks).");
            }
        }
        return Ok(());
    }
    if !asker.present() {
        return Err(miette::Report::new(CliError::UsageRefused {
            message: "`apprafter lock on` asks for a new lock password and shows a recovery \
                      key: run it in a terminal"
                .to_string(),
            help: "It cannot be scripted: whoever runs it chooses the password.".to_string(),
        }));
    }
    let refused = prompt_refusal(&root);
    asker
        .tell(&on_plan_text(&ctx, &plan, &shown, &targets))
        .map_err(|e| miette::Report::new(refused(e)))?;
    let input = match plan.payload.start().clone() {
        LockOnStart::Resume { id } => {
            asker
                .tell(&format!(
                    "An earlier `apprafter lock on` on {shown} did not finish; finishing it. \
                     Before any secret leaves the files, the vault it wrote is opened with the \
                     lock password and with the recovery key.\n"
                ))
                .map_err(|e| miette::Report::new(refused(e)))?;
            let password = ask_slot(asker, &root, &id, LockSlotKind::Password, &shown)
                .map_err(miette::Report::new)?;
            let recovery = ask_slot(asker, &root, &id, LockSlotKind::RecoveryKey, &shown)
                .map_err(miette::Report::new)?;
            LockOnInput::Resume { password, recovery }
        }
        LockOnStart::New { restart } => {
            if restart {
                asker
                    .tell(&format!(
                        "An earlier `apprafter lock on` on {shown} stopped before it wrote the \
                         vault: the recovery key shown then is not valid; a new one follows.\n"
                    ))
                    .map_err(|e| miette::Report::new(refused(e)))?;
            }
            let vault_shown = cli_core::paths::abbreviate_home(
                &root.join(apprafter_core::vault::VAULT_FILE),
                ctx.home_dir(),
            );
            new_input(asker, &shown, &vault_shown, &root).map_err(miette::Report::new)?
        }
        LockOnStart::AlreadyLocked => unreachable!("answered above"),
    };
    eprintln!("Locking...");
    let done = completed(
        lock::execute_on(&ctx, plan, input, &Progress, &CancellationToken::new())
            .map_err(report)?,
    )?;
    print!("{}", on_result(&done, &shown));
    Ok(())
}

fn off(asker: &dyn Asker) -> miette::Result<()> {
    let ctx = crate::context::cli_context()?;
    let root = ctx.config_root().to_path_buf();
    let (shown, _) = named(&ctx);
    let plan = lock::plan_off(&ctx).map_err(report)?;
    if let LockOffStart::NotLocked = plan.payload.start() {
        println!("The target store at {shown} is not locked: its secrets are already in files.");
        return Ok(());
    }
    let text = off_plan_text(&plan, &shown);
    if asker.present() {
        asker
            .tell(&text)
            .map_err(|e| miette::Report::new(prompt_refusal(&root)(e)))?;
    } else {
        // Target names and paths, shown where a person may read them: printable only.
        eprint!("{}", crate::prompt::printable(&text));
    }
    let opened = match plan.payload.store_id() {
        Some(id) if plan.payload.needs_secret() => Some(
            ask_and_open(
                asker,
                &root,
                id,
                "",
                &format!("Lock password or recovery key of {shown}, to confirm: "),
                KdfPolicy::active(),
            )
            .map_err(miette::Report::new)?,
        ),
        _ => None,
    };
    // D.4a T56: this terminal's window ends before the vault goes, so no holder of it keeps
    // serving a copy of the vault (critic-d4a-e finding 14); one that does not answer and could
    // not be ended is named, and never stops `lock off` (rule 1).
    if let Some(said) = off_window_said(holder_window::end_window(&ctx)) {
        eprintln!("{said}");
    }
    eprintln!("Unlocking...");
    let done = completed(
        lock::execute_off(&ctx, plan, opened, &Progress, &CancellationToken::new())
            .map_err(report)?,
    )?;
    print!("{}", off_result(&done, &shown));
    Ok(())
}

/// What `lock off` says of this terminal's window, ended before the vault goes (D.4a T56): that
/// it closed; or, rule 1, that its holder does not answer and could not be ended — the store
/// goes back to files while that holder keeps the store's key until it is stopped. Never a
/// reason to stop `lock off`: a holder that hung must not keep a person from leaving the lock.
fn off_window_said(ended: CoreResult<holder_window::Ended>) -> Option<String> {
    use holder_window::Ended;
    match ended {
        Ok(Ended::Closed | Ended::Stopped) => {
            Some("The unlock window in this terminal is closed.".to_string())
        }
        Ok(Ended::StillOpen { pid: Some(pid) }) => {
            let stop = if cfg!(windows) {
                format!("taskkill /F /PID {pid}")
            } else {
                format!("kill -9 {pid}")
            };
            Some(format!(
                "The unlock window in this terminal could not be ended: its holder (pid {pid}) \
                 does not answer, and keeps this store's key until it is stopped (`{stop}`)."
            ))
        }
        Ok(Ended::StillOpen { pid: None }) => Some(
            "The unlock window in this terminal could not be ended: its holder (an `apprafter` \
             process) does not answer, and keeps this store's key until it is stopped."
                .to_string(),
        ),
        Ok(Ended::None) | Err(_) => None,
    }
}

/// The new lock password (O2; surface C10–C13 as §3.11 amends them): asked, refused only when
/// empty, too long or recovery-key shaped (asked again), repeated, and — under 15 characters
/// — kept only on `y` / `yes` after C13's warning.
fn new_password(asker: &dyn Asker, shown: &str) -> Result<SecretString, PromptError> {
    loop {
        let password = asker.ask(&format!("New lock password for {shown}: "))?;
        let advice = match check_password(&password) {
            Ok(advice) => advice,
            Err(CoreError::LockPasswordRefused { reason }) => {
                asker.tell(match reason {
                    PasswordProblem::Empty => EMPTY,
                    PasswordProblem::RecoveryKeyShape => KEY_SHAPED_REFUSED,
                    PasswordProblem::TooLong { .. } => {
                        "A lock password can be at most 1024 bytes long. Choose a shorter one.\n"
                    }
                })?;
                continue;
            }
            Err(other) => {
                asker.tell(&format!("{other}\n"))?;
                continue;
            }
        };
        let again = asker.ask(REPEAT)?;
        if again.expose() != password.expose() {
            asker.tell(DIFFER)?;
            continue;
        }
        if let Some(PasswordAdvice::Short { chars, recommended }) = advice {
            asker.tell(&format!(
                "This lock password has {chars} characters; {recommended} or more is \
                 recommended. Anyone who copies this store's vault file, an AI agent running as \
                 you included, can try passwords against it offline, as fast as their hardware \
                 allows. A long phrase is easier to type and harder to guess.\n"
            ))?;
            let answer = asker.answer(ANYWAY)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                continue;
            }
        }
        return Ok(password);
    }
}

/// C14 with O8's sentences: the key, shown once, and what it opens.
fn recovery_text(shown: &str, vault_shown: &str, key: &SecretString) -> Zeroizing<String> {
    let mut text = Zeroizing::new(format!("Recovery key for {shown} — shown only now:\n\n"));
    for line in key.expose().lines() {
        text.push_str("    ");
        text.push_str(line);
        text.push('\n');
    }
    text.push_str(&format!(
        "\nIt opens this store if the lock password is lost. AppRafter cannot open a locked \
         store without its lock password or its recovery key. Keep the recovery key somewhere \
         other than this computer. The recovery key opens nothing without the vault file: \
         include {vault_shown} in your backups; it is encrypted. Clear this screen once it is \
         saved.\n"
    ));
    text
}

/// The recovery key shown at the terminal, then typed back whole until it matches (T25, O12):
/// a mistake is told, never what was typed.
fn show_and_confirm(
    asker: &dyn Asker,
    key: RecoveryKey,
    shown: &str,
    vault_shown: &str,
) -> Result<ConfirmedRecoveryKey, PromptError> {
    let text = recovery_text(shown, vault_shown, &key.to_display());
    asker.tell(&text)?;
    let mut key = key;
    loop {
        let typed = asker.ask(CONFIRM)?;
        match key.confirm(typed.expose()) {
            Ok(confirmed) => return Ok(confirmed),
            Err((back, error)) => {
                key = back;
                asker.tell(&format!(
                    "{}. Type it again, or press Ctrl-C to stop (nothing has been changed).\n",
                    sentence(&RecoveryKeyProblem::from(error).message())
                ))?;
            }
        }
    }
}

/// A new run's secrets: the password, then the recovery key (nothing is written before both).
fn new_input(
    asker: &dyn Asker,
    shown: &str,
    vault_shown: &str,
    root: &Path,
) -> Result<LockOnInput, CliError> {
    let refused = prompt_refusal(root);
    let password = new_password(asker, shown).map_err(&refused)?;
    let key = RecoveryKey::generate(&mut OsRandom).map_err(|e| CliError::RandomUnavailable {
        detail: e.to_string(),
    })?;
    let recovery = show_and_confirm(asker, key, shown, vault_shown).map_err(&refused)?;
    Ok(LockOnInput::New { password, recovery })
}

/// A resume's question for the secret of a `kind` slot of the store `shown` (rule 4).
fn resume_question(kind: LockSlotKind, shown: &str) -> String {
    let what = match kind {
        LockSlotKind::Password => "Lock password",
        _ => "Recovery key",
    };
    format!("{what} of {shown} (Enter alone if you no longer have it): ")
}

/// One slot's secret for a resume (rule 4; team decision 2026-10-13): asked up to
/// [`ATTEMPTS`] times by its own question and opened for that slot alone
/// (`lock::open_slot_at`). A wrong secret or a mistyped key is told and costs one attempt.
/// Enter alone — the person no longer has it — or a third wrong answer ends the resume:
/// [`WAY_OUT`] at the terminal, then `lock_incomplete`, whose help says the same.
fn ask_slot(
    asker: &dyn Asker,
    root: &Path,
    id: &StoreId,
    kind: LockSlotKind,
    shown: &str,
) -> Result<SlotOpened, CliError> {
    let refused = prompt_refusal(root);
    let question = resume_question(kind, shown);
    for _ in 0..ATTEMPTS {
        let secret = asker.ask(&question).map_err(&refused)?;
        if secret.expose().trim().is_empty() {
            break;
        }
        match lock::open_slot_at(root, id, kind, secret, KdfPolicy::active()) {
            Ok(opened) => return Ok(opened),
            Err(CoreError::Cli(CliError::UnlockFailed { .. })) => asker
                .tell(match kind {
                    LockSlotKind::Password => NOT_THE_PASSWORD,
                    _ => NOT_THE_KEY,
                })
                .map_err(&refused)?,
            Err(CoreError::RecoveryKeyInvalid { reason }) => asker
                .tell(&format!("{}.\n", sentence(&reason.message())))
                .map_err(&refused)?,
            Err(e) => return Err(crate::context::into_cli_error(e)),
        }
    }
    asker.tell(WAY_OUT).map_err(&refused)?;
    Err(CliError::LockIncomplete {
        store: root.to_path_buf(),
        phase: LockPhase::Locking,
    })
}

/// One plan line (surface C2–C6, C22): the action, what it acts on, the detail.
fn plan_line(c: &PlannedChange) -> String {
    let action = match c.action {
        ChangeAction::Move => "move",
        ChangeAction::Create => "create",
        ChangeAction::Update => "update",
        ChangeAction::Delete => "delete",
        ChangeAction::Keep => "keep",
        _ => "change",
    };
    let what = match c.kind.as_str() {
        "Credentials" => format!("Hetzner token of `{}`", c.object),
        "Cache" => format!("cached secrets of `{}`", c.object),
        "AgeKey" if c.object == "this store" => "age key of this store".to_string(),
        "AgeKey" => format!("shared age key {}", c.object),
        _ => c.object.clone(),
    };
    let line = format!(
        "  {action:<7} {what:<34} {}",
        c.detail.as_deref().unwrap_or("")
    );
    format!("{}\n", line.trim_end())
}

/// `lock on`'s plan (C1–C9, O1's two notes; C7 without the window a later step opens). A test
/// build said so on stderr already (`insecure_kdf_note`, T23).
fn on_plan_text(ctx: &Context, plan: &Plan<LockOnPayload>, shown: &str, targets: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Lock the secrets of the target store at {shown} ({targets}):\n"
    ));
    for change in &plan.changes {
        out.push_str(&plan_line(change));
    }
    out.push_str(&format!(
        "  note    the SSH key {} that fetches kubeconfigs stays outside the vault: a program \
         running as you can use it to reach the nodes\n",
        cli_providers::hetzner_cloud::default_ssh_identity_path().display()
    ));
    out.push_str(
        "  note    while a command or the app uses a secret, it is in that process's memory \
         and, for a kubeconfig, in a file programs running as you can read\n",
    );
    // D.4a f Task 3: a plan that reads no shared key (no cache to move) names none.
    let reads_key = !plan.payload.keys().is_empty();
    if let (Some(path), true) = (&ctx.overrides().age_key, reads_key) {
        out.push_str(&format!(
            "The cached secrets are read with the age key at {} (APPRAFTER_AGE_KEY), and not \
             with the default one; once the store is locked the variable no longer applies.\n",
            path.display()
        ));
    }
    out.push_str(&format!(
        "Unlock window: {} min (`--timeout` changes it). Once locked, a command that needs \
         these secrets asks for the lock password or the recovery key in its terminal, and a \
         command with no terminal is refused. HCLOUD_TOKEN and APPRAFTER_AGE_KEY no longer \
         apply to this store.\n",
        plan.payload.timeout_min()
    ));
    out.push_str(AGENT_NOTE);
    out
}

/// `lock off`'s plan (C21–C23).
fn off_plan_text(plan: &Plan<LockOffPayload>, shown: &str) -> String {
    let mut out = match plan.payload.start() {
        LockOffStart::BackOutNoVault
        | LockOffStart::Unlock {
            from: SecretsMode::Locking,
            ..
        } => format!(
            "Back out of the interrupted `apprafter lock on` of the target store at {shown}:\n"
        ),
        LockOffStart::FinishNoVault { .. }
        | LockOffStart::Unlock {
            from: SecretsMode::Unlocking,
            ..
        } => format!(
            "Finish the interrupted `apprafter lock off`: put the secrets of the target store \
             at {shown} back in files:\n"
        ),
        _ => format!("Put the secrets of the target store at {shown} back in files:\n"),
    };
    for change in &plan.changes {
        out.push_str(&plan_line(change));
    }
    if plan.payload.needs_secret() {
        out.push_str(CLEAR_AGAIN);
    }
    out
}

/// A cached secret's wire name, as a sentence names it.
fn noun(secret: &str) -> &str {
    match secret {
        "argocd_password" => "Argo CD admin password",
        other => other,
    }
}

/// C16, C17 (no window yet), G2's and G13's lines.
fn on_result(done: &LockedOn, shown: &str) -> String {
    if done.already_locked {
        return format!(
            "The target store at {shown} was locked by another `apprafter lock on` meanwhile; \
             the recovery key shown here is not valid.\n"
        );
    }
    let mut out = format!(
        "Locked: the secrets of {shown} are in its vault (opened by the lock password or the \
         recovery key).\nA command that needs them asks for the lock password or the recovery \
         key in its terminal; a command with no terminal is refused.\n  ✓ {} token(s) moved \
         into the vault\n  ✓ {} cached secret(s) re-encrypted\n",
        done.tokens_moved.len(),
        done.caches_resealed
    );
    for d in &done.caches_dropped {
        out.push_str(&format!(
            "  dropped the cached {} of `{}`: {}\n",
            noun(&d.secret),
            d.target,
            d.why
        ));
    }
    for d in &done.caches_left {
        out.push_str(&format!(
            "  left the cached {} of `{}`: {}\n",
            noun(&d.secret),
            d.target,
            d.why
        ));
    }
    for t in &done.tokens_left {
        out.push_str(&format!(
            "  `{t}`: its credentials file holds a token in clear that is not the vault's, \
             written while this ran; it is not used. `apprafter target add {t} --renew` stores \
             a token in the vault (at a terminal it asks for the new token without showing it; \
             in a script, `--token`).\n"
        ));
    }
    out
}

/// C25, C26, and the back-out's line.
fn off_result(done: &LockedOff, shown: &str) -> String {
    let mut out = if done.not_locked {
        format!("The target store at {shown} is not locked: its secrets are already in files.\n")
    } else if done.backed_out {
        format!("Backed out: the secrets of {shown} are in files, as before `apprafter lock on`.\n")
    } else {
        format!(
            "Unlocked for good: the secrets of {shown} are in files again, and its vault is \
             deleted.\n"
        )
    };
    if let (true, Some(key)) = (done.age_key_created, &done.age_key) {
        out.push_str(&format!(
            "Created the shared age key at {key}: the cached secrets are encrypted to it.\n"
        ));
    }
    for t in &done.tokens_kept {
        out.push_str(&format!(
            "  `{t}`: its credentials file held a token written while the store was locked, \
             which differs from the vault's; that token was kept.\n"
        ));
    }
    for d in &done.caches_dropped {
        out.push_str(&format!(
            "  dropped the cached {} of `{}`: {}\n",
            noun(&d.secret),
            d.target,
            d.why
        ));
    }
    for d in &done.caches_left {
        out.push_str(&format!(
            "  left the cached {} of `{}`: {}\n",
            noun(&d.secret),
            d.target,
            d.why
        ));
    }
    out
}

/// `apprafter lock status` (D.4a §3.13, T66, T72, T73): stdout, exit 0; never a prompt, never
/// the vault opened. A diagnostic (T66), so it takes the diagnostic context: no read it makes,
/// today's or a later one, can reach a prompt.
pub(crate) fn status() -> miette::Result<()> {
    let ctx = diagnostic_context()?;
    let terminal = holder_window::terminal_present();
    holder_window::fast_kdf_note();
    let place = holder_window::HolderPlace::of(&ctx);
    let window = holder_window::window_state(&ctx, &place, terminal).map_err(report)?;
    let client = limits_client::cli_limit_client(&ProcessEnv, terminal);
    let lock_report = apprafter_core::lock::status(&ctx, window, &client).map_err(report)?;
    print!("{}", render_status(&lock_report));
    Ok(())
}

/// `apprafter lock now` (T56, T59, T65, S9): `End` to this terminal's holder; one that does not
/// answer is ended through its connection, and no other process. One that could not be ended
/// is an error naming it, never "no window" (rule 1).
pub(crate) fn now() -> miette::Result<()> {
    let ctx = cli_context()?;
    let said =
        now_said(holder_window::end_window(&ctx).map_err(report)?).map_err(miette::Report::new)?;
    eprintln!("{said}");
    Ok(())
}

/// What `lock now` says of how ending the window went.
fn now_said(ended: holder_window::Ended) -> Result<&'static str, NotEnded> {
    Ok(match ended {
        holder_window::Ended::Closed => "The unlock window in this terminal is closed.",
        holder_window::Ended::Stopped => {
            "The unlock window in this terminal is closed (its holder did not answer and was stopped)."
        }
        holder_window::Ended::StillOpen { pid } => return Err(NotEnded::new(pid)),
        holder_window::Ended::None => "No unlock window is open in this terminal.",
    })
}

/// `apprafter unlock [--timeout M]` (T56, T65, O3, O11): always asks, then replaces this
/// terminal's holder.
pub(crate) fn unlock(timeout: Option<u32>) -> miette::Result<()> {
    let ctx = cli_context()?;
    let root = cli_core::paths::abbreviate_home(ctx.config_root(), ctx.home_dir());
    let incomplete = match ctx.secrets().mode().map_err(miette::Report::new)? {
        SecretsMode::Files => {
            eprintln!(
                "The target store at {root} is not locked: its secrets are in files, readable \
                 by any program running as you. `apprafter lock on` protects them."
            );
            return Ok(());
        }
        SecretsMode::Locking => Some(LockPhase::Locking),
        SecretsMode::Unlocking => Some(LockPhase::Unlocking),
        SecretsMode::Locked => None,
    };
    // WI-478 (d4a-h F1's class): a store other accounts can change is refused before its lock
    // password is asked, as `lock off` refuses it when planned, once it knows the store is not
    // in files: the window would open a vault whose every secret is then refused. Read only.
    cli_core::store_privacy::ensure_private(&ctx.store()).map_err(miette::Report::new)?;
    if let Some(phase) = incomplete {
        return Err(miette::Report::new(CliError::LockIncomplete {
            store: ctx.config_root().to_path_buf(),
            phase,
        }));
    }
    // T61, T69: nothing can be asked here, so nothing else is looked at.
    if !holder_window::terminal_present() {
        return Err(miette::Report::new(CliError::StoreLocked {
            store: ctx.config_root().to_path_buf(),
            reason: LockedWhy::NoTerminal,
            stopped_before: None,
        }));
    }
    holder_window::fast_kdf_note();
    let file = ctx.secrets().store_file().map_err(miette::Report::new)?;
    let Some(id) = file.as_ref().and_then(|f| f.id.clone()) else {
        return Err(miette::Report::new(CliError::StoreFileUnreadable {
            path: ctx.config_root().join(STORE_FILE),
            detail: "it says the secrets are locked, and names no id".into(),
        }));
    };
    let store = holder_window::binding(ctx.config_root(), id.as_str());
    let place = holder_window::HolderPlace::of(&ctx);
    place
        .possible(&store)
        .map_err(|u| miette::Report::new(NoWindow::from(&u)))?;
    let mirror = file.as_ref().and_then(|f| f.unlock_timeout_min);
    let zero = || {
        eprintln!(
            "The target store at {root} opens no unlock window: its window is 0 min, so every \
             command asks. `apprafter unlock --timeout 5` opens one for 5 min."
        );
    };
    if timeout.is_none() && mirror == Some(0) {
        zero();
        return Ok(());
    }
    let shown = timeout.or(mirror).unwrap_or(DEFAULT_UNLOCK_TIMEOUT_MIN);
    let targets = cli_core::list_target_names(&ctx.store())
        .map_err(miette::Report::new)?
        .join(", ");
    let intro = format!(
        "Unlock the target store at {root} (targets: {targets}) in this terminal for {shown} min.\n"
    );
    let opened = holder_window::ask_and_open(&ctx, &id, &intro)?;
    // T56: a new window replaces this terminal's old one — an older install's too (finding 13).
    if let Ok(Some(mut old)) = place.find_to_end(&store) {
        let _ = old.end();
    }
    // The vault's own default, which `store.yaml` only mirrors.
    let minutes = timeout.unwrap_or(opened.default_minutes);
    if minutes == 0 {
        zero();
        return Ok(());
    }
    match holder_window::start_window(&place, &store, &opened.key, opened.vault_id, minutes) {
        Ok(HolderStarted::Started { .. }) => {}
        Ok(HolderStarted::AlreadyRunning { pid }) => {
            return Err(miette::Report::new(NoWindow::from(
                &HolderUnavailable::NotResponding { pid: Some(pid) },
            )))
        }
        Err(u) => return Err(miette::Report::new(NoWindow::from(&u))),
    }
    eprintln!(
        "{}",
        unlock_said(minutes, holder_window::leads_own_session())
    );
    Ok(())
}

/// What `unlock` says once its window started (T52, C30): C30's line; or, when this CLI leads
/// its own session (`exec apprafter unlock`, `ssh -t host apprafter unlock`), that the window
/// ends with it — this CLI is still running as it says so, and no window is left for the next
/// command (d4a-h F16: never C30's "Unlocked in this terminal for <n> min" after it).
fn unlock_said(minutes: u32, leads_session: bool) -> String {
    if leads_session {
        "This command leads its own session, so the unlock window it opened ends with it: the \
         next command asks again (run `apprafter unlock` from a shell to keep a window)."
            .to_string()
    } else {
        holder_window::opened_note(minutes)
    }
}

/// No unlock window is possible in this terminal (T49–T52, T59): `unlock`'s refusal. A CLI-local
/// diagnostic, as the sandbox's are: the desktop has no terminal window to refuse
/// (**Deviations** 15).
#[derive(Debug, miette::Diagnostic)]
#[diagnostic(
    code(apprafter::lock::no_window),
    help(
        "every command in this terminal asks for the lock password instead; `apprafter lock \
         status` says why"
    )
)]
pub(crate) struct NoWindow {
    why: String,
}

impl From<&HolderUnavailable> for NoWindow {
    fn from(u: &HolderUnavailable) -> Self {
        NoWindow { why: u.describe() }
    }
}

impl std::fmt::Display for NoWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no unlock window is possible in this terminal: {}",
            self.why
        )
    }
}

impl std::error::Error for NoWindow {}

/// `lock now` met this terminal's holder and could not end it: it does not answer, and takes no
/// connection or could not be stopped (T59; rule 1: the window is still open, never "no
/// window"). A CLI-local diagnostic, as `no_window` is.
#[derive(Debug, miette::Diagnostic)]
#[diagnostic(code(apprafter::lock::not_ended))]
pub(crate) struct NotEnded {
    pid: Option<u32>,
    #[help]
    help: String,
}

impl NotEnded {
    fn new(pid: Option<u32>) -> NotEnded {
        let help = match pid {
            Some(pid) if cfg!(windows) => format!(
                "Stop that process (`taskkill /F /PID {pid}`): until it ends it keeps this \
                 store's key. Meanwhile every command asks at its prompt."
            ),
            Some(pid) => format!(
                "Stop that process (`kill -9 {pid}`): until it ends it keeps this store's key. \
                 Meanwhile every command asks at its prompt."
            ),
            None => "Stop this terminal's holder (an `apprafter` process): until it ends it \
                     keeps this store's key. Meanwhile every command asks at its prompt."
                .to_string(),
        };
        NotEnded { pid, help }
    }
}

impl std::fmt::Display for NotEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.pid {
            Some(pid) => write!(
                f,
                "the unlock window in this terminal is still open: its holder (pid {pid}) does \
                 not answer and could not be ended from here"
            ),
            None => write!(
                f,
                "the unlock window in this terminal is still open: its holder does not answer \
                 and could not be ended from here"
            ),
        }
    }
}

impl std::error::Error for NotEnded {}

/// O1's sentence: what the lock covers and what it does not.
const AT_REST: &str = "The lock protects secrets at rest. While a command or the app uses one, it \
     is in that process's memory and, for a kubeconfig, in a file programs running as you can \
     read; the SSH key that fetches kubeconfigs stays outside the lock.";

/// The `lock status` text (§3.13).
pub(crate) fn render_status(r: &LockReport) -> String {
    let mut lines = Vec::new();
    let targets = if r.targets.is_empty() {
        "no targets".to_string()
    } else {
        format!("targets: {}", r.targets.join(", "))
    };
    lines.push(match &r.store_id {
        Some(id) => format!("Target store:   {} (id {id}; {targets})", r.store),
        None => format!("Target store:   {} ({targets})", r.store),
    });
    lines.push(format!("Secrets:        {}", secrets_text(r)));
    if r.mode != SecretsMode::Files {
        let slots: Vec<String> = r.slots.iter().map(slot_text).collect();
        lines.push(format!("Slots:          {}", slots.join(" · ")));
    }
    lines.push(format!("This terminal:  {}", terminal_text(r)));
    let mut rows: Vec<(bool, String)> = Vec::new();
    if let UnlockWindow::Open { hardening, .. } = &r.window {
        if let Some(line) = text::hardening_line(hardening) {
            rows.push((true, line));
        }
    }
    rows.extend(
        r.limits
            .iter()
            .filter_map(text::limit_line)
            .map(|l| (l.warn, l.text)),
    );
    // §3.13: the warnings first, each kind in the report's order (a stable sort).
    rows.sort_by_key(|(warn, _)| !*warn);
    if rows.is_empty() {
        lines.push("This computer:  nothing to report".into());
    } else {
        lines.push("This computer:".into());
        for (warn, text) in rows {
            lines.push(format!("  {} {text}", if warn { "⚠" } else { "ℹ" }));
        }
    }
    if r.mode != SecretsMode::Files {
        lines.push(AT_REST.into());
    }
    lines.iter().map(|l| format!("{l}\n")).collect()
}

fn secrets_text(r: &LockReport) -> String {
    match r.mode {
        SecretsMode::Files => {
            "in files — readable by any program running as you; `apprafter lock on` protects them"
                .into()
        }
        SecretsMode::Locked => format!("locked — in the vault, opened by {}", opened_by(&r.slots)),
        SecretsMode::Locking => "locking — an earlier `apprafter lock on` did not finish: run it \
                                 again, or `apprafter lock off` to back out"
            .into(),
        SecretsMode::Unlocking => {
            "unlocking — an earlier `apprafter lock off` did not finish: run it again".into()
        }
    }
}

/// "a password or the recovery key": each kind of slot once, in the vault's order.
fn opened_by(slots: &[SlotView]) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    for s in slots {
        let kind = match s.kind {
            LockSlotKind::Password => "a password",
            LockSlotKind::RecoveryKey => "the recovery key",
            LockSlotKind::OsPassword => "your OS password",
            LockSlotKind::WindowsHello => "Windows Hello",
            LockSlotKind::TouchId => "Touch ID",
            LockSlotKind::Unknown => continue,
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds.join(" or ")
}

fn slot_text(s: &SlotView) -> String {
    match s.refused {
        Some(SlotRefusalView::KdfTooWeak) => {
            format!("{} (refused by this build: kdf_too_weak)", s.label)
        }
        Some(SlotRefusalView::KdfOutOfRange) => {
            format!("{} (refused by this build: kdf_out_of_range)", s.label)
        }
        None => s.label.clone(),
    }
}

fn terminal_text(r: &LockReport) -> String {
    let mut line = match &r.window {
        UnlockWindow::Open {
            left_secs,
            minutes,
            holder_version,
            ..
        } => {
            let left = if *left_secs < 60 {
                format!("{left_secs} s")
            } else {
                format!("{} min", left_secs.div_ceil(60))
            };
            let mut line = format!(
                "unlocked, {left} left (a window lasts {minutes} min, restarted by each command \
                 that uses it)"
            );
            if holder_version != env!("CARGO_PKG_VERSION") {
                line.push_str(&format!("; its holder is apprafter {holder_version}"));
            }
            line
        }
        UnlockWindow::Closed => "no unlock window".into(),
        UnlockWindow::NoTerminal => "not run from a terminal".into(),
        UnlockWindow::EveryCommand => "every command asks (the store's window is 0 min)".into(),
        // A holder that does not answer still holds the key: an open window, never "no window"
        // (rule 1), and not one a command can use.
        UnlockWindow::Unavailable {
            reason: HolderUnavailableReason::NotResponding,
        } => "open, but its holder does not answer: every command asks; `apprafter lock now` \
              ends it, or names the process to stop"
            .into(),
        UnlockWindow::Unavailable { reason } => {
            format!("no window possible here ({})", unavailable_text(*reason))
        }
        UnlockWindow::App => "unlocked in this app".into(),
    };
    let fallback = r.limits.iter().find_map(|l| match l {
        MachineLimit::RuntimeDirFallback { path } => Some(path),
        _ => None,
    });
    if let Some(path) = fallback {
        line.push_str(&format!(
            " (holder base: {path}, XDG_RUNTIME_DIR is not set)"
        ));
    }
    line
}

fn unavailable_text(r: HolderUnavailableReason) -> &'static str {
    match r {
        HolderUnavailableReason::NoRuntimeDir => "no directory to keep it in",
        HolderUnavailableReason::UnsafeRuntimeDir => "its directory is not private to you",
        HolderUnavailableReason::PathTooLong => "its socket path would be too long for this OS",
        HolderUnavailableReason::NoSessionLeader => "this terminal's session has no leader",
        HolderUnavailableReason::NoConsole => "there is no console to bind it to",
        HolderUnavailableReason::NotResponding => {
            "its holder does not answer; `apprafter lock now` ends it"
        }
        HolderUnavailableReason::Timeout => "the holder did not start in time",
    }
}

#[cfg(test)]
mod tests {
    use apprafter_core::vault::{OsRandom, RecoveryKey};
    use apprafter_core::{ChangeAction, PlannedChange};

    use super::*;
    use crate::prompt::{PromptError, Script};

    const PW: &str = "correct horse battery staple";
    /// 58 characters of the Bech32 set: the shape of a recovery key without its prefix.
    const KEY_SHAPED: &str = "qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
    const NEW: &str = "New lock password for ~/store: ";

    /// d4a-h F16 (matrix L-8): a CLI that leads its own session says only that the window it
    /// opened ends with it — never C30's "Unlocked in this terminal for 5 min" right after it;
    /// any other `unlock` says C30's line.
    #[test]
    fn an_unlock_that_leads_its_session_promises_no_window() {
        let led = unlock_said(5, true);
        assert_eq!(
            led,
            "This command leads its own session, so the unlock window it opened ends with it: \
             the next command asks again (run `apprafter unlock` from a shell to keep a window)."
        );
        assert!(!led.contains("Unlocked in this terminal"), "{led}");
        assert_eq!(unlock_said(5, false), holder_window::opened_note(5));
        assert_eq!(unlock_said(15, false), holder_window::opened_note(15));
    }

    #[test]
    fn a_password_is_asked_twice_and_kept() {
        let s = Script::new(vec![Ok(PW), Ok(PW)]);
        assert_eq!(new_password(&s, "~/store").unwrap().expose(), PW);
        assert_eq!(s.asked(), vec![NEW.to_string(), REPEAT.to_string()]);
        assert_eq!(s.told(), "");
    }

    #[test]
    fn two_passwords_that_differ_start_again() {
        let s = Script::new(vec![
            Ok(PW),
            Ok("correct horse battery stapel"),
            Ok(PW),
            Ok(PW),
        ]);
        assert_eq!(new_password(&s, "~/store").unwrap().expose(), PW);
        assert_eq!(s.told(), DIFFER);
        assert_eq!(s.asked().len(), 4);
    }

    /// O2, T62: empty and recovery-key-shaped are asked again, before the repeat.
    #[test]
    fn empty_and_key_shaped_passwords_are_asked_again() {
        let s = Script::new(vec![Ok(""), Ok(KEY_SHAPED), Ok(PW), Ok(PW)]);
        assert_eq!(new_password(&s, "~/store").unwrap().expose(), PW);
        assert_eq!(s.told(), format!("{EMPTY}{KEY_SHAPED_REFUSED}"));
        assert_eq!(
            s.asked(),
            vec![
                NEW.to_string(),
                NEW.to_string(),
                NEW.to_string(),
                REPEAT.to_string()
            ]
        );
    }

    /// O2: under 15 characters is advice — C13 and `Use it anyway? [y/N]`; only `y` / `yes`
    /// (any case) keeps it, anything else starts again.
    #[test]
    fn a_short_password_is_kept_only_on_yes() {
        let s = Script::new(vec![
            Ok("short pw"),
            Ok("short pw"),
            Ok("n"),
            Ok("short pw2"),
            Ok("short pw2"),
            Ok(" YES "),
        ]);
        assert_eq!(new_password(&s, "~/store").unwrap().expose(), "short pw2");
        let told = s.told();
        assert!(
            told.contains("This lock password has 8 characters; 15 or more is recommended."),
            "{told}"
        );
        assert!(
            told.contains("This lock password has 9 characters;"),
            "{told}"
        );
        assert!(
            told.contains("can try passwords against it offline"),
            "{told}"
        );
        assert_eq!(s.asked().iter().filter(|a| a.as_str() == ANYWAY).count(), 2);
    }

    #[test]
    fn a_cancel_at_any_prompt_stops_the_dialogue() {
        let s = Script::new(vec![Ok(PW), Err(PromptError::Cancelled)]);
        assert!(matches!(
            new_password(&s, "~/store"),
            Err(PromptError::Cancelled)
        ));
    }

    /// O12, T25: the recovery key is shown at the terminal and taken back whole; a mistake is
    /// told and asked again, and the key is never anywhere else.
    #[test]
    fn the_recovery_key_is_shown_once_and_taken_back_whole() {
        let key = RecoveryKey::generate(&mut OsRandom).unwrap();
        let shown = key.to_display().expose().to_string();
        let typed = shown.replace('\n', " ");
        let other = RecoveryKey::generate(&mut OsRandom)
            .unwrap()
            .to_display()
            .expose()
            .replace('\n', " ");
        let s = Script::new(vec![Ok(other.as_str()), Ok(typed.as_str())]);
        let confirmed = show_and_confirm(&s, key, "~/store", "~/store/vault").unwrap();
        assert_eq!(confirmed.to_display().expose(), shown);
        let told = s.told();
        let groups = shown.lines().nth(1).unwrap();
        assert!(told.contains(&format!("    {groups}\n")), "{told}");
        assert!(
            told.contains("include ~/store/vault in your backups"),
            "{told}"
        );
        assert!(
            told.contains("That is not the recovery key shown. Type it again"),
            "{told}"
        );
        assert_eq!(s.asked(), vec![CONFIRM.to_string(), CONFIRM.to_string()]);
    }

    #[test]
    fn plan_lines_name_what_they_act_on() {
        let line = |kind: &str, object: &str, action, detail: &str| {
            plan_line(&PlannedChange {
                kind: kind.into(),
                object: object.into(),
                action,
                detail: Some(detail.into()),
            })
        };
        assert!(line(
            "Credentials",
            "prod",
            ChangeAction::Move,
            "credentials.yaml → vault"
        )
        .starts_with("  move    Hetzner token of `prod`"));
        assert!(line("Cache", "prod", ChangeAction::Delete, "x")
            .starts_with("  delete  cached secrets of `prod`"));
        assert!(line("AgeKey", "this store", ChangeAction::Create, "x")
            .starts_with("  create  age key of this store"));
        assert!(
            line("Vault", "/s/vault", ChangeAction::Delete, "x").starts_with("  delete  /s/vault")
        );
        assert!(line(
            "Credentials",
            "prod",
            ChangeAction::Move,
            "credentials.yaml → vault"
        )
        .ends_with("credentials.yaml → vault\n"));
    }

    /// d4a-d Task 13b: a token left in clear while `lock on` ran is named with the renewal
    /// that replaces it — no token on the command line at a terminal.
    #[test]
    fn a_token_left_in_clear_names_a_renewal_that_asks() {
        let done = LockedOn {
            store: "/s".into(),
            store_id: Some("0123456789abcdef".into()),
            already_locked: false,
            restarted: false,
            tokens_moved: Vec::new(),
            tokens_left: vec!["prod".into()],
            caches_resealed: 0,
            caches_dropped: Vec::new(),
            caches_left: Vec::new(),
            unlock_timeout_min: 5,
        };
        let out = on_result(&done, "/s");
        assert!(
            out.contains(
                "`apprafter target add prod --renew` stores a token in the vault (at a terminal \
                 it asks for the new token without showing it; in a script, `--token`).\n"
            ),
            "{out}"
        );
        assert!(!out.contains("--renew --token"), "{out}");
    }

    /// Rule 4 (team decision 2026-10-13): a resume asks each slot by its own question and opens
    /// that slot alone; a mistyped key is told and asked again. Enter alone is the person
    /// saying the secret is gone: the way out is told at once and the resume refused as the
    /// interrupted lock it leaves. (A recovery-key slot only: no key derivation.)
    #[test]
    fn a_resume_asks_each_slot_and_enter_alone_names_the_way_out() {
        let dir = tempfile::tempdir().unwrap();
        let made = apprafter_core::secrets::testing::locked_store(dir.path(), &[("prod", None)]);
        let key = made.recovery_key().expose().replace('\n', " ");
        let question = resume_question(LockSlotKind::RecoveryKey, "~/store");
        assert_eq!(
            question,
            "Recovery key of ~/store (Enter alone if you no longer have it): "
        );
        let s = Script::new(vec![Ok("not a recovery key"), Ok(key.as_str())]);
        let opened = ask_slot(
            &s,
            dir.path(),
            &made.store_id,
            LockSlotKind::RecoveryKey,
            "~/store",
        )
        .unwrap();
        assert_eq!(opened.kind(), LockSlotKind::RecoveryKey);
        assert_eq!(s.asked(), vec![question.clone(), question.clone()]);
        assert!(
            !s.told().is_empty() && !s.told().contains(WAY_OUT),
            "{}",
            s.told()
        );
        let s = Script::new(vec![Ok("")]);
        assert!(matches!(
            ask_slot(
                &s,
                dir.path(),
                &made.store_id,
                LockSlotKind::RecoveryKey,
                "~/store"
            ),
            Err(CliError::LockIncomplete {
                phase: cli_core::LockPhase::Locking,
                ..
            })
        ));
        assert_eq!(s.told(), WAY_OUT);
        assert_eq!(s.asked(), vec![question]);
    }
}

#[cfg(test)]
mod status_render_tests {
    use apprafter_core::lock::{
        HolderHardening, HolderUnavailableReason, LockReport, MachineLimit, SlotRefusalView,
        SlotView, UnlockWindow,
    };
    use apprafter_core::secrets::{LockSlotKind, SecretsMode};

    use super::render_status;

    fn slot(kind: LockSlotKind, label: &str) -> SlotView {
        SlotView {
            kind,
            label: label.into(),
            refused: None,
        }
    }

    fn hardening(memory_locked: bool) -> HolderHardening {
        HolderHardening {
            non_dumpable: Some(true),
            deny_attach: None,
            core_off: true,
            memory_locked,
            process_dacl: None,
        }
    }

    fn open(left_secs: u64, minutes: u32, version: &str, memory_locked: bool) -> UnlockWindow {
        UnlockWindow::Open {
            left_secs,
            minutes,
            holder_version: version.into(),
            hardening: hardening(memory_locked),
        }
    }

    fn locked(window: UnlockWindow, limits: Vec<MachineLimit>) -> LockReport {
        LockReport {
            store: "~/.config/apprafter".into(),
            store_id: Some("3f9c0a1b7d2e4c56".into()),
            targets: vec!["prod".into(), "staging".into()],
            mode: SecretsMode::Locked,
            slots: vec![
                slot(LockSlotKind::Password, "lock password"),
                slot(LockSlotKind::RecoveryKey, "recovery key"),
            ],
            window_minutes: 5,
            window,
            limits,
        }
    }

    fn text(lines: &[&str]) -> String {
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    /// D.4a §3.13: the sample, line for line — from a report whose limits come in another
    /// order than the sample's (a warning after two infos): the warnings are listed first.
    #[test]
    fn a_locked_store_with_an_open_window_reads_as_the_sample() {
        let r = locked(
            open(170, 5, env!("CARGO_PKG_VERSION"), true),
            vec![
                MachineLimit::X11Session,
                MachineLimit::PtraceScope { value: Some(0) },
                MachineLimit::Multiplexer {
                    name: "tmux".into(),
                },
                MachineLimit::InheritedWindow,
                MachineLimit::SshKeyUsable {
                    path: "~/.ssh/id_ed25519".into(),
                    unencrypted: true,
                    in_agent: Some(false),
                },
                MachineLimit::SecretsInUse,
                MachineLimit::Hibernation,
            ],
        );
        assert_eq!(
            render_status(&r),
            text(&[
                "Target store:   ~/.config/apprafter (id 3f9c0a1b7d2e4c56; targets: prod, staging)",
                "Secrets:        locked — in the vault, opened by a password or the recovery key",
                "Slots:          lock password · recovery key",
                "This terminal:  unlocked, 3 min left (a window lasts 5 min, restarted by each command that uses it)",
                "This computer:",
                "  ⚠ X11 session: any program running as you sees what you type, the lock password included",
                "  ⚠ Yama ptrace_scope is 0: programs running as you can attach to your shell",
                "  ⚠ The SSH key ~/.ssh/id_ed25519 that reaches your nodes has no passphrase",
                "  ℹ This terminal runs inside tmux: programs running as you can type into it",
                "  ℹ Everything started from this terminal can use its window, an AI agent included",
                "  ℹ While a command talks to a cluster, its kubeconfig is a file programs running as you can read",
                "  ℹ Hibernation writes memory to disk, an open window's key included",
                "The lock protects secrets at rest. While a command or the app uses one, it is in that process's memory and, for a kubeconfig, in a file programs running as you can read; the SSH key that fetches kubeconfigs stays outside the lock.",
            ])
        );
    }

    /// §3.13's variants of the window line; a holder of another version; a holder whose key is
    /// less protected than usual (T72, `window_hardening`); a slot this build refuses (L3 greps
    /// `kdf_too_weak`); nothing to report; the fallback holder base.
    #[test]
    fn every_window_variant_has_its_line() {
        for (window, line) in [
            (UnlockWindow::Closed, "This terminal:  no unlock window\n"),
            (UnlockWindow::NoTerminal, "This terminal:  not run from a terminal\n"),
            (
                UnlockWindow::EveryCommand,
                "This terminal:  every command asks (the store's window is 0 min)\n",
            ),
            (
                UnlockWindow::Unavailable {
                    reason: HolderUnavailableReason::PathTooLong,
                },
                "This terminal:  no window possible here (its socket path would be too long for this OS)\n",
            ),
            (
                UnlockWindow::Unavailable {
                    reason: HolderUnavailableReason::NotResponding,
                },
                "This terminal:  open, but its holder does not answer: every command asks; `apprafter lock now` ends it, or names the process to stop\n",
            ),
            (
                open(42, 1, "0.0.1", true),
                "This terminal:  unlocked, 42 s left (a window lasts 1 min, restarted by each command that uses it); its holder is apprafter 0.0.1\n",
            ),
        ] {
            let got = render_status(&locked(window.clone(), vec![]));
            assert!(got.contains(line), "{window:?}:\n{got}");
        }
        let weak = render_status(&locked(
            open(60, 5, env!("CARGO_PKG_VERSION"), false),
            vec![],
        ));
        assert!(
            weak.contains(
                "  ⚠ The unlock window's key is less protected than usual: its memory is not locked against paging\n"
            ),
            "{weak}"
        );
        let mut refused = locked(UnlockWindow::Closed, vec![]);
        refused.slots[0].refused = Some(SlotRefusalView::KdfTooWeak);
        assert!(render_status(&refused).contains(
            "Slots:          lock password (refused by this build: kdf_too_weak) · recovery key\n"
        ));
        assert!(render_status(&locked(UnlockWindow::Closed, vec![]))
            .contains("This computer:  nothing to report\n"));
        let fallback = render_status(&locked(
            UnlockWindow::Closed,
            vec![MachineLimit::RuntimeDirFallback {
                path: "/s/run".into(),
            }],
        ));
        assert!(
            fallback.contains(
                "This terminal:  no unlock window (holder base: /s/run, XDG_RUNTIME_DIR is not set)\n"
            ),
            "{fallback}"
        );
        assert!(
            fallback.contains("This computer:  nothing to report\n"),
            "{fallback}"
        );
    }

    /// A store in files: no id, no slots, no promise; an interrupted lock names its state.
    #[test]
    fn a_store_in_files_and_an_interrupted_one_say_so() {
        let mut files = locked(UnlockWindow::NoTerminal, vec![]);
        files.mode = SecretsMode::Files;
        files.store_id = None;
        files.slots = vec![];
        let got = render_status(&files);
        assert!(
            got.starts_with("Target store:   ~/.config/apprafter (targets: prod, staging)\n"),
            "{got}"
        );
        assert!(got.contains(
            "Secrets:        in files — readable by any program running as you; `apprafter lock on` protects them\n"
        ));
        assert!(
            !got.contains("Slots:") && !got.contains("The lock protects"),
            "{got}"
        );
        let mut locking = locked(UnlockWindow::Closed, vec![]);
        locking.mode = SecretsMode::Locking;
        assert!(render_status(&locking).contains(
            "Secrets:        locking — an earlier `apprafter lock on` did not finish: run it again, or `apprafter lock off` to back out\n"
        ));
        let mut unlocking = locked(UnlockWindow::Closed, vec![]);
        unlocking.mode = SecretsMode::Unlocking;
        assert!(render_status(&unlocking).contains(
            "Secrets:        unlocking — an earlier `apprafter lock off` did not finish: run it again\n"
        ));
    }
}

#[cfg(test)]
mod now_tests {
    use apprafter_core::CoreError;

    use super::{now_said, off_window_said};
    use crate::holder_window::Ended;

    /// T65, T59, rule 1: `lock now` says what came of the window, and a holder it could not end
    /// is an error that names it — never "no unlock window is open".
    #[test]
    fn lock_now_never_reads_a_window_it_could_not_end_as_none() {
        assert_eq!(
            now_said(Ended::None).unwrap(),
            "No unlock window is open in this terminal."
        );
        assert_eq!(
            now_said(Ended::Closed).unwrap(),
            "The unlock window in this terminal is closed."
        );
        let e = now_said(Ended::StillOpen { pid: Some(4242) }).unwrap_err();
        let said = e.to_string();
        assert!(
            said.contains("still open") && said.contains("pid 4242"),
            "{said}"
        );
        assert!(e.help.contains("4242"), "{}", e.help);
        assert!(now_said(Ended::StillOpen { pid: None }).is_err());
    }

    /// T56, rule 1: `lock off` says it ended this terminal's window, and names a holder
    /// it could not end — the store goes back to files while that holder keeps the key until it
    /// is stopped — and never stops for it (`off_window_said` only says).
    #[test]
    fn lock_off_names_a_window_it_could_not_end() {
        let closed = Some("The unlock window in this terminal is closed.".to_string());
        assert_eq!(off_window_said(Ok(Ended::Closed)), closed);
        assert_eq!(off_window_said(Ok(Ended::Stopped)), closed);
        assert_eq!(off_window_said(Ok(Ended::None)), None);
        assert_eq!(off_window_said(Err(CoreError::NoActiveTarget)), None);
        let said = off_window_said(Ok(Ended::StillOpen { pid: Some(4242) })).unwrap();
        let stop = if cfg!(windows) {
            "`taskkill /F /PID 4242`"
        } else {
            "`kill -9 4242`"
        };
        assert!(
            said.contains("could not be ended") && said.contains("pid 4242") && said.contains(stop),
            "{said}"
        );
        let said = off_window_said(Ok(Ended::StillOpen { pid: None })).unwrap();
        assert!(said.contains("could not be ended"), "{said}");
    }
}
