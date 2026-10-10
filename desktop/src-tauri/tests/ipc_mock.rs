// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The IPC surface as the webview meets it: Tauri's mock runtime, with the app's real config
//! and capability (`generate_context!`), so every invoke passes the same ACL, lock gate and
//! argument parsing it does in the app.
//!
//! The app manifest makes Tauri check the ACL for app commands: a command missing from the
//! capability is refused before the gate, and one missing from `generate_handler!` answers
//! `Command <name> not found` — the first test catches both.
//!
//! The macOS app menu's check is in tests/app_menu.rs, a target of its own: muda builds a
//! menu item on the main thread only, and libtest runs every test on a thread of its own.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use apprafter_core::{CoreError, Outcome, PathSource, PlanClass};
use apprafter_desktop::app;
use apprafter_desktop::env::ToolSearchPath;
use apprafter_desktop::ops::{Executor, PlanParts};
use apprafter_desktop_ipc::{errors, Settings, Theme, ALLOWED_WHILE_LOCKED, COMMANDS, QUITTING};
use common::{
    code, invoke, lock_off, rig, rig_by, rig_with_tools, wait_for, watch, Log, Rig, Route,
    PAM_SAYS, PASSWORD, WATCH_DROPPED,
};
use serde_json::{json, Value};
use tauri::Listener;

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

/// Plugin commands never reach the app's invoke handler, so the lock gate never sees them:
/// the capability is all that stands between a page and a plugin command. Pinned here for
/// those a page could misuse — forging an event (`lock-changed` among them), closing the
/// window past the quit (the Windows caption's close button quits through the app instead),
/// reading the app's details, and the window's own `set_theme`, whose `null` forces a light
/// theme on Linux (`theme_apply` sets the theme instead) — each refused by the ACL, while the
/// granted ones pass it.
#[test]
fn plugin_commands_beyond_the_granted_ones_are_refused_by_the_acl() {
    let rig = rig(lock_off());
    for cmd in [
        "plugin:event|emit",
        "plugin:window|close",
        "plugin:app|version",
        "plugin:window|set_theme",
    ] {
        match invoke(&rig, cmd, json!({})) {
            Err(Value::String(error)) => assert!(
                error.contains("not allowed"),
                "{cmd} was not refused by the ACL: {error}"
            ),
            other => panic!("{cmd} was not refused by the ACL: {other:?}"),
        }
    }
    // The control: `listen` is granted, so it passes the ACL and fails only on its (empty)
    // arguments.
    match invoke(&rig, "plugin:event|listen", json!({})) {
        Err(Value::String(error)) => {
            assert!(
                !error.contains("not allowed"),
                "listen was refused: {error}"
            );
            assert!(error.contains("invalid args"), "{error}");
        }
        other => panic!("listen with no arguments: {other:?}"),
    }
    // The other controls: the Windows caption buttons minimize and maximize the window; each
    // passes the ACL and runs on the mock window.
    for cmd in ["plugin:window|minimize", "plugin:window|toggle_maximize"] {
        assert_eq!(invoke(&rig, cmd, json!({})), Ok(Value::Null), "{cmd}");
    }
    assert_eq!(
        invoke(&rig, "plugin:window|is_maximized", json!({})),
        Ok(json!(false))
    );
}

/// The page may write text to the clipboard (Copy report, Copy command) and nothing else: reading
/// what another program put there, writing HTML or an image, and clearing are refused by the ACL
/// (D.3 overview R11). Plugin commands never pass the lock gate, so this grant must be harmless
/// while locked — it only ever writes.
#[test]
fn the_clipboard_takes_text_and_gives_nothing_back() {
    let rig = rig(lock_off());
    for cmd in [
        "plugin:clipboard-manager|read_text",
        "plugin:clipboard-manager|read_image",
        "plugin:clipboard-manager|write_html",
        "plugin:clipboard-manager|write_image",
        "plugin:clipboard-manager|clear",
    ] {
        match invoke(&rig, cmd, json!({})) {
            Err(Value::String(error)) => assert!(
                error.contains("not allowed"),
                "{cmd} was not refused by the ACL: {error}"
            ),
            other => panic!("{cmd} was not refused by the ACL: {other:?}"),
        }
    }
    // Empty arguments: past the ACL, refused for the missing text only — nothing is written.
    match invoke(&rig, "plugin:clipboard-manager|write_text", json!({})) {
        Err(Value::String(error)) => {
            assert!(
                !error.contains("not allowed"),
                "write_text was refused: {error}"
            );
            assert!(error.contains("invalid args"), "{error}");
        }
        other => panic!("write_text with no arguments: {other:?}"),
    }
}

