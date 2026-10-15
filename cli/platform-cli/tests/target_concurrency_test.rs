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
//! be, through the spawn point (`tests/common/spawn.rs`) like `golden.rs`'s,
//! with no network: the adds are `--no-ping`, and every Hetzner call would
//! go to a closed port.

mod common;

use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::spawn::Spawn;

const ADDS: usize = 8;
/// The most the whole race may take. Each add is a few file operations
/// under the lock; one still running after this is stuck — a lock never
/// released, say — and is killed so the suite fails instead of hanging.
const RACE_BOUND: Duration = Duration::from_secs(90);
/// 64 ASCII alphanumerics: the shape `cli_core::target` accepts.
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FIRST_TARGET: &str = "set as active (first target on fresh store)";

fn apprafter(sp: &Spawn, args: &[&str]) -> Command {
    let mut c = sp.std(args);
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// Wait for `child` until `deadline`, draining its output as it runs; past
/// the deadline it is killed, and the error says so.
fn wait_until(mut child: Child, deadline: Instant) -> Result<Output, String> {
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(Output {
                status,
                stdout: stdout.join().unwrap_or_default(),
                stderr: stderr.join().unwrap_or_default(),
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "still running {}s after the race began, so it was killed; stderr so far:\n{}",
                RACE_BOUND.as_secs(),
                String::from_utf8_lossy(&stderr.join().unwrap_or_default())
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
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
    let sp = Spawn::new();
    // A fresh store, as before: the test never made `store/`, so every add raced to create it.
    // The spawn point makes the store directory; remove it (the fence resolves a missing store
    // through its parent, the sandbox root).
    std::fs::remove_dir(sp.store()).unwrap();
    let names: Vec<String> = (0..ADDS).map(|i| format!("racer{i}")).collect();

    // Spawn every add before waiting on any, so they overlap; each gets a
    // waiter of its own, all bound by one deadline.
    let deadline = Instant::now() + RACE_BOUND;
    let children: Vec<_> = names
        .iter()
        .map(|name| {
            apprafter(
                &sp,
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
    let waiters: Vec<_> = children
        .into_iter()
        .map(|child| thread::spawn(move || wait_until(child, deadline)))
        .collect();
    let mut outputs = Vec::new();
    let mut stuck = Vec::new();
    for (name, waiter) in names.iter().zip(waiters) {
        match waiter.join().expect("a waiter thread") {
            Ok(output) => outputs.push(output),
            Err(why) => stuck.push(format!("`target add {name}`: {why}")),
        }
    }
    assert!(
        stuck.is_empty(),
        "{} of {ADDS} concurrent adds did not finish:\n{}",
        stuck.len(),
        stuck.join("\n")
    );

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

    let config = std::fs::read_to_string(sp.store().join("config.yaml")).unwrap();
    let config: serde_yaml::Value = serde_yaml::from_str(&config).unwrap();
    let active = config["active_target"].as_str().unwrap_or_default();
    assert_eq!(
        active, first[0],
        "the active target is the one that announced itself first"
    );

    let list = apprafter(&sp, &["target", "list"]).output().unwrap();
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
