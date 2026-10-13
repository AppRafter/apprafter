// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter bootstrap-all` — convenience wrapper that chains the
//! three subcommands a fresh cluster needs (`apply` → wait for
//! k3s to come up on the new node → `cluster-bootstrap`). Each
//! phase still has its own subcommand for re-runs / partial
//! recovery; this command exists so first-run users don't have to
//! glue the loop themselves.
//!
//! UX layout (v0.1.85 rework): phases 1 and 3 invoke heavy
//! subcommands that print verbose helm / kubectl output to
//! stdout, so we don't run a spinner around them — its frames
//! would interleave with the subcommand chatter and persist
//! stale lines. Instead we print one `→ [N/3] phase  msg` line
//! before the work, let the subcommand stream normally, then
//! print one `✓ [N/3] phase  done in Ns` line after. Phase 2
//! (`kubeconfig` poll) is OUR retry loop with no inner
//! subcommand output, so it gets a real `indicatif` spinner —
//! the spinner finishes cleanly via `finish_and_clear()` and is
//! replaced with the static success line on success.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use cli_core::secrets::default_age_key_path;
use cli_core::style;
use cli_core::target::{
    default_config_root, load_active_target_config, resolve_active_target_name, TargetConfig,
    TargetStorePaths,
};
use cli_core::{CliError, ProvisionStop, ProvisionedBy, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tracing::{info, warn};

use crate::commands::state_paths::resolve_state_paths;
use crate::commands::{age_cache, apply, cluster_bootstrap, kubeconfig};

/// Maximum time we wait for k3s + SSH to come up after `apply`
/// before giving up on Phase 2. Hetzner Cloud cloud-init on a
/// `cpx22` consistently finishes in 90–180s on `ubuntu-24.04`;
/// 5 minutes leaves headroom for slow regions / image fetches.
const KUBECONFIG_POLL_TIMEOUT: Duration = Duration::from_secs(300);
/// Sleep between kubeconfig fetch retries. Short enough that the
/// progress bar feels responsive; long enough that we don't hammer
/// SSH with reconnects.
const KUBECONFIG_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// `by` is the command running it, `up` itself or `restore --reprovision`: the one a stop on a
/// lost age key tells the user to run again, before phase 1 or after it (review #6).
pub fn run(
    target_override: Option<&str>,
    dry_run: bool,
    server_type: Option<&str>,
    by: ProvisionedBy,
) -> Result<()> {
    info!(target_override, dry_run, "bootstrap-all invoked");
    let total_start = Instant::now();

    if dry_run {
        print_dry_run_plan(target_override)?;
        return Ok(());
    }

    // D11 / 2.22a. Phase 1/3 creates billable resources; phase 3/3 is the
    // first code to need `helm`. The provider layer already learned this one
    // rung down — it validates the SKU against the live catalogue before any
    // create, because a retired type used to fail mid-apply at step 4 and leak
    // SSH-key + network + firewall state. Same lesson, one level up: check the
    // binaries before spending anything. After the dry-run return, so a plan
    // preview still works on a machine with neither installed.
    cli_core::tools::preflight_tools(
        &[&cli_core::tools::KUBECTL, &cli_core::tools::HELM],
        "apprafter bootstrap-all",
    )?;

    // WI-457. Phase 2/3 caches the new kubeconfig under the age key and never replaces a lost
    // one, so a lost key would stop this run there, after phase 1/3 created a server that
    // bills. Same lesson again: check it before spending anything.
    let resolved = resolve_state_paths(target_override)?;
    age_cache::check_before_provisioning(
        &resolved.store,
        &resolved.target_name,
        &default_age_key_path(),
        by,
    )?;

    // `restore --reprovision` says it is provisioning here, past every check, and not before
    // it calls this: a run the check refuses says nothing was created, so it must not have
    // said it started (WI-457).
    if by == ProvisionedBy::RestoreReprovision {
        println!(
            "→ --reprovision: provisioning a fresh cluster in target '{}' before replay",
            resolved.target_name
        );
    }

    // Phase 1/3 — apply. No spinner: apply itself spends most of
    // its time inside `provider.apply()` calling the Hetzner REST
    // API, which logs through `tracing` to stderr; a spinner here
    // would just fight those tracing lines for the same row.
    println!(
        "{} [1/3] apply        provisioning Hetzner Cloud resources…",
        style::info("→")
    );
    let p1_start = Instant::now();
    apply::run(target_override, server_type)
        .map_err(|e| failed(1, "apply", p1_start.elapsed(), e))?;
    let p1 = p1_start.elapsed();
    println!(
        "{} [1/3] apply        done in {}",
        style::ok("✓"),
        format_elapsed(p1)
    );

    // Phase 2/3 — kubeconfig poll. Our retry loop, no inner
    // subcommand output, so we get a clean indicatif spinner with
    // a live attempt counter + truncated last error.
    let p2 = run_kubeconfig_phase(target_override, &resolved.target_name, by)?;

    // Phase 3/3 — cluster-bootstrap. Same rationale as phase 1:
    // helm/kubectl print release notes, would clobber a spinner.
    //
    // `target_override` is threaded here for the same reason phases 1
    // and 2 take it. It used to be omitted (finding C1): phase 3
    // re-resolved the ACTIVE target and bootstrapped it, so
    // `apprafter restore --reprovision --target X` provisioned X and
    // then ran helm + SSA against whatever target was active — see the
    // `cluster_bootstrap::run` doc comment for the blast radius.
    println!(
        "{} [3/3] bootstrap    installing Cilium + Argo CD + cert-manager + operator…",
        style::info("→")
    );
    let p3_start = Instant::now();
    cluster_bootstrap::run(target_override)
        .map_err(|e| failed(3, "bootstrap", p3_start.elapsed(), e))?;
    let p3 = p3_start.elapsed();
    println!(
        "{} [3/3] bootstrap    done in {}",
        style::ok("✓"),
        format_elapsed(p3)
    );

    println!(
        "bootstrap-all complete in {} (apply {} + k3s-ready {} + bootstrap {})",
        format_elapsed(total_start.elapsed()),
        format_elapsed(p1),
        format_elapsed(p2),
        format_elapsed(p3),
    );
    Ok(())
}

/// Map an inner-subcommand error into a structured one with the
/// phase prefix prepended and the elapsed time annotated. Keeps the
/// timing accountable in failure mode without losing the original
/// error chain.
fn failed(num: u8, name: &str, elapsed: Duration, err: CliError) -> CliError {
    eprintln!(
        "{} [{num}/3] {name:<10} FAILED after {}",
        style::fail("✗"),
        format_elapsed(elapsed)
    );
    err
}

/// Phase 2 for `target` (`target_override` resolved), which `by` provisioned in phase 1.
fn run_kubeconfig_phase(
    target_override: Option<&str>,
    target: &str,
    by: ProvisionedBy,
) -> Result<Duration> {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} [2/3] k3s-ready    {wide_msg}")
            .expect("static template"),
    );
    pb.enable_steady_tick(Duration::from_millis(120));
    pb.set_message("waiting for cloud-init + k3s on the new node…");
    let start = Instant::now();
    match wait_for_kubeconfig(target_override, &pb) {
        Ok(()) => {
            let elapsed = start.elapsed();
            pb.finish_and_clear();
            println!(
                "{} [2/3] k3s-ready    done in {}",
                style::ok("✓"),
                format_elapsed(elapsed)
            );
            Ok(elapsed)
        }
        Err(e) => {
            let elapsed = start.elapsed();
            pb.finish_and_clear();
            Err(failed(
                2,
                "k3s-ready",
                elapsed,
                after_provision(e, target, by),
            ))
        }
    }
}

