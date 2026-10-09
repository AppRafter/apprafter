// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The commands the webview invokes (`apprafter_desktop_ipc::COMMANDS`; JS argument names are
//! camelCase). Each one is `async` and does its work on the runtime's blocking pool: a
//! synchronous command would run on the main thread, and the work here takes locks, writes
//! files and, for `unlock`, `unlock_with_password` and `op_execute`, waits for an OS prompt or
//! a password check. Every error reaches the webview as a `UiError`; a command that panicked is
//! `apprafter::desktop::internal`.
//!
//! None of them checks the lock: the invoke handler's gate ([`crate::app::builder`]) did,
//! before the command was even parsed.
//!
//! A password from the page is moved into a [`Zeroizing`] string on the command's first line,
//! wiped when dropped, and never logged, printed or kept; so is a Hetzner token
//! (`op_start_verify_token`, `op_plan_target_renew`), moved into a [`SecretString`] there. No
//! command here is traced with its arguments.
//!
//! The D.3 commands (targets, doctor, whoami) are thin wrappers over [`target_ops`], which
//! holds their bodies and their tests.

use std::path::PathBuf;
use std::sync::Arc;

use apprafter_core::session::WhoamiReport;
use apprafter_core::ssh::{SshKeyCandidate, SshKeyInfo};
use apprafter_core::target::{TargetListReport, TargetReport};
use apprafter_core::tools::ToolchainReport;
use apprafter_core::{SecretString, UiError};
use apprafter_desktop_ipc::{
    AppInfo, CatalogueSourceArg, DraftId, LockState, OpEvent, OpId, OpSummary, PlanView, Settings,
    Subscribed, SubscriptionId, TargetAddArgs, Theme,
};
use tauri::ipc::Channel;
use tauri::{AppHandle, Runtime, State, Webview, WebviewWindow};
use zeroize::Zeroizing;

use crate::app::{self, Shell};
use crate::errors::{DesktopError, Refusal};
use crate::ops::{panic_message, EventSink};
use crate::{target_ops, window};

type ShellState<'a> = State<'a, Arc<Shell>>;

/// Run `work` on the async runtime's blocking pool. Its error is a [`DesktopError`], or a
/// [`Refusal`] carrying what the OS said.
async fn blocking<T: Send + 'static, E: Into<Refusal> + Send + 'static>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, UiError> {
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(result) => result.map_err(|e| e.into().to_ui()),
        Err(e) => Err(join_error(e).to_ui()),
    }
}

/// [`blocking`], with the shell.
async fn on_shell<T: Send + 'static, E: Into<Refusal> + Send + 'static>(
    shell: &Arc<Shell>,
    work: impl FnOnce(&Arc<Shell>) -> Result<T, E> + Send + 'static,
) -> Result<T, UiError> {
    let shell = Arc::clone(shell);
    blocking(move || work(&shell)).await
}

/// [`on_shell`] for work that cannot fail.
async fn on_shell_ok<T: Send + 'static>(
    shell: &Arc<Shell>,
    work: impl FnOnce(&Arc<Shell>) -> T + Send + 'static,
) -> Result<T, UiError> {
    on_shell(shell, move |shell| Ok::<_, DesktopError>(work(shell))).await
}

/// A blocking task that never returned: it panicked (the message says how) or was cancelled.
fn join_error(e: tauri::Error) -> DesktopError {
    match e {
        tauri::Error::JoinError(join) if join.is_panic() => DesktopError::Internal(format!(
            "the command panicked: {}",
            panic_message(&*join.into_panic())
        )),
        other => DesktopError::Internal(format!("the command did not finish: {other}")),
    }
}

/// An operation's events to one page, through the channel the page passed.
struct ChannelSink {
    channel: Channel<OpEvent>,
    webview: String,
}

impl ChannelSink {
    fn new<R: Runtime>(channel: Channel<OpEvent>, webview: &Webview<R>) -> Arc<Self> {
        Arc::new(Self {
            channel,
            webview: webview.label().to_string(),
        })
    }
}

