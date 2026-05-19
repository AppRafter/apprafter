// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Renders the tier-1 Backstage manifest set with placeholder
//! values, used to refresh manifests/tier-1/backstage/example.yaml.

use cli_providers::k8s::{backstage_manifests_yaml, BACKSTAGE_DEFAULT_IMAGE};

fn main() {
    print!(
        "{}",
        backstage_manifests_yaml("backstage.example.com", BACKSTAGE_DEFAULT_IMAGE)
    );
}
