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

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use cli_core::{resolve_hetzner_token, CliError, Result};
use cli_providers::hetzner_cloud::user_data::{
    k3s_reservation_config, swap_enable_script, swap_kubelet_dropin, K3S_CONFIG_PATH,
    K3S_OOM_DROPIN, K3S_OOM_DROPIN_PATH, SKIP_NODE_SWAP_ENV, SWAP_KUBELET_DROPIN_PATH,
    SWAP_PROVISION_STATUS_PATH,
};
use cli_providers::hetzner_cloud::{default_ssh_identity_path, SshCommandRunner};
use cli_providers::{node_public_ips, HetznerCloudClient};
use cli_state::State;
use tracing::info;

use crate::cli::NodeAction;
use crate::commands::hcloud::hcloud_base_url;
use crate::commands::k8s_helpers::ensure_kubeconfig_tempfile;
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
        NodeAction::Prep { yes } => node_prep(yes),
        NodeAction::Status => status(),
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

// ===========================================================================
// `apprafter node status` (design decision 6, P15/Q10/Q17/Q18).
//
// The state is assembled from TWO sources — the kube-API (no SSH) and a
// live SSH probe (may be down) — with graceful degradation: if SSH fails
// the SSH-derived fields render `unknown`, the kube-API fields still show,
// and each field is labelled `[api]` / `[ssh]` so a partial result reads
// cleanly. The renderer is PURE ([`render_status`]) over a [`NodeSwapState`]
// value; the IO ([`probe_node_swap_state`]) is a thin wrapper.
// ===========================================================================

/// A single reported field: `Known(value)` when the source answered,
/// `Unknown` when it was unreachable (SSH down, jsonpath empty, …). The
/// renderer prints `Unknown` as `unknown` so a degraded result is loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    /// The source answered with this value.
    Known(String),
    /// The source was unreachable / gave no answer.
    Unknown,
}

impl Field {
    /// Renders the field body: the value, or the literal `unknown`.
    fn render(&self) -> &str {
        match self {
            Field::Known(v) => v.as_str(),
            Field::Unknown => "unknown",
        }
    }
}

/// The classified provision state, derived from the breadcrumb + the live
/// probes (design decision 6). Drives the leading one-line verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapProvisionState {
    /// Swap is live (`swapon --show` reports `/swapfile`); carries the size
    /// column when known.
    Active { size: Option<String> },
    /// The undocumented `APPRAFTER_SKIP_NODE_SWAP` hook was set at provision
    /// — the node was installed one-shot, no swap, deliberately.
    SkippedByEnv,
    /// The node's k8s is `<1.34` — the NoSwap GA gate is not met; the fix is
    /// a k3s upgrade.
    Ineligible { detail: String },
    /// The node is eligible (≥1.34 + cgroup2) but no swap is applied yet —
    /// the actionable state: run `apprafter node prep`.
    EligibleNotApplied,
    /// The bootstrap swap step ran and FAILED (the FAIL-SOFT breadcrumb) —
    /// the node started cushionless.
    ProvisionFailed { detail: String },
    /// `/swapfile` exists on disk but is neither active nor in fstab — an
    /// orphan remnant of a half-provision / partial rollback.
    OrphanSwapfile,
    /// Neither the breadcrumb nor the live probes could be read (SSH down and
    /// no other signal) — the state is genuinely unknown.
    Unknown,
}

/// The whole reported node-swap posture — the pure renderer's sole input.
/// Every field carries its own `Known`/`Unknown` so a partial (SSH-down)
/// result is representable without failing wholesale (design decision 6 /
/// Q10). The `state` is the classified verdict; `ssh_available` records
/// whether the SSH probe connected at all (drives the degradation banner).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSwapState {
    /// The node name the status was read for.
    pub node_name: String,
    /// Whether the SSH probe reached the node (false ⇒ SSH fields degrade).
    pub ssh_available: bool,
    /// The classified verdict.
    pub state: SwapProvisionState,

    // --- kube-API sourced (`[api]`) — available without SSH. ---
    /// `failSwapOn` from `configz` (expected `false` when applied).
    pub fail_swap_on: Field,
    /// `swapBehavior` from `configz` (expected `NoSwap` when applied ≥1.34).
    pub swap_behavior: Field,
    /// `status.nodeInfo.swap.capacity` off the Node object.
    pub swap_capacity: Field,
    /// `status.nodeInfo.kubeletVersion` off the Node object.
    pub kubelet_version: Field,

    // --- SSH sourced (`[ssh]`) — `Unknown` when SSH is down. ---
    /// Live `swapon --show` summary line for `/swapfile` (name/size/used).
    pub swapon: Field,
    /// `vm.swappiness` sysctl.
    pub swappiness: Field,
    /// `GOMEMLIMIT` from the k3s.service environment.
    pub gomemlimit: Field,
    /// The raw `/var/lib/apprafter/swap-provision.status` breadcrumb line.
    pub provision_breadcrumb: Field,
}

