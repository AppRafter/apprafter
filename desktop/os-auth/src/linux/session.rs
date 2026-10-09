// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Whether polkitd sees the app in an active local session, read from the files it reads.
//!
//! polkit answers "not authorized, and no challenge" for two reasons the answer does not tell
//! apart: the policy's own defaults (`allow_any` and `allow_inactive` are `no`) refusing a
//! session that is not an active local one, and an administrator's `rules.d` rule returning
//! `NO`. In an active local session the defaults ask (`allow_active` is `auth_self`), so there
//! the refusal can only be the administrator's, and the authenticator takes it as final
//! ([`crate::linux::OsAuthenticator`]).
//!
//! The method: the reads polkitd 126 makes (`polkitbackendsessionmonitor-systemd.c`) through
//! sd-login, done on sd-login's files directly, since libsystemd is not in this workspace's
//! graph. The paths are relative to `/`:
//! 1. The session (`sd_pid_get_session`): the systemd hierarchy's line of `proc/self/cgroup`
//!    (`name=systemd` on cgroup v1 and the hybrid layout, else the unified `0::`), relative to
//!    PID 1's (`proc/1/cgroup`, without a trailing `/init.scope`; unreadable, as on a host). Past
//!    the leading `*.slice` components the first unit is `session-<id>.scope`. Failing that
//!    (`sd_pid_get_owner_uid`, `sd_uid_get_display`): the `DISPLAY=` of
//!    `run/systemd/users/<uid>`, for the uid of the last leading `user-<uid>.slice`. That is how
//!    polkitd places an app a desktop started in a scope of the user's own systemd, outside any
//!    session's cgroup. Neither: no session.
//! 2. Local (`sd_session_get_seat`): the session's `run/systemd/sessions/<id>` has a `SEAT=`.
//! 3. Active (`sd_uid_get_state`, falling back to `sd_session_is_active`): the session's user
//!    (`UID=`) has `STATE=active` in `run/systemd/users/<uid>`, which is true while any of their
//!    sessions is active; a missing file is `offline`. Only without a valid `UID=`, or with a user
//!    file whose `STATE=` is empty or absent, does the session's own `ACTIVE=` decide, and one
//!    that is absent or not a boolean counts as active, as polkitd reads sd-login's error.
//!
//! An inactive or remote session, or none, is what the policy's defaults refuse, and the
//! authenticator then still offers the password. The same reads where the app runs see what
//! polkitd sees on the same machine; a sandbox that hides `/run/systemd` from the app reads as
//! no session.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// Whether polkitd sees this process in an active local session (the module docs give the
/// reads). Reads a few small files; asks nobody.
pub fn active_local_session() -> bool {
    active_local_in(Path::new("/"))
}

/// The id of the logind session polkitd sees this process in (step 1 of the module docs), if
/// any: the session whose `Lock` the session watch listens for.
pub(crate) fn session_id() -> Option<String> {
    session_of(Path::new("/"))
}

/// [`active_local_session`] with the files under `root`.
fn active_local_in(root: &Path) -> bool {
    let Some(id) = session_of(root) else {
        return false;
    };
    let Ok(session) = env_file(&root.join("run/systemd/sessions").join(&id)) else {
        return false;
    };
    let local = session.get("SEAT").is_some_and(|seat| !seat.is_empty());
    local && active(root, &session)
}

/// Step 1: the process's session id, from its cgroup, else its user's display session.
fn session_of(root: &Path) -> Option<String> {
    let own = fs::read_to_string(root.join("proc/self/cgroup")).ok()?;
    let own = systemd_cgroup(&own)?;
    let init = fs::read_to_string(root.join("proc/1/cgroup")).ok();
    let init = init.as_deref().and_then(systemd_cgroup).map(init_root);
    let path = shifted(own, init);

    let mut slice = None;
    let mut unit = None;
    for component in path {
        if component.ends_with(".slice") {
            slice = Some(component);
        } else {
            unit = Some(component);
            break;
        }
    }
    let from_unit = unit
        .and_then(|unit| unit.strip_prefix("session-"))
        .and_then(|rest| rest.strip_suffix(".scope"))
        .filter(|id| valid_session_id(id));
    if let Some(id) = from_unit {
        return Some(id.to_owned());
    }
    let uid = slice?
        .strip_prefix("user-")?
        .strip_suffix(".slice")
        .and_then(parse_uid)?;
    let mut user = env_file(&root.join("run/systemd/users").join(uid.to_string())).ok()?;
    user.remove("DISPLAY").filter(|id| valid_session_id(id))
}

