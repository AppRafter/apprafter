// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `--help` must not reach a cluster.
//!
//! Before any command, the binary may run two checks that reach outside this
//! machine: the newer-release notice (api.github.com) and the node-disk
//! banner, a `kubectl get platformstack` against the ambient kubeconfig. They
//! used to run before clap had parsed the arguments, so `apprafter --help`
//! read the PlatformStack of whatever cluster the current kubeconfig context
//! named. These tests run the shipped binary the way a person does and watch
//! both checks from outside it:
//!
//! * `kubectl` on the child's `PATH` is a stand-in that writes its arguments
//!   to a log and fails, so a run of the node-disk probe leaves a line and
//!   nothing reaches a real cluster (`KUBECONFIG` points nowhere as well);
//! * the version check's cache is seeded, fresh, with a release far above
//!   this build, so a run of that check prints its notice without the
//!   network.
//!
//! The same sandbox with a command that does reach a cluster must show BOTH
//! traces — which is what proves the watch sees a check when one runs, and
//! keeps the "nothing ran" assertions from passing on a harness that could
//! not have seen anything.
//!
//! `cli/.cargo/config.toml` sets `APPRAFTER_SKIP_STARTUP_CHECKS` for
//! everything cargo runs; the child's environment is cleared, so it runs with
//! the checks on.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use assert_cmd::Command;
use tempfile::TempDir;

/// What the seeded version cache names as the latest release: newer than any
/// build this test can run against.
const SEEDED_RELEASE: &str = "999.0.0";

/// The node-disk probe's own arguments, as the stand-in logs them.
const DISK_PROBE: &str = "get platformstack default -n apprafter-system -o json";

/// A throwaway machine: a `PATH` holding only the logging `kubectl`, and a
/// home, cache, configuration and age key of its own.
struct Sandbox {
    dir: TempDir,
}

/// What one invocation left behind.
struct Traces {
    stderr: String,
    /// Every argument list `kubectl` was started with, one per line.
    kubectl: String,
}

impl Traces {
    fn version_check_ran(&self) -> bool {
        self.stderr
            .contains(&format!("apprafter {SEEDED_RELEASE} is available"))
    }

    fn disk_probe_ran(&self) -> bool {
        self.kubectl.lines().any(|l| l.trim() == DISK_PROBE)
    }
}

impl Sandbox {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let kubectl = bin.join("kubectl");
        fs::write(
            &kubectl,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n",
                root.join("kubectl.log").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&kubectl, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let seeded = format!(r#"{{"latest_tag":"{SEEDED_RELEASE}","fetched_at_secs":{now}}}"#);
        // `dirs::cache_dir()`: `$XDG_CACHE_HOME` on Linux, the home's
        // `Library/Caches` on macOS.
        for cache in [
            root.join("cache"),
            root.join("home").join("Library").join("Caches"),
        ] {
            let at = cache.join("apprafter");
            fs::create_dir_all(&at).unwrap();
            fs::write(at.join("version-check.json"), &seeded).unwrap();
        }
        Sandbox { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn run(&self, args: &[&str]) -> Traces {
        let output = Command::cargo_bin("apprafter")
            .unwrap()
            .env_clear()
            .env("PATH", self.path("bin"))
            .env("HOME", self.path("home"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("APPRAFTER_CONFIG_DIR", self.path("apprafter-config"))
            .env("APPRAFTER_AGE_KEY", self.path("age.key"))
            .env("KUBECONFIG", "/nonexistent")
            .current_dir(self.dir.path())
            .args(args)
            .output()
            .unwrap();
        Traces {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            kubectl: fs::read_to_string(self.path("kubectl.log")).unwrap_or_default(),
        }
    }
}

/// The control: a command that reads the cluster runs both checks, and the
/// sandbox sees each of them.
#[test]
fn a_cluster_command_runs_both_startup_checks_and_the_sandbox_sees_them() {
    for args in [&["platform", "status"][..], &["backup", "status"]] {
        let traces = Sandbox::new().run(args);
        assert!(
            traces.version_check_ran(),
            "{args:?}: no newer-release notice on stderr: {}",
            traces.stderr
        );
        assert!(
            traces.disk_probe_ran(),
            "{args:?}: kubectl never ran the node-disk probe; it logged {:?}",
            traces.kubectl
        );
    }
}

/// `--help`, `--version`, `help` and an argument error are answered by clap
/// before any check could run: no `kubectl`, no notice.
#[test]
fn help_version_and_argument_errors_reach_no_cluster() {
    for args in [
        &["--help"][..],
        &["-h"],
        &["--version"],
        &["-V"],
        &["help"],
        &["help", "backup"],
        &["backup", "--help"],
        &["backup", "prune", "-h"],
        &["platform", "status", "--help"],
        &[],
        &["no-such-command"],
        &["backup", "prune", "--no-such-flag"],
    ] {
        let traces = Sandbox::new().run(args);
        assert!(
            traces.kubectl.is_empty(),
            "{args:?} started kubectl: {:?}",
            traces.kubectl
        );
        assert!(
            !traces.version_check_ran(),
            "{args:?} ran the version check: {}",
            traces.stderr
        );
    }
}

/// A command that only reads or writes this machine runs neither check.
#[test]
fn a_command_that_never_leaves_this_machine_reaches_no_cluster() {
    for args in [
        &["completion", "bash"][..],
        &["completion", "zsh"],
        &["target", "list"],
        &["target", "show"],
        &["plan"],
        &["login"],
        &["auth", "status"],
        &["app", "validate", "Application.cue"],
    ] {
        let traces = Sandbox::new().run(args);
        assert!(
            traces.kubectl.is_empty(),
            "{args:?} started kubectl: {:?}",
            traces.kubectl
        );
        assert!(
            !traces.version_check_ran(),
            "{args:?} ran the version check: {}",
            traces.stderr
        );
    }
}
