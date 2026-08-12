// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure pre-flight validator for the Hetzner `server_type` field.
//!
//! Given the live API view of `server_types`, the requested name,
//! and the target region, returns `Ok(())` or a structured
//! `CliError::ServerTypeUnavailable` with the closest 3 live
//! alternatives — letting `apprafter apply` fail fast before
//! provisioning SSH-key / network / firewall.

use cli_core::{CliError, Result};

use super::types::ServerType;

/// Validate that `requested` is a known, non-deprecated server
/// type that is currently orderable in `location`.
pub fn validate_server_type(types: &[ServerType], requested: &str, location: &str) -> Result<()> {
    let found = types.iter().find(|t| t.name == requested);
    match found {
        None => Err(CliError::ServerTypeUnavailable {
            requested: requested.to_string(),
            location: location.to_string(),
            reason: format!(
                "unknown server type (Hetzner returned {} alive types)",
                types.len()
            ),
            alternatives: format_alternatives(types, location, requested),
        }),
        Some(t) if t.deprecation.is_some() => Err(CliError::ServerTypeUnavailable {
            requested: requested.to_string(),
            location: location.to_string(),
            reason: "type is deprecated globally".to_string(),
            alternatives: format_alternatives(types, location, requested),
        }),
        Some(t)
            if !t
                .locations
                .iter()
                .any(|l| l.name == location && l.available) =>
        {
            Err(CliError::ServerTypeUnavailable {
                requested: requested.to_string(),
                location: location.to_string(),
                reason: "type is not currently orderable in this region".to_string(),
                alternatives: format_alternatives(types, location, requested),
            })
        }
        Some(_) => Ok(()),
    }
}

/// Pick up to 3 live, region-available alternatives, prioritising
/// matches on cores then memory then disk. Used purely for the
/// human-facing error message.
fn format_alternatives(types: &[ServerType], location: &str, requested: &str) -> String {
    let target = types.iter().find(|t| t.name == requested);

    let mut candidates: Vec<&ServerType> = types
        .iter()
        .filter(|t| t.deprecation.is_none())
        .filter(|t| {
            t.locations
                .iter()
                .any(|l| l.name == location && l.available)
        })
        .filter(|t| t.name != requested)
        .collect();

    if let Some(target) = target {
        candidates.sort_by_key(|t| {
            (
                (t.cores as i64 - target.cores as i64).abs(),
                // scale memory (f64) to i64 with 0.001 GB granularity for sort-key inclusion
                ((t.memory - target.memory).abs() * 1000.0) as i64,
                (t.disk as i64 - target.disk as i64).abs(),
            )
        });
    } else {
        candidates.sort_by(|a, b| {
            a.cores.cmp(&b.cores).then_with(|| {
                a.memory
                    .partial_cmp(&b.memory)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
    }

    if candidates.is_empty() {
        return "(no live alternatives found in this region)".into();
    }
    candidates
        .iter()
        .take(3)
        .map(|t| {
            format!(
                "{} ({}c/{}g/{}d, {})",
                t.name, t.cores, t.memory, t.disk, t.architecture
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hetzner_cloud::types::{Deprecation, ServerTypeLocation};

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
                reason,
                alternatives,
            } => {
                assert_eq!(requested, "made-up");
                assert_eq!(location, "nbg1");
                assert!(reason.contains("unknown server type"));
                assert!(alternatives.contains("cpx22"));
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
                reason,
                alternatives,
                ..
            } => {
                assert!(reason.contains("deprecated"));
                assert!(alternatives.contains("cpx22"));
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
                reason,
                alternatives,
                ..
            } => {
                assert!(reason.contains("not currently orderable in this region"));
                assert!(alternatives.contains("cax11"));
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
                assert_eq!(alternatives, "(no live alternatives found in this region)");
            }
            other => panic!("expected ServerTypeUnavailable, got {other:?}"),
        }
    }
}
