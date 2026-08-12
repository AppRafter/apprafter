// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Node public-IP readout for `apprafter target ip` (1.83i): pure extraction
//! over the Hetzner `Server.public_net` + a live `list_servers` wrapper.

use cli_core::Result;

use super::client::HetznerCloudClient;
use super::types::Server;

/// Hetzner returns IPv6 as a `<prefix>::/64` delegation; the node's own address
/// is `<prefix>::1`. Strip the `/64` and, if the base ends in `::`, append `1`;
/// otherwise return the base unchanged (a full address / non-`::` base passes
/// through).
pub fn node_ipv6_address(cidr: &str) -> String {
    let base = cidr.split('/').next().unwrap_or(cidr);
    if base.ends_with("::") {
        format!("{base}1")
    } else {
        base.to_string()
    }
}

/// Find the server by `server_id` and return (IPv4, node-IPv6) for DNS A/AAAA.
/// IPv4 is the plain address; IPv6 is `node_ipv6_address` of the `/64`. Either
/// may be `None` (server absent, no `public_net`, or that family missing).
pub fn extract_public_ips(servers: &[Server], server_id: u64) -> (Option<String>, Option<String>) {
    let Some(server) = servers.iter().find(|s| s.id == server_id) else {
        return (None, None);
    };
    let Some(public_net) = server.public_net.as_ref() else {
        return (None, None);
    };
    let v4 = public_net.ipv4.as_ref().map(|p| p.ip.clone());
    let v6 = public_net.ipv6.as_ref().map(|p| node_ipv6_address(&p.ip));
    (v4, v6)
}

/// Live: list servers + extract the node's public IPv4/IPv6 for `server_id`.
pub fn node_public_ips(
    client: &HetznerCloudClient,
    server_id: u64,
) -> Result<(Option<String>, Option<String>)> {
    let servers = client.list_servers()?.servers;
    Ok(extract_public_ips(&servers, server_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hetzner_cloud::types::{PublicIpv4, PublicIpv6, PublicNet, Server, ServerStatus};
    use std::collections::BTreeMap;

    fn server(id: u64, public_net: Option<PublicNet>) -> Server {
        Server {
            id,
            name: format!("srv-{id}"),
            status: ServerStatus::Running,
            labels: BTreeMap::new(),
            public_net,
            server_type: None,
            location: None,
        }
    }

    #[test]
    fn ipv6_address_derives_node_one_from_slash64() {
        assert_eq!(
            node_ipv6_address("2a01:4f8:c0c:abcd::/64"),
            "2a01:4f8:c0c:abcd::1"
        );
        assert_eq!(
            node_ipv6_address("2a01:4f8:c0c:abcd::5"),
            "2a01:4f8:c0c:abcd::5"
        );
        assert_eq!(
            node_ipv6_address("2001:db8::dead:beef/64"),
            "2001:db8::dead:beef"
        );
    }

    #[test]
    fn extract_finds_v4_and_derived_v6() {
        let servers = vec![
            server(1, None),
            server(
                42,
                Some(PublicNet {
                    ipv4: Some(PublicIpv4 {
                        ip: "203.0.113.10".into(),
                    }),
                    ipv6: Some(PublicIpv6 {
                        ip: "2a01:4f8:c0c:abcd::/64".into(),
                    }),
                }),
            ),
        ];
        let (v4, v6) = extract_public_ips(&servers, 42);
        assert_eq!(v4.as_deref(), Some("203.0.113.10"));
        assert_eq!(v6.as_deref(), Some("2a01:4f8:c0c:abcd::1"));
    }

    #[test]
    fn extract_handles_missing_cases() {
        assert_eq!(extract_public_ips(&[server(1, None)], 99), (None, None));
        assert_eq!(extract_public_ips(&[server(7, None)], 7), (None, None));
        let servers = vec![server(
            7,
            Some(PublicNet {
                ipv4: None,
                ipv6: Some(PublicIpv6 {
                    ip: "2a01:4f8::/64".into(),
                }),
            }),
        )];
        let (v4, v6) = extract_public_ips(&servers, 7);
        assert_eq!(v4, None);
        assert_eq!(v6.as_deref(), Some("2a01:4f8::1"));
    }
}