impl EventSink for ChannelSink {
    /// `false` once the page cannot be reached.
    fn send(&self, event: &OpEvent) -> bool {
        self.channel.send(event.clone()).is_ok()
    }

    fn webview(&self) -> &str {
        &self.webview
    }
}

#[tauri::command]
pub async fn app_info(shell: ShellState<'_>) -> Result<AppInfo, UiError> {
    on_shell_ok(&shell, |shell| shell.app_info()).await
}

#[tauri::command]
pub async fn settings_get(shell: ShellState<'_>) -> Result<Settings, UiError> {
    on_shell_ok(&shell, |shell| shell.settings.get()).await
}

/// Save and apply; the settings now in use. Switching the lock on with nothing to verify
/// the owner is `apprafter::desktop::auth_unavailable`.
#[tauri::command]
pub async fn settings_set(shell: ShellState<'_>, settings: Settings) -> Result<Settings, UiError> {
    on_shell(&shell, move |shell| shell.set_settings(settings)).await
}

#[tauri::command]
pub async fn lock_status(shell: ShellState<'_>) -> Result<LockState, UiError> {
    on_shell_ok(&shell, |shell| shell.lock.state()).await
}

/// Lock now; the resulting state, which is unlocked when the lock is not in effect.
#[tauri::command]
pub async fn lock_now(shell: ShellState<'_>) -> Result<LockState, UiError> {
    on_shell_ok(&shell, |shell| shell.lock_now()).await
}

/// Ask the OS for the owner (the prompt may stay open for minutes) and unlock; the resulting
/// state.
#[tauri::command]
pub async fn unlock(shell: ShellState<'_>) -> Result<LockState, UiError> {
    on_shell(&shell, |shell| shell.unlock()).await
}

/// Unlock with the password from the lock screen's own field, which the page shows where the
/// OS cannot prompt (`AuthInfo.passwordField`, Linux's PAM path; where the OS prompts itself the
/// answer is `auth_unavailable` with `use_system_prompt`); the resulting state. As `unlock`: one
/// check at a time (`auth_busy`), and a lock while it runs refuses its yes. A refusal may carry
/// what the OS said as `fields.messages`.
#[tauri::command]
pub async fn unlock_with_password(
    shell: ShellState<'_>,
    password: String,
) -> Result<LockState, UiError> {
    let password = Zeroizing::new(password);
    on_shell(&shell, move |shell| shell.unlock_with_password(password)).await
}

/// The owner did something: the idle time starts again.
#[tauri::command]
pub async fn activity(shell: ShellState<'_>) -> Result<(), UiError> {
    on_shell_ok(&shell, |shell| {
        shell.lock.activity();
    })
    .await
}

/// Quit: answers once nothing new can start; the app exits when the running operations have
/// stopped (or after `app::STOP_BOUND`).
#[tauri::command]
pub async fn quit<R: Runtime>(app: AppHandle<R>, shell: ShellState<'_>) -> Result<(), UiError> {
    on_shell_ok(&shell, move |shell| {
        app::quit(&app, shell);
    })
    .await
}

/// The running and recently ended operations, the latest started first.
#[tauri::command]
pub async fn op_list(shell: ShellState<'_>) -> Result<Vec<OpSummary>, UiError> {
    on_shell_ok(&shell, |shell| shell.ops.list()).await
}

/// Follow an operation (or a plan): the events so far, and every later one on `on_event`.
#[tauri::command]
pub async fn op_subscribe<R: Runtime>(
    webview: Webview<R>,
    shell: ShellState<'_>,
    op_id: OpId,
    on_event: Channel<OpEvent>,
) -> Result<Subscribed, UiError> {
    let sink = ChannelSink::new(on_event, &webview);
    on_shell(&shell, move |shell| shell.ops.subscribe(op_id, sink)).await
}

/// End one subscription; an operation that has ended or gone is no error.
#[tauri::command]
pub async fn op_unsubscribe(
    shell: ShellState<'_>,
    op_id: OpId,
    subscription: SubscriptionId,
) -> Result<(), UiError> {
    on_shell_ok(&shell, move |shell| {
        shell.ops.unsubscribe(op_id, subscription);
    })
    .await
}

