// SPDX-License-Identifier: FSL-1.1-MIT
//! `apprafter bootstrap-all` — convenience wrapper that chains the
//! three subcommands a fresh cluster needs (`apply` →
//! kubeconfig-wait → `cluster-bootstrap`) under a single
//! `indicatif`-driven progress UX. Each phase still has its own
//! subcommand for re-runs / partial recovery; this command exists
//! so first-run users don't have to glue the loop themselves.

use std::time::{Duration, Instant};

use cli_core::{CliError, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use tracing::{info, warn};

use crate::commands::{apply, cluster_bootstrap, kubeconfig};

/// Maximum time we wait for k3s + SSH to come up after `apply`
/// before giving up on Phase 2. Hetzner Cloud cloud-init on a
/// `cpx22` consistently finishes in 90–180s on `ubuntu-24.04`;
/// 5 minutes leaves headroom for slow regions / image fetches.
const KUBECONFIG_POLL_TIMEOUT: Duration = Duration::from_secs(300);
/// Sleep between kubeconfig fetch retries. Short enough that the
/// progress bar feels responsive; long enough that we don't hammer
/// SSH with reconnects.
const KUBECONFIG_POLL_INTERVAL: Duration = Duration::from_secs(10);

pub fn run(target_override: Option<&str>, dry_run: bool) -> Result<()> {
    info!(target_override, dry_run, "bootstrap-all invoked");
    let total_start = Instant::now();

    if dry_run {
        print_dry_run_plan(target_override);
        return Ok(());
    }

    let mp = MultiProgress::new();
    let style = ProgressStyle::with_template("{spinner:.cyan} {prefix:<14.bold} {wide_msg}")
        .expect("static template");

    // ── Phase 1/3: apply ────────────────────────────────────────
    let pb1 = mp.add(ProgressBar::new_spinner());
    pb1.set_style(style.clone());
    pb1.set_prefix("[1/3] apply");
    pb1.enable_steady_tick(Duration::from_millis(120));
    pb1.set_message("provisioning Hetzner Cloud resources…");
    let p1_start = Instant::now();
    apply::run(target_override)?;
    pb1.finish_with_message(format!("done in {}", format_elapsed(p1_start.elapsed())));

    // ── Phase 2/3: wait for kubeconfig (k3s + SSH readiness) ──
    let pb2 = mp.add(ProgressBar::new_spinner());
    pb2.set_style(style.clone());
    pb2.set_prefix("[2/3] kubeconfig");
    pb2.enable_steady_tick(Duration::from_millis(120));
    pb2.set_message("waiting for k3s SSH to become reachable…");
    let p2_start = Instant::now();
    wait_for_kubeconfig(target_override, &pb2)?;
    pb2.finish_with_message(format!("ready in {}", format_elapsed(p2_start.elapsed())));

    // ── Phase 3/3: cluster-bootstrap ───────────────────────────
    let pb3 = mp.add(ProgressBar::new_spinner());
    pb3.set_style(style);
    pb3.set_prefix("[3/3] bootstrap");
    pb3.enable_steady_tick(Duration::from_millis(120));
    pb3.set_message("installing Cilium + Argo CD + cert-manager + operator…");
    let p3_start = Instant::now();
    cluster_bootstrap::run()?;
    pb3.finish_with_message(format!("done in {}", format_elapsed(p3_start.elapsed())));

    println!(
        "bootstrap-all complete in {}",
        format_elapsed(total_start.elapsed())
    );
    Ok(())
}

fn print_dry_run_plan(target_override: Option<&str>) {
    let target_label = target_override
        .map(|t| format!("--target {t}"))
        .unwrap_or_else(|| "<active target>".to_string());
    println!("bootstrap-all — DRY RUN (no provider or cluster mutation)");
    println!("  target resolution: {target_label}");
    println!("  [1/3] apply              — apprafter apply {target_label}");
    println!(
        "  [2/3] kubeconfig (poll)  — apprafter kubeconfig --refresh {target_label} (≤{}s timeout, {}s interval)",
        KUBECONFIG_POLL_TIMEOUT.as_secs(),
        KUBECONFIG_POLL_INTERVAL.as_secs(),
    );
    println!("  [3/3] cluster-bootstrap  — apprafter cluster-bootstrap");
}

/// Poll the SSH-backed kubeconfig fetcher until it succeeds or the
/// poll budget runs out. The progress bar message is updated each
/// attempt so the operator can see WHY we're still spinning (the
/// last error is typically `kex_exchange_identification` while k3s
/// systemd unit is still coming up). Returns `Ok(())` on the first
/// success — the YAML itself is cached in state by `fetch_and_cache`,
/// so no further plumbing is needed for Phase 3.
fn wait_for_kubeconfig(target_override: Option<&str>, pb: &ProgressBar) -> Result<()> {
    let deadline = Instant::now() + KUBECONFIG_POLL_TIMEOUT;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        pb.set_message(format!(
            "attempt {attempt} — fetching /etc/rancher/k3s/k3s.yaml over SSH…"
        ));
        let err = match kubeconfig::fetch_and_cache(true, target_override) {
            Ok(_) => return Ok(()),
            Err(e) => format!("{e}"),
        };
        warn!(attempt, error = %err, "kubeconfig fetch failed; retrying");
        pb.set_message(format!(
            "attempt {attempt} — k3s not ready yet ({}); next retry in {}s",
            short_error(&err),
            KUBECONFIG_POLL_INTERVAL.as_secs(),
        ));
        if Instant::now() >= deadline {
            return Err(timeout_error(attempt, &err));
        }
        std::thread::sleep(KUBECONFIG_POLL_INTERVAL);
        if Instant::now() >= deadline {
            return Err(timeout_error(attempt, &err));
        }
    }
}

fn timeout_error(attempt: u32, last_err: &str) -> CliError {
    CliError::Other(format!(
        "kubeconfig did not become reachable within {}s ({attempt} attempts); last error: {last_err}. Run `apprafter kubeconfig --refresh` once the cluster reports ready.",
        KUBECONFIG_POLL_TIMEOUT.as_secs(),
    ))
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Trim a multi-line error chain down to the first line so the
/// progress bar stays single-line. Kept tiny so a too-large stderr
/// from `ssh` doesn't shove the spinner glyph off-screen.
fn short_error(msg: &str) -> String {
    let first = msg.lines().next().unwrap_or(msg);
    if first.len() > 80 {
        format!("{}…", &first[..79])
    } else {
        first.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_elapsed_uses_seconds_under_one_minute() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(45)), "45s");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn format_elapsed_switches_to_minutes_at_sixty_seconds() {
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m00s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m05s");
        assert_eq!(format_elapsed(Duration::from_secs(3601)), "60m01s");
    }

    #[test]
    fn short_error_keeps_first_line_only() {
        let multi = "ssh: connect to host 1.2.3.4 port 22: Connection refused\nfollow-up line";
        assert_eq!(
            short_error(multi),
            "ssh: connect to host 1.2.3.4 port 22: Connection refused"
        );
    }

    #[test]
    fn short_error_truncates_long_first_line_with_ellipsis() {
        let long = "x".repeat(200);
        let out = short_error(&long);
        assert!(out.ends_with('…'));
        // 79 'x' chars + the ellipsis byte sequence (3 bytes in UTF-8).
        assert_eq!(out.chars().count(), 80);
    }
}
