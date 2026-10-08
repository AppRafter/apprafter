// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The commands the webview invokes (`apprafter_desktop_ipc::COMMANDS`; JS argument names are
//! camelCase). Each one is `async` and does its work on the runtime's blocking pool: a
//! synchronous command would run on the main thread, and the work here takes locks, writes
//! files and, for `unlock` and `op_execute`, waits for an OS prompt. Every error reaches the
//! webview as a `UiError`; a command that panicked is `apprafter::desktop::internal`.
//!
//! None of them checks the lock: the invoke handler's gate ([`crate::app::builder`]) did,
//! before the command was even parsed.

use std::sync::Arc;

use apprafter_core::UiError;
use apprafter_desktop_ipc::{
    AppInfo, LockState, OpEvent, OpId, OpSummary, Settings, Subscribed, SubscriptionId,
};
use tauri::ipc::Channel;
use tauri::{AppHandle, Runtime, State, Webview};

use crate::app::{self, Shell};
use crate::errors::DesktopError;
use crate::ops::{panic_message, EventSink};

type ShellState<'a> = State<'a, Arc<Shell>>;

/// Run `work` on the async runtime's blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, DesktopError> + Send + 'static,
) -> Result<T, UiError> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .unwrap_or_else(|e| Err(join_error(e)))
        .map_err(|e| e.to_ui())
}

/// [`blocking`], with the shell.
async fn on_shell<T: Send + 'static>(
    shell: &Arc<Shell>,
    work: impl FnOnce(&Arc<Shell>) -> Result<T, DesktopError> + Send + 'static,
) -> Result<T, UiError> {
    let shell = Arc::clone(shell);
    blocking(move || work(&shell)).await
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
    on_shell(&shell, |shell| Ok(shell.app_info())).await
}

#[tauri::command]
pub async fn settings_get(shell: ShellState<'_>) -> Result<Settings, UiError> {
    on_shell(&shell, |shell| Ok(shell.settings.get())).await
}

/// Save and apply; the settings now in use. Switching the lock on with nothing to verify
/// the owner is `apprafter::desktop::auth_unavailable`.
#[tauri::command]
pub async fn settings_set(shell: ShellState<'_>, settings: Settings) -> Result<Settings, UiError> {
    on_shell(&shell, move |shell| shell.set_settings(settings)).await
}

#[tauri::command]
pub async fn lock_status(shell: ShellState<'_>) -> Result<LockState, UiError> {
    on_shell(&shell, |shell| Ok(shell.lock.state())).await
}

/// Lock now; the resulting state, which is unlocked when the lock is not in effect.
#[tauri::command]
pub async fn lock_now(shell: ShellState<'_>) -> Result<LockState, UiError> {
    on_shell(&shell, |shell| Ok(shell.lock_now())).await
}

/// Ask the OS for the owner (the prompt may stay open for minutes) and unlock; the resulting
/// state.
#[tauri::command]
pub async fn unlock(shell: ShellState<'_>) -> Result<LockState, UiError> {
    on_shell(&shell, |shell| shell.unlock()).await
}

/// The owner did something: the idle time starts again.
#[tauri::command]
pub async fn activity(shell: ShellState<'_>) -> Result<(), UiError> {
    on_shell(&shell, |shell| {
        shell.lock.activity();
        Ok(())
    })
    .await
}

/// Quit: answers once nothing new can start; the app exits when the running operations have
/// stopped (or after `app::STOP_BOUND`).
#[tauri::command]
pub async fn quit<R: Runtime>(app: AppHandle<R>, shell: ShellState<'_>) -> Result<(), UiError> {
    on_shell(&shell, move |shell| {
        app::quit(&app, shell);
        Ok(())
    })
    .await
}

/// The running and recently ended operations, the latest started first.
#[tauri::command]
pub async fn op_list(shell: ShellState<'_>) -> Result<Vec<OpSummary>, UiError> {
    on_shell(&shell, |shell| Ok(shell.ops.list())).await
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
    on_shell(&shell, move |shell| {
        shell.ops.unsubscribe(op_id, subscription);
        Ok(())
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
    on_shell(&shell, move |shell| {
        shell.ops.discard(op_id);
        Ok(())
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
/// the rejection ignores that event on its own channel; on a busy prompt
/// (`apprafter::desktop::auth_busy`) nothing is sent, the plan waits, and the channel's
/// subscription has already ended.
#[tauri::command]
pub async fn op_execute<R: Runtime>(
    webview: Webview<R>,
    shell: ShellState<'_>,
    op_id: OpId,
    on_event: Channel<OpEvent>,
) -> Result<SubscriptionId, UiError> {
    let sink = ChannelSink::new(on_event, &webview);
    on_shell(&shell, move |shell| shell.execute(op_id, sink)).await
}

#[cfg(test)]
mod tests {
    use apprafter_desktop_ipc::errors;

    use super::blocking;

    #[test]
    fn a_command_that_panics_is_an_internal_error_with_the_message() {
        let result: Result<(), _> =
            tauri::async_runtime::block_on(blocking(|| panic!("the core broke")));
        let ui = result.unwrap_err();
        assert_eq!(ui.code.as_deref(), Some(errors::INTERNAL));
        assert!(ui.message.contains("the core broke"), "{}", ui.message);
    }

    #[test]
    fn a_command_error_reaches_the_webview_as_its_ui_error() {
        let result: Result<(), _> =
            tauri::async_runtime::block_on(blocking(|| Err(crate::errors::DesktopError::Closing)));
        assert_eq!(result.unwrap_err().code.as_deref(), Some(errors::CLOSING));
        let ok = tauri::async_runtime::block_on(blocking(|| Ok(7)));
        assert_eq!(ok.unwrap(), 7);
    }
}
