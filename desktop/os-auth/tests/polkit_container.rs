// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The polkit backend against a real polkitd, inside the container `scripts/test-osauth-linux.sh`
//! builds: Debian's dbus and polkitd, `dev.apprafter.desktop.policy` installed, a user `walk`
//! with a known password, and an active local session for it that polkitd reads through
//! sd-login. Each test is one case. The script prepares the system for it (the policy file
//! present or not, a `rules.d` rule, the session) and runs exactly that test as `walk`; the
//! test brings the authentication agent: pkttyagent on a pseudo-terminal, a stub agent that
//! dismisses, or none.
//!
//! ```text
//! bash scripts/test-osauth-linux.sh          # every case, each in a fresh container
//! ```
//!
//! They run nowhere else: they need a polkitd of their own and a password to type, so outside
//! the container they fail instead of passing quietly.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthOutcome, CancelledBy, UnavailableReason};
use apprafter_os_auth::linux::polkit::{verify, Action};
use zbus::zvariant::{OwnedValue, Value};
use zbus_polkit::policykit1::{AuthorityProxyBlocking, Subject};

/// Longer than any step of a passing case, PAM's delay after a wrong password included.
const PATIENCE: Duration = Duration::from_secs(60);

/// What the script passes in; asserting it is what keeps a host run from passing.
struct Container {
    password: String,
    session: String,
}

fn container() -> Container {
    let var = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| {
            panic!("{name} is unset: run these through scripts/test-osauth-linux.sh")
        })
    };
    assert_eq!(
        var("APPRAFTER_OSAUTH_CONTAINER"),
        "1",
        "run these through scripts/test-osauth-linux.sh: they talk to the system's polkitd"
    );
    Container {
        password: var("APPRAFTER_OSAUTH_PASSWORD"),
        session: var("APPRAFTER_OSAUTH_SESSION"),
    }
}

const fn unavailable(reason: UnavailableReason) -> AuthOutcome {
    AuthOutcome::Unavailable { reason }
}

/// `verify` on a thread of its own, as the app runs it, so the test can play the user.
struct Request {
    outcome: mpsc::Receiver<AuthOutcome>,
}

impl Request {
    fn start(action: Action, cancel: &CancellationToken) -> Self {
        let (tx, outcome) = mpsc::channel();
        let cancel = cancel.clone();
        thread::spawn(move || {
            let _ = tx.send(verify(action, &cancel));
        });
        Self { outcome }
    }

    fn outcome(self, agent: Option<&TtyAgent>) -> AuthOutcome {
        self.outcome.recv_timeout(PATIENCE).unwrap_or_else(|_| {
            panic!(
                "verify did not return within {PATIENCE:?}; agent transcript:\n{}",
                agent.map(TtyAgent::transcript).unwrap_or_default()
            )
        })
    }

    /// Fails at once if `verify` has already answered, which it must not before the dialog.
    fn still_open(&self, agent: &TtyAgent) {
        if let Ok(outcome) = self.outcome.try_recv() {
            panic!(
                "verify answered {outcome:?} without a dialog; agent transcript:\n{}",
                agent.transcript()
            );
        }
    }
}

/// pkttyagent, polkit's text agent, on a pseudo-terminal the test types into. It registers for
/// its parent process, this test, which is how polkitd finds it for the test's bus name.
struct TtyAgent {
    child: Child,
    terminal: File,
    output: Arc<Mutex<Vec<u8>>>,
}

