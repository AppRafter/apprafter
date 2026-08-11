// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter node prep` — the idempotent umbrella that retrofits the
//! 2.16d kubelet node reservations + k3s OOM-protection AND provisions
//! host swap onto the active target's node over one k3s restart, with an
//! ATOMIC apply and a whole-step ROLLBACK (2.16g, ADR 0055).
//!
//! `node prep` supersedes the 2.16d `reserve-headroom`: it still writes
//! the same reservation `config.yaml` + `k3s.service.d/oom.conf`
//! (via [`reservation_files_script`]), but layers the swap step on top —
//! gated on k8s ≥1.34 (`NoSwap` GA) + cgroup v2 (`memory.swap.max` is
//! v2-only). The swap step is applied ATOMICALLY: on a `/readyz` timeout
//! after the k3s restart the whole step is rolled back, `failSwapOn:false`
//! removed LAST (never while swap is on), so a bad kubelet drop-in can
//! never brick the (SSH-only recoverable) node.
//!
//! Design source: `docs/superpowers/specs/2026-08-11-2.16g-node-swap-design.md`
//! decisions 1, 2, 4, 5.
//!
//! ## Structure
//!
//! - The **gate** ([`swap_eligibility`]) is a pure decision: k8s version
//!   (parsed as a SEMVER minor, numeric NOT lexical) + cgroup2fs → either
//!   `Eligible` or `Refuse(hint)`. On `<1.34` / not-cgroup2 the swap step
//!   is refused with an "upgrade k3s to ≥1.34 first" hint; reservations
//!   still apply (design decision 1, D4 — gate the STEP, not the command).
//! - The **apply** builders are pure ([`swap_dropin_write_script`]): they
//!   emit the ordered shell (drop-in → shared swap steps) from the shared
//!   `cli-providers` builders, so the retrofit and bootstrap never drift.
//! - The **rollback** is a UNIT-TESTED pure state machine ([`rollback`])
//!   driven over a mockable [`NodeOps`] trait — no node needed to test
//!   the ordering + both `swapoff` branches + the `failSwapOn:false`-last
//!   invariant (design decision 5 / Q9).
//! - **Idempotency** predicates ([`swap_already_active`],
//!   [`fstab_has_swap_entry`], [`orphan_swapfile`]) are pure functions of
//!   remote-command output (design decision D2 / P11 / Q11).

use std::time::{Duration, Instant};

use cli_core::{resolve_hetzner_token, CliError, Result};
use cli_providers::hetzner_cloud::user_data::{
    k3s_reservation_config, swap_enable_script, swap_kubelet_dropin, K3S_CONFIG_PATH,
    K3S_OOM_DROPIN, K3S_OOM_DROPIN_PATH, SWAP_KUBELET_DROPIN_PATH,
};
use cli_providers::hetzner_cloud::{default_ssh_identity_path, SshCommandRunner};
use cli_providers::{node_public_ips, HetznerCloudClient};
use cli_state::State;
use tracing::info;

use crate::cli::NodeAction;
use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::resolve_state_paths;

/// How long to wait for the k3s API to come back after the restart
/// before declaring the retrofit failed. A single-node k3s restart
/// is typically back in ~20-30s; 180s is generous headroom.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(180);
/// Poll interval while waiting for the API to recover.
const RECOVERY_INTERVAL: Duration = Duration::from_secs(5);

/// Path of the canonical swapfile provisioned by 2.16g.
const SWAPFILE_PATH: &str = "/swapfile";

/// The exact fstab line 2.16g persists (design decision 2 / Q11). The
/// idempotency predicate matches the WHOLE line incl. `sw,nofail` so an
/// old bare `/swapfile` entry with different options is NOT mistaken for
/// ours.
const SWAP_FSTAB_LINE: &str = "/swapfile none swap sw,nofail 0 0";

pub fn run(action: NodeAction) -> Result<()> {
    match action {
        // Task 6 wires the `Prep` variant + `node status`; today the enum
        // still carries `ReserveHeadroom`, so `node prep`'s umbrella entry
        // ([`node_prep`]) is exposed as a pub fn and dispatched here.
        NodeAction::ReserveHeadroom { yes } => node_prep(yes),
    }
}

// ===========================================================================
// Version gate — SEMVER minor, numeric NOT lexical.
// ===========================================================================

/// Parses a kubelet/k3s version string (`v1.35.5+k3s1`, `1.34.0`, …) into
/// its `(major, minor)` pair, comparing numerically. Returns the parsed
/// pair, or `None` if the string does not carry a `major.minor` prefix.
///
/// The leading `v` is stripped; anything after the minor (`.5+k3s1`) is
/// ignored. Both components must parse as integers — a numeric compare,
/// so `1.9 < 1.34` (lexically `"9" > "34"` is the trap this avoids).
pub fn parse_k8s_major_minor(version: &str) -> Option<(u64, u64)> {
    let v = version.trim().trim_start_matches('v');
    let mut parts = v.split('.');
    let major: u64 = parts.next()?.trim().parse().ok()?;
    // The minor may be followed by `+k3s1` / `-rc1` / etc. — take only the
    // leading run of digits.
    let minor_field = parts.next()?;
    let minor_digits: String = minor_field
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let minor: u64 = minor_digits.parse().ok()?;
    Some((major, minor))
}

/// `true` when the parsed version is ≥1.34 (the `NoSwap` GA gate),
/// comparing the minor NUMERICALLY (so `1.35 ≥ 1.34` and, critically,
/// `1.9 < 1.34`). Returns `false` on an unparseable version — the caller
/// turns that into the same REFUSE-with-hint as a genuine `<1.34`.
pub fn k8s_ge_134(version: &str) -> bool {
    match parse_k8s_major_minor(version) {
        Some((major, minor)) => major > 1 || (major == 1 && minor >= 34),
        None => false,
    }
}

