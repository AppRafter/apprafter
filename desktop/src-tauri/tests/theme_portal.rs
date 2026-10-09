// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The desktop's colour scheme from the XDG desktop portal (`theme::portal`), against a real
//! D-Bus daemon: a private bus each test starts, on which the test plays the portal.
//!
//! The bus runs from a configuration of the test's own, with no service directories, so nothing
//! on it is started on demand (a real portal never is), and its socket lives in a temporary
//! directory: nothing of the host's session bus is reached. `dbus-daemon` must be on `PATH` (the
//! nix desktop shell has it; CI installs the `dbus-daemon` package): without it each test fails
//! rather than passing quietly.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_desktop::theme::portal::{self, Bus};
use apprafter_desktop::theme::{Appearance, Apply};
use apprafter_desktop_ipc::Theme;
use zbus::blocking::{connection, Connection};
use zbus::zvariant::{OwnedValue, Value};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";

/// Longer than any step of a passing test.
const PATIENCE: Duration = Duration::from_secs(10);
/// How long a test listens for an answer that must not come.
const QUIET: Duration = Duration::from_millis(500);

/// A private bus: its daemon, stopped when dropped, and the directory of its socket.
struct PrivateBus {
    daemon: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl PrivateBus {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("bus.conf");
        let xml = format!(
            "<busconfig>\n  <type>session</type>\n  <listen>unix:dir={}</listen>\n  \
             <auth>EXTERNAL</auth>\n  <policy context=\"default\">\n    \
             <allow send_destination=\"*\" eavesdrop=\"true\"/>\n    \
             <allow eavesdrop=\"true\"/>\n    <allow own=\"*\"/>\n  </policy>\n</busconfig>\n",
            dir.path().display()
        );
        std::fs::write(&config, xml).unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!("these tests need dbus-daemon on PATH (the dbus-daemon package): {e}")
            });
        let mut line = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line.trim().to_owned();
        assert!(address.starts_with("unix:"), "dbus-daemon said {line:?}");
        PrivateBus {
            daemon,
            address,
            _dir: dir,
        }
    }

    fn connect(&self) -> Connection {
        connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .unwrap()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// The portal's settings, as much of them as the watch asks: `ReadOne` for the colour scheme,
/// which fails as the portal does for a setting it does not have when `scheme` is `None`.
struct Settings {
    scheme: Arc<Mutex<Option<u32>>>,
    /// How long `ReadOne` takes to answer.
    delay: Duration,
}

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl Settings {
    fn read_one(&self, namespace: &str, key: &str) -> zbus::fdo::Result<OwnedValue> {
        thread::sleep(self.delay);
        match (namespace, key, *self.scheme.lock().unwrap()) {
            (APPEARANCE, COLOR_SCHEME, Some(scheme)) => Ok(OwnedValue::from(scheme)),
            _ => Err(zbus::fdo::Error::Failed(format!(
                "Requested setting {key} not found"
            ))),
        }
    }
}

/// The test's portal: owns `org.freedesktop.portal.Desktop` and sends `SettingChanged`.
struct FakePortal {
    connection: Connection,
    scheme: Arc<Mutex<Option<u32>>>,
}

impl FakePortal {
    fn start(bus: &PrivateBus, scheme: Option<u32>) -> Self {
        Self::answering_after(bus, scheme, Duration::ZERO)
    }

    fn answering_after(bus: &PrivateBus, scheme: Option<u32>, delay: Duration) -> Self {
        let scheme = Arc::new(Mutex::new(scheme));
        let connection = connection::Builder::address(bus.address.as_str())
            .and_then(|builder| builder.name(PORTAL))
            .and_then(|builder| {
                builder.serve_at(
                    PATH,
                    Settings {
                        scheme: scheme.clone(),
                        delay,
                    },
                )
            })
            .and_then(|builder| builder.build())
            .unwrap();
        FakePortal { connection, scheme }
    }

    /// The colour scheme changes, and the portal says so as it does: a broadcast from the
    /// owner of its name.
    fn set_scheme(&self, scheme: u32) {
        *self.scheme.lock().unwrap() = Some(scheme);
        self.signal(APPEARANCE, COLOR_SCHEME, Value::U32(scheme));
    }

    /// A `SettingChanged` broadcast, whatever it carries.
    fn signal(&self, namespace: &str, key: &str, value: Value<'_>) {
        emit(&self.connection, namespace, key, value);
    }
}

fn emit(connection: &Connection, namespace: &str, key: &str, value: Value<'_>) {
    connection
        .emit_signal(
            None::<&str>,
            PATH,
            SETTINGS,
            "SettingChanged",
            &(namespace, key, value),
        )
        .unwrap();
}

/// The watch on `bus`, every answer it reports on the returned channel.
fn watch(bus: &PrivateBus, call_timeout: Duration) -> Receiver<Option<u32>> {
    let (heard, answers) = mpsc::channel();
    portal::watch(
        Bus::Address(bus.address.clone()),
        call_timeout,
        move |scheme| {
            let _ = heard.send(scheme);
        },
    )
    .unwrap();
    answers
}

#[test]
fn the_first_answer_is_the_portal_s_and_every_change_follows() {
    let bus = PrivateBus::start();
    let portal = FakePortal::start(&bus, Some(1));
    let heard = watch(&bus, portal::CALL_TIMEOUT);
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(Some(1)));
    for scheme in [2, 0, 1] {
        portal.set_scheme(scheme);
        assert_eq!(heard.recv_timeout(PATIENCE), Ok(Some(scheme)));
    }
    assert!(heard.recv_timeout(QUIET).is_err());
}

