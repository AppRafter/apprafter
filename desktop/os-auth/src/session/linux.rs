// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Linux: logind on the system bus and the screen saver on the session bus (the module docs of
//! [`super`] list the signals).
//!
//! One thread of the watch's own runs zbus's executor: it connects to each bus, adds a match
//! rule per signal, and then reads every stream at once. Dropping the sources closes a channel
//! the thread also waits on, which ends it; the drop then joins it.
//!
//! Only logind speaks for logind, and only a screen saver for itself. Each rule names the
//! signal's sender by its well-known name, and the bus holds a broadcast to it: it delivers one
//! only from the connection that owns that name. A signal sent to the app's connection alone,
//! though, the bus delivers whatever its sender (the system bus's default policy lets every user
//! send signals), and zbus does not match a rule's well-known sender itself (zbus 5
//! `MatchRule::matches`) — so heard as it came, any local user could send the app
//! `PrepareForSleep(true)` and lock it, closing its unlock prompt, at will. The watch therefore
//! keeps a signal only when it is a broadcast, as logind and the screen savers always send
//! these, from the connection that owns the sender's name: asked of the bus at set-up, and asked
//! again when a broadcast comes from another connection, since the owner changes when logind
//! restarts or a screen saver starts. A sender that starts later is heard.
//!
//! A bus takes a rule for a name nobody owns, so a rule says nothing of whether anything will
//! ever send: on sway, i3 or Hyprland no screen saver runs, and on a system without logind
//! (Devuan, OpenRC without elogind) nothing sends `PrepareForSleep`. What the watch says it
//! hears ([`Listening`]) therefore counts a source only when its sender runs as it is set up: a
//! screen saver that owns its name, logind owning its name for sleeps, and logind having
//! answered for the app's session for its `Lock` — neither of logind's under WSL, whose logind
//! never sends a sleep ([`sleeps_expected`]) and never hears the Windows screen lock
//! ([`session_locks_expected`]). A name the bus can start proves nothing (its service file may
//! hand the start to a systemd that is not PID 1, as in a container), and on a desktop the
//! owner's session was made by a logind that runs. It listens to every source all the same, so
//! a sender that starts later is heard without being promised.

use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;
use std::task::Poll;
use std::thread::{self, JoinHandle};

use futures_lite::{future, StreamExt};
use zbus::message::Type;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, MatchRule, Message, MessageStream};

use super::{Emitter, Listening, Signal};
use crate::linux::session;

const LOGIN1: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIN1_SESSION: &str = "org.freedesktop.login1.Session";
/// The bus driver, which answers for names.
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
/// The screen savers' names, each also its interface: KDE and others, and GNOME.
const SCREEN_SAVERS: [&str; 2] = ["org.freedesktop.ScreenSaver", "org.gnome.ScreenSaver"];

/// Which signal a stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    PrepareForSleep,
    SessionLock,
    ScreenSaver,
}

impl Kind {
    /// The signal `message` is, or `None` for a body that is not the signal's.
    fn signal(self, message: &Message) -> Option<Signal> {
        let flag = || message.body().deserialize::<bool>().ok();
        match self {
            Self::PrepareForSleep => flag().map(|start| Signal::PrepareForSleep { start }),
            Self::SessionLock => Some(Signal::SessionLock),
            Self::ScreenSaver => flag().map(|active| Signal::ScreenSaverActive { active }),
        }
    }
}

