// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Kubeconfig retrieval helpers for the Hetzner provider.
//!
//! Two concerns live here:
//! 1. `rewrite_server_url` — pure substitution that replaces the
//!    loopback URL k3s writes in /etc/rancher/k3s/k3s.yaml with
//!    the server's public IPv4, so the file is usable from outside
//!    the VM.
//! 2. `KubeconfigFetcher` — trait + `SshKubeconfigFetcher` impl
//!    that shells out to the system `ssh` binary. The trait gives
//!    callers a seam they can stub out in tests; the SSH impl is
//!    deliberately tiny (~one Command builder) so it doesn't need
//!    its own coverage.

use std::path::{Path, PathBuf};
use std::process::Command;

use cli_core::{CliError, Result};

/// Replace any `server: https://127.0.0.1:<port>` line in a k3s
/// kubeconfig with `server: https://<public_ip>:<port>`. The IPv6
/// loopback (`[::1]`) gets the same treatment for completeness,
/// even though k3s only emits the IPv4 form today.
pub fn rewrite_server_url(yaml: &str, public_ip: &str) -> String {
    let mut out: String = yaml
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("server:") {
                let indent_len = line.len() - trimmed.len();
                let indent = &line[..indent_len];
                let rewritten = trimmed
                    .replace("127.0.0.1", public_ip)
                    .replace("[::1]", public_ip);
                format!("{indent}{rewritten}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if yaml.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// One-method seam for "fetch /etc/rancher/k3s/k3s.yaml from a
/// running cluster server". Real impls call `ssh`; test impls
/// return canned strings.
pub trait KubeconfigFetcher {
    fn fetch(&self, host: &str) -> Result<String>;
}

/// Real fetcher: shells out to system `ssh` as `root@<host>`,
/// reads the kubeconfig file, returns its contents.
///
/// Uses a **per-cluster** `known_hosts` file (typically
/// `<cwd>/.apprafter/known_hosts` from
/// `cli_state::StatePaths::known_hosts_file()`) instead of the
/// user's `~/.ssh/known_hosts`. Two consequences:
///
/// 1. After `destroy --yes` clears the cluster's state directory,
///    the per-cluster known_hosts is gone with it. Next `apply`
///    against a fresh server (Hetzner readily reuses public IPs)
///    starts with an empty file → `StrictHostKeyChecking=accept-new`
///    silently writes the new host key, no `Host key verification
///    failed` interrupt and no manual `ssh-keygen -R`.
/// 2. The user's `~/.ssh/known_hosts` is never touched by the CLI,
///    so it continues to defend against MITM exactly as the user
///    configured it for their other SSH targets.
pub struct SshKubeconfigFetcher {
    pub identity_path: PathBuf,
    pub known_hosts_path: PathBuf,
}

impl SshKubeconfigFetcher {
    pub fn new<I: Into<PathBuf>, K: Into<PathBuf>>(identity_path: I, known_hosts_path: K) -> Self {
        Self {
            identity_path: identity_path.into(),
            known_hosts_path: known_hosts_path.into(),
        }
    }

    fn build_command(&self, host: &str) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.arg("-o")
            .arg("StrictHostKeyChecking=accept-new")
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                self.known_hosts_path.display()
            ))
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            // ConnectTimeout=5: when cloud-init is still bringing
            // sshd up, the port is closed and the kernel's default
            // TCP connect timeout is ~30 seconds. That makes the
            // first kubeconfig-poll attempt block for ~30 s before
            // returning `Connection refused`, which is the
            // dominant reason Phase 2 of `bootstrap-all` stabilises
            // around 60 s on Hetzner cpx22 + Ubuntu 24.04. Capping
            // ConnectTimeout at 5 s lets the retry loop's
            // 10-second sleep do the waiting instead — typical
            // Phase 2 drops from ~60 s to ~20–30 s, attempt
            // counter ticks up evenly, and the operator sees the
            // spinner make progress within the first 5 seconds
            // instead of "frozen" for half a minute.
            .arg("-o")
            .arg("ConnectTimeout=5")
            .arg("-i")
            .arg(&self.identity_path)
            .arg(format!("root@{host}"))
            .arg("cat")
            .arg("/etc/rancher/k3s/k3s.yaml");
        cmd
    }
}

