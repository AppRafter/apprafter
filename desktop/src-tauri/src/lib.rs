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
pub mod single_instance;
pub mod theme;
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
use crate::single_instance::SingleInstance;

/// Build and run the app. Returns only when it could not start; once running, the process
/// exits from the event loop.
///
/// In order: on Linux, WebKitGTK's DMA-BUF renderer turned off under Wayland on NVIDIA's driver
/// ([`env::turn_off_dmabuf_renderer_on_nvidia_wayland`]: the process restarts with it off,
/// before anything else starts; logged once the log starts), the async runtime and the crypto
/// provider (before anything of Tauri's), the allow-listed environment and the core context,
/// the app's identity and directories (a data-directory override moves every app directory and
/// keys the single-instance lock on it; one set but empty or not Unicode stops the start,
/// [`exit_code`] 2), the authenticator ([`auth::choice`]: the OS's in a release, the fake in a
/// test build), the single-instance lock ([`single_instance`]: on Linux only when the session
/// bus answers; without it the app starts all the same, and its log says why), the app itself
/// (the lock's plugin first: a second launch only focuses the first window and exits; on
/// macOS, the app menu), then the log, the settings and the shell, on Linux the window's theme
/// and the desktop's colour scheme ([`theme::start`]), the tickers, the OS session watch
/// (lock-on-sleep) and, on Linux and macOS, the quit signals. On Windows the prompts are
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
    // First, before the app starts anything: on NVIDIA under Wayland this restarts the process
    // with WebKitGTK's DMA-BUF renderer off, and nothing started before it would survive that.
    #[cfg(target_os = "linux")]
    let dmabuf_renderer = env::turn_off_dmabuf_renderer_on_nvidia_wayland();
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
    // Linux: the plugin panics on a session bus address that does not parse, so the bus is
    // checked first, and the app starts without the lock when it cannot keep it.
    let single_instance = SingleInstance::for_this_launch();

    let cell = app::ShellCell::default();
    let builder = single_instance
        .register(app::builder(tauri::Builder::default(), cell.clone()))
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
    #[cfg(target_os = "linux")]
    dmabuf_renderer.log();
    single_instance.log();

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
    // Linux: GTK's own preference for a dark theme before anything sets it, the stored theme
    // on the window before it exists, and the desktop's colour scheme followed from here on.
    #[cfg(target_os = "linux")]
    theme::start(app.handle(), &shell.appearance);
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

    /// `run`, as this file spells it.
    fn run_fn() -> syn::ItemFn {
        let file = syn::parse_file(include_str!("lib.rs")).unwrap();
        file.items
            .into_iter()
            .find_map(|item| match item {
                syn::Item::Fn(f) if f.sig.ident == "run" => Some(f),
                _ => None,
            })
            .expect("fn run")
    }

    /// The path of the function a statement calls, through a `?`: `["app", "install"]` for
    /// `app::install(..)?;`.
    fn called(stmt: &syn::Stmt) -> Option<(Vec<String>, &syn::ExprCall)> {
        let syn::Stmt::Expr(expr, _) = stmt else {
            return None;
        };
        let expr = match expr {
            syn::Expr::Try(attempt) => &*attempt.expr,
            expr => expr,
        };
        let syn::Expr::Call(call) = expr else {
            return None;
        };
        let syn::Expr::Path(path) = &*call.func else {
            return None;
        };
        let names = path.path.segments.iter().map(|s| s.ident.to_string());
        Some((names.collect(), call))
    }

    /// The System theme on Linux needs `theme::start`: it gives the window the stored theme
    /// before the window exists, and starts the portal's watch. Without it `theme_apply`
    /// resolves System knowing nothing — light on a dark desktop, the bug it fixed — and no
    /// other test fails. So `run` calls it on Linux, with the shell's own `appearance` (the one
    /// `theme_apply` uses), once `app::install` has put the shell in place, and before
    /// `app.run`, whose event loop builds the window.
    #[test]
    fn run_starts_the_theme_on_linux_before_the_window() {
        let run = run_fn();
        let stmts = &run.block.stmts;
        let position = |name: [&str; 2]| {
            stmts
                .iter()
                .position(|stmt| called(stmt).is_some_and(|(path, _)| path == name))
                .unwrap_or_else(|| panic!("run calls {}", name.join("::")))
        };
        let install = position(["app", "install"]);
        let theme = position(["theme", "start"]);
        let window = stmts
            .iter()
            .position(|stmt| {
                matches!(stmt, syn::Stmt::Expr(syn::Expr::MethodCall(call), _)
                    if call.method == "run")
            })
            .expect("run ends in app.run");
        assert!(
            install < theme && theme < window,
            "app::install at {install}, theme::start at {theme}, app.run at {window}"
        );
        let (_, call) = called(&stmts[theme]).unwrap();
        let cfg: Vec<String> = call
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("cfg"))
            .map(|attr| attr.meta.require_list().unwrap().tokens.to_string())
            .collect();
        assert_eq!(cfg, ["target_os = \"linux\""], "theme::start's cfg");
        let appearance = match call.args.iter().nth(1) {
            Some(syn::Expr::Reference(reference)) => match &*reference.expr {
                syn::Expr::Field(field) => matches!(
                    (&*field.base, &field.member),
                    (syn::Expr::Path(base), syn::Member::Named(member))
                        if base.path.is_ident("shell") && member == "appearance"
                ),
                _ => false,
            },
            _ => false,
        };
        assert!(appearance, "theme::start is given &shell.appearance");
    }

    /// The DMA-BUF restart replaces the process: it is the first statement of `run`, before the
    /// runtime's threads, the log file or the single-instance name exist, so it throws nothing
    /// away and leaves nothing behind.
    #[test]
    fn the_graphics_workaround_is_run_s_first_statement() {
        let run = run_fn();
        let Some(syn::Stmt::Local(first)) = run.block.stmts.first() else {
            panic!("run's first statement is no `let`");
        };
        let init = first.init.as_ref().expect("a `let` with a value");
        let syn::Expr::Call(call) = &*init.expr else {
            panic!("run's first statement calls no function");
        };
        let syn::Expr::Path(path) = &*call.func else {
            panic!("run's first statement calls no path");
        };
        let names: Vec<String> = path
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        assert_eq!(
            names,
            ["env", "turn_off_dmabuf_renderer_on_nvidia_wayland"],
            "run's first statement"
        );
    }

    /// Every path in `run`, and every method call on a plain name (`builder.build`), in source
    /// order, each with its line.
    #[derive(Default)]
    struct Calls(Vec<(usize, String)>);

    impl<'ast> syn::visit::Visit<'ast> for Calls {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            let names: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
            let line = path
                .segments
                .first()
                .map_or(0, |s| s.ident.span().start().line);
            self.0.push((line, names.join("::")));
            syn::visit::visit_path(self, path);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if let syn::Expr::Path(receiver) = &*call.receiver {
                if let Some(receiver) = receiver.path.get_ident() {
                    let line = call.method.span().start().line;
                    self.0.push((line, format!("{receiver}.{}", call.method)));
                }
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }

    /// The single-instance plugin panics while the app is built on a session bus address that
    /// does not parse (WI-456), and tests/single_instance.rs runs the lock's wiring, not `run`.
    /// So `run` is held to that wiring: it never names the plugin's crate, which only
    /// `single_instance` registers, and only behind its check; it makes the decision
    /// (`SingleInstance::for_this_launch`), registers through it (`single_instance.register`)
    /// before the app is built, and logs it (`single_instance.log`) once the log has started.
    #[test]
    fn run_registers_the_single_instance_plugin_only_behind_its_check() {
        let mut calls = Calls::default();
        syn::visit::Visit::visit_item_fn(&mut calls, &run_fn());
        let calls = calls.0;
        let plugin: Vec<_> = calls
            .iter()
            .filter(|(_, path)| path.contains("tauri_plugin_single_instance"))
            .collect();
        assert!(
            plugin.is_empty(),
            "run names the plugin's crate: {plugin:?}"
        );
        let line = |name: &str| {
            let found: Vec<usize> = calls
                .iter()
                .filter(|(_, call)| call == name)
                .map(|(line, _)| *line)
                .collect();
            assert_eq!(found.len(), 1, "run calls {name} once: {found:?}");
            found[0]
        };
        let decided = line("SingleInstance::for_this_launch");
        let registered = line("single_instance.register");
        let built = line("builder.build");
        let logging = line("runtime::init_logging");
        let logged = line("single_instance.log");
        assert!(
            decided < registered && registered < built && built < logging && logging < logged,
            "decided at {decided}, registered at {registered}, built at {built}, the log \
             started at {logging}, logged at {logged}"
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
