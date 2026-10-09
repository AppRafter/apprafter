// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Linux session watch against real D-Bus daemons, inside the container
//! `scripts/test-osauth-linux.sh` builds: the container's system bus, on which the test plays
//! logind (the script's `--fake-logind` lets `walk` own `org.freedesktop.login1`; there is no
//! systemd), and, with `--session-bus`, a private session bus of `walk`'s, on which it plays the
//! screen savers. The app is in logind session `c1` as sd-login's files describe it, the session
//! the watch must find on its own. Each test is one case.
//!
//! ```text
//! bash scripts/test-osauth-linux.sh          # every case, each in a fresh container
//! ```
//!
//! They run nowhere else: they own logind's name on the system bus, so outside the container
//! they fail instead of passing quietly.
#![cfg(target_os = "linux")]

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use apprafter_os_auth::{watch, SessionEvent, SessionWatch};
use zbus::blocking::{connection, Connection};
use zbus::zvariant::OwnedObjectPath;

/// Longer than any step of a passing case.
const PATIENCE: Duration = Duration::from_secs(10);
/// How long a case listens for an event that must not come.
const QUIET: Duration = Duration::from_millis(700);

const LOGIN1: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIN1_SESSION: &str = "org.freedesktop.login1.Session";
/// Not the path logind's escaping would give `c1`: the watch must ask `GetSession`.
const OWN_SESSION: &str = "/org/freedesktop/login1/session/apprafter_test_own";
/// Another session's object, whose lock is not the app's.
const OTHER_SESSION: &str = "/org/freedesktop/login1/session/c2";
const FREEDESKTOP: &str = "org.freedesktop.ScreenSaver";
const GNOME: &str = "org.gnome.ScreenSaver";

/// What the script passes in; asserting it is what keeps a host run from passing.
fn container() -> String {
    assert_eq!(
        std::env::var("APPRAFTER_OSAUTH_CONTAINER").as_deref(),
        Ok("1"),
        "run these through scripts/test-osauth-linux.sh: they own logind's name on the system bus"
    );
    std::env::var("APPRAFTER_OSAUTH_SESSION")
        .expect("APPRAFTER_OSAUTH_SESSION is unset: run these through scripts/test-osauth-linux.sh")
}

/// logind's manager, as much of it as the watch asks: `GetSession` for the app's session.
struct Manager {
    id: String,
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Manager {
    fn get_session(&self, id: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        if id == self.id {
            OwnedObjectPath::try_from(OWN_SESSION)
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
        } else {
            Err(zbus::fdo::Error::Failed(format!("No session '{id}' known")))
        }
    }
}

/// The test's logind: owns `org.freedesktop.login1` on the system bus and sends its signals.
struct FakeLogind(Connection);

impl FakeLogind {
    fn start(session: &str) -> Self {
        let connection = connection::Builder::system()
            .and_then(|builder| builder.name(LOGIN1))
            .and_then(|builder| {
                builder.serve_at(
                    LOGIN1_PATH,
                    Manager {
                        id: session.to_owned(),
                    },
                )
            })
            .and_then(connection::Builder::build)
            .unwrap_or_else(|e| panic!("owning {LOGIN1} on the system bus (--fake-logind): {e}"));
        Self(connection)
    }

    fn prepare_for_sleep(&self, start: bool) {
        self.0
            .emit_signal(
                None::<&str>,
                LOGIN1_PATH,
                LOGIN1_MANAGER,
                "PrepareForSleep",
                &(start,),
            )
            .unwrap();
    }

    fn lock(&self, session: &str) {
        self.0
            .emit_signal(None::<&str>, session, LOGIN1_SESSION, "Lock", &())
            .unwrap();
    }
}

/// The test's screen savers: one connection owning both names on the session bus.
struct FakeScreenSavers(Connection);

impl FakeScreenSavers {
    fn start() -> Self {
        let connection = connection::Builder::session()
            .and_then(|builder| builder.name(FREEDESKTOP))
            .and_then(|builder| builder.name(GNOME))
            .and_then(connection::Builder::build)
            .unwrap_or_else(|e| panic!("owning the screen savers' names on the session bus: {e}"));
        Self(connection)
    }

