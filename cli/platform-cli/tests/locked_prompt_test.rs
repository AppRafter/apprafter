// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A command on a locked target store asks for its secret at its controlling terminal (D.4a
//! T61, T62) — never anywhere else — once per command, and puts the terminal back on every way
//! out; diagnostics never ask (T66). Each case runs the CLI at a pseudo-terminal of its own
//! (`common::terminal`), on `secrets::testing::locked_store`'s store, opened with its recovery
//! key: no case derives a key, in either build (T23). Unix only: the Windows console prompt has
//! no pseudo-console in the runners (its walk row is d4a-h's).
#![cfg(unix)]

mod common;

use apprafter_core::secrets::testing::{locked_store, LockedTestStore};
use common::spawn::{open_pty, Spawn};
use common::terminal::{converse, converse_with, line, Step};

const TOKEN: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";
const ASKED: &str = "Lock password or recovery key (asked again by the next command): ";
const INTRO: &str = "is locked, and `apprafter kubeconfig` needs its secrets.";

/// `prod` in a locked store, the CLI default, its kubeconfig cached under the store's own key.
fn locked(sp: &Spawn) -> LockedTestStore {
    let root = sp.store();
    let made = locked_store(&root, &[("prod", Some(TOKEN))]);
    let paths = cli_core::TargetStorePaths::for_root(root);
    cli_core::save_global_config(
        &paths,
        &cli_core::GlobalConfig {
            active_target: "prod".into(),
            version: cli_core::TARGET_STORE_VERSION,
        },
    )
    .unwrap();
    let state = cli_state::StatePaths::for_active_target(&paths, "prod");
    std::fs::create_dir_all(state.state_dir()).unwrap();
    let sealed = cli_core::secrets::encrypt_for_recipient(YAML, made.recipient()).unwrap();
    let body = serde_json::json!({
        "hetzner_cloud": {"server_id": 7, "server_name": "n", "kubeconfig_age": sealed},
    });
    std::fs::write(state.state_file(), body.to_string()).unwrap();
    made
}

/// The recovery key on one line, as a person types it back.
fn key_of(made: &LockedTestStore) -> String {
    made.recovery_key().expose().replace('\n', " ")
}

#[test]
fn kubeconfig_asks_once_and_the_recovery_key_opens_it() {
    let sp = Spawn::new();
    let made = locked(&sp);
    let end = converse(
        &sp,
        &["kubeconfig"],
        &[
            Step::Expect(INTRO),
            Step::Expect("Asked by `apprafter kubeconfig`, started by `"),
            Step::Expect(ASKED),
            line(&key_of(&made)),
        ],
    );
    assert!(end.status.success(), "{}", end.terminal);
    assert_eq!(end.stdout, YAML);
    assert_eq!(
        end.terminal.matches(ASKED).count(),
        1,
        "one prompt per command"
    );
    let data = made
        .recovery_key()
        .expose()
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    assert!(!end.terminal.contains(&data), "nothing typed is shown");
    assert!(end.restored);
}

/// T45: what `kubeconfig` printed went to a pipe, out of the store's lock: one note says so.
#[test]
fn a_kubeconfig_into_a_pipe_is_named_outside_the_lock() {
    let sp = Spawn::new();
    let made = locked(&sp);
    let end = converse(
        &sp,
        &["kubeconfig"],
        &[Step::Expect(ASKED), line(&key_of(&made))],
    );
    assert!(end.status.success(), "{}", end.terminal);
    assert_eq!(end.stdout, YAML, "the note is never on stdout");
    assert_eq!(
        end.terminal
            .matches("is outside the lock of the target store at")
            .count(),
        1,
        "{}",
        end.terminal
    );
}

#[test]
fn three_wrong_secrets_are_unlock_failed() {
    let sp = Spawn::new();
    locked(&sp);
    let wrong = "neither the lock password nor the recovery key";
    let end = converse(
        &sp,
        &["kubeconfig"],
        &[
            Step::Expect(ASKED),
            line("not it"),
            Step::Expect(wrong),
            Step::Expect(ASKED),
            line("nor this"),
            Step::Expect(wrong),
            Step::Expect(ASKED),
            line("nor that"),
        ],
    );
    assert_eq!(end.status.code(), Some(1));
    assert!(
        end.terminal.contains("apprafter::secrets::unlock_failed"),
        "{}",
        end.terminal
    );
    assert!(end.stdout.is_empty());
    assert!(end.restored);
}

/// T62: a mistyped recovery key names its group; the next try opens the store.
#[test]
fn a_mistyped_recovery_key_names_its_group_and_the_next_try_opens() {
    let sp = Spawn::new();
    let made = locked(&sp);
    let right = key_of(&made);
    let (prefix, data) = right.split_at(right.find(' ').unwrap() + 1);
    let mut chars: Vec<char> = data.chars().collect();
    chars[0] = if chars[0] == 'Q' { 'P' } else { 'Q' };
    let wrong = format!("{prefix}{}", chars.into_iter().collect::<String>());
    let end = converse(
        &sp,
        &["kubeconfig"],
        &[
            Step::Expect(ASKED),
            line(&wrong),
            Step::Expect("mistyped near group"),
            Step::Expect(ASKED),
            line(&right),
        ],
    );
    assert!(end.status.success(), "{}", end.terminal);
    assert_eq!(end.stdout, YAML);
}