/// The path of the systemd hierarchy in a `/proc/<pid>/cgroup`.
fn systemd_cgroup(text: &str) -> Option<&str> {
    let mut unified = None;
    for line in text.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(id), Some(controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if controllers.split(',').any(|c| c == "name=systemd") {
            return Some(path);
        }
        if id == "0" && controllers.is_empty() {
            unified = Some(path);
        }
    }
    unified
}

/// PID 1's cgroup as the root sd-login reads others' relative to (`cg_get_root_path`).
fn init_root(path: &str) -> &str {
    ["/init.scope", "/system.slice", "/system"]
        .iter()
        .find_map(|suffix| path.strip_suffix(suffix))
        .unwrap_or(path)
}

/// `path`'s components, past `root`'s when it lies under it (`cg_shift_path`).
fn shifted<'a>(path: &'a str, root: Option<&str>) -> impl Iterator<Item = &'a str> {
    let components = |p: &'a str| p.split('/').filter(|c| !c.is_empty());
    let skip = root
        .map(|root| {
            root.split('/')
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|root| {
            let mut own = components(path);
            root.iter().all(|c| own.next() == Some(*c))
        })
        .map_or(0, |root| root.len());
    components(path).skip(skip)
}

/// Step 3, for a session's fields.
fn active(root: &Path, session: &HashMap<String, String>) -> bool {
    let user_state = session
        .get("UID")
        .and_then(|uid| parse_uid(uid))
        .and_then(|uid| user_state(root, uid));
    match user_state {
        Some(state) => state == "active",
        None => session
            .get("ACTIVE")
            .and_then(|active| parse_boolean(active))
            .unwrap_or(true),
    }
}

/// `sd_uid_get_state`: `None` where polkitd falls back to the session's `ACTIVE=`.
fn user_state(root: &Path, uid: u32) -> Option<String> {
    match env_file(&root.join("run/systemd/users").join(uid.to_string())) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some("offline".to_owned()),
        Err(_) => None,
        Ok(mut user) => user.remove("STATE").filter(|state| !state.is_empty()),
    }
}

/// The `KEY=value` lines of one of logind's files, the later of two assignments winning.
/// Comments and blank lines are skipped, and one pair of surrounding quotes is dropped: all the
/// fields read here need of sd-login's `parse_env_file`.
fn env_file(path: &Path) -> io::Result<HashMap<String, String>> {
    let text = fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(['#', ';']))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), unquote(value.trim()).to_owned()))
        .collect())
}

fn unquote(value: &str) -> &str {
    ['"', '\'']
        .iter()
        .find_map(|quote| value.strip_prefix(*quote)?.strip_suffix(*quote))
        .unwrap_or(value)
}

/// sd-login's `session_id_valid`: letters and digits, at least one.
fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// systemd's `parse_uid`: a decimal uid that is neither `(uid_t) -1` nor the 16-bit `-1`.
fn parse_uid(text: &str) -> Option<u32> {
    let uid = text.parse::<u32>().ok()?;
    (uid != u32::MAX && uid != u32::from(u16::MAX)).then_some(uid)
}

