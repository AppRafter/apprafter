// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! AppRafter Desktop (ADR 0067): the GUI twin of the apprafter CLI over `apprafter-core`.

pub mod app;
pub mod auth;
pub mod commands;
pub mod env;
pub mod errors;
pub mod lock;
pub mod menu;
pub mod ops;
pub mod runtime;
pub mod settings;
pub mod signals;
pub mod window;

use std::error::Error;
use std::sync::Arc;

use tauri::utils::config::AppDirectoriesOverride;
use tauri::{Manager, RunEvent};

use crate::auth::Authenticator;
use crate::env::AllowListEnv;
use crate::ops::SystemClock;
use crate::settings::SettingsStore;

/// Build and run the app. Returns only when it could not start; once running, the process
/// exits from the event loop.
///
/// In order: the async runtime and the crypto provider (before anything of Tauri's), the
/// allow-listed environment and the core context, the app's identity and directories (a
/// data-directory override moves every app directory and keys the single-instance lock on
/// it), the app itself (the single-instance plugin first: a second launch only focuses the
/// first window and exits; on macOS, the app menu), then the log, the settings and the shell,
/// the tickers and, on Linux and macOS, the quit signals.
///
/// Every way out the app is told of runs the quit sequence ([`app`]'s module docs): the
/// event loop's exit request, a quit signal, and the event loop's exit itself, which an exit
/// the OS forces reaches with no request before it.
pub fn run() -> Result<(), Box<dyn Error>> {
    runtime::init_runtime()?;
    runtime::install_crypto();
    let env = AllowListEnv::from_process(cfg!(feature = "test-build"));
    let context = env::desktop_context(&env)?;
    // Absolute against the working directory, as the CLI reads APPRAFTER_CONFIG_DIR: Tauri
    // would resolve a relative override against the binary's directory instead.
    let data_dir = env::data_dir_override(&env)
        .map(std::path::absolute)
        .transpose()?;

    let mut tauri_context = tauri::generate_context!();
    let config = tauri_context.config_mut();
    config.identifier = env::instance_identifier(&config.identifier, data_dir.as_deref());
    if let Some(dir) = &data_dir {
        // Config, data and local data (the webview's own storage too) are `dir`; the log
        // goes to `dir/logs`.
        config.app.app_directories_override = Some(AppDirectoriesOverride::Root(dir.clone()));
    }

    let cell = app::ShellCell::default();
    let builder = app::builder(tauri::Builder::default(), cell.clone())
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            window::show_and_focus(app)
        }))
        .setup(|app| {
            window::build_main(app.handle())?;
            Ok(())
        });
    // Cmd+Q quits through the app, not through `terminate:` (see `menu`).
    #[cfg(target_os = "macos")]
    let builder = builder
        .menu(menu::app_menu)
        .on_menu_event(|app, event| menu::on_menu_event(app, &event));
    let app = builder.build(tauri_context)?;

    match app.path().app_log_dir() {
        Ok(dir) => {
            if let Err(e) = runtime::init_logging(&dir) {
                eprintln!("AppRafter Desktop runs without a log file: {e}");
            }
        }
        Err(e) => eprintln!("AppRafter Desktop runs without a log file: {e}"),
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        test_build = env.test_build(),
        data_dir = ?data_dir,
        "AppRafter Desktop is starting"
    );

    let settings = SettingsStore::load(&app.path().app_config_dir()?, &SystemClock);
    if let Some(notice) = settings.notice() {
        tracing::warn!("{notice}");
    }
    let handle = app.handle().clone();
    let shell = app::Shell::new(
        settings,
        authenticator(),
        Arc::new(SystemClock),
        context,
        env.test_build(),
        move |state| app::emit_lock_changed(&handle, state),
    );
    app::install(&app, &cell, shell.clone())?;
    app::start_tickers(&shell)?;
    #[cfg(unix)]
    {
        let (handle, shell) = (app.handle().clone(), shell.clone());
        let quit = move |_| app::quit(&handle, &shell);
        if let Err(e) = signals::on_quit_signals(&signals::QUIT_SIGNALS, quit) {
            tracing::warn!("a signal will end the app without its quit sequence: {e}");
        }
    }

    app.run(move |app, event| match event {
        RunEvent::ExitRequested { api, .. } => app::on_exit_requested(app, &shell, &api),
        RunEvent::Exit => shell.on_exit(),
        _ => {}
    });
    Ok(())
}

/// Until the OS backends (D.2d): the scripted fake in a test build, and in a release nothing —
/// so the lock and every gesture fail closed.
fn authenticator() -> Arc<dyn Authenticator> {
    #[cfg(feature = "test-build")]
    let auth: Arc<dyn Authenticator> = Arc::new(auth::FakeAuthenticator::new());
    #[cfg(not(feature = "test-build"))]
    let auth: Arc<dyn Authenticator> = Arc::new(auth::NoAuthenticator);
    auth
}
