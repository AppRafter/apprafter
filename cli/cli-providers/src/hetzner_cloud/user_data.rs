// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure builder for the cloud-init `#cloud-config` user-data
//! attached to AppRafter-managed Hetzner Cloud servers.
//!
//! Tier-1 firewall split:
//!
//! - **Network-edge default-deny + AppRafter port whitelist** is
//!   handled by the Hetzner Cloud Firewall (built by
//!   `apply.rs::build_firewall_spec`). It applies BEFORE packets
//!   reach the VM; same allow-list (22 + 6443 + 80 + 443 / tcp,
//!   51820 / udp), default-deny everything else.
//!
//! - **Log-driven abuse detection** stays in-VM via `fail2ban`.
//!   Orthogonal to network-edge filtering — fail2ban watches
//!   `/var/log/auth.log` (and later, app logs as we expose
//!   workloads via Gateway/HTTPRoute) and bans IPs that exceed
//!   thresholds.
//!
//! `ufw` was previously layered on top of both — strict
//! defense-in-depth duplicating the Hetzner Cloud Firewall. It
//! was removed in v0.1.43: `ufw allow …` calls in `runcmd` failed
//! silently at cloud-init time on Ubuntu 24.04 because
//! `iptables-nft` returns `Could not fetch rule set generation
//! id: Invalid argument` (`initcaps` error) before netfilter
//! modules are fully wired. Result: ufw came up with `ENABLED=yes`
//! and ZERO user-allow rules, locking out the whole VM. fail2ban
//! sidesteps the bug because its systemd unit starts after
//! `network-online.target`, when modules are stable.
//!
//! The k3s install line disables five default components that
//! `cluster-bootstrap` replaces with cluster-tier alternatives:
//!
//! - `--flannel-backend=none` — k3s's embedded flannel-vxlan
//!   daemon claims the same VXLAN UDP port (8472) that Cilium's
//!   `cilium_vxlan` device wants. Without this flag the Cilium
//!   agent crashes on `address already in use` setting up its
//!   datapath (root cause of the v0.1.45 fix).
//! - `--disable-network-policy` — k3s ships `kube-router` for
//!   `NetworkPolicy` enforcement; Cilium provides the same and
//!   running both writes conflicting iptables/nftables rules.
//! - `--disable-kube-proxy` — Cilium's kube-proxy replacement
//!   (eBPF) takes over service routing.
//! - `--disable=traefik` — Gateway API + Cilium gateway replace
//!   it (cluster-bootstrap installs the upstream Gateway CRDs).
//! - `--disable=servicelb` — Cilium L2 announcements replace the
//!   k3s default LoadBalancer implementation.
//!
//! ## Node reservations (2.16d)
//!
//! The k3s process (`k3s.service`, ~1.5 GiB measured on the solo
//! tier) runs in `system.slice`, **outside** `kubepods` — so
//! per-pod requests/limits never account for it. Without node
//! reservations the scheduler treats the whole node's memory as
//! schedulable and over-commits until the kernel OOM-killer fires,
//! and `k3s.service` (default `OOMScoreAdjust=0`) is a prime
//! victim. 2.16d closes both gaps:
//!
//! - **kubelet reservations** (`system-reserved`, `kube-reserved`,
//!   `eviction-hard`) are written to `/etc/rancher/k3s/config.yaml`
//!   via cloud-init `write_files` rather than passed as
//!   `--kubelet-arg=…` flags. The config-file form is what k3s
//!   recommends for values containing shell/YAML metacharacters —
//!   the `<` in `eviction-hard=memory.available<100Mi` would
//!   otherwise have to survive both the `INSTALL_K3S_EXEC="…"`
//!   double-quoting inside the single-quoted `runcmd` entry AND
//!   cloud-init's own YAML parse. A plain YAML list value sidesteps
//!   all of that. k3s reads `config.yaml` at install time (the file
//!   lands via `write_files`, which cloud-init runs before
//!   `runcmd`), so the reservations apply to the very first boot.
//! - **`OOMScoreAdjust=-999`** is set via a systemd drop-in at
//!   `/etc/systemd/system/k3s.service.d/oom.conf`. The k3s
//!   install-script unit does NOT set `OOMScoreAdjust` (verified
//!   against k3s `install.sh` — unlike rke2, which does), so the
//!   default of `0` leaves k3s equally OOM-killable as any pod.
//!   `-999` puts it just above the kernel's own `-1000` reserve so
//!   the control plane survives a node-level memory squeeze. A
//!   `systemctl daemon-reload` in `runcmd` picks the drop-in up
//!   (the k3s install runs `daemon-reload` itself before starting
//!   the unit, but we reload again defensively in case the drop-in
//!   is applied to an already-running unit by the retrofit path).
//!
//! The function is pure and side-effect-free; the caller passes
//! the result to `ServerSpec.user_data`.

/// Tier-1 defaults for the Hetzner cloud-init payload.
///
/// `dual_stack` controls whether k3s is installed with the dual
/// pod-CIDR / service-CIDR pair from ADR 0017. Default is `true`
/// (matches the platform-wide dual-stack-everywhere posture);
/// single-stack opt-out is plumbed through `Infrastructure.network.
/// ipFamilies` when the manifest layer grows that knob.
#[derive(Debug, Clone)]
pub struct K3sBootstrapOptions {
    pub dual_stack: bool,
    /// 2.16g: whether the provisioned node should bake host swap into
    /// its bootstrap (the no-swap-forgiving T1 default). `true` on the
    /// only tier shipped today (T1 single-node); T2+ multi-node is
    /// deferred (design decision 7), so the caller sets it `false` for
    /// anything that isn't a single-node cluster. When `true`,
    /// [`build_k3s_user_data`] installs k3s with `INSTALL_K3S_SKIP_START`
    /// and runs the FAIL-SOFT swap block before starting the unit
    /// (design decision 2 / Q2).
    pub swap_eligible: bool,
}

impl Default for K3sBootstrapOptions {
    fn default() -> Self {
        Self {
            dual_stack: true,
            // T1 single-node is the only tier today → swap on by default.
            swap_eligible: true,
        }
    }
}

/// Undocumented test hook: `APPRAFTER_SKIP_NODE_SWAP` (set to ANY value,
/// including empty) forces swap ineligible, letting the retrofit walk
/// provision a deliberately cushionless node without editing a manifest.
///
/// This is intentionally NOT surfaced in `--help` (design decision 1 /
/// Q17) — it exists only to drive the `apprafter node prep` retrofit path
/// in the e2e walk. Applied at the point the CLI reads/constructs
/// [`K3sBootstrapOptions`]; `base` is the tier-derived eligibility.
pub const SKIP_NODE_SWAP_ENV: &str = "APPRAFTER_SKIP_NODE_SWAP";