/// The running sources: dropping them stops the thread and waits for it.
pub(super) struct Sources {
    stop: Option<async_channel::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Sources {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) fn start(emitter: Emitter) -> Sources {
    let (stop, stopped) = async_channel::bounded::<()>(1);
    let thread = {
        let emitter = emitter.clone();
        thread::Builder::new()
            .name("session-watch-dbus".to_owned())
            .spawn(move || {
                zbus::block_on(future::or(
                    async {
                        // Err once the sender is dropped: the watch stops.
                        let _ = stopped.recv().await;
                    },
                    listen(emitter),
                ));
            })
    };
    let thread = match thread {
        Ok(thread) => Some(thread),
        Err(error) => {
            tracing::warn!("no thread for logind and the screen saver ({error}): not watched");
            emitter.ready(Listening::NONE);
            None
        }
    };
    Sources {
        stop: Some(stop),
        thread,
    }
}

/// One source's stream: the signal it carries, the well-known name of its one sender, the bus it
/// comes on (to ask who owns that name), and its messages.
struct Stream {
    kind: Kind,
    sender: &'static str,
    bus: Connection,
    messages: Pin<Box<MessageStream>>,
}

/// The connection last seen owning each sender's name (the names on the two buses differ).
type Owners = HashMap<&'static str, Option<OwnedUniqueName>>;

/// What the set-up found.
#[derive(Default)]
struct Setup {
    streams: Vec<Stream>,
    /// The sources whose senders are there (see the module docs).
    heard: Vec<Kind>,
    owners: Owners,
}

/// Sets up every source the buses offer, then reports their signals until it is dropped.
async fn listen(emitter: Emitter) {
    let mut setup = Setup::default();
    match Connection::system().await {
        Ok(system) => logind(&mut setup, &system, kernel_release).await,
        Err(error) => tracing::info!(
            "no system bus ({error}): sleep and session locks from logind are not watched"
        ),
    }
    match Connection::session().await {
        Ok(session_bus) => screen_savers(&mut setup, &session_bus).await,
        Err(error) => {
            tracing::info!("no session bus ({error}): the screen saver's locks are not watched")
        }
    }
    let Setup {
        mut streams,
        heard,
        mut owners,
    } = setup;
    emitter.ready(listening(heard));
    loop {
        let (index, message) = next(&mut streams).await;
        let stream = &streams[index];
        if from_sender(stream, &message, &mut owners).await {
            if let Some(signal) = stream.kind.signal(&message) {
                emitter.signal(signal);
            }
        }
    }
}

/// How a message on a source's stream stands to the source's sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Broadcast by the connection that owns the sender's name: the signal.
    FromSender,
    /// Sent to the app's connection alone, which the senders never do (see the module docs):
    /// dropped, whoever sent it.
    Directed,
    /// Broadcast by another connection than the owner known, or with no owner known.
    NotFromOwner,
}

/// What `message` is to a source whose sender's name `owner` owns (see [`Verdict`]).
fn verdict(message: &Message, owner: Option<&OwnedUniqueName>) -> Verdict {
    let header = message.header();
    if header.destination().is_some() {
        return Verdict::Directed;
    }
    match (header.sender(), owner) {
        (Some(sender), Some(owner)) if sender.as_str() == owner.as_str() => Verdict::FromSender,
        _ => Verdict::NotFromOwner,
    }
}

/// Whether `message` on `stream` is its sender's signal ([`verdict`]). A broadcast from another
/// connection than the owner known asks the bus who owns the name now, once, and keeps the
/// answer: the owner changes when logind restarts or a screen saver starts.
async fn from_sender(stream: &Stream, message: &Message, owners: &mut Owners) -> bool {
    let known = owners.get(stream.sender).and_then(Option::as_ref);
    let verdict = match verdict(message, known) {
        Verdict::NotFromOwner => {
            let owner = owner(&stream.bus, stream.sender).await;
            let now = verdict(message, owner.as_ref());
            owners.insert(stream.sender, owner);
            now
        }
        verdict => verdict,
    };
    if verdict != Verdict::FromSender {
        // Debug only: anyone on the bus can send these, as often as they like.
        tracing::debug!(
            kind = ?stream.kind,
            ?verdict,
            "a signal not broadcast by {}: dropped",
            stream.sender
        );
    }
    verdict == Verdict::FromSender
}

/// What the sources whose senders are there hear: logind's `Lock` and the screen savers are the
/// session's locks, `PrepareForSleep` the sleeps.
fn listening(kinds: impl IntoIterator<Item = Kind>) -> Listening {
    kinds
        .into_iter()
        .fold(Listening::NONE, |listening, kind| match kind {
            Kind::PrepareForSleep => Listening {
                sleep: true,
                ..listening
            },
            Kind::SessionLock | Kind::ScreenSaver => Listening {
                lock: true,
                ..listening
            },
        })
}

