// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The rig the IPC tests share: Tauri's mock runtime with the app's real config and capability
//! (`generate_context!`), stand-ins for the clipboard and opener plugins (plugins.rs: the real
//! ones reach the owner's session), the shell installed, and the main window open.
//! tests/ipc_mock.rs runs on libtest; tests/app_menu.rs is a `harness = false` target of its own,
//! so its checks run on the process's main thread. Each target compiles its own copy of this
//! module.

pub mod plugins;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_core::{CancellationToken, Context, PathSource};
use apprafter_desktop::app::{self, SessionSource, Shell, ShellCell};
use apprafter_desktop::auth::{AuthPurpose, Authenticator, PasswordAnswer};
use apprafter_desktop::env::ToolSearchPath;
use apprafter_desktop::ops::{EventSink, SystemClock};
use apprafter_desktop::settings::SettingsStore;
use apprafter_desktop_ipc::{
    AuthInfo, AuthMethod, AuthOutcome, OpEvent, OpId, Settings, UnavailableReason,
};
use serde_json::Value;
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use plugins::Asked;

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
            return PasswordAnswer::USE_SYSTEM_PROMPT;
        }
        self.checks.fetch_add(1, SeqCst);
        if password.as_str() == PASSWORD {
            AuthOutcome::Verified.into()
        } else {
            PasswordAnswer {
                outcome: AuthOutcome::Failed {
                    exhausted: false,
                    retry_in_ms: None,
                },
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
    /// What the clipboard and opener stand-ins were asked.
    // Read by tests/ipc_mock.rs only; tests/app_menu.rs compiles this module too.
    #[allow(dead_code)]
    pub asked: Asked,
    pub _app: tauri::App<MockRuntime>,
    window: WebviewWindow<MockRuntime>,
}

/// The app with `settings` saved, an OS whose prompt verifies, and the main window open.
pub fn rig(settings: Settings) -> Rig {
    rig_by(settings, Route::Prompt)
}

/// [`rig`], with an OS that verifies by `route`.
pub fn rig_by(settings: Settings, route: Route) -> Rig {
    // The tool search path the context has: an empty one, set by the caller.
    let tools = ToolSearchPath::known(Default::default(), PathSource::Explicit);
    rig_with_tools(settings, route, tools)
}

/// [`rig_by`], the shell learning the tool search path through `tools` (as the app on macOS
/// learns it from the login shell), the rest built as the app builds it meanwhile.
pub fn rig_with_tools(settings: Settings, route: Route, tools: ToolSearchPath) -> Rig {
    rig_on(settings, route, tools, "http://127.0.0.1:9")
}

/// [`rig`], the provider API at `base_url` (a loopback mock of it).
// Read by tests/token_secrecy.rs only; the other targets compile this module too.
#[allow(dead_code)]
pub fn rig_with_api(settings: Settings, base_url: &str) -> Rig {
    let tools = ToolSearchPath::known(Default::default(), PathSource::Explicit);
    rig_on(settings, Route::Prompt, tools, base_url)
}

/// The rig, its provider API at `api_base`: nothing answers at the default
/// (`127.0.0.1:9`), so a test that reaches the provider by mistake fails rather than calls out.
fn rig_on(settings: Settings, route: Route, tools: ToolSearchPath, api_base: &str) -> Rig {
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
        Context::for_desktop(dir.path().join("store"), api_base),
        tools,
        false,
        |_| {},
    );
    assert!(
        shell.auth.settled(Duration::from_secs(10)),
        "the authenticator never answered"
    );
    let cell = ShellCell::default();
    let asked = Asked::default();
    let app = app::builder(mock_builder(), cell.clone())
        .plugin(plugins::opener(asked.clone()))
        .plugin(plugins::clipboard(asked.clone()))
        .build(tauri::generate_context!())
        .unwrap();
    assert_no_session_plugin(&app);
    app::install(&app, &cell, shell.clone()).unwrap();
    let window = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .unwrap();
    Rig {
        _dir: dir,
        auth,
        shell,
        asked,
        _app: app,
        window,
    }
}

