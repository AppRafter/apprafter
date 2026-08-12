// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Provider-agnostic machine catalog: one `MachineOffer` row per (SKU, location).

use crate::hetzner_cloud::types::{Deprecation, ServerType};

/// A provider-agnostic (region × SKU) catalog row. Pure data — the picker
/// wraps it with UI-only latency in platform-cli. Prices are Option because
/// `prices[]` and `locations[]` are separate arrays keyed by location string.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineOffer {
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
    pub deprecation: Option<Deprecation>,
}

/// Flatten the catalog: one row per (server_type, location). Price is looked
/// up by matching the location string in `prices[]`; a missing price → None.
pub fn offers_from_server_types(types: &[ServerType]) -> Vec<MachineOffer> {
    let mut out = Vec::new();
    for t in types {
        for loc in &t.locations {
            let price = t.prices.iter().find(|p| p.location == loc.name);
            out.push(MachineOffer {
                location: loc.name.clone(),
                sku: t.name.clone(),
                cores: t.cores,
                memory_gb: t.memory,
                disk_gb: t.disk,
                arch: t.architecture.clone(),
                cpu_type: t.cpu_type.clone(),
                price_monthly_net: price.map(|p| p.price_monthly.net.clone()),
                price_hourly_net: price.map(|p| p.price_hourly.net.clone()),
                available: loc.available,
                recommended: loc.recommended,
                deprecation: loc.deprecation.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hetzner_cloud::types::{
        HetznerPrice, ServerType, ServerTypeLocation, ServerTypePrice,
    };

    fn sample_types() -> Vec<ServerType> {
        vec![ServerType {
            id: 1,
            name: "cx22".into(),
            architecture: "x86".into(),
            cpu_type: "shared".into(),
            cores: 2,
            memory: 4.0,
            disk: 40,
            deprecation: None,
            locations: vec![
                ServerTypeLocation {
                    name: "nbg1".into(),
                    available: true,
                    recommended: true,
                    deprecation: None,
                },
                ServerTypeLocation {
                    name: "fsn1".into(),
                    available: false,
                    recommended: false,
                    deprecation: None,
                },
            ],
            prices: vec![ServerTypePrice {
                location: "nbg1".into(),
                price_monthly: HetznerPrice {
                    net: "3.9200".into(),
                    gross: "4.66".into(),
                },
                price_hourly: HetznerPrice {
                    net: "0.0066".into(),
                    gross: "0.0079".into(),
                },
            }],
        }]
    }

    #[test]
    fn offers_join_emits_row_per_location_with_optional_price() {
        let offers = offers_from_server_types(&sample_types());
        assert_eq!(offers.len(), 2);
        let nbg = offers
            .iter()
            .find(|o| o.location == "nbg1" && o.sku == "cx22")
            .unwrap();
        assert!(nbg.available && nbg.recommended);
        assert_eq!(nbg.cores, 2);
        assert_eq!(nbg.memory_gb, 4.0);
        assert_eq!(nbg.disk_gb, 40);
        assert_eq!(nbg.price_monthly_net.as_deref(), Some("3.9200"));
        let fsn = offers.iter().find(|o| o.location == "fsn1").unwrap();
        assert!(!fsn.available);
        assert!(fsn.price_monthly_net.is_none()); // no price entry for fsn1 → None, not a panic
    }
}