/// Stop an operation, or refuse its open prompt, or drop its plan.
#[tauri::command]
pub async fn op_cancel(shell: ShellState<'_>, op_id: OpId) -> Result<(), UiError> {
    on_shell(&shell, move |shell| shell.ops.cancel(op_id)).await
}

/// Forget an ended operation or a plan.
#[tauri::command]
pub async fn op_discard(shell: ShellState<'_>, op_id: OpId) -> Result<(), UiError> {
    on_shell_ok(&shell, move |shell| {
        shell.ops.discard(op_id);
    })
    .await
}

/// Run a plan, once, `on_event` following it from before the OS prompt (when the plan needs
/// one); the subscription that channel holds, for `op_unsubscribe`.
///
/// A plan that does not run says so twice, and the page shows it once. The command's
/// rejection is authoritative: it is the answer to this call, and it alone says the call
/// failed. The `Failed` event it may also send on `on_event` — the channel this call has just
/// subscribed — carries the same error: it is meant for the pages that subscribed to the plan
/// earlier (through `op_subscribe`), which have no other way to hear it. A page that handles
/// the rejection ignores that event on its own channel. Some refusals are not the plan's end,
/// and send nothing: a busy prompt (`apprafter::desktop::auth_busy`), a failed gesture
/// (`apprafter::desktop::auth_failed`: a wrong password, a finger not recognised, or the
/// back-off, which says how long it still refuses as `fields.retryInMs`), a gesture asked the
/// way that is not there while the other is (`apprafter::desktop::auth_unavailable` with
/// `no_agent`, `use_password_field` or `use_system_prompt`), and an expired password (`password_expired`, which the
/// owner changes first). The plan then waits for the owner to try again with the same `op_id`,
/// and the channel's subscription has already ended.
///
/// `password`, when the page sends one, is the confirm dialog's own field (shown where the OS
/// cannot prompt, `AuthInfo.passwordField`): a plan that needs the gesture checks it in place of
/// the OS's prompt, and a refusal may carry what the OS said as `fields.messages`. Without it
/// the gesture is the OS's prompt.
#[tauri::command]
pub async fn op_execute<R: Runtime>(
    webview: Webview<R>,
    shell: ShellState<'_>,
    op_id: OpId,
    on_event: Channel<OpEvent>,
    password: Option<String>,
) -> Result<SubscriptionId, UiError> {
    let password = password.map(Zeroizing::new);
    let sink = ChannelSink::new(on_event, &webview);
    on_shell(&shell, move |shell| {
        shell.execute_with(op_id, sink, password)
    })
    .await
}

/// The page has painted: the window, created hidden so it never flashes white, shows. The one
/// command with no blocking work: showing the window is a message to the event loop.
#[tauri::command]
pub async fn window_ready<R: Runtime>(window: WebviewWindow<R>) -> Result<(), UiError> {
    window::reveal(&window)
        .map_err(|e| DesktopError::Internal(format!("the window could not be shown: {e}")).to_ui())
}

/// Give the native window the theme the setting `theme` names ([`crate::theme`]): Light and
/// Dark as they are; System left to the OS on macOS and Windows, and on Linux resolved from the
/// desktop's colour scheme and followed while it stays System. Answered while locked, so the
/// lock screen has its theme too. No blocking work: the theme is a message to the event loop.
#[tauri::command]
pub async fn theme_apply<R: Runtime>(
    window: WebviewWindow<R>,
    shell: ShellState<'_>,
    theme: Theme,
) -> Result<(), UiError> {
    shell
        .appearance
        .apply(theme, |native| window.set_theme(native))
        .map_err(|e| {
            DesktopError::Internal(format!("the window theme was not applied: {e}")).to_ui()
        })
}

// D.3: targets, doctor, whoami (overview §3.12.1). Each runs on the blocking pool; a read
// answers with the id of the operation the page follows, a plan with its view.