/// One-line human verdict for a [`SwapProvisionState`].
fn state_verdict(state: &SwapProvisionState) -> String {
    match state {
        SwapProvisionState::Active { size: Some(sz) } => {
            format!("swap active ({sz}) — NoSwap for pods")
        }
        SwapProvisionState::Active { size: None } => "swap active — NoSwap for pods".to_string(),
        SwapProvisionState::SkippedByEnv => {
            format!("swap skipped by env ({SKIP_NODE_SWAP_ENV}) — node runs cushionless by request")
        }
        SwapProvisionState::Ineligible { detail } => {
            format!("ineligible ({detail}) — upgrade k3s to ≥1.34, then run `apprafter node prep`")
        }
        SwapProvisionState::EligibleNotApplied => {
            "eligible, not applied — run `apprafter node prep`".to_string()
        }
        SwapProvisionState::ProvisionFailed { detail } => {
            format!("swap step failed at provision ({detail}) — node started cushionless; re-run `apprafter node prep`")
        }
        SwapProvisionState::OrphanSwapfile => {
            "orphan /swapfile (present but not active and not in fstab) — `apprafter node prep` removes it before a clean re-provision".to_string()
        }
        SwapProvisionState::Unknown => {
            "state unknown — could not reach the node over SSH and no other signal".to_string()
        }
    }
}

/// The PURE renderer (design decision 6): a [`NodeSwapState`] → a
/// human-readable report. Every field line is labelled with its SOURCE
/// (`[api]` / `[ssh]`) so a partial (SSH-down) result reads cleanly; the
/// SSH-derived fields print `unknown` on degradation, with a leading banner
/// noting SSH was unavailable. No IO — unit-testable in full.
pub fn render_status(s: &NodeSwapState) -> String {
    let mut out = String::new();
    out.push_str(&format!("Node swap status for {}\n", s.node_name));
    out.push_str(&format!("  {}\n", state_verdict(&s.state)));
    if !s.ssh_available {
        out.push_str(
            "  ⚠ SSH unavailable — the [ssh] fields below could not be read (shown as `unknown`); \
             the [api] fields are still authoritative.\n",
        );
    }
    out.push('\n');
    // kube-API fields (available without SSH).
    out.push_str(&format!(
        "  [api] kubeletVersion : {}\n",
        s.kubelet_version.render()
    ));
    out.push_str(&format!(
        "  [api] failSwapOn     : {}\n",
        s.fail_swap_on.render()
    ));
    out.push_str(&format!(
        "  [api] swapBehavior   : {}\n",
        s.swap_behavior.render()
    ));
    out.push_str(&format!(
        "  [api] swap.capacity  : {}\n",
        s.swap_capacity.render()
    ));
    // SSH fields (unknown when degraded).
    out.push_str(&format!("  [ssh] swapon         : {}\n", s.swapon.render()));
    out.push_str(&format!(
        "  [ssh] vm.swappiness  : {}\n",
        s.swappiness.render()
    ));
    out.push_str(&format!(
        "  [ssh] GOMEMLIMIT     : {}\n",
        s.gomemlimit.render()
    ));
    out.push_str(&format!(
        "  [ssh] provision      : {}\n",
        s.provision_breadcrumb.render()
    ));
    out
}

