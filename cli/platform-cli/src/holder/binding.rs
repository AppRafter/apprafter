// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Who a holder serves (ADR 0068 §4; D.4a T51, holder §1.6). Pure: the OS facts come in through
//! [`ProcLookup`] and the socket's peer credentials, so every rule is unit- and mutation-tested.
//!
//! Unix: the peer runs as the holder's user, has a pid this pid namespace can see, and belongs
//! to the session the holder was born in — the terminal where the person typed the secret —
//! whose leader is still the process that was bound (sudo's `tty` timestamp rule: a session id
//! is reused only after its leader and every member are gone; the leader's start tells a new one
//! apart). Windows: the client is attached to the holder's console, which any process of the same
//! user can join (`AttachConsole`): best effort, and pinned as such by
//! `windows_console_binding_admits_a_process_that_attaches_itself` (Task 7).

use super::protocol::Refusal;

/// What a Unix holder recorded at start: its uid, its session, and that session leader's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionBinding<S> {
    pub uid: u32,
    pub sid: i32,
    pub leader_started: S,
}

/// A connected client, as the socket reports it (`SO_PEERCRED`; `getpeereid` + `LOCAL_PEERPID`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    pub uid: u32,
    pub pid: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Serve,
    Refuse(Refusal),
    /// The bound session is gone: the holder ends.
    EndNow,
}

/// What a holder may ask about other processes.
pub trait ProcLookup {
    /// A process's start, compared only with another reading of the same pid.
    type Start: Copy + Eq;
    fn sid_of(&self, pid: i32) -> Option<i32>;
    fn started(&self, pid: i32) -> Option<Self::Start>;
}

/// Unix (T51): the leader first — a gone or replaced leader ends the holder whoever asks —
/// then the peer: the holder's uid, a pid above 0 (`SO_PEERCRED` reports 0 for a peer outside
/// this pid namespace, and `getsid(0)` would then be the holder's own session), the bound session.
pub fn admit<P: ProcLookup>(b: &SessionBinding<P::Start>, peer: PeerCred, os: &P) -> Admission {
    if os.started(b.sid) != Some(b.leader_started) {
        return Admission::EndNow;
    }
    if peer.uid != b.uid || peer.pid <= 0 {
        return Admission::Refuse(Refusal::NotThisTerminal);
    }
    if os.sid_of(peer.pid) == Some(b.sid) {
        Admission::Serve
    } else {
        Admission::Refuse(Refusal::NotThisTerminal)
    }
}

/// Windows (T51): a client pid the holder's console lists, other than 0 and the holder's own.
pub fn admit_console(peer_pid: u32, console: &[u32], me: u32) -> Admission {
    if peer_pid != 0 && peer_pid != me && console.contains(&peer_pid) {
        Admission::Serve
    } else {
        Admission::Refuse(Refusal::NotThisTerminal)
    }
}

/// Windows (T52): the holder is alone on its console, or the console is gone.
pub fn console_alone(console: &[u32], me: u32) -> bool {
    console.iter().all(|p| *p == me)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// A machine where each pid has a session and a start.
    struct Procs(HashMap<i32, (i32, u64)>);

    impl ProcLookup for Procs {
        type Start = u64;
        fn sid_of(&self, pid: i32) -> Option<i32> {
            self.0.get(&pid).map(|(sid, _)| *sid)
        }
        fn started(&self, pid: i32) -> Option<u64> {
            self.0.get(&pid).map(|(_, start)| *start)
        }
    }

    /// The terminal's shell (pid 100, leading session 100) and a command in it (pid 101), an
    /// agent in another terminal (pid 201, session 200) — and pid 0, which `getsid(0)` answers
    /// with the asking process's own session: the holder's, the bound one.
    fn machine() -> Procs {
        Procs(HashMap::from([
            (0, (100, 1)),
            (100, (100, 7)),
            (101, (100, 9)),
            (200, (200, 3)),
            (201, (200, 4)),
        ]))
    }

    fn bound() -> SessionBinding<u64> {
        SessionBinding {
            uid: 1000,
            sid: 100,
            leader_started: 7,
        }
    }

    fn peer(uid: u32, pid: i32) -> PeerCred {
        PeerCred { uid, pid }
    }

    /// T51: a command of the unlocking terminal's session is served; one of another session, of
    /// another user, or with a pid this namespace cannot see (0, or a negative one) is not.
    #[test]
    fn the_session_of_the_unlocking_terminal_is_served_and_no_other() {
        let os = machine();
        assert_eq!(admit(&bound(), peer(1000, 101), &os), Admission::Serve);
        assert_eq!(
            admit(&bound(), peer(1000, 100), &os),
            Admission::Serve,
            "the shell itself"
        );
        for (who, p) in [
            ("another terminal", peer(1000, 201)),
            ("another user", peer(1001, 101)),
            (
                "SO_PEERCRED's 0 for a peer outside this pid namespace",
                peer(1000, 0),
            ),
            ("a negative pid", peer(1000, -1)),
            ("a pid that is gone", peer(1000, 999)),
        ] {
            assert_eq!(
                admit(&bound(), p, &os),
                Admission::Refuse(Refusal::NotThisTerminal),
                "{who}"
            );
        }
    }

    /// sudo's rule: a session leader that is gone, or whose pid now names another process (its
    /// start moved), ends the holder whoever asks.
    #[test]
    fn a_gone_or_replaced_session_leader_ends_the_holder() {
        let mut os = machine();
        os.0.insert(100, (100, 8));
        assert_eq!(admit(&bound(), peer(1000, 101), &os), Admission::EndNow);
        os.0.remove(&100);
        assert_eq!(admit(&bound(), peer(1000, 101), &os), Admission::EndNow);
    }

    /// T51 (Windows, best effort): a client the holder's console lists is served; one it does
    /// not, a pid 0, or the holder's own pid is not.
    #[test]
    fn a_windows_client_on_the_holders_console_is_served() {
        let console = [10, 11, 12];
        assert_eq!(admit_console(11, &console, 12), Admission::Serve);
        for pid in [13, 0, 12] {
            assert_eq!(
                admit_console(pid, &console, 12),
                Admission::Refuse(Refusal::NotThisTerminal),
                "pid {pid}"
            );
        }
    }

    /// T52 (Windows): the holder leaves a console it is alone on (the shell exited in a classic
    /// console window, which the holder would otherwise keep open), and one that is gone.
    #[test]
    fn a_windows_holder_alone_on_its_console_leaves() {
        assert!(console_alone(&[12], 12));
        assert!(console_alone(&[], 12));
        assert!(!console_alone(&[10, 12], 12));
    }
}
