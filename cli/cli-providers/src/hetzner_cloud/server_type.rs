// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure pre-flight validator for the Hetzner `server_type` field.
//!
//! Given the live API view of `server_types`, the requested name,
//! and the target region, returns `Ok(())` or a structured
//! `CliError::ServerTypeUnavailable` with context-appropriate
//! alternatives — letting `apprafter apply` fail fast before
//! provisioning SSH-key / network / firewall.

use chrono::{DateTime, Utc};
use cli_core::{CliError, Result, UnavailableKind};

use crate::machine::{offers_from_server_types, MachineOffer};

use super::types::ServerType;

/// Classify a (sku, region) pair against the flattened offer catalog.
///
/// Pure — the caller injects `now` so this function can be tested
/// with a fixed clock.
///
/// * `Unknown` — SKU not found in the catalog at all.
/// * `NotOfferedInRegion` — SKU exists globally but has no entry for `region`.
/// * `Retired` — `deprecation.unavailable_after` is present and ≤ `now`.
/// * `OutOfCapacity` — offer exists, is not retired, but `available == false`.
/// * `Ok(&MachineOffer)` — selectable; a *future* `unavailable_after` is NOT
///   an error (the picker may show a deprecation badge).
pub fn classify<'a>(
    catalog: &'a [MachineOffer],
    sku: &str,
    region: &str,
    now: DateTime<Utc>,
) -> std::result::Result<&'a MachineOffer, UnavailableKind> {
    let sku_exists = catalog.iter().any(|o| o.sku == sku);
    if !sku_exists {
        return Err(UnavailableKind::Unknown);
    }
    let offer = match catalog
        .iter()
        .find(|o| o.sku == sku && o.location == region)
    {
        Some(o) => o,
        None => return Err(UnavailableKind::NotOfferedInRegion),
    };
    if let Some(dep) = &offer.deprecation {
        if let Some(ua) = &dep.unavailable_after {
            if let Ok(ts) = DateTime::parse_from_rfc3339(ua) {
                if ts.with_timezone(&Utc) <= now {
                    return Err(UnavailableKind::Retired);
                }
            }
        }
    }
    if !offer.available {
        return Err(UnavailableKind::OutOfCapacity);
    }
    Ok(offer)
}

/// Validate that `requested` is a known, non-retired server type that
/// is currently orderable in `location`.
///
/// Signature is stable — `provider.rs` calls this directly.
pub fn validate_server_type(types: &[ServerType], requested: &str, location: &str) -> Result<()> {
    let offers = offers_from_server_types(types);
    match classify(&offers, requested, location, Utc::now()) {
        Ok(_) => Ok(()),
        Err(kind) => Err(CliError::ServerTypeUnavailable {
            requested: requested.into(),
            location: location.into(),
            kind,
            alternatives: alternatives_for(kind, &offers, requested, location),
        }),
    }
}

