// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The machine catalogue (D.3 overview §3.7.4): regions, machine offers and region latencies,
//! as the add wizard and the machine picker show them, and the SKU check `target add` and
//! `target machine` make before they save a server type.

use std::time::Duration;

use cli_providers::hetzner_cloud::types::ServerType;
use cli_providers::hetzner_cloud::HetznerCloudClient;
use serde::Serialize;

use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult};
use crate::target_ref::TargetRef;
use crate::CancellationToken;

/// Server types are per location; a SKU checked without a region is checked here (the same
/// default the rest of the CLI provisions into).
pub const DEFAULT_REGION: &str = "nbg1";

/// Each latency probe's bound (DNS + TCP connect).
pub const LATENCY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Whose token reads the catalogue: one given (the add wizard, before the target exists) or a
/// stored target's.
#[derive(Debug, Clone, Copy)]
pub enum CatalogueSource<'a> {
    Token {
        provider: &'a str,
        token: &'a SecretString,
    },
    Target(&'a TargetRef),
}

/// One provider region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RegionView {
    /// The provider's code, e.g. `nbg1`.
    pub code: String,
    pub city: String,
    pub country: String,
    pub description: String,
}

/// A machine type the provider has announced it will retire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DeprecationView {
    pub announced: Option<String>,
    pub unavailable_after: Option<String>,
}

/// One machine type in one region.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct MachineOfferView {
    pub location: String,
    pub sku: String,
    pub cores: u32,
    pub memory_gb: f64,
    pub disk_gb: u32,
    pub arch: String,
    pub cpu_type: String,
    pub price_monthly_net: Option<String>,
    pub price_hourly_net: Option<String>,
    pub available: bool,
    pub recommended: bool,
    pub deprecation: Option<DeprecationView>,
    /// `unavailable_after` has passed.
    pub retired: bool,
}

/// Every region and every offer the provider lists.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct MachineCatalogue {
    pub regions: Vec<RegionView>,
    pub offers: Vec<MachineOfferView>,
}

/// How far a region is from this computer; `None` when the probe got no answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RegionLatency {
    pub region: String,
    pub latency_ms: Option<u32>,
}

impl MachineOfferView {
    /// The CLI picker's row type (`cli_providers::machine::MachineOffer`), field for field.
    pub fn to_offer(&self) -> cli_providers::machine::MachineOffer {
        cli_providers::machine::MachineOffer {
            location: self.location.clone(),
            sku: self.sku.clone(),
            cores: self.cores,
            memory_gb: self.memory_gb,
            disk_gb: self.disk_gb,
            arch: self.arch.clone(),
            cpu_type: self.cpu_type.clone(),
            price_monthly_net: self.price_monthly_net.clone(),
            price_hourly_net: self.price_hourly_net.clone(),
            available: self.available,
            recommended: self.recommended,
            deprecation: self.deprecation.as_ref().map(|d| {
                cli_providers::hetzner_cloud::types::Deprecation {
                    announced: d.announced.clone(),
                    unavailable_after: d.unavailable_after.clone(),
                }
            }),
        }
    }
}

fn view(o: &cli_providers::machine::MachineOffer) -> MachineOfferView {
    MachineOfferView {
        location: o.location.clone(),
        sku: o.sku.clone(),
        cores: o.cores,
        memory_gb: o.memory_gb,
        disk_gb: o.disk_gb,
        arch: o.arch.clone(),
        cpu_type: o.cpu_type.clone(),
        price_monthly_net: o.price_monthly_net.clone(),
        price_hourly_net: o.price_hourly_net.clone(),
        available: o.available,
        recommended: o.recommended,
        deprecation: o.deprecation.as_ref().map(|d| DeprecationView {
            announced: d.announced.clone(),
            unavailable_after: d.unavailable_after.clone(),
        }),
        retired: o.is_retired(),
    }
}

/// `CliError::Hetzner` passes through (its status reaches the UI, bug 11); anything else of a
/// provider read is `ProviderRequestFailed` naming the endpoint (overview §3.6.1).
pub(crate) fn provider_read_error(e: cli_core::CliError, endpoint: &str) -> CoreError {
    match e {
        e @ cli_core::CliError::Hetzner { .. } => CoreError::Cli(e),
        other => CoreError::ProviderRequestFailed {
            provider: "hetzner-cloud".into(),
            endpoint: endpoint.into(),
            cause: Box::new(CoreError::from(other)),
        },
    }
}

