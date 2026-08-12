// SPDX-License-Identifier: FSL-1.1-Apache-2.0
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

/// Provider-agnostic region descriptor. Surfaced as a `Select`
/// picker in the interactive wizard (Track A.4b) and reused by
/// `init` / `apply` flag-completion in Track A.8 once they
/// resolve to the active target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionInfo {
    /// Short ID used by the provider API (Hetzner: `nbg1`).
    /// This is what the CLI persists in `TargetConfig.region` and
    /// passes back to the provider on subsequent calls.
    pub name: String,
    /// Human-readable label (Hetzner: `Nuremberg DC Park 1`).
    /// `Display` for `Select` uses `name — description` so the
    /// picker is scannable without the user having to memorise IDs.
    pub description: String,
}

impl std::fmt::Display for RegionInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.description.is_empty() {
            f.write_str(&self.name)
        } else {
            write!(f, "{} — {}", self.name, self.description)
        }
    }
}

/// Common surface every provider validator exposes to the CLI.
///
/// `validate_credentials` is the read-only "does this token
/// authenticate?" check. `list_regions` drives the wizard's
/// region picker (Track A.4b) and stays optional for providers
/// where regions don't apply (returns an empty `Vec` is OK).
/// `list_machine_offers` returns the joined (SKU × location)
/// catalog used by the machine picker and the validator so both
/// surfaces share a single HTTP call path. Keeping the trait
/// small means each new provider gets a tiny surface to implement.
pub trait ProviderValidator {
    /// Hit a lightweight, read-only endpoint to confirm the
    /// carried credentials authenticate. Returns `Ok(())` on a
    /// 2xx response; surfaces the provider's typed error
    /// otherwise (mapped through `CliError`).
    fn validate_credentials(&self) -> Result<()>;

    /// Enumerate the provider's regions for the wizard's
    /// `Select` picker. Mirrors `validate_credentials` in surface
    /// shape: 2xx → list, error → typed `CliError`. Hetzner's
    /// answer is sorted by `name` so the picker is alphabetical
    /// without each call site re-sorting.
    fn list_regions(&self) -> Result<Vec<RegionInfo>>;

    /// Return the joined (SKU × location) machine offer catalog.
    /// Each row is one `MachineOffer` — see `crate::machine` for
    /// the struct definition. Providers without a typed catalog
    /// return an empty `Vec`. The picker and any validator that
    /// needs server-type data call this method so the HTTP path
    /// is shared and mocked uniformly in tests.
    fn list_machine_offers(&self) -> Result<Vec<crate::machine::MachineOffer>>;
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
        // layer. `list_regions` below uses the same endpoint when
        // the caller needs the actual list.
        self.client.list_locations().map(|_| ())
    }

    fn list_regions(&self) -> Result<Vec<RegionInfo>> {
        let resp = self.client.list_locations()?;
        let mut regions: Vec<RegionInfo> = resp
            .locations
            .into_iter()
            .map(|loc| RegionInfo {
                name: loc.name,
                description: loc.description,
            })
            .collect();
        regions.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(regions)
    }

    fn list_machine_offers(&self) -> Result<Vec<crate::machine::MachineOffer>> {
        let resp = self.client.list_server_types()?;
        Ok(crate::machine::offers_from_server_types(&resp.server_types))
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
    fn list_regions_returns_sorted_region_info_from_locations_response() {
        // Two locations on purpose, second one alphabetically
        // before the first to pin the sort step.
        let body = r#"{
            "locations": [
                {"id":1,"name":"nbg1","description":"Nuremberg DC Park 1",
                 "country":"DE","city":"Nuremberg","network_zone":"eu-central"},
                {"id":2,"name":"fsn1","description":"Falkenstein DC Park 1",
                 "country":"DE","city":"Falkenstein","network_zone":"eu-central"}
            ]
        }"#;
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", "/v1/locations")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create();
        let v = HetznerCloudValidator::new(server.url(), "test-token");
        let regions = v.list_regions().expect("list_regions ok");
        assert_eq!(regions.len(), 2);
        // Sorted alphabetically — `fsn1` before `nbg1` even though
        // the wire payload had them flipped.
        assert_eq!(regions[0].name, "fsn1");
        assert_eq!(regions[0].description, "Falkenstein DC Park 1");
        assert_eq!(regions[1].name, "nbg1");
        // Display impl: `<name> — <description>`.
        assert_eq!(
            regions[0].to_string(),
            "fsn1 — Falkenstein DC Park 1",
            "Display impl drives inquire::Select labels"
        );
    }

    #[test]
    fn region_info_display_falls_back_to_name_when_description_empty() {
        let r = RegionInfo {
            name: "nbg1".into(),
            description: String::new(),
        };
        assert_eq!(r.to_string(), "nbg1");
    }

    #[test]
    fn list_machine_offers_returns_joined_catalog_from_server_types() {
        // Single server type with one location+price, one location without price.
        // Mirrors the join shape exercised in machine::tests so we pin that the
        // validator wires through `offers_from_server_types` correctly.
        let body = r#"{
            "server_types": [
                {
                    "id": 1,
                    "name": "cx22",
                    "architecture": "x86",
                    "cpu_type": "shared",
                    "cores": 2,
                    "memory": 4.0,
                    "disk": 40,
                    "deprecation": null,
                    "locations": [
                        {"name": "nbg1", "available": true,  "recommended": true},
                        {"name": "fsn1", "available": false, "recommended": false}
                    ],
                    "prices": [
                        {
                            "location": "nbg1",
                            "price_monthly": {"net": "3.9200", "gross": "4.6648"},
                            "price_hourly":  {"net": "0.0066", "gross": "0.0079"}
                        }
                    ]
                }
            ],
            "meta": {"pagination": {"page": 1, "per_page": 50, "next_page": null}}
        }"#;
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", "/v1/server_types")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create();
        let v = HetznerCloudValidator::new(server.url(), "test-token");
        let offers = v.list_machine_offers().expect("list_machine_offers ok");
        assert_eq!(offers.len(), 2, "one row per (sku, location)");
        let nbg = offers
            .iter()
            .find(|o| o.location == "nbg1" && o.sku == "cx22")
            .expect("nbg1/cx22 row must exist");
        assert!(nbg.available);
        assert_eq!(nbg.price_monthly_net.as_deref(), Some("3.9200"));
        let fsn = offers
            .iter()
            .find(|o| o.location == "fsn1" && o.sku == "cx22")
            .expect("fsn1/cx22 row must exist");
        assert!(!fsn.available);
        assert!(
            fsn.price_monthly_net.is_none(),
            "no price entry for fsn1 → None"
        );
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