/// Why phase 2's wait for the node ended without a kubeconfig.
#[derive(Debug)]
enum Unready {
    /// An error waiting does not cure: a lost age key (WI-457).
    Failed(CliError),
    /// The wait ran out, after `attempts` fetches; `last_error` is the last one's.
    TimedOut {
        waited: Duration,
        attempts: u32,
        last_error: String,
    },
}

/// Phase 2's error once `by` has provisioned `target` in phase 1 (the target `run` resolved,
/// which the fetch resolves too). Each says what that leaves (a server that bills, no
/// cluster-bootstrap) and names `by` to run again (review #6):
///
/// - a lost age key, once the key is back or `kubeconfig --refresh` has made a new one. The
///   check before phase 1 refuses a key lost before the run, so this is a key that went
///   missing during it;
/// - a wait that ran out ([`CliError::K3sNotReady`]), once the node is up.
///
/// Any other error is unchanged.
fn after_provision(unready: Unready, target: &str, by: ProvisionedBy) -> CliError {
    match unready {
        Unready::Failed(CliError::AgeKeyMissing { path, .. }) => CliError::AgeKeyMissing {
            path,
            target: target.to_string(),
            stopped: Some(ProvisionStop::After { by }),
        },
        Unready::Failed(other) => other,
        Unready::TimedOut {
            waited,
            attempts,
            last_error,
        } => CliError::K3sNotReady {
            target: target.to_string(),
            by,
            waited_secs: waited.as_secs(),
            attempts,
            last_error,
        },
    }
}