/// Classifies the [`SwapProvisionState`] from the probed facts (pure). The
/// ORDER matters: a live `/swapfile` in `swapon` is `Active` regardless of
/// what the breadcrumb says; then the breadcrumb's `skipped`/`ineligible`/
/// `failed` verdicts; then an orphan on-disk `/swapfile`; then eligible-vs-
/// unknown when SSH itself was down.
fn classify_state(
    ssh_available: bool,
    swap_active: bool,
    swap_size: Option<String>,
    orphan: bool,
    breadcrumb: &Field,
    api_eligible: Option<bool>,
) -> SwapProvisionState {
    if swap_active {
        return SwapProvisionState::Active { size: swap_size };
    }
    // The breadcrumb is authoritative for the non-active provision verdicts.
    if let Field::Known(line) = breadcrumb {
        let l = line.trim();
        if l.starts_with("skipped") || l.contains(SKIP_NODE_SWAP_ENV) {
            return SwapProvisionState::SkippedByEnv;
        }
        if let Some(rest) = l.strip_prefix("ineligible:") {
            return SwapProvisionState::Ineligible {
                detail: rest.trim().to_string(),
            };
        }
        if let Some(rest) = l.strip_prefix("failed:") {
            return SwapProvisionState::ProvisionFailed {
                detail: rest.trim().to_string(),
            };
        }
        // `applied: …` but not currently active ⇒ the swapfile silently did
        // not reactivate (the `sw,nofail` P9 trap) — treat it as not-applied
        // if there is no orphan, else fall through to the orphan branch.
    }
    if orphan {
        return SwapProvisionState::OrphanSwapfile;
    }
    match api_eligible {
        Some(true) => SwapProvisionState::EligibleNotApplied,
        Some(false) => SwapProvisionState::Ineligible {
            detail: "k8s <1.34".to_string(),
        },
        None => {
            if ssh_available {
                // SSH is up and there is no swap / breadcrumb / orphan, but the
                // kube-API eligibility could not be read — still actionable.
                SwapProvisionState::EligibleNotApplied
            } else {
                SwapProvisionState::Unknown
            }
        }
    }
}

/// The `apprafter node status` entry (design decision 6). Reads the kube-API
/// fields (over the cached kubeconfig — no SSH) and, best-effort, the live
/// SSH fields; assembles a [`NodeSwapState`] and prints [`render_status`].
/// SSH being down degrades gracefully — the SSH fields read `unknown`, the
/// command still succeeds.
pub fn status() -> Result<()> {
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

    let runner = SshCommandRunner::new(default_ssh_identity_path(), paths.known_hosts_file());

    // Resolve the cached (age-encrypted) out-of-cluster kubeconfig for the
    // active target — the kube-API fields read through it, so SSH being down
    // no longer forces them `Unknown` (Q10). Best-effort: a missing cache
    // just leaves the `[api]` fields `Unknown`, it is not a hard error.
    let kubeconfig = ensure_kubeconfig_tempfile().ok();

    let node_state = probe_node_swap_state(&runner, &host, kubeconfig.as_ref().map(|f| f.path()));
    print!("{}", render_status(&node_state));
    Ok(())
}

