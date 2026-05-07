// SPDX-License-Identifier: FSL-1.1-MIT
//! Pure builder for the cloud-init `#cloud-config` user-data
//! attached to AppRafter-managed Hetzner Cloud servers.
//!
//! The current shape installs k3s in single-node mode (with
//! traefik + servicelb disabled because Cilium + Gateway API
//! replace them in phase 1.4), enables UFW with the AppRafter
//! port whitelist, and turns on fail2ban for the SSH jail. The
//! function is pure and side-effect-free; the caller passes
//! the result to `ServerSpec.user_data`.

/// Tier-1 defaults for the Hetzner cloud-init payload. Currently
/// no fields are exposed for tweaking — `Default::default()` is
/// the only constructor — but the struct exists so we can grow
/// knobs later without breaking the call site.
#[derive(Debug, Clone, Default)]
pub struct K3sBootstrapOptions {}

/// AppRafter port whitelist enforced by UFW inside the VM. Mirrors
/// the planned cloud-side firewall: ssh, kube-API, HTTP, HTTPS,
/// wireguard. (Wireguard is whitelisted ahead of phase 1.4 mesh
/// work so we don't have to revisit user-data when it lands.)
const UFW_TCP_PORTS: &[&str] = &["22", "6443", "80", "443"];
const UFW_UDP_PORTS: &[&str] = &["51820"];

pub fn build_k3s_user_data(_opts: &K3sBootstrapOptions) -> String {
    let mut ufw_rules = String::new();
    for p in UFW_TCP_PORTS {
        ufw_rules.push_str(&format!("  - ufw allow in {p}/tcp\n"));
    }
    for p in UFW_UDP_PORTS {
        ufw_rules.push_str(&format!("  - ufw allow in {p}/udp\n"));
    }

    format!(
        "#cloud-config\n\
package_update: true\n\
package_upgrade: false\n\
packages:\n\
  - ufw\n\
  - fail2ban\n\
runcmd:\n\
  - ufw default deny incoming\n\
  - ufw default allow outgoing\n\
{ufw_rules}\
  - ufw --force enable\n\
  - systemctl enable --now fail2ban\n\
  - 'curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC=\"--disable=traefik --disable=servicelb\" sh -'\n",
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
    fn declares_ufw_and_fail2ban_packages() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(s.contains("- ufw\n"));
        assert!(s.contains("- fail2ban\n"));
    }

    #[test]
    fn whitelists_every_apprafter_port_in_ufw_rules() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        for p in UFW_TCP_PORTS {
            assert!(
                s.contains(&format!("ufw allow in {p}/tcp")),
                "missing tcp/{p} rule:\n{s}"
            );
        }
        for p in UFW_UDP_PORTS {
            assert!(
                s.contains(&format!("ufw allow in {p}/udp")),
                "missing udp/{p} rule:\n{s}"
            );
        }
    }

    #[test]
    fn k3s_install_disables_traefik_and_servicelb() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(s.contains("get.k3s.io"));
        assert!(s.contains("--disable=traefik"));
        assert!(s.contains("--disable=servicelb"));
    }

    #[test]
    fn ends_with_newline_for_clean_yaml_concatenation() {
        let s = build_k3s_user_data(&K3sBootstrapOptions::default());
        assert!(s.ends_with('\n'));
    }
}