/// Applies the [`SKIP_NODE_SWAP_ENV`] override: returns `false` when the
/// env var is present (any value), else the `base` value unchanged.
pub fn swap_eligible_from_env(base: bool) -> bool {
    if std::env::var_os(SKIP_NODE_SWAP_ENV).is_some() {
        return false;
    }
    base
}

/// Dual-stack pod and service CIDRs per ADR 0017 §"Pod network".
/// `10.42.0.0/16` and `10.43.0.0/16` are k3s's own IPv4 defaults
/// (we re-state them to make the `,fd00:…` continuation valid).
/// IPv6 ranges are ULA (RFC 4193 `fd00::/8`), private to the
/// cluster — pod-to-pod traffic stays inside the node's /64
/// public delegation only when explicitly routed; cluster-internal
/// is on ULA which never escapes the cluster.
pub const CLUSTER_CIDR_DUAL_STACK: &str = "10.42.0.0/16,fd00:42::/64";
pub const SERVICE_CIDR_DUAL_STACK: &str = "10.43.0.0/16,fd00:43::/112";

/// Absolute path of the k3s config file that carries the kubelet
/// node reservations. k3s reads it at install and on every start.
pub const K3S_CONFIG_PATH: &str = "/etc/rancher/k3s/config.yaml";

/// Absolute path of the systemd drop-in that pins `k3s.service`
/// out of the kernel OOM-killer's default reach.
pub const K3S_OOM_DROPIN_PATH: &str = "/etc/systemd/system/k3s.service.d/oom.conf";

/// The systemd drop-in body applied at [`K3S_OOM_DROPIN_PATH`].
/// Shared by the bootstrap cloud-init and the `apprafter node prep`
/// retrofit so the two paths can never diverge.
///
/// 2.16g (ADR 0055): `GOMEMLIMIT=2GiB` caps the k3s Go-heap component of
/// a spike. k3s is Go; without it, host swap just lets the GC fault
/// swapped live-heap pages back in (thrash on 2 vCPU) instead of the
/// cushion absorbing the spike. This bounds the Go-HEAP component only —
/// the non-Go residual (cgo, sqlite/kine page cache) is what swap covers
/// (the coherent story vs `system-reserved=1500Mi`; the ~500Mi overshoot
/// is what the swap cushion absorbs). Starting value; tune from the walk.
pub const K3S_OOM_DROPIN: &str = "[Service]\nOOMScoreAdjust=-999\nEnvironment=GOMEMLIMIT=2GiB\n";

/// Renders the k3s `config.yaml` body carrying the 2.16d kubelet
/// node reservations. Pure and side-effect-free.
///
/// The three `kubelet-arg` list entries encode:
/// - `system-reserved=memory=1500Mi` — covers `k3s.service` (in
///   `system.slice`, outside `kubepods`; ~1.5 GiB measured).
/// - `kube-reserved=cpu=100m,memory=256Mi` — kubelet + containerd.
/// - `eviction-hard=memory.available<100Mi` — the hard-eviction
///   floor. The `<` is why this lives in YAML, not a shell flag.
///
/// This is the single source of truth for the reservation values —
/// [`build_k3s_user_data`] embeds it at bootstrap and the CLI
/// retrofit subcommand writes the identical body to a live node.
pub fn k3s_reservation_config() -> String {
    // Values from docs/measurements/2.16d-baseline-2026-08-08.md.
    "kubelet-arg:\n\
     \x20 - \"system-reserved=memory=1500Mi\"\n\
     \x20 - \"kube-reserved=cpu=100m,memory=256Mi\"\n\
     \x20 - \"eviction-hard=memory.available<100Mi\"\n"
        .to_string()
}

/// Absolute path of the kubelet drop-in that carries the swap
/// policy (2.16g). This is the **Option-A** managed directory k3s
/// reads directly (`/var/lib/rancher/k3s/agent/etc/kubelet.conf.d/`),
/// which exists after a `INSTALL_K3S_SKIP_START` install and before
/// the first kubelet start — so we write into it verbatim with NO
/// `--config-dir` kubelet-arg (design decision 2 / Q4). Option B's
/// `config-dir` would copy this file into the same managed dir and
/// leave a stale copy live on rollback; Option A has nothing to
/// copy or prune.
pub const SWAP_KUBELET_DROPIN_PATH: &str =
    "/var/lib/rancher/k3s/agent/etc/kubelet.conf.d/10-apprafter-swap.conf";

/// The 2.6c capacity threshold: the platform's own capacity signal
/// warns once free disk drops below 15% of total. The swap disk
/// pre-check refuses to create a swapfile that would trip it, so we
/// never succeed here and then immediately degrade the node's
/// capacity health (design decision 2 / Q8).
const SWAP_MIN_FREE_DISK_FRACTION: f64 = 0.15;

/// Upper bound on the swapfile size in MiB: `swap = min(RAM, 8Gi)`
/// (design decision 2). 8 GiB = 8192 MiB.
const SWAP_MAX_MIB: u64 = 8192;

/// Renders the kubelet drop-in body written at
/// [`SWAP_KUBELET_DROPIN_PATH`]. Pure and side-effect-free.
///
/// `failSwapOn: false` is emitted **unconditionally** — on a
/// swap-free node it is a harmless no-op, and writing it first
/// closes the "swap active in fstab / kubelet refuses to start"
/// brick window (design decision 2, apply order).
///
/// The `memorySwap: { swapBehavior: NoSwap }` block is emitted
/// **only** when `k8s_ge_134` is true: `NoSwap` is the GA behaviour
/// gated on Kubernetes ≥1.34, and emitting it on an older kubelet
/// would make the kubelet reject the config.
pub fn swap_kubelet_dropin(k8s_ge_134: bool) -> String {
    let mut out = String::new();
    out.push_str("apiVersion: kubelet.config.k8s.io/v1beta1\n");
    out.push_str("kind: KubeletConfiguration\n");
    out.push_str("failSwapOn: false\n");
    if k8s_ge_134 {
        out.push_str("memorySwap:\n");
        out.push_str("  swapBehavior: NoSwap\n");
    }
    out
}