/// The IO wrapper (design decision 6 / Q10): probes the kube-API fields + the
/// live SSH fields off the node and assembles a [`NodeSwapState`]. Both source
/// groups are BEST-EFFORT — a missing kube-API field or a down SSH becomes
/// `Field::Unknown` rather than an error, so the report degrades gracefully.
///
/// The kube-API fields (`configz` → `failSwapOn`/`swapBehavior`, the Node
/// object's `swap.capacity`/`kubeletVersion`) are read through the CLI's own
/// cached OUT-OF-CLUSTER kubeconfig (`kubeconfig_path`), NOT over SSH — so SSH
/// being down no longer forces them `Unknown` (the graceful-degradation intent
/// of decision 6). When the cached kubeconfig is missing / the API is
/// unreachable, THOSE fields are `Unknown`; independently, when SSH is down the
/// `[ssh]` fields are `Unknown`. The two groups stay labelled distinctly.
fn probe_node_swap_state(
    runner: &SshCommandRunner,
    host: &str,
    kubeconfig_path: Option<&Path>,
) -> NodeSwapState {
    // --- kube-API fields (out-of-cluster kubeconfig, no SSH). ---
    // The node name comes from the kube-API too (k3s uses the hostname as the
    // single node's name) so the configz/Node-object reads never need SSH.
    let api_node_name = kubeconfig_path.and_then(kube_node_name);
    let (fail_swap_on, swap_behavior) = probe_configz(kubeconfig_path, api_node_name.as_deref());
    let swap_capacity = probe_node_field(
        kubeconfig_path,
        api_node_name.as_deref(),
        "{.status.nodeInfo.swap.capacity}",
    );
    let kubelet_version = probe_node_field(
        kubeconfig_path,
        api_node_name.as_deref(),
        "{.status.nodeInfo.kubeletVersion}",
    );

    // A single cheap SSH probe decides SSH reachability up-front (`hostname`
    // also gives a node-name fallback for the report header when the kube-API
    // could not answer it).
    let ssh_node_name = runner
        .run(host, "hostname")
        .ok()
        .map(|s| s.trim().to_string());
    let ssh_available = ssh_node_name.is_some();
    let node_name = api_node_name
        .or(ssh_node_name)
        .unwrap_or_else(|| "<unknown>".to_string());

    // --- SSH fields (live probes). ---
    let swapon_raw = runner.run(host, "swapon --show").ok();
    let fstab_raw = runner.run(host, "cat /etc/fstab").unwrap_or_default();
    let swapfile_exists = runner
        .run(host, &format!("test -e {SWAPFILE_PATH}"))
        .is_ok();
    let swapon = swapon_summary(swapon_raw.as_deref());
    let swappiness = field_or_unknown(runner.run(host, "sysctl -n vm.swappiness").ok());
    let gomemlimit = probe_gomemlimit(runner, host);
    let provision_breadcrumb = field_or_unknown(
        runner
            .run(host, &format!("cat {SWAP_PROVISION_STATUS_PATH}"))
            .ok(),
    );

    // --- Classify. ---
    let swap_active = swapon_raw
        .as_deref()
        .map(swap_already_active)
        .unwrap_or(false);
    let swap_size = swapon_raw.as_deref().and_then(swapfile_size_column);
    let orphan = orphan_swapfile(
        swapfile_exists,
        swapon_raw.as_deref().unwrap_or(""),
        &fstab_raw,
    );
    // `APPRAFTER_SKIP_NODE_SWAP` at status time also forces the skipped verdict
    // (the operator asks "what did I get" with the same hook set).
    let env_skipped = std::env::var_os(SKIP_NODE_SWAP_ENV).is_some();
    let api_eligible = match &kubelet_version {
        Field::Known(v) => Some(k8s_ge_134(v)),
        Field::Unknown => None,
    };
    let state = if env_skipped && !swap_active {
        SwapProvisionState::SkippedByEnv
    } else {
        classify_state(
            ssh_available,
            swap_active,
            swap_size,
            orphan,
            &provision_breadcrumb,
            api_eligible,
        )
    };

    NodeSwapState {
        node_name,
        ssh_available,
        state,
        fail_swap_on,
        swap_behavior,
        swap_capacity,
        kubelet_version,
        swapon,
        swappiness,
        gomemlimit,
        provision_breadcrumb,
    }
}

