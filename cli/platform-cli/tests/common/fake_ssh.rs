// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The fake `ssh` of the read goldens (D.5 overview §6.1, d5-a2): the node `node status`
//! probes. [`FakeSsh::install`] writes a `/bin/sh` script, builtins only, to the sandbox's
//! `bin`. It takes the last argument as the remote command and the one before it as the host,
//! logs the command to `calls.log` and the whole argv to `argv.log` (`fake_kube`'s format:
//! `+<argument>` lines, `.` after each call; d5-f's policy baseline, B68), refuses any host but
//! `root@203.0.113.10` (TEST-NET-3, never routed), and answers by the remote command — or,
//! built with [`FakeSsh::down`], refuses every connection as an unreachable node does. A
//! refused host and a command it has no answer for are lines of `unanswered.log`, which every
//! case asserts is absent (`node status` reads a failed probe as `unknown`, so only the log
//! shows it). It writes no `known_hosts` and connects nowhere.

use std::fs;

use super::fake_kube::{quoted, read_argv, read_lines, reply_code, Reply, EMIT, LOG_ARGV};
use super::golden::Sandbox;

/// The node's public IPv4, as the node cases' provider lists it.
pub const NODE_ADDRESS: &str = "203.0.113.10";
/// ssh's words for a node whose port 22 refuses the connection.
pub const REFUSED: &str = "ssh: connect to host 203.0.113.10 port 22: Connection refused";
/// What the fake does with a remote command it has no answer for (`printf`, never `echo`).
const UNANSWERED: &str = r#"printf '%s\n' "$cmd" >> "$d/unanswered.log"; printf 'fake ssh: no answer for %s\n' "$cmd" >&2; exit 255"#;

/// The answers of a fake `ssh`, by remote command (module docs).
#[derive(Debug, Clone, Default)]
pub struct FakeSsh {
    arms: Vec<(String, Reply)>,
    down: bool,
}

impl FakeSsh {
    pub fn new() -> Self {
        Self::default()
    }

    /// A node SSH cannot reach: every call refused, exit 255.
    pub fn down() -> Self {
        FakeSsh {
            down: true,
            ..Self::default()
        }
    }

    /// `remote` prints `stdout`, exit 0.
    pub fn answer(self, remote: &str, stdout: &str) -> Self {
        self.arm(remote, Reply::Out(stdout.to_string()))
    }

    /// `remote` fails: `stderr` (one line, or none when empty) and `exit`.
    pub fn fail(self, remote: &str, exit: i32, stderr: &str) -> Self {
        let reply = Reply::Fail {
            exit,
            stderr: stderr.to_string(),
        };
        self.arm(remote, reply)
    }

    fn arm(mut self, remote: &str, reply: Reply) -> Self {
        assert!(
            !remote.contains('\''),
            "a remote command here holds no single quote: {remote}"
        );
        self.arms.retain(|(r, _)| r != remote);
        self.arms.push((remote.to_string(), reply));
        self
    }

    /// Writes the fake to `<sandbox>/bin/ssh`, its answers and logs under `<sandbox>/fakes/ssh`.
    pub fn install(&self, sb: &Sandbox) {
        let dir = sb.path("fakes/ssh");
        fs::create_dir_all(&dir).expect("the fake ssh's directory");
        let mut lines = vec![
            format!("d='{}'", quoted(&dir)),
            EMIT.to_string(),
            LOG_ARGV.to_string(),
        ];
        lines.extend(
            [
                r#"host="#,
                r#"cmd="#,
                r#"for a in "$@"; do host=$cmd; cmd=$a; done"#,
                r#"printf '%s\n' "$cmd" >> "$d/calls.log""#,
            ]
            .map(str::to_string),
        );
        if self.down {
            lines.push(format!("printf '%s\\n' '{REFUSED}' >&2"));
            lines.push("exit 255".to_string());
        }
        // A raw string, so the host check's mutant (t2m9) quotes plain text.
        lines.push(format!(r#"if [ "$host" != 'root@{NODE_ADDRESS}' ]; then"#));
        lines.push(
            r#"  printf 'unexpected host %s: %s\n' "$host" "$cmd" >> "$d/unanswered.log""#
                .to_string(),
        );
        lines.push(r#"  printf 'fake ssh: unexpected host %s\n' "$host" >&2"#.to_string());
        lines.push("  exit 255".to_string());
        lines.push("fi".to_string());
        lines.push(r#"case "$cmd" in"#.to_string());
        for (i, (remote, reply)) in self.arms.iter().enumerate() {
            let reply = reply_code(&dir, &i.to_string(), reply);
            lines.push(format!("'{remote}') {reply} ;;"));
        }
        lines.push(format!("*) {UNANSWERED} ;;"));
        lines.push("esac".to_string());
        super::stand_in::script(&sb.path("bin/ssh"), &lines.join("\n"));
    }
}

/// Every remote command the fake got, in order.
pub fn calls(sb: &Sandbox) -> Vec<String> {
    read_lines(&sb.path("fakes/ssh/calls.log"))
}

/// Each call's whole argv, in order.
pub fn argv(sb: &Sandbox) -> Vec<Vec<String>> {
    read_argv(&sb.path("fakes/ssh/argv.log"))
}

/// The remote commands the fake had no answer for, and each refused host
/// (`unexpected host <host>: <command>`).
pub fn unanswered(sb: &Sandbox) -> Vec<String> {
    read_lines(&sb.path("fakes/ssh/unanswered.log"))
}

/// Every call the fake got had an answer.
pub fn assert_all_answered(sb: &Sandbox) {
    let missing = unanswered(sb);
    assert!(
        missing.is_empty(),
        "the fake ssh had no answer for {missing:?}"
    );
}