/// Renders the POSIX shell script that provisions host swap for the
/// no-swap-forgiving node default (2.16g, design decisions 2 & 4).
/// Pure and side-effect-free — the caller runs the returned script
/// either from the SKIP_START bootstrap block (FAIL-SOFT) or the
/// `apprafter node prep` retrofit.
///
/// Returns `None` when swap is ineligible — either k8s is <1.34
/// (no `NoSwap` GA gate) or the node is not on cgroup v2
/// (`memory.swap.max` is a v2-only knob). The caller turns `None`
/// into a REFUSE-with-hint at the retrofit and a skip-with-breadcrumb
/// at bootstrap; this builder emits nothing in that case.
///
/// When eligible, the script performs — **in this order** — the
/// steps locked by design decision 2 (apply order) and 4:
/// 1. disk pre-check via `df`: refuse (non-zero exit, numeric error)
///    if free-after-swapfile would drop below the 2.6c 15% floor;
/// 2. compute `count = min(MemTotal_MiB, 8192)` from `mem_total_kib`;
/// 3. write `vm.swappiness=10` to a sysctl.d drop-in AND
///    `sysctl -w` it — BEFORE any swap is created (Q12);
/// 4. `dd … oflag=direct` under `ionice -c3`, with an
///    `EINVAL`/failure fallback to buffered `dd … conv=fsync` + warn;
/// 5. `chmod 600` / `mkswap` / `swapon`;
/// 6. immediately zero `memory.swap.max` on every existing pod
///    cgroup depth-agnostically (`find … -path '*kubepods*' -name
///    memory.swap.max`), tolerating ENOENT/EBUSY (never aborts);
/// 7. add the idempotent `sw,nofail` fstab entry.
pub fn swap_enable_script(mem_total_kib: u64, k8s_ge_134: bool, cgroup_v2: bool) -> Option<String> {
    if !k8s_ge_134 || !cgroup_v2 {
        return None;
    }

    // count = min(MemTotal_MiB, 8Gi). MemTotal is in KiB; 1 MiB = 1024 KiB.
    // The retrofit path KNOWS MemTotal at build time (it was read off the live
    // node before building the script), so it pins the count as a literal.
    let mem_total_mib = mem_total_kib / 1024;
    let count = mem_total_mib.min(SWAP_MAX_MIB);

    let mut s = String::new();
    s.push_str(&swap_script_header());
    s.push_str(&format!("SWAP_COUNT_MIB={count}\n"));
    s.push('\n');
    s.push_str(&swap_steps_body());
    Some(s)
}

/// The `#!/bin/sh` header + the "no `set -e`" contract comment, shared by
/// the retrofit ([`swap_enable_script`]) and the bootstrap in-shell variant.
fn swap_script_header() -> String {
    // Built by concatenation (not a `\`-continued literal) for the same
    // reason as `build_k3s_user_data`: Rust's line-continuation escape
    // eats leading whitespace, and this script's shell indentation is
    // load-bearing for readability. The script deliberately does NOT
    // `set -e` — the cgroup loop must tolerate ENOENT/EBUSY per-file
    // without aborting the caller (design decision 4).
    let mut s = String::new();
    s.push_str("#!/bin/sh\n");
    s.push_str("# AppRafter 2.16g: provision host swap + NoSwap-friendly kubelet.\n");
    s.push_str("# NOTE: intentionally no `set -e` — the pod-cgroup swap.max loop\n");
    s.push_str("# tolerates ENOENT/EBUSY per file and must not abort the caller.\n");
    s
}

/// Emits the shell that reads `MemTotal` from `/proc/meminfo` AT RUNTIME and
/// sets `SWAP_COUNT_MIB=min(MemTotal_MiB, 8192)` in-shell. Used by the
/// bootstrap path, which — unlike the retrofit — cannot know the node's RAM at
/// build time (design decision 2 / P13: derive count from `/proc/meminfo`).
fn swap_count_from_meminfo() -> String {
    let mut s = String::new();
    s.push_str("# Read MemTotal (KiB) from /proc/meminfo → MiB → cap at 8Gi.\n");
    s.push_str("MEM_TOTAL_KIB=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo)\n");
    s.push_str("SWAP_COUNT_MIB=$((MEM_TOTAL_KIB / 1024))\n");
    s.push_str(&format!(
        "if [ \"$SWAP_COUNT_MIB\" -gt {SWAP_MAX_MIB} ]; then SWAP_COUNT_MIB={SWAP_MAX_MIB}; fi\n"
    ));
    s
}

/// The swap provisioning steps (a)–(g), shared VERBATIM by the retrofit and
/// the bootstrap. `SWAP_COUNT_MIB` must already be assigned by the caller
/// (build-time literal for the retrofit, in-shell `/proc/meminfo` read for the
/// bootstrap) — the single point of divergence between the two paths.
fn swap_steps_body() -> String {
    // The 2.6c floor as a whole percentage for the human-readable message.
    let min_free_pct = (SWAP_MIN_FREE_DISK_FRACTION * 100.0) as u64;

    let mut s = String::new();

    // (a) Disk pre-check: refuse if free-after-swapfile < 15% of the disk.
    // `df -Pk /` → 1K-blocks; POSIX `-P` guarantees a single data line.
    s.push_str("# (a) Disk pre-check against the 2.6c 15% capacity floor.\n");
    s.push_str("DISK_TOTAL_KIB=$(df -Pk / | awk 'NR==2 {print $2}')\n");
    s.push_str("DISK_AVAIL_KIB=$(df -Pk / | awk 'NR==2 {print $4}')\n");
    s.push_str("SWAP_KIB=$((SWAP_COUNT_MIB * 1024))\n");
    s.push_str("FREE_AFTER_KIB=$((DISK_AVAIL_KIB - SWAP_KIB))\n");
    s.push_str(&format!(
        "MIN_FREE_KIB=$((DISK_TOTAL_KIB * {min_free_pct} / 100))\n"
    ));
    s.push_str("if [ \"$FREE_AFTER_KIB\" -lt \"$MIN_FREE_KIB\" ]; then\n");
    s.push_str(&format!(
        "  echo \"apprafter: refusing to create a ${{SWAP_COUNT_MIB}}MiB swapfile: \
free disk after it (${{FREE_AFTER_KIB}}KiB) would fall below the {min_free_pct}%% \
capacity floor (${{MIN_FREE_KIB}}KiB of ${{DISK_TOTAL_KIB}}KiB)\" >&2\n"
    ));
    s.push_str("  exit 1\n");
    s.push_str("fi\n");
    s.push('\n');

    // (c) swappiness=10 — persisted drop-in + live sysctl — BEFORE swapon,
    // so the default-60 window never overlaps `dd`/`swapon` (Q12).
    s.push_str("# (c) swappiness=10 BEFORE any swap is created (Q12).\n");
    s.push_str("echo 'vm.swappiness=10' > /etc/sysctl.d/99-apprafter-swap.conf\n");
    s.push_str("sysctl -w vm.swappiness=10\n");
    s.push('\n');

    // (d) Allocate the swapfile. Prefer O_DIRECT (bypasses page cache) under
    // ionice -c3 (idle class — protects the live-cluster IO window on the
    // retrofit). EINVAL (some filesystems reject O_DIRECT) → buffered fsync.
    s.push_str("# (d) Allocate /swapfile (O_DIRECT + ionice idle; buffered fallback).\n");
    s.push_str(
        "if ! ionice -c3 dd if=/dev/zero of=/swapfile bs=1M count=\"$SWAP_COUNT_MIB\" oflag=direct; then\n",
    );
    s.push_str(
        "  echo 'apprafter: O_DIRECT dd failed (EINVAL?), falling back to buffered dd conv=fsync' >&2\n",
    );
    s.push_str(
        "  ionice -c3 dd if=/dev/zero of=/swapfile bs=1M count=\"$SWAP_COUNT_MIB\" conv=fsync\n",
    );
    s.push_str("fi\n");
    s.push('\n');

    // (e) Format + activate.
    s.push_str("# (e) chmod / mkswap / swapon.\n");
    s.push_str("chmod 600 /swapfile\n");
    s.push_str("mkswap /swapfile\n");
    s.push_str("swapon /swapfile\n");
    s.push('\n');

    // (f) Zero swap on every existing pod cgroup IMMEDIATELY after swapon,
    // depth-agnostically — Burstable/BestEffort pods sit 3 levels deep, so a
    // fixed-depth glob would miss them. Per-file failures (ENOENT on
    // non-leaf cgroups, EBUSY) are swallowed; the loop never aborts.
    s.push_str("# (f) Zero memory.swap.max on existing pod cgroups (depth-agnostic).\n");
    s.push_str(
        "find /sys/fs/cgroup -path '*kubepods*' -name memory.swap.max -exec sh -c 'echo 0 > \"$1\" 2>/dev/null || true' _ {} \\;\n",
    );
    s.push('\n');

    // (g) Persist to fstab idempotently — never append a duplicate line.
    s.push_str("# (g) Persist to fstab (sw,nofail) idempotently.\n");
    s.push_str("if ! grep -qE '^/swapfile[[:space:]]' /etc/fstab; then\n");
    s.push_str("  echo '/swapfile none swap sw,nofail 0 0' >> /etc/fstab\n");
    s.push_str("fi\n");

    s
}

