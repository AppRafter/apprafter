// SPDX-License-Identifier: FSL-1.1-MIT
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

pub fn build_k3s_user_data(opts: &K3sBootstrapOptions) -> String {
    let install_exec = if opts.dual_stack {
        format!(
            "--flannel-backend=none --disable-network-policy --disable-kube-proxy --disable=traefik --disable=servicelb --cluster-cidr={} --service-cidr={}",
            CLUSTER_CIDR_DUAL_STACK, SERVICE_CIDR_DUAL_STACK
        )
    } else {
        "--flannel-backend=none --disable-network-policy --disable-kube-proxy --disable=traefik --disable=servicelb".to_string()
    };
    format!(
        "#cloud-config\n\
package_update: true\n\
package_upgrade: false\n\
packages:\n\
  - fail2ban\n\
runcmd:\n\
  - systemctl enable --now fail2ban\n\
  - 'curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC=\"{}\" sh -'\n",
        install_exec
    )
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
