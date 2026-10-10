// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The single-instance lock on the session bus the process is given (WI-456): the app's own
//! wiring of it — `SingleInstance::for_this_launch`, `register` on the app's builder, the app
//! built, then `log` — run in a child process of its own: this test binary again, made to run
//! [`wiring_probe`] on Tauri's mock runtime, with the environment a case gives it.
//!
//! No case reaches the session bus of whoever runs the tests. The child's environment is
//! cleared, then given `HOME` and `XDG_RUNTIME_DIR` in the case's temporary directory, the
//! loader's `LD_LIBRARY_PATH` when the parent has one, and the case's
//! `DBUS_SESSION_BUS_ADDRESS`: an address that does not parse reaches no bus at all, the
//! others name a socket in that directory or a private `dbus-daemon` the test starts from a
//! configuration of its own (as tests/theme_portal.rs does), and with the variable unset zbus
//! falls back on `$XDG_RUNTIME_DIR/bus`, in that directory too. `dbus-daemon` must be on `PATH`
//! for the last case (the nix desktop shell has it; CI installs it): without it that case fails
//! rather than passing quietly.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_desktop::single_instance::SingleInstance;
use tauri::test::mock_builder;
use zbus::blocking::{connection, fdo::DBusProxy};
use zbus::names::BusName;

/// Set only in the child processes these tests start: the probe refuses to run without it.
const PROBE: &str = "APPRAFTER_TEST_SINGLE_INSTANCE_PROBE";

/// Longer than any child of a passing case takes, the bus that never answers included.
const PATIENCE: Duration = Duration::from_secs(30);

/// What the probe prints once the app is built, before the app's identifier.
const STARTED: &str = "started identifier=";

/// What the app logs, at `WARN`, when it starts without the lock.
const OFF: &str = "single-instance is off";

/// The child's half: the lock as the app wires it, the log on standard output. Once the app is
/// built it prints [`STARTED`] and holds the lock until its standard input closes, so a parent
/// can try a second launch meanwhile.
#[test]
#[ignore = "run by the tests below, as their child"]
fn wiring_probe() {
    assert_eq!(
        std::env::var(PROBE).as_deref(),
        Ok("1"),
        "a probe runs only as its parent test's child"
    );
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_ansi(false)
        .without_time()
        .init();
    let single_instance = SingleInstance::for_this_launch();
    let app = single_instance
        .register(mock_builder())
        .build(tauri::generate_context!())
        .unwrap();
    single_instance.log();
    println!("{STARTED}{}", app.config().identifier);
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}

/// A child running [`wiring_probe`], its standard output read line by line as it comes.
struct Probe {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
}

/// How a child ended: its status, and everything it wrote.
struct Ended {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl Probe {
    /// Start the probe in `dir` (its `HOME` and `XDG_RUNTIME_DIR`), with
    /// `DBUS_SESSION_BUS_ADDRESS` set to `address`, or unset, and nothing else but the loader's
    /// library path.
    fn start(dir: &Path, address: Option<&str>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "wiring_probe",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env(PROBE, "1")
            .env("HOME", dir)
            .env("XDG_RUNTIME_DIR", dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(address) = address {
            command.env("DBUS_SESSION_BUS_ADDRESS", address);
        }
        // Where the loader finds the binary's libraries (GTK's, in a nix shell): it reaches no bus.
        if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
            command.env("LD_LIBRARY_PATH", path);
        }
        let mut child = command.spawn().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (send, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Probe {
            child,
            lines,
            seen: Vec::new(),
        }
    }

    /// What follows `marker` on the first line, from here on, that holds it (libtest starts the
    /// probe's first line with `test wiring_probe ... `); panics with what the child said if
    /// none comes within [`PATIENCE`].
    fn wait_for(&mut self, marker: &str) -> String {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if let Some((_, rest)) = line.split_once(marker) {
                        return rest.to_owned();
                    }
                }
                Err(e) => {
                    let ended = self.finish();
                    panic!(
                        "no line holding {marker:?} ({e:?}); the child said:\n{}\n{}",
                        ended.stdout, ended.stderr
                    );
                }
            }
        }
    }

    /// Close the child's standard input and wait for it to end, killing it after [`PATIENCE`].
    fn finish(&mut self) -> Ended {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + PATIENCE;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                break self.child.wait().unwrap();
            }
            thread::sleep(Duration::from_millis(20));
        };
        self.seen.extend(self.lines.try_iter());
        let mut stderr = String::new();
        let _ = self
            .child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr);
        Ended {
            status,
            stdout: self.seen.join("\n"),
            stderr,
        }
    }
}