fn print_dry_run_plan(target_override: Option<&str>) -> Result<()> {
    let store_root = default_config_root().ok();
    let store = store_root
        .as_ref()
        .map(|p| TargetStorePaths::for_root(p.clone()));
    let name = store.as_ref().and_then(|s| {
        resolve_active_target_name(s, target_override)
            .ok()
            .flatten()
    });
    let config = store
        .as_ref()
        .and_then(|s| load_active_target_config(s, target_override));

    println!(
        "{} — DRY RUN (no provider or cluster mutation)",
        style::bold("bootstrap-all")
    );
    println!();
    print_target_block(target_override, name.as_deref(), config.as_ref());
    println!();
    println!("{}", style::bold("Phases:"));
    println!(
        "  {} apply              — provision Hetzner Cloud resources",
        style::info("[1/3]")
    );
    println!(
        "                              {}",
        style::dim("(server, network, firewall, SSH key, optional floating IPs)")
    );
    println!(
        "  {} k3s-ready (poll)   — wait for cloud-init + k3s on the new node",
        style::info("[2/3]"),
    );
    println!(
        "                              {}",
        style::dim(&format!(
            "(SCP /etc/rancher/k3s/k3s.yaml every {}s, up to {}s; cache age-encrypted in state)",
            KUBECONFIG_POLL_INTERVAL.as_secs(),
            KUBECONFIG_POLL_TIMEOUT.as_secs(),
        ))
    );
    println!(
        "  {} cluster-bootstrap  — install Cilium + Gateway API CRDs + Application CRD",
        style::info("[3/3]")
    );
    println!("                              + default-deny NetworkPolicy + Argo CD + cert-manager");
    println!("                              + self-signed ClusterIssuer + apprafter-operator");
    println!("                              + admission-webhook");
    println!();
    println!(
        "{}",
        style::dim("Run `apprafter bootstrap-all` (without --dry-run) to execute.")
    );
    Ok(())
}

fn print_target_block(
    target_override: Option<&str>,
    resolved_name: Option<&str>,
    config: Option<&TargetConfig>,
) {
    let label = style::bold("Target:");
    match (target_override, resolved_name) {
        (Some(t), _) => println!(
            "{label} {} {}",
            style::bold(t),
            style::info("(via --target override)")
        ),
        (None, Some(n)) => println!("{label} {} {}", style::bold(n), style::info("(active)")),
        (None, None) => {
            println!(
                "{label} {}",
                style::dim("<none — no active target in store>")
            );
            println!(
                "  {}",
                style::dim("Run `apprafter target add <name> --provider hetzner-cloud …` first.")
            );
            return;
        }
    }
    match config {
        Some(c) => {
            let placeholder_region = style::dim("<unset — apply uses nbg1>");
            let placeholder_tier = style::dim("<unset — apply uses solo>");
            let placeholder_cluster = style::dim("<unset — apply uses platform-1>");
            let placeholder_ssh = style::dim("<unset>");
            let label_provider = style::dim("Provider:   ");
            let label_region = style::dim("Region:     ");
            let label_tier = style::dim("Tier:       ");
            let label_cluster = style::dim("Cluster:    ");
            let label_ssh = style::dim("SSH key:    ");
            println!("  {label_provider} {}", c.provider);
            println!(
                "  {label_region} {}",
                c.region
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| placeholder_region.clone()),
            );
            println!(
                "  {label_tier} {}",
                c.default_tier
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| placeholder_tier.clone()),
            );
            println!(
                "  {label_cluster} {}",
                c.cluster_name
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| placeholder_cluster.clone()),
            );
            println!(
                "  {label_ssh} {}",
                c.ssh_key_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| placeholder_ssh.clone()),
            );
        }
        None => println!(
            "  {}",
            style::dim("(config unreadable — target file missing or malformed)")
        ),
    }
}

