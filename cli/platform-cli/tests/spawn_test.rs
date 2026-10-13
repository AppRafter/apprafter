// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The spawn point's own guarantees (D.4a T1–T3; safety-net mutants 5–9): the child sees exactly
//! the sandbox's environment, a store a test adds joins the sandbox, the CLI and the shim run in
//! it, the child has no controlling terminal and no console unless the test gives it one, and a
//! terminal it is given is its controlling terminal. Each forced condition is set up by the test
//! and checked first; a test that cannot set it up fails, it never skips.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::spawn::{Spawn, CLOSED_PORT_URL, SANDBOX_ENV};

/// 64 ASCII alphanumerics: the shape `cli_core::target` accepts.
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Set by an outer test on its re-run of this binary. An inner test checks it first, so an
/// `--ignored` run of the whole suite (as one might start to reach the kind smoke) fails there
/// naming the outer test, instead of on a missing terminal or console. Never a skip.
const INNER_ENV: &str = "APPRAFTER_SPAWN_INNER";

/// The child's environment as `name → value`, printed by a program the spawn point starts under
/// its own rules: `/usr/bin/env` on Unix, the tool stand-in's `__env` mode on Windows (names
/// upper-cased there: Windows compares them without case).
fn child_env(sp: &Spawn) -> BTreeMap<String, String> {
    #[cfg(unix)]
    let out = sp
        .probe_program(Path::new("/usr/bin/env"), &[])
        .output()
        .expect("run /usr/bin/env");
    #[cfg(windows)]
    let out = sp
        .probe_program(Path::new(common::stand_in::stand_in_binary()), &["__env"])
        .output()
        .expect("run the stand-in's __env mode");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("the environment is UTF-8");
    let mut env = BTreeMap::new();
    let mut last: Option<String> = None;
    for line in text.lines() {
        match line.split_once('=') {
            Some((name, value)) => {
                let name = if cfg!(windows) {
                    name.to_ascii_uppercase()
                } else {
                    name.to_string()
                };
                env.insert(name.clone(), value.to_string());
                last = Some(name);
            }
            // A value with a newline spans lines: the line continues the variable before. Only
            // an inherited variable holds one (a nix shell exports some), never the sandbox's,
            // so a leak shows in the key set below instead of failing the parse here.
            None => {
                let name = last
                    .as_ref()
                    .unwrap_or_else(|| panic!("not NAME=value: {line:?}"));
                let value: &mut String = env.get_mut(name).expect("the variable just read");
                value.push('\n');
                value.push_str(line);
            }
        }
    }
    env
}