    /// `ActiveChanged(active)` from the screen saver `name` (also its interface).
    fn active_changed(&self, name: &str, active: bool) {
        active_changed(&self.0, name, active);
    }
}

fn active_changed(connection: &Connection, name: &str, active: bool) {
    let path = format!("/{}", name.replace('.', "/"));
    connection
        .emit_signal(
            None::<&str>,
            path.as_str(),
            name,
            "ActiveChanged",
            &(active,),
        )
        .unwrap();
}

/// A watch whose events the case reads, ready to hear.
fn watching() -> (SessionWatch, Receiver<SessionEvent>) {
    let (events, received) = mpsc::channel();
    let watch = watch(move |event| {
        let _ = events.send(event);
    });
    assert!(
        watch.ready(PATIENCE),
        "the watch was not listening within {PATIENCE:?}"
    );
    (watch, received)
}

fn next(events: &Receiver<SessionEvent>) -> SessionEvent {
    events
        .recv_timeout(PATIENCE)
        .unwrap_or_else(|_| panic!("no event within {PATIENCE:?}"))
}

fn quiet(events: &Receiver<SessionEvent>, what: &str) {
    if let Ok(event) = events.recv_timeout(QUIET) {
        panic!("{what}: {event:?}");
    }
}

/// logind's sleep and the app's session lock, both screen savers' activation: each is heard,
/// and nothing else is, neither the wake, another session's lock, a screen saver going away,
/// nor an impostor that does not own the name it speaks for.
#[test]
#[ignore = "needs the session container: bash scripts/test-osauth-linux.sh"]
fn each_source_reports_its_event() {
    let session = container();
    let logind = FakeLogind::start(&session);
    let screen_savers = FakeScreenSavers::start();
    let (_watch, events) = watching();

    // On one connection, in order: the unheard signals come before the heard one.
    logind.prepare_for_sleep(false);
    logind.lock(OTHER_SESSION);
    logind.prepare_for_sleep(true);
    assert_eq!(
        next(&events),
        SessionEvent::Sleeping,
        "PrepareForSleep(true)"
    );
    logind.lock(OWN_SESSION);
    assert_eq!(
        next(&events),
        SessionEvent::Locked,
        "the app's session Lock"
    );

    screen_savers.active_changed(FREEDESKTOP, false);
    screen_savers.active_changed(FREEDESKTOP, true);
    assert_eq!(next(&events), SessionEvent::Locked, "{FREEDESKTOP}");
    screen_savers.active_changed(GNOME, false);
    screen_savers.active_changed(GNOME, true);
    assert_eq!(next(&events), SessionEvent::Locked, "{GNOME}");
    quiet(
        &events,
        "a wake, another session's lock or an inactive screen saver was heard",
    );

    let impostor = Connection::session().unwrap();
    active_changed(&impostor, FREEDESKTOP, true);
    active_changed(&impostor, GNOME, true);
    let impostor = Connection::system().unwrap();
    impostor
        .emit_signal(
            None::<&str>,
            LOGIN1_PATH,
            LOGIN1_MANAGER,
            "PrepareForSleep",
            &(true,),
        )
        .unwrap();
    impostor
        .emit_signal(None::<&str>, OWN_SESSION, LOGIN1_SESSION, "Lock", &())
        .unwrap();
    quiet(
        &events,
        "a signal from a connection that does not own the name was heard",
    );
}

/// Once the watch is dropped no signal reaches the callback, and the drop does not hang.
#[test]
#[ignore = "needs the session container: bash scripts/test-osauth-linux.sh"]
fn a_dropped_watch_reports_nothing() {
    let session = container();
    let logind = FakeLogind::start(&session);
    let screen_savers = FakeScreenSavers::start();
    let (watch, events) = watching();
    logind.lock(OWN_SESSION);
    assert_eq!(next(&events), SessionEvent::Locked, "heard while watching");

    let dropping = Instant::now();
    drop(watch);
    assert!(
        dropping.elapsed() < PATIENCE,
        "the drop took {:?}",
        dropping.elapsed()
    );
    logind.prepare_for_sleep(true);
    logind.lock(OWN_SESSION);
    screen_savers.active_changed(FREEDESKTOP, true);
    screen_savers.active_changed(GNOME, true);
    quiet(&events, "a signal reached a dropped watch");
}

/// Without a session bus the screen savers are skipped, and logind is still heard.
#[test]
#[ignore = "needs the session container: bash scripts/test-osauth-linux.sh"]
fn without_a_session_bus_logind_still_reports() {
    let session = container();
    assert!(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none()
            && std::env::var_os("XDG_RUNTIME_DIR").is_none(),
        "this case runs without a session bus"
    );
    assert!(Connection::session().is_err(), "there is no session bus");
    let logind = FakeLogind::start(&session);
    let (_watch, events) = watching();
    logind.prepare_for_sleep(true);
    assert_eq!(next(&events), SessionEvent::Sleeping);
    logind.lock(OWN_SESSION);
    assert_eq!(next(&events), SessionEvent::Locked);
}
