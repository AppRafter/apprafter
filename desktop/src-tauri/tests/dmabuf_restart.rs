// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The DMA-BUF renderer workaround as the process meets it: the decision on the facts the app
//! gathers from its own environment (`GraphicsFacts::from_process`), and, for real, the restart
//! that turns WebKitGTK's DMA-BUF renderer off on NVIDIA under Wayland
//! (`env::apply_dmabuf_renderer`). Each runs in a child process of its own: this test binary
//! again, made to run one probe, with an environment the parent sets — every name the decision
//! reads removed, then the ones a case names set. Only NVIDIA's driver cannot be set that way,
//! so each probe takes it as loaded.
//!
//! The restart replaces the process it runs in. The image it starts is this binary once more,
//! with the same arguments, so it runs the probe again. That run finds the variable and the
//! restart's mark naming its own process, decides that the app turned the renderer off, and
//! says so instead of restarting.
#![cfg(target_os = "linux")]

use std::process::Command;

use apprafter_desktop::env::{
    apply_dmabuf_renderer, dmabuf_renderer, GraphicsFacts, DMABUF_RENDERER_ENV,
    DMABUF_RESTARTED_ENV,
};

/// Set only in the child processes these tests start: the probes refuse to run without it.
const PROBE: &str = "APPRAFTER_TEST_DMABUF_RESTART_PROBE";

/// Every name the decision reads from the environment.
const READ: [&str; 5] = [
    "WAYLAND_DISPLAY",
    "XDG_SESSION_TYPE",
    "GDK_BACKEND",
    DMABUF_RENDERER_ENV,
    DMABUF_RESTARTED_ENV,
];

/// A probe's half: refuse to run but as a parent test's child.
fn only_as_a_probe() {
    assert_eq!(
        std::env::var(PROBE).as_deref(),
        Ok("1"),
        "a probe runs only as its parent test's child (the restart replaces its process)"
    );
}

/// A parent test's half: a restart that lost its arguments would run every test again, this
/// one included, in the probe's environment.
fn not_a_probe() {
    assert!(
        std::env::var_os(PROBE).is_none(),
        "the restarted image ran more than the probe: it lost its arguments"
    );
}

/// The facts as the app gathers them in this process, with NVIDIA's driver loaded.
fn facts() -> GraphicsFacts {
    GraphicsFacts {
        nvidia_driver: true,
        ..GraphicsFacts::from_process()
    }
}

/// Run the probe `name` in a child process whose environment has, of the names the decision
/// reads, only `set`; its standard output, once it succeeded.
fn probe(name: &str, set: &[(&str, &str)]) -> String {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, "1");
    for read in READ {
        command.env_remove(read);
    }
    command.envs(set.iter().copied());
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{set:?}\n{stdout}\n{stderr}");
    stdout
}

/// The child's half: the decision this process's environment gives.
#[test]
#[ignore = "run by the_decision_reads_each_name_from_the_environment"]
fn decision_probe() {
    only_as_a_probe();
    println!("decision={:?}", dmabuf_renderer(&facts()));
}

/// The decision on the environment as the app reads it, a case for each name: misread, the
/// name would leave the decision as the case before it. A mark this process inherited (here
/// the parent test's own ID, which no child has) is not its restart: with the variable it is a
/// value the user set, and without it the app restarts as it would without the mark.
#[test]
fn the_decision_reads_each_name_from_the_environment() {
    not_a_probe();
    let inherited = std::process::id().to_string();
    let inherited = inherited.as_str();
    let wayland = ("WAYLAND_DISPLAY", "wayland-0");
    let cases: &[(&[(&str, &str)], &str)] = &[
        (&[], "Untouched"),
        (&[wayland], "Restart"),
        (&[("XDG_SESSION_TYPE", "wayland")], "Restart"),
        (&[wayland, ("GDK_BACKEND", "x11")], "Untouched"),
        (&[wayland, (DMABUF_RENDERER_ENV, "0")], "UserSet(\"0\")"),
        (
            &[
                wayland,
                (DMABUF_RENDERER_ENV, "1"),
                (DMABUF_RESTARTED_ENV, inherited),
            ],
            "UserSet(\"1\")",
        ),
        (&[wayland, (DMABUF_RESTARTED_ENV, inherited)], "Restart"),
    ];
    for (set, expected) in cases {
        let stdout = probe("decision_probe", set);
        // libtest starts the line the probe prints on with `test decision_probe ... `.
        let decision = stdout
            .lines()
            .find_map(|line| line.split_once("decision=").map(|(_, decision)| decision));
        assert_eq!(decision, Some(*expected), "{set:?}:\n{stdout}");
    }
}

/// The child's half: carry out the decision; once restarted, say what this process is, was
/// given and decided.
#[test]
#[ignore = "run by the_restart_replaces_the_process_with_this_program_and_the_renderer_off"]
fn restart_probe() {
    only_as_a_probe();
    let pid = std::process::id();
    if std::env::var_os(DMABUF_RESTARTED_ENV).is_none() {
        println!("before pid={pid}");
    }
    let decision = apply_dmabuf_renderer(&facts());
    println!(
        "restarted pid={pid} decision={decision:?} renderer={:?} mark={:?} arg={:?}",
        std::env::var(DMABUF_RENDERER_ENV).ok(),
        std::env::var(DMABUF_RESTARTED_ENV).ok(),
        std::env::args().nth(1),
    );
}

/// On NVIDIA under Wayland the process restarts: the same process (one ID), this program with
/// its arguments, the variable set to `1`, and the mark naming the process.
#[test]
fn the_restart_replaces_the_process_with_this_program_and_the_renderer_off() {
    not_a_probe();
    let stdout = probe("restart_probe", &[("WAYLAND_DISPLAY", "wayland-0")]);
    let pid = |prefix: &str| -> String {
        stdout
            .lines()
            .find_map(|line| line.split_once(prefix).map(|(_, rest)| rest))
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or_else(|| panic!("no {prefix:?} line in:\n{stdout}"))
            .to_owned()
    };
    let before = pid("before pid=");
    let restarted = pid("restarted pid=");
    assert_eq!(before, restarted, "replaced, not started anew:\n{stdout}");
    assert!(
        stdout.contains(&format!(
            "restarted pid={restarted} decision=TurnedOff renderer=Some(\"1\") \
             mark=Some(\"{restarted}\") arg=Some(\"--exact\")"
        )),
        "{stdout}"
    );
}