/// Mutants 5 (no `env_clear`) and 6 (one scratch variable dropped): cargo hands every test
/// process `CARGO_*` / `RUST_*` variables, so a leak always shows in the key set.
#[test]
fn the_child_sees_exactly_the_sandbox_environment() {
    let sp = Spawn::new();
    let env = child_env(&sp);
    let mut want: Vec<String> = [
        "PATH",
        "HOME",
        "APPRAFTER_CONFIG_DIR",
        SANDBOX_ENV,
        "APPRAFTER_SKIP_STARTUP_CHECKS",
        "KUBECONFIG",
        "APPRAFTER_HCLOUD_BASE_URL",
        "TZ",
        "LANG",
        "USER",
    ]
    .map(String::from)
    .into();
    #[cfg(unix)]
    want.extend(
        [
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_RUNTIME_DIR",
            "TMPDIR",
        ]
        .map(String::from),
    );
    #[cfg(windows)]
    {
        want.extend(
            [
                "TMP",
                "TEMP",
                "USERPROFILE",
                "APPDATA",
                "LOCALAPPDATA",
                "APPRAFTER_AGE_KEY",
                "APPRAFTER_SSH_PRIVATE_KEY",
            ]
            .map(String::from),
        );
        want.extend(
            common::spawn::WINDOWS_INHERITED
                .iter()
                .filter(|name| std::env::var_os(name).is_some())
                .map(|name| name.to_string()),
        );
        for name in &mut want {
            *name = name.to_ascii_uppercase();
        }
    }
    want.sort();
    let got: Vec<String> = env.keys().cloned().collect();
    assert_eq!(
        got, want,
        "the child sees the sandbox's variables and nothing else"
    );

    let shown = |p: &Path| p.display().to_string();
    assert_eq!(env["HOME"], shown(&sp.home()));
    assert_eq!(env["APPRAFTER_CONFIG_DIR"], shown(&sp.store()));
    assert_eq!(env[SANDBOX_ENV], shown(sp.root()));
    assert_eq!(env["APPRAFTER_HCLOUD_BASE_URL"], CLOSED_PORT_URL);
    assert_eq!(env["APPRAFTER_SKIP_STARTUP_CHECKS"], "1");
    assert!(
        !Path::new(&env["KUBECONFIG"]).exists(),
        "KUBECONFIG names nothing"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(env["XDG_RUNTIME_DIR"], shown(&sp.path("run")));
        assert_eq!(env["TMPDIR"], shown(&sp.path("tmp")));
        let mode = std::fs::metadata(sp.path("run"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "the runtime directory is its owner's alone");
    }
    #[cfg(windows)]
    {
        assert_eq!(env["APPRAFTER_AGE_KEY"], shown(&sp.age_key()));
        assert!(env["APPRAFTER_SSH_PRIVATE_KEY"].starts_with(&shown(sp.root())));
    }
}

#[test]
fn a_store_a_test_adds_joins_the_sandbox() {
    let store = tempfile::tempdir().unwrap();
    let sp = Spawn::new().with_store(store.path());
    let env = child_env(&sp);
    let roots: Vec<_> = std::env::split_paths(&env[SANDBOX_ENV]).collect();
    assert_eq!(
        roots,
        vec![sp.root().to_path_buf(), store.path().to_path_buf()]
    );
    assert_eq!(
        env["APPRAFTER_CONFIG_DIR"],
        store.path().display().to_string()
    );
}

#[test]
#[should_panic(expected = "must be a temporary directory")]
fn a_store_outside_the_temporary_directories_is_refused() {
    // The checkout is no temporary directory: a test never hands the CLI a real store.
    let _ = Spawn::new().with_store(Path::new(env!("CARGO_MANIFEST_DIR")));
}

#[test]
fn the_cli_and_the_shim_run_in_their_sandbox() {
    let sp = Spawn::new();
    sp.cmd(&[
        "target",
        "add",
        "prod",
        "--provider",
        "hetzner-cloud",
        "--token",
        TOKEN,
        "--region",
        "nbg1",
        "--tier",
        "solo",
        "--no-ping",
        "--no-interactive",
    ])
    .assert()
    .success();
    assert!(
        sp.store().join("targets/prod/config.yaml").is_file(),
        "the target landed in the sandbox's store"
    );
    let shim = Spawn::shim().with_store(&sp.store());
    let out = shim.cmd(&["target", "list"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("prod"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("renamed to `apprafter`"));
}

/// Mutant 7 (no `setsid`): the test re-runs one test of this binary at a fresh pseudo-terminal
/// made its controlling terminal, so the parent has a terminal by construction, not by luck of
/// the machine running the suite; that inner test checks it has one, then spawns through
/// `Spawn`, and the child must not be able to open `/dev/tty`.
#[cfg(unix)]
#[test]
fn the_child_has_no_controlling_terminal() {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;

    let (master, slave) = common::spawn::open_pty();
    let mut rerun = std::process::Command::new(std::env::current_exe().expect("this test binary"));
    rerun
        .args([
            "--exact",
            "inner_a_spawned_child_cannot_open_dev_tty",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(INNER_ENV, "1")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: setsid(2) and ioctl(2) are async-signal-safe and touch no memory of this process;
    // std puts the pty on fd 0 before it runs pre_exec closures.
    unsafe {
        rerun.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = rerun
        .output()
        .expect("re-run this test binary at a terminal of its own");
    drop(master);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the inner test failed or did not run:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
#[ignore = "runs only inside the_child_has_no_controlling_terminal, which gives it a terminal"]
fn inner_a_spawned_child_cannot_open_dev_tty() {
    assert!(
        std::env::var_os(INNER_ENV).is_some(),
        "run only by the_child_has_no_controlling_terminal, which gives this test a terminal of \
         its own: an `--ignored` run reaches it alone and fails here on purpose"
    );
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .expect(
            "this test needs a controlling terminal: run the_child_has_no_controlling_terminal",
        );
    let sp = Spawn::new();
    let out = sp
        .probe_program(Path::new("/bin/sh"), &["-c", "exec 3</dev/tty"])
        .output()
        .expect("run sh");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a spawned child opened /dev/tty: it kept the test's controlling terminal"
    );
    // ENXIO: glibc and musl say "No such device or address", macOS "Device not configured".
    assert!(
        stderr.contains("No such device or address") || stderr.contains("Device not configured"),
        "the open must fail for want of a controlling terminal (ENXIO), not: {stderr}"
    );
}

/// Mutant 8 (no `TIOCSCTTY`): a child given the test's pseudo-terminal can open `/dev/tty`, and
/// what it writes there arrives on the test's master. The child is this test binary, re-run for
/// one inner test that writes to `/dev/tty` — never a shell: bash takes the terminal on its own
/// (when `/dev/tty` will not open it opens `ttyname(0)`, and a session leader that opens a
/// terminal makes it its controlling terminal), so with a shell the mutant would survive.
#[cfg(unix)]
#[test]
fn a_terminal_case_owns_its_pty() {
    use std::io::Read;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let sp = Spawn::new();
    let (master, slave) = common::spawn::open_pty();
    let mut child = sp
        .probe_program_at_terminal(
            &std::env::current_exe().expect("this test binary"),
            &[
                "--exact",
                "inner_a_child_writes_to_its_controlling_terminal",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ],
            slave,
        )
        .env(INNER_ENV, "1")
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("re-run this test binary at the pseudo-terminal");
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&seen);
    let mut reader = std::fs::File::from(master);
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the inner test did not exit within 10 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = loop {
        let text = String::from_utf8_lossy(&seen.lock().unwrap()).into_owned();
        if text.contains("spawn-pty-ok") || Instant::now() >= deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        status.success() && text.contains("spawn-pty-ok"),
        "the child's /dev/tty must be the test's pseudo-terminal (exit {status}): {text:?}"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "runs only inside a_terminal_case_owns_its_pty, which gives it a terminal"]
fn inner_a_child_writes_to_its_controlling_terminal() {
    use std::io::Write;
    assert!(
        std::env::var_os(INNER_ENV).is_some(),
        "run only by a_terminal_case_owns_its_pty, which starts this test at a pseudo-terminal \
         through the spawn point: an `--ignored` run reaches it alone and fails here on purpose"
    );
    // The panic text must not hold the marker the outer test looks for.
    let mut tty = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .expect("open /dev/tty: the spawn point gave this child no controlling terminal");
    writeln!(tty, "spawn-pty-ok").expect("write to /dev/tty");
}

/// Mutant 9 (no `DETACHED_PROCESS`): as the Unix test above, with a console. The test re-runs
/// one test of this binary with a console of its own (`CREATE_NEW_CONSOLE`; a hosted runner's
/// test process may have none), which checks it has one, then spawns the stand-in through
/// `Spawn`: it must find no console.
#[cfg(windows)]
#[test]
fn the_child_has_no_console() {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "inner_a_spawned_child_has_no_console",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(INNER_ENV, "1")
        .creation_flags(CREATE_NEW_CONSOLE)
        .output()
        .expect("re-run this test binary with a console of its own");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the inner test failed or did not run:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(windows)]
#[test]
#[ignore = "runs only inside the_child_has_no_console, which gives it a console"]
fn inner_a_spawned_child_has_no_console() {
    assert!(
        std::env::var_os(INNER_ENV).is_some(),
        "run only by the_child_has_no_console, which gives this test a console of its own: an \
         `--ignored` run reaches it alone and fails here on purpose"
    );
    // SAFETY: GetConsoleCP takes no argument and only reads this process's console.
    let attached = unsafe { windows_sys::Win32::System::Console::GetConsoleCP() } != 0;
    assert!(
        attached,
        "this test needs a console: run the_child_has_no_console"
    );
    let sp = Spawn::new();
    let out = sp
        .probe_program(
            Path::new(common::stand_in::stand_in_binary()),
            &["__console"],
        )
        .output()
        .expect("run the stand-in's __console mode");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "no console",
        "a spawned child inherited the test's console"
    );
}