/// A quit that has operations to wait for tells the page, which then shows that it is stopping
/// them instead of a page whose every command is refused: `quitting`, with how many and the
/// longest wait. The page hears it through `core:event:allow-listen`, no new permission.
#[test]
fn a_quit_with_operations_running_tells_the_page_what_it_waits_for() {
    let rig = rig(lock_off());
    let (heard_tx, heard) = mpsc::channel::<String>();
    rig._app.listen_any(QUITTING, move |event| {
        let _ = heard_tx.send(event.payload().to_string());
    });
    let (started_tx, started) = mpsc::channel::<()>();
    let exec: Executor = Box::new(move |_, cancel| {
        let _ = started_tx.send(());
        while !cancel.is_cancelled() {
            thread::sleep(Duration::from_millis(1));
        }
        Err(CoreError::Cancelled)
    });
    let plan = rig.shell.ops.register_plan(
        PlanParts::new(PlanClass::Bounded, "Upgrade", "upgrade"),
        exec,
    );
    let reply = invoke(
        &rig,
        "op_execute",
        json!({ "opId": plan.op_id, "onEvent": "__CHANNEL__:7" }),
    );
    assert!(reply.is_ok(), "{reply:?}");
    started.recv_timeout(Duration::from_secs(5)).unwrap();

    // As in the first test, the mock runtime cannot exit: the quit thread's final exit
    // panics on its own thread, after the operation stopped.
    assert_eq!(invoke(&rig, "quit", json!({})), Ok(Value::Null));
    let payload = heard
        .recv_timeout(Duration::from_secs(5))
        .expect("the quit did not say what it waits for");
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap(),
        json!({ "running": 1, "waitMs": app::STOP_BOUND.as_millis() as u64 })
    );
}

/// WI-452: a slow login shell (macOS asks one for the tools' `PATH`) delays neither the shell
/// nor the window: the app is built, and `window_ready` shows the window, while the shell has
/// not answered — the context meanwhile has the fallback — and the first lookup of a tool
/// waits for the answer.
#[test]
fn a_slow_login_shell_does_not_delay_window_ready() {
    let (release, released) = mpsc::channel::<()>();
    let tools = ToolSearchPath::probe(
        move || {
            // The slow shell: it answers when the test lets it.
            let _ = released.recv_timeout(Duration::from_secs(60));
            Some("/from/the/login/shell".into())
        },
        PathSource::LoginShell,
        ("/usr/bin:/bin".into(), PathSource::Fallback),
        Duration::from_secs(60),
    );
    let rig = rig_with_tools(lock_off(), Route::Prompt, tools.clone());
    assert_eq!(invoke(&rig, "window_ready", json!({})), Ok(Value::Null));
    assert_eq!(
        tools.now().1,
        PathSource::Fallback,
        "the window showed before the login shell answered"
    );
    let lookup = {
        let shell = rig.shell.clone();
        thread::spawn(move || {
            let context = shell.tool_context();
            (
                context.tool_search_path().to_owned(),
                context.tool_search_path_source(),
            )
        })
    };
    thread::sleep(Duration::from_millis(50));
    assert!(!lookup.is_finished(), "the lookup waits for the answer");
    release.send(()).unwrap();
    assert_eq!(
        lookup.join().unwrap(),
        ("/from/the/login/shell".into(), PathSource::LoginShell)
    );
}

/// A quit with nothing running exits at once: there is nothing to say, and nothing is said.
#[test]
fn a_quit_with_nothing_running_says_nothing() {
    let rig = rig(lock_off());
    let (heard_tx, heard) = mpsc::channel::<String>();
    rig._app.listen_any(QUITTING, move |event| {
        let _ = heard_tx.send(event.payload().to_string());
    });
    assert_eq!(invoke(&rig, "quit", json!({})), Ok(Value::Null));
    assert!(heard.recv_timeout(Duration::from_millis(200)).is_err());
}