/// Absolute path of the FAIL-SOFT breadcrumb the bootstrap swap block drops on
/// any failure (design decision 2 / Q2). `apprafter node status` reads it to
/// tell "swap step failed at provision" apart from "ineligible". Its presence
/// after a boot means the swap block ran and failed but the node was still
/// started (the cushionless-but-working guarantee).
pub const SWAP_PROVISION_STATUS_PATH: &str = "/var/lib/apprafter/swap-provision.status";

/// Absolute path of the bootstrap swap script shipped via `write_files` and
/// invoked FAIL-SOFT from `runcmd`. Shipping it as a file (not an inline
/// `runcmd` one-liner) sidesteps all the nested-quoting hazards — the script's
/// bodies use single-quoted `awk`/`find`/`grep` patterns that a one-line
/// `sh -c '…'` runcmd entry could not carry. cloud-init runs `write_files`
/// before `runcmd`, so the file is present when the runcmd invokes it.
pub const SWAP_PROVISION_SCRIPT_PATH: &str = "/var/lib/apprafter/provision-swap.sh";

/// Renders the bootstrap swap SCRIPT body (written verbatim to
/// [`SWAP_PROVISION_SCRIPT_PATH`] via `write_files`). It runs AT RUNTIME on the
/// node: reads the k8s version off the SKIP_START-installed `k3s` binary
/// (≥1.34?) and cgroup v2 via `stat -fc %T /sys/fs/cgroup`; on an ineligible
/// gate it drops the breadcrumb and exits 0 (skip, not fail); when eligible it
/// writes the Option-A kubelet drop-in and runs the swap steps (a)–(g) with the
/// count derived in-shell from `/proc/meminfo`. Pure and side-effect-free.
///
/// FAIL-SOFT is the CALLER's job: [`build_k3s_user_data`] invokes this script
/// from `runcmd` guarded so any non-zero exit only logs + records the
/// [`SWAP_PROVISION_STATUS_PATH`] breadcrumb and never aborts the runcmd
/// (design decision 2 / Q2), so `systemctl start k3s` still runs after it.
fn build_bootstrap_swap_script() -> String {
    let mut body = String::new();
    body.push_str(&swap_script_header());
    body.push('\n');

    // Ensure the breadcrumb directory exists (best-effort).
    body.push_str("mkdir -p /var/lib/apprafter 2>/dev/null || true\n");
    body.push('\n');

    // --- Runtime gate (design decision 2). Both must pass or we skip swap. ---
    body.push_str("# Gate (a): kubelet/k8s version >=1.34 read off the SKIP_START binary.\n");
    body.push_str("K3S_VER=$(k3s --version 2>/dev/null | awk '/k3s version/ {print $3}')\n");
    // Strip a leading `v`, then compare major.minor numerically (1.34+).
    body.push_str("K3S_MAJOR=$(echo \"$K3S_VER\" | sed 's/^v//' | cut -d. -f1)\n");
    body.push_str("K3S_MINOR=$(echo \"$K3S_VER\" | sed 's/^v//' | cut -d. -f2)\n");
    body.push('\n');
    body.push_str("# Gate (b): cgroup v2 -- memory.swap.max is a v2-only knob.\n");
    body.push_str("CGROUP_FS=$(stat -fc %T /sys/fs/cgroup 2>/dev/null)\n");
    body.push('\n');

    body.push_str(
        "if [ -z \"$K3S_MAJOR\" ] || [ -z \"$K3S_MINOR\" ] || \
[ \"$K3S_MAJOR\" -lt 1 ] || { [ \"$K3S_MAJOR\" -eq 1 ] && [ \"$K3S_MINOR\" -lt 34 ]; }; then\n",
    );
    body.push_str(
        "  echo \"apprafter: swap ineligible -- k3s $K3S_VER < 1.34 (NoSwap GA gate); skipping swap, node starts cushionless\" >&2\n",
    );
    body.push_str(&format!(
        "  echo \"ineligible: k3s $K3S_VER < 1.34\" > {SWAP_PROVISION_STATUS_PATH} 2>/dev/null || true\n"
    ));
    body.push_str("  exit 0\n");
    body.push_str("fi\n");
    body.push_str("if [ \"$CGROUP_FS\" != \"cgroup2fs\" ]; then\n");
    body.push_str(
        "  echo \"apprafter: swap ineligible -- /sys/fs/cgroup is not cgroup v2 ($CGROUP_FS); skipping swap, node starts cushionless\" >&2\n",
    );
    body.push_str(&format!(
        "  echo \"ineligible: cgroup $CGROUP_FS != cgroup2fs\" > {SWAP_PROVISION_STATUS_PATH} 2>/dev/null || true\n"
    ));
    body.push_str("  exit 0\n");
    body.push_str("fi\n");
    body.push('\n');

    // --- Eligible: write the Option-A kubelet drop-in (>=1.34 → NoSwap). ---
    body.push_str("# Write the Option-A kubelet drop-in (failSwapOn:false + NoSwap >=1.34).\n");
    body.push_str(&format!(
        "mkdir -p \"$(dirname {SWAP_KUBELET_DROPIN_PATH})\"\n"
    ));
    // Heredoc the drop-in body (k8s_ge_134 is TRUE here — the gate guaranteed it).
    body.push_str(&format!(
        "cat > {SWAP_KUBELET_DROPIN_PATH} <<'APPRAFTER_KUBELET_EOF'\n"
    ));
    body.push_str(&swap_kubelet_dropin(true));
    body.push_str("APPRAFTER_KUBELET_EOF\n");
    body.push('\n');

    // --- Run the swap steps: count from /proc/meminfo (in-shell), then a–g. ---
    body.push_str(&swap_count_from_meminfo());
    body.push('\n');
    body.push_str(&swap_steps_body());
    body.push('\n');
    body.push_str(&format!(
        "echo \"applied: swap $SWAP_COUNT_MIB MiB\" > {SWAP_PROVISION_STATUS_PATH} 2>/dev/null || true\n"
    ));

    body
}

