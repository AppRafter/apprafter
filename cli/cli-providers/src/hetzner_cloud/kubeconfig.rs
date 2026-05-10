// SPDX-License-Identifier: FSL-1.1-MIT
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