/// logind's two sources, the manager's `PrepareForSleep` and `Lock` on the app's session, and
/// which of them have a logind to send them ([`logind_heard`], given the kernel's release that
/// `osrelease` reads).
async fn logind(setup: &mut Setup, system: &Connection, osrelease: fn() -> Option<String>) {
    let running = owner(system, LOGIN1).await;
    let mut found = LogindFound {
        runs: running.is_some(),
        sleep_rule: false,
        lock_rule: false,
    };
    let sleep = rule(LOGIN1, Some(LOGIN1_PATH), LOGIN1_MANAGER, "PrepareForSleep");
    if let Some(stream) = subscribe(system, Kind::PrepareForSleep, LOGIN1, sleep).await {
        setup.streams.push(stream);
        found.sleep_rule = true;
    }
    setup.owners.insert(LOGIN1, running);
    match own_session(system).await {
        Ok(path) => {
            let lock = rule(LOGIN1, Some(path.as_str()), LOGIN1_SESSION, "Lock");
            if let Some(stream) = subscribe(system, Kind::SessionLock, LOGIN1, lock).await {
                setup.streams.push(stream);
                found.lock_rule = true;
            }
        }
        Err(why) => tracing::info!("{why}: logind's session locks are not watched"),
    }
    setup.heard.extend(logind_heard(found, osrelease));
}

/// What logind's set-up found: whether logind owns its name, and which of its two rules the bus
/// took (the session's only once logind answered for the app's session).
#[derive(Debug, Clone, Copy)]
struct LogindFound {
    runs: bool,
    sleep_rule: bool,
    lock_rule: bool,
}

/// Which of logind's sources count as heard (see the module docs): `PrepareForSleep` when its
/// rule was taken and sleeps are expected ([`sleeps_expected`]), `Lock` when its rule on the
/// app's session was taken — logind answered for the session, so it runs — and session locks
/// are expected ([`session_locks_expected`]); neither under WSL. A source listened for but not
/// counted is logged with why. `osrelease` reads the kernel's release, only when it matters.
fn logind_heard(found: LogindFound, osrelease: impl Fn() -> Option<String>) -> Vec<Kind> {
    let mut heard = Vec::new();
    if found.sleep_rule {
        match sleeps_expected(found.runs, &osrelease) {
            Ok(()) => heard.push(Kind::PrepareForSleep),
            Err(why) => tracing::info!("{why}: sleeps are listened for, not expected"),
        }
    }
    if found.lock_rule {
        match session_locks_expected(&osrelease) {
            Ok(()) => heard.push(Kind::SessionLock),
            Err(why) => tracing::info!("{why}: session locks are listened for, not expected"),
        }
    }
    heard
}

/// The screen savers' sources, and which of them run.
async fn screen_savers(setup: &mut Setup, session_bus: &Connection) {
    for name in SCREEN_SAVERS {
        let active = rule(name, None, name, "ActiveChanged");
        let Some(stream) = subscribe(session_bus, Kind::ScreenSaver, name, active).await else {
            continue;
        };
        setup.streams.push(stream);
        let running = owner(session_bus, name).await;
        // A screen saver must run to send; none is started on demand.
        if running.is_some() {
            setup.heard.push(Kind::ScreenSaver);
        } else {
            tracing::info!("no {name} runs: its locks are listened for, not expected");
        }
        setup.owners.insert(name, running);
    }
}

/// Whether logind's `PrepareForSleep` can be expected here, given whether logind runs and the
/// kernel's release (`osrelease`, read only when it does); `Err` says why not. A logind the bus
/// could only start counts for nothing (see the module docs). Under WSL, whose kernel names it
/// (`microsoft` in WSL1's and WSL2's releases, `WSL` in WSL2's), logind may run with systemd
/// but never sends it: the WSL VM does not suspend through it. So the watch reports no sleep
/// there, and the page shows that lock-on-sleep has no sleep to follow.
fn sleeps_expected(
    logind_runs: bool,
    osrelease: impl FnOnce() -> Option<String>,
) -> Result<(), &'static str> {
    if !logind_runs {
        return Err("no logind runs on the system bus");
    }
    if wsl(osrelease()) {
        return Err("under WSL logind never sends sleeps (the WSL VM does not suspend through it)");
    }
    Ok(())
}

/// Whether logind's `Lock` on the app's session can be expected here, given the kernel's
/// release; `Err` says why not. logind answered for the session, so it runs. Under WSL it may
/// run with systemd, but the screen lock is Windows': Win+L locks the Windows session, and
/// nothing tells WSL's logind, so only a `loginctl lock-session` typed inside WSL would send
/// `Lock`. Counting it would have the page say the app hears screen locks it never hears, so the
/// watch reports none there. A screen saver on a WSLg session bus is not this rule's: it counts
/// as anywhere else, when it owns its name.
fn session_locks_expected(osrelease: impl FnOnce() -> Option<String>) -> Result<(), &'static str> {
    if wsl(osrelease()) {
        return Err(
            "under WSL the Windows screen lock never reaches logind (only `loginctl lock-session` \
             sends its Lock)",
        );
    }
    Ok(())
}

