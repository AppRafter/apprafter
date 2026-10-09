// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The machine catalogue (D.3 overview §3.7.4): regions, machine offers and region latencies,
//! as the add wizard and the machine picker show them. `catalogue` and `region_latencies`
//! arrive in D.3b.

use serde::Serialize;

use crate::context::SecretString;
use crate::target_ref::TargetRef;

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