impl KubeconfigFetcher for SshKubeconfigFetcher {
    fn fetch(&self, host: &str) -> Result<String> {
        let output = self.build_command(host).output().map_err(|e| {
            CliError::Other(format!("failed to spawn ssh (is the binary in PATH?): {e}"))
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(CliError::Other(format!(
                "ssh root@{host} cat /etc/rancher/k3s/k3s.yaml failed (exit {:?}); \
                 wait ~3-5 minutes after `apply` for cloud-init to finish, \
                 then retry. stderr: {stderr}",
                output.status.code()
            )));
        }
        String::from_utf8(output.stdout)
            .map_err(|e| CliError::Other(format!("kubeconfig is not utf-8: {e}")))
    }
}

/// Runs an arbitrary shell command on a cluster node over SSH as
/// `root@<host>`, using the same connection posture as
/// [`SshKubeconfigFetcher`] (per-cluster `known_hosts`,
/// `accept-new`, `BatchMode`, `IdentitiesOnly`, `ConnectTimeout=5`).
///
/// This is the shared SSH seam for node-level retrofits (e.g.
/// `apprafter node reserve-headroom`) — it reuses the exact same
/// identity + known-hosts resolution as the kubeconfig fetch so the
/// two never drift on host-key handling.
pub struct SshCommandRunner {
    pub identity_path: PathBuf,
    pub known_hosts_path: PathBuf,
}

impl SshCommandRunner {
    pub fn new<I: Into<PathBuf>, K: Into<PathBuf>>(identity_path: I, known_hosts_path: K) -> Self {
        Self {
            identity_path: identity_path.into(),
            known_hosts_path: known_hosts_path.into(),
        }
    }