/// Every target in the store, the unreadable ones listed apart.
#[tauri::command]
pub async fn target_list(shell: ShellState<'_>) -> Result<TargetListReport, UiError> {
    on_shell(&shell, |shell| target_ops::target_list(shell)).await
}

/// One target in full; an unknown name is `apprafter::target::not_found`.
#[tauri::command]
pub async fn target_show(shell: ShellState<'_>, name: String) -> Result<TargetReport, UiError> {
    on_shell(&shell, move |shell| target_ops::target_show(shell, &name)).await
}

/// The public keys under `~/.ssh` the key picker offers.
#[tauri::command]
pub async fn ssh_key_candidates(shell: ShellState<'_>) -> Result<Vec<SshKeyCandidate>, UiError> {
    on_shell(&shell, |shell| target_ops::ssh_key_candidates(shell)).await
}

/// A public key file as the clients show it.
#[tauri::command]
pub async fn ssh_key_inspect(shell: ShellState<'_>, path: String) -> Result<SshKeyInfo, UiError> {
    on_shell(&shell, move |shell| {
        target_ops::ssh_key_inspect(shell, &path)
    })
    .await
}

/// Every tool the app runs and where it was looked for (the probes bounded by the core).
#[tauri::command]
pub async fn toolchain_status(shell: ShellState<'_>) -> Result<ToolchainReport, UiError> {
    on_shell(&shell, |shell| target_ops::toolchain_status(shell)).await
}

/// Who the app acts as and the CLI's default target, without a ping (`op_start_whoami` pings).
#[tauri::command]
pub async fn whoami(shell: ShellState<'_>) -> Result<WhoamiReport, UiError> {
    on_shell(&shell, |shell| target_ops::whoami(shell)).await
}

/// Verify a provider token (a read the page follows): the result names a draft, never the
/// token, which crosses IPC this once and is wiped when its draft goes.
#[tauri::command]
pub async fn op_start_verify_token(
    shell: ShellState<'_>,
    provider: String,
    token: String,
) -> Result<OpId, UiError> {
    let token = SecretString::from(Zeroizing::new(token));
    on_shell(&shell, move |shell| {
        target_ops::start_verify_token(shell, provider, token)
    })
    .await
}

/// Read the regions and machines a picker offers, with a draft's token or a stored target's.
#[tauri::command]
pub async fn op_start_machine_catalogue(
    shell: ShellState<'_>,
    source: CatalogueSourceArg,
) -> Result<OpId, UiError> {
    on_shell(&shell, move |shell| {
        target_ops::start_machine_catalogue(shell, source)
    })
    .await
}

/// Measure how far each region is from this computer.
#[tauri::command]
pub async fn op_start_region_latencies(
    shell: ShellState<'_>,
    regions: Vec<String>,
) -> Result<OpId, UiError> {
    on_shell(&shell, move |shell| {
        target_ops::start_region_latencies(shell, regions)
    })
    .await
}

/// Run doctor on the target called `target`.
#[tauri::command]
pub async fn op_start_doctor(shell: ShellState<'_>, target: String) -> Result<OpId, UiError> {
    on_shell(&shell, move |shell| target_ops::start_doctor(shell, target)).await
}

/// whoami with a ping of the CLI default's stored token.
#[tauri::command]
pub async fn op_start_whoami(shell: ShellState<'_>) -> Result<OpId, UiError> {
    on_shell(&shell, |shell| target_ops::start_whoami(shell)).await
}

/// Plan adding a target with a verified token's draft (Bounded); the plan takes the draft.
#[tauri::command]
pub async fn op_plan_target_add(
    shell: ShellState<'_>,
    args: TargetAddArgs,
) -> Result<PlanView, UiError> {
    on_shell(&shell, move |shell| {
        target_ops::plan_target_add(shell, args)
    })
    .await
}

