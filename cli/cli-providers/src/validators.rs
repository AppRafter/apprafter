// SPDX-License-Identifier: FSL-1.1-MIT
//! Provider validator framework — cheap, read-only credential
//! checks that catch bad tokens before any side-effecting CLI
//! flow runs.
//!
//! Per `cli-dx-task.md` §11, every provider plugs in a validator
//! that the CLI invokes during `apprafter target add` and similar
//! flows. The trait stays small on purpose: format checks live in
//! provider-specific helpers (e.g. `cli_core::target::
//! validate_hetzner_token_format`), API ping lives here, region /
//! type lookups arrive in Track A.4b when the wizard needs them.
//!
//! For Hetzner Cloud the canonical ping is `GET /v1/locations` —
//! a list of datacenters that every valid `Authorization` token
//! can read, no quota spent, no resources touched.

use crate::hetzner_cloud::HetznerCloudClient;
use cli_core::Result;

/// Common surface every provider validator exposes to the CLI.
///
/// `validate_credentials` is the read-only "does this token
/// authenticate?" check. Track A.4b will add wizard-driving
/// helpers (`list_regions`, etc.) — keeping the trait minimal
/// now means each new provider gets one method to implement and
/// no API-shape commitment we have to walk back later.
pub trait ProviderValidator {
    /// Hit a lightweight, read-only endpoint to confirm the
    /// carried credentials authenticate. Returns `Ok(())` on a
    /// 2xx response; surfaces the provider's typed error
    /// otherwise (mapped through `CliError`).
    fn validate_credentials(&self) -> Result<()>;
}

/// Hetzner Cloud validator. Probes `GET /v1/locations` — Hetzner's
/// own recommendation for cheap auth checks. The struct owns the
/// `HetznerCloudClient` outright; construction is the only place
/// where the base URL + token live next to each other, which keeps
/// integration tests easy (point `base_url` at a `mockito::Server`,
/// run `validate_credentials()`, assert).
pub struct HetznerCloudValidator {
    client: HetznerCloudClient,
}

impl HetznerCloudValidator {
    /// Build a validator against the supplied base URL + token.
    /// Production callers use `cli_providers::hetzner_cloud::
    /// DEFAULT_BASE_URL`; tests use a `mockito::Server.url()`.
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            client: HetznerCloudClient::new(base_url, token),
        }
    }
}

impl ProviderValidator for HetznerCloudValidator {
    fn validate_credentials(&self) -> Result<()> {
        // The response body is intentionally discarded — we only
        // care about the 2xx vs 4xx/5xx classification at this
        // layer. Track A.4b will surface the locations into the
        // region picker via a separate method.
        self.client.list_locations().map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal LocationListResponse JSON Hetzner returns. Stripped
    /// to a single location with all required wire-type fields so
    /// serde doesn't trip on missing keys.
    const LOCATIONS_FIXTURE: &str = r#"{
        "locations": [
            {
                "id": 1,
                "name": "fsn1",
                "description": "Falkenstein DC Park 1",
                "country": "DE",
                "city": "Falkenstein",
                "network_zone": "eu-central"
            }
        ]
    }"#;

    #[test]
    fn validate_credentials_returns_ok_on_200() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", "/v1/locations")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(LOCATIONS_FIXTURE)
            .create();
        let v = HetznerCloudValidator::new(server.url(), "test-token");
        assert!(v.validate_credentials().is_ok());
    }

    #[test]
    fn validate_credentials_surfaces_hetzner_401_with_typed_error() {
        // Real Hetzner 401 response when the token is wrong /
        // revoked — we want the canonical `CliError::Hetzner`
        // surface so error UX can stay consistent across CLI
        // entry points.
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", "/v1/locations")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"code":"unauthorized","message":"unable to authenticate"}}"#)
            .create();
        let v = HetznerCloudValidator::new(server.url(), "bogus-token");
        let err = v.validate_credentials().expect_err("401 must error");
        match err {
            cli_core::CliError::Hetzner {
                status,
                code,
                message,
                ..
            } => {
                assert_eq!(status, 401);
                assert_eq!(code, "unauthorized");
                assert!(
                    message.to_lowercase().contains("authenticate"),
                    "got: {message}"
                );
            }
            other => panic!("expected CliError::Hetzner, got {other:?}"),
        }
    }

    #[test]
    fn validate_credentials_surfaces_transport_error_when_endpoint_unreachable() {
        // Point at a closed port → ureq returns a Transport error
        // that we surface as the generic `CliError::Other`.
        // Network unreachable / DNS-fail / connection-refused all
        // collapse into this branch (cli-dx-task.md §10: error UX
        // for "network unreachable" is the responsibility of the
        // caller; here we just guarantee a typed surface).
        let v = HetznerCloudValidator::new("http://127.0.0.1:1", "test-token");
        let err = v
            .validate_credentials()
            .expect_err("unreachable host must error");
        match err {
            cli_core::CliError::Other(msg) => {
                assert!(
                    msg.to_lowercase().contains("transport"),
                    "expected transport-flagged message, got: {msg}"
                );
            }
            cli_core::CliError::Hetzner { .. } => {
                // Some environments may translate ECONNREFUSED to
                // a synthetic 5xx (e.g., behind a corporate
                // proxy). Surfacing it as `CliError::Hetzner` is
                // also acceptable as long as it isn't silently
                // succeeding.
            }
            other => panic!("expected Other or Hetzner, got {other:?}"),
        }
    }
}
