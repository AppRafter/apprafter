// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What `apprafter node status` reads of the node: its kube API fields — the answers to
//! kubectl's jsonpath and `--raw` reads, as text, as kubectl prints them — and its shell's
//! answers to the seven probes the CLI sends over SSH.

use serde_json::{json, Map};

/// The node, as its kube API and its `hostname` name it.
pub const NODE: &str = super::top::NODE;
/// `status.nodeInfo.kubeletVersion`: a kubelet past the NoSwap gate (1.34).
pub const KUBELET_VERSION: &str = "v1.35.5+k3s1";
/// `status.nodeInfo.swap.capacity`: 4 GiB.
pub const SWAP_CAPACITY: &str = "4294967296";

/// The kubelet's `configz`, on one line as `kubectl get --raw` prints it.
pub fn configz(fail_swap_on: bool, swap_behavior: Option<&str>) -> String {
    let mut memory_swap = Map::new();
    if let Some(behavior) = swap_behavior {
        memory_swap.insert("swapBehavior".to_string(), json!(behavior));
    }
    json!({ "kubeletconfig": { "failSwapOn": fail_swap_on, "memorySwap": memory_swap } })
        .to_string()
}

/// The remote commands, exactly as `node status` sends them.
pub mod remote {
    /// The SSH reachability probe, and the node's name.
    pub const HOSTNAME: &str = "hostname";
    pub const SWAPON: &str = "swapon --show";
    pub const FSTAB: &str = "cat /etc/fstab";
    pub const SWAPFILE_EXISTS: &str = "test -e /swapfile";
    pub const SWAPPINESS: &str = "sysctl -n vm.swappiness";
    pub const GOMEMLIMIT: &str = "systemctl show -p Environment k3s 2>/dev/null";
    pub const BREADCRUMB: &str = "cat /var/lib/apprafter/swap-provision.status";
    /// All seven, in the order the CLI sends them.
    pub const ALL: [&str; 7] = [
        HOSTNAME,
        SWAPON,
        FSTAB,
        SWAPFILE_EXISTS,
        SWAPPINESS,
        GOMEMLIMIT,
        BREADCRUMB,
    ];
}

/// `swapon --show` with the 4 GiB swapfile active.
pub const SWAPON_ACTIVE: &str = "NAME      TYPE SIZE USED PRIO\n/swapfile file   4G   0B   -2\n";
/// `/etc/fstab` with the swapfile persisted, as 2.16g writes it.
pub const FSTAB_WITH_SWAP: &str =
    "LABEL=cloudimg-rootfs / ext4 discard,errors=remount-ro 0 1\n/swapfile none swap sw,nofail 0 0\n";
/// `/etc/fstab` of a node with no swap.
pub const FSTAB_WITHOUT_SWAP: &str = "LABEL=cloudimg-rootfs / ext4 discard,errors=remount-ro 0 1\n";
/// `vm.swappiness` as 2.16g sets it.
pub const SWAPPINESS: &str = "10\n";
/// The k3s unit's environment, with a memory limit for its Go runtime.
pub const K3S_ENVIRONMENT: &str = "Environment=GOMEMLIMIT=2GiB K3S_NODE_NAME=solo-1\n";
/// The k3s unit's environment without one.
pub const K3S_ENVIRONMENT_WITHOUT_GOMEMLIMIT: &str = "Environment=K3S_NODE_NAME=solo-1\n";
/// The breadcrumb the bootstrap's swap step leaves when it applied swap.
pub const BREADCRUMB_APPLIED: &str = "applied: /swapfile 4G, swappiness 10\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configz_is_one_line_with_what_it_was_given() {
        assert_eq!(
            configz(false, Some("NoSwap")),
            r#"{"kubeletconfig":{"failSwapOn":false,"memorySwap":{"swapBehavior":"NoSwap"}}}"#
        );
        assert_eq!(
            configz(true, None),
            r#"{"kubeletconfig":{"failSwapOn":true,"memorySwap":{}}}"#
        );
    }
}