/// Plan renewing `name` (Bounded): a new token, a new SSH key path, or both. With no token the
/// key alone changes and the credentials are kept; a new token is checked with the provider
/// when the plan runs, and only then saved.
#[tauri::command]
pub async fn op_plan_target_renew(
    shell: ShellState<'_>,
    name: String,
    token: Option<String>,
    ssh_key: Option<String>,
) -> Result<PlanView, UiError> {
    let token = token.map(|t| SecretString::from(Zeroizing::new(t)));
    on_shell(&shell, move |shell| {
        target_ops::plan_target_renew(shell, &name, token, ssh_key.map(PathBuf::from))
    })
    .await
}

/// Plan making `name` the CLI's default (Reversible: the page runs it at once).
#[tauri::command]
pub async fn op_plan_target_use(shell: ShellState<'_>, name: String) -> Result<PlanView, UiError> {
    on_shell(&shell, move |shell| {
        target_ops::plan_target_use(shell, &name)
    })
    .await
}

/// Plan renaming `from` to `to` (Bounded).
#[tauri::command]
pub async fn op_plan_target_rename(
    shell: ShellState<'_>,
    from: String,
    to: String,
) -> Result<PlanView, UiError> {
    on_shell(&shell, move |shell| {
        target_ops::plan_target_rename(shell, &from, &to)
    })
    .await
}

/// Plan removing `name` from this computer (Destructive: the gesture runs inside `op_execute`).
#[tauri::command]
pub async fn op_plan_target_remove(
    shell: ShellState<'_>,
    name: String,
) -> Result<PlanView, UiError> {
    on_shell(&shell, move |shell| {
        target_ops::plan_target_remove(shell, &name)
    })
    .await
}

/// Plan changing the machine of `name` (Bounded); a provisioned target is refused.
#[tauri::command]
pub async fn op_plan_target_machine(
    shell: ShellState<'_>,
    name: String,
    sku: String,
    region: Option<String>,
) -> Result<PlanView, UiError> {
    on_shell(&shell, move |shell| {
        target_ops::plan_target_machine(shell, &name, sku, region)
    })
    .await
}

/// The add wizard closed: its draft goes. An unknown draft is no error.
#[tauri::command]
pub async fn target_draft_discard(shell: ShellState<'_>, draft_id: DraftId) -> Result<(), UiError> {
    on_shell_ok(&shell, move |shell| {
        target_ops::draft_discard(shell, draft_id)
    })
    .await
}

#[cfg(test)]
mod tests {
    use apprafter_desktop_ipc::errors;

    use super::blocking;
    use crate::errors::{DesktopError, Refusal};

    #[test]
    fn a_command_that_panics_is_an_internal_error_with_the_message() {
        let result: Result<(), _> =
            tauri::async_runtime::block_on(blocking(|| -> Result<(), DesktopError> {
                panic!("the core broke")
            }));
        let ui = result.unwrap_err();
        assert_eq!(ui.code.as_deref(), Some(errors::INTERNAL));
        assert!(ui.message.contains("the core broke"), "{}", ui.message);
    }

    #[test]
    fn a_command_error_reaches_the_webview_as_its_ui_error() {
        let result: Result<(), _> =
            tauri::async_runtime::block_on(blocking(|| Err(DesktopError::Closing)));
        assert_eq!(result.unwrap_err().code.as_deref(), Some(errors::CLOSING));
        let ok = tauri::async_runtime::block_on(blocking(|| Ok::<_, DesktopError>(7)));
        assert_eq!(ok.unwrap(), 7);
    }

    #[test]
    fn a_refusal_reaches_the_webview_with_what_the_os_said() {
        let result: Result<(), _> = tauri::async_runtime::block_on(blocking(|| {
            Err(Refusal::new(
                DesktopError::AuthFailed {
                    exhausted: false,
                    retry_in_ms: None,
                },
                vec!["Password expired".into()],
            ))
        }));
        let ui = result.unwrap_err();
        assert_eq!(ui.code.as_deref(), Some(errors::AUTH_FAILED));
        assert_eq!(
            ui.fields["messages"],
            serde_json::json!(["Password expired"])
        );
    }
}