/// Build the human-readable alternatives string for each kind of failure.
///
/// * `Unknown` — up to 3 available SKUs in `region` (cores asc) as a
///   fallback selection.
/// * `NotOfferedInRegion` — list the regions where THIS sku IS offered.
/// * `OutOfCapacity` — up to 3 available alternatives in `region` +
///   regions where the same SKU does have capacity.
/// * `Retired` — up to 3 available SKUs in `region` nearest by cores.
fn alternatives_for(
    kind: UnavailableKind,
    offers: &[MachineOffer],
    sku: &str,
    region: &str,
) -> String {
    match kind {
        UnavailableKind::Unknown => {
            // We know nothing about target specs — list a few available
            // offers in the region sorted by cores asc.
            let mut candidates: Vec<&MachineOffer> = offers
                .iter()
                .filter(|o| o.location == region && o.available && o.deprecation.is_none())
                .collect();
            candidates.sort_by_key(|o| (o.cores, ((o.memory_gb * 1000.0) as i64), o.disk_gb));
            if candidates.is_empty() {
                return "(no live alternatives found in this region)".into();
            }
            let list = candidates
                .iter()
                .take(3)
                .map(format_offer)
                .collect::<Vec<_>>()
                .join(", ");
            format!("try one of: {list}")
        }

        UnavailableKind::NotOfferedInRegion => {
            // Show regions where the SKU IS offered (available=true).
            let regions: Vec<&str> = offers
                .iter()
                .filter(|o| o.sku == sku && o.available)
                .map(|o| o.location.as_str())
                .collect();
            if regions.is_empty() {
                format!("`{sku}` is not available in any region")
            } else {
                format!("`{sku}` is offered in: {}", regions.join(", "))
            }
        }

        UnavailableKind::OutOfCapacity => {
            // Axis 1: other available SKUs in `region`.
            let mut in_region: Vec<&MachineOffer> = offers
                .iter()
                .filter(|o| {
                    o.location == region && o.available && o.sku != sku && o.deprecation.is_none()
                })
                .collect();
            // Sort nearest by cores then memory.
            if let Some(ref_offer) = offers.iter().find(|o| o.sku == sku && o.location == region) {
                let (rc, rm) = (ref_offer.cores, ref_offer.memory_gb);
                in_region.sort_by_key(|o| {
                    (
                        (o.cores as i64 - rc as i64).abs(),
                        ((o.memory_gb - rm).abs() * 1000.0) as i64,
                    )
                });
            } else {
                in_region.sort_by_key(|o| (o.cores, ((o.memory_gb * 1000.0) as i64)));
            }

            // Axis 2: regions where the same SKU has capacity.
            let free_regions: Vec<&str> = offers
                .iter()
                .filter(|o| o.sku == sku && o.available && o.location != region)
                .map(|o| o.location.as_str())
                .collect();

            let mut parts: Vec<String> = Vec::new();
            if !in_region.is_empty() {
                let list = in_region
                    .iter()
                    .take(3)
                    .map(format_offer)
                    .collect::<Vec<_>>()
                    .join(", ");
                parts.push(format!("alternatives in {region}: {list}"));
            }
            if !free_regions.is_empty() {
                parts.push(format!(
                    "`{sku}` has capacity in: {}",
                    free_regions.join(", ")
                ));
            }
            if parts.is_empty() {
                "(no live alternatives found in this region)".into()
            } else {
                parts.join("\n  ")
            }
        }

        UnavailableKind::Retired => {
            // Nearest available SKUs to the retired one (by cores).
            let ref_cores = offers
                .iter()
                .find(|o| o.sku == sku)
                .map(|o| o.cores)
                .unwrap_or(2);
            let ref_mem = offers
                .iter()
                .find(|o| o.sku == sku)
                .map(|o| o.memory_gb)
                .unwrap_or(4.0);

            let mut candidates: Vec<&MachineOffer> = offers
                .iter()
                .filter(|o| {
                    o.location == region && o.available && o.sku != sku && o.deprecation.is_none()
                })
                .collect();
            candidates.sort_by_key(|o| {
                (
                    (o.cores as i64 - ref_cores as i64).abs(),
                    ((o.memory_gb - ref_mem).abs() * 1000.0) as i64,
                )
            });
            if candidates.is_empty() {
                return "(no live alternatives found in this region)".into();
            }
            let list = candidates
                .iter()
                .take(3)
                .map(format_offer)
                .collect::<Vec<_>>()
                .join(", ");
            format!("try one of: {list}")
        }
    }
}

