// SPDX-License-Identifier: FSL-1.1-MIT
//! Renders the tier-1 Application CRD with the v1alpha1 schema,
//! used to refresh manifests/tier-1/application/example-crd.yaml.

use cli_providers::k8s::application_crd_yaml;

fn main() {
    print!("{}", application_crd_yaml());
}
