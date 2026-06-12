// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter open <ui>` — local port-forward, fetch admin
//! credentials, open default browser. Track B.1.79.
//!
//! Currently supports `argocd` only; `backstage` / `grafana`
//! / `hubble` deferred to later phases (those UIs aren't
//! tier-1 resident yet).
//!
//! Generic port-forward + browser-open helpers live in
//! `commands::port_forward`; this module just composes them
//! for the well-known Argo CD target. The sibling
//! `commands::app_open` does the same for user apps (Track
//! B.1.79b).

use cli_core::Result;

use crate::commands::argocd_password;
use crate::commands::k8s_helpers::ensure_kubeconfig_tempfile;
use crate::commands::port_forward::{open_in_browser, spawn_kubectl_port_forward, wait_ready};

/// Open Argo CD's web UI:
///
///   1. Decrypt the cached kubeconfig.
///   2. Resolve the admin password (cached age-encrypted in
///      state via `apprafter argocd-password`).
///   3. Spawn `kubectl port-forward svc/argocd-server -n argocd
///      8080:443` in the background; wait until the bind
///      message lands on stdout.
///   4. Print URL (with optional `?proj=<filter>` AppProject
///      filter) + username + password; copy password to
///      clipboard when possible; open browser.
///   5. Block on the port-forward child; both die when the
///      operator Ctrl+C's the parent process (kubectl
///      inherits the terminal's SIGINT through the process
///      group, Rust's default).
///
/// `project_filter` controls the URL's `?proj=<name>` query
/// parameter (Argo CD accepts a comma-separated list). `None`
/// drops the filter entirely (renders `apprafter open argocd
/// --all-projects`); `Some("apps,default")` is the CLI default
/// — operators land on their own user apps (`apps`) plus the
/// platform root Application (which lives in `default`), while
/// the platform component apps (cilium, operator, cert-manager,
/// …, in the `platform` project) stay hidden from the default
/// view.
pub fn argocd(project_filter: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // Resolve password through the existing cached path. This
    // also seeds the encrypted cache on first run.
    let password =
        argocd_password::compute_argocd_password(&cli_providers::k8s::KubectlCli, kc.path())?;

    let local_port = 8080;
    let mut child =
        spawn_kubectl_port_forward("svc/argocd-server", "argocd", local_port, 443, kc.path())?;

    wait_ready(&mut child)?;

    let url = build_argocd_url(local_port, project_filter);
    let clipboard_status = copy_password_to_clipboard(&password);

    println!();
    println!("Opening Argo CD UI...");
    println!("  URL:       {url}");
    println!("  Username:  admin");
    print!("  Password:  {password}");
    match clipboard_status {
        ClipboardStatus::Copied => println!("  (copied to clipboard)"),
        ClipboardStatus::Failed => println!("  (clipboard unavailable — copy manually)"),
    }
    println!();

    let browser_status = open_in_browser(&url);
    match browser_status {
        Ok(()) => println!("✓ Browser opened"),
        Err(_) => println!("ℹ Browser open failed — paste the URL into your browser"),
    }
    println!("ℹ Press Ctrl+C to stop port-forward");

    // Block until the port-forward child exits — which happens
    // when the operator Ctrl+C's this process (kubectl receives
    // SIGINT via the process group and tears down cleanly).
    let _ = child.wait();
    Ok(())
}

/// Build the Argo CD UI URL, optionally narrowing to a specific
/// AppProject via Argo CD's `?proj=<name>` filter.
///
/// Pure fn (no IO, no globals) so tests can exhaustively
/// cover the default / explicit / drop-filter shapes.
fn build_argocd_url(local_port: u16, project_filter: Option<&str>) -> String {
    let base = format!("https://localhost:{local_port}");
    match project_filter {
        Some(proj) if !proj.is_empty() => format!("{base}/applications?proj={proj}"),
        _ => base,
    }
}

/// Outcome of the optional clipboard copy step. Distinct
/// variants so the printed banner can distinguish "we copied
/// for you" from "we tried and failed" — the second case warns
/// the operator that they need to manually copy from terminal
/// before the buffer flushes.
#[derive(Debug, PartialEq, Eq)]
enum ClipboardStatus {
    Copied,
    Failed,
}

/// Copy `password` to the system clipboard via `arboard`.
/// Fail-quiet — an environment without a clipboard daemon
/// (headless servers, sandboxed shells, fresh SSH session
/// without X11 forwarding) returns `Failed` and the caller
/// surfaces a hint, without raising an error through the
/// `?` chain.
fn copy_password_to_clipboard(password: &str) -> ClipboardStatus {
    match arboard::Clipboard::new() {
        Ok(mut cb) => match cb.set_text(password.to_string()) {
            Ok(()) => ClipboardStatus::Copied,
            Err(_) => ClipboardStatus::Failed,
        },
        Err(_) => ClipboardStatus::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_argocd_url_defaults_no_filter() {
        // Without a project filter — bare base URL, no query
        // params. Matches the --all-projects code path.
        assert_eq!(build_argocd_url(8080, None), "https://localhost:8080");
    }

    #[test]
    fn build_argocd_url_appends_proj_filter() {
        // CLI default — `apps,default` shows the user's apps plus
        // the platform root Application (in `default`), passed
        // through verbatim as a comma-separated `?proj=` list.
        // Regression: ensure the path is
        // `/applications?proj=<filter>` exactly (Argo CD's
        // documented filter URL shape).
        assert_eq!(
            build_argocd_url(8080, Some("apps,default")),
            "https://localhost:8080/applications?proj=apps,default"
        );
        assert_eq!(
            build_argocd_url(8080, Some("apps")),
            "https://localhost:8080/applications?proj=apps"
        );
        assert_eq!(
            build_argocd_url(9999, Some("platform")),
            "https://localhost:9999/applications?proj=platform"
        );
    }

    #[test]
    fn build_argocd_url_treats_empty_filter_as_no_filter() {
        // Defensive: an empty string filter (e.g. caller
        // passed `Some("")`) renders as if --all-projects
        // was set — a bare `?proj=` would mean "filter to
        // a project with an empty name", which is nonsense.
        assert_eq!(build_argocd_url(8080, Some("")), "https://localhost:8080");
    }
}
