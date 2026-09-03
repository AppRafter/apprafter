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

/// Retrofit swap-size cap in MiB — mirrors the private `SWAP_MAX_MIB` in
/// `cli_providers::hetzner_cloud::user_data` that [`swap_enable_script`]
/// applies internally (`min(MemTotal_MiB, 8Gi)`). We recompute the same count
/// here only for the human-readable retrofit breadcrumb (Fix 2); the actual
/// swapfile size is still owned by `swap_enable_script`.
const SWAP_MAX_MIB: u64 = 8192;

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

/// Builds the shell that OVERWRITES the [`SWAP_PROVISION_STATUS_PATH`]
/// breadcrumb with `state` for the RETROFIT path (`apprafter node prep`).
/// The bootstrap/cloud-init path drops this breadcrumb itself
/// ([`build_bootstrap_swap_script`]), but the retrofit never did — so a
/// retrofitted node reported `provision: unknown` on `node status`.
///
/// Mirrors the bootstrap breadcrumb writes: ensure the dir, then a single
/// `echo … > path`, and BEST-EFFORT (`2>/dev/null || true`) so a breadcrumb
/// failure never fails the prep. Pure so the shape is unit-testable.
pub fn provision_breadcrumb_write_script(state: &str) -> String {
    format!(
        "mkdir -p /var/lib/apprafter 2>/dev/null || true; \
         echo \"{state}\" > {SWAP_PROVISION_STATUS_PATH} 2>/dev/null || true"
    )
}

// ===========================================================================
// Swap apply — the drop-in write (Option A) + shared swap steps.
// ===========================================================================

/// Undocumented fault-injection hook: `APPRAFTER_NODE_SWAP_FORCE_INVALID_DROPIN`
/// (set to ANY value, including empty). When present, the apply path writes a
/// syntactically-INVALID `KubeletConfiguration` drop-in instead of the valid
/// one, so the k3s restart FAILS → the `/readyz` poll times out → the existing
/// whole-step rollback fires. It exists ONLY to make the walk's STEP 6 (the
/// rollback safety net) executable on a live node (design decision 5 / Q1); it
/// is intentionally NOT surfaced in `--help`, exactly like [`SKIP_NODE_SWAP_ENV`].
pub const FORCE_INVALID_DROPIN_ENV: &str = "APPRAFTER_NODE_SWAP_FORCE_INVALID_DROPIN";

/// `true` when the [`FORCE_INVALID_DROPIN_ENV`] fault hook is set (any value).
fn force_invalid_dropin() -> bool {
    std::env::var_os(FORCE_INVALID_DROPIN_ENV).is_some()
}

/// A deliberately-BROKEN kubelet drop-in body used ONLY by the
/// [`FORCE_INVALID_DROPIN_ENV`] fault hook. The `apiVersion` is bogus so the
/// kubelet rejects the config and k3s fails to come back up — driving the
/// `/readyz` timeout that the whole-step rollback recovers from. It keeps the
/// `kind: KubeletConfiguration` so it lands in the same code path, but its
/// group/version is nonexistent and it carries an unparseable field.
fn invalid_swap_kubelet_dropin() -> String {
    // A nonexistent apiVersion group + a field that is not a valid kubelet
    // config key — the kubelet's strict decoder refuses both.
    "apiVersion: apprafter.invalid/v0\n\
     kind: KubeletConfiguration\n\
     thisFieldDoesNotExist: {{{not-yaml\n"
        .to_string()
}

/// The drop-in body the apply path writes — the valid
/// [`swap_kubelet_dropin`], UNLESS `force_invalid` forces the invalid body
/// (walk STEP 6, Q1). Pure over its `force_invalid` input so the fault path is
/// unit-testable without touching the environment.
fn swap_dropin_body(k8s_ge_134: bool, force_invalid: bool) -> String {
    if force_invalid {
        invalid_swap_kubelet_dropin()
    } else {
        swap_kubelet_dropin(k8s_ge_134)
    }
}

/// Wraps a drop-in `body` in the Option-A write script (mkdir + quoted-heredoc
/// `cat >` into [`SWAP_KUBELET_DROPIN_PATH`]).
fn dropin_write_script_for_body(body: &str) -> String {
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

/// Builds the remote script that writes the VALID Option-A kubelet swap drop-in
/// (design decision 2). `failSwapOn:false` is UNCONDITIONAL; `swapBehavior:
/// NoSwap` only on `k8s_ge_134`. Written FIRST in the apply order so a
/// swap-active-in-fstab / kubelet-refuses-start brick window never opens.
///
/// This does NOT swapon anything itself — it only lands the drop-in file. It is
/// ALWAYS the valid body — the [`FORCE_INVALID_DROPIN_ENV`] fault hook only
/// perturbs the INITIAL apply write ([`swap_dropin_write_script_apply`]); the
/// rollback's rewrite MUST stay valid so recovery works, so it goes through
/// this builder.
pub fn swap_dropin_write_script(k8s_ge_134: bool) -> String {
    dropin_write_script_for_body(&swap_dropin_body(k8s_ge_134, false))
}

/// The INITIAL-apply drop-in write. Identical to [`swap_dropin_write_script`]
/// EXCEPT that it honours the undocumented [`FORCE_INVALID_DROPIN_ENV`] fault
/// hook: when set, it writes a deliberately-invalid body so the k3s restart
/// fails and the whole-step rollback fires (walk STEP 6 rollback e2e, Q1). Only
/// the first write of the apply path uses this — the rollback rewrite does not.
pub fn swap_dropin_write_script_apply(k8s_ge_134: bool) -> String {
    dropin_write_script_for_body(&swap_dropin_body(k8s_ge_134, force_invalid_dropin()))
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

/// The single `daemon-reload && restart k3s` the umbrella (and the rollback)
/// issue. One const so the retrofit and the rollback can never drift into
/// restarting k3s two different ways.
const RESTART_K3S_COMMAND: &str = "systemctl daemon-reload && systemctl restart k3s";

/// Remote command that deletes the kubelet swap drop-in ENTIRELY — this is
/// what removes the last `failSwapOn:false`, so it is only ever issued after
/// a successful `swapoff`. Extracted from [`SshNodeOps`] (whose methods are
/// pure SSH IO) so the command shape is testable without a node.
fn remove_dropin_command() -> String {
    format!("rm -f {SWAP_KUBELET_DROPIN_PATH}")
}

/// Remote `swapoff`, CAPPED BY `timeout` (design decision 5 / P8): `swapoff`
/// can hang or `ENOMEM` under the very memory pressure that triggered the
/// rollback, and an unbounded hang would strand the rollback mid-flight. The
/// cap makes the failure surface as a non-zero exit (124) instead.
/// Extracted from [`SshNodeOps`] so the cap is unit-testable.
fn swapoff_command() -> String {
    format!("timeout 60 swapoff {SWAPFILE_PATH}")
}

/// Remote command that un-persists swap: delete the exact fstab line + the
/// swappiness drop-in. Extracted from [`SshNodeOps`] so the two halves — the
/// fstab line MUST go (a surviving `sw,nofail` silently reactivates swap on
/// the next boot, undoing the rollback) and the sysctl removal must tolerate
/// an absent file — are pinned without a node.
fn remove_fstab_and_sysctl_command() -> String {
    // `sed -i` with an anchored pattern; `rm -f` tolerates an absent file.
    format!(
        "sed -i '\\#^{SWAPFILE_PATH} #d' /etc/fstab; \
         rm -f /etc/sysctl.d/99-apprafter-swap.conf"
    )
}

/// Remote command that deletes the swapfile itself.
fn remove_swapfile_command() -> String {
    format!("rm -f {SWAPFILE_PATH}")
}

/// Remote command that writes `message` into the NODE's journal. Extracted
/// from [`SshNodeOps::emit_runbook`] so the quoting is testable: the runbook
/// text carries backticks, quotes and slashes, and an unescaped `'` would
/// terminate the argument early and truncate (or worse, split) the line the
/// operator is meant to read.
fn logger_command(message: &str) -> String {
    format!(
        "logger -t apprafter-node-prep {}",
        shell_single_quote(message)
    )
}

impl NodeOps for SshNodeOps<'_> {
    fn write_swap_dropin(&mut self, k8s_ge_134: bool) -> Result<()> {
        self.runner
            .run(self.host, &swap_dropin_write_script(k8s_ge_134))
            .map(|_| ())
    }

    fn remove_swap_dropin(&mut self) -> Result<()> {
        self.runner
            .run(self.host, &remove_dropin_command())
            .map(|_| ())
    }

    fn swapoff_with_timeout(&mut self) -> Result<()> {
        self.runner.run(self.host, &swapoff_command()).map(|_| ())
    }

    fn remove_fstab_and_sysctl(&mut self) -> Result<()> {
        self.runner
            .run(self.host, &remove_fstab_and_sysctl_command())
            .map(|_| ())
    }

    fn restart_k3s_and_wait(&mut self) -> Result<()> {
        self.runner.run(self.host, RESTART_K3S_COMMAND)?;
        wait_for_recovery(self.runner, self.host)
    }

    fn remove_swapfile(&mut self) -> Result<()> {
        self.runner
            .run(self.host, &remove_swapfile_command())
            .map(|_| ())
    }

    fn emit_runbook(&mut self, message: &str) -> Result<()> {
        // Echo the runbook line locally AND onto the NODE's journal (so the
        // operator finds it) — the node write is best-effort.
        eprintln!("{message}");
        let _ = self.runner.run(self.host, &logger_command(message));
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
/// The error both `node prep` and `node status` raise when the active target
/// has no provisioned server. Extracted (and shared by the two IO entries,
/// which carried byte-identical copies) so the remedy stays one string.
fn no_provisioned_server_error() -> CliError {
    CliError::Other("no provisioned server for the active target — run `apprafter up` first".into())
}

/// The error both `node prep` and `node status` raise when the node exists but
/// has not been given a public IPv4 yet. Shared for the same reason as
/// [`no_provisioned_server_error`].
fn no_public_ipv4_error() -> CliError {
    CliError::Other("the active target's node has no public IPv4 yet — wait for cloud-init".into())
}

/// The refusal `node prep` raises when it would have to prompt but stdin is
/// not a TTY. Extracted so the remedy — the flag that makes the command
/// scriptable — is pinned; a prompt written to a non-TTY would hang CI.
fn non_interactive_error() -> CliError {
    CliError::Other("non-interactive shell — pass `--yes` to skip the confirmation prompt".into())
}

/// The consent text shown before the (disruptive) k3s restart. Extracted so
/// the disclosure is testable: this is the operator's ONLY warning that the
/// API goes away for ~30s, and it must name the host it is about to touch —
/// with several targets configured, an unnamed prompt is a footgun.
fn prep_confirmation_message(host: &str) -> String {
    format!(
        "This restarts k3s on {host} (~30s, API briefly unavailable) to apply node \
         reservations (system-reserved=1500Mi, kube-reserved, eviction-hard) and — when the \
         node is eligible (k8s ≥1.34 + cgroup v2) — provision host swap (NoSwap for pods)."
    )
}

/// The error raised when the gate REFUSED the swap step. Design decision 1 /
/// N6: the reservations DID apply, but the skip is surfaced as a non-zero exit
/// carrying the gate's hint — never a silent success, which would let an
/// operator believe swap is on when it is not.
fn swap_step_skipped_error(hint: &str) -> CliError {
    CliError::Other(format!("swap step skipped: {hint}"))
}

pub fn node_prep(yes: bool) -> Result<()> {
    info!(yes, "node prep invoked");

    let resolved = resolve_state_paths(None)?;
    let paths = resolved.paths;
    let store = resolved.store;
    let state = State::load_or_default(&paths)?;

    let Some(server_id) = state.hetzner_cloud.as_ref().map(|h| h.server_id) else {
        return Err(no_provisioned_server_error());
    };

    let token = resolve_hetzner_token(None, &store, None)?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);
    let (v4, _v6) = node_public_ips(&client, server_id)?;
    let host = v4.ok_or_else(no_public_ipv4_error)?;

    if !yes {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Err(non_interactive_error());
        }
        println!("{}", prep_confirmation_message(&host));
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
            Err(swap_step_skipped_error(&hint))
        }
        SwapGate::Eligible { k8s_ge_134 } => apply_eligible(&runner, &host, k8s_ge_134),
    }
}

