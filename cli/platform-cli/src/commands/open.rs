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
    let mut ui = KubectlArgocdUi {
        kubeconfig: kc,
        child: None,
    };
    argocd_core(&mut ui, ARGOCD_LOCAL_PORT, project_filter)
}

/// Local port the UI is forwarded to.
const ARGOCD_LOCAL_PORT: u16 = 8080;

/// Last line of the banner — the operator has to know the command does not
/// return on its own.
const STOP_HINT: &str = "ℹ Press Ctrl+C to stop port-forward";

/// Everything [`argocd_core`] needs from the outside world.
///
/// Inverting these lets the composition be unit-tested: that the tunnel is up
/// BEFORE the browser is pointed at it, that the password is on screen even
/// when the clipboard works, and that a failed browser launch still leaves a
/// pasteable URL behind.
pub(crate) trait ArgocdUi {
    /// Resolve the Argo CD admin password (seeds the encrypted cache).
    fn password(&mut self) -> Result<String>;
    /// Spawn the port-forward and block until it reports it is listening.
    fn start_port_forward(&mut self, local_port: u16) -> Result<()>;
    /// Best-effort clipboard copy.
    fn copy_to_clipboard(&mut self, password: &str) -> ClipboardStatus;
    /// Launch the default browser; `false` when that failed.
    fn open_browser(&mut self, url: &str) -> bool;
    /// Emit one line to the operator.
    fn emit(&mut self, line: &str);
    /// Block until the port-forward child exits (operator hits Ctrl+C).
    fn block_until_stopped(&mut self);
}

/// The `apprafter open argocd` composition, free of direct IO.
///
/// Behaviour is identical to the pre-extraction inline body; only the effects
/// are routed through [`ArgocdUi`].
pub(crate) fn argocd_core(
    ui: &mut dyn ArgocdUi,
    local_port: u16,
    project_filter: Option<&str>,
) -> Result<()> {
    // Resolve password through the existing cached path. This
    // also seeds the encrypted cache on first run.
    let password = ui.password()?;

    // The tunnel comes up BEFORE the URL is handed to anyone — a browser
    // pointed at a port nothing is listening on shows a hard connection error
    // that reads like a broken cluster.
    ui.start_port_forward(local_port)?;

    let url = build_argocd_url(local_port, project_filter);
    let clipboard_status = ui.copy_to_clipboard(&password);
    for line in banner_lines(&url, &password, clipboard_status) {
        ui.emit(&line);
    }

    let opened = ui.open_browser(&url);
    ui.emit(browser_line(opened));
    ui.emit(STOP_HINT);

    // Block until the port-forward child exits — which happens
    // when the operator Ctrl+C's this process (kubectl receives
    // SIGINT via the process group and tears down cleanly).
    ui.block_until_stopped();
    Ok(())
}

/// The credentials banner.
///
/// The password is printed even on a successful copy: clipboards do not
/// survive an SSH session, and the operator has no other way to read it back
/// once the screen scrolls.
pub(crate) fn banner_lines(url: &str, password: &str, clipboard: ClipboardStatus) -> Vec<String> {
    let clipboard_note = match clipboard {
        ClipboardStatus::Copied => "  (copied to clipboard)",
        ClipboardStatus::Failed => "  (clipboard unavailable — copy manually)",
    };
    vec![
        String::new(),
        "Opening Argo CD UI...".to_string(),
        format!("  URL:       {url}"),
        "  Username:  admin".to_string(),
        format!("  Password:  {password}{clipboard_note}"),
        String::new(),
    ]
}

/// Outcome line for the browser launch. A failure has to point back at the URL
/// — the tunnel is up, so pasting it by hand still works.
pub(crate) fn browser_line(opened: bool) -> &'static str {
    if opened {
        "✓ Browser opened"
    } else {
        "ℹ Browser open failed — paste the URL into your browser"
    }
}

/// The production [`ArgocdUi`]: a decrypted kubeconfig, a real `kubectl
/// port-forward` child, the system clipboard and the default browser.
struct KubectlArgocdUi {
    kubeconfig: tempfile::NamedTempFile,
    child: Option<std::process::Child>,
}

impl ArgocdUi for KubectlArgocdUi {
    fn password(&mut self) -> Result<String> {
        argocd_password::compute_argocd_password(
            &cli_providers::k8s::KubectlCli,
            self.kubeconfig.path(),
        )
    }

    fn start_port_forward(&mut self, local_port: u16) -> Result<()> {
        let mut child = spawn_kubectl_port_forward(
            "svc/argocd-server",
            "argocd",
            local_port,
            443,
            self.kubeconfig.path(),
        )?;
        wait_ready(&mut child)?;
        self.child = Some(child);
        Ok(())
    }

    fn copy_to_clipboard(&mut self, password: &str) -> ClipboardStatus {
        copy_password_to_clipboard(password)
    }

    fn open_browser(&mut self, url: &str) -> bool {
        open_in_browser(url).is_ok()
    }

