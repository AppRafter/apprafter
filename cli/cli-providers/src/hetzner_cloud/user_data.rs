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
//! The function is pure and side-effect-free; the caller passes
//! the result to `ServerSpec.user_data`.

/// Tier-1 defaults for the Hetzner cloud-init payload. Currently
/// no fields are exposed for tweaking — `Default::default()` is
/// the only constructor — but the struct exists so we can grow
/// knobs later without breaking the call site.
#[derive(Debug, Clone, Default)]
pub struct K3sBootstrapOptions {}

pub fn build_k3s_user_data(_opts: &K3sBootstrapOptions) -> String {
    "#cloud-config\n\
package_update: true\n\
package_upgrade: false\n\
packages:\n\
  - fail2ban\n\
runcmd:\n\
  - systemctl enable --now fail2ban\n\
  - 'curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC=\"--disable=traefik --disable=servicelb --disable-kube-proxy\" sh -'\n"
        .to_string()
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
    fn k3s_install_disables_traefik_servicelb_and_kube_proxy() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(s.contains("get.k3s.io"));
        assert!(s.contains("--disable=traefik"));
        assert!(s.contains("--disable=servicelb"));
        assert!(s.contains("--disable-kube-proxy"));
    }

    #[test]
    fn ends_with_newline_for_clean_yaml_concatenation() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(s.ends_with('\n'));
    }
}