pub fn build_k3s_user_data(opts: &K3sBootstrapOptions) -> String {
    let install_exec = if opts.dual_stack {
        format!(
            "--flannel-backend=none --disable-network-policy --disable-kube-proxy --disable=traefik --disable=servicelb --cluster-cidr={} --service-cidr={}",
            CLUSTER_CIDR_DUAL_STACK, SERVICE_CIDR_DUAL_STACK
        )
    } else {
        "--flannel-backend=none --disable-network-policy --disable-kube-proxy --disable=traefik --disable=servicelb".to_string()
    };

    // Both files ship as cloud-init `write_files` YAML block
    // literal scalars (`content: |`). A block literal is verbatim —
    // the `<` in `eviction-hard=memory.available<100Mi`, the inner
    // double-quotes, and the newlines are all preserved with no
    // shell or YAML-scalar escaping (this is exactly why the
    // reservations go in a file, not a `--kubelet-arg=…` flag on
    // the double-quoted `INSTALL_K3S_EXEC` inside a single-quoted
    // runcmd entry). cloud-init runs `write_files` before `runcmd`,
    // so k3s's install script finds /etc/rancher/k3s/config.yaml
    // already present and folds the reservations into the very
    // first kubelet start.
    //
    // Built by concatenation, NOT a `\`-continued format literal:
    // Rust's line-continuation escape eats the leading whitespace of
    // the continuation line, which would silently strip the YAML
    // indentation the `write_files` mapping keys and block scalars
    // depend on. List items sit at column 0 under their parent key
    // (`- path:` → sibling keys at col 2, block content at col 4).
    let mut out = String::new();
    out.push_str("#cloud-config\n");
    out.push_str("package_update: true\n");
    out.push_str("package_upgrade: false\n");
    out.push_str("packages:\n");
    out.push_str("  - fail2ban\n");
    out.push_str("write_files:\n");
    out.push_str(&write_files_entry(
        K3S_CONFIG_PATH,
        &k3s_reservation_config(),
    ));
    out.push_str(&write_files_entry(K3S_OOM_DROPIN_PATH, K3S_OOM_DROPIN));
    // 2.16g: on a swap-eligible node, ship the FAIL-SOFT swap script alongside
    // the reservation files. It runs from `runcmd` after the SKIP_START install
    // but before `systemctl start k3s` (design decision 2). Executable so the
    // runcmd can invoke it directly.
    if opts.swap_eligible {
        out.push_str(&write_files_entry_mode(
            SWAP_PROVISION_SCRIPT_PATH,
            &build_bootstrap_swap_script(),
            "0755",
        ));
    }
    out.push_str("runcmd:\n");
    out.push_str("  - systemctl enable --now fail2ban\n");

    if opts.swap_eligible {
        // Swap-eligible flow (design decision 2 / Q2):
        // 1. Install with SKIP_START — binary + unit present, NOT started; the
        //    unit is still `enable`d, so `systemctl start k3s` below suffices
        //    (Q13 — NOT `enable --now`).
        out.push_str(&format!(
            "  - 'curl -sfL https://get.k3s.io | INSTALL_K3S_SKIP_START=true INSTALL_K3S_EXEC=\"{install_exec}\" sh -'\n"
        ));
        out.push_str("  - systemctl daemon-reload\n");
        // 2. Run the FAIL-SOFT swap block: a non-zero exit only logs + records
        //    the breadcrumb, never aborts the runcmd (`|| { … }` swallows it).
        out.push_str(&format!(
            "  - 'sh {SWAP_PROVISION_SCRIPT_PATH} || {{ echo \"apprafter: swap provisioning failed; node will start cushionless\" >&2; echo \"failed: swap step errored at provision\" > {SWAP_PROVISION_STATUS_PATH} 2>/dev/null || true; }}'\n"
        ));
        // 3. Start k3s UNCONDITIONALLY — a swap failure (or gate-skip) must
        //    leave a WORKING cushionless node, never an unstarted one (Q2).
        out.push_str("  - systemctl start k3s\n");
    } else {
        // Ineligible (T2+ / env-skip): the pre-2.16g one-shot install — the
        // install script starts k3s itself. Reservations + OOM drop-in still
        // apply (they ship via write_files above, unconditionally).
        out.push_str(&format!(
            "  - 'curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC=\"{install_exec}\" sh -'\n"
        ));
        out.push_str("  - systemctl daemon-reload\n");
    }
    out
}

/// Renders one cloud-init `write_files` list entry (path +
/// permissions + a `content: |` block literal). `body` is emitted
/// verbatim as a block scalar indented one level under `content`.
fn write_files_entry(path: &str, body: &str) -> String {
    write_files_entry_mode(path, body, "0644")
}