/// systemd's `parse_boolean`.
fn parse_boolean(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "1" | "yes" | "y" | "true" | "t" | "on" => Some(true),
        "0" | "no" | "n" | "false" | "f" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A directory standing in for `/`, with the files a test writes into it.
    struct Root(tempfile::TempDir);

    impl Root {
        fn new() -> Self {
            Self(tempfile::tempdir().unwrap())
        }

        /// Writes `text` to `path`, relative to the root.
        fn file(self, path: &str, text: &str) -> Self {
            let path = self.0.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
            self
        }

        fn active_local(&self) -> bool {
            active_local_in(self.0.path())
        }
    }

    /// A process in `cgroup` (cgroup v2), on a host whose PID 1 is systemd.
    fn host(cgroup: &str) -> Root {
        Root::new()
            .file("proc/1/cgroup", "0::/init.scope\n")
            .file("proc/self/cgroup", &format!("0::{cgroup}\n"))
    }

    const IN_SESSION: &str = "/user.slice/user-1000.slice/session-2.scope";
    /// Where GNOME starts an app: a scope of the user's systemd, outside any session's cgroup.
    const IN_USER_SCOPE: &str = "/user.slice/user-1000.slice/user@1000.service/app.slice/\
                                 app-gnome-dev.apprafter.desktop-4242.scope";
    /// What logind writes for walk's session 2 on seat0, as far as polkitd reads it.
    const SESSION: &str = "UID=1000\nUSER=walk\nACTIVE=1\nSTATE=active\nREMOTE=0\nSEAT=seat0\n";
    const USER: &str = "NAME=walk\nSTATE=active\nSESSIONS=2\nSEATS=seat0\n";

    /// polkitd's view of a process in `cgroup`, with session 2 and user 1000 as given (`None`:
    /// no file).
    fn view(cgroup: &str, session: Option<&str>, user: Option<&str>) -> bool {
        let mut root = host(cgroup);
        if let Some(session) = session {
            root = root.file("run/systemd/sessions/2", session);
        }
        if let Some(user) = user {
            root = root.file("run/systemd/users/1000", user);
        }
        root.active_local()
    }

    #[test]
    fn a_session_on_a_seat_whose_user_is_active_is_active_local() {
        assert!(view(IN_SESSION, Some(SESSION), Some(USER)));
        assert!(
            view(&format!("{IN_SESSION}/sub"), Some(SESSION), Some(USER)),
            "a cgroup below the session's scope is still in the session"
        );
    }

    /// polkitd's "local" is a seat: an SSH session has none.
    #[test]
    fn a_session_without_a_seat_is_not_local() {
        for session in [
            "UID=1000\nACTIVE=1\nREMOTE=1\nREMOTE_HOST=192.0.2.1\n",
            "UID=1000\nACTIVE=1\nREMOTE=0\nSEAT=\n",
        ] {
            assert!(!view(IN_SESSION, Some(session), Some(USER)), "{session}");
        }
    }

    /// polkitd asks whether the session's user is active (any of their sessions), and only when
    /// that cannot be read, whether the session is; an unreadable `ACTIVE=` it takes as active.
    #[test]
    fn activity_is_the_user_s_state_and_then_the_session_s() {
        let inactive_session = "UID=1000\nACTIVE=0\nSEAT=seat0\n";
        for (session, user, active) in [
            // The user's state decides.
            (inactive_session, Some("STATE=active\n"), true),
            (SESSION, Some("STATE=online\n"), false),
            (SESSION, Some("STATE=closing\n"), false),
            (SESSION, Some("STATE=lingering\n"), false),
            // No user file is "offline", whatever the session says.
            (SESSION, None, false),
            // A user file without a state, or with an empty one: the session's ACTIVE=.
            (SESSION, Some("NAME=walk\n"), true),
            (inactive_session, Some("NAME=walk\n"), false),
            (inactive_session, Some("STATE=\n"), false),
            ("UID=1000\nACTIVE=yes\nSEAT=seat0\n", Some("STATE=\n"), true),
            (
                "UID=1000\nACTIVE=garbage\nSEAT=seat0\n",
                Some("STATE=\n"),
                true,
            ),
            ("UID=1000\nSEAT=seat0\n", Some("STATE=\n"), true),
            // A session without a readable UID=: its ACTIVE= straight away.
            ("ACTIVE=1\nSEAT=seat0\n", None, true),
            ("ACTIVE=0\nSEAT=seat0\n", Some("STATE=active\n"), false),
            (
                "UID=walk\nACTIVE=0\nSEAT=seat0\n",
                Some("STATE=active\n"),
                false,
            ),
        ] {
            assert_eq!(
                view(IN_SESSION, Some(session), user),
                active,
                "session {session:?}, user {user:?}"
            );
        }
    }

    /// An app a desktop starts in a scope of the user's systemd is in no session's cgroup;
    /// polkitd then takes the user's graphical session, their `DISPLAY=`.
    #[test]
    fn outside_a_session_s_cgroup_the_user_s_display_session_counts() {
        let user = "STATE=active\nDISPLAY=2\n";
        assert!(view(IN_USER_SCOPE, Some(SESSION), Some(user)));
        assert!(
            !view(IN_USER_SCOPE, Some(SESSION), Some(USER)),
            "no DISPLAY="
        );
        assert!(
            !view(IN_USER_SCOPE, None, Some(user)),
            "no file for the display session"
        );
        let remote = "UID=1000\nACTIVE=1\nREMOTE=1\n";
        assert!(!view(IN_USER_SCOPE, Some(remote), Some(user)));
        for display in ["../2", "2/../2", "", "2 3"] {
            let user = format!("STATE=active\nDISPLAY={display}\n");
            assert!(
                !view(IN_USER_SCOPE, Some(SESSION), Some(&user)),
                "DISPLAY={display} is no session id"
            );
        }
    }

    /// The user comes from the cgroup (`user-<uid>.slice`), never from the process's uid.
    #[test]
    fn outside_any_user_s_slice_there_is_no_session() {
        let user = "STATE=active\nDISPLAY=2\n";
        for cgroup in [
            "/",
            "/system.slice/cron.service",
            "/user.slice/user-walk.slice/x.scope",
        ] {
            assert!(!view(cgroup, Some(SESSION), Some(user)), "{cgroup}");
        }
    }

    /// sd-login reads a cgroup relative to PID 1's, without its `/init.scope`: a container
    /// without a cgroup namespace, and one with.
    #[test]
    fn the_cgroup_is_read_relative_to_pid_1_s() {
        let pod = "/machine.slice/libpod-0123.scope";
        for (init, own) in [
            (pod.to_owned(), format!("{pod}{IN_SESSION}")),
            (format!("{pod}/init.scope"), format!("{pod}{IN_SESSION}")),
            ("/".to_owned(), IN_SESSION.to_owned()),
            ("/init.scope".to_owned(), IN_SESSION.to_owned()),
        ] {
            let root = Root::new()
                .file("proc/1/cgroup", &format!("0::{init}\n"))
                .file("proc/self/cgroup", &format!("0::{own}\n"))
                .file("run/systemd/sessions/2", SESSION)
                .file("run/systemd/users/1000", USER);
            assert!(root.active_local(), "PID 1 in {init}, the app in {own}");
        }
        let unshifted = Root::new()
            .file("proc/1/cgroup", &format!("0::{pod}\n"))
            .file("proc/self/cgroup", &format!("0::/other{IN_SESSION}\n"))
            .file("run/systemd/sessions/2", SESSION)
            .file("run/systemd/users/1000", USER);
        assert!(
            !unshifted.active_local(),
            "outside PID 1's cgroup the path is read as it is"
        );
        // PID 1's cgroup unreadable (`hidepid`): as on a host, where it is /init.scope.
        let hidden = Root::new()
            .file("proc/self/cgroup", &format!("0::{IN_SESSION}\n"))
            .file("run/systemd/sessions/2", SESSION)
            .file("run/systemd/users/1000", USER);
        assert!(hidden.active_local());
    }

    /// cgroup v1 and the hybrid layout: the `name=systemd` hierarchy, not the empty `0::/`.
    #[test]
    fn a_cgroup_v1_host_reads_the_name_systemd_hierarchy() {
        let root = Root::new()
            .file("proc/1/cgroup", "1:name=systemd:/init.scope\n0::/\n")
            .file(
                "proc/self/cgroup",
                &format!("4:memory:/user.slice\n1:name=systemd:{IN_SESSION}\n0::/\n"),
            )
            .file("run/systemd/sessions/2", SESSION)
            .file("run/systemd/users/1000", USER);
        assert!(root.active_local());
    }

    #[test]
    fn without_the_files_there_is_no_session() {
        assert!(!Root::new().active_local(), "nothing at all");
        assert!(!view(IN_SESSION, None, Some(USER)), "no session file");
        let no_cgroup = Root::new()
            .file("run/systemd/sessions/2", SESSION)
            .file("run/systemd/users/1000", "STATE=active\nDISPLAY=2\n");
        assert!(!no_cgroup.active_local(), "no cgroup file");
    }

    /// The parts of sd-login's env-file reading and id checks the answer depends on.
    #[test]
    fn values_and_ids_are_read_as_sd_login_reads_them() {
        let quoted = "# written by logind\nUID=\"1000\"\n ACTIVE = 1 \nSEAT='seat0'\n\n";
        assert!(view(IN_SESSION, Some(quoted), Some("STATE=\"active\"\n")));
        let later_wins = "UID=1000\nSEAT=\nSEAT=seat0\n";
        assert!(view(IN_SESSION, Some(later_wins), Some(USER)));
        let bad_id = "/user.slice/user-1000.slice/session-2.x.scope";
        assert!(
            !view(bad_id, Some(SESSION), Some(USER)),
            "session ids are letters and digits"
        );
    }
}