/// The opener's scope is the three URLs exactly as the capability writes them: a URL the page
/// does not show, a trailing slash on one it does, and a host that only starts like ours are
/// each refused by the plugin's own scope check — reached past the ACL, which grants
/// `open_url` with that scope. (A listed URL would open the browser, so none is tried here.)
#[test]
fn the_opener_opens_the_listed_urls_only_and_only_as_written() {
    let rig = rig(lock_off());
    for url in [
        "https://example.com",
        "https://apprafter.dev/",
        "https://apprafter.dev.evil",
        "https://apprafter.dev.evil/",
        "http://apprafter.dev",
        "https://docs.apprafter.dev/../x",
        "https://github.com/AppRafter/apprafter/",
        "https://github.com/AppRafter/apprafter-evil",
    ] {
        match invoke(&rig, "plugin:opener|open_url", json!({ "url": url })) {
            Err(Value::String(error)) => assert_eq!(
                error,
                format!("Not allowed to open url {url}"),
                "{url} was not refused by the opener's scope"
            ),
            other => panic!("{url} was not refused by the opener's scope: {other:?}"),
        }
    }
}

/// The capability, read as Tauri reads it: exactly the pinned core permissions, the opener
/// scoped to the three links the app shows, and the generated `allow-<command>` of every app
/// command — nothing more, nothing less, once each. A permission added for a later step must
/// be added here too, with its reason in the file.
#[test]
fn the_capability_grants_the_pinned_permissions_and_every_app_command_only() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities/main.json5");
    let text = fs::read_to_string(&path).unwrap();
    let capability: Value = json5::from_str(&text).unwrap();
    let permissions = capability["permissions"]
        .as_array()
        .expect("a permissions list");
    let granted: Vec<&str> = permissions.iter().filter_map(Value::as_str).collect();
    let scoped: Vec<&Value> = permissions.iter().filter(|p| !p.is_string()).collect();
    let set: BTreeSet<String> = granted.iter().map(|p| p.to_string()).collect();
    assert_eq!(set.len(), granted.len(), "a permission twice: {granted:?}");
    let mut expected: BTreeSet<String> = [
        "core:event:allow-listen",
        "core:event:allow-unlisten",
        "core:window:allow-minimize",
        "core:window:allow-toggle-maximize",
        "core:window:allow-is-maximized",
        "core:window:allow-start-dragging",
        "core:window:allow-internal-toggle-maximize",
        "clipboard-manager:allow-write-text",
    ]
    .map(String::from)
    .into();
    expected.extend(
        COMMANDS
            .iter()
            .map(|cmd| format!("allow-{}", cmd.replace('_', "-"))),
    );
    assert_eq!(set, expected);
    // The one scoped permission: the opener, for exactly these URLs, no pattern.
    let links = json!({
        "identifier": "opener:allow-open-url",
        "allow": [
            { "url": "https://apprafter.dev" },
            { "url": "https://docs.apprafter.dev" },
            { "url": "https://github.com/AppRafter/apprafter" },
        ],
    });
    assert_eq!(scoped, [&links]);
    assert_eq!(capability["windows"], json!(["main"]));
    assert_eq!(capability.get("remote"), None, "no remote origin");
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
    assert_eq!(rig.auth.prompts.load(SeqCst), 0, "nothing asked the owner");
}

/// The lock screen has its theme too: `theme_apply` answers while locked, for every setting,
/// and keeps the last one for the desktop's later changes; a value that is no setting is
/// refused before it reaches the window.
#[test]
fn locked_the_page_still_applies_its_theme() {
    let rig = rig(Settings::default());
    assert_eq!(
        invoke(&rig, "lock_status", json!({})).unwrap()["locked"],
        true
    );
    for (theme, setting) in [
        ("system", Theme::System),
        ("light", Theme::Light),
        ("dark", Theme::Dark),
    ] {
        let reply = invoke(&rig, "theme_apply", json!({ "theme": theme }));
        assert_eq!(reply, Ok(Value::Null), "{theme}");
        assert_eq!(rig.shell.appearance.setting(), setting);
    }
    match invoke(&rig, "theme_apply", json!({ "theme": "sepia" })) {
        Err(Value::String(error)) => assert!(error.contains("invalid args"), "{error}"),
        other => panic!("a theme that is no setting: {other:?}"),
    }
    assert_eq!(rig.shell.appearance.setting(), Theme::Dark);
}