/// `/v1/server_types`, page by page, `cancel` checked before each request.
fn server_types(
    client: &HetznerCloudClient,
    cancel: &CancellationToken,
) -> CoreResult<Vec<ServerType>> {
    let (mut all, mut page) = (Vec::new(), 1u32);
    loop {
        cancel.check()?;
        let (types, next) = client
            .list_server_types_page(page)
            .map_err(|e| provider_read_error(e, "GET /v1/server_types"))?;
        all.extend(types);
        match next {
            Some(n) => page = n,
            None => return Ok(all),
        }
    }
}

/// The SKU check of `execute_add` / `execute_machine`: server types only (no `/v1/locations`).
/// `context` names the command, so a refusal carries it (its `UiError` field `context`, and the
/// CLI's help).
pub(crate) fn check_sku(
    ctx: &Context,
    token: &SecretString,
    sku: &str,
    region: &str,
    context: cli_core::SkuCheckFor,
    cancel: &CancellationToken,
) -> CoreResult<()> {
    let types = server_types(&ctx.hetzner_client(token), cancel)?;
    Ok(cli_providers::hetzner_cloud::validate_server_type(
        &types, sku, region, context,
    )?)
}

/// Regions and offers for a picker. `Token` is the wizard's (a token being added), `Target` a
/// stored target's (the CLI override, else the stored token — R4). Locations first, then server
/// types page by page; `cancel` checked before every request; each request bounded by the
/// context's agent.
pub fn catalogue(
    ctx: &Context,
    source: CatalogueSource<'_>,
    cancel: &CancellationToken,
) -> CoreResult<MachineCatalogue> {
    let (provider, token) = match source {
        CatalogueSource::Token { provider, token } => (provider.to_string(), token.clone()),
        CatalogueSource::Target(t) => (
            cli_core::target::load_target_config(&ctx.store(), t.name())?.provider,
            crate::target::hetzner_token(ctx, t)?,
        ),
    };
    crate::provider::require_supported(&provider)?;
    let client = ctx.hetzner_client(&token);
    cancel.check()?;
    let mut regions: Vec<RegionView> = client
        .list_locations()
        .map_err(|e| provider_read_error(e, "GET /v1/locations"))?
        .locations
        .into_iter()
        .map(|l| RegionView {
            code: l.name,
            city: l.city,
            country: l.country,
            description: l.description,
        })
        .collect();
    regions.sort_by(|a, b| a.code.cmp(&b.code));
    let offers = cli_providers::machine::offers_from_server_types(&server_types(&client, cancel)?)
        .iter()
        .map(view)
        .collect();
    Ok(MachineCatalogue { regions, offers })
}

/// One probe per region, concurrently, in input order; `None` for a region that did not answer.
pub fn region_latencies_with(
    regions: &[String],
    probe: impl Fn(&str) -> Option<u32> + Sync,
) -> Vec<RegionLatency> {
    std::thread::scope(|s| {
        let handles: Vec<_> = regions.iter().map(|r| s.spawn(|| probe(r))).collect();
        regions
            .iter()
            .zip(handles)
            .map(|(r, h)| RegionLatency {
                region: r.clone(),
                latency_ms: h.join().ok().flatten(),
            })
            .collect()
    })
}

