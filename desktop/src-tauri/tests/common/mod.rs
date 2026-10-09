// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The rig the IPC tests share: Tauri's mock runtime with the app's real config and capability
//! (`generate_context!`), the opener plugin as the app builds it, the shell installed, and the
//! main window open. tests/ipc_mock.rs runs on libtest; tests/app_menu.rs is a `harness = false`
//! target of its own, so its checks run on the process's main thread. Each target compiles its
//! own copy of this module.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_core::{CancellationToken, Context};
use apprafter_desktop::app::{self, SessionSource, Shell, ShellCell};
use apprafter_desktop::auth::{AuthPurpose, Authenticator, PasswordAnswer};
use apprafter_desktop::ops::SystemClock;
use apprafter_desktop::settings::SettingsStore;
use apprafter_desktop_ipc::{AuthInfo, AuthMethod, AuthOutcome, Settings, UnavailableReason};
use serde_json::Value;
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// The password the PAM route's field accepts ([`Route::PasswordField`]).
// Read by tests/ipc_mock.rs only; tests/app_menu.rs compiles this module too.
#[allow(dead_code)]
pub const PASSWORD: &str = "open sesame";

/// What the PAM route's field says with a wrong password, as PAM would.
#[allow(dead_code)]
pub const PAM_SAYS: &str = "Authentication failure";

/// How the stand-in OS verifies the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Its own prompt, which always verifies; the app's field is not its way.
    Prompt,
    /// Linux's PAM route: the app's own field, which accepts [`PASSWORD`]; its prompt finds no
    /// agent, as polkit did to bring the field.
    PasswordField,
}

/// An OS that verifies the owner by `route`, counting its prompts and its field's checks.
pub struct StandIn {
    pub route: Route,
    pub prompts: AtomicUsize,
    pub checks: AtomicUsize,
}

impl Authenticator for StandIn {
    fn info(&self) -> AuthInfo {
        AuthInfo {
            available: true,
            method: Some(AuthMethod::Fake),
            unavailable: None,
            biometrics_choice: false,
            password_field: self.route == Route::PasswordField,
        }
    }

    fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
        self.prompts.fetch_add(1, SeqCst);
        match self.route {
            Route::Prompt => AuthOutcome::Verified,
            Route::PasswordField => AuthOutcome::Unavailable {
                reason: UnavailableReason::NoAgent,
            },
        }
    }

    fn verify_password(
        &self,
        _purpose: &AuthPurpose,
        password: zeroize::Zeroizing<String>,
        _cancel: &CancellationToken,
    ) -> PasswordAnswer {
        if self.route == Route::Prompt {
            return PasswordAnswer::NOT_HERE;
        }
        self.checks.fetch_add(1, SeqCst);
        if password.as_str() == PASSWORD {
            AuthOutcome::Verified.into()
        } else {
            PasswordAnswer {
                outcome: AuthOutcome::Failed { exhausted: false },
                messages: vec![PAM_SAYS.to_owned()],
            }
        }
    }
}

pub struct Rig {
    _dir: tempfile::TempDir,
    // Read by tests/ipc_mock.rs only; tests/app_menu.rs compiles this module too.
    #[allow(dead_code)]
    pub auth: Arc<StandIn>,
    pub shell: Arc<Shell>,
    pub _app: tauri::App<MockRuntime>,
    window: WebviewWindow<MockRuntime>,
}

/// The app with `settings` saved, an OS whose prompt verifies, and the main window open.
pub fn rig(settings: Settings) -> Rig {
    rig_by(settings, Route::Prompt)
}

/// [`rig`], with an OS that verifies by `route`.
pub fn rig_by(settings: Settings, route: Route) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::load(dir.path(), &SystemClock);
    store.set(settings).unwrap();
    let auth = Arc::new(StandIn {
        route,
        prompts: AtomicUsize::new(0),
        checks: AtomicUsize::new(0),
    });
    let shell = Shell::new(
        store,
        auth.clone(),
        Arc::new(SystemClock),
        Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:9"),
        false,
        |_| {},
    );
    assert!(
        shell.auth.settled(Duration::from_secs(10)),
        "the authenticator never answered"
    );
    let cell = ShellCell::default();
    let app = app::builder(mock_builder(), cell.clone())
        .plugin(app::opener_plugin())
        .build(tauri::generate_context!())
        .unwrap();
    app::install(&app, &cell, shell.clone()).unwrap();
    let window = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .unwrap();
    Rig {
        _dir: dir,
        auth,
        shell,
        _app: app,
        window,
    }
}

pub fn lock_off() -> Settings {
    Settings {
        lock_enabled: false,
        ..Settings::default()
    }
}

/// What the webview gets back: `Ok` with the value, or `Err` with what was rejected.
pub fn invoke(rig: &Rig, cmd: &str, args: Value) -> Result<Value, Value> {
    let url = if cfg!(windows) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    get_ipc_response(
        &rig.window,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|body| body.deserialize::<Value>().unwrap())
}

/// The `UiError.code` of a rejection, if it is one.
pub fn code(reply: &Result<Value, Value>) -> Option<&str> {
    reply.as_ref().err()?.get("code")?.as_str()
}

/// What happened, in order, as the tests that quit log it.
pub type Log = Arc<Mutex<Vec<&'static str>>>;

/// What [`Watch`] logs when it is dropped.
pub const WATCH_DROPPED: &str = "the session watch was dropped";

/// The OS session watch as the IPC tests keep it: it listens at once, and its drop is logged.
pub struct Watch(Log);

impl SessionSource for Watch {
    fn listening(&self) -> Option<apprafter_os_auth::Listening> {
        Some(apprafter_os_auth::Listening {
            lock: true,
            sleep: true,
        })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.lock().unwrap().push(WATCH_DROPPED);
    }
}

/// Starts a [`Watch`] on the rig's shell, logging into `log`.
pub fn watch(rig: &Rig, log: &Log) {
    let log = log.clone();
    rig.shell.watch_session(move |_| Box::new(Watch(log)));
}

/// Waits until `log` holds `entry`; panics after ten seconds.
pub fn wait_for(log: &Log, entry: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !log.lock().unwrap().contains(&entry) {
        assert!(Instant::now() < deadline, "never logged: {entry}");
        thread::sleep(Duration::from_millis(2));
    }
}