/// What the apply path must do about whatever swap state is already on the
/// node (design decision D2 / Q11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreApplyPlan {
    /// Swap is active AND persisted in fstab — a re-run is a near no-op; the
    /// reservations + drop-in are still refreshed (cheap, idempotent).
    AlreadyProvisioned,
    /// `/swapfile` is an orphan remnant — remove it before re-provisioning so
    /// the fresh `dd` starts clean rather than writing over a live-but-
    /// unlisted file.
    RemoveOrphan,
    /// Nothing in the way — provision from scratch.
    FreshProvision,
}

/// Decides the [`PreApplyPlan`] from the three probed facts. Extracted from
/// [`apply_eligible`] (which is otherwise SSH IO) so the ORDER is pinned:
/// `AlreadyProvisioned` is checked FIRST, because an active-and-persisted
/// swapfile also satisfies "exists on disk", and misreading it as an orphan
/// would `swapoff` + `rm` a perfectly healthy node's live swap.
fn pre_apply_plan(swapon_show: &str, fstab: &str, swapfile_exists: bool) -> PreApplyPlan {
    if swap_already_active(swapon_show) && fstab_has_swap_entry(fstab) {
        PreApplyPlan::AlreadyProvisioned
    } else if orphan_swapfile(swapfile_exists, swapon_show, fstab) {
        PreApplyPlan::RemoveOrphan
    } else {
        PreApplyPlan::FreshProvision
    }
}

/// The remote command that clears an orphan swapfile. The `swapoff` is
/// BEST-EFFORT (`2>/dev/null`, no `&&`) because an orphan is by definition not
/// active — the `rm` must still run when `swapoff` errors, or the orphan
/// survives and the fresh `dd` writes over it.
fn orphan_cleanup_command() -> String {
    format!("swapoff {SWAPFILE_PATH} 2>/dev/null; rm -f {SWAPFILE_PATH}")
}

/// The line shown when a re-run finds the node already fully provisioned. It
/// must say the run is a REFRESH, not a fresh provision — an operator who sees
/// the generic "applying…" line on a healthy node has no way to tell that the
/// idempotency probes fired.
fn already_provisioned_notice(host: &str) -> String {
    format!("Swap already active and persisted; refreshing reservations + drop-in on {host}…")
}

/// The warning shown before an orphan is removed. Extracted so the notice is
/// testable: removing a file on the operator's node must never be silent, and
/// the line has to say WHICH file on WHICH host.
fn orphan_notice(host: &str) -> String {
    format!(
        "⚠ Found an orphan {SWAPFILE_PATH} on {host} (present but not active and not in \
         fstab) — removing it before a clean re-provision."
    )
}

/// The retrofit breadcrumb's swap size in MiB — mirrors the `min(MemTotal_MiB,
/// 8Gi)` cap [`swap_enable_script`] applies internally (Fix 2). Extracted from
/// [`apply_eligible`] so the cap is pinned: `node status` reads this number
/// back, and an uncapped one would claim a 32 GiB swapfile on a 32 GiB node
/// that actually only got 8 GiB.
fn retrofit_swap_count_mib(mem_total_kib: u64) -> u64 {
    (mem_total_kib / 1024).min(SWAP_MAX_MIB)
}

/// The eligible apply path: write reservations + the swap drop-in + the
/// swap steps, batched into ONE `daemon-reload && restart k3s`, then poll
/// `/readyz`; on timeout, run the whole-step [`rollback`].
fn apply_eligible(runner: &SshCommandRunner, host: &str, k8s_ge_134: bool) -> Result<()> {
    // --- Idempotency probes (design decision D2 / Q11). ---
    let swapon_show = runner.run(host, SWAPON_SHOW_COMMAND).unwrap_or_default();
    let fstab = runner.run(host, READ_FSTAB_COMMAND).unwrap_or_default();
    let swapfile_exists = runner.run(host, &swapfile_exists_command()).is_ok();

    match pre_apply_plan(&swapon_show, &fstab, swapfile_exists) {
        PreApplyPlan::AlreadyProvisioned => {
            println!("{}", already_provisioned_notice(host));
        }
        PreApplyPlan::RemoveOrphan => {
            println!("{}", orphan_notice(host));
            runner.run(host, &orphan_cleanup_command())?;
        }
        PreApplyPlan::FreshProvision => {}
    }

    println!("Applying node reservations + host swap on {host} over SSH…");

    // (1) Reservations (files only — the umbrella owns the single restart).
    runner.run(host, &reservation_files_script())?;
    // (2) Swap drop-in FIRST (failSwapOn:false unconditional; NoSwap ≥1.34).
    //     The `_apply` variant honours the undocumented FORCE_INVALID_DROPIN_ENV
    //     fault hook (walk STEP 6 rollback e2e); the rollback rewrite below
    //     stays valid so recovery works.
    runner.run(host, &swap_dropin_write_script_apply(k8s_ge_134))?;
    // (3) The shared swap steps (swappiness → dd → mkswap → swapon → inline
    //     cgroup swap.max=0 → fstab). Read MemTotal off the node first so
    //     the shared retrofit builder can pin the count.
    let mem_total_kib = probe_mem_total_kib(runner, host)?;
    // The retrofit breadcrumb size mirrors `swap_enable_script`'s internal
    // `min(MemTotal_MiB, 8Gi)` count (Fix 2) — recomputed here only for the
    // human-readable status line, not to drive the swapfile size.
    let swap_count_mib = retrofit_swap_count_mib(mem_total_kib);
    let swap_steps = swap_enable_script(
        mem_total_kib,
        k8s_ge_134,
        true, // cgroup2 already gated above
    )
    .ok_or_else(swap_builder_none_error)?;
    runner.run(host, &swap_steps)?;

    // (4) Single daemon-reload && restart k3s → /readyz poll.
    println!("k3s restarting to pick up the swap drop-in. Waiting for the API to recover…");
    runner.run(host, RESTART_K3S_COMMAND)?;

    match wait_for_recovery(runner, host) {
        Ok(()) => {
            println!("✓ Node reservations + host swap applied and the k3s API is back.");
            println!("  swap active (NoSwap for pods), swappiness=10, fstab sw,nofail.");
            // Drop the retrofit provision breadcrumb so `node status` shows
            // `applied (retrofit)` instead of `unknown` (Fix 2). Best-effort.
            let _ = runner.run(
                host,
                &provision_breadcrumb_write_script(&applied_breadcrumb_state(swap_count_mib)),
            );
            Ok(())
        }
        Err(recovery_err) => {
            // ATOMIC: the k3s API did not come back → the whole swap step is
            // rolled back (design decision 5), `failSwapOn:false` removed LAST.
            eprintln!("{}", recovery_failed_notice(&recovery_err.to_string()));
            let mut ops = SshNodeOps::new(runner, host);
            match rollback(&mut ops)? {
                RollbackOutcome::Recovered => {
                    // Overwrite the breadcrumb so a rolled-back node does not
                    // show a stale `applied` (Fix 2). Best-effort.
                    let _ = runner.run(
                        host,
                        &provision_breadcrumb_write_script(ROLLED_BACK_BREADCRUMB),
                    );
                    Err(rollback_recovered_error(&recovery_err.to_string()))
                }
                RollbackOutcome::SwapoffFailedSwapLeft => {
                    // Swap could NOT be turned off — the breadcrumb must say so
                    // (consistent with the runbook: swap left active) (Fix 2).
                    let _ = runner.run(
                        host,
                        &provision_breadcrumb_write_script(ROLLED_BACK_PARTIAL_BREADCRUMB),
                    );
                    Err(rollback_partial_error(&recovery_err.to_string()))
                }
            }
        }
    }
}

/// The error raised if the shared swap builder declines AFTER the eligibility
/// gate passed. That combination is a contradiction between two pieces of the
/// same decision, so it is labelled `internal:` — it is a bug report, not
/// something an operator can act on.
fn swap_builder_none_error() -> CliError {
    CliError::Other(
        "internal: swap builder returned None despite passing the eligibility gate".into(),
    )
}

/// The breadcrumb an APPLIED retrofit leaves. `node status` classifies off
/// this line, so the `applied` prefix is load-bearing.
fn applied_breadcrumb_state(swap_count_mib: u64) -> String {
    format!("applied (retrofit): swap {swap_count_mib} MiB")
}

/// The breadcrumb a CLEANLY rolled-back node is left with. It must NOT read as
/// an `applied…` state — `node status` would otherwise report swap as provisioned
/// on a node that has none.
const ROLLED_BACK_BREADCRUMB: &str = "rolled-back: swap removed";

/// The breadcrumb a PARTIALLY rolled-back node is left with (`swapoff` failed,
/// swap is still on). Distinct from [`ROLLED_BACK_BREADCRUMB`] because the two
/// need different operator follow-up.
const ROLLED_BACK_PARTIAL_BREADCRUMB: &str = "rolled-back-partial: swap left active";

