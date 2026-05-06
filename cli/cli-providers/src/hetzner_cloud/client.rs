// SPDX-License-Identifier: FSL-1.1-MIT
//! Thin blocking HTTP wrapper around the Hetzner Cloud API.
//!
//! Implementation lands in Task 4; this file currently only
//! declares the type so the module compiles.

#[derive(Debug, Clone)]
pub struct HetznerCloudClient {
    pub base_url: String,
    pub token: String,
}
