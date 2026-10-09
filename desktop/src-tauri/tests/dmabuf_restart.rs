// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The restart that turns WebKitGTK's DMA-BUF renderer off on NVIDIA under Wayland
//! (`env::apply_dmabuf_renderer`), for real: it replaces the process it runs in, so it runs in a
//! child process of its own — this test binary again, made to run only [`restart_probe`], which
//! carries out the app's decision on the facts of NVIDIA's driver under Wayland and this
//! process's environment. The image the restart starts is this binary once more, with the same
//! arguments, so it runs the probe again, which now finds the variable and the restart's mark,
//! decides that the app turned the renderer off, and says so instead of restarting.
#![cfg(target_os = "linux")]

use std::process::Command;

use apprafter_desktop::env::{
    apply_dmabuf_renderer, GraphicsFacts, DMABUF_RENDERER_ENV, DMABUF_RESTARTED_ENV,
};

/// Set only in the child process this test starts: the probe refuses to run without it.
const PROBE: &str = "APPRAFTER_TEST_DMABUF_RESTART_PROBE";

/// The child's half: carry out the decision on NVIDIA's driver under Wayland; once restarted,
/// say what this process is, was given and decided.
#[test]
#[ignore = "run by the_restart_replaces_the_process_with_this_program_and_the_renderer_off"]
fn restart_probe() {
    assert_eq!(
        std::env::var(PROBE).as_deref(),
        Ok("1"),
        "the restart replaces the process it runs in: only its parent test runs it"
    );
    let pid = std::process::id();
    if std::env::var_os(DMABUF_RESTARTED_ENV).is_none() {
        println!("before pid={pid}");
    }
    let facts = GraphicsFacts {
        wayland_display: Some("wayland-0".into()),
        gdk_backend: None,
        nvidia_driver: true,
        ..GraphicsFacts::from_process()
    };
    let decision = apply_dmabuf_renderer(&facts);
    println!(
        "restarted pid={pid} decision={decision:?} renderer={:?} mark={:?} arg={:?}",
        std::env::var(DMABUF_RENDERER_ENV).ok(),
        std::env::var(DMABUF_RESTARTED_ENV).ok(),
        std::env::args().nth(1),
    );
}

#[test]
fn the_restart_replaces_the_process_with_this_program_and_the_renderer_off() {
    // A restart that lost its arguments would run every test again, this one included.
    assert!(
        std::env::var_os(PROBE).is_none(),
        "the restarted image ran more than the probe: it lost its arguments"
    );
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "restart_probe",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, "1")
        .env_remove(DMABUF_RENDERER_ENV)
        .env_remove(DMABUF_RESTARTED_ENV)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let pid = |prefix: &str| -> String {
        stdout
            .lines()
            .find_map(|line| line.split_once(prefix).map(|(_, rest)| rest))
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or_else(|| panic!("no {prefix:?} line in:\n{stdout}\n{stderr}"))
            .to_owned()
    };
    let before = pid("before pid=");
    let restarted = pid("restarted pid=");
    assert_eq!(before, restarted, "replaced, not started anew:\n{stdout}");
    assert!(
        stdout.contains(&format!(
            "restarted pid={restarted} decision=TurnedOff renderer=Some(\"1\") mark=Some(\"1\") \
             arg=Some(\"--exact\")"
        )),
        "{stdout}"
    );
}