/// The stderr notice printed the moment recovery fails, BEFORE the rollback
/// runs. Extracted so it is pinned that the operator is told the rollback is
/// starting — a silent multi-minute rollback looks like a hang on the exact
/// command that just took their API away.
fn recovery_failed_notice(recovery_err: &str) -> String {
    format!(
        "✗ k3s did not recover after the swap apply ({recovery_err}). \
         Rolling the whole swap step back…"
    )
}

/// The error returned after a CLEAN whole-step rollback. It reports what was
/// undone AND keeps the original recovery error — the rollback succeeding does
/// not make the apply a success, so this is still an `Err`.
fn rollback_recovered_error(recovery_err: &str) -> CliError {
    CliError::Other(format!(
        "swap apply failed and was rolled back cleanly (swap removed, kubelet \
         drop-in removed, /swapfile deleted, k3s recovered). Original error: \
         {recovery_err}"
    ))
}

/// The error returned when the rollback could NOT `swapoff`. It must say that
/// swap and `failSwapOn:false` were LEFT in place and point at the runbook
/// line — the node is in a deliberately-not-cleaned state and the operator has
/// manual work to do.
fn rollback_partial_error(recovery_err: &str) -> CliError {
    CliError::Other(format!(
        "swap apply failed; rollback could NOT swapoff (swap + failSwapOn:false \
         left in place, /swapfile kept — see the runbook line above). Original \
         error: {recovery_err}"
    ))
}

/// Reads the node's kubelet version via `kubectl get node … jsonpath`. The
/// node-local `k3s kubectl` needs no external creds. Design decision 1 (P3):
/// the kubeletVersion (not `configz`) answers "would swapBehavior be
/// accepted".
fn probe_kubelet_version(runner: &SshCommandRunner, host: &str) -> Result<String> {
    // `-o jsonpath` over the single node. `$(hostname)` resolves the node
    // name on the node itself; k3s uses the hostname as the node name.
    let out = runner.run(host, KUBELET_VERSION_PROBE_COMMAND)?;
    parse_kubelet_version_output(&out)
}

/// The node-local command that reads the kubelet version. `$(hostname)` is the
/// k3s node name, resolved ON the node, and `k3s kubectl` is cluster-admin
/// there — so this probe needs no external credentials.
const KUBELET_VERSION_PROBE_COMMAND: &str = "k3s kubectl get node \"$(hostname)\" \
     -o jsonpath='{.status.nodeInfo.kubeletVersion}'";

/// Turns the kubelet-version jsonpath output into the version string.
/// Extracted from [`probe_kubelet_version`] so the EMPTY case is testable: a
/// `jsonpath` miss exits ZERO with empty stdout, and an empty version would
/// flow into [`k8s_ge_134`] as `false` and silently REFUSE the swap step with
/// a nonsense "<1.34" hint instead of reporting that the probe failed.
fn parse_kubelet_version_output(out: &str) -> Result<String> {
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
    parse_mem_total_kib(&out)
}

/// Parses the `awk` MemTotal output into KiB. Extracted from
/// [`probe_mem_total_kib`] so the failure is testable and LOUD: a
/// silently-zero MemTotal would size the swapfile at 0 MiB, and the error must
/// quote what was actually read so an operator can see what the node said.
fn parse_mem_total_kib(out: &str) -> Result<u64> {
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
    wait_for_recovery_with(
        || runner.run(host, recovery_probe_command()),
        RECOVERY_TIMEOUT,
        RECOVERY_INTERVAL,
    )
}

/// The recovery poll loop, over an injectable probe + clock budget. Extracted
/// from [`wait_for_recovery`] so the loop is unit-testable in milliseconds
/// instead of the 180s the real one budgets. Three things it pins:
///
/// - A probe that succeeds on the FIRST attempt returns immediately and never
///   sleeps — the happy path must not pay the poll interval.
/// - A probe that succeeds LATER still returns `Ok`; a transient failure while
///   k3s is coming back is expected, not fatal.
/// - The deadline is checked AFTER a failed probe, so the node is always
///   probed at least once, and the timeout error carries the attempt count and
///   the LAST probe error (the only diagnostic an operator gets for a node
///   that never came back).
fn wait_for_recovery_with<F>(mut probe: F, timeout: Duration, interval: Duration) -> Result<()>
where
    F: FnMut() -> Result<String>,
{
    let deadline = Instant::now() + timeout;
    let mut last_err;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match probe() {
            Ok(_) => return Ok(()),
            Err(e) => last_err = e.to_string(),
        }
        if Instant::now() >= deadline {
            return Err(recovery_timeout_error(timeout, attempt, &last_err));
        }
        std::thread::sleep(interval);
    }
}

/// The timeout error [`wait_for_recovery_with`] returns.
fn recovery_timeout_error(timeout: Duration, attempts: u32, last_err: &str) -> CliError {
    CliError::Other(format!(
        "k3s API did not recover within {}s after restart ({attempts} attempts). \
         Last probe error: {last_err}",
        timeout.as_secs()
    ))
}

/// Remote command that succeeds once BOTH the k3s apiserver AND the kubelet
/// are serving again. `/readyz` alone is only the APISERVER's readiness — in
/// k3s it stays green even when the kubelet rejected the swap drop-in (the
/// exact failure the whole-step rollback guards against: a bad drop-in bricks
/// the kubelet while the control plane keeps serving `/readyz`). So ALSO
/// require the kubelet to answer through the apiserver node-proxy: on a bad
/// drop-in the kubelet never serves `configz`, this fails for the whole
/// [`RECOVERY_TIMEOUT`], and [`wait_for_recovery`] returns `Err` → the whole-
/// step rollback fires. Walk STEP6 proved `/readyz`-only false-reported
/// "recovered" while the kubelet was down and NO rollback ran. `$(hostname)`
/// is the k3s node name (resolved on the node); the node-local `k3s kubectl`
/// is cluster-admin so `nodes/proxy` is permitted.
fn recovery_probe_command() -> &'static str {
    "k3s kubectl get --raw='/readyz' >/dev/null && \
     k3s kubectl get --raw=\"/api/v1/nodes/$(hostname)/proxy/configz\" >/dev/null"
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
        return Err(no_provisioned_server_error());
    };

    let token = resolve_hetzner_token(None, &store, None)?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);
    let (v4, _v6) = node_public_ips(&client, server_id)?;
    let host = v4.ok_or_else(no_public_ipv4_error)?;

    let runner = SshCommandRunner::new(default_ssh_identity_path(), paths.known_hosts_file());

    // Resolve the cached (age-encrypted) out-of-cluster kubeconfig for the
    // active target — the kube-API fields read through it, so SSH being down
    // no longer forces them `Unknown` (Q10). Best-effort: a missing cache
    // just leaves the `[api]` fields `Unknown`, it is not a hard error.
    let kubeconfig = ensure_kubeconfig_tempfile().ok();

    let shell = SshShell::new(&runner, &host);
    let node_state = probe_node_swap_state(&shell, kubeconfig.as_ref().map(|f| f.path()));
    print!("{}", render_status(&node_state));
    Ok(())
}

/// The remote-command seam for the READ-ONLY `node status` probes: one method,
/// "run this on the node". Introduced so the whole probe pass
/// ([`probe_node_swap_state`]) is exercisable against a scripted mock instead
/// of a live SSH session — the graceful-degradation contract (SSH down must
/// degrade the `[ssh]` fields and nothing else) has no other cheap test.
pub trait RemoteShell {
    /// Run `command` on the node, returning its stdout.
    fn run_remote(&self, command: &str) -> Result<String>;
}

/// The production [`RemoteShell`]: an SSH session to one host.
pub struct SshShell<'a> {
    runner: &'a SshCommandRunner,
    host: &'a str,
}

impl<'a> SshShell<'a> {
    pub fn new(runner: &'a SshCommandRunner, host: &'a str) -> Self {
        Self { runner, host }
    }
}

impl RemoteShell for SshShell<'_> {
    fn run_remote(&self, command: &str) -> Result<String> {
        self.runner.run(self.host, command)
    }
}

/// `hostname` — doubles as the SSH-reachability probe and the node-name
/// fallback for the report header.
const HOSTNAME_PROBE_COMMAND: &str = "hostname";
/// The live swap listing every idempotency predicate reads.
const SWAPON_SHOW_COMMAND: &str = "swapon --show";
/// The fstab read that decides whether swap is PERSISTED.
const READ_FSTAB_COMMAND: &str = "cat /etc/fstab";
/// The swappiness sysctl read.
const SWAPPINESS_PROBE_COMMAND: &str = "sysctl -n vm.swappiness";
/// The k3s unit environment read that carries `GOMEMLIMIT`.
const GOMEMLIMIT_PROBE_COMMAND: &str = "systemctl show -p Environment k3s 2>/dev/null";

/// `test -e /swapfile` — the on-disk existence probe. Shared by the apply path
/// and the status path so the two can never drift onto different files.
fn swapfile_exists_command() -> String {
    format!("test -e {SWAPFILE_PATH}")
}

