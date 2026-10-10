// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The opener's scope as the real plugin applies it (`app::opener_plugin()`, behind the app's
//! capability): its URLs exactly as the capability writes them, and nothing near them.
//!
//! The plugin checks the scope inside its own `open_url`, so only the real plugin can show it —
//! and the real plugin starts a browser for every URL it lets through: `xdg-open`, or `gio open`,
//! which needs no display, only the session bus (GOTCHA-156). A test of it in the test process
//! would open the owner's browser the day the scope lets one URL too many through (a widened
//! entry, or a mutant of one). So the plugin runs in a child process, this binary again made to
//! run [`opener_probe`] alone, with an environment the parent builds from nothing: a `PATH` of one
//! empty directory, so no launcher can be found, and no display, session bus or runtime
//! directory. There a URL the scope allows ends in "No such file or directory" from the last
//! launcher the plugin tried, and one it refuses in "Not allowed to open url". The probe checks
//! that seal itself before it builds anything, and refuses to run otherwise.
//!
//! Linux only: macOS opens through `/usr/bin/open`, an absolute path no `PATH` hides, and
//! Windows through the shell (under wine, `winebrowser`). The scope check is the plugin's own
//! code, the same on every OS; the capability's URL set is pinned on every OS in
//! tests/ipc_mock.rs.
#![cfg(target_os = "linux")]

// The rig's other helpers serve the other targets.
#[allow(dead_code)]
mod common;

use std::collections::BTreeSet;
use std::process::Command;
use std::{env, fs, io};

use apprafter_desktop::app::{self, ShellCell};
use common::{install_pages, invoke_on, APP_LINKS};
use serde_json::{json, Value};
use tauri::test::mock_builder;
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Set only in the child process the parent test starts.
const PROBE: &str = "APPRAFTER_TEST_OPENER_PROBE";

/// What a session hands a process to reach it: none may be in the probe's environment.
const SESSION: [&str; 6] = [
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "WAYLAND_SOCKET",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "BROWSER",
];

/// The launchers the plugin's `open` crate tries on Linux (under WSL, PowerShell first), by name
/// on `PATH`.
const LAUNCHERS: [&str; 5] = [
    "xdg-open",
    "gio",
    "gnome-open",
    "kde-open",
    "powershell.exe",
];

/// URLs the page does not show, each near one it does: a trailing slash added or missing, a path
/// beyond one, another scheme, a host that only starts like ours, a parent path, a query.
const NEAR_MISSES: [&str; 16] = [
    "https://example.com",
    "https://apprafter.dev/",
    "https://apprafter.dev.evil",
    "https://apprafter.dev.evil/",
    "http://apprafter.dev",
    "https://docs.apprafter.dev/../x",
    "https://github.com/AppRafter/apprafter/",
    "https://github.com/AppRafter/apprafter-evil",
    "https://helm.sh/docs/intro/install",
    "https://helm.sh/docs/intro/install/x",
    "http://helm.sh/docs/intro/install/",
    "https://helm.sh/",
    "https://kubernetes.io/docs/tasks/tools/../../x",
    "https://git-scm.com/downloads/",
    "https://restic.readthedocs.io/en/stable/020_installation.html?x",
    "https://cuelang.org/docs/introduction/installation",
];

/// The probe's half: refuse to run unless the parent sealed this process — nothing on `PATH`,
/// no way into the session.
fn sealed() {
    assert_eq!(
        env::var(PROBE).as_deref(),
        Ok("1"),
        "the probe runs only as the parent test's child, in the environment it builds"
    );
    for name in SESSION {
        assert!(env::var_os(name).is_none(), "{name} is set in the probe");
    }
    let path = env::var_os("PATH").expect("PATH is set: unset, a lookup falls back to /bin");
    for dir in env::split_paths(&path) {
        let entries = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("PATH entry {}: {e}", dir.display()))
            .count();
        assert_eq!(entries, 0, "PATH entry {} is not empty", dir.display());
        for launcher in LAUNCHERS {
            assert!(!dir.join(launcher).exists(), "{launcher} on PATH");
        }
    }
}

/// The child's half: the real opener, as the app builds it, behind the app's capability.
#[test]
#[ignore = "run by the_opener_opens_the_listed_urls_only_and_only_as_written, sealed"]
fn opener_probe() {
    sealed();
    let app = app::builder(mock_builder(), ShellCell::default())
        .plugin(app::opener_plugin())
        .build(tauri::generate_context!())
        .unwrap();
    let window = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .unwrap();
    let open = |url: &str| invoke_on(&window, "plugin:opener|open_url", json!({ "url": url }));
    for url in NEAR_MISSES {
        match open(url) {
            Err(Value::String(error)) => assert_eq!(
                error,
                format!("Not allowed to open url {url}"),
                "{url} was not refused by the opener's scope"
            ),
            other => panic!("{url} was not refused by the opener's scope: {other:?}"),
        }
    }
    println!("ok: refused {} near misses", NEAR_MISSES.len());
    // Every listed URL passes the scope and reaches the launchers, none of which can be found.
    let no_launcher = io::Error::from_raw_os_error(2).to_string();
    let mut listed: BTreeSet<String> = APP_LINKS.map(String::from).into();
    listed.extend(install_pages());
    for url in &listed {
        assert_eq!(
            open(url),
            Err(Value::String(no_launcher.clone())),
            "{url} did not pass the opener's scope"
        );
    }
    println!("ok: let through {} listed URLs", listed.len());
}

/// The opener's scope is its URLs exactly as the capability writes them: each near miss is
/// refused by the plugin's own scope check, reached past the ACL that grants `open_url` with
/// that scope, and each URL the capability lists passes it — shown, where the plugin would start
/// a browser, by a launch that finds no launcher (see the module docs).
#[test]
fn the_opener_opens_the_listed_urls_only_and_only_as_written() {
    assert!(
        env::var_os(PROBE).is_none(),
        "the probe ran more than itself"
    );
    let seal = tempfile::tempdir().unwrap();
    let empty = seal.path().join("bin");
    let home = seal.path().join("home");
    fs::create_dir(&empty).unwrap();
    fs::create_dir(&home).unwrap();
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "opener_probe",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env(PROBE, "1")
        .env("PATH", &empty)
        .env("HOME", &home)
        .env("TMPDIR", seal.path());
    // How this binary finds its libraries, where a dev shell sets it: not a way into the session.
    if let Some(libraries) = env::var_os("LD_LIBRARY_PATH") {
        command.env("LD_LIBRARY_PATH", libraries);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    // libtest prints `running 1 test` for the probe: a filter that matched nothing passes too.
    assert!(stdout.contains("running 1 test"), "{stdout}");
    assert!(stdout.contains("ok: refused 16 near misses"), "{stdout}");
    assert!(stdout.contains("ok: let through"), "{stdout}");
}