/// Whether a kernel release is WSL's: `microsoft` in WSL1's and WSL2's releases, `WSL` in
/// WSL2's. A release that could not be read is not.
fn wsl(osrelease: Option<String>) -> bool {
    osrelease.is_some_and(|release| {
        let release = release.to_ascii_lowercase();
        release.contains("microsoft") || release.contains("wsl")
    })
}

/// The running kernel's release, as `uname -r` prints it.
fn kernel_release() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()
}

/// The connection that owns `name` on `bus` now, if any: the bus driver's `GetNameOwner`.
async fn owner(bus: &Connection, name: &str) -> Option<OwnedUniqueName> {
    let reply = bus
        .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "GetNameOwner", &(name,))
        .await
        .ok()?;
    reply.body().deserialize::<OwnedUniqueName>().ok()
}

/// The object path of the session polkitd sees the app in, as logind names it.
async fn own_session(system: &Connection) -> Result<OwnedObjectPath, String> {
    let id = session::session_id().ok_or("the app is in no logind session")?;
    let reply = system
        .call_method(
            Some(LOGIN1),
            LOGIN1_PATH,
            Some(LOGIN1_MANAGER),
            "GetSession",
            &(id.as_str(),),
        )
        .await
        .map_err(|error| format!("logind does not know session {id} ({error})"))?;
    reply
        .body()
        .deserialize::<OwnedObjectPath>()
        .map_err(|error| format!("logind's answer for session {id} is no object path ({error})"))
}

/// A rule for the signal `member` of `interface`, sent by `sender` from `path` (any path when
/// `None`).
fn rule(
    sender: &'static str,
    path: Option<&str>,
    interface: &'static str,
    member: &'static str,
) -> Result<MatchRule<'static>, zbus::Error> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(sender)?
        .interface(interface)?
        .member(member)?;
    let rule = match path {
        Some(path) => rule.path(path.to_owned())?,
        None => rule,
    };
    Ok(rule.build())
}

/// The stream of messages `rule` matches, for `kind` from `sender`, once the bus has added it;
/// `None`, logged, when it cannot.
async fn subscribe(
    bus: &Connection,
    kind: Kind,
    sender: &'static str,
    rule: Result<MatchRule<'static>, zbus::Error>,
) -> Option<Stream> {
    let subscribed = match rule {
        Ok(rule) => MessageStream::for_match_rule(rule, bus, None).await,
        Err(error) => Err(error),
    };
    match subscribed {
        Ok(messages) => Some(Stream {
            kind,
            sender,
            bus: bus.clone(),
            messages: Box::pin(messages),
        }),
        Err(error) => {
            tracing::info!("cannot listen for {kind:?} ({error}): it is not watched");
            None
        }
    }
}