/// Like [`write_files_entry`] but with an explicit octal `permissions`
/// string — the swap provisioning script ships `0755` so `runcmd` can
/// invoke it directly.
fn write_files_entry_mode(path: &str, body: &str, permissions: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("  - path: {path}\n"));
    out.push_str(&format!("    permissions: '{permissions}'\n"));
    out.push_str("    content: |\n");
    // Block-scalar lines are indented 6 spaces — two levels past the
    // `- ` item marker (col 0..2) and the `content:` key (col 4).
    for line in body.split_inclusive('\n') {
        out.push_str("      ");
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_cloud_config_header() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(s.starts_with("#cloud-config\n"), "{s}");
    }

    #[test]
    fn k3s_user_data_includes_node_reservations() {
        let ud = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(ud.contains("kube-reserved"), "{ud}");
        assert!(
            ud.contains("system-reserved=memory=1500Mi") || ud.contains("system-reserved"),
            "{ud}"
        );
        assert!(ud.contains("eviction-hard"), "{ud}");
        // k3s's install-script unit does NOT set OOMScoreAdjust (unlike
        // rke2 — verified against k3s install.sh, R4-N4), so we ship the
        // drop-in that pins k3s.service out of the kernel OOM-killer's reach.
        assert!(ud.contains("OOMScoreAdjust=-999"), "{ud}");
        // 2.16g (ADR 0055): GOMEMLIMIT caps the k3s Go-heap so host swap
        // absorbs a spike instead of the GC thrashing swapped live-heap.
        assert!(ud.contains("Environment=GOMEMLIMIT="), "{ud}");
    }

    #[test]
    fn declares_only_fail2ban_in_packages_block() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(
            s.contains("- fail2ban\n"),
            "fail2ban must be installed: {s}"
        );
        assert!(
            !s.contains("- ufw\n"),
            "ufw must NOT be in packages — Hetzner Cloud Firewall is the network-edge defense, ufw added a silent-fail path on early-boot iptables-nft (see v0.1.43 changelog).\n{s}"
        );
    }

    #[test]
    fn runcmd_does_not_invoke_ufw_anywhere() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(
            !s.contains("ufw "),
            "no `ufw …` shell call must appear in the rendered cloud-config — initcaps bug at runcmd time silently breaks the host firewall.\n{s}"
        );
    }

    #[test]
    fn runcmd_invokes_fail2ban_systemctl_then_k3s_install() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        let f2b_idx = s
            .find("systemctl enable --now fail2ban")
            .expect("fail2ban systemctl enable must be in runcmd");
        let k3s_idx = s
            .find("get.k3s.io")
            .expect("k3s install curl must be in runcmd");
        assert!(
            f2b_idx < k3s_idx,
            "fail2ban must enable BEFORE k3s install (k3s pulls in containerd which adds its own iptables; fail2ban startup needs to land in a clean state first).\n{s}"
        );
    }

    #[test]
    fn k3s_install_disables_default_components_replaced_by_cluster_bootstrap() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(s.contains("get.k3s.io"));
        // Replaced by Cilium: CNI + NetworkPolicy + kube-proxy.
        assert!(
            s.contains("--flannel-backend=none"),
            "flannel must be disabled — k3s flannel-vxlan claims UDP 8472, Cilium's cilium_vxlan needs the same port; without this flag Cilium agent crashes on `address already in use` (see v0.1.45 changelog).\n{s}"
        );
        assert!(
            s.contains("--disable-network-policy"),
            "k3s NetworkPolicy (kube-router) must be disabled — Cilium provides its own enforcement and running both writes conflicting iptables rules.\n{s}"
        );
        assert!(
            s.contains("--disable-kube-proxy"),
            "kube-proxy must be disabled — Cilium's eBPF replacement takes over service routing.\n{s}"
        );
        // Replaced by cluster-bootstrap'd components.
        assert!(
            s.contains("--disable=traefik"),
            "traefik must be disabled — Gateway API + Cilium gateway replace it.\n{s}"
        );
        assert!(
            s.contains("--disable=servicelb"),
            "servicelb must be disabled — Cilium L2 announcements replace the default LoadBalancer.\n{s}"
        );
    }

    #[test]
    fn ends_with_newline_for_clean_yaml_concatenation() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn default_options_install_dual_stack_per_adr_0017() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(
            s.contains("--cluster-cidr=10.42.0.0/16,fd00:42::/64"),
            "dual-stack cluster-CIDR must be in install line so pods get IPv4 + IPv6 interfaces (ADR 0017 §Pod network).\n{s}"
        );
        assert!(
            s.contains("--service-cidr=10.43.0.0/16,fd00:43::/112"),
            "dual-stack service-CIDR must be in install line so Services accept both families (ADR 0017 §Service network).\n{s}"
        );
    }

    #[test]
    fn single_stack_flag_drops_dual_stack_cidr_args() {
        let s = build_k3s_user_data(&K3sBootstrapOptions {
            dual_stack: false,
            ..Default::default()
        });
        assert!(
            !s.contains("--cluster-cidr"),
            "single-stack opt-out must NOT inject cluster-CIDR — k3s falls back to its IPv4-only default.\n{s}"
        );
        assert!(
            !s.contains("--service-cidr"),
            "single-stack opt-out must NOT inject service-CIDR.\n{s}"
        );
        // Other disable flags must remain regardless of stack mode.
        assert!(s.contains("--flannel-backend=none"), "{s}");
        assert!(s.contains("--disable-kube-proxy"), "{s}");
    }

    // ---- 2.16g swap_eligible + env hook ---------------------------------

    #[test]
    fn swap_eligible_defaults_true_for_t1_single_node() {
        // T1 single-node is the only tier today → swap is on by default.
        assert!(
            K3sBootstrapOptions::default().swap_eligible,
            "swap_eligible must default true"
        );
    }

    // A single test drives all env-hook cases sequentially under one guard —
    // `std::env::set_var` is process-wide, so we save/restore and never rely
    // on cargo's parallel test isolation for the same variable.
    #[test]
    fn skip_node_swap_env_hook_forces_ineligible_and_passes_through_when_unset() {
        let saved = std::env::var("APPRAFTER_SKIP_NODE_SWAP").ok();

        // Set (any value) → forces false regardless of base.
        std::env::set_var("APPRAFTER_SKIP_NODE_SWAP", "1");
        assert!(!swap_eligible_from_env(true), "env set must force false");
        assert!(!swap_eligible_from_env(false), "env set stays false");

        // "Any value" — even an empty string set counts as present.
        std::env::set_var("APPRAFTER_SKIP_NODE_SWAP", "");
        assert!(
            !swap_eligible_from_env(true),
            "empty-but-set still forces false"
        );

        // Unset → the base value passes through unchanged.
        std::env::remove_var("APPRAFTER_SKIP_NODE_SWAP");
        assert!(swap_eligible_from_env(true), "unset passes base=true");
        assert!(!swap_eligible_from_env(false), "unset passes base=false");

        // Restore whatever the outer environment had.
        match saved {
            Some(v) => std::env::set_var("APPRAFTER_SKIP_NODE_SWAP", v),
            None => std::env::remove_var("APPRAFTER_SKIP_NODE_SWAP"),
        }
    }

    // ---- 2.16g bootstrap swap flow --------------------------------------

    #[test]
    fn bootstrap_installs_with_skip_start_when_swap_eligible() {
        let ud = build_k3s_user_data(&K3sBootstrapOptions {
            dual_stack: true,
            swap_eligible: true,
        });
        assert!(
            ud.contains("INSTALL_K3S_SKIP_START=true"),
            "swap-eligible install must SKIP_START so the swap block runs before first kubelet start\n{ud}"
        );
    }

    #[test]
    fn bootstrap_gates_swap_on_version_and_cgroup_at_runtime() {
        let ud = build_k3s_user_data(&K3sBootstrapOptions {
            dual_stack: true,
            swap_eligible: true,
        });
        // Version gate read AT RUNTIME on the node off the SKIP_START binary.
        assert!(
            ud.contains("k3s --version"),
            "swap block must read the k8s version at runtime via `k3s --version`\n{ud}"
        );
        // cgroup v2 gate: stat -fc %T /sys/fs/cgroup == cgroup2fs.
        assert!(
            ud.contains("cgroup2fs"),
            "swap block must gate on cgroup v2 (cgroup2fs)\n{ud}"
        );
        // MemTotal read in-shell (bootstrap can't know it at build time).
        assert!(
            ud.contains("/proc/meminfo") || ud.contains("MemTotal"),
            "swap block must read MemTotal at runtime from /proc/meminfo\n{ud}"
        );
    }

    #[test]
    fn bootstrap_swap_block_is_fail_soft_with_breadcrumb() {
        let ud = build_k3s_user_data(&K3sBootstrapOptions {
            dual_stack: true,
            swap_eligible: true,
        });
        // Breadcrumb marker so `node status` can distinguish
        // "swap step failed at provision" from "ineligible" (Q2).
        assert!(
            ud.contains("/var/lib/apprafter/swap-provision.status"),
            "a swap failure must drop the breadcrumb marker file\n{ud}"
        );
    }

    #[test]
    fn bootstrap_writes_the_kubelet_dropin_and_swap_logic_when_eligible() {
        let ud = build_k3s_user_data(&K3sBootstrapOptions {
            dual_stack: true,
            swap_eligible: true,
        });
        // The kubelet drop-in Option-A managed path is written.
        assert!(
            ud.contains(SWAP_KUBELET_DROPIN_PATH),
            "swap block must write the kubelet drop-in to the Option-A path\n{ud}"
        );
        assert!(
            ud.contains("failSwapOn: false"),
            "kubelet drop-in body must set failSwapOn:false\n{ud}"
        );
        // The swapfile logic (reused from Task 1) must be present.
        assert!(ud.contains("mkswap /swapfile"), "{ud}");
        assert!(ud.contains("swapon /swapfile"), "{ud}");
        assert!(ud.contains("vm.swappiness=10"), "{ud}");
        assert!(
            ud.contains("/swapfile none swap sw,nofail 0 0"),
            "swap block must persist the fstab entry\n{ud}"
        );
    }

    #[test]
    fn bootstrap_starts_k3s_unconditionally_after_the_swap_block() {
        let ud = build_k3s_user_data(&K3sBootstrapOptions {
            dual_stack: true,
            swap_eligible: true,
        });
        // A swap failure (or gate-skip) must leave a WORKING node — so the
        // unit is started unconditionally AFTER the fail-soft swap block (Q2).
        let start_idx = ud
            .find("systemctl start k3s")
            .expect("must start k3s explicitly (SKIP_START leaves the unit enabled)");
        let swap_marker_idx = ud
            .find("/var/lib/apprafter/swap-provision.status")
            .expect("swap block present");
        assert!(
            swap_marker_idx < start_idx,
            "systemctl start k3s must come AFTER the fail-soft swap block\n{ud}"
        );
        // Q13: NOT `enable --now` — the unit is already enabled by SKIP_START.
        assert!(
            !ud.contains("systemctl enable --now k3s"),
            "must not `enable --now` k3s — the unit is already enabled by SKIP_START\n{ud}"
        );
    }

    #[test]
    fn ineligible_bootstrap_has_no_swap_tokens_but_keeps_reservations_and_starts_node() {
        let ud = build_k3s_user_data(&K3sBootstrapOptions {
            dual_stack: true,
            swap_eligible: false,
        });
        // No swap machinery at all.
        assert!(
            !ud.contains("/swapfile"),
            "no swapfile when ineligible\n{ud}"
        );
        assert!(
            !ud.contains("vm.swappiness"),
            "no swappiness tuning when ineligible\n{ud}"
        );
        assert!(
            !ud.contains(SWAP_KUBELET_DROPIN_PATH),
            "no kubelet swap drop-in when ineligible\n{ud}"
        );
        assert!(
            !ud.contains("/var/lib/apprafter/swap-provision.status"),
            "no swap breadcrumb when ineligible\n{ud}"
        );
        // Reservations are UNCONDITIONAL.
        assert!(ud.contains("system-reserved=memory=1500Mi"), "{ud}");
        assert!(ud.contains("OOMScoreAdjust=-999"), "{ud}");
        // The node still comes up — k3s ends up running either way. The
        // ineligible path uses the pre-2.16g one-shot install (NO SKIP_START),
        // so the install script starts k3s itself — no explicit start needed.
        assert!(
            ud.contains("get.k3s.io"),
            "k3s must still install when ineligible\n{ud}"
        );
        assert!(
            !ud.contains("INSTALL_K3S_SKIP_START"),
            "ineligible path must use the one-shot install that starts k3s itself\n{ud}"
        );
    }

    // ---- 2.16g swap builders --------------------------------------------

    #[test]
    fn swap_dropin_is_the_option_a_managed_dir_path() {
        // Option A writes into k3s's managed kubelet.conf.d dir directly,
        // with NO --config-dir kubelet-arg (design decision 2 / Q4).
        assert_eq!(
            SWAP_KUBELET_DROPIN_PATH,
            "/var/lib/rancher/k3s/agent/etc/kubelet.conf.d/10-apprafter-swap.conf"
        );
    }

    #[test]
    fn swap_dropin_declares_kubelet_configuration_kind_and_api_version() {
        let d = swap_kubelet_dropin(true);
        assert!(
            d.contains("apiVersion: kubelet.config.k8s.io/v1beta1"),
            "{d}"
        );
        assert!(d.contains("kind: KubeletConfiguration"), "{d}");
    }

    #[test]
    fn swap_dropin_sets_fail_swap_on_false_in_both_variants() {
        // failSwapOn:false is UNCONDITIONAL — a no-op on a swap-free node,
        // but closing the fstab-swap-active / kubelet-refuses-start brick.
        let with_gate = swap_kubelet_dropin(true);
        let without_gate = swap_kubelet_dropin(false);
        assert!(
            with_gate.contains("failSwapOn: false"),
            "≥1.34 variant must set failSwapOn:false\n{with_gate}"
        );
        assert!(
            without_gate.contains("failSwapOn: false"),
            "<1.34 variant must STILL set failSwapOn:false\n{without_gate}"
        );
    }

    #[test]
    fn swap_dropin_emits_noswap_only_on_the_ge_134_variant() {
        let with_gate = swap_kubelet_dropin(true);
        let without_gate = swap_kubelet_dropin(false);
        assert!(
            with_gate.contains("swapBehavior: NoSwap"),
            "≥1.34 variant must gate NoSwap behaviour\n{with_gate}"
        );
        assert!(
            with_gate.contains("memorySwap:"),
            "≥1.34 variant must carry the memorySwap block\n{with_gate}"
        );
        assert!(
            !without_gate.contains("swapBehavior"),
            "<1.34 variant must NOT emit swapBehavior — an older kubelet rejects it\n{without_gate}"
        );
        assert!(
            !without_gate.contains("memorySwap"),
            "<1.34 variant must NOT emit the memorySwap block\n{without_gate}"
        );
    }

    #[test]
    fn swap_script_is_none_when_below_134() {
        // 4 GiB RAM, cgroup v2, but k8s <1.34 → ineligible → skip swap.
        assert!(swap_enable_script(4 * 1024 * 1024, false, true).is_none());
    }

    #[test]
    fn swap_script_is_none_when_not_cgroup_v2() {
        // memory.swap.max is a cgroup-v2-only knob → cgroup v1 → skip swap.
        assert!(swap_enable_script(4 * 1024 * 1024, true, false).is_none());
    }

    #[test]
    fn swap_script_is_none_when_both_gates_fail() {
        assert!(swap_enable_script(4 * 1024 * 1024, false, false).is_none());
    }

    #[test]
    fn swap_script_present_when_both_gates_true() {
        assert!(swap_enable_script(4 * 1024 * 1024, true, true).is_some());
    }

    #[test]
    fn swap_script_writes_swappiness_before_swapon() {
        // Apply order (Q12): swappiness=10 must be set BEFORE any swap is on.
        let s = swap_enable_script(4 * 1024 * 1024, true, true).expect("eligible");
        let swappiness_idx = s
            .find("vm.swappiness=10")
            .expect("must set vm.swappiness=10");
        let swapon_idx = s.find("swapon /swapfile").expect("must swapon");
        assert!(
            swappiness_idx < swapon_idx,
            "swappiness must be written before swapon so the default-60 window never overlaps dd/swapon\n{s}"
        );
        // AND both forms are present: persisted drop-in + live sysctl -w.
        assert!(s.contains("sysctl -w vm.swappiness=10"), "{s}");
        assert!(s.contains("/etc/sysctl.d/"), "{s}");
    }

    #[test]
    fn swap_script_disk_precheck_precedes_dd() {
        // The df pre-check must run before the swapfile is allocated (Q8) —
        // never succeed and then trip the 2.6c capacity signal.
        let s = swap_enable_script(4 * 1024 * 1024, true, true).expect("eligible");
        let df_idx = s.find("df -Pk").expect("must pre-check disk via df");
        let dd_idx = s.find("dd if=/dev/zero").expect("must dd the swapfile");
        assert!(
            df_idx < dd_idx,
            "the df disk pre-check must precede dd\n{s}"
        );
        // The refusal names the 15% floor and exits non-zero.
        assert!(s.contains("15%"), "error must name the 15% floor\n{s}");
        assert!(s.contains("exit 1"), "pre-check must exit non-zero\n{s}");
    }

    #[test]
    fn swap_script_dd_has_odirect_ionice_and_buffered_fallback() {
        let s = swap_enable_script(4 * 1024 * 1024, true, true).expect("eligible");
        assert!(
            s.contains("ionice -c3"),
            "dd must run under ionice -c3\n{s}"
        );
        assert!(s.contains("oflag=direct"), "dd must prefer O_DIRECT\n{s}");
        assert!(
            s.contains("conv=fsync"),
            "must fall back to buffered dd conv=fsync on EINVAL\n{s}"
        );
    }

    #[test]
    fn swap_script_formats_and_activates_the_swapfile() {
        let s = swap_enable_script(4 * 1024 * 1024, true, true).expect("eligible");
        assert!(s.contains("chmod 600 /swapfile"), "{s}");
        assert!(s.contains("mkswap /swapfile"), "{s}");
        assert!(s.contains("swapon /swapfile"), "{s}");
    }

    #[test]
    fn swap_script_zeroes_pod_cgroup_swap_depth_agnostically_after_swapon() {
        // Depth-agnostic find loop (design decision 4) — a fixed-depth glob
        // misses 3-level-deep Burstable/BestEffort pods. Must come AFTER
        // swapon (swap usage is 0 at that instant) and tolerate ENOENT/EBUSY.
        let s = swap_enable_script(4 * 1024 * 1024, true, true).expect("eligible");
        assert!(
            s.contains("find /sys/fs/cgroup -path '*kubepods*' -name memory.swap.max"),
            "must zero swap.max across all kubepods cgroups depth-agnostically\n{s}"
        );
        assert!(
            !s.contains("*/*/memory.swap.max"),
            "must NOT use a fixed-depth glob (misses Burstable/BestEffort)\n{s}"
        );
        let swapon_idx = s.find("swapon /swapfile").expect("must swapon");
        let loop_idx = s.find("-path '*kubepods*'").expect("cgroup loop present");
        assert!(
            swapon_idx < loop_idx,
            "the cgroup zero-out must run immediately AFTER swapon\n{s}"
        );
        // Tolerate per-file failures (never abort the caller).
        assert!(s.contains("2>/dev/null || true"), "{s}");
        // The script must not `set -e`-abort. Check for a `set -e` *command*
        // line (trimmed), not a mere substring — the explanatory comment
        // legitimately names the flag.
        assert!(
            !s.lines().any(|l| {
                let t = l.trim();
                t == "set -e" || t == "set -eu" || t == "set -euo pipefail"
            }),
            "the script must not `set -e`-abort on the cgroup loop\n{s}"
        );
    }

    #[test]
    fn swap_script_fstab_entry_is_nofail_and_idempotent() {
        let s = swap_enable_script(4 * 1024 * 1024, true, true).expect("eligible");
        assert!(
            s.contains("/swapfile none swap sw,nofail 0 0"),
            "fstab entry must be sw,nofail\n{s}"
        );
        // Guarded by a grep so a re-run never appends a duplicate line.
        assert!(
            s.contains("grep -qE '^/swapfile[[:space:]]' /etc/fstab"),
            "fstab write must be idempotent (grep-guarded)\n{s}"
        );
    }

    #[test]
    fn swap_script_count_is_full_ram_when_below_8gib() {
        // 4 GiB RAM = 4 * 1024 * 1024 KiB → count = 4096 MiB (min(4096, 8192)).
        let s = swap_enable_script(4 * 1024 * 1024, true, true).expect("eligible");
        assert!(
            s.contains("SWAP_COUNT_MIB=4096"),
            "small-RAM node → swap = full RAM (4096 MiB)\n{s}"
        );
    }

    #[test]
    fn swap_script_count_is_capped_at_8gib_for_large_ram() {
        // 32 GiB RAM = 32 * 1024 * 1024 KiB → count = min(32768, 8192) = 8192.
        let s = swap_enable_script(32 * 1024 * 1024, true, true).expect("eligible");
        assert!(
            s.contains("SWAP_COUNT_MIB=8192"),
            "large-RAM node → swap capped at 8192 MiB\n{s}"
        );
        assert!(
            !s.contains("SWAP_COUNT_MIB=32768"),
            "must NOT size swap to full 32 GiB\n{s}"
        );
    }
}