/// Neither real plugin set up in `app`: each manages its handle when it does — the clipboard's
/// holds the arboard connection to the session's clipboard, the opener's is what `open_url`
/// starts a browser through. Every rig is checked, so a rig that registers either fails every
/// test, before any test can ask it anything (GOTCHA-156).
fn assert_no_session_plugin(app: &tauri::App<MockRuntime>) {
    assert!(
        app.try_state::<tauri_plugin_clipboard_manager::Clipboard<MockRuntime>>()
            .is_none(),
        "the real clipboard plugin is in the test rig: it reaches the session's clipboard"
    );
    assert!(
        app.try_state::<tauri_plugin_opener::Opener<MockRuntime>>()
            .is_none(),
        "the real opener plugin is in the test rig: it starts the session's browser"
    );
}

pub fn lock_off() -> Settings {
    Settings {
        lock_enabled: false,
        ..Settings::default()
    }
}

/// What the webview gets back: `Ok` with the value, or `Err` with what was rejected.
pub fn invoke(rig: &Rig, cmd: &str, args: Value) -> Result<Value, Value> {
    invoke_on(&rig.window, cmd, args)
}

/// [`invoke`], on any window of a mock app.
pub fn invoke_on(
    window: &WebviewWindow<MockRuntime>,
    cmd: &str,
    args: Value,
) -> Result<Value, Value> {
    let url = if cfg!(windows) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    get_ipc_response(
        window,
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

/// The app's own links, read from the page's list (src/shell/links.ts `LINKS`), so the
/// capability is pinned to what the page opens, not to a copy of it: every single-quoted
/// address between `export const LINKS = {` and its closing brace. Each must be one https
/// address, and there are the page's three (Website, Docs, GitHub).
#[allow(dead_code)]
pub fn app_links() -> BTreeSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/shell/links.ts");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let start = "export const LINKS = {";
    let at = text
        .find(start)
        .unwrap_or_else(|| panic!("{}: no `{start}`", path.display()));
    let body = &text[at + start.len()..];
    let body = &body[..body.find('}').expect("LINKS closes")];
    let links: Vec<&str> = body.split('\'').skip(1).step_by(2).collect();
    for link in &links {
        assert!(
            link.starts_with("https://") && !link.contains(char::is_whitespace),
            "a link must be one https address: {link}"
        );
    }
    let set: BTreeSet<String> = links.iter().map(|l| l.to_string()).collect();
    assert_eq!(set.len(), 3, "the page's three links: {links:?}");
    assert_eq!(set.len(), links.len(), "a link twice: {links:?}");
    set
}

/// The install pages the toolchain panel can open: every install line of the core's tool specs
/// that is an address. Each must be https; an http one fails here rather than being left out.
#[allow(dead_code)]
pub fn install_pages() -> BTreeSet<String> {
    let mut pages = BTreeSet::new();
    for tool in apprafter_core::tools::ToolId::ALL {
        for hint in tool.spec().hints {
            if hint.text.contains("://") {
                assert!(
                    hint.text.starts_with("https://") && !hint.text.contains(char::is_whitespace),
                    "{}: an install page must be one https address: {}",
                    tool.name(),
                    hint.text
                );
                pages.insert(hint.text.to_string());
            }
        }
    }
    pages
}

/// The `UiError.code` of a rejection, if it is one.
pub fn code(reply: &Result<Value, Value>) -> Option<&str> {
    reply.as_ref().err()?.get("code")?.as_str()
}

/// An event sink for the webview `main` that keeps every event it is sent.
// Read by tests/token_secrecy.rs only; the other targets compile this module too.
#[allow(dead_code)]
#[derive(Default)]
pub struct Recorder(pub Mutex<Vec<OpEvent>>);

impl EventSink for Recorder {
    fn send(&self, event: &OpEvent) -> bool {
        self.0.lock().unwrap().push(event.clone());
        true
    }

    fn webview(&self) -> &str {
        "main"
    }
}

/// Every event of operation `id`, from its first to its end: a [`Recorder`] subscribes, the
/// replay first (an operation that ended before the subscription has its end there), then what
/// the recorder hears, until the last is `Finished` or `Failed`. Panics after ten seconds.
#[allow(dead_code)]
pub fn follow_to_end(rig: &Rig, id: OpId) -> Vec<OpEvent> {
    let recorder = Arc::new(Recorder::default());
    let subscribed = rig.shell.ops.subscribe(id, recorder.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut events = subscribed.replay.clone();
        events.extend(recorder.0.lock().unwrap().iter().cloned());
        if matches!(
            events.last(),
            Some(OpEvent::Finished { .. } | OpEvent::Failed { .. })
        ) {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "operation {id:?} never ended: {events:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
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