/// Poll the SSH-backed kubeconfig fetcher until it succeeds or the
/// poll budget runs out. The progress bar message is updated each
/// attempt so the operator can see WHY we're still spinning (the
/// last error is typically `kex_exchange_identification` while k3s
/// systemd unit is still coming up). Returns `Ok(())` on the first
/// success — the YAML itself is cached in state by `fetch_and_cache`,
/// so no further plumbing is needed for Phase 3.
fn wait_for_kubeconfig(
    target_override: Option<&str>,
    pb: &ProgressBar,
) -> std::result::Result<(), Unready> {
    poll_kubeconfig(
        pb,
        KUBECONFIG_POLL_TIMEOUT,
        KUBECONFIG_POLL_INTERVAL,
        &mut || kubeconfig::fetch_and_cache(true, target_override),
        &|addr| apiserver_listening(addr),
        &SystemClock,
    )
}

/// Where [`poll_kubeconfig`] reads the time and how it waits: [`SystemClock`] in the product; a
/// clock its tests move themselves, so the attempts a wait allows are exact on any runner.
trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration);
}

/// `Instant::now` and `std::thread::sleep`.
struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// [`wait_for_kubeconfig`]'s loop, with the fetch, the apiserver probe, the timings and the
/// clock given, so a test drives it without a node or a real wait.
fn poll_kubeconfig(
    pb: &ProgressBar,
    timeout: Duration,
    interval: Duration,
    fetch: &mut dyn FnMut() -> Result<String>,
    listening: &dyn Fn(&str) -> bool,
    clock: &dyn Clock,
) -> std::result::Result<(), Unready> {
    let deadline = clock.now() + timeout;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        pb.set_message(format!(
            "attempt {attempt} — fetching /etc/rancher/k3s/k3s.yaml over SSH…"
        ));
        // Two gates, not one. k3s writes /etc/rancher/k3s/k3s.yaml BEFORE the
        // apiserver binds :6443, so "the kubeconfig is fetchable over SSH" is
        // NOT "the cluster is reachable". Without the second gate, [3/3]
        // cluster-bootstrap races a slow-booting node and fails
        // "Kubernetes cluster unreachable … :6443 connect: connection refused".
        let err = match fetch() {
            Ok(yaml) => match apiserver_addr(&yaml) {
                Some(addr) if listening(&addr) => return Ok(()),
                Some(addr) => format!("apiserver {addr} not accepting connections yet"),
                None => "fetched kubeconfig has no server URL yet".to_string(),
            },
            // A lost age key does not come back by waiting: its help names
            // `apprafter kubeconfig --refresh`, which asks before it makes a new one (WI-457).
            Err(e @ CliError::AgeKeyMissing { .. }) => return Err(Unready::Failed(e)),
            Err(e) => format!("{e}"),
        };
        warn!(attempt, error = %err, "kubeconfig fetch failed; retrying");
        pb.set_message(format!(
            "attempt {attempt} — k3s not ready yet ({}); next retry in {}s",
            short_error(&err),
            interval.as_secs(),
        ));
        let timed_out = |last_error| Unready::TimedOut {
            waited: timeout,
            attempts: attempt,
            last_error,
        };
        if clock.now() >= deadline {
            return Err(timed_out(err));
        }
        clock.sleep(interval);
        if clock.now() >= deadline {
            return Err(timed_out(err));
        }
    }
}

/// Parse `HOST:PORT` from the kubeconfig's `server: https://HOST:PORT` line.
/// `None` when there is no server line yet / it is unparseable, in which case
/// the caller keeps polling. Pure (unit-tested); locates the `server:` line the
/// same way `cli_providers::…::rewrite_server_url` does.
fn apiserver_addr(kubeconfig_yaml: &str) -> Option<String> {
    let line = kubeconfig_yaml
        .lines()
        .map(str::trim_start)
        .find(|l| l.starts_with("server:"))?;
    let url = line.trim_start_matches("server:").trim();
    let hostport = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
        .trim_end_matches('/');
    if hostport.is_empty() {
        return None;
    }
    // k3s always includes the port (`:6443`); default it defensively when the
    // trailing colon-segment isn't a port number.
    match hostport.rsplit(':').next().map(|p| p.parse::<u16>()) {
        Some(Ok(_)) => Some(hostport.to_string()),
        _ => Some(format!("{hostport}:6443")),
    }
}