/// Only the colour scheme, as a number, from the portal itself: another setting, another
/// namespace, a value that is no number, and the same signal from a connection that does not
/// own the portal's name are each dropped — broadcast, which the bus itself holds back for a
/// rule naming the portal, and sent to the watch's connection alone, which the bus delivers
/// whoever sends it (only the watch's own check of the sender stops that one). The portal's
/// own change after them is the control: the one answer heard.
#[test]
fn only_the_portal_s_own_colour_scheme_is_heard() {
    let bus = PrivateBus::start();
    let portal = FakePortal::start(&bus, Some(2));
    let heard = watch(&bus, portal::CALL_TIMEOUT);
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(Some(2)));

    portal.signal(APPEARANCE, "contrast", Value::U32(1));
    portal.signal("org.gnome.desktop.interface", COLOR_SCHEME, Value::U32(1));
    portal.signal(APPEARANCE, COLOR_SCHEME, Value::from("prefer-dark"));
    let forger = bus.connect();
    emit(&forger, APPEARANCE, COLOR_SCHEME, Value::U32(1));
    // Every other connection on the bus but the portal's is the watch's.
    let ours = [forger.unique_name(), portal.connection.unique_name()].map(|name| {
        name.expect("a bus connection has a unique name")
            .to_string()
    });
    let others: Vec<String> = zbus::blocking::fdo::DBusProxy::new(&forger)
        .unwrap()
        .list_names()
        .unwrap()
        .into_iter()
        .map(|name| name.to_string())
        .filter(|name| name.starts_with(':') && !ours.contains(name))
        .collect();
    assert!(!others.is_empty(), "the watch's connection is on the bus");
    for name in &others {
        forger
            .emit_signal(
                Some(name.as_str()),
                PATH,
                SETTINGS,
                "SettingChanged",
                &(APPEARANCE, COLOR_SCHEME, Value::U32(1)),
            )
            .unwrap();
    }

    portal.set_scheme(1);
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(Some(1)));
    assert!(heard.recv_timeout(QUIET).is_err());
}

/// No portal on the bus: the first answer is "none", at once (nothing starts it), and a
/// portal that starts later is heard from its first change.
#[test]
fn without_a_portal_the_answer_is_none_and_one_that_starts_later_is_heard() {
    let bus = PrivateBus::start();
    let heard = watch(&bus, portal::CALL_TIMEOUT);
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(None));
    let portal = FakePortal::start(&bus, Some(1));
    portal.set_scheme(2);
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(Some(2)));
}

/// A portal that does not have the setting answers "none", and its later changes are heard.
#[test]
fn a_portal_without_the_setting_answers_none() {
    let bus = PrivateBus::start();
    let portal = FakePortal::start(&bus, None);
    let heard = watch(&bus, portal::CALL_TIMEOUT);
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(None));
    portal.set_scheme(1);
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(Some(1)));
}

/// The read is bounded: a portal that does not answer within the call's timeout counts as
/// none, long before it would have answered.
#[test]
fn a_portal_that_does_not_answer_in_time_counts_as_none() {
    let bus = PrivateBus::start();
    let _portal = FakePortal::answering_after(&bus, Some(1), Duration::from_secs(5));
    let started = Instant::now();
    let heard = watch(&bus, Duration::from_millis(200));
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(None));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

/// The watch and the theme together, as the app starts them: under System the window follows
/// the portal; once the page chooses Light, the portal's changes no longer reach it.
#[test]
fn under_system_the_window_follows_the_portal_and_not_under_an_explicit_theme() {
    let bus = PrivateBus::start();
    let portal = FakePortal::start(&bus, Some(1));
    let appearance = Arc::new(Appearance::new(Theme::System));
    let (given_tx, given) = mpsc::channel();
    let apply: Apply = Box::new(move |theme| {
        let _ = given_tx.send(theme);
    });
    // GTK prefers light at start; the portal's dark wins once it answers.
    appearance.start(Some(false), apply);
    assert_eq!(given.recv_timeout(PATIENCE), Ok(tauri::Theme::Light));
    // Each answer, once the theme has had it.
    let (heard_tx, heard) = mpsc::channel();
    let watched = appearance.clone();
    portal::watch(
        Bus::Address(bus.address.clone()),
        portal::CALL_TIMEOUT,
        move |scheme| {
            watched.desktop_said(scheme);
            let _ = heard_tx.send(scheme);
        },
    )
    .unwrap();
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(Some(1)));
    assert_eq!(given.recv_timeout(PATIENCE), Ok(tauri::Theme::Dark));
    portal.set_scheme(2);
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(Some(2)));
    assert_eq!(given.recv_timeout(PATIENCE), Ok(tauri::Theme::Light));

    appearance.apply(Theme::Light, |_| ());
    portal.set_scheme(1);
    assert_eq!(heard.recv_timeout(PATIENCE), Ok(Some(1)));
    assert!(given.recv_timeout(QUIET).is_err());
}
