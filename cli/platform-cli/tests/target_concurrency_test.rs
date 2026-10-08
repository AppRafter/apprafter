// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Concurrent `apprafter target add` runs against one target store.
//!
//! Every add is a read-modify-write of the store: it checks that the name
//! is free, saves the target, and on a fresh store makes it the active
//! one. Without the store lock (`cli_core::StoreLock`) two adds racing on
//! a fresh store can both see no `config.yaml`, both announce themselves
//! as the first target, and the last writer silently wins the active
//! pointer. With the lock exactly one of them is first.
//!
//! The runs are real processes, as the CLI and AppRafter Desktop would
//! be, in a cleared environment like `golden.rs`'s, with no network: the
//! adds are `--no-ping`, and every Hetzner call would go to a closed port.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ADDS: usize = 8;
/// 64 ASCII alphanumerics: the shape `cli_core::target` accepts.
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FIRST_TARGET: &str = "set as active (first target on fresh store)";

fn apprafter(sandbox: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_apprafter"));
    c.env_clear()
        .env("PATH", sandbox.join("bin"))
        .env("HOME", sandbox.join("home"))
        .env("XDG_CONFIG_HOME", sandbox.join("config"))
        .env("XDG_CACHE_HOME", sandbox.join("cache"))
        .env("APPRAFTER_CONFIG_DIR", sandbox.join("store"))
        .env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
        .env("KUBECONFIG", sandbox.join("no-kubeconfig"))
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .current_dir(sandbox.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(args);
    // Windows cannot start a process without its system root.
    if let Some(root) = std::env::var_os("SYSTEMROOT") {
        c.env("SYSTEMROOT", root);
    }
    c
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn assert_success(what: &str, out: &Output) {
    assert!(
        out.status.success(),
        "{what} failed (exit {:?}):\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn concurrent_adds_on_a_fresh_store_keep_every_target_and_one_first() {
    let dir = tempfile::tempdir().unwrap();
    let sandbox: PathBuf = dir.path().to_path_buf();
    for sub in ["home", "config", "cache", "bin"] {
        std::fs::create_dir_all(sandbox.join(sub)).unwrap();
    }
    let names: Vec<String> = (0..ADDS).map(|i| format!("racer{i}")).collect();

    // Spawn every add before waiting on any, so they overlap.
    let children: Vec<_> = names
        .iter()
        .map(|name| {
            apprafter(
                &sandbox,
                &[
                    "target",
                    "add",
                    name,
                    "--provider",
                    "hetzner-cloud",
                    "--token",
                    TOKEN,
                    "--no-ping",
                    "--no-interactive",
                ],
            )
            .spawn()
            .expect("spawn apprafter")
        })
        .collect();
    let outputs: Vec<Output> = children
        .into_iter()
        .map(|c| c.wait_with_output().expect("wait for apprafter"))
        .collect();

    let mut first = Vec::new();
    for (name, out) in names.iter().zip(&outputs) {
        assert_success(&format!("`target add {name}`"), out);
        if stdout(out).contains(FIRST_TARGET) {
            first.push(name.clone());
        }
    }
    assert_eq!(
        first.len(),
        1,
        "exactly one add may find the store fresh; these did: {first:?}"
    );

    let config = std::fs::read_to_string(sandbox.join("store/config.yaml")).unwrap();
    let config: serde_yaml::Value = serde_yaml::from_str(&config).unwrap();
    let active = config["active_target"].as_str().unwrap_or_default();
    assert_eq!(
        active, first[0],
        "the active target is the one that announced itself first"
    );

    let list = apprafter(&sandbox, &["target", "list"]).output().unwrap();
    assert_success("`target list`", &list);
    let list = stdout(&list);
    for name in &names {
        assert!(
            list.lines()
                .any(|l| l.split_whitespace().any(|w| w == name)),
            "`target list` lost {name}:\n{list}"
        );
    }
    assert!(
        list.contains(&format!("{ADDS} targets configured. Active: '{active}'.")),
        "{list}"
    );
}