    fn emit(&mut self, line: &str) {
        println!("{line}");
    }

    fn block_until_stopped(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.wait();
        }
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardStatus {
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

    // ── banner_lines ─────────────────────────────────────────────────────

    /// The password is on screen even when the copy SUCCEEDED. A clipboard
    /// does not survive an SSH session or a scrollback purge, and there is no
    /// second chance to read it once the banner scrolls away.
    #[test]
    fn the_password_is_printed_whether_or_not_the_clipboard_worked() {
        for status in [ClipboardStatus::Copied, ClipboardStatus::Failed] {
            let text = banner_lines("https://localhost:8080", "s3cr3t", status).join("\n");
            assert!(text.contains("s3cr3t"), "{status:?}: {text}");
            assert!(
                text.contains("https://localhost:8080"),
                "{status:?}: {text}"
            );
            assert!(text.contains("admin"), "{status:?}: {text}");
        }
    }

    /// A failed copy must SAY so — silence reads as "it's on your clipboard"
    /// and the operator pastes whatever was there before.
    #[test]
    fn a_failed_copy_tells_the_operator_to_copy_manually() {
        let failed = banner_lines("u", "pw", ClipboardStatus::Failed).join("\n");
        assert!(failed.contains("clipboard unavailable"), "{failed}");
        assert!(!failed.contains("(copied to clipboard)"), "{failed}");

        let copied = banner_lines("u", "pw", ClipboardStatus::Copied).join("\n");
        assert!(copied.contains("(copied to clipboard)"), "{copied}");
        assert!(!copied.contains("unavailable"), "{copied}");
    }

    /// The clipboard note sits on the SAME line as the password (the original
    /// `print!` + `println!` pair). On its own line it reads as a note about
    /// the next field instead.
    #[test]
    fn the_clipboard_note_rides_the_password_line() {
        let lines = banner_lines("u", "pw", ClipboardStatus::Copied);
        let pw_line = lines
            .iter()
            .find(|l| l.contains("pw"))
            .expect("a password line");
        assert!(pw_line.contains("(copied to clipboard)"), "{pw_line}");
    }

    // ── browser_line ─────────────────────────────────────────────────────

    /// A failed launch must point back at the URL: the tunnel IS up, so
    /// pasting by hand works — the operator just needs to be told.
    #[test]
    fn a_failed_browser_launch_points_back_at_the_url() {
        assert!(browser_line(false).contains("paste the URL"));
        assert!(!browser_line(true).contains("failed"));
    }

    // ── argocd_core (against a recording fake UI) ─────────────────────────

    /// Records every effect in order so tests can assert on the SEQUENCE, not
    /// just the final output.
    #[derive(Default)]
    struct FakeUi {
        password: Option<String>,
        password_error: bool,
        forward_error: bool,
        clipboard: Option<ClipboardStatus>,
        browser_ok: bool,
        /// "password", "forward:<port>", "clipboard:<pw>", "browser:<url>",
        /// "emit:<line>", "block".
        log: Vec<String>,
    }

    impl FakeUi {
        fn steps(&self) -> Vec<&str> {
            self.log
                .iter()
                .map(|l| l.split(':').next().unwrap_or_default())
                .collect()
        }
        fn output(&self) -> String {
            self.log
                .iter()
                .filter_map(|l| l.strip_prefix("emit:"))
                .collect::<Vec<_>>()
                .join("\n")
        }
        fn index_of(&self, step: &str) -> Option<usize> {
            self.steps().iter().position(|s| *s == step)
        }
    }

    impl ArgocdUi for FakeUi {
        fn password(&mut self) -> Result<String> {
            self.log.push("password".to_string());
            if self.password_error {
                return Err(cli_core::CliError::Other("no argocd secret".to_string()));
            }
            Ok(self.password.clone().unwrap_or_else(|| "pw".to_string()))
        }
        fn start_port_forward(&mut self, local_port: u16) -> Result<()> {
            self.log.push(format!("forward:{local_port}"));
            if self.forward_error {
                return Err(cli_core::CliError::Other("port in use".to_string()));
            }
            Ok(())
        }
        fn copy_to_clipboard(&mut self, password: &str) -> ClipboardStatus {
            self.log.push(format!("clipboard:{password}"));
            self.clipboard.unwrap_or(ClipboardStatus::Failed)
        }
        fn open_browser(&mut self, url: &str) -> bool {
            self.log.push(format!("browser:{url}"));
            self.browser_ok
        }
        fn emit(&mut self, line: &str) {
            self.log.push(format!("emit:{line}"));
        }
        fn block_until_stopped(&mut self) {
            self.log.push("block".to_string());
        }
    }

    /// The tunnel must be listening BEFORE anything hands the URL to a browser
    /// — otherwise the operator's first sight of Argo CD is a connection
    /// refused page that reads like a broken cluster.
    #[test]
    fn the_port_forward_comes_up_before_the_browser_is_pointed_at_it() {
        let mut ui = FakeUi {
            browser_ok: true,
            ..FakeUi::default()
        };
        argocd_core(&mut ui, 8080, Some("apps")).unwrap();
        let forward = ui.index_of("forward").expect("the tunnel is started");
        let browser = ui.index_of("browser").expect("the browser is opened");
        assert!(forward < browser, "{:?}", ui.log);
    }

    /// And it must block on the child LAST — returning early would tear the
    /// tunnel down the instant the browser opened.
    #[test]
    fn the_command_blocks_on_the_tunnel_last() {
        let mut ui = FakeUi::default();
        argocd_core(&mut ui, 8080, None).unwrap();
        assert_eq!(ui.steps().last(), Some(&"block"), "{:?}", ui.log);
    }

    /// A password that cannot be resolved must abort BEFORE a tunnel is
    /// spawned — otherwise a stray `kubectl port-forward` is left behind on
    /// every failed run.
    #[test]
    fn a_password_failure_never_spawns_a_tunnel() {
        let mut ui = FakeUi {
            password_error: true,
            ..FakeUi::default()
        };
        ui.log.clear();
        argocd_core(&mut ui, 8080, None).expect_err("an unresolvable password must abort");
        assert_eq!(ui.steps(), vec!["password"], "{:?}", ui.log);
    }

    /// A tunnel that will not come up must abort before the banner — printing
    /// a URL nothing serves sends the operator debugging Argo CD instead of
    /// the port-forward.
    #[test]
    fn a_failed_tunnel_aborts_before_the_banner() {
        let mut ui = FakeUi {
            forward_error: true,
            ..FakeUi::default()
        };
        argocd_core(&mut ui, 8080, None).expect_err("a dead tunnel must abort");
        assert_eq!(ui.steps(), vec!["password", "forward"], "{:?}", ui.log);
        assert!(ui.output().is_empty(), "{:?}", ui.log);
    }

    /// The browser is sent EXACTLY the URL that was printed. Two different
    /// URLs (say, one filtered and one not) is a silent trap: the operator
    /// reads one and lands on the other.
    #[test]
    fn the_browser_gets_the_same_url_that_was_printed() {
        let mut ui = FakeUi {
            browser_ok: true,
            ..FakeUi::default()
        };
        argocd_core(&mut ui, 9090, Some("apps,default")).unwrap();
        let opened = ui
            .log
            .iter()
            .find_map(|l| l.strip_prefix("browser:"))
            .expect("the browser was opened");
        assert_eq!(
            opened,
            "https://localhost:9090/applications?proj=apps,default"
        );
        assert!(ui.output().contains(opened), "{}", ui.output());
    }

    /// The port the tunnel binds is the port the URL points at.
    #[test]
    fn the_forwarded_port_is_the_port_in_the_url() {
        let mut ui = FakeUi::default();
        argocd_core(&mut ui, 9090, None).unwrap();
        assert!(ui.log.contains(&"forward:9090".to_string()), "{:?}", ui.log);
        assert!(
            ui.output().contains("https://localhost:9090"),
            "{}",
            ui.output()
        );
    }

    /// A failed browser launch is NOT fatal: the tunnel stays up, the URL
    /// stays on screen and the command still blocks so the operator can paste
    /// it into a browser by hand.
    #[test]
    fn a_failed_browser_launch_leaves_a_usable_session_behind() {
        let mut ui = FakeUi {
            browser_ok: false,
            ..FakeUi::default()
        };
        argocd_core(&mut ui, 8080, None).expect("a browser failure must not abort");
        let out = ui.output();
        assert!(out.contains("https://localhost:8080"), "{out}");
        assert!(out.contains("paste the URL"), "{out}");
        assert_eq!(ui.steps().last(), Some(&"block"), "{:?}", ui.log);
    }

    /// The clipboard is offered the password that is printed — copying a
    /// different (say, stale) value is worse than not copying at all.
    #[test]
    fn the_clipboard_is_offered_the_password_that_was_printed() {
        let mut ui = FakeUi {
            password: Some("s3cr3t".to_string()),
            clipboard: Some(ClipboardStatus::Copied),
            ..FakeUi::default()
        };
        argocd_core(&mut ui, 8080, None).unwrap();
        assert!(
            ui.log.contains(&"clipboard:s3cr3t".to_string()),
            "{:?}",
            ui.log
        );
        assert!(ui.output().contains("s3cr3t"), "{}", ui.output());
        assert!(
            ui.output().contains("(copied to clipboard)"),
            "{}",
            ui.output()
        );
    }

    /// The operator has to be told the command does not return on its own.
    #[test]
    fn the_banner_ends_with_the_ctrl_c_hint() {
        let mut ui = FakeUi::default();
        argocd_core(&mut ui, 8080, None).unwrap();
        let emitted: Vec<&str> = ui
            .log
            .iter()
            .filter_map(|l| l.strip_prefix("emit:"))
            .collect();
        assert_eq!(emitted.last().copied(), Some(STOP_HINT));
        assert!(STOP_HINT.contains("Ctrl+C"), "{STOP_HINT}");
    }
}
