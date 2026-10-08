// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The IPC surface as the webview meets it: Tauri's mock runtime, with the app's real config
//! and capability (`generate_context!`), so every invoke passes the same ACL, lock gate and
//! argument parsing it does in the app.
//!
//! The app manifest makes Tauri check the ACL for app commands: a command missing from the
//! capability is refused before the gate, and one missing from `generate_handler!` answers
//! `Command <name> not found` — the first test catches both.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

use apprafter_core::{CancellationToken, Context, Outcome, PlanClass};
use apprafter_desktop::app::{self, Shell, ShellCell};
use apprafter_desktop::auth::{AuthPurpose, Authenticator};
use apprafter_desktop::ops::{Executor, PlanParts, SystemClock};
use apprafter_desktop::settings::SettingsStore;
use apprafter_desktop_ipc::{
    errors, AuthInfo, AuthMethod, AuthOutcome, Settings, Theme, ALLOWED_WHILE_LOCKED, COMMANDS,
};
use serde_json::{json, Value};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// An OS that verifies the owner every time it is asked, and counts the asks.
#[derive(Default)]
struct Verifies(AtomicUsize);

impl Authenticator for Verifies {
    fn info(&self) -> AuthInfo {
        AuthInfo {
            available: true,
            method: Some(AuthMethod::Fake),
            unavailable: None,
            biometrics_choice: false,
            password_field: false,
        }
    }

    fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
        self.0.fetch_add(1, SeqCst);
        AuthOutcome::Verified
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    auth: Arc<Verifies>,
    shell: Arc<Shell>,
    _app: tauri::App<MockRuntime>,
    window: WebviewWindow<MockRuntime>,
}

/// The app with `settings` saved, an OS that verifies, and the main window open.
fn rig(settings: Settings) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::load(dir.path(), &SystemClock);
    store.set(settings).unwrap();
    let auth = Arc::new(Verifies::default());
    let shell = Shell::new(
        store,
        auth.clone(),
        Arc::new(SystemClock),
        Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:9"),
        false,
        |_| {},
    );
    let cell = ShellCell::default();
    let app = app::builder(mock_builder(), cell.clone())
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

fn lock_off() -> Settings {
    Settings {
        lock_enabled: false,
        ..Settings::default()
    }
}

/// What the webview gets back: `Ok` with the value, or `Err` with what was rejected.
fn invoke(rig: &Rig, cmd: &str, args: Value) -> Result<Value, Value> {
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
fn code(reply: &Result<Value, Value>) -> Option<&str> {
    reply.as_ref().err()?.get("code")?.as_str()
}

/// The command ran, past the ACL and into its own handler: it answered, or rejected with a
/// `UiError`, or could not parse the (empty) arguments it was given. Never Tauri's
/// `Command <name> not found` nor an ACL refusal.
fn assert_reached_its_handler(name: &str, reply: &Result<Value, Value>) {
    match reply {
        Ok(_) => {}
        Err(Value::Object(error)) => assert!(error.contains_key("code"), "{name}: {error:?}"),
        Err(Value::String(error)) => {
            assert!(
                !error.contains("not found"),
                "{name} is not registered: {error}"
            );
            assert!(
                error.starts_with("invalid args") && error.contains(&format!("command `{name}`")),
                "{name} did not reach its handler: {error}"
            );
        }
        Err(other) => panic!("{name}: {other:?}"),
    }
}

#[test]
fn every_command_is_registered_and_allowed_and_a_quit_starts_nothing_new() {
    // The lock off, so the gate lets everything through to the handler.
    let rig = rig(lock_off());
    for name in COMMANDS.iter().filter(|name| **name != "quit") {
        let reply = invoke(&rig, name, json!({}));
        assert_reached_its_handler(name, &reply);
    }
    // The control: a name the app does not know is refused (by the ACL, before the gate).
    assert!(invoke(&rig, "not_a_command", json!({})).is_err());

    // Last, since it quits. It answers once nothing new can start; the mock runtime cannot
    // exit (its `request_exit` is `unimplemented!()`), so the quit thread's final exit panics
    // there, on its own thread, after everything checked here.
    let reply = invoke(&rig, "quit", json!({}));
    assert_reached_its_handler("quit", &reply);
    assert_eq!(reply, Ok(Value::Null));
    let ran = Arc::new(AtomicUsize::new(0));
    let exec: Executor = {
        let ran = ran.clone();
        Box::new(move |_, _| {
            ran.fetch_add(1, SeqCst);
            Ok(Outcome::Completed { result: json!(0) })
        })
    };
    let plan = rig.shell.ops.register_plan(
        PlanParts::new(PlanClass::Bounded, "Upgrade", "upgrade"),
        exec,
    );
    let reply = invoke(
        &rig,
        "op_execute",
        json!({ "opId": plan.op_id, "onEvent": "__CHANNEL__:7" }),
    );
    assert_eq!(code(&reply), Some(errors::CLOSING), "{reply:?}");
    assert_eq!(ran.load(SeqCst), 0);
}

#[test]
fn locked_the_app_answers_only_the_allowed_commands() {
    let rig = rig(Settings::default());
    assert_eq!(
        invoke(&rig, "lock_status", json!({})).unwrap()["locked"],
        true
    );
    // Refused before the arguments are read: an empty body is refused as locked too.
    for name in COMMANDS
        .iter()
        .filter(|name| !ALLOWED_WHILE_LOCKED.contains(name))
    {
        let reply = invoke(&rig, name, json!({}));
        assert_eq!(code(&reply), Some(errors::LOCKED), "{name}: {reply:?}");
    }
    let settings = json!({ "settings": Settings::default() });
    let reply = invoke(&rig, "settings_set", settings);
    assert_eq!(code(&reply), Some(errors::LOCKED), "{reply:?}");
    for name in ["lock_status", "app_info", "settings_get"] {
        let reply = invoke(&rig, name, json!({}));
        assert!(reply.is_ok(), "{name}: {reply:?}");
    }
    let info = invoke(&rig, "app_info", json!({})).unwrap();
    assert_eq!(info["auth"]["method"], "fake");
    assert_eq!(rig.auth.0.load(SeqCst), 0, "nothing asked the owner");
}

#[test]
fn unlocked_by_a_verified_owner_the_same_calls_answer() {
    let rig = rig(Settings::default());
    let state = invoke(&rig, "unlock", json!({})).unwrap();
    assert_eq!(state["locked"], false, "{state}");
    assert_eq!(rig.auth.0.load(SeqCst), 1);
    assert_eq!(invoke(&rig, "op_list", json!({})).unwrap(), json!([]));
    let light = Settings {
        theme: Theme::Light,
        ..Settings::default()
    };
    let saved = invoke(&rig, "settings_set", json!({ "settings": light })).unwrap();
    assert_eq!(saved["theme"], "light");
    assert_eq!(rig.shell.settings.get(), light);
    let state = invoke(&rig, "lock_now", json!({})).unwrap();
    assert_eq!(state["locked"], true, "{state}");
    assert_eq!(state["reason"], "manual");
    let reply = invoke(&rig, "op_list", json!({}));
    assert_eq!(
        code(&reply),
        Some(errors::LOCKED),
        "locked again: {reply:?}"
    );
}
