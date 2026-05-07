// SPDX-License-Identifier: FSL-1.1-MIT
//! Shared helpers for Hetzner-touching commands (apply / destroy /
//! import). Currently just `hcloud_base_url()`.

use cli_providers::hetzner_cloud::DEFAULT_BASE_URL;

/// Resolve the Hetzner Cloud API base URL.
///
/// Honours `APPRAFTER_HCLOUD_BASE_URL` when set (used by integration
/// tests to point the CLI at a `mockito` server) and falls back to
/// the upstream URL otherwise.
pub fn hcloud_base_url() -> String {
    std::env::var("APPRAFTER_HCLOUD_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}