/// The outcome of the swap gate (design decision 1 / D4). Either the node
/// is eligible for the swap step, or it is refused with an actionable hint
/// — but the umbrella still applies the reservations either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapGate {
    /// Both gates pass — the swap step may proceed. Carries the resolved
    /// `k8s_ge_134` flag (always `true` here, but carried explicitly so
    /// the drop-in builder is fed the same value the gate decided on).
    Eligible { k8s_ge_134: bool },
    /// A gate failed. Carries the human-facing hint the caller surfaces
    /// (the swap step is skipped; reservations still apply).
    Refuse { hint: String },
}

/// Decides swap eligibility from the two probed facts (design decision 1):
/// the node's kubelet version string and its cgroup filesystem type
/// (`stat -fc %T /sys/fs/cgroup`, expected `cgroup2fs`).
///
/// On k8s `<1.34` → REFUSE with an "upgrade k3s to ≥1.34 first" hint.
/// On not-cgroup2 → REFUSE with a cgroup-v2 hint. Both are the same shape:
/// the swap step is skipped, reservations still apply (D4).
pub fn swap_eligibility(kubelet_version: &str, cgroup_fs_type: &str) -> SwapGate {
    let ge = k8s_ge_134(kubelet_version);
    if !ge {
        return SwapGate::Refuse {
            hint: format!(
                "node kubelet is {} (<1.34) — the NoSwap GA gate needs Kubernetes ≥1.34. \
                 Upgrade k3s to ≥1.34 first, then re-run `apprafter node prep`. \
                 (Node reservations were still applied.)",
                kubelet_version.trim()
            ),
        };
    }
    if cgroup_fs_type.trim() != "cgroup2fs" {
        return SwapGate::Refuse {
            hint: format!(
                "node /sys/fs/cgroup is {} (not cgroup2fs) — `memory.swap.max` is a cgroup v2 \
                 only knob, so the NoSwap swap step cannot be applied. Boot the node on cgroup v2 \
                 first. (Node reservations were still applied.)",
                cgroup_fs_type.trim()
            ),
        };
    }
    SwapGate::Eligible { k8s_ge_134: ge }
}

// ===========================================================================
// Idempotency predicates — pure functions of remote-command output.
// ===========================================================================

/// `true` when `swapon --show` output reports `/swapfile` already active
/// (design decision D2 / P11). `swapon --show` prints a header + one line
/// per active swap area; we match the NAME column being our swapfile.
pub fn swap_already_active(swapon_show_output: &str) -> bool {
    swapon_show_output
        .lines()
        .any(|line| line.split_whitespace().next() == Some(SWAPFILE_PATH))
}

/// `true` when `/etc/fstab` already carries OUR full swap entry — matched
/// on the WHOLE normalised line incl. `sw,nofail` (design decision Q11).
/// A bare `^/swapfile` prefix match would skip an OLD entry with different
/// options; we require the exact tuple so a re-run is a genuine no-op only
/// when the persisted line is truly ours.
pub fn fstab_has_swap_entry(fstab_contents: &str) -> bool {
    fstab_contents.lines().any(|line| {
        // Normalise runs of whitespace to a single space so an entry with
        // tab-separated columns still matches the canonical spaced form.
        let normalised = line.split_whitespace().collect::<Vec<_>>().join(" ");
        normalised == SWAP_FSTAB_LINE
    })
}

/// `true` when `/swapfile` is an ORPHAN: the file exists on disk but is
/// NOT currently active (`swapon --show`) AND is NOT persisted in fstab
/// (design decision Q11). An orphan is a half-provisioned or
/// partially-rolled-back remnant — the caller surfaces it and either
/// reuses (skip the `dd`) or removes it, rather than silently `dd`-ing
/// over a live-but-unlisted file.
pub fn orphan_swapfile(
    swapfile_exists: bool,
    swapon_show_output: &str,
    fstab_contents: &str,
) -> bool {
    swapfile_exists
        && !swap_already_active(swapon_show_output)
        && !fstab_has_swap_entry(fstab_contents)
}

// ===========================================================================
// Reservation retrofit script (unchanged 2.16d logic, kept verbatim).
// ===========================================================================

/// Builds the remote shell script that lands the reservation
/// `config.yaml` + OOM drop-in atomically. Pure and side-effect-free.
///
/// This is the 2.16d reservation half of the umbrella, kept verbatim: it
/// does NOT restart k3s (the umbrella batches the reservation write + the
/// swap write into a SINGLE `daemon-reload && restart k3s`, design
/// decision 2), so this builder omits the restart the old
/// `reserve-headroom` did inline — the umbrella owns the single restart.
pub fn reservation_files_script() -> String {
    let config_body = k3s_reservation_config();
    let oom_dir = std::path::Path::new(K3S_OOM_DROPIN_PATH)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("/etc/systemd/system/k3s.service.d");
    let config_dir = std::path::Path::new(K3S_CONFIG_PATH)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("/etc/rancher/k3s");

    format!(
        "set -e\n\
         mkdir -p {config_dir} {oom_dir}\n\
         cat > {config_path} <<'APPRAFTER_K3S_CFG_EOF'\n\
         {config_body}\
         APPRAFTER_K3S_CFG_EOF\n\
         cat > {oom_path} <<'APPRAFTER_K3S_OOM_EOF'\n\
         {oom_body}\
         APPRAFTER_K3S_OOM_EOF\n",
        config_dir = config_dir,
        oom_dir = oom_dir,
        config_path = K3S_CONFIG_PATH,
        config_body = config_body,
        oom_path = K3S_OOM_DROPIN_PATH,
        oom_body = K3S_OOM_DROPIN,
    )
}