/// Runs `kubectl --kubeconfig <path> <args…>` and returns its stdout on
/// success, or `None` on any failure (no kubeconfig, spawn error, non-zero
/// exit). The cached out-of-cluster kubeconfig is the source, so the kube-API
/// fields are readable even when SSH to the node is down (Q10).
fn kubectl_capture(kubeconfig_path: Option<&Path>, args: &[&str]) -> Option<String> {
    let path = kubeconfig_path?;
    let out = Command::new("kubectl")
        .args(args)
        .env("KUBECONFIG", path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Reads the single node's name off the kube-API (`kubectl get nodes
/// -o jsonpath='{.items[0].metadata.name}'`). `None` when the kubeconfig is
/// absent / the API is unreachable / there is no node. T1 is single-node, so
/// `items[0]` is the node.
fn kube_node_name(kubeconfig_path: &Path) -> Option<String> {
    let out = kubectl_capture(
        Some(kubeconfig_path),
        &["get", "nodes", "-o", "jsonpath={.items[0].metadata.name}"],
    )?;
    let name = out.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Reads `failSwapOn` + `swapBehavior` from the kubelet `configz` endpoint
/// (`/api/v1/nodes/<n>/proxy/configz`) via the cached OUT-OF-CLUSTER
/// kubeconfig — `kubectl get --raw` (NOT SSH, Q10). Both degrade to `Unknown`
/// independently on any failure / absent key / absent kubeconfig / unknown
/// node name.
fn probe_configz(kubeconfig_path: Option<&Path>, node_name: Option<&str>) -> (Field, Field) {
    let Some(node) = node_name else {
        return (Field::Unknown, Field::Unknown);
    };
    let path = format!("/api/v1/nodes/{node}/proxy/configz");
    let raw = match kubectl_capture(kubeconfig_path, &["get", "--raw", &path]) {
        Some(r) => r,
        None => return (Field::Unknown, Field::Unknown),
    };
    (
        configz_scalar(&raw, "failSwapOn"),
        configz_scalar(&raw, "swapBehavior"),
    )
}

/// Extracts a scalar kubelet-config value from the `configz` JSON blob by
/// key. Pure over the raw text — a tolerant substring scan (the blob is a
/// single-line JSON object; a full serde parse is overkill for two scalars).
fn configz_scalar(configz_json: &str, key: &str) -> Field {
    // Find `"<key>":` then take the following JSON token (bool / string).
    let needle = format!("\"{key}\":");
    let Some(idx) = configz_json.find(&needle) else {
        return Field::Unknown;
    };
    let rest = configz_json[idx + needle.len()..].trim_start();
    let token: String = rest
        .chars()
        .take_while(|c| *c != ',' && *c != '}' && !c.is_whitespace())
        .collect();
    let cleaned = token.trim_matches('"');
    if cleaned.is_empty() {
        Field::Unknown
    } else {
        Field::Known(cleaned.to_string())
    }
}

/// Reads a jsonpath field off the Node object via the cached OUT-OF-CLUSTER
/// kubeconfig — `kubectl get node <n> -o jsonpath=…` (NOT SSH, Q10). An empty
/// / `<unknown>` / `<none>` / errored result — or an absent kubeconfig /
/// unknown node name — degrades to `Unknown`.
fn probe_node_field(
    kubeconfig_path: Option<&Path>,
    node_name: Option<&str>,
    jsonpath: &str,
) -> Field {
    let Some(node) = node_name else {
        return Field::Unknown;
    };
    let jp = format!("jsonpath={jsonpath}");
    match kubectl_capture(kubeconfig_path, &["get", "node", node, "-o", &jp]) {
        Some(out) => {
            let v = out.trim();
            if v.is_empty() || v == "<unknown>" || v == "<none>" {
                Field::Unknown
            } else {
                Field::Known(v.to_string())
            }
        }
        None => Field::Unknown,
    }
}

/// Reads the effective `GOMEMLIMIT` off the running k3s unit's environment
/// (`systemctl show -p Environment k3s`). `Unknown` when absent / SSH down.
fn probe_gomemlimit(runner: &SshCommandRunner, host: &str) -> Field {
    let out = match runner.run(host, "systemctl show -p Environment k3s 2>/dev/null") {
        Ok(o) => o,
        Err(_) => return Field::Unknown,
    };
    // `Environment=GOMEMLIMIT=2GiB FOO=bar` → pull the GOMEMLIMIT token.
    for tok in out.split_whitespace() {
        if let Some(v) = tok.strip_prefix("GOMEMLIMIT=") {
            if !v.is_empty() {
                return Field::Known(v.to_string());
            }
        }
    }
    Field::Unknown
}

/// Wraps an optional command output as a `Field`, trimming; empty → `Unknown`.
fn field_or_unknown(out: Option<String>) -> Field {
    match out {
        Some(s) if !s.trim().is_empty() => Field::Known(s.trim().to_string()),
        _ => Field::Unknown,
    }
}

/// Renders the `swapon --show` output down to a one-line summary of OUR
/// `/swapfile` row (`/swapfile file 4G 0B -2`), or `Unknown` when SSH gave
/// no output / the file is not listed.
fn swapon_summary(swapon_show: Option<&str>) -> Field {
    let Some(text) = swapon_show else {
        return Field::Unknown;
    };
    for line in text.lines() {
        if line.split_whitespace().next() == Some(SWAPFILE_PATH) {
            let normalised = line.split_whitespace().collect::<Vec<_>>().join(" ");
            return Field::Known(normalised);
        }
    }
    Field::Unknown
}

/// Pulls the SIZE column (3rd) of OUR `/swapfile` row out of `swapon --show`.
/// `swapon --show` columns are `NAME TYPE SIZE USED PRIO`.
fn swapfile_size_column(swapon_show: &str) -> Option<String> {
    swapon_show.lines().find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.first() == Some(&SWAPFILE_PATH) {
            cols.get(2).map(|s| s.to_string())
        } else {
            None
        }
    })
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

    // ---- (d) node status pure renderer (design decision 6) --------------

    /// A fully-populated (SSH-up) `NodeSwapState` in a given `state`.
    fn full_state(state: SwapProvisionState) -> NodeSwapState {
        NodeSwapState {
            node_name: "node-1".into(),
            ssh_available: true,
            state,
            fail_swap_on: Field::Known("false".into()),
            swap_behavior: Field::Known("NoSwap".into()),
            swap_capacity: Field::Known("4294967296".into()),
            kubelet_version: Field::Known("v1.35.5+k3s1".into()),
            swapon: Field::Known("/swapfile file 4G 0B -2".into()),
            swappiness: Field::Known("10".into()),
            gomemlimit: Field::Known("2GiB".into()),
            provision_breadcrumb: Field::Known("applied: swap 4096 MiB".into()),
        }
    }

    #[test]
    fn render_status_labels_every_field_with_its_source() {
        let out = render_status(&full_state(SwapProvisionState::Active {
            size: Some("4G".into()),
        }));
        // kube-API fields are `[api]`, SSH fields `[ssh]`.
        assert!(out.contains("[api] kubeletVersion"), "{out}");
        assert!(out.contains("[api] failSwapOn"), "{out}");
        assert!(out.contains("[api] swapBehavior"), "{out}");
        assert!(out.contains("[api] swap.capacity"), "{out}");
        assert!(out.contains("[ssh] swapon"), "{out}");
        assert!(out.contains("[ssh] vm.swappiness"), "{out}");
        assert!(out.contains("[ssh] GOMEMLIMIT"), "{out}");
        assert!(out.contains("[ssh] provision"), "{out}");
    }

    #[test]
    fn render_status_active_states_size() {
        let out = render_status(&full_state(SwapProvisionState::Active {
            size: Some("4G".into()),
        }));
        assert!(out.contains("swap active (4G)"), "{out}");
        assert!(out.contains("NoSwap for pods"), "{out}");
    }

    #[test]
    fn render_status_skipped_by_env_names_the_hook() {
        let out = render_status(&full_state(SwapProvisionState::SkippedByEnv));
        assert!(out.contains("swap skipped by env"), "{out}");
        assert!(out.contains(SKIP_NODE_SWAP_ENV), "{out}");
    }

    #[test]
    fn render_status_ineligible_tells_upgrade() {
        let out = render_status(&full_state(SwapProvisionState::Ineligible {
            detail: "k8s <1.34".into(),
        }));
        assert!(out.contains("ineligible"), "{out}");
        assert!(out.contains("1.34"), "{out}");
        assert!(
            out.to_lowercase().contains("upgrade"),
            "ineligible verdict must tell the user to UPGRADE k3s: {out}"
        );
    }

    #[test]
    fn render_status_eligible_not_applied_points_at_node_prep() {
        let out = render_status(&full_state(SwapProvisionState::EligibleNotApplied));
        assert!(out.contains("eligible, not applied"), "{out}");
        assert!(out.contains("apprafter node prep"), "{out}");
    }

    #[test]
    fn render_status_provision_failed_verdict() {
        let out = render_status(&full_state(SwapProvisionState::ProvisionFailed {
            detail: "swap step errored at provision".into(),
        }));
        assert!(out.contains("swap step failed at provision"), "{out}");
        assert!(out.contains("cushionless"), "{out}");
    }

    #[test]
    fn render_status_orphan_verdict() {
        let out = render_status(&full_state(SwapProvisionState::OrphanSwapfile));
        assert!(out.to_lowercase().contains("orphan"), "{out}");
        assert!(out.contains("/swapfile"), "{out}");
    }

    #[test]
    fn render_status_degrades_when_ssh_unavailable() {
        // SSH down: the [api] fields still render (they are Known), every
        // [ssh] field is Unknown → `unknown`, and a banner flags it.
        let s = NodeSwapState {
            node_name: "node-1".into(),
            ssh_available: false,
            state: SwapProvisionState::Unknown,
            fail_swap_on: Field::Known("false".into()),
            swap_behavior: Field::Known("NoSwap".into()),
            swap_capacity: Field::Known("4294967296".into()),
            kubelet_version: Field::Known("v1.35.5+k3s1".into()),
            swapon: Field::Unknown,
            swappiness: Field::Unknown,
            gomemlimit: Field::Unknown,
            provision_breadcrumb: Field::Unknown,
        };
        let out = render_status(&s);
        // Degradation banner present.
        assert!(
            out.contains("SSH unavailable"),
            "a degraded result must banner that SSH was unavailable: {out}"
        );
        // [api] fields still authoritative.
        assert!(out.contains("v1.35.5+k3s1"), "{out}");
        assert!(out.contains("[api] failSwapOn     : false"), "{out}");
        // Every [ssh] field renders `unknown`.
        assert!(out.contains("[ssh] swapon         : unknown"), "{out}");
        assert!(out.contains("[ssh] vm.swappiness  : unknown"), "{out}");
        assert!(out.contains("[ssh] GOMEMLIMIT     : unknown"), "{out}");
        assert!(out.contains("[ssh] provision      : unknown"), "{out}");
    }

    // ---- (e) status classifier + probe helpers --------------------------

    #[test]
    fn classify_active_beats_breadcrumb() {
        // A live /swapfile is Active even if the breadcrumb said ineligible.
        let st = classify_state(
            true,
            true,
            Some("4G".into()),
            false,
            &Field::Known("ineligible: k3s v1.33 < 1.34".into()),
            Some(true),
        );
        assert_eq!(
            st,
            SwapProvisionState::Active {
                size: Some("4G".into())
            }
        );
    }

    #[test]
    fn classify_breadcrumb_ineligible_and_failed_and_skipped() {
        assert_eq!(
            classify_state(
                true,
                false,
                None,
                false,
                &Field::Known("ineligible: cgroup tmpfs != cgroup2fs".into()),
                None,
            ),
            SwapProvisionState::Ineligible {
                detail: "cgroup tmpfs != cgroup2fs".into()
            }
        );
        assert_eq!(
            classify_state(
                true,
                false,
                None,
                false,
                &Field::Known("failed: swap step errored at provision".into()),
                Some(true),
            ),
            SwapProvisionState::ProvisionFailed {
                detail: "swap step errored at provision".into()
            }
        );
        assert_eq!(
            classify_state(
                true,
                false,
                None,
                false,
                &Field::Known(format!("skipped: {SKIP_NODE_SWAP_ENV} set")),
                Some(true),
            ),
            SwapProvisionState::SkippedByEnv
        );
    }

    #[test]
    fn classify_eligible_not_applied_and_orphan_and_unknown() {
        // No breadcrumb, eligible per api, not active, no orphan → actionable.
        assert_eq!(
            classify_state(true, false, None, false, &Field::Unknown, Some(true)),
            SwapProvisionState::EligibleNotApplied
        );
        // Orphan on disk → orphan verdict.
        assert_eq!(
            classify_state(true, false, None, true, &Field::Unknown, None),
            SwapProvisionState::OrphanSwapfile
        );
        // SSH down and no signal at all → Unknown.
        assert_eq!(
            classify_state(false, false, None, false, &Field::Unknown, None),
            SwapProvisionState::Unknown
        );
    }

    #[test]
    fn api_fields_unknown_without_kubeconfig_never_spawn() {
        // With no cached kubeconfig the kube-API probes short-circuit to
        // `Unknown` WITHOUT spawning kubectl (Q10: the api fields come from the
        // out-of-cluster kubeconfig, so their absence — not SSH — is what makes
        // them unknown). `None` node name likewise degrades independently.
        assert_eq!(
            probe_configz(None, Some("node-1")),
            (Field::Unknown, Field::Unknown)
        );
        assert_eq!(probe_configz(None, None), (Field::Unknown, Field::Unknown));
        assert_eq!(
            probe_node_field(None, Some("node-1"), "{.status.nodeInfo.kubeletVersion}"),
            Field::Unknown
        );
        assert_eq!(
            probe_node_field(None, None, "{.status.nodeInfo.swap.capacity}"),
            Field::Unknown
        );
    }

    #[test]
    fn render_api_known_ssh_unknown_shows_api_values_not_all_unknown() {
        // Q10 core: when the kube-API fields (read via the cached out-of-cluster
        // kubeconfig) are KNOWN but SSH is DOWN, the report renders the api
        // values — SSH being down no longer forces the api fields unknown, so
        // the result is NOT all-unknown. This mirrors the shape
        // `probe_node_swap_state` assembles when the kubeconfig answered but the
        // SSH probe did not connect.
        let s = NodeSwapState {
            node_name: "node-1".into(),
            ssh_available: false,
            state: SwapProvisionState::Active {
                size: Some("4G".into()),
            },
            fail_swap_on: Field::Known("false".into()),
            swap_behavior: Field::Known("NoSwap".into()),
            swap_capacity: Field::Known("4294967296".into()),
            kubelet_version: Field::Known("v1.35.5+k3s1".into()),
            swapon: Field::Unknown,
            swappiness: Field::Unknown,
            gomemlimit: Field::Unknown,
            provision_breadcrumb: Field::Unknown,
        };
        let out = render_status(&s);
        // The [api] fields render their real values (NOT `unknown`).
        assert!(out.contains("[api] kubeletVersion : v1.35.5+k3s1"), "{out}");
        assert!(out.contains("[api] failSwapOn     : false"), "{out}");
        assert!(out.contains("[api] swapBehavior   : NoSwap"), "{out}");
        assert!(out.contains("[api] swap.capacity  : 4294967296"), "{out}");
        // No [api] line degraded to `unknown` (the whole point of Q10).
        assert!(
            !out.contains("[api] kubeletVersion : unknown"),
            "api kubeletVersion must NOT be unknown when SSH is down: {out}"
        );
        // The [ssh] fields, by contrast, ARE unknown (SSH genuinely down).
        assert!(out.contains("[ssh] swapon         : unknown"), "{out}");
        assert!(out.contains("SSH unavailable"), "{out}");
    }

    #[test]
    fn configz_scalar_extracts_bool_and_string() {
        let json = r#"{"kubeletconfig":{"failSwapOn":false,"swapBehavior":"NoSwap","x":1}}"#;
        assert_eq!(
            configz_scalar(json, "failSwapOn"),
            Field::Known("false".into())
        );
        assert_eq!(
            configz_scalar(json, "swapBehavior"),
            Field::Known("NoSwap".into())
        );
        assert_eq!(configz_scalar(json, "missing"), Field::Unknown);
    }

    #[test]
    fn swapon_summary_and_size_pick_our_row() {
        let show = "NAME      TYPE SIZE USED PRIO\n/swapfile file 4G   0B   -2\n";
        assert_eq!(
            swapon_summary(Some(show)),
            Field::Known("/swapfile file 4G 0B -2".into())
        );
        assert_eq!(swapfile_size_column(show), Some("4G".into()));
        // No SSH output → Unknown.
        assert_eq!(swapon_summary(None), Field::Unknown);
        // A different swap area is not our row.
        let other = "NAME TYPE SIZE\n/dev/sda2 partition 2G\n";
        assert_eq!(swapon_summary(Some(other)), Field::Unknown);
        assert_eq!(swapfile_size_column(other), None);
    }

    #[test]
    fn field_or_unknown_trims_and_maps_empty() {
        assert_eq!(
            field_or_unknown(Some("  10 \n".into())),
            Field::Known("10".into())
        );
        assert_eq!(field_or_unknown(Some("   ".into())), Field::Unknown);
        assert_eq!(field_or_unknown(None), Field::Unknown);
    }
}
