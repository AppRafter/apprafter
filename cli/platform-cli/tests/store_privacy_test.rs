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