    /// Builds the `ssh` invocation that runs `remote_command` on
    /// `host`. `remote_command` is passed as a single argv slot, so
    /// the remote login shell (`sh -c`) parses it — embed a full
    /// script here (heredocs, `&&` chains) and it runs verbatim.
    pub fn build_command(&self, host: &str, remote_command: &str) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.arg("-o")
            .arg("StrictHostKeyChecking=accept-new")
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                self.known_hosts_path.display()
            ))
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg("-o")
            .arg("ConnectTimeout=5")
            .arg("-i")
            .arg(&self.identity_path)
            .arg(format!("root@{host}"))
            .arg(remote_command);
        cmd
    }

    /// Runs `remote_command` on `host`, returning captured stdout on
    /// success or a `CliError` carrying the exit code + stderr.
    pub fn run(&self, host: &str, remote_command: &str) -> Result<String> {
        let output = self
            .build_command(host, remote_command)
            .output()
            .map_err(|e| {
                CliError::Other(format!("failed to spawn ssh (is the binary in PATH?): {e}"))
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(CliError::Other(format!(
                "ssh root@{host} command failed (exit {:?}). stderr: {stderr}",
                output.status.code()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Path to the SSH private key used by the fetcher. Resolves
/// `APPRAFTER_SSH_PRIVATE_KEY` first, then falls back to
/// `$HOME/.ssh/id_ed25519`.
pub fn default_ssh_identity_path() -> PathBuf {
    if let Ok(p) = std::env::var("APPRAFTER_SSH_PRIVATE_KEY") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("/"));
    Path::new(&home).join(".ssh").join("id_ed25519")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_server_url_replaces_loopback_v4() {
        let yaml = "apiVersion: v1\n\
clusters:\n\
- cluster:\n\
    server: https://127.0.0.1:6443\n\
  name: default\n";
        let out = rewrite_server_url(yaml, "203.0.113.10");
        assert!(out.contains("server: https://203.0.113.10:6443"), "{out}");
        assert!(!out.contains("127.0.0.1"));
    }

    #[test]
    fn rewrite_server_url_replaces_loopback_v6_form() {
        let yaml = "    server: https://[::1]:6443\n";
        let out = rewrite_server_url(yaml, "203.0.113.10");
        assert!(out.contains("server: https://203.0.113.10:6443"));
    }

    #[test]
    fn rewrite_server_url_preserves_indentation() {
        let yaml = "    server: https://127.0.0.1:6443";
        let out = rewrite_server_url(yaml, "1.2.3.4");
        assert!(out.starts_with("    server:"), "{out}");
    }

    #[test]
    fn rewrite_server_url_leaves_other_lines_untouched() {
        let yaml = "apiVersion: v1\nuser:\n  client-certificate-data: 127.0.0.1-fake\n";
        let out = rewrite_server_url(yaml, "1.2.3.4");
        assert!(
            out.contains("client-certificate-data: 127.0.0.1-fake"),
            "{out}"
        );
    }

    #[test]
    fn ssh_fetcher_builds_expected_argv() {
        let f = SshKubeconfigFetcher::new("/tmp/key", "/tmp/.apprafter/known_hosts");
        let cmd = f.build_command("198.51.100.5");
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        let argv_str: Vec<String> = argv
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            argv_str.iter().any(|a| a == "BatchMode=yes"),
            "{argv_str:?}"
        );
        assert!(argv_str.iter().any(|a| a == "/tmp/key"), "{argv_str:?}");
        assert!(
            argv_str.iter().any(|a| a == "root@198.51.100.5"),
            "{argv_str:?}"
        );
        assert!(
            argv_str
                .iter()
                .any(|a| a.contains("/etc/rancher/k3s/k3s.yaml")),
            "{argv_str:?}"
        );
    }

    #[test]
    fn ssh_fetcher_caps_connect_timeout_at_five_seconds() {
        // Pins the v0.1.91 fix: without ConnectTimeout=5 the
        // first poll attempt after `apply` blocks ~30s on the
        // kernel's TCP connect retry while cloud-init is still
        // bringing sshd up. 5s lets the loop's 10s sleep absorb
        // the wait instead.
        let f = SshKubeconfigFetcher::new("/tmp/key", "/tmp/.apprafter/known_hosts");
        let cmd = f.build_command("198.51.100.5");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            argv.iter().any(|a| a == "ConnectTimeout=5"),
            "ssh must cap the TCP connect timeout at 5s so the kubeconfig poll loop doesn't block on the kernel's default ~30s retry while cloud-init is still bringing sshd up.\n{argv:?}"
        );
    }

    #[test]
    fn ssh_fetcher_uses_per_cluster_known_hosts_file() {
        let f = SshKubeconfigFetcher::new("/tmp/key", "/tmp/.apprafter/known_hosts");
        let cmd = f.build_command("198.51.100.5");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            argv.iter()
                .any(|a| a == "UserKnownHostsFile=/tmp/.apprafter/known_hosts"),
            "ssh must point at the per-cluster known_hosts so destroy+apply on a recycled Hetzner IP doesn't trip `Host key verification failed`.\n{argv:?}"
        );
        assert!(
            argv.iter().any(|a| a == "StrictHostKeyChecking=accept-new"),
            "accept-new + per-cluster file: silent on first contact, blocks on key change → still defends against unexpected key swap mid-cluster.\n{argv:?}"
        );
    }

    #[test]
    fn ssh_command_runner_builds_expected_argv() {
        // The retrofit SSH seam (Task 11) must carry the SAME
        // connection posture as the kubeconfig fetch (per-cluster
        // known_hosts, accept-new, BatchMode, IdentitiesOnly,
        // ConnectTimeout=5) so the two never drift on host-key
        // handling — and pass the remote command as one argv slot.
        let r = SshCommandRunner::new("/tmp/key", "/tmp/.apprafter/known_hosts");
        let cmd = r.build_command("198.51.100.5", "systemctl restart k3s");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(argv.iter().any(|a| a == "BatchMode=yes"), "{argv:?}");
        assert!(argv.iter().any(|a| a == "IdentitiesOnly=yes"), "{argv:?}");
        assert!(argv.iter().any(|a| a == "ConnectTimeout=5"), "{argv:?}");
        assert!(
            argv.iter()
                .any(|a| a == "UserKnownHostsFile=/tmp/.apprafter/known_hosts"),
            "{argv:?}"
        );
        assert!(
            argv.iter().any(|a| a == "StrictHostKeyChecking=accept-new"),
            "{argv:?}"
        );
        assert!(argv.iter().any(|a| a == "/tmp/key"), "{argv:?}");
        assert!(argv.iter().any(|a| a == "root@198.51.100.5"), "{argv:?}");
        // Remote command is one argv slot (the login shell parses it).
        assert_eq!(
            argv.last().map(String::as_str),
            Some("systemctl restart k3s"),
            "{argv:?}"
        );
    }

    #[test]
    fn default_ssh_identity_path_honours_env_override() {
        std::env::set_var("APPRAFTER_SSH_PRIVATE_KEY", "/tmp/custom-key");
        assert_eq!(
            default_ssh_identity_path(),
            PathBuf::from("/tmp/custom-key")
        );
        std::env::remove_var("APPRAFTER_SSH_PRIVATE_KEY");
        let p = default_ssh_identity_path();
        assert!(p.ends_with(".ssh/id_ed25519"), "{p:?}");
    }
}
