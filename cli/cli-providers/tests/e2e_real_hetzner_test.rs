// SPDX-License-Identifier: FSL-1.1-MIT
//! End-to-end test against a real Hetzner Cloud project.
//!
//! Skipped by default (`#[ignore]`). Run with:
//!
//!     APPRAFTER_HCLOUD_E2E=1 \
//!     HCLOUD_TOKEN=… \
//!     cargo test -p cli-providers --test hetzner_cloud_test \
//!         e2e_real_hetzner_test -- --ignored
//!
//! The test creates a CX22 server, asserts a second apply is a
//! no-op, and tears it down on the way out.

use std::collections::BTreeMap;

use cli_providers::hetzner_cloud::{
    HetznerCloudClient, HetznerCloudProvider, ServerSpec, DEFAULT_BASE_URL,
};
use cli_providers::Provider;

#[test]
#[ignore = "real Hetzner Cloud — set APPRAFTER_HCLOUD_E2E=1 + HCLOUD_TOKEN to run"]
fn create_then_destroy_real_cpx22() {
    if std::env::var("APPRAFTER_HCLOUD_E2E").as_deref() != Ok("1") {
        eprintln!("skip: APPRAFTER_HCLOUD_E2E is not set to 1");
        return;
    }
    let token = match std::env::var("HCLOUD_TOKEN") {
        Ok(t) => t,
        Err(_) => {
            eprintln!("skip: HCLOUD_TOKEN is not set");
            return;
        }
    };

    // Use a unique server name so parallel runs don't collide.
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let name = format!("apprafter-e2e-{suffix}");

    let mut labels = BTreeMap::new();
    labels.insert("apprafter-e2e".into(), name.clone());

    let provider = HetznerCloudProvider {
        client: HetznerCloudClient::new(DEFAULT_BASE_URL, token),
        spec: ServerSpec {
            name: name.clone(),
            server_type: "cpx22".into(),
            image: "ubuntu-24.04".into(),
            location: "nbg1".into(),
            labels,
            user_data: None,
        },
        ssh_keys: vec![],
        networks: vec![],
        firewalls: vec![],
        floating_ips: vec![],
    };

    // First apply: should create.
    let first = provider.apply().expect("first apply");
    assert_eq!(first.applied, 1);

    // Second apply: should be no-op.
    let second = provider.apply().expect("second apply");
    assert_eq!(second.applied, 0, "second apply should be idempotent");

    // Cleanup. (Manual smoke test: panic in earlier step skips
    // cleanup; we accept this trade-off in the skeleton.)
    let destroyed = provider.destroy().expect("destroy");
    assert!(destroyed.destroyed >= 1);
}
