// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! AppRafter Desktop (ADR 0067): the GUI twin of the apprafter CLI over `apprafter-core`.

pub mod app;
pub mod auth;
pub mod auth_cache;
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

/// The product's name as people see it: the window title and the macOS app menu. Not Tauri's
/// `productName`, which tauri.linux.conf.json5 sets to the binary's name so the Linux packages
/// are called `apprafter-desktop`; tauri.conf.json5 keeps it equal to this everywhere else.
pub const PRODUCT_NAME: &str = "AppRafter";

use std::error::Error;
use std::sync::Arc;

use tauri::utils::config::AppDirectoriesOverride;
use tauri::{Manager, RunEvent};

use crate::env::AllowListEnv;
use crate::ops::{Clock, SystemClock};
use crate::settings::SettingsStore;

/// Build and run the app. Returns only when it could not start; once running, the process
/// exits from the event loop.
///
/// In order: the async runtime and the crypto provider (before anything of Tauri's), the
/// allow-listed environment and the core context, the app's identity and directories (a
/// data-directory override moves every app directory and keys the single-instance lock on
/// it; one set but empty or not Unicode stops the start, [`exit_code`] 2), the authenticator
/// ([`auth::choice`]: the OS's in a release, the fake in a test build), the app itself (the
/// single-instance plugin first: a second launch only focuses the first window and exits; on
/// macOS, the app menu), then the log, the settings and the shell, the tickers, the OS session
/// watch (lock-on-sleep) and, on Linux and macOS, the quit signals. On Windows the prompts are
/// parented to the main window as soon as it is built.
///
/// The log starts once the app is built, so a second launch, which exits while the plugins
/// start, writes nothing to the running app's log. It is still up before the window: Tauri
/// runs `setup`, which builds it, on the event loop's first event inside `App::run`, so a
/// window or webview failure — and the panic Tauri makes of a `setup` error — reach the file.
///
/// With an override the webview's storage moves too, except on macOS 13: WKWebView keeps its
/// own store, given one per override on macOS 14 and later only ([`window::build_main`]).
///
/// Every way out the app is told of runs the quit sequence ([`app`]'s module docs): the
/// event loop's exit request, a quit signal, and the event loop's exit itself, which an exit
/// the OS forces reaches with no request before it. Each drops the OS session watch once the
/// running operations have stopped.
pub fn run() -> Result<(), Box<dyn Error>> {
    runtime::init_runtime()?;
    runtime::install_crypto();
    let env = AllowListEnv::from_process(cfg!(feature = "test-build"));
    let context = env::desktop_context(&env)?;
    // Created and canonical (absolute against the working directory, as the CLI reads
    // APPRAFTER_CONFIG_DIR — Tauri would resolve a relative one against the binary's
    // directory), so every spelling of one directory is one instance on one set of files. A
    // broken override stops the start here, before anything could use the owner's files.
    let data_dir = env::data_dir_override(&env)?
        .map(|dir| env::prepare_data_dir(&dir))
        .transpose()?;

    let mut tauri_context = tauri::generate_context!();
    let config = tauri_context.config_mut();
    config.identifier = env::instance_identifier(&config.identifier, data_dir.as_deref());
    if let Some(dir) = &data_dir {
        // Config, data and local data are `dir`, and so is the webview's own storage on Linux
        // and Windows (macOS: `window::build_main`); the log goes to `dir/logs`.
        config.app.app_directories_override = Some(AppDirectoriesOverride::Root(dir.clone()));
    }

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let auth = auth::authenticator(auth::choice(&env), clock.clone());

    let cell = app::ShellCell::default();
    let builder = app::builder(tauri::Builder::default(), cell.clone())
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            window::show_and_focus(app)
        }))
        // The capability lets the page open three URLs with it, nothing else.
        .plugin(app::opener_plugin())
        .setup({
            let data_dir = data_dir.clone();
            #[cfg(windows)]
            let auth = auth.clone();
            move |app| {
                let main = window::build_main(app.handle(), data_dir.as_deref())?;
                // Windows: until the prompts have a parent, a request opens nothing.
                #[cfg(windows)]
                auth.set_window(main.hwnd()?.0 as isize);
                #[cfg(not(windows))]
                drop(main);
                tracing::info!("the main window is open");
                Ok(())
            }
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
        auth,
        clock,
        context,
        env.test_build(),
        move |state| app::emit_lock_changed(&handle, state),
    );
    app::install(&app, &cell, shell.clone())?;
    app::start_tickers(&shell)?;
    // The OS's lock and sleep signals, until a quit drops the watch. Started here, before the
    // event loop runs: macOS delivers them through it.
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    shell.watch_session(|on_event| Box::new(apprafter_os_auth::watch(on_event)));
    #[cfg(unix)]
    if let Err(e) = app::quit_on_signals(app.handle(), &shell, &signals::QUIT_SIGNALS) {
        tracing::warn!("a signal will end the app without its quit sequence: {e}");
    }

    app.run(move |app, event| match event {
        RunEvent::ExitRequested { api, .. } => app::on_exit_requested(app, &shell, &api),
        RunEvent::Exit => shell.on_exit(),
        _ => {}
    });
    Ok(())
}

/// The process's exit code when [`run`] returns `error`: 2 for a data-directory override the
/// app refuses ([`env::DataDirError`]), a usage error like the CLI's; 1 for anything else.
pub fn exit_code(error: &(dyn Error + 'static)) -> i32 {
    if error.is::<env::DataDirError>() {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use crate::env::DataDirError;

    /// The name people see is the configured product name everywhere but Linux, where the
    /// packaging names the product after the binary: the menu and the window title must not
    /// follow tauri.linux.conf.json5 there (they once did, and the menu said "Quit apprafter-desktop").
    #[test]
    fn the_product_name_is_the_config_s_except_in_linux_packaging() {
        let base = include_str!("../tauri.conf.json5");
        let linux = include_str!("../tauri.linux.conf.json5");
        assert!(
            base.contains(&format!("productName: \"{}\",", super::PRODUCT_NAME)),
            "tauri.conf.json5's productName is PRODUCT_NAME"
        );
        assert!(base.contains("mainBinaryName: \"apprafter-desktop\","));
        assert!(
            linux.contains("productName: \"apprafter-desktop\","),
            "on Linux the package is named after the binary"
        );
    }

    #[test]
    fn a_refused_data_dir_exits_2_and_anything_else_1() {
        let refused: Box<dyn Error> = Box::new(DataDirError::Empty);
        assert_eq!(super::exit_code(&*refused), 2);
        let other: Box<dyn Error> = "the app could not be built".into();
        assert_eq!(super::exit_code(&*other), 1);
    }
}
