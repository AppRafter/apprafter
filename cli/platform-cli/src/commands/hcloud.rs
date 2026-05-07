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

#[cfg(test)]
mod tests {
    use super::*;

    // Both branches are exercised sequentially in one test because
    // cargo runs unit tests on multiple threads in a single process,
    // and concurrent set/unset of the same env var would race.
    #[test]
    fn honours_env_var_override_and_falls_back_to_default() {
        std::env::remove_var("APPRAFTER_HCLOUD_BASE_URL");
        assert_eq!(hcloud_base_url(), DEFAULT_BASE_URL);

        std::env::set_var("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:9999");
        assert_eq!(hcloud_base_url(), "http://127.0.0.1:9999");
        std::env::remove_var("APPRAFTER_HCLOUD_BASE_URL");
    }
}