/// True when the k3s apiserver is actually accepting TCP connections on
/// `host:port`. k3s writes the kubeconfig BEFORE binding the port, so this is
/// the gate that stops [3/3] cluster-bootstrap racing a slow-booting node. A
/// short connect timeout keeps each poll attempt snappy; network-dependent, so
/// not unit-tested (the SSH fetch it complements isn't either).
fn apiserver_listening(addr: &str) -> bool {
    let Ok(socks) = addr.to_socket_addrs() else {
        return false;
    };
    socks
        .take(4)
        .any(|sa| TcpStream::connect_timeout(&sa, Duration::from_secs(5)).is_ok())
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
    use std::cell::Cell;

    use super::*;

    /// A clock that moves only when [`poll_kubeconfig`] sleeps or a fetch says it took time, so
    /// the attempts a wait allows are counted, never sampled from a runner's speed.
    struct FakeClock {
        start: Instant,
        now: Cell<Instant>,
        sleeps: Cell<u32>,
    }

    impl FakeClock {
        fn new() -> Self {
            let start = Instant::now();
            Self {
                start,
                now: Cell::new(start),
                sleeps: Cell::new(0),
            }
        }

        /// A fetch that takes `d`.
        fn advance(&self, d: Duration) {
            self.now.set(self.now.get() + d);
        }

        fn elapsed(&self) -> Duration {
            self.now.get() - self.start
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.now.get()
        }

        fn sleep(&self, d: Duration) {
            self.sleeps.set(self.sleeps.get() + 1);
            self.advance(d);
        }
    }

    /// WI-457: `up` never replaces a lost age key, and does not wait it out either — the first
    /// attempt ends the phase with the error whose help names `kubeconfig --refresh`.
    #[test]
    fn a_lost_age_key_ends_the_wait_at_once() {
        let mut calls = 0;
        let mut fetch = || {
            calls += 1;
            Err(CliError::AgeKeyMissing {
                path: "/k/age.key".into(),
                target: "prod".into(),
                stopped: None,
            })
        };
        let clock = FakeClock::new();
        let err = poll_kubeconfig(
            &ProgressBar::hidden(),
            Duration::from_millis(200),
            Duration::from_millis(20),
            &mut fetch,
            &|_| true,
            &clock,
        )
        .unwrap_err();
        assert!(
            matches!(err, Unready::Failed(CliError::AgeKeyMissing { .. })),
            "{err:?}"
        );
        assert_eq!(calls, 1);
        assert_eq!(clock.sleeps.get(), 0, "a lost key is not waited out");
    }

    /// Review #6: phase 2's lost key names the target and the command that provisioned it, so
    /// its help says what is left and what to run again; any other error passes through as it
    /// was.
    #[test]
    fn a_lost_age_key_after_phase_1_names_the_command_to_run_again() {
        for by in [ProvisionedBy::Up, ProvisionedBy::RestoreReprovision] {
            let err = after_provision(
                Unready::Failed(CliError::AgeKeyMissing {
                    path: "/k/age.key".into(),
                    target: "prod".into(),
                    stopped: None,
                }),
                "prod",
                by,
            );
            assert!(
                matches!(&err, CliError::AgeKeyMissing { path, target, stopped }
                    if path == "/k/age.key" && target == "prod"
                        && *stopped == Some(ProvisionStop::After { by })),
                "{err:?}"
            );
        }
        let other = after_provision(
            Unready::Failed(CliError::Other("ssh: refused".into())),
            "prod",
            ProvisionedBy::Up,
        );
        assert!(
            matches!(&other, CliError::Other(m) if m == "ssh: refused"),
            "{other:?}"
        );
    }

    #[test]
    fn any_other_failure_is_retried_until_the_apiserver_listens() {
        let mut calls = 0;
        let mut fetch = || {
            calls += 1;
            if calls < 3 {
                Err(CliError::Other("ssh: connection refused".into()))
            } else {
                Ok("    server: https://10.0.0.5:6443\n".to_string())
            }
        };
        let clock = FakeClock::new();
        poll_kubeconfig(
            &ProgressBar::hidden(),
            Duration::from_secs(30),
            Duration::from_millis(1),
            &mut fetch,
            &|addr| addr == "10.0.0.5:6443",
            &clock,
        )
        .unwrap();
        assert_eq!(calls, 3);
        assert_eq!(clock.sleeps.get(), 2, "one sleep between two attempts");
    }

    /// WI-457 follow-up: a wait that runs out keeps the wait, the count of attempts and the
    /// last error, and phase 2 then names the target and the command that provisioned it, whose
    /// help says the server bills, cluster-bootstrap did not run, and to run the command again.
    #[test]
    fn a_wait_that_runs_out_names_the_target_the_command_and_the_last_error() {
        let mut calls = 0u32;
        let mut fetch = || {
            calls += 1;
            Err(CliError::Other("ssh: connection refused".into()))
        };
        let clock = FakeClock::new();
        let unready = poll_kubeconfig(
            &ProgressBar::hidden(),
            Duration::from_millis(60),
            Duration::from_millis(10),
            &mut fetch,
            &|_| true,
            &clock,
        )
        .unwrap_err();
        // Attempts at 0, 10, 20, 30, 40 and 50 ms; the sleep after the sixth reaches the
        // deadline, and no attempt starts at it.
        assert_eq!(calls, 6, "one attempt per interval until the wait ran out");
        assert_eq!(clock.elapsed(), Duration::from_millis(60));
        assert!(
            matches!(&unready, Unready::TimedOut { waited, attempts, last_error }
                if *waited == Duration::from_millis(60) && *attempts == calls
                    && last_error == "ssh: connection refused"),
            "{unready:?}"
        );
        for by in [ProvisionedBy::Up, ProvisionedBy::RestoreReprovision] {
            let err = after_provision(
                Unready::TimedOut {
                    waited: Duration::from_secs(300),
                    attempts: 30,
                    last_error: "ssh: connection refused".into(),
                },
                "prod",
                by,
            );
            assert!(
                matches!(&err, CliError::K3sNotReady {
                    target, by: named, waited_secs: 300, attempts: 30, last_error,
                } if target == "prod" && *named == by && last_error == "ssh: connection refused"),
                "{err:?}"
            );
        }
    }

    /// The deadline's other side: an attempt that itself ends at the deadline is the last, and
    /// no sleep follows it.
    #[test]
    fn an_attempt_that_ends_at_the_deadline_is_the_last_and_no_sleep_follows_it() {
        let clock = FakeClock::new();
        let mut calls = 0u32;
        let mut fetch = || {
            calls += 1;
            clock.advance(Duration::from_millis(20));
            Err(CliError::Other("ssh: connection refused".into()))
        };
        let unready = poll_kubeconfig(
            &ProgressBar::hidden(),
            Duration::from_millis(50),
            Duration::from_millis(10),
            &mut fetch,
            &|_| true,
            &clock,
        )
        .unwrap_err();
        // Attempt 1 from 0 to 20 ms, a sleep to 30; attempt 2 from 30 to 50 ms, the deadline.
        assert_eq!(calls, 2);
        assert_eq!(
            clock.sleeps.get(),
            1,
            "no sleep after the attempt that met the deadline"
        );
        assert_eq!(clock.elapsed(), Duration::from_millis(50));
        assert!(
            matches!(&unready, Unready::TimedOut { attempts: 2, .. }),
            "{unready:?}"
        );
    }

    #[test]
    fn apiserver_addr_parses_https_server_line() {
        let kc = "apiVersion: v1\nclusters:\n- cluster:\n    \
                  server: https://167.233.83.240:6443\n    \
                  certificate-authority-data: eHh4\n";
        assert_eq!(apiserver_addr(kc).as_deref(), Some("167.233.83.240:6443"));
    }

    #[test]
    fn apiserver_addr_none_when_no_server_line() {
        assert_eq!(apiserver_addr("apiVersion: v1\nkind: Config\n"), None);
    }

    #[test]
    fn apiserver_addr_defaults_port_when_absent() {
        assert_eq!(
            apiserver_addr("    server: https://10.0.0.5\n").as_deref(),
            Some("10.0.0.5:6443")
        );
    }

    #[test]
    fn apiserver_addr_strips_trailing_slash() {
        assert_eq!(
            apiserver_addr("    server: https://10.0.0.5:6443/\n").as_deref(),
            Some("10.0.0.5:6443")
        );
    }

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
        // 79 'x' chars + the ellipsis = 80 chars total
        assert_eq!(out.chars().count(), 80);
    }
}