impl TtyAgent {
    fn spawn() -> Self {
        let (terminal, user_side) = open_pty();
        let mut notify = [0; 2];
        // SAFETY: `notify` has room for the two descriptors pipe2 writes.
        let rc = unsafe { libc::pipe2(notify.as_mut_ptr(), libc::O_CLOEXEC) };
        assert_eq!(rc, 0, "pipe2: {}", std::io::Error::last_os_error());
        // SAFETY: pipe2 succeeded, so both descriptors are open and owned by nobody else.
        let (mut registered, notify_write) = unsafe {
            (
                File::from_raw_fd(notify[0]),
                OwnedFd::from_raw_fd(notify[1]),
            )
        };
        let notify_raw = notify_write.as_raw_fd();

        let mut command = Command::new("pkttyagent");
        command
            .args(["--notify-fd", "3"])
            .stdin(Stdio::from(user_side.try_clone().unwrap()))
            .stdout(Stdio::from(user_side.try_clone().unwrap()))
            .stderr(Stdio::from(user_side));
        // SAFETY: only async-signal-safe calls between fork and the new program.
        unsafe {
            command.pre_exec(move || {
                // The pty becomes the agent's controlling terminal, which it reads the password
                // from (`ctermid()`).
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // fd 3 is the one pkttyagent closes once it has registered.
                let rc = if notify_raw == 3 {
                    libc::fcntl(3, libc::F_SETFD, 0)
                } else {
                    libc::dup2(notify_raw, 3)
                };
                if rc < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn pkttyagent");
        drop(notify_write);

        let output = Arc::new(Mutex::new(Vec::new()));
        {
            let output = Arc::clone(&output);
            let mut reader = terminal.try_clone().unwrap();
            thread::spawn(move || {
                let mut buf = [0; 4096];
                // Ends with EIO once the agent has exited and the terminal has no user side.
                while let Ok(n @ 1..) = reader.read(&mut buf) {
                    output.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
        }
        let mut agent = Self {
            child,
            terminal,
            output,
        };

        // The pipe closes when pkttyagent has registered, or when it has exited.
        let mut ignored = Vec::new();
        wait_readable(&registered, "pkttyagent registered");
        registered.read_to_end(&mut ignored).unwrap();
        if let Some(status) = agent.child.try_wait().unwrap() {
            panic!(
                "pkttyagent exited ({status}) instead of registering:\n{}",
                agent.transcript()
            );
        }
        agent
    }

    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// Waits for the agent's `nth` password prompt (from 1) to be open: printed, and the
    /// terminal's echo off. pkttyagent flushes the terminal's input when it turns echo off, so
    /// anything typed before that would be lost.
    fn wait_for_prompt(&self, nth: usize, request: &Request) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let prompts = self.transcript().matches("Password:").count();
            if prompts >= nth && !self.echoes() {
                return;
            }
            request.still_open(self);
            assert!(
                Instant::now() < deadline,
                "no password prompt #{nth}; agent transcript:\n{}",
                self.transcript()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn echoes(&self) -> bool {
        // SAFETY: an all-zero termios is a valid value for tcgetattr to overwrite.
        let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: the descriptor is the open pty master; `attrs` is a valid termios.
        let rc = unsafe { libc::tcgetattr(self.terminal.as_raw_fd(), &mut attrs) };
        assert_eq!(rc, 0, "tcgetattr: {}", std::io::Error::last_os_error());
        attrs.c_lflag & libc::ECHO != 0
    }

    fn type_line(&self, line: &str) {
        (&self.terminal)
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
    }
}

impl Drop for TtyAgent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A new pseudo-terminal: the side the test types into, and the side the agent gets.
fn open_pty() -> (File, File) {
    // SAFETY: plain libc calls on a descriptor this function owns; ptsname_r writes at most
    // `name.len()` bytes and NUL-terminates on success.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        assert!(
            master >= 0,
            "posix_openpt: {}",
            std::io::Error::last_os_error()
        );
        let terminal = File::from_raw_fd(master);
        assert_eq!(libc::grantpt(master), 0, "grantpt");
        assert_eq!(libc::unlockpt(master), 0, "unlockpt");
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(
            libc::ptsname_r(master, name.as_mut_ptr(), name.len()),
            0,
            "ptsname_r"
        );
        let path = CStr::from_ptr(name.as_ptr()).to_str().unwrap().to_owned();
        let user_side = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
            .open(&path)
            .unwrap_or_else(|e| panic!("open {path}: {e}"));
        (terminal, user_side)
    }
}

fn wait_readable(file: &File, what: &str) {
    let mut poll = libc::pollfd {
        fd: file.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = libc::c_int::try_from(PATIENCE.as_millis()).unwrap();
    // SAFETY: one valid pollfd.
    let ready = unsafe { libc::poll(&mut poll, 1, millis) };
    assert_eq!(ready, 1, "{what} within {PATIENCE:?}");
}

/// An authentication agent that answers every request as its user closing the dialog: the
/// error GNOME Shell's agent returns on Cancel. pkttyagent cannot do that: on end of input it
/// fails the authentication instead. Registered for the session, as a desktop's agent is.
struct DismissingAgent {
    asked: Arc<AtomicUsize>,
    _connection: zbus::blocking::Connection,
}

struct Dismiss {
    asked: Arc<AtomicUsize>,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Cancelled(String),
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Dismiss {
    #[allow(clippy::too_many_arguments)]
    fn begin_authentication(
        &self,
        _action_id: String,
        _message: String,
        _icon_name: String,
        _details: HashMap<String, String>,
        _cookie: String,
        _identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> Result<(), AgentError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Err(AgentError::Cancelled(
            "Authentication dialog was dismissed by the user".to_owned(),
        ))
    }

    fn cancel_authentication(&self, _cookie: String) -> Result<(), AgentError> {
        Ok(())
    }
}

impl DismissingAgent {
    const PATH: &'static str = "/dev/apprafter/test/DismissingAgent";

    fn register(session: &str) -> Self {
        let asked = Arc::new(AtomicUsize::new(0));
        let connection = zbus::blocking::connection::Builder::system()
            .unwrap()
            .serve_at(
                Self::PATH,
                Dismiss {
                    asked: Arc::clone(&asked),
                },
            )
            .unwrap()
            .build()
            .expect("connect to the system bus");
        let session = OwnedValue::try_from(Value::from(session)).unwrap();
        let subject = Subject {
            subject_kind: "unix-session".to_owned(),
            subject_details: HashMap::from([("session-id".to_owned(), session)]),
        };
        AuthorityProxyBlocking::new(&connection)
            .unwrap()
            .register_authentication_agent(&subject, "C", Self::PATH)
            .expect("register the dismissing agent for the session");
        Self {
            asked,
            _connection: connection,
        }
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

/// The user types their password: both actions, each asking again (`auth_self`, no kept grant).
#[test]
#[ignore = "needs the polkit container: bash scripts/test-osauth-linux.sh"]
fn the_right_password_is_verified_and_asked_for_every_time() {
    let env = container();
    let agent = TtyAgent::spawn();
    for (nth, action) in [(1, Action::Unlock), (2, Action::Confirm)] {
        let request = Request::start(action, &CancellationToken::new());
        agent.wait_for_prompt(nth, &request);
        agent.type_line(&env.password);
        assert_eq!(
            request.outcome(Some(&agent)),
            AuthOutcome::Verified,
            "{action:?}; agent transcript:\n{}",
            agent.transcript()
        );
    }
}

#[test]
#[ignore = "needs the polkit container: bash scripts/test-osauth-linux.sh"]
fn a_wrong_password_fails() {
    let env = container();
    let agent = TtyAgent::spawn();
    let request = Request::start(Action::Unlock, &CancellationToken::new());
    agent.wait_for_prompt(1, &request);
    agent.type_line(&format!("not-{}", env.password));
    assert_eq!(
        request.outcome(Some(&agent)),
        AuthOutcome::Failed { exhausted: false },
        "agent transcript:\n{}",
        agent.transcript()
    );
}

#[test]
#[ignore = "needs the polkit container: bash scripts/test-osauth-linux.sh"]
fn a_dismissed_dialog_is_cancelled_by_the_user() {
    let env = container();
    let agent = DismissingAgent::register(&env.session);
    let outcome = verify(Action::Unlock, &CancellationToken::new());
    assert_eq!(
        outcome,
        AuthOutcome::Cancelled {
            by: CancelledBy::User
        }
    );
    assert_eq!(agent.asked(), 1, "polkitd asked the agent once");
}

#[test]
#[ignore = "needs the polkit container: bash scripts/test-osauth-linux.sh"]
fn without_an_agent_nobody_can_ask() {
    container();
    let outcome = verify(Action::Unlock, &CancellationToken::new());
    assert_eq!(outcome, unavailable(UnavailableReason::NoAgent));
}

/// The script removed the policy file.
#[test]
#[ignore = "needs the polkit container: bash scripts/test-osauth-linux.sh"]
fn without_the_policy_file_the_action_is_missing() {
    container();
    for action in [Action::Unlock, Action::Confirm] {
        let outcome = verify(action, &CancellationToken::new());
        assert_eq!(
            outcome,
            unavailable(UnavailableReason::PolicyMissing),
            "{action:?}"
        );
    }
}

/// The script installed a `rules.d` rule that answers YES for the app's actions.
#[test]
#[ignore = "needs the polkit container: bash scripts/test-osauth-linux.sh"]
fn a_rule_that_grants_without_asking_is_refused() {
    container();
    let outcome = verify(Action::Confirm, &CancellationToken::new());
    assert_eq!(outcome, unavailable(UnavailableReason::ImplicitGrant));
}

/// The script ran this case outside the session: `allow_any` and `allow_inactive` are `no`.
#[test]
#[ignore = "needs the polkit container: bash scripts/test-osauth-linux.sh"]
fn outside_an_active_local_session_it_is_not_permitted() {
    container();
    let outcome = verify(Action::Unlock, &CancellationToken::new());
    assert_eq!(outcome, unavailable(UnavailableReason::NotPermittedHere));
}

/// Lock-on-sleep while the dialog is open: polkitd closes it on `CancelCheckAuthorization`
/// and answers at once, though nobody typed anything; without the cancel `verify` would wait
/// for the password.
#[test]
#[ignore = "needs the polkit container: bash scripts/test-osauth-linux.sh"]
fn a_dialog_the_app_cancels_is_cancelled_by_the_app() {
    container();
    let agent = TtyAgent::spawn();
    let cancel = CancellationToken::new();
    let request = Request::start(Action::Unlock, &cancel);
    agent.wait_for_prompt(1, &request);
    cancel.cancel();
    assert_eq!(
        request.outcome(Some(&agent)),
        AuthOutcome::Cancelled {
            by: CancelledBy::App
        },
        "agent transcript:\n{}",
        agent.transcript()
    );
}
