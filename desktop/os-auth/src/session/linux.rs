// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Linux: logind on the system bus and the screen saver on the session bus (the module docs of
//! [`super`] list the signals).
//!
//! One thread of the watch's own runs zbus's executor: it connects to each bus, adds a match
//! rule per signal, and then reads every stream at once. Each rule names the signal's sender,
//! so another program on the bus cannot speak for logind or the screen saver; a sender's
//! well-known name matches whenever its owner sends, so logind or a screen saver that starts
//! later is still heard. Dropping the sources closes a channel the thread also waits on, which
//! ends it; the drop then joins it.

use std::future::poll_fn;
use std::pin::Pin;
use std::task::Poll;
use std::thread::{self, JoinHandle};

use futures_lite::{future, StreamExt};
use zbus::message::Type;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, MatchRule, Message, MessageStream};

use super::{Emitter, Listening, Signal};
use crate::linux::session;

const LOGIN1: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIN1_SESSION: &str = "org.freedesktop.login1.Session";
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

/// Sets up every source the buses offer, then reports their signals until it is dropped.
async fn listen(emitter: Emitter) {
    let mut streams = Vec::new();
    // Kept for as long as their streams are read.
    let mut _connections = Vec::new();
    match Connection::system().await {
        Ok(system) => {
            streams.extend(logind(&system).await);
            _connections.push(system);
        }
        Err(error) => tracing::info!(
            "no system bus ({error}): sleep and session locks from logind are not watched"
        ),
    }
    match Connection::session().await {
        Ok(session_bus) => {
            for name in SCREEN_SAVERS {
                if let Some(stream) = subscribe(
                    &session_bus,
                    Kind::ScreenSaver,
                    rule(name, None, name, "ActiveChanged"),
                )
                .await
                {
                    streams.push(stream);
                }
            }
            _connections.push(session_bus);
        }
        Err(error) => {
            tracing::info!("no session bus ({error}): the screen saver's locks are not watched")
        }
    }
    emitter.ready(listening(streams.iter().map(|(kind, _)| *kind)));
    loop {
        let (kind, message) = next(&mut streams).await;
        if let Some(signal) = kind.signal(&message) {
            emitter.signal(signal);
        }
    }
}

/// What the streams set up hear: logind's `Lock` and the screen savers are the session's
/// locks, `PrepareForSleep` the sleeps.
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

/// logind's two sources: the manager's `PrepareForSleep`, and `Lock` on the app's session.
async fn logind(system: &Connection) -> Vec<(Kind, Pin<Box<MessageStream>>)> {
    let mut streams = Vec::new();
    let sleep = rule(LOGIN1, Some(LOGIN1_PATH), LOGIN1_MANAGER, "PrepareForSleep");
    streams.extend(subscribe(system, Kind::PrepareForSleep, sleep).await);
    match own_session(system).await {
        Ok(path) => {
            let lock = rule(LOGIN1, Some(path.as_str()), LOGIN1_SESSION, "Lock");
            streams.extend(subscribe(system, Kind::SessionLock, lock).await);
        }
        Err(why) => tracing::info!("{why}: logind's session locks are not watched"),
    }
    streams
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

/// The stream of messages `rule` matches, once the bus has added it; `None`, logged, when it
/// cannot.
async fn subscribe(
    bus: &Connection,
    kind: Kind,
    rule: Result<MatchRule<'static>, zbus::Error>,
) -> Option<(Kind, Pin<Box<MessageStream>>)> {
    let subscribed = match rule {
        Ok(rule) => MessageStream::for_match_rule(rule, bus, None).await,
        Err(error) => Err(error),
    };
    match subscribed {
        Ok(stream) => Some((kind, Box::pin(stream))),
        Err(error) => {
            tracing::info!("cannot listen for {kind:?} ({error}): it is not watched");
            None
        }
    }
}

/// The next message of any stream, with its kind. A stream that ends or fails is dropped;
/// without any, this waits for ever (until the watch stops).
async fn next(streams: &mut Vec<(Kind, Pin<Box<MessageStream>>)>) -> (Kind, Message) {
    poll_fn(|cx| {
        let mut index = 0;
        while index < streams.len() {
            let (kind, stream) = &mut streams[index];
            match stream.poll_next(cx) {
                Poll::Ready(Some(Ok(message))) => return Poll::Ready((*kind, message)),
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

    /// No bus, one bus, a logind without the app's session: each says what it hears.
    #[test]
    fn the_streams_set_up_say_what_the_watch_hears() {
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