impl Ended {
    /// The child built the app, logged at `WARN` that it runs without the lock, and why — the
    /// log line holding `why` — and ended on its own, cleanly.
    fn started_without_the_lock(&self, why: &str) {
        let said = format!("{}\n{}", self.stdout, self.stderr);
        assert!(self.status.success(), "{:?}:\n{said}", self.status);
        assert!(
            self.stdout.contains(STARTED),
            "the app never started:\n{said}"
        );
        let warning = self
            .stdout
            .lines()
            .find(|line| line.contains(OFF))
            .unwrap_or_else(|| panic!("nothing logged {OFF:?}:\n{said}"));
        assert!(warning.contains("WARN"), "not a warning: {warning}");
        assert!(warning.contains(why), "{why:?} is not why: {warning}");
    }
}

/// An address that does not parse: the plugin unwraps zbus's parse of it and panics while the
/// app starts. Empty, no `transport:` at all, and a transport with nothing to connect to.
#[test]
fn an_address_that_does_not_parse_starts_the_app_without_the_lock() {
    for address in ["", "not-an-address", "unix:"] {
        let dir = tempfile::tempdir().unwrap();
        Probe::start(dir.path(), Some(address))
            .finish()
            .started_without_the_lock("is no D-Bus address");
    }
}

/// An address that parses, with no bus behind it — named, or the fallback on
/// `$XDG_RUNTIME_DIR/bus` when the variable is unset: the plugin would fail quietly and the
/// app would run without the lock, its log silent about it.
#[test]
fn an_address_with_no_bus_behind_it_starts_the_app_without_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let nowhere = format!("unix:path={}", dir.path().join("no-bus").display());
    for address in [Some(nowhere.as_str()), None] {
        Probe::start(dir.path(), address)
            .finish()
            .started_without_the_lock("cannot be reached");
    }
}

/// A socket that takes the connection and never answers: the plugin would wait for it on the
/// main thread for ever. The app waits [`SingleInstance`]'s bound, then starts without the lock.
#[test]
fn a_bus_that_never_answers_starts_the_app_without_the_lock_in_time() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("silent-bus");
    // Never accepted: the connection waits in the backlog, and nothing ever answers it.
    let _listener = UnixListener::bind(&socket).unwrap();
    let address = format!("unix:path={}", socket.display());
    Probe::start(dir.path(), Some(&address))
        .finish()
        .started_without_the_lock("did not answer within");
}

/// A private bus, from a configuration of the test's own: no service directories (nothing
/// starts on demand), its socket in a temporary directory. Stopped when dropped.
struct PrivateBus {
    daemon: Child,
    address: String,
}

impl PrivateBus {
    fn start(dir: &Path) -> Self {
        let config = dir.join("bus.conf");
        let xml = format!(
            "<busconfig>\n  <type>session</type>\n  <listen>unix:dir={}</listen>\n  \
             <auth>EXTERNAL</auth>\n  <policy context=\"default\">\n    \
             <allow send_destination=\"*\" eavesdrop=\"true\"/>\n    \
             <allow eavesdrop=\"true\"/>\n    <allow own=\"*\"/>\n  </policy>\n</busconfig>\n",
            dir.display()
        );
        std::fs::write(&config, xml).unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!("this test needs dbus-daemon on PATH (the dbus-daemon package): {e}")
            });
        let mut line = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line.trim().to_owned();
        assert!(address.starts_with("unix:"), "dbus-daemon said {line:?}");
        PrivateBus { daemon, address }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// On a session bus that answers, the lock is the plugin's as before: the app owns the name the
/// plugin keys it on, logs nothing about it, and a second launch hands over and exits before its
/// app is built.
#[test]
fn on_a_bus_that_answers_the_lock_holds_and_a_second_launch_exits() {
    let dir = tempfile::tempdir().unwrap();
    let bus = PrivateBus::start(dir.path());
    let mut first = Probe::start(dir.path(), Some(&bus.address));
    let identifier = first.wait_for(STARTED);

    let connection = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .unwrap();
    let name = format!("{identifier}.SingleInstance");
    let owned = DBusProxy::new(&connection)
        .unwrap()
        .name_has_owner(BusName::try_from(name.as_str()).unwrap())
        .unwrap();
    assert!(owned, "nobody owns {name} on the bus");

    let second = Probe::start(dir.path(), Some(&bus.address)).finish();
    let said = format!("{}\n{}", second.stdout, second.stderr);
    assert!(second.status.success(), "{:?}:\n{said}", second.status);
    assert!(
        !second.stdout.contains(STARTED),
        "a second app started:\n{said}"
    );

    let first = first.finish();
    let said = format!("{}\n{}", first.stdout, first.stderr);
    assert!(first.status.success(), "{:?}:\n{said}", first.status);
    assert!(!first.stdout.contains(OFF), "{said}");
}