/// Builds the retrofit reservation script AND the single restart, for the
/// reservations-only path (swap gate refused). This is the closest analogue
/// of the old `reserve-headroom` behaviour: write the two files + one
/// `daemon-reload && restart k3s`.
pub fn reservations_only_script() -> String {
    let mut s = reservation_files_script();
    s.push_str("systemctl daemon-reload\n");
    s.push_str("systemctl restart k3s\n");
    s
}

// ===========================================================================
// Swap apply — the drop-in write (Option A) + shared swap steps.
// ===========================================================================

/// Builds the remote script that writes the Option-A kubelet swap drop-in
/// (design decision 2). `failSwapOn:false` is UNCONDITIONAL; `swapBehavior:
/// NoSwap` only on `k8s_ge_134`. Written FIRST in the apply order so a
/// swap-active-in-fstab / kubelet-refuses-start brick window never opens.
///
/// This does NOT swapon anything itself — it only lands the drop-in file.
pub fn swap_dropin_write_script(k8s_ge_134: bool) -> String {
    let body = swap_kubelet_dropin(k8s_ge_134);
    format!(
        "set -e\n\
         mkdir -p \"$(dirname {path})\"\n\
         cat > {path} <<'APPRAFTER_KUBELET_EOF'\n\
         {body}\
         APPRAFTER_KUBELET_EOF\n",
        path = SWAP_KUBELET_DROPIN_PATH,
        body = body,
    )
}

// ===========================================================================
// NodeOps — the SSH side-effect seam. Mockable so the rollback state
// machine + both branches are UNIT-TESTED with no node (design decision 5).
// ===========================================================================

/// The mockable side-effect seam for `node prep`: every operation that
/// touches the remote node goes through this trait so the rollback state
/// machine (and its `swapoff`-ok / `swapoff`-fail branches) can be
/// exhaustively UNIT-TESTED against a mock, no node required (design
/// decision 5 / Q9).
///
/// Each method returns `Result<()>` — an `Err` is a remote failure the
/// state machine must react to (e.g. a failed `swapoff` short-circuits the
/// `rm /swapfile`). The real implementation ([`SshNodeOps`]) drives an
/// [`SshCommandRunner`]; tests drive an in-memory recorder.
pub trait NodeOps {
    /// Rewrite the kubelet swap drop-in to `swap_kubelet_dropin(k8s_ge_134)`
    /// — i.e. remove ONLY `swapBehavior` (pass `false`) while KEEPING
    /// `failSwapOn:false`, or the full drop-in (pass `true`).
    fn write_swap_dropin(&mut self, k8s_ge_134: bool) -> Result<()>;

    /// Delete the kubelet swap drop-in file entirely (removes the LAST
    /// `failSwapOn:false` — only ever called after `swapoff` succeeded).
    fn remove_swap_dropin(&mut self) -> Result<()>;

    /// `swapoff /swapfile`, WITH A TIMEOUT (design decision 5 / P8 —
    /// `swapoff` can hang or `ENOMEM` under the very pressure that
    /// triggered the rollback). An `Err` means it failed/timed out.
    fn swapoff_with_timeout(&mut self) -> Result<()>;

    /// Remove the fstab swap entry AND the swappiness sysctl drop-in.
    fn remove_fstab_and_sysctl(&mut self) -> Result<()>;

    /// `daemon-reload && restart k3s`, then poll `/readyz` until the API
    /// is back or the recovery timeout elapses. `Err` on timeout.
    fn restart_k3s_and_wait(&mut self) -> Result<()>;

    /// `rm -f /swapfile` — only reachable once `swapoff` succeeded.
    fn remove_swapfile(&mut self) -> Result<()>;

    /// Emit a loud operator-facing runbook line (the `swapoff`-failed
    /// escape hatch: swap + `failSwapOn:false` are LEFT in place, the
    /// swapfile is NOT removed).
    fn emit_runbook(&mut self, message: &str) -> Result<()>;
}

/// The loud runbook line emitted when `swapoff` fails during rollback and
/// swap must be LEFT active. It deliberately names operator-mediated
/// workload rolls and NEVER a raw `kubectl delete` of a stateful backend
/// pod (design decision 4 / P5 — deleting a CNPG primary = failover +
/// unclean shutdown).
pub const SWAPOFF_FAILED_RUNBOOK: &str = "\
apprafter: ROLLBACK PARTIAL — `swapoff /swapfile` failed (likely under memory pressure). \
Swap is LEFT ACTIVE and the kubelet drop-in KEEPS `failSwapOn:false` (removing it now while \
swap is on would brick the kubelet on restart). /swapfile was NOT removed. \
To finish by hand once the node has headroom: `swapoff /swapfile && rm -f /swapfile`, then \
remove the fstab entry. To roll workloads off swap WITHOUT deleting stateful pods: annotate \
CNPG clusters `cnpg.io/restart` (or `kubectl cnpg restart <cluster>`), use the Dragonfly \
operator's restart path, and `kubectl rollout restart deployment <name>` for app Deployments. \
NEVER `kubectl delete pod` a CNPG or Dragonfly pod.";

/// The concrete [`NodeOps`] backed by an [`SshCommandRunner`]. Every method
/// builds a small remote command and runs it over SSH (the `restart` one
/// also drives the `/readyz` poll loop).
pub struct SshNodeOps<'a> {
    runner: &'a SshCommandRunner,
    host: &'a str,
}

impl<'a> SshNodeOps<'a> {
    pub fn new(runner: &'a SshCommandRunner, host: &'a str) -> Self {
        Self { runner, host }
    }
}

