// SPDX-License-Identifier: FSL-1.1-MIT
//! Implementation of the `Provider` trait against Hetzner Cloud.
//! Filled in by Task 8.

use crate::HetznerCloudClient;

#[derive(Debug)]
pub struct HetznerCloudProvider {
    pub client: HetznerCloudClient,
}
