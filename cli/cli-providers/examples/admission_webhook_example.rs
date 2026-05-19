// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Renders the tier-1 admission-webhook manifest set with a
//! placeholder image, used to refresh
//! manifests/tier-1/admission-webhook/example.yaml.

use cli_providers::k8s::admission_webhook_yaml;

fn main() {
    print!(
        "{}",
        admission_webhook_yaml("ghcr.io/apprafter/admission-webhook:placeholder")
    );
}