impl NodeOps for SshNodeOps<'_> {
    fn write_swap_dropin(&mut self, k8s_ge_134: bool) -> Result<()> {
        self.runner
            .run(self.host, &swap_dropin_write_script(k8s_ge_134))
            .map(|_| ())
    }

    fn remove_swap_dropin(&mut self) -> Result<()> {
        self.runner
            .run(self.host, &format!("rm -f {SWAP_KUBELET_DROPIN_PATH}"))
            .map(|_| ())
    }

    fn swapoff_with_timeout(&mut self) -> Result<()> {
        // `timeout` caps a hung `swapoff` (P8). A non-zero exit (incl. the
        // 124 `timeout` returns on expiry) surfaces as an `Err`.
        self.runner
            .run(self.host, &format!("timeout 60 swapoff {SWAPFILE_PATH}"))
            .map(|_| ())
    }

    fn remove_fstab_and_sysctl(&mut self) -> Result<()> {
        // Delete the exact fstab line + the swappiness drop-in. `sed -i`
        // with an anchored pattern; `rm -f` tolerates an absent sysctl file.
        let cmd = format!(
            "sed -i '\\#^{SWAPFILE_PATH} #d' /etc/fstab; \
             rm -f /etc/sysctl.d/99-apprafter-swap.conf"
        );
        self.runner.run(self.host, &cmd).map(|_| ())
    }

    fn restart_k3s_and_wait(&mut self) -> Result<()> {
        self.runner.run(
            self.host,
            "systemctl daemon-reload && systemctl restart k3s",
        )?;
        wait_for_recovery(self.runner, self.host)
    }

    fn remove_swapfile(&mut self) -> Result<()> {
        self.runner
            .run(self.host, &format!("rm -f {SWAPFILE_PATH}"))
            .map(|_| ())
    }

    fn emit_runbook(&mut self, message: &str) -> Result<()> {
        // Echo the runbook line locally AND onto the NODE's journal (so the
        // operator finds it) — the node write is best-effort.
        eprintln!("{message}");
        let _ = self.runner.run(
            self.host,
            &format!(
                "logger -t apprafter-node-prep {}",
                shell_single_quote(message)
            ),
        );
        Ok(())
    }
}

/// Minimal POSIX single-quote escaper for embedding a message in a remote
/// `logger '<msg>'` argv slot.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ===========================================================================
// Rollback — the UNIT-TESTED pure state machine (design decision 5 / Q9).
// ===========================================================================

/// The outcome of a whole-step rollback, distinguishing the two branches
/// so the caller (and the tests) can assert which path ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackOutcome {
    /// `swapoff` succeeded: swap is off, the drop-in (incl.
    /// `failSwapOn:false`) is fully removed, and `/swapfile` was deleted.
    Recovered,
    /// `swapoff` FAILED: swap + `failSwapOn:false` are LEFT in place, the
    /// runbook line was emitted, and `/swapfile` was NOT removed.
    SwapoffFailedSwapLeft,
}

/// The whole-step rollback state machine (design decision 5). Pure over a
/// [`NodeOps`]: no SSH, no node — the ordering + both `swapoff` branches +
/// the `failSwapOn:false`-LAST invariant are exhaustively unit-testable.
///
/// Sequence:
/// 1. Rewrite the drop-in to remove ONLY `swapBehavior`, KEEPING
///    `failSwapOn:false` (`write_swap_dropin(false)`). This is why a bad
///    `swapBehavior` is undone without ever, at any intermediate step,
///    running the node with swap on and `failSwapOn:true`.
/// 2. `swapoff` WITH a timeout.
/// 3. Remove the fstab entry + swappiness sysctl.
/// 4. Restart k3s + poll `/readyz` (get the API back).
/// 5. **Only if `swapoff` succeeded:** remove the REST of the drop-in
///    (deletes the LAST `failSwapOn:false`) + `rm /swapfile`.
///    **If `swapoff` failed:** LEAVE swap + `failSwapOn:false`, emit the
///    loud runbook line — `rm /swapfile` is UNREACHABLE.
///
/// The invariant — `failSwapOn:false` is removed LAST and NEVER while swap
/// is on — is structural: `remove_swap_dropin()` is called only on the
/// `swapoff`-ok branch, strictly after `swapoff_with_timeout()` returned
/// `Ok`.
pub fn rollback<N: NodeOps>(ops: &mut N) -> Result<RollbackOutcome> {
    // (1) Remove ONLY swapBehavior; KEEP failSwapOn:false.
    ops.write_swap_dropin(false)?;

    // (2) swapoff with timeout — remember whether it succeeded; a failure
    // is NOT propagated here (it drives the branch, it does not abort).
    let swapoff_ok = ops.swapoff_with_timeout().is_ok();

    // (3) Remove fstab + sysctl regardless (the fstab entry must not
    // survive a rollback — `sw,nofail` would silently reactivate on the
    // next boot).
    ops.remove_fstab_and_sysctl()?;

    // (4) Restart + poll to get the API back.
    ops.restart_k3s_and_wait()?;

    if swapoff_ok {
        // (5a) Swap is off → it is now SAFE to remove the last
        // `failSwapOn:false`, then delete the swapfile.
        ops.remove_swap_dropin()?;
        ops.remove_swapfile()?;
        Ok(RollbackOutcome::Recovered)
    } else {
        // (5b) Swap is STILL ON → leaving `failSwapOn:false` in place is
        // MANDATORY (removing it now bricks the kubelet on next restart).
        // `rm /swapfile` is unreachable. Emit the loud runbook.
        ops.emit_runbook(SWAPOFF_FAILED_RUNBOOK)?;
        Ok(RollbackOutcome::SwapoffFailedSwapLeft)
    }
}

// ===========================================================================
// Umbrella entry — SSH orchestration wrapping the pure pieces.
// ===========================================================================

