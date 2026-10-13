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

use apprafter_core::lock::{
    self, check_password, LockOffPayload, LockOffStart, LockOnInput, LockOnPayload, LockOnStart,
    LockedOff, LockedOn, PasswordAdvice, PasswordProblem, RecoveryKeyProblem, SlotOpened,
};
use apprafter_core::secrets::{LockSlotKind, SecretsMode, StoreId};
use apprafter_core::vault::{ConfirmedRecoveryKey, KdfPolicy, OsRandom, RecoveryKey};
use apprafter_core::{
    CancellationToken, ChangeAction, Context, CoreError, Event, Plan, PlannedChange, Reporter,
    SecretString, Zeroizing,
};
use cli_core::{CliError, LockPhase};

use crate::cli::LockCommand;
use crate::commands::target::completed;
use crate::prompt::{Asker, PromptError, Terminal};
use crate::render::core_error::report;
use crate::render::reporter::CliReporter;
use crate::vault_access::{ask_and_open, prompt_refusal, sentence, ATTEMPTS};

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
    eprintln!("Unlocking...");
    let done = completed(
        lock::execute_off(&ctx, plan, opened, &Progress, &CancellationToken::new())
            .map_err(report)?,
    )?;
    print!("{}", off_result(&done, &shown));
    Ok(())
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
    if let Some(path) = &ctx.overrides().age_key {
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
             written while this ran; it is not used. `apprafter target add {t} --renew --token \
             <token>` stores a token in the vault.\n"
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
    if done.age_key_created {
        out.push_str(&format!(
            "Created the shared age key at {}: the cached secrets are encrypted to it.\n",
            done.age_key
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

    /// Rule 4 (team decision 2026-10-13): a resume asks each slot by its own question and opens
    /// that slot alone; a mistyped key is told and asked again. Enter alone is the person
    /// saying the secret is gone: the way out is told at once and the resume refused as the
    /// interrupted lock it leaves. (A recovery-key slot only: no key derivation.)
    #[test]
    fn a_resume_asks_each_slot_and_enter_alone_names_the_way_out() {
        let dir = tempfile::TempDir::new_in("/tmp").unwrap();
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
