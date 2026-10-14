// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! WI-475: a target store other accounts can change, through the CLI. Its `targets` folder is
//! opened to them in the test's own scratch store — mode 0777 on Unix, an ACE that lets the
//! Users group write on Windows — and then every command that writes the store's secrets, or
//! uses one, refuses before writing anything, with `apprafter::secrets::store_not_private`
//! naming the folder; `apprafter doctor` reports the folder and changes nothing; and once the
//! folder is this account's alone again, the store is used as before.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::spawn::Spawn;

const TOKEN: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const OTHER: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

/// stderr with miette's wrapping undone: its `│` gutter dropped, each run of whitespace one space.
fn squeeze(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` without any whitespace: miette wraps a long path anywhere, `C:` from the rest of it
/// included.
fn unwrapped(text: &str) -> String {
    text.split_whitespace().collect()
}

/// `apprafter <args>`; `ping`: whether it may reach the provider (here a closed port), which
/// `whoami` needs to read the token at all.
fn run(sp: &Spawn, args: &[&str], ping: bool) -> (Option<i32>, String, String) {
    let mut cmd = sp.cmd(args);
    if !ping {
        cmd.env("APPRAFTER_NO_PING", "1");
    }
    let out = cmd.output().expect("run apprafter");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        squeeze(&out.stderr),
    )
}

/// Every file and folder under `root`, a file with its bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.insert(path.clone(), None);
                stack.push(path);
            } else {
                out.insert(path.clone(), Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    out
}

/// Lets other accounts write to `folder` (`open`), or makes it this account's alone again.
#[cfg(unix)]
fn others_may_write(folder: &Path, open: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if open { 0o777 } else { 0o700 };
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Lets other accounts write to `folder` (`open`), or makes it this account's alone again.
#[cfg(windows)]
fn others_may_write(folder: &Path, open: bool) {
    use cli_core::owner_only::testing;
    let me = testing::this_user_sid().unwrap();
    let others = if open { "(A;OICI;0x1301bf;;;BU)" } else { "" };
    testing::set_dacl(folder, &format!("D:P(A;OICI;FA;;;{me}){others}")).unwrap();
}

#[test]
fn a_store_others_can_write_is_refused_reported_and_used_again_once_fixed() {
    let sp = Spawn::new();
    let add = ["target", "add", "prod", "--provider", "hetzner-cloud"];
    let (code, _, stderr) = run(&sp, &[&add[..], &["--token", TOKEN]].concat(), false);
    assert_eq!(code, Some(0), "{stderr}");
    let targets = sp.store().join("targets");
    others_may_write(&targets, true);
    let before = snapshot(&sp.store());
    let shown = targets.display().to_string();
    for (args, ping) in [
        (
            &[
                "target",
                "add",
                "dev",
                "--provider",
                "hetzner-cloud",
                "--token",
                TOKEN,
            ][..],
            false,
        ),
        (
            &[
                "target",
                "add",
                "prod",
                "--renew",
                "--no-interactive",
                "--token",
                OTHER,
            ],
            false,
        ),
        (&["target", "rename", "prod", "staging"], false),
        (&["target", "remove", "prod", "--yes"], false),
        (&["target", "use", "prod"], false),
        (&["whoami"], true),
        // d4a-h F1: refused when planned, before it asks for a password or shows a key.
        (&["lock", "on"], false),
    ] {
        let (code, stdout, stderr) = run(&sp, args, ping);
        assert_eq!(code, Some(1), "{args:?}: {stderr}");
        assert!(
            stderr.contains("apprafter::secrets::store_not_private")
                && unwrapped(&stderr)
                    .contains(&unwrapped(&format!("other accounts can write to {shown}"))),
            "{args:?}: {stderr}"
        );
        assert!(
            !stdout.contains(TOKEN) && !stderr.contains(TOKEN),
            "{args:?}"
        );
        assert_eq!(snapshot(&sp.store()), before, "{args:?} wrote to the store");
    }
    // What hands out no secret still runs (the troubleshooting entry names these).
    for args in [
        &["target", "list"][..],
        &["target", "show", "prod"],
        &["lock", "status"],
    ] {
        let (code, stdout, stderr) = run(&sp, args, false);
        assert_eq!(code, Some(0), "{args:?}: {stdout}\n{stderr}");
        assert!(!stdout.contains(TOKEN), "{args:?}");
    }
    // WI-481: `status` is a report: it is not refused before its first read, as the commands
    // that act on the state are; its cluster half, which reads the kubeconfig, says the refusal.
    let (code, stdout, stderr) = run(&sp, &["status"], false);
    assert_eq!(code, Some(0), "status: {stdout}\n{stderr}");
    assert!(stdout.contains("Target: prod"), "{stdout}");
    assert!(
        unwrapped(&stdout).contains(&unwrapped(&format!("other accounts can write to {shown}"))),
        "{stdout}"
    );
    assert_eq!(snapshot(&sp.store()), before, "the reads change nothing");
    let (_, stdout, stderr) = run(&sp, &["doctor", "--target", "prod", "--no-ping"], false);
    assert!(
        stdout.contains("Target store folder `targets` can be changed by other accounts"),
        "{stdout}\n{stderr}"
    );
    assert_eq!(snapshot(&sp.store()), before, "doctor changes nothing");
    others_may_write(&targets, false);
    let (code, stdout, stderr) = run(&sp, &["whoami"], true);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    let (code, _, stderr) = run(&sp, &["target", "use", "prod"], false);
    assert_eq!(code, Some(0), "{stderr}");
}

/// WI-481 (d4a-h P-3): every flag-driven command that reads the store's files and then uses a
/// secret of it refuses a store others can change BEFORE its first read, naming the folder. A
/// folder another account owns needs root to make, and the core's tests pretend one (in
/// process); here the order is shown with what a user can make: the store's root open to other
/// accounts (mode 0777, real) and the target's folder and its state folder closed to this one
/// (mode 000, its own). A command that read first stops there — a bare "Permission denied", or
/// a state it cannot see read as no state — and never says `store_not_private`. Outside a
/// terminal, the flags given: `target add`'s renewal, forced add and new add, and the commands
/// that resolve the target's state (`state_paths::resolve_state_paths`). Unix only: mode 000.
#[cfg(unix)]
#[test]
fn a_flag_driven_command_refuses_a_store_others_can_change_before_it_reads_it() {
    use std::os::unix::fs::PermissionsExt;
    let sp = Spawn::new();
    let add = ["target", "add", "prod", "--provider", "hetzner-cloud"];
    let (code, _, stderr) = run(&sp, &[&add[..], &["--token", TOKEN]].concat(), false);
    assert_eq!(code, Some(0), "{stderr}");
    let store = sp.store();
    let paths = cli_core::TargetStorePaths::for_root(store.clone());
    let state = cli_state::StatePaths::for_active_target(&paths, "prod");
    std::fs::create_dir_all(state.state_dir()).unwrap();
    let body = serde_json::json!({
        "hetzner_cloud": {"server_id": 7, "server_name": "n", "kubeconfig_yaml": "apiVersion: v1\n"},
    });
    std::fs::write(state.state_file(), body.to_string()).unwrap();
    let closed = [paths.target_dir("prod"), paths.state_dir("prod")];
    for folder in &closed {
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            std::fs::read_dir(folder).is_err(),
            "mode 000 does not lock this account out of {folder:?}: run as a user, not as root"
        );
    }
    others_may_write(&store, true);
    let shown = unwrapped(&format!("other accounts can write to {}", store.display()));
    let commands: &[&[&str]] = &[
        &[
            "target",
            "add",
            "prod",
            "--renew",
            "--token",
            OTHER,
            "--no-ping",
        ],
        &[
            "target",
            "add",
            "prod",
            "--force",
            "--provider",
            "hetzner-cloud",
            "--token",
            OTHER,
            "--no-ping",
        ],
        &[
            "target",
            "add",
            "dev",
            "--provider",
            "hetzner-cloud",
            "--token",
            OTHER,
            "--no-ping",
        ],
        &["kubeconfig"],
        &["argocd-password"],
        &["target", "ip"],
        &["target", "machine", "--server-type", "cx22"],
        &["node", "status"],
        &["plan"],
    ];
    let mut wrong = Vec::new();
    for args in commands {
        let (code, stdout, stderr) = run(&sp, args, false);
        if code != Some(1)
            || !stderr.contains("apprafter::secrets::store_not_private")
            || !unwrapped(&stderr).contains(&shown)
        {
            wrong.push(format!("{args:?} exited {code:?}:\n{stdout}\n{stderr}"));
        }
        assert!(
            !stdout.contains(OTHER) && !stderr.contains(OTHER),
            "{args:?}"
        );
    }
    others_may_write(&store, false);
    for folder in &closed {
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert!(
        wrong.is_empty(),
        "read before the check:\n{}",
        wrong.join("\n")
    );
}

/// WI-481: doctor on a store others can change reports it and moves nothing into it: the
/// one-shot `<cwd>/.apprafter/state.json` migration every state command runs first is skipped
/// there, so a legacy state file stays where it is and the store is left as it was.
#[test]
fn doctor_moves_no_legacy_state_into_a_store_others_can_change() {
    let project = tempfile::tempdir().unwrap();
    let legacy = project.path().join(".apprafter").join("state.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let body = serde_json::json!({"cluster_name": "legacy"}).to_string();
    std::fs::write(&legacy, &body).unwrap();
    let sp = Spawn::new().current_dir(project.path());
    let add = ["target", "add", "prod", "--provider", "hetzner-cloud"];
    let (code, _, stderr) = run(&sp, &[&add[..], &["--token", TOKEN]].concat(), false);
    assert_eq!(code, Some(0), "{stderr}");
    let targets = sp.store().join("targets");
    others_may_write(&targets, true);
    let before = snapshot(&sp.store());
    let (_, stdout, stderr) = run(&sp, &["doctor", "--target", "prod", "--no-ping"], false);
    others_may_write(&targets, false);
    assert!(
        stdout.contains("Target store folder `targets` can be changed by other accounts"),
        "{stdout}\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&legacy).unwrap(),
        body,
        "the legacy state moved"
    );
    assert_eq!(snapshot(&sp.store()), before, "doctor changed the store");
}