/// G7: Ctrl-C cancels, nothing is changed, and the terminal comes back.
#[test]
fn ctrl_c_at_the_prompt_is_cancelled_and_the_terminal_comes_back() {
    let sp = Spawn::new();
    locked(&sp);
    let end = converse(
        &sp,
        &["kubeconfig"],
        &[Step::Expect(ASKED), Step::Type(b"half\x03".to_vec())],
    );
    assert_eq!(end.status.code(), Some(1));
    assert!(
        end.terminal.contains("apprafter::op::cancelled"),
        "{}",
        end.terminal
    );
    assert!(end.restored);
}

/// G7: SIGTERM and SIGHUP mid-prompt end the command as they would without a prompt, and the
/// terminal is put back first (SIGKILL cannot be handled, and is not tried).
#[test]
fn sigterm_and_sighup_mid_prompt_put_the_terminal_back() {
    use std::os::unix::process::ExitStatusExt;
    for sig in [libc::SIGTERM, libc::SIGHUP] {
        let sp = Spawn::new();
        locked(&sp);
        let end = converse(
            &sp,
            &["kubeconfig"],
            &[Step::Expect(ASKED), Step::Signal(sig)],
        );
        assert_eq!(end.status.signal(), Some(sig), "ended by signal {sig}");
        assert!(end.restored, "echo back after signal {sig}");
    }
}

/// Critic finding 15: a SIGTSTP from outside mid-prompt puts the terminal back before it acts,
/// and a process that goes on never reads on: the prompt ends as cancelled. (The child leads
/// its own session, so its process group is orphaned and the kernel discards the re-raised
/// stop — the "goes on" case, every time.)
#[test]
fn an_outside_sigtstp_mid_prompt_cancels_and_puts_the_terminal_back() {
    let sp = Spawn::new();
    locked(&sp);
    let end = converse(
        &sp,
        &["kubeconfig"],
        &[Step::Expect(ASKED), Step::Signal(libc::SIGTSTP)],
    );
    assert_eq!(end.status.code(), Some(1), "{}", end.terminal);
    assert!(
        end.terminal.contains("apprafter::op::cancelled"),
        "{}",
        end.terminal
    );
    assert!(end.stdout.is_empty());
    assert!(end.restored);
}

/// T61: the prompt is the terminal's, not stderr's: `2>/dev/null` cannot hide it.
#[test]
fn the_prompt_shows_with_stderr_thrown_away() {
    let sp = Spawn::new();
    let made = locked(&sp);
    let end = converse_with(
        &sp,
        &["kubeconfig"],
        &[
            Step::Expect(INTRO),
            Step::Expect(ASKED),
            line(&key_of(&made)),
        ],
        |cmd| {
            cmd.stderr(std::process::Stdio::null());
        },
    );
    assert!(end.status.success(), "{}", end.terminal);
    assert_eq!(end.stdout, YAML);
}

/// T61: a terminal on stdin that is not the process's controlling terminal (an agent's tool
/// with a pty but a session of its own) is no terminal: `store_locked`, nothing drawn.
#[test]
fn a_terminal_that_is_not_the_controlling_one_is_no_terminal() {
    let sp = Spawn::new();
    locked(&sp);
    let (master, slave) = open_pty();
    let out = sp
        .std(&["kubeconfig"])
        .stdin(std::process::Stdio::from(slave))
        .output()
        .expect("run apprafter");
    drop(master);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("apprafter::secrets::store_locked"),
        "{stderr}"
    );
}

/// T35 at a terminal: the override is refused before anything is asked.
#[test]
fn hcloud_token_is_refused_before_anything_is_asked() {
    let sp = Spawn::new().with_env("HCLOUD_TOKEN", "d".repeat(64));
    locked(&sp);
    let end = converse(&sp, &["target", "ip"], &[]);
    assert_eq!(end.status.code(), Some(1));
    assert!(
        end.terminal
            .contains("apprafter::secrets::override_refused"),
        "{}",
        end.terminal
    );
    assert!(!end.terminal.contains(ASKED));
}

/// T66: diagnostics never ask, even at a terminal.
#[test]
fn doctor_and_whoami_never_ask() {
    for args in [&["doctor", "--no-ping"][..], &["whoami", "--no-ping"][..]] {
        let sp = Spawn::new();
        locked(&sp);
        let end = converse(&sp, args, &[]);
        assert!(!end.terminal.contains(ASKED), "{args:?}:\n{}", end.terminal);
        assert!(end.restored);
    }
}
