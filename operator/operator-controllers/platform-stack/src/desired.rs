// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Compute the desired `Application.spec.source.{targetRevision,
//! helm.valuesObject}` from a `PlatformStack` spec and a
//! resolved chart version.
//!
//! `targetRevision` is the resolved version (pin or
//! channel-latest). `helm.valuesObject.overrides` mirrors
//! `PlatformStack.spec.overrides` — the umbrella chart's CUE
//! source consumes this block at render time (chart-side
//! contract added in Task 12).

use serde_json::{json, Map, Value};

use operator_core::{PlatformStackComponentOverride, PlatformStackSpec};

/// The two payload halves the reconciler SSA-patches onto the
/// parent `platform` Application. Kept separate for cleaner
/// patch construction in `reconcile`.
#[derive(Debug, Clone, PartialEq)]
pub struct DesiredSource {
    pub target_revision: String,
    pub helm_values: Value,
}

/// Build the desired payload. `resolved_version` is the version
/// the reconciler picked — either `stack.spec.pin` when set, or
/// the channel-latest from `oci::latest_in_channel`.
pub fn build(stack_spec: &PlatformStackSpec, resolved_version: &str) -> DesiredSource {
    let mut values = json!({
        "tier": stack_spec.values.tier,
    });
    if let Some(domain) = &stack_spec.values.domain {
        values["domain"] = json!(domain);
    }
    for (k, v) in &stack_spec.values.extras {
        values[k] = v.clone();
    }
    if let Some(overrides) = &stack_spec.overrides {
        let mut over = Map::new();
        for (name, payload) in overrides {
            over.insert(name.clone(), serialize_override(payload));
        }
        values["overrides"] = Value::Object(over);
    }
    if let Some(backup) = &stack_spec.backup {
        values["backup"] = serde_json::to_value(backup).unwrap_or(serde_json::Value::Null);
    }
    DesiredSource {
        target_revision: resolved_version.to_string(),
        helm_values: values,
    }
}

fn serialize_override(o: &PlatformStackComponentOverride) -> Value {
    let mut map = Map::new();
    if let Some(pin) = &o.pin {
        map.insert("pin".into(), Value::String(pin.clone()));
    }
    if let Some(vals) = &o.values {
        map.insert("values".into(), vals.clone());
    }
    if let Some(enabled) = o.enabled {
        map.insert("enabled".into(), Value::Bool(enabled));
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use operator_core::{PlatformStackSource, PlatformStackValues};

    fn base_spec() -> PlatformStackSpec {
        PlatformStackSpec {
            channel: "stable".into(),
            pin: None,
            auto_upgrade: false,
            default_environment: None,
            network: None,
            backup: None,
            resources: None,
            source: PlatformStackSource::default(),
            values: PlatformStackValues {
                tier: 1,
                domain: None,
                extras: BTreeMap::new(),
            },
            overrides: None,
        }
    }

    #[test]
    fn minimal_spec_produces_target_revision_and_tier_only() {
        let desired = build(&base_spec(), "0.1.15");
        assert_eq!(desired.target_revision, "0.1.15");
        assert_eq!(desired.helm_values["tier"], json!(1));
        assert!(desired.helm_values.get("domain").is_none());
        assert!(desired.helm_values.get("overrides").is_none());
    }

    #[test]
    fn domain_propagates_to_helm_values() {
        let mut spec = base_spec();
        spec.values.domain = Some("example.com".into());
        let desired = build(&spec, "0.1.15");
        assert_eq!(desired.helm_values["domain"], json!("example.com"));
    }

    #[test]
    fn extras_are_flattened_into_helm_values() {
        let mut spec = base_spec();
        spec.values
            .extras
            .insert("ingress_class".into(), json!("nginx"));
        let desired = build(&spec, "0.1.15");
        assert_eq!(desired.helm_values["ingress_class"], json!("nginx"));
    }

    #[test]
    fn backup_config_propagates_to_helm_values() {
        let mut spec = base_spec();
        spec.backup = Some(operator_core::platform_stack::BackupConfig {
            enabled: true,
            schedule: "@daily".into(),
            bucket: "s3:https://ep/b".into(),
            credential_ref: operator_core::platform_stack::CredentialRef {
                name: "bkcreds".into(),
            },
            staging_mode: "monolithic".into(),
            staging_size_limit: None,
            retention: None,
            check_schedule: "@weekly".into(),
            check_read_data: false,
            time_zone: Some("Europe/Berlin".into()),
            failure_webhook: None,
        });
        let desired = build(&spec, "0.2.31");
        assert_eq!(desired.helm_values["backup"]["enabled"], json!(true));
        // 2.22g: the zone must reach the chart, or the CronJob runs in the
        // kube-controller-manager's and nothing says which.
        assert_eq!(
            desired.helm_values["backup"]["timeZone"],
            json!("Europe/Berlin")
        );
        assert_eq!(
            desired.helm_values["backup"]["bucket"],
            json!("s3:https://ep/b")
        );
        assert_eq!(
            desired.helm_values["backup"]["credentialRef"]["name"],
            json!("bkcreds")
        );
    }

    #[test]
    fn no_backup_block_leaves_helm_values_without_backup_key() {
        let spec = base_spec();
        assert!(build(&spec, "0.2.31").helm_values.get("backup").is_none());
    }

    #[test]
    fn overrides_serialize_pin_values_enabled_independently() {
        let mut spec = base_spec();
        let mut over = BTreeMap::new();
        over.insert(
            "cilium".to_string(),
            PlatformStackComponentOverride {
                pin: Some("1.16.6".into()),
                values: None,
                enabled: None,
            },
        );
        over.insert(
            "backstage".to_string(),
            PlatformStackComponentOverride {
                pin: None,
                values: None,
                enabled: Some(false),
            },
        );
        spec.overrides = Some(over);
        let desired = build(&spec, "0.1.15");
        assert_eq!(
            desired.helm_values["overrides"]["cilium"]["pin"],
            json!("1.16.6")
        );
        assert_eq!(
            desired.helm_values["overrides"]["backstage"]["enabled"],
            json!(false)
        );
        // The cilium override must NOT have an enabled key (it
        // was None and skip_serializing_if'd out).
        assert!(desired.helm_values["overrides"]["cilium"]
            .get("enabled")
            .is_none());
    }
}