/// The read of the provision breadcrumb `node status` classifies from.
fn read_breadcrumb_command() -> String {
    format!("cat {SWAP_PROVISION_STATUS_PATH}")
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
fn probe_node_swap_state<S: RemoteShell>(
    shell: &S,
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

    assemble_node_swap_state(ProbedFacts {
        api_node_name,
        fail_swap_on,
        swap_behavior,
        swap_capacity,
        kubelet_version,
        // A single cheap SSH probe decides SSH reachability up-front
        // (`hostname` also gives a node-name fallback for the report header
        // when the kube-API could not answer it).
        ssh_node_name: shell
            .run_remote(HOSTNAME_PROBE_COMMAND)
            .ok()
            .map(|s| s.trim().to_string()),
        swapon_raw: shell.run_remote(SWAPON_SHOW_COMMAND).ok(),
        fstab_raw: shell.run_remote(READ_FSTAB_COMMAND).unwrap_or_default(),
        swapfile_exists: shell.run_remote(&swapfile_exists_command()).is_ok(),
        swappiness_raw: shell.run_remote(SWAPPINESS_PROBE_COMMAND).ok(),
        gomemlimit: probe_gomemlimit(shell),
        breadcrumb_raw: shell.run_remote(&read_breadcrumb_command()).ok(),
        // `APPRAFTER_SKIP_NODE_SWAP` at status time also forces the skipped
        // verdict (the operator asks "what did I get" with the same hook set).
        env_skipped: std::env::var_os(SKIP_NODE_SWAP_ENV).is_some(),
    })
}

/// The raw facts [`probe_node_swap_state`] gathers from its two sources, before
/// any interpretation. Splitting them out is what makes
/// [`assemble_node_swap_state`] — where every degradation decision lives —
/// testable with no node and no cluster.
struct ProbedFacts {
    /// Node name per the kube-API (`None` when the API could not answer).
    api_node_name: Option<String>,
    /// `failSwapOn` from `configz`.
    fail_swap_on: Field,
    /// `swapBehavior` from `configz`.
    swap_behavior: Field,
    /// `status.nodeInfo.swap.capacity` off the Node object.
    swap_capacity: Field,
    /// `status.nodeInfo.kubeletVersion` off the Node object.
    kubelet_version: Field,
    /// `hostname` over SSH — `None` is the SSH-down signal.
    ssh_node_name: Option<String>,
    /// Raw `swapon --show` (`None` when SSH is down).
    swapon_raw: Option<String>,
    /// Raw `/etc/fstab` (empty when SSH is down).
    fstab_raw: String,
    /// Whether `/swapfile` exists on disk.
    swapfile_exists: bool,
    /// Raw `sysctl -n vm.swappiness`.
    swappiness_raw: Option<String>,
    /// Already-parsed `GOMEMLIMIT`.
    gomemlimit: Field,
    /// Raw provision breadcrumb line.
    breadcrumb_raw: Option<String>,
    /// Whether `APPRAFTER_SKIP_NODE_SWAP` is set in THIS process.
    env_skipped: bool,
}

/// Assemble the report value from the probed facts — the pure half of
/// [`probe_node_swap_state`] (design decision 6 / Q10). Extracted so the
/// graceful-degradation rules are testable with no node and no cluster:
///
/// - `ssh_available` is derived SOLELY from the `hostname` probe, so the
///   `[ssh]` fields degrade together and the `[api]` fields do not degrade
///   with them.
/// - The report header prefers the kube-API node name over the SSH hostname,
///   and falls back to `<unknown>` rather than rendering an empty header.
/// - `APPRAFTER_SKIP_NODE_SWAP` forces the skipped verdict ONLY when swap is
///   not actually active — a node with live swap must never be reported as
///   "skipped" just because the operator happens to have the hook exported.
fn assemble_node_swap_state(f: ProbedFacts) -> NodeSwapState {
    let ssh_available = f.ssh_node_name.is_some();
    let node_name = f
        .api_node_name
        .or(f.ssh_node_name)
        .unwrap_or_else(|| "<unknown>".to_string());

    let swapon = swapon_summary(f.swapon_raw.as_deref());
    let swappiness = field_or_unknown(f.swappiness_raw);
    let provision_breadcrumb = field_or_unknown(f.breadcrumb_raw);

    let swap_active = f
        .swapon_raw
        .as_deref()
        .map(swap_already_active)
        .unwrap_or(false);
    let swap_size = f.swapon_raw.as_deref().and_then(swapfile_size_column);
    let orphan = orphan_swapfile(
        f.swapfile_exists,
        f.swapon_raw.as_deref().unwrap_or(""),
        &f.fstab_raw,
    );
    let api_eligible = match &f.kubelet_version {
        Field::Known(v) => Some(k8s_ge_134(v)),
        Field::Unknown => None,
    };
    let state = if f.env_skipped && !swap_active {
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
        fail_swap_on: f.fail_swap_on,
        swap_behavior: f.swap_behavior,
        swap_capacity: f.swap_capacity,
        kubelet_version: f.kubelet_version,
        swapon,
        swappiness,
        gomemlimit: f.gomemlimit,
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
    parse_kube_node_name(&out)
}

/// Turns the node-name jsonpath output into `Some(name)`. Extracted from
/// [`kube_node_name`] because the EMPTY case is the one that matters: on a
/// cluster with no nodes `jsonpath={.items[0]…}` exits ZERO with empty stdout,
/// and an empty node name would be interpolated into the `configz` URL as
/// `/api/v1/nodes//proxy/configz` — a request that fails confusingly instead
/// of the field simply degrading to `unknown`.
fn parse_kube_node_name(out: &str) -> Option<String> {
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
    let path = configz_raw_path(node);
    let raw = match kubectl_capture(kubeconfig_path, &["get", "--raw", &path]) {
        Some(r) => r,
        None => return (Field::Unknown, Field::Unknown),
    };
    configz_fields(&raw)
}

/// Pull the two kubelet-config scalars `node status` reports out of one
/// `configz` blob. Extracted from [`probe_configz`] so the pairing is testable:
/// the two fields must degrade INDEPENDENTLY — a `<1.34` kubelet legitimately
/// has `failSwapOn` and no `swapBehavior`, and reporting both `unknown` there
/// would hide the one fact that IS known.
fn configz_fields(configz_json: &str) -> (Field, Field) {
    (
        configz_scalar(configz_json, "failSwapOn"),
        configz_scalar(configz_json, "swapBehavior"),
    )
}

/// The apiserver node-proxy path that serves a node's live kubelet config.
/// Extracted from [`probe_configz`] so the shape is pinned — it must go
/// through the `nodes/<n>/proxy` node-proxy (the kubelet's own `configz` is
/// not otherwise reachable from outside the node).
fn configz_raw_path(node: &str) -> String {
    format!("/api/v1/nodes/{node}/proxy/configz")
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
    let args = node_field_args(node, jsonpath);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match kubectl_capture(kubeconfig_path, &argv) {
        Some(out) => node_field_from_output(&out),
        None => Field::Unknown,
    }
}

/// Build the argv for `kubectl get node <n> -o jsonpath=<expr>`. Extracted
/// from [`probe_node_field`] so the `-o` / `jsonpath=` split is pinned: kubectl
/// wants the selector as the VALUE of `-o` (`-o` `jsonpath={…}`), and gluing
/// them the other way round (`-o=jsonpath…`) or dropping the `jsonpath=`
/// prefix makes kubectl print the whole object instead of the one field.
fn node_field_args(node: &str, jsonpath: &str) -> Vec<String> {
    vec![
        "get".to_string(),
        "node".to_string(),
        node.to_string(),
        "-o".to_string(),
        format!("jsonpath={jsonpath}"),
    ]
}

/// Maps a Node-object jsonpath result to a [`Field`]. Extracted from
/// [`probe_node_field`] because the three "kubectl answered, but with nothing"
/// shapes are the interesting ones: an empty result (the field is absent —
/// `swap.capacity` is simply missing on a node without swap), and kubectl's
/// own `<unknown>` / `<none>` placeholders. All three must render as `unknown`
/// rather than being reported as a literal value like `<none>`.
fn node_field_from_output(out: &str) -> Field {
    let v = out.trim();
    if v.is_empty() || v == "<unknown>" || v == "<none>" {
        Field::Unknown
    } else {
        Field::Known(v.to_string())
    }
}

/// Reads the effective `GOMEMLIMIT` off the running k3s unit's environment
/// (`systemctl show -p Environment k3s`). `Unknown` when absent / SSH down.
fn probe_gomemlimit<S: RemoteShell>(shell: &S) -> Field {
    gomemlimit_from_probe(shell.run_remote(GOMEMLIMIT_PROBE_COMMAND))
}

/// Maps the `systemctl show` probe result to a [`Field`]. Extracted from
/// [`probe_gomemlimit`] so the SSH-failure arm is pinned: a failed probe must
/// degrade to `Unknown`, never be fed to [`parse_gomemlimit`] (whose input
/// would then be a stray error string).
fn gomemlimit_from_probe(probe: Result<String>) -> Field {
    match probe {
        Ok(o) => parse_gomemlimit(&o),
        Err(_) => Field::Unknown,
    }
}

/// Extracts `GOMEMLIMIT` from the raw `systemctl show -p Environment k3s`
/// output. systemd renders the WHOLE space-separated `key=val` list behind a
/// SINGLE `Environment=` key prefix — so the real shape is
/// `Environment=GOMEMLIMIT=2GiB` (or `Environment=GOMEMLIMIT=2GiB FOO=bar`,
/// order-independent), NOT a per-token `GOMEMLIMIT=…`. This also tolerates the
/// bare `GOMEMLIMIT=2GiB` shape (`systemctl show … --value`) for robustness.
///
/// Pure over the raw text so the real shapes are unit-testable without a node.
fn parse_gomemlimit(systemctl_output: &str) -> Field {
    for tok in systemctl_output.split_whitespace() {
        // Strip the OPTIONAL `Environment=` key prefix first — the first token
        // of the non-`--value` output is `Environment=GOMEMLIMIT=…`, so a raw
        // `strip_prefix("GOMEMLIMIT=")` on it would miss the real value.
        let tok = tok.strip_prefix("Environment=").unwrap_or(tok);
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

    // ---- GOMEMLIMIT parse (Fix 1: systemd `Environment=` key prefix) -----

    #[test]
    fn parse_gomemlimit_strips_environment_key_prefix() {
        // The REAL non-`--value` shape: systemd puts the whole key=val list
        // behind ONE `Environment=` key prefix.
        assert_eq!(
            parse_gomemlimit("Environment=GOMEMLIMIT=2GiB"),
            Field::Known("2GiB".into())
        );
    }

    #[test]
    fn parse_gomemlimit_first_of_multiple_vars() {
        assert_eq!(
            parse_gomemlimit("Environment=GOMEMLIMIT=2GiB FOO=bar"),
            Field::Known("2GiB".into())
        );
    }

    #[test]
    fn parse_gomemlimit_not_first_of_multiple_vars() {
        // Order-independent: only the FIRST token carries the `Environment=`
        // prefix, later tokens are bare `key=val`.
        assert_eq!(
            parse_gomemlimit("Environment=FOO=bar GOMEMLIMIT=2GiB"),
            Field::Known("2GiB".into())
        );
    }

    #[test]
    fn parse_gomemlimit_bare_value_shape() {
        // The `systemctl show … --value` shape (key prefix already stripped).
        assert_eq!(
            parse_gomemlimit("GOMEMLIMIT=2GiB"),
            Field::Known("2GiB".into())
        );
    }

    #[test]
    fn parse_gomemlimit_absent_is_unknown() {
        // Empty environment (`Environment=`), an unrelated var, and empty
        // output all → Unknown.
        assert_eq!(parse_gomemlimit("Environment="), Field::Unknown);
        assert_eq!(parse_gomemlimit("Environment=FOO=bar"), Field::Unknown);
        assert_eq!(parse_gomemlimit(""), Field::Unknown);
    }

    #[test]
    fn parse_gomemlimit_empty_value_is_unknown() {
        // `GOMEMLIMIT=` with no value must NOT report a bogus empty Known.
        assert_eq!(parse_gomemlimit("Environment=GOMEMLIMIT="), Field::Unknown);
    }

    #[test]
    fn parse_gomemlimit_real_world_edge_shapes() {
        // The REAL `systemctl show` output always carries a trailing newline —
        // `split_whitespace` absorbs it (the dogfood-facing shape).
        assert_eq!(
            parse_gomemlimit("Environment=GOMEMLIMIT=2GiB\n"),
            Field::Known("2GiB".into())
        );
        // A value with an embedded `=` is returned verbatim (strip_prefix stops
        // at the FIRST `GOMEMLIMIT=`), not truncated.
        assert_eq!(
            parse_gomemlimit("Environment=GOMEMLIMIT=KEY=VALUE"),
            Field::Known("KEY=VALUE".into())
        );
        // Case-sensitive: systemd's env key is uppercase `GOMEMLIMIT` — a
        // lowercased key must NOT match.
        assert_eq!(
            parse_gomemlimit("Environment=gomemlimit=2GiB"),
            Field::Unknown
        );
    }

    // ---- retrofit provision breadcrumb (Fix 2) --------------------------

    #[test]
    fn provision_breadcrumb_write_script_writes_state_to_the_exported_path() {
        let s = provision_breadcrumb_write_script("applied (retrofit): swap 4096 MiB");
        // Writes to the canonical const path (not a hardcoded string), the
        // state verbatim, and ensures the dir first.
        assert!(s.contains(SWAP_PROVISION_STATUS_PATH), "{s}");
        assert!(
            s.contains("echo \"applied (retrofit): swap 4096 MiB\""),
            "{s}"
        );
        assert!(s.contains("mkdir -p /var/lib/apprafter"), "{s}");
    }

    #[test]
    fn provision_breadcrumb_write_script_is_best_effort() {
        // Must never fail the prep: both the mkdir and the echo swallow errors.
        let s = provision_breadcrumb_write_script("rolled-back: swap removed");
        assert!(s.contains("echo \"rolled-back: swap removed\""), "{s}");
        assert!(
            s.matches("2>/dev/null || true").count() >= 2,
            "both the mkdir and the echo must be best-effort:\n{s}"
        );
    }

    #[test]
    fn provision_breadcrumb_rollback_states_are_not_the_applied_prefix() {
        // The rollback breadcrumbs must NOT read as an `applied…` state.
        for state in [
            "rolled-back: swap removed",
            "rolled-back-partial: swap left active",
        ] {
            let s = provision_breadcrumb_write_script(state);
            assert!(s.contains(&format!("echo \"{state}\"")), "{s}");
            assert!(!state.starts_with("applied"), "{state}");
        }
    }

    #[test]
    fn recovery_probe_uses_node_local_k3s_kubectl() {
        // Node-local k3s kubectl needs no external creds.
        assert!(recovery_probe_command().contains("k3s kubectl get --raw"));
    }

    #[test]
    fn recovery_probe_verifies_the_kubelet_not_just_the_apiserver() {
        // Regression guard for the walk-STEP6 blind spot: `/readyz` alone is
        // the apiserver's readiness and stays green when the kubelet bricked on
        // a bad swap drop-in, so the whole-step rollback never fired. The probe
        // MUST also verify the kubelet is back via the apiserver node-proxy so a
        // kubelet-only failure is detected and the rollback fires.
        let cmd = recovery_probe_command();
        assert!(cmd.contains("/readyz"), "still checks the apiserver: {cmd}");
        assert!(
            cmd.contains("nodes/$(hostname)/proxy/configz"),
            "recovery probe must also verify the kubelet through the node-proxy: {cmd}"
        );
        // Both probes are joined by `&&` so EITHER being down = not recovered.
        assert!(cmd.contains("&&"), "both probes must be required: {cmd}");
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

    // ---- fault-injection hook (walk STEP 6 rollback e2e, Q1) ------------

    #[test]
    fn swap_dropin_body_force_invalid_emits_bogus_kubelet_config() {
        // With force_invalid set, the body is a syntactically-INVALID
        // KubeletConfiguration (bogus apiVersion + unparseable field) so the
        // kubelet rejects it and k3s fails to restart → rollback fires.
        let bad = swap_dropin_body(true, true);
        assert!(bad.contains("kind: KubeletConfiguration"), "{bad}");
        // The apiVersion is a nonexistent group (not kubelet.config.k8s.io).
        assert!(
            !bad.contains("kubelet.config.k8s.io"),
            "invalid body must NOT carry the real kubelet apiVersion group\n{bad}"
        );
        assert!(bad.contains("apprafter.invalid/v0"), "{bad}");
        // And it is NOT the valid body.
        assert_ne!(bad, swap_kubelet_dropin(true));
        assert_ne!(bad, swap_kubelet_dropin(false));

        // Without the hook, the body is the normal valid drop-in.
        assert_eq!(swap_dropin_body(true, false), swap_kubelet_dropin(true));
        assert_eq!(swap_dropin_body(false, false), swap_kubelet_dropin(false));
    }

    #[test]
    fn apply_write_honours_force_invalid_env_but_rollback_write_stays_valid() {
        // The undocumented APPRAFTER_NODE_SWAP_FORCE_INVALID_DROPIN hook makes
        // the INITIAL-apply write emit the invalid body; the rollback-rewrite
        // builder (swap_dropin_write_script) stays VALID regardless, so
        // recovery is never sabotaged by the fault hook still being set.
        let saved = std::env::var_os(FORCE_INVALID_DROPIN_ENV);
        std::env::set_var(FORCE_INVALID_DROPIN_ENV, "1");

        let apply = swap_dropin_write_script_apply(true);
        assert!(
            apply.contains("apprafter.invalid/v0"),
            "apply write must honour the fault hook and emit the invalid body\n{apply}"
        );
        assert!(
            !apply.contains("swapBehavior: NoSwap"),
            "the invalid apply body must NOT be the valid NoSwap drop-in\n{apply}"
        );

        // The rollback-rewrite builder is ALWAYS valid, even with the hook set.
        let rollback_write = swap_dropin_write_script(true);
        assert!(
            rollback_write.contains("failSwapOn: false"),
            "rollback write must stay a valid drop-in even under the fault hook\n{rollback_write}"
        );
        assert!(
            !rollback_write.contains("apprafter.invalid"),
            "rollback write must NEVER emit the invalid body\n{rollback_write}"
        );

        match saved {
            Some(v) => std::env::set_var(FORCE_INVALID_DROPIN_ENV, v),
            None => std::env::remove_var(FORCE_INVALID_DROPIN_ENV),
        }
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

    // ---- (f) gate: an UNPARSEABLE version is a refusal, not a proceed -----

    #[test]
    fn gate_refuses_an_unparseable_kubelet_version() {
        // INVARIANT: `k8s_ge_134` returns false when the version does not
        // parse, so an unrecognised kubelet string REFUSES the swap step. The
        // dangerous alternative is defaulting to "probably new enough" and
        // writing `swapBehavior: NoSwap` onto a kubelet that rejects it —
        // which bricks the kubelet on restart.
        assert!(!k8s_ge_134("k3s-dev"));
        assert!(!k8s_ge_134("v1"));
        assert!(!k8s_ge_134(""));
        match swap_eligibility("k3s-dev", "cgroup2fs") {
            SwapGate::Refuse { hint } => assert!(hint.contains("1.34"), "{hint}"),
            other => panic!("an unparseable version must REFUSE, got {other:?}"),
        }
    }

    // ---- (g) SshNodeOps remote-command builders -------------------------

    #[test]
    fn swapoff_command_is_capped_by_a_timeout() {
        // INVARIANT (design decision 5 / P8): `swapoff` can hang or ENOMEM
        // under the very pressure that triggered the rollback. Without the
        // `timeout` cap an unbounded hang strands the rollback mid-flight with
        // swap on and the drop-in half-rewritten.
        let cmd = swapoff_command();
        assert!(cmd.starts_with("timeout "), "must be capped: {cmd}");
        assert!(cmd.contains(&format!("swapoff {SWAPFILE_PATH}")), "{cmd}");
    }

    #[test]
    fn dropin_and_swapfile_removals_target_different_files() {
        // The drop-in removal deletes the kubelet config; the swapfile removal
        // deletes the backing file. Confusing the two would either leave
        // `failSwapOn:false` behind or delete a live swapfile.
        assert_eq!(
            remove_dropin_command(),
            format!("rm -f {SWAP_KUBELET_DROPIN_PATH}")
        );
        assert_eq!(remove_swapfile_command(), format!("rm -f {SWAPFILE_PATH}"));
        assert_ne!(remove_dropin_command(), remove_swapfile_command());
    }

    #[test]
    fn fstab_removal_is_anchored_and_still_removes_the_sysctl_on_failure() {
        // INVARIANT: the fstab line MUST go — a surviving `sw,nofail` entry
        // silently reactivates swap on the next boot, undoing the rollback.
        // The two halves are joined by `;` not `&&` so an unmatched `sed`
        // still leaves the swappiness drop-in removed.
        let cmd = remove_fstab_and_sysctl_command();
        assert!(cmd.contains("/etc/fstab"), "{cmd}");
        assert!(
            cmd.contains(&format!("^{SWAPFILE_PATH} ")),
            "the sed pattern must be anchored at the NAME column: {cmd}"
        );
        assert!(
            cmd.contains("rm -f /etc/sysctl.d/99-apprafter-swap.conf"),
            "{cmd}"
        );
        assert!(
            !cmd.contains("&&"),
            "a failed sed must not skip the sysctl removal: {cmd}"
        );
    }

    #[test]
    fn restart_command_reloads_units_before_restarting_k3s() {
        // A drop-in written under k3s.service.d is invisible to systemd until
        // `daemon-reload`, so restarting first would start k3s with the OLD
        // unit and the apply would look like a silent no-op.
        let reload = RESTART_K3S_COMMAND
            .find("daemon-reload")
            .expect("must daemon-reload");
        let restart = RESTART_K3S_COMMAND
            .find("restart k3s")
            .expect("must restart k3s");
        assert!(
            reload < restart,
            "daemon-reload must precede the restart: {RESTART_K3S_COMMAND}"
        );
        assert!(RESTART_K3S_COMMAND.contains("&&"), "{RESTART_K3S_COMMAND}");
    }

    #[test]
    fn logger_command_escapes_embedded_single_quotes() {
        // INVARIANT: the runbook text carries backticks and apostrophes. An
        // unescaped `'` would terminate the shell argument early, so the node's
        // journal would get a TRUNCATED (or word-split) runbook — exactly when
        // the operator most needs the whole line.
        assert_eq!(shell_single_quote("plain"), "'plain'");
        assert_eq!(shell_single_quote("it's"), "'it'\\''s'");
        assert_eq!(
            logger_command("it's"),
            "logger -t apprafter-node-prep 'it'\\''s'"
        );
        // The real payload: the runbook survives the escaping intact — decode
        // the quoted word the way `sh` would and it must equal the input.
        let cmd = logger_command(SWAPOFF_FAILED_RUNBOOK);
        let quoted = cmd
            .strip_prefix("logger -t apprafter-node-prep '")
            .and_then(|s| s.strip_suffix('\''))
            .expect("the message must be a single-quoted shell word");
        assert_eq!(
            quoted.replace("'\\''", "'"),
            SWAPOFF_FAILED_RUNBOOK,
            "the runbook must survive the quoting byte-for-byte"
        );
    }

    // ---- (h) probe output parsers ---------------------------------------

    #[test]
    fn kubelet_version_probe_is_node_local_and_reads_the_node_info() {
        let cmd = KUBELET_VERSION_PROBE_COMMAND;
        assert!(cmd.contains("k3s kubectl"), "node-local kubectl: {cmd}");
        assert!(cmd.contains("$(hostname)"), "{cmd}");
        assert!(cmd.contains(".status.nodeInfo.kubeletVersion"), "{cmd}");
    }

    #[test]
    fn kubelet_version_parse_rejects_an_empty_jsonpath_result() {
        // INVARIANT: a jsonpath miss exits ZERO with empty stdout. Returning
        // "" here would flow into `k8s_ge_134` as false and REFUSE the swap
        // step with a nonsense "node kubelet is  (<1.34)" hint instead of
        // reporting that the probe itself failed.
        assert_eq!(
            parse_kubelet_version_output("  v1.35.5+k3s1\n").unwrap(),
            "v1.35.5+k3s1"
        );
        let err = parse_kubelet_version_output("   \n").unwrap_err();
        assert!(
            err.to_string().contains("kubeletVersion"),
            "the empty case must name what could not be read: {err}"
        );
        assert!(parse_kubelet_version_output("").is_err());
    }

    #[test]
    fn mem_total_parse_quotes_what_the_node_actually_said() {
        assert_eq!(parse_mem_total_kib(" 16384000 \n").unwrap(), 16_384_000);
        // A shape surprise must be LOUD and quote the raw text — a silent 0
        // would size the swapfile at 0 MiB.
        let err = parse_mem_total_kib("MemTotal:  16384000 kB").unwrap_err();
        assert!(
            err.to_string().contains("MemTotal:  16384000 kB"),
            "the error must quote the unparsed output: {err}"
        );
        assert!(parse_mem_total_kib("").is_err());
    }

    // ---- (i) recovery poll loop ------------------------------------------

    #[test]
    fn recovery_returns_on_the_first_success_without_sleeping() {
        // INVARIANT: the happy path must not pay the poll interval. The loop
        // probes BEFORE sleeping, so a node that is already back returns
        // immediately — with a 5s interval, sleeping first would add 5s to
        // every successful `node prep`.
        let mut calls = 0;
        let started = Instant::now();
        wait_for_recovery_with(
            || {
                calls += 1;
                Ok("ok".to_string())
            },
            Duration::from_secs(60),
            Duration::from_secs(5),
        )
        .expect("first probe succeeds");
        assert_eq!(calls, 1, "must not probe twice on success");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "must not sleep before returning: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn recovery_tolerates_transient_failures_then_succeeds() {
        // k3s refuses connections for the first seconds after a restart, so a
        // failing probe must NOT abort the wait.
        let mut calls = 0;
        wait_for_recovery_with(
            || {
                calls += 1;
                if calls < 3 {
                    Err(CliError::Other("connection refused".into()))
                } else {
                    Ok("ok".to_string())
                }
            },
            Duration::from_secs(30),
            Duration::from_millis(1),
        )
        .expect("recovers on the third attempt");
        assert_eq!(calls, 3);
    }

    #[test]
    fn recovery_timeout_reports_the_attempt_count_and_the_last_error() {
        // INVARIANT: the node is probed at least once even on a spent budget
        // (the deadline is checked AFTER the probe), and the error carries the
        // LAST probe error — the only diagnostic an operator gets for a node
        // that never came back. Reporting the FIRST error would show the
        // transient "connection refused" instead of the real cause.
        let mut calls = 0;
        let err = wait_for_recovery_with(
            || {
                calls += 1;
                Err(CliError::Other(format!("probe failure #{calls}")))
            },
            Duration::from_millis(30),
            Duration::from_millis(5),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(calls > 1, "expected several attempts, got {calls}");
        assert!(
            msg.contains(&format!("({calls} attempts)")),
            "must report the attempt count: {msg}"
        );
        assert!(
            msg.contains(&format!("probe failure #{calls}")),
            "must carry the LAST probe error, not an earlier one: {msg}"
        );
        assert!(
            !msg.contains("probe failure #1)"),
            "must not report the first error as the last: {msg}"
        );

        // A spent budget still probes once.
        let mut once = 0;
        let _ = wait_for_recovery_with(
            || {
                once += 1;
                Err(CliError::Other("down".into()))
            },
            Duration::ZERO,
            Duration::from_millis(1),
        );
        assert_eq!(once, 1, "the node must be probed even with a zero budget");
    }

    #[test]
    fn recovery_timeout_error_names_the_budget_in_seconds() {
        let err = recovery_timeout_error(Duration::from_secs(180), 7, "connection refused");
        let msg = err.to_string();
        assert!(msg.contains("within 180s"), "{msg}");
        assert!(msg.contains("(7 attempts)"), "{msg}");
        assert!(msg.contains("connection refused"), "{msg}");
    }

    // ---- (j) pre-apply idempotency plan ----------------------------------

    const ACTIVE_SWAPON: &str = "NAME      TYPE SIZE USED PRIO\n/swapfile file 4G   0B   -2\n";
    const OUR_FSTAB: &str = "UUID=x / ext4 defaults 0 1\n/swapfile none swap sw,nofail 0 0\n";

    #[test]
    fn pre_apply_plan_never_treats_a_live_swapfile_as_an_orphan() {
        // INVARIANT: `AlreadyProvisioned` is checked FIRST. A fully provisioned
        // node also satisfies "the file exists on disk", so a reordered check
        // would `swapoff` + `rm` a healthy node's LIVE swap on every re-run.
        assert_eq!(
            pre_apply_plan(ACTIVE_SWAPON, OUR_FSTAB, true),
            PreApplyPlan::AlreadyProvisioned
        );
        // Active but NOT persisted: still not an orphan (it is in use), so the
        // apply must not delete it either.
        assert_eq!(
            pre_apply_plan(ACTIVE_SWAPON, "UUID=x / ext4 defaults 0 1\n", true),
            PreApplyPlan::FreshProvision
        );
    }

    #[test]
    fn pre_apply_plan_removes_a_true_orphan_and_leaves_a_clean_node_alone() {
        // Present on disk, not active, not persisted → the half-provisioned
        // remnant the fresh `dd` must not write over.
        assert_eq!(
            pre_apply_plan(
                "NAME TYPE SIZE USED PRIO\n",
                "UUID=x / ext4 defaults 0 1\n",
                true
            ),
            PreApplyPlan::RemoveOrphan
        );
        // Nothing on disk → nothing to clean up.
        assert_eq!(
            pre_apply_plan(
                "NAME TYPE SIZE USED PRIO\n",
                "UUID=x / ext4 defaults 0 1\n",
                false
            ),
            PreApplyPlan::FreshProvision
        );
    }

    #[test]
    fn orphan_cleanup_removes_the_file_even_when_swapoff_fails() {
        // INVARIANT: an orphan is by definition NOT active, so its `swapoff`
        // is expected to fail. Joining the two with `&&` would skip the `rm`,
        // the orphan would survive, and the fresh `dd` would write over it.
        let cmd = orphan_cleanup_command();
        assert!(cmd.contains(&format!("swapoff {SWAPFILE_PATH}")), "{cmd}");
        assert!(cmd.contains(&format!("rm -f {SWAPFILE_PATH}")), "{cmd}");
        assert!(
            !cmd.contains("&&"),
            "a failing swapoff must not skip the rm: {cmd}"
        );
        assert!(cmd.contains(';'), "{cmd}");
    }

    #[test]
    fn orphan_notice_names_the_file_and_the_host_it_is_deleted_from() {
        // Deleting a file on the operator's node must never be silent.
        let msg = orphan_notice("203.0.113.7");
        assert!(msg.contains(SWAPFILE_PATH), "{msg}");
        assert!(msg.contains("203.0.113.7"), "{msg}");
        assert!(msg.to_lowercase().contains("orphan"), "{msg}");
    }

    // ---- (k) retrofit swap size cap --------------------------------------

    #[test]
    fn retrofit_swap_count_is_capped_at_the_shared_max() {
        // INVARIANT (Fix 2): the breadcrumb size must mirror
        // `swap_enable_script`'s internal `min(MemTotal_MiB, 8Gi)`. Reporting
        // an uncapped size would claim 32 GiB of swap on a 32 GiB node that
        // actually received 8 GiB.
        assert_eq!(retrofit_swap_count_mib(4 * 1024 * 1024), 4096);
        assert_eq!(retrofit_swap_count_mib(32 * 1024 * 1024), SWAP_MAX_MIB);
        assert_eq!(retrofit_swap_count_mib(8 * 1024 * 1024), SWAP_MAX_MIB);
        assert_eq!(retrofit_swap_count_mib(0), 0);
    }

    // ---- (l) breadcrumbs + rollback errors -------------------------------

    #[test]
    fn breadcrumb_states_distinguish_applied_from_both_rollbacks() {
        // INVARIANT: `classify_state` reads these lines back. A rollback
        // breadcrumb that started with `applied` would make `node status`
        // report swap as provisioned on a node that has none.
        let applied = applied_breadcrumb_state(4096);
        assert!(applied.starts_with("applied"), "{applied}");
        assert!(applied.contains("4096 MiB"), "{applied}");
        assert_eq!(
            applied_breadcrumb_state(8192),
            "applied (retrofit): swap 8192 MiB"
        );

        assert!(!ROLLED_BACK_BREADCRUMB.starts_with("applied"));
        assert!(!ROLLED_BACK_PARTIAL_BREADCRUMB.starts_with("applied"));
        assert_ne!(ROLLED_BACK_BREADCRUMB, ROLLED_BACK_PARTIAL_BREADCRUMB);
        assert!(
            ROLLED_BACK_PARTIAL_BREADCRUMB.contains("left active"),
            "the partial breadcrumb must say swap is STILL ON: \
             {ROLLED_BACK_PARTIAL_BREADCRUMB}"
        );
    }

    #[test]
    fn rollback_errors_keep_the_original_failure_and_describe_different_states() {
        // INVARIANT: a successful rollback is still a FAILED apply, and the
        // original recovery error must survive into the message — it is the
        // only clue why the node did not come back. The two branches leave the
        // node in very different states, so their messages must differ.
        let original = "k3s API did not recover within 180s after restart (36 attempts)";
        let clean = rollback_recovered_error(original).to_string();
        let partial = rollback_partial_error(original).to_string();

        assert!(clean.contains(original), "{clean}");
        assert!(partial.contains(original), "{partial}");
        assert!(clean.contains("rolled back cleanly"), "{clean}");
        assert!(clean.contains("/swapfile deleted"), "{clean}");
        assert!(
            partial.contains("could NOT swapoff"),
            "the partial branch must say the swapoff failed: {partial}"
        );
        assert!(
            partial.contains("failSwapOn:false"),
            "the partial branch must say the drop-in was LEFT: {partial}"
        );
        assert_ne!(clean, partial);
    }

    #[test]
    fn recovery_failed_notice_announces_the_rollback_is_starting() {
        // A silent multi-minute rollback looks like a hang on the very command
        // that just took the operator's API away.
        let msg = recovery_failed_notice("timed out");
        assert!(msg.contains("timed out"), "{msg}");
        assert!(msg.to_lowercase().contains("rolling"), "{msg}");
    }

    // ---- (m) `node prep` / `node status` entry errors ---------------------

    #[test]
    fn entry_errors_each_name_their_own_remedy() {
        // These are the first thing an operator sees when the command cannot
        // even start; each must say what to run next.
        let no_server = no_provisioned_server_error().to_string();
        assert!(no_server.contains("apprafter up"), "{no_server}");
        let no_ip = no_public_ipv4_error().to_string();
        assert!(no_ip.contains("cloud-init"), "{no_ip}");
        let non_tty = non_interactive_error().to_string();
        assert!(
            non_tty.contains("--yes"),
            "the non-TTY refusal must name the flag that makes it scriptable: {non_tty}"
        );
    }

    #[test]
    fn prep_confirmation_discloses_the_restart_and_names_the_host() {
        // INVARIANT: this is the operator's ONLY warning that the API goes
        // away. With several targets configured, an unnamed prompt is a
        // footgun — the message must say WHICH node is about to restart.
        let msg = prep_confirmation_message("203.0.113.7");
        assert!(msg.contains("203.0.113.7"), "{msg}");
        assert!(msg.contains("restarts k3s"), "{msg}");
        assert!(msg.contains("API briefly unavailable"), "{msg}");
    }

    #[test]
    fn refused_gate_becomes_a_loud_error_carrying_the_hint() {
        // Design decision 1 / N6: never silently skip. The refusal hint the
        // gate produced must reach the operator through a non-zero exit.
        let SwapGate::Refuse { hint } = swap_eligibility("v1.33.4+k3s1", "cgroup2fs") else {
            panic!("v1.33 must refuse");
        };
        let err = swap_step_skipped_error(&hint).to_string();
        assert!(err.contains("swap step skipped"), "{err}");
        assert!(err.contains(&hint), "the gate's hint must survive: {err}");
    }

    // ---- (n) status assembly from raw probes ------------------------------

    /// A `ProbedFacts` for a healthy, fully-probed node; tests override the
    /// one field they are about.
    fn healthy_facts() -> ProbedFacts {
        ProbedFacts {
            api_node_name: Some("api-node".into()),
            fail_swap_on: Field::Known("false".into()),
            swap_behavior: Field::Known("NoSwap".into()),
            swap_capacity: Field::Known("4294967296".into()),
            kubelet_version: Field::Known("v1.35.5+k3s1".into()),
            ssh_node_name: Some("ssh-node".into()),
            swapon_raw: Some(ACTIVE_SWAPON.to_string()),
            fstab_raw: OUR_FSTAB.to_string(),
            swapfile_exists: true,
            swappiness_raw: Some("10\n".into()),
            gomemlimit: Field::Known("2GiB".into()),
            breadcrumb_raw: Some("applied: swap 4096 MiB\n".into()),
            env_skipped: false,
        }
    }

    #[test]
    fn assemble_prefers_the_api_node_name_and_falls_back_in_order() {
        // The kube-API name is the authoritative one; the SSH hostname is only
        // a fallback for the header, and `<unknown>` is the last resort — an
        // empty header would read as a rendering bug.
        assert_eq!(
            assemble_node_swap_state(healthy_facts()).node_name,
            "api-node"
        );

        let ssh_only = ProbedFacts {
            api_node_name: None,
            ..healthy_facts()
        };
        assert_eq!(assemble_node_swap_state(ssh_only).node_name, "ssh-node");

        let neither = ProbedFacts {
            api_node_name: None,
            ssh_node_name: None,
            ..healthy_facts()
        };
        assert_eq!(assemble_node_swap_state(neither).node_name, "<unknown>");
    }

    #[test]
    fn assemble_derives_ssh_availability_solely_from_the_hostname_probe() {
        // INVARIANT (Q10): SSH being down must degrade ONLY the [ssh] fields.
        // The [api] fields come from the cached out-of-cluster kubeconfig and
        // must keep their values.
        let down = ProbedFacts {
            ssh_node_name: None,
            swapon_raw: None,
            fstab_raw: String::new(),
            swapfile_exists: false,
            swappiness_raw: None,
            gomemlimit: Field::Unknown,
            breadcrumb_raw: None,
            ..healthy_facts()
        };
        let s = assemble_node_swap_state(down);
        assert!(!s.ssh_available);
        assert_eq!(s.swapon, Field::Unknown);
        assert_eq!(s.swappiness, Field::Unknown);
        assert_eq!(s.provision_breadcrumb, Field::Unknown);
        // …while the kube-API fields survive.
        assert_eq!(s.kubelet_version, Field::Known("v1.35.5+k3s1".into()));
        assert_eq!(s.fail_swap_on, Field::Known("false".into()));

        // And a reachable node reports available.
        assert!(assemble_node_swap_state(healthy_facts()).ssh_available);
    }

    #[test]
    fn assemble_reads_size_and_swappiness_out_of_the_raw_probe_output() {
        let s = assemble_node_swap_state(healthy_facts());
        assert_eq!(
            s.state,
            SwapProvisionState::Active {
                size: Some("4G".into())
            },
            "the size column must come from the swapon row"
        );
        assert_eq!(s.swapon, Field::Known("/swapfile file 4G 0B -2".into()));
        assert_eq!(s.swappiness, Field::Known("10".into()));
    }

    #[test]
    fn assemble_env_skip_never_masks_live_swap() {
        // INVARIANT: `APPRAFTER_SKIP_NODE_SWAP` exported in the OPERATOR's
        // shell says nothing about what the node actually has. Reporting
        // `skipped` for a node with live swap would be a flat lie.
        let skipped_but_live = ProbedFacts {
            env_skipped: true,
            ..healthy_facts()
        };
        assert_eq!(
            assemble_node_swap_state(skipped_but_live).state,
            SwapProvisionState::Active {
                size: Some("4G".into())
            }
        );

        // With no swap on the node, the hook DOES pick the skipped verdict.
        let skipped_and_absent = ProbedFacts {
            env_skipped: true,
            swapon_raw: Some("NAME TYPE SIZE USED PRIO\n".into()),
            fstab_raw: String::new(),
            swapfile_exists: false,
            breadcrumb_raw: None,
            ..healthy_facts()
        };
        assert_eq!(
            assemble_node_swap_state(skipped_and_absent).state,
            SwapProvisionState::SkippedByEnv
        );
    }

    #[test]
    fn assemble_flags_an_orphan_swapfile_from_the_raw_probes() {
        let orphan = ProbedFacts {
            swapon_raw: Some("NAME TYPE SIZE USED PRIO\n".into()),
            fstab_raw: "UUID=x / ext4 defaults 0 1\n".into(),
            swapfile_exists: true,
            breadcrumb_raw: None,
            ..healthy_facts()
        };
        assert_eq!(
            assemble_node_swap_state(orphan).state,
            SwapProvisionState::OrphanSwapfile
        );
    }

    // ---- (o) classifier branches the earlier tests left open --------------

    #[test]
    fn classify_reports_ineligible_when_the_api_says_the_kubelet_is_old() {
        // The kube-API eligibility (not the breadcrumb) is what makes a node
        // with no breadcrumb at all ineligible.
        assert_eq!(
            classify_state(true, false, None, false, &Field::Unknown, Some(false)),
            SwapProvisionState::Ineligible {
                detail: "k8s <1.34".to_string()
            }
        );
    }

    #[test]
    fn classify_stays_actionable_when_ssh_is_up_but_the_api_is_silent() {
        // SSH answered, nothing is provisioned, and the kube-API could not be
        // read: still tell the operator to run `node prep` rather than
        // reporting the state as unknowable.
        assert_eq!(
            classify_state(true, false, None, false, &Field::Unknown, None),
            SwapProvisionState::EligibleNotApplied
        );
    }

    #[test]
    fn classify_does_not_trust_an_applied_breadcrumb_over_a_dead_swapfile() {
        // INVARIANT (the P9 `sw,nofail` trap): the breadcrumb says the swap was
        // applied, but `swapon` says it is not live — e.g. it silently failed
        // to reactivate after a reboot. That node must read as NOT applied, or
        // the operator believes it has a cushion it does not have.
        assert_eq!(
            classify_state(
                true,
                false,
                None,
                false,
                &Field::Known("applied (retrofit): swap 4096 MiB".into()),
                Some(true),
            ),
            SwapProvisionState::EligibleNotApplied
        );
    }

    #[test]
    fn render_status_active_without_a_size_omits_the_parenthetical() {
        let out = render_status(&full_state(SwapProvisionState::Active { size: None }));
        assert!(out.contains("swap active — NoSwap for pods"), "{out}");
        assert!(
            !out.contains("swap active ("),
            "an unknown size must not render an empty parenthetical: {out}"
        );
    }

    // ---- (p) kube-API output parsers -------------------------------------

    #[test]
    fn kube_node_name_parse_rejects_an_empty_result() {
        // INVARIANT: on a cluster with no nodes the jsonpath exits ZERO with
        // empty stdout. An empty name would be interpolated into the configz
        // URL as `/api/v1/nodes//proxy/configz`, which fails confusingly
        // instead of the field simply degrading to `unknown`.
        assert_eq!(
            parse_kube_node_name(" node-1 \n"),
            Some("node-1".to_string())
        );
        assert_eq!(parse_kube_node_name("  \n"), None);
        assert_eq!(parse_kube_node_name(""), None);
    }

    #[test]
    fn node_field_maps_kubectls_placeholders_to_unknown() {
        // kubectl prints its own `<none>` / `<unknown>` placeholders for absent
        // jsonpath targets; reporting those verbatim would render
        // `swap.capacity : <none>` as if it were a value.
        assert_eq!(node_field_from_output(""), Field::Unknown);
        assert_eq!(node_field_from_output("  \n"), Field::Unknown);
        assert_eq!(node_field_from_output("<none>"), Field::Unknown);
        assert_eq!(node_field_from_output("<unknown>"), Field::Unknown);
        assert_eq!(
            node_field_from_output(" 4294967296 \n"),
            Field::Known("4294967296".into())
        );
    }

    #[test]
    fn configz_path_goes_through_the_apiserver_node_proxy() {
        // The kubelet's `configz` is not reachable from outside the node; it
        // has to be proxied through the apiserver's `nodes/<n>/proxy`.
        assert_eq!(
            configz_raw_path("node-1"),
            "/api/v1/nodes/node-1/proxy/configz"
        );
    }

    #[test]
    fn configz_fields_degrade_independently() {
        // INVARIANT: a <1.34 kubelet legitimately reports `failSwapOn` and no
        // `swapBehavior`. Reporting both `unknown` there would hide the one
        // fact that IS known — and it is the fact that says whether the
        // drop-in landed at all.
        assert_eq!(
            configz_fields(r#"{"failSwapOn":false,"swapBehavior":"NoSwap"}"#),
            (Field::Known("false".into()), Field::Known("NoSwap".into()))
        );
        assert_eq!(
            configz_fields(r#"{"failSwapOn":false}"#),
            (Field::Known("false".into()), Field::Unknown)
        );
        assert_eq!(configz_fields("{}"), (Field::Unknown, Field::Unknown));
    }

    #[test]
    fn node_field_args_pass_the_jsonpath_as_the_value_of_dash_o() {
        // kubectl wants `-o` and `jsonpath={…}` as SEPARATE argv slots; gluing
        // them (`-o=jsonpath…`) or dropping the `jsonpath=` prefix makes
        // kubectl print the whole Node object instead of the one field.
        let args = node_field_args("node-1", "{.status.nodeInfo.kubeletVersion}");
        assert_eq!(
            args,
            vec![
                "get",
                "node",
                "node-1",
                "-o",
                "jsonpath={.status.nodeInfo.kubeletVersion}"
            ]
        );
    }

    #[test]
    fn gomemlimit_probe_failure_degrades_instead_of_parsing_an_error() {
        // A failed SSH must never be handed to the parser — its error text
        // could contain anything.
        assert_eq!(
            gomemlimit_from_probe(Ok("Environment=GOMEMLIMIT=2GiB".into())),
            Field::Known("2GiB".into())
        );
        assert_eq!(
            gomemlimit_from_probe(Err(CliError::Other(
                "ssh root@host command failed: GOMEMLIMIT=bogus".into()
            ))),
            Field::Unknown,
            "a failed probe must degrade, not be parsed"
        );
    }

    #[test]
    fn swap_builder_none_is_labelled_internal() {
        // The gate passing while the builder declines is a contradiction
        // between two halves of one decision — a bug report, not operator
        // guidance.
        let msg = swap_builder_none_error().to_string();
        assert!(msg.starts_with("internal:"), "{msg}");
        assert!(msg.contains("eligibility gate"), "{msg}");
    }

    #[test]
    fn already_provisioned_notice_says_refresh_not_provision() {
        // On a healthy node the operator must be able to tell the idempotency
        // probes fired — a generic "applying…" line hides that.
        let msg = already_provisioned_notice("203.0.113.7");
        assert!(msg.contains("203.0.113.7"), "{msg}");
        assert!(msg.contains("already active"), "{msg}");
        assert!(msg.contains("refreshing"), "{msg}");
    }

    // ---- (q) the full status probe pass over a scripted remote shell ------

    /// A [`RemoteShell`] that answers from a fixed script and records every
    /// command it was asked to run. An unscripted command is an `Err`, which
    /// is exactly what a real node does for a command it cannot satisfy — so a
    /// typo'd probe string shows up as a degraded field rather than passing.
    struct MockShell {
        answers: Vec<(&'static str, &'static str)>,
        asked: std::cell::RefCell<Vec<String>>,
    }

    impl MockShell {
        fn new(answers: Vec<(&'static str, &'static str)>) -> Self {
            Self {
                answers,
                asked: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn unreachable() -> Self {
            Self::new(Vec::new())
        }
    }

    impl RemoteShell for MockShell {
        fn run_remote(&self, command: &str) -> Result<String> {
            self.asked.borrow_mut().push(command.to_string());
            match self.answers.iter().find(|(c, _)| *c == command) {
                Some((_, out)) => Ok((*out).to_string()),
                None => Err(CliError::Other(format!("no route to host: {command}"))),
            }
        }
    }

    #[test]
    fn status_probe_reads_every_ssh_field_off_the_node() {
        // INVARIANT: each `[ssh]` field has its OWN probe command, and every
        // one of them is issued. The probes are all best-effort
        // (`.ok()`/`unwrap_or_default()`), so a mistyped command does NOT
        // error — it silently yields `unknown`, which is why the command set
        // is pinned here rather than left to the live walk.
        let shell = MockShell::new(vec![
            ("hostname", "node-1\n"),
            (
                "swapon --show",
                "NAME      TYPE SIZE USED PRIO\n/swapfile file 4G   0B   -2\n",
            ),
            (
                "cat /etc/fstab",
                "UUID=x / ext4 defaults 0 1\n/swapfile none swap sw,nofail 0 0\n",
            ),
            ("test -e /swapfile", ""),
            ("sysctl -n vm.swappiness", "10\n"),
            (
                "systemctl show -p Environment k3s 2>/dev/null",
                "Environment=GOMEMLIMIT=2GiB\n",
            ),
            (
                "cat /var/lib/apprafter/swap-provision.status",
                "applied: swap 4096 MiB\n",
            ),
        ]);

        // No cached kubeconfig ⇒ the [api] fields short-circuit to Unknown
        // without spawning kubectl; the [ssh] half is what is under test.
        let s = probe_node_swap_state(&shell, None);

        assert!(s.ssh_available);
        assert_eq!(s.node_name, "node-1");
        assert_eq!(
            s.state,
            SwapProvisionState::Active {
                size: Some("4G".into())
            }
        );
        assert_eq!(s.swapon, Field::Known("/swapfile file 4G 0B -2".into()));
        assert_eq!(s.swappiness, Field::Known("10".into()));
        assert_eq!(s.gomemlimit, Field::Known("2GiB".into()));
        assert_eq!(
            s.provision_breadcrumb,
            Field::Known("applied: swap 4096 MiB".into())
        );
        // The kube-API half stays Unknown — it has no kubeconfig to read.
        assert_eq!(s.kubelet_version, Field::Unknown);
        assert_eq!(s.swap_behavior, Field::Unknown);

        // Every scripted probe was actually issued (none was dropped).
        let asked = shell.asked.borrow().clone();
        for (cmd, _) in &shell.answers {
            assert!(
                asked.iter().any(|a| a == cmd),
                "probe `{cmd}` was never run; ran {asked:?}"
            );
        }
    }

    #[test]
    fn status_probe_degrades_every_ssh_field_when_the_node_is_unreachable() {
        // INVARIANT (Q10 / decision 6): an unreachable node must still produce
        // a report — `ssh_available: false`, every [ssh] field `unknown`, a
        // `<unknown>` header, and the `Unknown` verdict — rather than failing
        // the command outright.
        let shell = MockShell::unreachable();
        let s = probe_node_swap_state(&shell, None);

        assert!(!s.ssh_available);
        assert_eq!(s.node_name, "<unknown>");
        assert_eq!(s.state, SwapProvisionState::Unknown);
        assert_eq!(s.swapon, Field::Unknown);
        assert_eq!(s.swappiness, Field::Unknown);
        assert_eq!(s.gomemlimit, Field::Unknown);
        assert_eq!(s.provision_breadcrumb, Field::Unknown);
        // A failed `test -e` must not be read as "the swapfile is there" —
        // that would report a phantom orphan on an unreachable node.
        assert_ne!(s.state, SwapProvisionState::OrphanSwapfile);

        let rendered = render_status(&s);
        assert!(rendered.contains("SSH unavailable"), "{rendered}");
    }

    #[test]
    fn status_probe_reports_a_node_whose_swap_never_came_back() {
        // The P9 `sw,nofail` trap end-to-end: the breadcrumb says applied, the
        // fstab entry is there, but `swapon` lists nothing and the file is
        // gone. That node has no cushion and must read as actionable.
        let shell = MockShell::new(vec![
            ("hostname", "node-1\n"),
            ("swapon --show", "NAME TYPE SIZE USED PRIO\n"),
            (
                "cat /etc/fstab",
                "UUID=x / ext4 defaults 0 1\n/swapfile none swap sw,nofail 0 0\n",
            ),
            ("sysctl -n vm.swappiness", "10\n"),
            (
                "cat /var/lib/apprafter/swap-provision.status",
                "applied (retrofit): swap 4096 MiB\n",
            ),
        ]);
        let s = probe_node_swap_state(&shell, None);
        assert!(s.ssh_available);
        assert_eq!(s.swapon, Field::Unknown);
        assert_eq!(s.state, SwapProvisionState::EligibleNotApplied);
    }

    #[test]
    fn configz_scalar_treats_an_empty_token_as_unknown() {
        // A present-but-empty value must not be reported as a blank Known —
        // `swapBehavior : ` reads as "configured to nothing".
        assert_eq!(
            configz_scalar(r#"{"swapBehavior":"","failSwapOn":false}"#, "swapBehavior"),
            Field::Unknown
        );
        assert_eq!(
            configz_scalar(r#"{"swapBehavior":,"x":1}"#, "swapBehavior"),
            Field::Unknown
        );
    }
}