#[test]
fn unlocked_by_a_verified_owner_the_same_calls_answer() {
    let rig = rig(Settings::default());
    let state = invoke(&rig, "unlock", json!({})).unwrap();
    assert_eq!(state["locked"], false, "{state}");
    assert_eq!(rig.auth.prompts.load(SeqCst), 1);
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

/// The lock screen's own password field: allowed while locked, it unlocks with the right
/// password; a wrong one is refused with what the OS said, and the password itself is in no
/// answer.
#[test]
fn the_password_field_unlocks_with_the_right_password_and_says_why_not_otherwise() {
    let rig = rig_by(Settings::default(), Route::PasswordField);
    let info = invoke(&rig, "app_info", json!({})).unwrap();
    assert_eq!(info["auth"]["passwordField"], true, "{info}");
    let reply = invoke(&rig, "unlock", json!({}));
    assert_eq!(code(&reply), Some(errors::AUTH_UNAVAILABLE), "{reply:?}");
    assert_eq!(reply.unwrap_err()["fields"]["reason"], "no_agent");
    let reply = invoke(&rig, "unlock_with_password", json!({ "password": "guess" }));
    assert_eq!(code(&reply), Some(errors::AUTH_FAILED), "{reply:?}");
    let error = reply.unwrap_err();
    assert_eq!(error["fields"]["messages"], json!([PAM_SAYS]));
    assert!(!error.to_string().contains("guess"), "{error}");
    let still = invoke(&rig, "op_list", json!({}));
    assert_eq!(
        code(&still),
        Some(errors::LOCKED),
        "still locked: {still:?}"
    );

    let state = invoke(
        &rig,
        "unlock_with_password",
        json!({ "password": PASSWORD }),
    )
    .unwrap();
    assert_eq!(state["locked"], false, "{state}");
    assert!(!state.to_string().contains(PASSWORD), "{state}");
    assert_eq!(
        rig.auth.checks.load(SeqCst),
        2,
        "the field was checked twice"
    );
    assert_eq!(
        rig.auth.prompts.load(SeqCst),
        1,
        "only the refused unlock asked a prompt"
    );
    assert_eq!(invoke(&rig, "op_list", json!({})).unwrap(), json!([]));
}

/// Where the OS prompts itself the field is not its way: refused, the password unread.
#[test]
fn where_the_os_prompts_itself_the_password_field_unlocks_nothing() {
    let rig = rig(Settings::default());
    let info = invoke(&rig, "app_info", json!({})).unwrap();
    assert_eq!(info["auth"]["passwordField"], false, "{info}");
    let reply = invoke(
        &rig,
        "unlock_with_password",
        json!({ "password": PASSWORD }),
    );
    assert_eq!(code(&reply), Some(errors::AUTH_UNAVAILABLE), "{reply:?}");
    assert_eq!(reply.unwrap_err()["fields"]["reason"], "use_system_prompt");
    assert_eq!(rig.auth.checks.load(SeqCst), 0);
    assert_eq!(
        invoke(&rig, "lock_status", json!({})).unwrap()["locked"],
        true
    );
}

/// `op_execute`'s `password` is the confirm dialog's own field. On the PAM route the gesture
/// checks it, and without it asks a prompt that finds no agent; where the OS prompts itself the
/// gesture is its prompt, and a password is refused unread.
#[test]
fn op_execute_checks_the_confirm_dialog_s_password_on_the_pam_route() {
    let destructive = |rig: &Rig| {
        rig.shell
            .ops
            .register_plan(
                PlanParts::new(PlanClass::Destructive, "Remove target prod", "delete"),
                Box::new(|_, _| Ok(Outcome::Completed { result: json!(0) })),
            )
            .op_id
    };
    let execute = |rig: &Rig, password: Option<&str>| {
        let mut args = json!({ "opId": destructive(rig), "onEvent": "__CHANNEL__:7" });
        if let Some(password) = password {
            args["password"] = json!(password);
        }
        invoke(rig, "op_execute", args)
    };

    let pam = rig_by(lock_off(), Route::PasswordField);
    let with = execute(&pam, Some(PASSWORD));
    assert!(with.is_ok(), "{with:?}");
    let wrong = execute(&pam, Some("guess"));
    assert_eq!(code(&wrong), Some(errors::AUTH_FAILED), "{wrong:?}");
    assert_eq!(wrong.unwrap_err()["fields"]["messages"], json!([PAM_SAYS]));
    let without = execute(&pam, None);
    assert_eq!(
        code(&without),
        Some(errors::AUTH_UNAVAILABLE),
        "{without:?}"
    );
    assert_eq!(without.unwrap_err()["fields"]["reason"], "no_agent");
    assert_eq!(
        (pam.auth.prompts.load(SeqCst), pam.auth.checks.load(SeqCst)),
        (1, 2)
    );

    let prompt = rig(lock_off());
    for args in [
        json!({ "opId": destructive(&prompt), "onEvent": "__CHANNEL__:9" }),
        json!({ "opId": destructive(&prompt), "onEvent": "__CHANNEL__:10", "password": null }),
    ] {
        let reply = invoke(&prompt, "op_execute", args.clone());
        assert!(reply.is_ok(), "{args}: {reply:?}");
    }
    let refused = execute(&prompt, Some(PASSWORD));
    assert_eq!(
        code(&refused),
        Some(errors::AUTH_UNAVAILABLE),
        "{refused:?}"
    );
    assert_eq!(
        refused.unwrap_err()["fields"]["reason"],
        "use_system_prompt"
    );
    assert_eq!(
        (
            prompt.auth.prompts.load(SeqCst),
            prompt.auth.checks.load(SeqCst)
        ),
        (2, 0)
    );
}

/// The confirm dialog's retry: a wrong password through `op_execute` keeps the plan, and the
/// same `opId` with the right password then runs it, once.
#[test]
fn op_execute_after_a_wrong_password_runs_the_same_op_id_with_the_right_one() {
    let pam = rig_by(lock_off(), Route::PasswordField);
    let op_id = pam
        .shell
        .ops
        .register_plan(
            PlanParts::new(PlanClass::Destructive, "Remove target prod", "delete"),
            Box::new(|_, _| Ok(Outcome::Completed { result: json!(0) })),
        )
        .op_id;
    let execute = |channel: u32, password: &str| {
        invoke(
            &pam,
            "op_execute",
            json!({ "opId": op_id, "onEvent": format!("__CHANNEL__:{channel}"), "password": password }),
        )
    };
    let wrong = execute(11, "guess");
    assert_eq!(code(&wrong), Some(errors::AUTH_FAILED), "{wrong:?}");
    let right = execute(12, PASSWORD);
    assert!(right.is_ok(), "the plan waited for the retry: {right:?}");
    let again = execute(13, PASSWORD);
    assert_eq!(
        code(&again),
        Some(errors::PLAN_NOT_FOUND),
        "it ran once: {again:?}"
    );
    assert_eq!(pam.auth.checks.load(SeqCst), 2);
}

/// The confirm dialog on Linux without a polkit agent: the OS's prompt finds none, the plan
/// waits, and the same `opId` confirmed with the dialog's own field then runs it.
#[test]
fn op_execute_without_an_agent_keeps_the_plan_for_the_password_field() {
    let pam = rig_by(lock_off(), Route::PasswordField);
    let op_id = pam
        .shell
        .ops
        .register_plan(
            PlanParts::new(PlanClass::Destructive, "Remove target prod", "delete"),
            Box::new(|_, _| Ok(Outcome::Completed { result: json!(0) })),
        )
        .op_id;
    let prompted = invoke(
        &pam,
        "op_execute",
        json!({ "opId": op_id, "onEvent": "__CHANNEL__:21" }),
    );
    assert_eq!(
        code(&prompted),
        Some(errors::AUTH_UNAVAILABLE),
        "{prompted:?}"
    );
    assert_eq!(prompted.unwrap_err()["fields"]["reason"], "no_agent");
    let field = invoke(
        &pam,
        "op_execute",
        json!({ "opId": op_id, "onEvent": "__CHANNEL__:22", "password": PASSWORD }),
    );
    assert!(field.is_ok(), "the plan waited for the field: {field:?}");
    assert_eq!(
        (pam.auth.prompts.load(SeqCst), pam.auth.checks.load(SeqCst)),
        (1, 1)
    );
}

/// What [`stops_when_cancelled`]'s operation logs as it stops.
const OP_STOPPED: &str = "the operation stopped";

/// Starts, through `op_execute`, an operation that stops a moment after it is cancelled and logs
/// that it did.
fn stops_when_cancelled(rig: &Rig, log: &Log) {
    let (started_tx, started) = mpsc::channel::<()>();
    let exec: Executor = {
        let log = log.clone();
        Box::new(move |_, cancel| {
            let _ = started_tx.send(());
            while !cancel.is_cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            thread::sleep(Duration::from_millis(50));
            log.lock().unwrap().push(OP_STOPPED);
            Err(CoreError::Cancelled)
        })
    };
    let plan = rig.shell.ops.register_plan(
        PlanParts::new(PlanClass::Bounded, "Upgrade", "upgrade"),
        exec,
    );
    let reply = invoke(
        rig,
        "op_execute",
        json!({ "opId": plan.op_id, "onEvent": "__CHANNEL__:7" }),
    );
    assert!(reply.is_ok(), "{reply:?}");
    started.recv_timeout(Duration::from_secs(5)).unwrap();
}

/// The quit command drops the OS session watch, and only once the running operations stopped.
/// (The mock runtime's exit then panics on the quit thread, as in the tests above.)
#[test]
fn the_quit_command_drops_the_session_watch_once_the_operations_stopped() {
    let rig = rig(lock_off());
    let log = Log::default();
    watch(&rig, &log);
    stops_when_cancelled(&rig, &log);
    assert_eq!(invoke(&rig, "quit", json!({})), Ok(Value::Null));
    wait_for(&log, WATCH_DROPPED);
    assert_eq!(*log.lock().unwrap(), [OP_STOPPED, WATCH_DROPPED]);
}

/// A quit signal takes the same way out: the watch goes once the operations stopped. SIGUSR1,
/// which nothing else in this binary uses, raised once: a second would end the process.
#[cfg(unix)]
#[test]
fn a_quit_signal_drops_the_session_watch_once_the_operations_stopped() {
    use signal_hook::consts::SIGUSR1;

    let rig = rig(lock_off());
    let log = Log::default();
    watch(&rig, &log);
    stops_when_cancelled(&rig, &log);
    app::quit_on_signals(rig._app.handle(), &rig.shell, &[SIGUSR1]).unwrap();
    signal_hook::low_level::raise(SIGUSR1).unwrap();
    wait_for(&log, WATCH_DROPPED);
    assert_eq!(*log.lock().unwrap(), [OP_STOPPED, WATCH_DROPPED]);
}

/// The renew command hands its `sshKey` to the core (the Target screen's SSH key row, WI-452):
/// a key the core cannot read is refused before any plan, and without one the same renew plans.
#[test]
fn the_renew_command_hands_its_ssh_key_to_the_core() {
    let rig = rig(lock_off());
    let target = cli_core::target::Target {
        name: "prod".into(),
        config: cli_core::target::TargetConfig {
            provider: "hetzner-cloud".into(),
            ..Default::default()
        },
        credentials: Default::default(),
    };
    cli_core::save_target(&rig.shell.context.store(), &target).unwrap();
    let token = "k".repeat(64);
    let missing = rig.shell.context.config_root().join("nothing-here.pub");
    let refused = invoke(
        &rig,
        "op_plan_target_renew",
        json!({ "name": "prod", "token": token, "sshKey": missing }),
    );
    assert_eq!(
        code(&refused),
        Some("apprafter::target::ssh_key_unreadable"),
        "{refused:?}"
    );
    let planned = invoke(
        &rig,
        "op_plan_target_renew",
        json!({ "name": "prod", "token": token, "sshKey": null }),
    )
    .unwrap();
    assert_eq!(planned["class"], "bounded");
}

/// WI-452: the token is optional on the wire. A key with `token: null` plans the key alone (the
/// SSH key row: no token field); neither is the core's nothing-to-change refusal.
#[test]
fn the_renew_command_takes_a_key_without_a_token() {
    let rig = rig(lock_off());
    let target = cli_core::target::Target {
        name: "prod".into(),
        config: cli_core::target::TargetConfig {
            provider: "hetzner-cloud".into(),
            ..Default::default()
        },
        credentials: Default::default(),
    };
    cli_core::save_target(&rig.shell.context.store(), &target).unwrap();
    let key = rig.shell.context.config_root().join("id_ed25519.pub");
    std::fs::write(&key, "ssh-ed25519 AAAA k\n").unwrap();
    let planned = invoke(
        &rig,
        "op_plan_target_renew",
        json!({ "name": "prod", "token": null, "sshKey": key }),
    )
    .unwrap();
    assert_eq!(
        (planned["class"].clone(), planned["title"].clone()),
        (json!("bounded"), json!("Change the SSH key of prod"))
    );
    assert_eq!(planned["changes"].as_array().map(Vec::len), Some(1));
    let refused = invoke(
        &rig,
        "op_plan_target_renew",
        json!({ "name": "prod", "token": null, "sshKey": null }),
    );
    assert_eq!(
        code(&refused),
        Some("apprafter::target::renew_nothing_to_change"),
        "{refused:?}"
    );
}