/// The `apprafter node prep` umbrella. SSHes the active target's node,
/// applies the reservations + (when eligible) host swap over ONE k3s
/// restart, atomically, with a whole-step rollback on a `/readyz` timeout.
///
/// This is the SSH-orchestration wrapper; every decision it makes lives in
/// a pure, unit-tested helper above ([`swap_eligibility`], the idempotency
/// predicates, [`rollback`]). Not wired into `cli.rs` yet (Task 6); exposed
/// for [`run`] to dispatch.
pub fn node_prep(yes: bool) -> Result<()> {
    info!(yes, "node prep invoked");

    let resolved = resolve_state_paths(None)?;
    let paths = resolved.paths;
    let store = resolved.store;
    let state = State::load_or_default(&paths)?;

    let Some(server_id) = state.hetzner_cloud.as_ref().map(|h| h.server_id) else {
        return Err(CliError::Other(
            "no provisioned server for the active target — run `apprafter up` first".into(),
        ));
    };

    let token = resolve_hetzner_token(None, &store, None)?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);
    let (v4, _v6) = node_public_ips(&client, server_id)?;
    let host = v4.ok_or_else(|| {
        CliError::Other(
            "the active target's node has no public IPv4 yet — wait for cloud-init".into(),
        )
    })?;

    if !yes {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!(
            "This restarts k3s on {host} (~30s, API briefly unavailable) to apply node \
             reservations (system-reserved=1500Mi, kube-reserved, eviction-hard) and — when the \
             node is eligible (k8s ≥1.34 + cgroup v2) — provision host swap (NoSwap for pods)."
        );
        let confirmed = inquire::Confirm::new("Continue?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let runner = SshCommandRunner::new(default_ssh_identity_path(), paths.known_hosts_file());

    // Probe the gate facts off the live node.
    let kubelet_version = probe_kubelet_version(&runner, &host)?;
    let cgroup_fs = runner
        .run(&host, "stat -fc %T /sys/fs/cgroup")
        .unwrap_or_default();

    match swap_eligibility(&kubelet_version, &cgroup_fs) {
        SwapGate::Refuse { hint } => {
            // Reservations still apply; the swap STEP is refused (D4).
            println!("Applying node reservations on {host} over SSH…");
            runner.run(&host, &reservations_only_script())?;
            println!("Config + OOM drop-in written; k3s restarting. Waiting for the API…");
            wait_for_recovery(&runner, &host)?;
            println!("✓ Node reservations applied and the k3s API is back.");
            // The swap step was refused — surface the hint as an Err so the
            // exit code is non-zero and the reason is loud (design decision
            // 1 / N6: never silently skip).
            Err(CliError::Other(format!("swap step skipped: {hint}")))
        }
        SwapGate::Eligible { k8s_ge_134 } => apply_eligible(&runner, &host, k8s_ge_134),
    }
}

/// The eligible apply path: write reservations + the swap drop-in + the
/// swap steps, batched into ONE `daemon-reload && restart k3s`, then poll
/// `/readyz`; on timeout, run the whole-step [`rollback`].
fn apply_eligible(runner: &SshCommandRunner, host: &str, k8s_ge_134: bool) -> Result<()> {
    // --- Idempotency probes (design decision D2 / Q11). ---
    let swapon_show = runner.run(host, "swapon --show").unwrap_or_default();
    let fstab = runner.run(host, "cat /etc/fstab").unwrap_or_default();
    let swapfile_exists = runner
        .run(host, &format!("test -e {SWAPFILE_PATH}"))
        .is_ok();

    if swap_already_active(&swapon_show) && fstab_has_swap_entry(&fstab) {
        // Fully provisioned already — re-run is a near no-op: still refresh
        // the drop-in + reservations (cheap, idempotent) then restart.
        println!("Swap already active and persisted; refreshing reservations + drop-in on {host}…");
    } else if orphan_swapfile(swapfile_exists, &swapon_show, &fstab) {
        // A half-provisioned/partially-rolled-back remnant. Surface it and
        // remove it so the fresh apply's `dd` starts clean (design Q11).
        println!(
            "⚠ Found an orphan {SWAPFILE_PATH} on {host} (present but not active and not in \
             fstab) — removing it before a clean re-provision."
        );
        runner.run(
            host,
            &format!("swapoff {SWAPFILE_PATH} 2>/dev/null; rm -f {SWAPFILE_PATH}"),
        )?;
    }

    println!("Applying node reservations + host swap on {host} over SSH…");

    // (1) Reservations (files only — the umbrella owns the single restart).
    runner.run(host, &reservation_files_script())?;
    // (2) Swap drop-in FIRST (failSwapOn:false unconditional; NoSwap ≥1.34).
    runner.run(host, &swap_dropin_write_script(k8s_ge_134))?;
    // (3) The shared swap steps (swappiness → dd → mkswap → swapon → inline
    //     cgroup swap.max=0 → fstab). Read MemTotal off the node first so
    //     the shared retrofit builder can pin the count.
    let mem_total_kib = probe_mem_total_kib(runner, host)?;
    let swap_steps = swap_enable_script(
        mem_total_kib,
        k8s_ge_134,
        true, // cgroup2 already gated above
    )
    .ok_or_else(|| {
        CliError::Other(
            "internal: swap builder returned None despite passing the eligibility gate".into(),
        )
    })?;
    runner.run(host, &swap_steps)?;

    // (4) Single daemon-reload && restart k3s → /readyz poll.
    println!("k3s restarting to pick up the swap drop-in. Waiting for the API to recover…");
    runner.run(host, "systemctl daemon-reload && systemctl restart k3s")?;

    match wait_for_recovery(runner, host) {
        Ok(()) => {
            println!("✓ Node reservations + host swap applied and the k3s API is back.");
            println!("  swap active (NoSwap for pods), swappiness=10, fstab sw,nofail.");
            Ok(())
        }
        Err(recovery_err) => {
            // ATOMIC: the k3s API did not come back → the whole swap step is
            // rolled back (design decision 5), `failSwapOn:false` removed LAST.
            eprintln!(
                "✗ k3s did not recover after the swap apply ({recovery_err}). \
                 Rolling the whole swap step back…"
            );
            let mut ops = SshNodeOps::new(runner, host);
            match rollback(&mut ops)? {
                RollbackOutcome::Recovered => Err(CliError::Other(format!(
                    "swap apply failed and was rolled back cleanly (swap removed, kubelet drop-in \
                     removed, /swapfile deleted, k3s recovered). Original error: {recovery_err}"
                ))),
                RollbackOutcome::SwapoffFailedSwapLeft => Err(CliError::Other(format!(
                    "swap apply failed; rollback could NOT swapoff (swap + failSwapOn:false left \
                     in place, /swapfile kept — see the runbook line above). Original error: \
                     {recovery_err}"
                ))),
            }
        }
    }
}

/// Reads the node's kubelet version via `kubectl get node … jsonpath`. The
/// node-local `k3s kubectl` needs no external creds. Design decision 1 (P3):
/// the kubeletVersion (not `configz`) answers "would swapBehavior be
/// accepted".
fn probe_kubelet_version(runner: &SshCommandRunner, host: &str) -> Result<String> {
    // `-o jsonpath` over the single node. `$(hostname)` resolves the node
    // name on the node itself; k3s uses the hostname as the node name.
    let out = runner.run(
        host,
        "k3s kubectl get node \"$(hostname)\" \
         -o jsonpath='{.status.nodeInfo.kubeletVersion}'",
    )?;
    let v = out.trim().to_string();
    if v.is_empty() {
        return Err(CliError::Other(
            "could not read the node's kubeletVersion (empty jsonpath result)".into(),
        ));
    }
    Ok(v)
}

/// Reads `MemTotal` (KiB) from `/proc/meminfo` on the node — the retrofit
/// pins the swap count at build time from this (design decision 2 / P13).
fn probe_mem_total_kib(runner: &SshCommandRunner, host: &str) -> Result<u64> {
    let out = runner.run(host, "awk '/^MemTotal:/ {print $2}' /proc/meminfo")?;
    out.trim().parse::<u64>().map_err(|e| {
        CliError::Other(format!(
            "could not parse MemTotal from /proc/meminfo ('{}'): {e}",
            out.trim()
        ))
    })
}

/// Polls the node until `k3s kubectl get --raw=/readyz` succeeds or the
/// timeout elapses.
fn wait_for_recovery(runner: &SshCommandRunner, host: &str) -> Result<()> {
    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    let mut last_err;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match runner.run(host, recovery_probe_command()) {
            Ok(_) => return Ok(()),
            Err(e) => last_err = e.to_string(),
        }
        if Instant::now() >= deadline {
            return Err(CliError::Other(format!(
                "k3s API did not recover within {}s after restart ({attempt} attempts). \
                 Last probe error: {last_err}",
                RECOVERY_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(RECOVERY_INTERVAL);
    }
}

/// Remote command that succeeds once the k3s API is serving again.
fn recovery_probe_command() -> &'static str {
    "k3s kubectl get --raw='/readyz'"
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- reservation retrofit (2.16d logic, kept) -----------------------

    #[test]
    fn reservation_files_script_embeds_shared_reservation_config() {
        let script = reservation_files_script();
        assert!(
            script.contains(&k3s_reservation_config()),
            "retrofit script must embed the shared k3s_reservation_config() verbatim:\n{script}"
        );
        assert!(script.contains(K3S_OOM_DROPIN), "{script}");
    }

    #[test]
    fn reservation_files_script_uses_quoted_heredocs_and_no_restart() {
        let script = reservation_files_script();
        assert!(script.contains("<<'APPRAFTER_K3S_CFG_EOF'"), "{script}");
        assert!(script.contains("<<'APPRAFTER_K3S_OOM_EOF'"), "{script}");
        assert!(script.starts_with("set -e\n"), "{script}");
        // The umbrella owns the SINGLE restart — the files-only builder
        // must NOT restart k3s itself.
        assert!(
            !script.contains("systemctl restart k3s"),
            "files-only builder must not restart k3s (umbrella owns the single restart)\n{script}"
        );
    }

    #[test]
    fn reservations_only_script_restarts_once() {
        let s = reservations_only_script();
        assert!(s.contains("systemctl daemon-reload"), "{s}");
        assert!(s.contains("systemctl restart k3s"), "{s}");
    }

    #[test]
    fn recovery_probe_uses_node_local_k3s_kubectl() {
        assert_eq!(recovery_probe_command(), "k3s kubectl get --raw='/readyz'");
    }

    // ---- (a) version gate — numeric NOT lexical -------------------------

    #[test]
    fn gate_proceeds_on_v1_35_5_k3s1() {
        // v1.35.5+k3s1 → ≥1.34 + cgroup2fs → Eligible.
        let gate = swap_eligibility("v1.35.5+k3s1", "cgroup2fs");
        assert_eq!(gate, SwapGate::Eligible { k8s_ge_134: true }, "{gate:?}");
    }

    #[test]
    fn gate_refuses_below_134_with_upgrade_hint() {
        // v1.33.x → refuse, hint must mention the ≥1.34 upgrade.
        let gate = swap_eligibility("v1.33.4+k3s1", "cgroup2fs");
        match gate {
            SwapGate::Refuse { hint } => {
                assert!(
                    hint.contains("1.34"),
                    "refuse hint must name the ≥1.34 requirement: {hint}"
                );
                assert!(
                    hint.to_lowercase().contains("upgrade"),
                    "refuse hint must tell the user to UPGRADE: {hint}"
                );
            }
            other => panic!("expected Refuse for <1.34, got {other:?}"),
        }
    }

    #[test]
    fn gate_compare_is_numeric_not_lexical_1_9_vs_1_34() {
        // The trap: lexically "1.9" > "1.34" (because '9' > '3'), but
        // NUMERICALLY 1.9 < 1.34. A lexical compare would wrongly PROCEED.
        assert!(!k8s_ge_134("v1.9.0+k3s1"), "1.9 must be < 1.34 numerically");
        assert!(k8s_ge_134("v1.34.0"), "1.34 must be ≥ 1.34");
        assert!(k8s_ge_134("v1.35.5+k3s1"), "1.35 must be ≥ 1.34");
        assert!(!k8s_ge_134("v1.33.9"), "1.33 must be < 1.34");
        // And the gate reflects it: 1.9 → Refuse even on cgroup2fs.
        assert!(matches!(
            swap_eligibility("v1.9.0+k3s1", "cgroup2fs"),
            SwapGate::Refuse { .. }
        ));
    }

    #[test]
    fn gate_refuses_when_not_cgroup2() {
        let gate = swap_eligibility("v1.35.5+k3s1", "tmpfs");
        match gate {
            SwapGate::Refuse { hint } => {
                assert!(
                    hint.to_lowercase().contains("cgroup"),
                    "not-cgroup2 refuse hint must mention cgroup: {hint}"
                );
            }
            other => panic!("expected Refuse for non-cgroup2, got {other:?}"),
        }
    }

    #[test]
    fn parse_major_minor_handles_suffixes_and_missing_parts() {
        assert_eq!(parse_k8s_major_minor("v1.35.5+k3s1"), Some((1, 35)));
        assert_eq!(parse_k8s_major_minor("1.34"), Some((1, 34)));
        assert_eq!(parse_k8s_major_minor("v2.0.0"), Some((2, 0)));
        assert_eq!(parse_k8s_major_minor("garbage"), None);
        assert_eq!(parse_k8s_major_minor(""), None);
    }

    // ---- (b) rollback state machine over a mock NodeOps -----------------

    /// A recording mock NodeOps: appends a tag per call so tests assert on
    /// the ordering + which branch ran. `swapoff_result` controls the
    /// branch under test.
    #[derive(Default)]
    struct MockNode {
        calls: Vec<&'static str>,
        swapoff_result: SwapoffResult,
        last_runbook: Option<String>,
    }

    #[derive(Default, Clone, Copy, PartialEq)]
    enum SwapoffResult {
        #[default]
        Ok,
        Fail,
    }

    impl NodeOps for MockNode {
        fn write_swap_dropin(&mut self, k8s_ge_134: bool) -> Result<()> {
            // The rollback's step (1) passes false (keep failSwapOn only).
            self.calls.push(if k8s_ge_134 {
                "write_dropin_full"
            } else {
                "write_dropin_nobehavior"
            });
            Ok(())
        }
        fn remove_swap_dropin(&mut self) -> Result<()> {
            self.calls.push("remove_dropin");
            Ok(())
        }
        fn swapoff_with_timeout(&mut self) -> Result<()> {
            self.calls.push("swapoff");
            match self.swapoff_result {
                SwapoffResult::Ok => Ok(()),
                SwapoffResult::Fail => Err(CliError::Other("swapoff timed out".into())),
            }
        }
        fn remove_fstab_and_sysctl(&mut self) -> Result<()> {
            self.calls.push("remove_fstab_sysctl");
            Ok(())
        }
        fn restart_k3s_and_wait(&mut self) -> Result<()> {
            self.calls.push("restart_wait");
            Ok(())
        }
        fn remove_swapfile(&mut self) -> Result<()> {
            self.calls.push("rm_swapfile");
            Ok(())
        }
        fn emit_runbook(&mut self, message: &str) -> Result<()> {
            self.calls.push("runbook");
            self.last_runbook = Some(message.to_string());
            Ok(())
        }
    }

    #[test]
    fn rollback_swapoff_ok_reaches_rm_and_removes_dropin_last() {
        let mut node = MockNode {
            swapoff_result: SwapoffResult::Ok,
            ..Default::default()
        };
        let outcome = rollback(&mut node).expect("rollback runs");
        assert_eq!(outcome, RollbackOutcome::Recovered);
        assert_eq!(
            node.calls,
            vec![
                "write_dropin_nobehavior", // (1) remove swapBehavior, KEEP failSwapOn
                "swapoff",                 // (2)
                "remove_fstab_sysctl",     // (3)
                "restart_wait",            // (4)
                "remove_dropin",           // (5a) failSwapOn:false removed LAST
                "rm_swapfile",             // (5a) then the swapfile
            ],
            "swapoff-ok branch must reach rm /swapfile and remove the drop-in LAST"
        );
    }

    #[test]
    fn rollback_swapoff_fail_does_not_reach_rm_leaves_swap_and_emits_runbook() {
        let mut node = MockNode {
            swapoff_result: SwapoffResult::Fail,
            ..Default::default()
        };
        let outcome = rollback(&mut node).expect("rollback runs");
        assert_eq!(outcome, RollbackOutcome::SwapoffFailedSwapLeft);
        // rm /swapfile is UNREACHABLE; the drop-in (failSwapOn:false) is NOT
        // removed; the runbook line is emitted.
        assert!(
            !node.calls.contains(&"rm_swapfile"),
            "swapoff-fail branch must NOT reach rm /swapfile: {:?}",
            node.calls
        );
        assert!(
            !node.calls.contains(&"remove_dropin"),
            "swapoff-fail branch must LEAVE failSwapOn:false (drop-in NOT removed): {:?}",
            node.calls
        );
        assert!(
            node.calls.contains(&"runbook"),
            "swapoff-fail branch must emit the loud runbook line: {:?}",
            node.calls
        );
        assert_eq!(
            node.last_runbook.as_deref(),
            Some(SWAPOFF_FAILED_RUNBOOK),
            "the emitted runbook must be the canonical SWAPOFF_FAILED_RUNBOOK"
        );
    }

    #[test]
    fn rollback_fail_swap_on_false_removed_last_invariant() {
        // The invariant: `failSwapOn:false` is only ever removed AFTER a
        // successful swapoff, and it is the LAST-but-one op (before rm).
        // Structurally: on the ok branch remove_dropin appears exactly once,
        // strictly after swapoff, and swapoff must have succeeded.
        let mut node = MockNode {
            swapoff_result: SwapoffResult::Ok,
            ..Default::default()
        };
        rollback(&mut node).unwrap();
        let swapoff_pos = node.calls.iter().position(|c| *c == "swapoff").unwrap();
        let remove_dropin_pos = node
            .calls
            .iter()
            .position(|c| *c == "remove_dropin")
            .expect("drop-in removed on ok branch");
        assert!(
            remove_dropin_pos > swapoff_pos,
            "failSwapOn:false (remove_dropin) must be removed strictly AFTER swapoff succeeds"
        );
        // Step (1) must NOT have removed the drop-in — it only rewrote it to
        // drop swapBehavior while KEEPING failSwapOn:false.
        assert_eq!(
            node.calls[0], "write_dropin_nobehavior",
            "step 1 must rewrite the drop-in keeping failSwapOn:false, not delete it"
        );
    }

    #[test]
    fn swapoff_failed_runbook_never_names_a_pod_delete() {
        // Design decision 4 / P5: the fallback names operator-mediated
        // rolls, NEVER a raw pod delete of a stateful backend.
        assert!(SWAPOFF_FAILED_RUNBOOK.contains("cnpg.io/restart"));
        assert!(SWAPOFF_FAILED_RUNBOOK.contains("rollout restart deployment"));
        // The only mention of `kubectl delete pod` must be the prohibition.
        assert!(
            SWAPOFF_FAILED_RUNBOOK.contains("NEVER `kubectl delete pod`"),
            "the runbook must explicitly forbid deleting CNPG/Dragonfly pods"
        );
        assert_eq!(
            SWAPOFF_FAILED_RUNBOOK.matches("kubectl delete pod").count(),
            1,
            "the ONLY `kubectl delete pod` mention must be the NEVER prohibition"
        );
    }

    // ---- (c) idempotency predicates incl. orphan ------------------------

    #[test]
    fn swap_active_predicate_matches_swapfile_name_column() {
        let show = "NAME      TYPE      SIZE USED PRIO\n/swapfile file      4G   0B   -2\n";
        assert!(swap_already_active(show));
        // A DIFFERENT swap area (a partition) must NOT count as our file.
        let other = "NAME      TYPE      SIZE USED PRIO\n/dev/sda2 partition 2G   0B   -2\n";
        assert!(!swap_already_active(other));
        assert!(!swap_already_active(""));
    }

    #[test]
    fn fstab_predicate_requires_full_line_incl_nofail() {
        // Our exact line matches (incl. tab-normalisation).
        assert!(fstab_has_swap_entry(
            "UUID=... / ext4 defaults 0 1\n/swapfile none swap sw,nofail 0 0\n"
        ));
        assert!(fstab_has_swap_entry(
            "/swapfile\tnone\tswap\tsw,nofail\t0\t0\n"
        ));
        // An OLD bare entry with DIFFERENT options must NOT match — a
        // `^/swapfile` prefix would wrongly skip re-persisting ours (Q11).
        assert!(
            !fstab_has_swap_entry("/swapfile none swap defaults 0 0\n"),
            "a different-options /swapfile entry must NOT satisfy the full-line predicate"
        );
        assert!(!fstab_has_swap_entry(""));
    }

    #[test]
    fn orphan_predicate_true_only_when_present_inactive_and_not_in_fstab() {
        // Present on disk, not active, not in fstab → ORPHAN.
        assert!(orphan_swapfile(true, "NAME TYPE\n", ""));
        // Present + ACTIVE → not an orphan (it's live).
        assert!(!orphan_swapfile(
            true,
            "NAME TYPE SIZE\n/swapfile file 4G\n",
            ""
        ));
        // Present + in fstab → not an orphan (fully provisioned).
        assert!(!orphan_swapfile(
            true,
            "NAME TYPE\n",
            "/swapfile none swap sw,nofail 0 0\n"
        ));
        // Absent on disk → never an orphan.
        assert!(!orphan_swapfile(false, "NAME TYPE\n", ""));
    }

    // ---- drop-in write builder ------------------------------------------

    #[test]
    fn swap_dropin_write_script_writes_option_a_path_and_body() {
        let s = swap_dropin_write_script(true);
        assert!(s.contains(SWAP_KUBELET_DROPIN_PATH), "{s}");
        assert!(s.contains("failSwapOn: false"), "{s}");
        assert!(
            s.contains("swapBehavior: NoSwap"),
            "≥1.34 → NoSwap block\n{s}"
        );
        // <1.34 variant keeps failSwapOn but drops swapBehavior.
        let s0 = swap_dropin_write_script(false);
        assert!(s0.contains("failSwapOn: false"), "{s0}");
        assert!(
            !s0.contains("swapBehavior"),
            "<1.34 must not emit swapBehavior\n{s0}"
        );
    }
}
