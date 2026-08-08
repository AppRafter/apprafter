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
}

impl Default for K3sBootstrapOptions {
    fn default() -> Self {
        Self { dual_stack: true }
    }
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
/// Shared by the bootstrap cloud-init (Task 10) and the
/// `apprafter node reserve-headroom` retrofit (Task 11) so the two
/// paths can never diverge.
pub const K3S_OOM_DROPIN: &str = "[Service]\nOOMScoreAdjust=-999\n";

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
    out.push_str(&write_files_entry(K3S_CONFIG_PATH, &k3s_reservation_config()));
    out.push_str(&write_files_entry(K3S_OOM_DROPIN_PATH, K3S_OOM_DROPIN));
    out.push_str("runcmd:\n");
    out.push_str("  - systemctl enable --now fail2ban\n");
    out.push_str(&format!(
        "  - 'curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC=\"{install_exec}\" sh -'\n"
    ));
    out.push_str("  - systemctl daemon-reload\n");
    out
}

/// Renders one cloud-init `write_files` list entry (path +
/// permissions + a `content: |` block literal). `body` is emitted
/// verbatim as a block scalar indented one level under `content`.
fn write_files_entry(path: &str, body: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("  - path: {path}\n"));
    out.push_str("    permissions: '0644'\n");
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
        let s = build_k3s_user_data(&K3sBootstrapOptions { dual_stack: false });
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
}