/// TCP connect to `<region>-speed.hetzner.com:443`, each bounded by [`LATENCY_PROBE_TIMEOUT`]
/// (DNS included, `net::tcp_probe`). A cancelled token answers `None` for every region.
pub fn region_latencies(
    _ctx: &Context,
    regions: &[String],
    cancel: &CancellationToken,
) -> Vec<RegionLatency> {
    region_latencies_with(regions, |r| {
        crate::net::tcp_probe(
            &format!("{r}-speed.hetzner.com"),
            443,
            LATENCY_PROBE_TIMEOUT,
            cancel,
        )
        .ok()
        .map(|d| d.as_millis().min(u32::MAX as u128) as u32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::UiError;
    use crate::target::testkit::*;

    fn token_source(token: &SecretString) -> CatalogueSource<'_> {
        CatalogueSource::Token {
            provider: "hetzner-cloud",
            token,
        }
    }

    #[test]
    fn the_catalogue_has_regions_and_offers_with_the_retired_flag() {
        let mut s = mockito::Server::new();
        let _l = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A).create();
        let _t = route(&mut s, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A).create();
        let ctx = Context::for_desktop("/unused".into(), s.url());
        let token = SecretString::new(TOKEN_A);
        let cat = catalogue(&ctx, token_source(&token), &CancellationToken::new()).unwrap();
        assert_eq!(
            cat.regions
                .iter()
                .map(|r| r.code.as_str())
                .collect::<Vec<_>>(),
            ["fsn1", "nbg1"]
        );
        assert_eq!(cat.regions[1].city, "Nuremberg");
        let cx11 = cat.offers.iter().find(|o| o.sku == "cx11").unwrap();
        assert!(cx11.retired && !cx11.available);
        let cx22 = cat.offers.iter().find(|o| o.sku == "cx22").unwrap();
        assert!(
            !cx22.retired
                && cx22.recommended
                && cx22.price_monthly_net.as_deref() == Some("3.7900")
        );
        assert_eq!(cx22.to_offer().sku, "cx22");
    }

    #[test]
    fn a_targets_catalogue_reads_with_the_targets_token() {
        let mut s = mockito::Server::new();
        let l = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A)
            .expect(1)
            .create();
        let _t = route(&mut s, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A).create();
        let (_d, ctx) = store_at(&["prod"], Some("prod"), &s.url());
        let prod = crate::TargetRef::named(&ctx, "prod").unwrap();
        let cat = catalogue(
            &ctx,
            CatalogueSource::Target(&prod),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(cat.regions.len(), 2);
        l.assert();
    }

    #[test]
    fn a_rejected_token_passes_through_with_its_status_and_a_dead_api_is_a_typed_request_failure() {
        let mut s = mockito::Server::new();
        let _l = route(
            &mut s,
            "/v1/locations",
            401,
            r#"{"error":{"code":"unauthorized","message":"no"}}"#,
            TOKEN_A,
        )
        .create();
        let ctx = Context::for_desktop("/unused".into(), s.url());
        let token = SecretString::new(TOKEN_A);
        let e = catalogue(&ctx, token_source(&token), &CancellationToken::new()).unwrap_err();
        assert!(
            matches!(
                e,
                CoreError::Cli(cli_core::CliError::Hetzner { status: 401, .. })
            ),
            "{e:?}"
        );
        let dead = Context::for_desktop("/unused".into(), "http://127.0.0.1:1");
        let e = catalogue(&dead, token_source(&token), &CancellationToken::new()).unwrap_err();
        assert_eq!(
            UiError::from(&e).code.as_deref(),
            Some("apprafter::provider::request_failed")
        );
    }

    #[test]
    fn a_cancelled_catalogue_sends_nothing() {
        let mut s = mockito::Server::new();
        let l = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A)
            .expect(0)
            .create();
        let ctx = Context::for_desktop("/unused".into(), s.url());
        let (token, cancel) = (SecretString::new(TOKEN_A), CancellationToken::new());
        cancel.cancel();
        assert!(matches!(
            catalogue(&ctx, token_source(&token), &cancel),
            Err(CoreError::Cancelled)
        ));
        l.assert();
    }

    #[test]
    fn an_unsupported_provider_is_refused_before_any_request() {
        let ctx = Context::for_desktop("/unused".into(), "http://127.0.0.1:1");
        let token = SecretString::new(TOKEN_A);
        assert!(matches!(
            catalogue(
                &ctx,
                CatalogueSource::Token {
                    provider: "aws",
                    token: &token
                },
                &CancellationToken::new()
            ),
            Err(CoreError::UnknownProvider { .. })
        ));
    }

    #[test]
    fn the_sku_check_reads_server_types_only() {
        let mut s = mockito::Server::new();
        let l = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A)
            .expect(0)
            .create();
        let _t = route(&mut s, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A).create();
        let ctx = Context::for_desktop("/unused".into(), s.url());
        let token = SecretString::new(TOKEN_A);
        let add = || cli_core::SkuCheckFor::TargetAdd { name: "p".into() };
        check_sku(
            &ctx,
            &token,
            "cx32",
            "fsn1",
            add(),
            &CancellationToken::new(),
        )
        .unwrap();
        let e = check_sku(
            &ctx,
            &token,
            "cx99",
            "nbg1",
            add(),
            &CancellationToken::new(),
        )
        .unwrap_err();
        let ui = UiError::from(&e);
        assert_eq!(
            ui.code.as_deref(),
            Some("apprafter::provider::server_type_unavailable")
        );
        assert_eq!(ui.fields["context"], serde_json::json!("target_add"));
        l.assert();
    }

    #[test]
    fn latencies_keep_the_input_order_and_run_concurrently() {
        let regions: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        let started = std::time::Instant::now();
        let got = region_latencies_with(&regions, |r| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            (r != "c").then_some(10)
        });
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1200),
            "probes ran one after another"
        );
        assert_eq!(
            got.iter()
                .map(|l| (l.region.as_str(), l.latency_ms))
                .collect::<Vec<_>>(),
            [
                ("a", Some(10)),
                ("b", Some(10)),
                ("c", None),
                ("d", Some(10)),
                ("e", Some(10))
            ]
        );
    }
}
