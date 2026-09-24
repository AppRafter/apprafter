// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter backup prune` with no cluster to read `spec.backup.timeZone`
//! from refuses unless `--timezone` names the zone.
//!
//! The keep policy counts its days, weeks and months in the zone the
//! cluster's schedules run in, and so does the in-cluster prune. The offline
//! form (`--repo`, the three `--keep-*`, `--cluster-uid` and a credential
//! file) used to count in UTC, and interleaved with the cluster's own prune
//! in another zone the two forgot runs each would keep. These tests run the
//! shipped binary with no target and no cluster:
//!
//! * `restic` on the child's `PATH` is a stand-in that logs its arguments
//!   and lists an empty repository, so a prune that runs is seen to run, and
//!   one that refuses is seen to have listed nothing;
//! * `kubectl` is a stand-in that logs and fails, `KUBECONFIG` points
//!   nowhere and the configuration directory is empty, so nothing reaches a
//!   real cluster.
//!
//! The run with `--timezone` is the control: the same sandbox prunes, which
//! is what proves the refusal is about the zone and not about the sandbox.

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use tempfile::TempDir;

/// A `kube-system` UID shaped like the ones Kubernetes stamps.
const UID: &str = "11111111-2222-3333-4444-555555555555";

struct Sandbox {
    dir: TempDir,
}

/// What one invocation did.
struct Run {
    success: bool,
    stdout: String,
    /// stderr with the error renderer's wrapping undone: its `│` gutter
    /// dropped and every run of whitespace one space.
    stderr: String,
    /// Every argument list `restic` was started with, one per line.
    restic: String,
}

impl Sandbox {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let restic_log = root.join("restic.log");
        let kubectl_log = root.join("kubectl.log");
        stub(
            &bin.join("restic"),
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\ncase \" $* \" in *\" snapshots \"*) echo '[]' ;; esac\nexit 0\n",
                restic_log.display()
            ),
        );
        stub(
            &bin.join("kubectl"),
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n",
                kubectl_log.display()
            ),
        );
        fs::write(
            root.join("operator-s3.env"),
            "S3_ACCESS_KEY_ID=AKIDEXAMPLE\nS3_SECRET_ACCESS_KEY=secret\nRESTIC_PASSWORD=pw\n",
        )
        .unwrap();
        Sandbox { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `backup prune` in its offline form, plus `extra`.
    fn prune(&self, extra: &[&str]) -> Run {
        let credentials = self.path("operator-s3.env");
        let mut args = vec![
            "backup",
            "prune",
            "--repo",
            "s3:https://s3.example.invalid/bucket/prefix",
            "--credential-file",
            credentials.to_str().unwrap(),
            "--keep-daily",
            "7",
            "--keep-weekly",
            "4",
            "--keep-monthly",
            "6",
            "--cluster-uid",
            UID,
        ];
        args.extend_from_slice(extra);
        let output = Command::cargo_bin("apprafter")
            .unwrap()
            .env_clear()
            .env("PATH", self.path("bin"))
            .env("HOME", self.path("home"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("APPRAFTER_CONFIG_DIR", self.path("apprafter-config"))
            .env("APPRAFTER_AGE_KEY", self.path("age.key"))
            .env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
            .env("KUBECONFIG", "/nonexistent")
            .current_dir(self.dir.path())
            .args(&args)
            .output()
            .unwrap();
        Run {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr)
                .replace('│', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            restic: fs::read_to_string(self.path("restic.log")).unwrap_or_default(),
        }
    }
}

fn stub(path: &std::path::Path, body: &str) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The control: with the zone named, the offline form prunes, counting in
/// that zone, and the stand-in `restic` saw the listing.
#[test]
fn the_offline_prune_runs_in_the_zone_timezone_names() {
    let run = Sandbox::new().prune(&["--timezone", "Europe/Berlin"]);
    assert!(
        run.success,
        "stdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert!(
        run.stdout
            .contains("(days, weeks and months in Europe/Berlin)"),
        "{}",
        run.stdout
    );
    assert!(
        run.restic.lines().any(|l| l.contains("snapshots")),
        "restic never listed the repository: {:?}",
        run.restic
    );
}

/// Without `--timezone` there is no zone to count in: the command refuses,
/// names the flag and the field it stands for, and lists nothing.
#[test]
fn with_no_cluster_and_no_timezone_the_prune_refuses_before_it_lists() {
    let run = Sandbox::new().prune(&[]);
    assert!(
        !run.success,
        "stdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert!(run.stderr.contains("--timezone <zone>"), "{}", run.stderr);
    assert!(
        run.stderr.contains("spec.backup.timeZone"),
        "{}",
        run.stderr
    );
    assert_eq!(run.restic, "", "restic ran although the prune refused");
    assert!(!run.stdout.contains("Pruned"), "{}", run.stdout);
}

/// A zone this build does not know is refused at the flag, not counted in
/// UTC: the flag is the operator saying which zone.
#[test]
fn an_unknown_timezone_is_refused_before_it_lists() {
    let run = Sandbox::new().prune(&["--timezone", "Europe/Atlantis"]);
    assert!(
        !run.success,
        "stdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert!(
        run.stderr.contains("--timezone 'Europe/Atlantis'"),
        "{}",
        run.stderr
    );
    assert_eq!(run.restic, "", "restic ran although the prune refused");
}