/// The next message of any stream, with the stream's index. A stream that ends or fails is
/// dropped; without any, this waits for ever (until the watch stops).
async fn next(streams: &mut Vec<Stream>) -> (usize, Message) {
    poll_fn(|cx| {
        let mut index = 0;
        while index < streams.len() {
            let Stream { kind, messages, .. } = &mut streams[index];
            match messages.poll_next(cx) {
                Poll::Ready(Some(Ok(message))) => return Poll::Ready((index, message)),
                // A message the connection could not read: the stream goes on.
                Poll::Ready(Some(Err(error))) => {
                    tracing::info!("a bad message on the {kind:?} stream ({error})");
                }
                Poll::Ready(None) => {
                    tracing::info!("the {kind:?} stream ended: it is not watched any more");
                    drop(streams.swap_remove(index));
                }
                Poll::Pending => index += 1,
            }
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(kind: Kind, body: Option<bool>) -> Option<Signal> {
        let builder = Message::signal("/org/example", "org.example.Interface", "Member").unwrap();
        let message = match body {
            Some(flag) => builder.build(&(flag,)).unwrap(),
            None => builder.build(&()).unwrap(),
        };
        kind.signal(&message)
    }

    #[test]
    fn each_stream_s_body_becomes_its_signal() {
        for (kind, body, expected) in [
            (
                Kind::PrepareForSleep,
                Some(true),
                Some(Signal::PrepareForSleep { start: true }),
            ),
            (
                Kind::PrepareForSleep,
                Some(false),
                Some(Signal::PrepareForSleep { start: false }),
            ),
            (Kind::PrepareForSleep, None, None),
            (Kind::SessionLock, None, Some(Signal::SessionLock)),
            (
                Kind::ScreenSaver,
                Some(true),
                Some(Signal::ScreenSaverActive { active: true }),
            ),
            (
                Kind::ScreenSaver,
                Some(false),
                Some(Signal::ScreenSaverActive { active: false }),
            ),
            (Kind::ScreenSaver, None, None),
        ] {
            assert_eq!(signal(kind, body), expected, "{kind:?} {body:?}");
        }
    }

    /// No sender, one bus's, a logind without the app's session: each says what it hears.
    #[test]
    fn the_sources_with_a_sender_say_what_the_watch_hears() {
        let none = Listening::NONE;
        let lock = Listening {
            lock: true,
            sleep: false,
        };
        let sleep = Listening {
            lock: false,
            sleep: true,
        };
        let both = Listening {
            lock: true,
            sleep: true,
        };
        for (kinds, expected) in [
            (vec![], none),
            (vec![Kind::ScreenSaver], lock),
            (vec![Kind::ScreenSaver, Kind::ScreenSaver], lock),
            (vec![Kind::SessionLock], lock),
            (vec![Kind::PrepareForSleep], sleep),
            (vec![Kind::PrepareForSleep, Kind::ScreenSaver], both),
            (
                vec![Kind::PrepareForSleep, Kind::SessionLock, Kind::ScreenSaver],
                both,
            ),
        ] {
            assert_eq!(listening(kinds.clone()), expected, "{kinds:?}");
        }
    }

    /// `PrepareForSleep(true)`, sent by `from` (the bus sets it), broadcast or `to` one
    /// connection.
    fn sent(from: Option<&str>, to: Option<&str>) -> Message {
        let mut builder = Message::signal(LOGIN1_PATH, LOGIN1_MANAGER, "PrepareForSleep").unwrap();
        if let Some(from) = from {
            builder = builder.sender(from).unwrap();
        }
        if let Some(to) = to {
            builder = builder.destination(to).unwrap();
        }
        builder.build(&(true,)).unwrap()
    }

    /// The bus already refuses a broadcast from a connection that does not own the rule's name;
    /// the watch holds to it as well, and drops what the bus does pass: a signal sent to the app
    /// alone, whoever sent it.
    #[test]
    fn only_a_broadcast_from_the_owner_of_the_name_is_its_signal() {
        let owner = OwnedUniqueName::try_from(":1.7").unwrap();
        let known = Some(&owner);
        for (from, to, owner, expected) in [
            (Some(":1.7"), None, known, Verdict::FromSender),
            (Some(":1.7"), Some(":1.9"), known, Verdict::Directed),
            (Some(":1.8"), Some(":1.9"), known, Verdict::Directed),
            (
                Some(":1.8"),
                Some("dev.apprafter.Desktop"),
                known,
                Verdict::Directed,
            ),
            (Some(":1.8"), None, known, Verdict::NotFromOwner),
            (Some(":1.7"), None, None, Verdict::NotFromOwner),
            (None, None, known, Verdict::NotFromOwner),
        ] {
            assert_eq!(
                verdict(&sent(from, to), owner),
                expected,
                "from {from:?} to {to:?}, the name owned by {owner:?}"
            );
        }
    }

    /// Sleeps come only from a logind that runs, and never under WSL, whose kernel names it:
    /// WSL2 runs logind with systemd, but its VM does not suspend through it.
    #[test]
    fn sleeps_are_expected_from_a_running_logind_outside_wsl() {
        let kernel = |release: &'static str| move || Some(release.to_owned());
        for (release, expected) in [
            ("6.10.3-arch1-1", true),
            ("6.1.0-26-amd64", true),
            // WSL2's kernels, WSL1's, and one built by hand.
            ("5.15.167.4-microsoft-standard-WSL2", false),
            ("6.6.87.2-microsoft-standard-WSL2+", false),
            ("4.4.0-19041-Microsoft", false),
            ("6.1.21-custom-wsl", false),
        ] {
            assert_eq!(
                sleeps_expected(true, kernel(release)).is_ok(),
                expected,
                "{release}"
            );
            assert!(
                sleeps_expected(false, kernel(release)).is_err(),
                "no logind: {release}"
            );
        }
        // A release that cannot be read is no WSL's.
        assert_eq!(sleeps_expected(true, || None), Ok(()));
        // Read only when logind runs.
        assert!(sleeps_expected(false, || panic!("read")).is_err());
        assert!(kernel_release().is_some_and(|release| !release.trim().is_empty()));
    }

    /// Under WSL logind promises neither half: no sleep, and no lock of the app's session
    /// either, though logind answered for it — Win+L locks Windows, never WSL's logind.
    #[test]
    fn under_wsl_logind_promises_neither_sleeps_nor_session_locks() {
        let kernel = |release: &'static str| move || Some(release.to_owned());
        for release in [
            "5.15.167.4-microsoft-standard-WSL2",
            "6.6.87.2-microsoft-standard-WSL2+",
            "4.4.0-19041-Microsoft",
            "6.1.21-custom-wsl",
        ] {
            assert!(sleeps_expected(true, kernel(release)).is_err(), "{release}");
            assert!(
                session_locks_expected(kernel(release)).is_err(),
                "{release}"
            );
        }
        for release in ["6.10.3-arch1-1", "6.1.0-26-amd64"] {
            assert_eq!(sleeps_expected(true, kernel(release)), Ok(()), "{release}");
            assert_eq!(session_locks_expected(kernel(release)), Ok(()), "{release}");
        }
        // A release that cannot be read is no WSL's.
        assert_eq!(session_locks_expected(|| None), Ok(()));
    }

    /// logind's counting as its set-up does it: with both rules taken from a logind that runs,
    /// both halves count outside WSL and neither under it; a rule not taken never counts, and a
    /// logind that does not run promises no sleep.
    #[test]
    fn logind_counts_what_it_heard_and_neither_half_under_wsl() {
        let release = |release: &'static str| move || Some(release.to_owned());
        let all = LogindFound {
            runs: true,
            sleep_rule: true,
            lock_rule: true,
        };
        let both = vec![Kind::PrepareForSleep, Kind::SessionLock];
        assert_eq!(logind_heard(all, release("6.10.3-arch1-1")), both);
        assert_eq!(logind_heard(all, || None), both);
        for wsl in [
            "5.15.167.4-microsoft-standard-WSL2",
            "4.4.0-19041-Microsoft",
        ] {
            assert_eq!(logind_heard(all, release(wsl)), Vec::<Kind>::new(), "{wsl}");
        }
        let without = |found: LogindFound| logind_heard(found, release("6.1.0-26-amd64"));
        assert_eq!(
            without(LogindFound {
                lock_rule: false,
                ..all
            }),
            [Kind::PrepareForSleep]
        );
        assert_eq!(
            without(LogindFound {
                sleep_rule: false,
                ..all
            }),
            [Kind::SessionLock]
        );
        assert_eq!(
            without(LogindFound { runs: false, ..all }),
            [Kind::SessionLock],
            "logind answered for the session: its lock still counts"
        );
    }

    #[test]
    fn the_rules_name_the_sender_the_interface_and_the_member() {
        let sleep = rule(LOGIN1, Some(LOGIN1_PATH), LOGIN1_MANAGER, "PrepareForSleep").unwrap();
        assert_eq!(
            sleep.to_string(),
            "type='signal',sender='org.freedesktop.login1',\
             interface='org.freedesktop.login1.Manager',member='PrepareForSleep',\
             path='/org/freedesktop/login1'"
        );
        let lock = rule(
            LOGIN1,
            Some("/org/freedesktop/login1/session/_32"),
            LOGIN1_SESSION,
            "Lock",
        )
        .unwrap();
        assert_eq!(
            lock.to_string(),
            "type='signal',sender='org.freedesktop.login1',\
             interface='org.freedesktop.login1.Session',member='Lock',\
             path='/org/freedesktop/login1/session/_32'"
        );
        for name in SCREEN_SAVERS {
            assert_eq!(
                rule(name, None, name, "ActiveChanged").unwrap().to_string(),
                format!("type='signal',sender='{name}',interface='{name}',member='ActiveChanged'")
            );
        }
        assert!(rule(LOGIN1, Some("not a path"), LOGIN1_SESSION, "Lock").is_err());
    }
}
