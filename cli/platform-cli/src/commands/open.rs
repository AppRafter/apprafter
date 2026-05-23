// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter open <ui>` — local port-forward, fetch admin
//! credentials, open default browser. Track B.1.79.
//!
//! Currently supports `argocd` only; `backstage` / `grafana`
//! / `hubble` deferred к later phases (those UIs aren't tier-
//! 1 resident yet).

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use cli_core::{CliError, Result};

use crate::commands::argocd_password;
use crate::commands::k8s_helpers::ensure_kubeconfig_tempfile;

/// Open Argo CD's web UI:
///
///   1. Decrypt the cached kubeconfig.
///   2. Resolve the admin password (cached age-encrypted в
///      state via `apprafter argocd-password`).
///   3. Spawn `kubectl port-forward svc/argocd-server -n argocd
///      8080:443` в background; wait until the bind message
///      lands on stdout.
///   4. Print URL + username + password; open browser.
///   5. Block on the port-forward child; both die when the
///      operator Ctrl+C's the parent process (kubectl
///      inherits the terminal's SIGINT через the process
///      group, Rust's default).
pub fn argocd() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // Resolve password through the existing cached path. This
    // also seeds the encrypted cache on first run.
    let password =
        argocd_password::compute_argocd_password(&cli_providers::k8s::KubectlCli, kc.path())?;

    let local_port = 8080;
    let mut child = spawn_port_forward("argocd-server", "argocd", local_port, 443, kc.path())?;

    wait_port_forward_ready(&mut child)?;

    let url = format!("https://localhost:{local_port}");
    println!();
    println!("Opening Argo CD UI...");
    println!("  URL:       {url}");
    println!("  Username:  admin");
    println!("  Password:  {password}");
    println!();
    println!("Press Ctrl+C к stop the port-forward.");

    let _ = open_in_browser(&url);

    // Block until the port-forward child exits — which happens
    // when the operator Ctrl+C's this process (kubectl receives
    // SIGINT via the process group и tears down cleanly).
    let _ = child.wait();
    Ok(())
}

fn spawn_port_forward(
    service: &str,
    namespace: &str,
    local_port: u16,
    remote_port: u16,
    kubeconfig_path: &Path,
) -> Result<Child> {
    let child = Command::new("kubectl")
        .arg("port-forward")
        .arg(format!("svc/{service}"))
        .arg("-n")
        .arg(namespace)
        .arg(format!("{local_port}:{remote_port}"))
        .env("KUBECONFIG", kubeconfig_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CliError::Other(format!("spawn kubectl port-forward: {e}")))?;
    Ok(child)
}

/// `kubectl port-forward` prints `Forwarding from 127.0.0.1:…`
/// after binding the local port. Drain stdout one line at a
/// time until either that line appears OR the process exits
/// early (in which case we propagate как error).
fn wait_port_forward_ready(child: &mut Child) -> Result<()> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::Other("kubectl port-forward без stdout".into()))?;
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let line = line.map_err(|e| CliError::Other(format!("read port-forward stdout: {e}")))?;
        if line.contains("Forwarding from") {
            return Ok(());
        }
    }
    Err(CliError::Other(
        "kubectl port-forward exited before binding local port".into(),
    ))
}

/// Open `url` в the operator's default browser. Cross-platform
/// shellout — `xdg-open` on Linux, `open` on macOS, `cmd /c
/// start` on Windows. Failures fall through quietly: the URL
/// is already printed к stdout, so the operator can paste
/// manually.
fn open_in_browser(url: &str) -> Result<()> {
    let (program, args): (&str, Vec<String>) = if cfg!(target_os = "macos") {
        ("open", vec![url.to_string()])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/c".into(), "start".into(), url.to_string()])
    } else {
        ("xdg-open", vec![url.to_string()])
    };
    Command::new(program)
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| CliError::Other(format!("spawn browser: {e}")))?;
    Ok(())
}