/// Format a single offer for the error message.
fn format_offer(o: &&MachineOffer) -> String {
    format!(
        "{} ({}c/{}g/{}d, {})",
        o.sku, o.cores, o.memory_gb, o.disk_gb, o.arch
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hetzner_cloud::types::{Deprecation, ServerTypeLocation};
    use chrono::TimeZone;

    // ── ServerType fixture builders ───────────────────────────────────────────

    fn live(name: &str, cores: u32, memory: f64, disk: u32, region: &str) -> ServerType {
        ServerType {
            id: 0,
            name: name.into(),
            architecture: "x86".into(),
            cpu_type: "shared".into(),
            cores,
            memory,
            disk,
            deprecation: None,
            locations: vec![ServerTypeLocation {
                name: region.into(),
                available: true,
                recommended: false,
                deprecation: None,
            }],
            prices: vec![],
        }
    }

    fn dead(name: &str, region: &str) -> ServerType {
        let mut t = live(name, 2, 4.0, 40, region);
        t.deprecation = Some(Deprecation {
            announced: Some("2025-10-01T00:00:00Z".into()),
            unavailable_after: None,
        });
        t.locations[0].available = false;
        t
    }

    /// Build a ServerType with a location-level deprecation that has an
    /// explicit `unavailable_after` RFC3339 timestamp.
    fn retiring_at(
        name: &str,
        cores: u32,
        memory: f64,
        disk: u32,
        region: &str,
        ua: &str,
    ) -> ServerType {
        ServerType {
            id: 0,
            name: name.into(),
            architecture: "x86".into(),
            cpu_type: "shared".into(),
            cores,
            memory,
            disk,
            deprecation: None,
            locations: vec![ServerTypeLocation {
                name: region.into(),
                available: true,
                recommended: false,
                deprecation: Some(Deprecation {
                    announced: Some("2025-10-01T00:00:00Z".into()),
                    unavailable_after: Some(ua.into()),
                }),
            }],
            prices: vec![],
        }
    }

    // ── Fixed clock for classify tests ────────────────────────────────────────

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 12, 0, 0, 0).unwrap()
    }

    // ── validate_server_type tests (migrated) ─────────────────────────────────

    #[test]
    fn ok_when_type_is_live_and_available_in_region() {
        let types = vec![live("cpx22", 2, 4.0, 80, "nbg1")];
        assert!(validate_server_type(&types, "cpx22", "nbg1").is_ok());
    }

    #[test]
    fn errs_when_type_is_unknown() {
        let types = vec![live("cpx22", 2, 4.0, 80, "nbg1")];
        let err = validate_server_type(&types, "made-up", "nbg1").unwrap_err();
        match err {
            CliError::ServerTypeUnavailable {
                requested,
                location,
                kind,
                alternatives,
            } => {
                assert_eq!(requested, "made-up");
                assert_eq!(location, "nbg1");
                assert_eq!(kind, UnavailableKind::Unknown);
                assert!(
                    alternatives.contains("cpx22"),
                    "alternatives should mention cpx22: {alternatives}"
                );
            }
            other => panic!("expected ServerTypeUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn errs_when_type_is_deprecated_globally() {
        let types = vec![dead("cx22", "nbg1"), live("cpx22", 2, 4.0, 80, "nbg1")];
        let err = validate_server_type(&types, "cx22", "nbg1").unwrap_err();
        match err {
            CliError::ServerTypeUnavailable {
                kind, alternatives, ..
            } => {
                // The `dead` fixture marks available=false + global deprecation=Some
                // with no unavailable_after → classify sees available=false → OutOfCapacity
                // (global deprecation sets available=false at the location level).
                assert!(
                    kind == UnavailableKind::OutOfCapacity
                        || kind == UnavailableKind::Retired
                        || kind == UnavailableKind::NotOfferedInRegion,
                    "unexpected kind: {kind:?}"
                );
                assert!(
                    alternatives.contains("cpx22"),
                    "alternatives should mention cpx22: {alternatives}"
                );
            }
            other => panic!("expected ServerTypeUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn errs_when_type_is_alive_but_not_in_region() {
        let mut t = live("cpx22", 2, 4.0, 80, "fsn1");
        t.locations.push(ServerTypeLocation {
            name: "nbg1".into(),
            available: false,
            recommended: false,
            deprecation: None,
        });
        let types = vec![t, live("cax11", 2, 4.0, 40, "nbg1")];
        let err = validate_server_type(&types, "cpx22", "nbg1").unwrap_err();
        match err {
            CliError::ServerTypeUnavailable {
                kind, alternatives, ..
            } => {
                // cpx22 has a nbg1 entry with available=false → OutOfCapacity
                assert_eq!(kind, UnavailableKind::OutOfCapacity);
                assert!(
                    alternatives.contains("cax11"),
                    "alternatives should mention cax11: {alternatives}"
                );
            }
            other => panic!("expected ServerTypeUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn alternatives_prefer_closest_specs_first() {
        let types = vec![
            dead("cx22", "nbg1"),
            live("cpx62", 16, 32.0, 640, "nbg1"),
            live("cpx22", 2, 4.0, 80, "nbg1"),
            live("cpx32", 4, 8.0, 160, "nbg1"),
        ];
        let err = validate_server_type(&types, "cx22", "nbg1").unwrap_err();
        match err {
            CliError::ServerTypeUnavailable { alternatives, .. } => {
                let cpx22_idx = alternatives.find("cpx22").expect("cpx22 in alternatives");
                let cpx62_idx = alternatives.find("cpx62").expect("cpx62 in alternatives");
                assert!(
                    cpx22_idx < cpx62_idx,
                    "cpx22 (2c/4g) should come before cpx62 (16c/32g) when target is 2c/4g; got {alternatives}"
                );
            }
            other => panic!("expected ServerTypeUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn alternatives_skip_dead_and_other_region_entries() {
        let mut other_region = live("cpx32", 4, 8.0, 160, "fsn1");
        other_region.locations[0].available = true;
        let types = vec![
            dead("cx22", "nbg1"),
            dead("cx32", "nbg1"),
            other_region,
            live("cpx22", 2, 4.0, 80, "nbg1"),
        ];
        let err = validate_server_type(&types, "cx22", "nbg1").unwrap_err();
        match err {
            CliError::ServerTypeUnavailable { alternatives, .. } => {
                assert!(alternatives.contains("cpx22"));
                assert!(
                    !alternatives.contains("cx32"),
                    "deprecated cx32 leaked into alternatives: {alternatives}"
                );
                assert!(
                    !alternatives.contains("cpx32"),
                    "fsn1-only cpx32 leaked into nbg1 alternatives: {alternatives}"
                );
            }
            other => panic!("expected ServerTypeUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn alternatives_show_placeholder_when_no_live_options_in_region() {
        let types = vec![dead("cx22", "nbg1"), dead("cx32", "nbg1")];
        let err = validate_server_type(&types, "cx22", "nbg1").unwrap_err();
        match err {
            CliError::ServerTypeUnavailable { alternatives, .. } => {
                assert!(
                    alternatives.contains("no live alternatives"),
                    "expected placeholder, got: {alternatives}"
                );
            }
            other => panic!("expected ServerTypeUnavailable, got {other:?}"),
        }
    }

    // ── classify unit tests (pure, clock-injected) ────────────────────────────

    #[test]
    fn classify_unknown_sku_returns_unknown() {
        let types = vec![live("cpx22", 2, 4.0, 80, "nbg1")];
        let offers = offers_from_server_types(&types);
        let result = classify(&offers, "ghost", "nbg1", fixed_now());
        assert_eq!(result, Err(UnavailableKind::Unknown));
    }

    #[test]
    fn classify_sku_in_other_region_returns_not_offered_in_region() {
        // cpx22 is only in fsn1, not nbg1
        let types = vec![live("cpx22", 2, 4.0, 80, "fsn1")];
        let offers = offers_from_server_types(&types);
        let result = classify(&offers, "cpx22", "nbg1", fixed_now());
        assert_eq!(result, Err(UnavailableKind::NotOfferedInRegion));
    }

    #[test]
    fn classify_out_of_capacity_returns_out_of_capacity() {
        let mut t = live("cpx22", 2, 4.0, 80, "nbg1");
        t.locations[0].available = false;
        let types = vec![t];
        let offers = offers_from_server_types(&types);
        let result = classify(&offers, "cpx22", "nbg1", fixed_now());
        assert_eq!(result, Err(UnavailableKind::OutOfCapacity));
    }

    #[test]
    fn classify_past_unavailable_after_returns_retired() {
        // unavailable_after is 2026-01-01 — before fixed_now (2026-08-12)
        let types = vec![retiring_at(
            "cpx22",
            2,
            4.0,
            80,
            "nbg1",
            "2026-01-01T00:00:00Z",
        )];
        let offers = offers_from_server_types(&types);
        let result = classify(&offers, "cpx22", "nbg1", fixed_now());
        assert_eq!(result, Err(UnavailableKind::Retired));
    }

    #[test]
    fn classify_future_unavailable_after_returns_ok() {
        // unavailable_after is 2027-01-01 — after fixed_now (2026-08-12)
        let types = vec![retiring_at(
            "cpx22",
            2,
            4.0,
            80,
            "nbg1",
            "2027-01-01T00:00:00Z",
        )];
        let offers = offers_from_server_types(&types);
        let result = classify(&offers, "cpx22", "nbg1", fixed_now());
        assert!(
            result.is_ok(),
            "future deprecation should be Ok, got {result:?}"
        );
    }

    #[test]
    fn classify_live_offer_returns_ok_with_offer() {
        let types = vec![live("cpx22", 2, 4.0, 80, "nbg1")];
        let offers = offers_from_server_types(&types);
        let result = classify(&offers, "cpx22", "nbg1", fixed_now());
        assert!(result.is_ok());
        let offer = result.unwrap();
        assert_eq!(offer.sku, "cpx22");
        assert_eq!(offer.location, "nbg1");
    }

    #[test]
    fn classify_unknown_vs_not_in_region_are_distinguished() {
        // cpx22 in fsn1 only → "cpx22 nbg1" is NotOfferedInRegion, not Unknown
        // "ghost nbg1" is Unknown
        let types = vec![live("cpx22", 2, 4.0, 80, "fsn1")];
        let offers = offers_from_server_types(&types);

        assert_eq!(
            classify(&offers, "ghost", "nbg1", fixed_now()),
            Err(UnavailableKind::Unknown)
        );
        assert_eq!(
            classify(&offers, "cpx22", "nbg1", fixed_now()),
            Err(UnavailableKind::NotOfferedInRegion)
        );
    }

    #[test]
    fn not_offered_in_region_alternatives_list_available_regions() {
        // cpx22 is available in fsn1 + hel1, not nbg1
        let mut t_fsn = live("cpx22", 2, 4.0, 80, "fsn1");
        t_fsn.locations.push(ServerTypeLocation {
            name: "hel1".into(),
            available: true,
            recommended: false,
            deprecation: None,
        });
        let types = vec![t_fsn];
        let err = validate_server_type(&types, "cpx22", "nbg1").unwrap_err();
        match err {
            CliError::ServerTypeUnavailable {
                kind, alternatives, ..
            } => {
                assert_eq!(kind, UnavailableKind::NotOfferedInRegion);
                assert!(
                    alternatives.contains("fsn1"),
                    "should list fsn1: {alternatives}"
                );
                assert!(
                    alternatives.contains("hel1"),
                    "should list hel1: {alternatives}"
                );
            }
            other => panic!("expected ServerTypeUnavailable, got {other:?}"),
        }
    }
}
