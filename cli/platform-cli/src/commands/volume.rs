// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter volume …` — manage `SharedVolume` CRs (2.6c).
//!
//! SharedVolumes are namespaced CRs (`sharedvolume.apprafter.io`)
//! that provision a PVC shared across multiple Applications.  The
//! CLI surfaces four verbs:
//!
//! * `create` — apply a new SharedVolume manifest to the cluster.
//! * `list`   — tabular view with refCount and used/free capacity.
//! * `status` — single-resource detail view.
//! * `rm`     — delete a SharedVolume; refused while still referenced.

use std::io::IsTerminal;

use cli_core::{CliError, Result};
use serde_json::Value;
use tabled::{settings::Style, Table, Tabled};

use crate::cli::VolumeCommand;
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_apply_json, kubectl_delete, kubectl_get_json,
    kubectl_get_json_cluster_wide,
};

const RESOURCE: &str = "sharedvolume.apprafter.io";

// ---------------------------------------------------------------------------
// Pure helpers (unit-testable without a cluster)
// ---------------------------------------------------------------------------

/// Build the SharedVolume manifest JSON ready for `kubectl apply`.
pub fn sharedvolume_manifest(name: &str, size: &str, namespace: &str) -> Value {
    serde_json::json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "SharedVolume",
        "metadata": { "name": name, "namespace": namespace },
        "spec": { "size": size }
    })
}

/// Return `true` when the volume is still referenced by one or more
/// Applications and must not be deleted.
pub fn delete_blocked(ref_count: i64) -> bool {
    ref_count > 0
}

/// One row in `apprafter volume list`.
pub struct VolumeRow {
    pub name: String,
    pub size: String,
    pub ready: bool,
    pub refs: i64,
    pub used_free: String,
}

/// Parse a single SharedVolume JSON object into a `VolumeRow`.
pub fn list_row(sv: &Value) -> VolumeRow {
    let used = sv
        .pointer("/status/capacity/usedBytes")
        .and_then(Value::as_i64);
    let cap = sv
        .pointer("/status/capacity/capacityBytes")
        .and_then(Value::as_i64);
    VolumeRow {
        name: sv
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        size: sv
            .pointer("/spec/size")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        ready: sv
            .pointer("/status/ready")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        refs: sv
            .pointer("/status/refCount")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        used_free: match (used, cap) {
            (Some(u), Some(c)) => format!("{u}/{}", c - u),
            _ => "\u{2014}".into(), // em-dash
        },
    }
}

// ---------------------------------------------------------------------------
// Table display
// ---------------------------------------------------------------------------

#[derive(Tabled)]
struct VolumeTableRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "SIZE")]
    size: String,
    #[tabled(rename = "READY")]
    ready: String,
    #[tabled(rename = "REFS")]
    refs: i64,
    #[tabled(rename = "USED/FREE")]
    used_free: String,
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

pub fn run(action: VolumeCommand) -> Result<()> {
    match action {
        VolumeCommand::Create {
            name,
            size,
            namespace,
        } => create(&name, &size, &namespace),
        VolumeCommand::List { namespace } => list(namespace.as_deref()),
        VolumeCommand::Status { name, namespace } => status(&name, &namespace),
        VolumeCommand::Rm {
            name,
            namespace,
            yes,
        } => rm(&name, &namespace, yes),
    }
}

// ---------------------------------------------------------------------------
// Subcommand implementations
// ---------------------------------------------------------------------------

fn create(name: &str, size: &str, namespace: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let manifest = sharedvolume_manifest(name, size, namespace);
    kubectl_apply_json(&manifest, kc.path())?;
    println!("sharedvolume/{name} created in namespace {namespace}.");
    Ok(())
}

fn list(namespace: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let json = kubectl_get_json_cluster_wide(RESOURCE, namespace, kc.path())?;
    let items = json
        .as_ref()
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if items.is_empty() {
        let scope = namespace.unwrap_or("<all namespaces>");
        println!("No SharedVolumes found in {scope}.");
        return Ok(());
    }

    let rows: Vec<VolumeTableRow> = items
        .iter()
        .map(|sv| {
            let r = list_row(sv);
            VolumeTableRow {
                name: r.name,
                size: r.size,
                ready: if r.ready { "true" } else { "false" }.into(),
                refs: r.refs,
                used_free: r.used_free,
            }
        })
        .collect();

    println!("{}", Table::new(&rows).with(Style::blank()));
    Ok(())
}

fn status(name: &str, namespace: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let sv =
        kubectl_get_json(RESOURCE, Some(name), Some(namespace), kc.path())?.ok_or_else(|| {
            CliError::Other(format!("SharedVolume '{name}' not found in {namespace}"))
        })?;

    let ready = sv
        .pointer("/status/ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ref_count = sv
        .pointer("/status/refCount")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let size = sv
        .pointer("/spec/size")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let pvc_ref = sv
        .pointer("/status/pvcRef")
        .and_then(Value::as_str)
        .unwrap_or("—");
    let used = sv
        .pointer("/status/capacity/usedBytes")
        .and_then(Value::as_i64);
    let cap = sv
        .pointer("/status/capacity/capacityBytes")
        .and_then(Value::as_i64);

    println!("SharedVolume:  {namespace}/{name}");
    println!("  Size:        {size}");
    println!("  Ready:       {ready}");
    println!("  PVC ref:     {pvc_ref}");
    println!("  Ref count:   {ref_count}");
    match (used, cap) {
        (Some(u), Some(c)) => println!("  Used/Free:   {u}/{} bytes", c - u),
        _ => println!("  Used/Free:   —"),
    }
    // 2.22d (D8): the operator has always sampled these numbers and, until
    // now, never judged them — `CapacityWarning` reported the NODE. It now
    // reports this volume, and this is where an operator reads it.
    if let Some(msg) = capacity_warning_message(&sv) {
        println!(
            "  {}",
            cli_core::style::warn(&format!("Capacity:    {msg}"))
        );
    }
    Ok(())
}

/// The `CapacityWarning` message when the condition is `True`.
///
/// Pure, so the shape is tested without a cluster. `None` for any other
/// status, including absent — a volume the sampler could not read is not a
/// volume with room, and printing a reassurance we did not measure is how a
/// capacity display stops being worth reading.
pub(crate) fn capacity_warning_message(sv: &Value) -> Option<String> {
    let conds = sv.pointer("/status/conditions")?.as_array()?;
    let c = conds
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some("CapacityWarning"))?;
    if c.get("status").and_then(Value::as_str) != Some("True") {
        return None;
    }
    c.get("message").and_then(Value::as_str).map(str::to_string)
}

fn rm(name: &str, namespace: &str, yes: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // Fetch the resource to check refCount before deleting.
    let sv =
        kubectl_get_json(RESOURCE, Some(name), Some(namespace), kc.path())?.ok_or_else(|| {
            CliError::Other(format!("SharedVolume '{name}' not found in {namespace}"))
        })?;

    let ref_count = sv
        .pointer("/status/refCount")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    if delete_blocked(ref_count) {
        return Err(CliError::Other(format!(
            "SharedVolume '{name}' is still referenced by {ref_count} app(s). \
             Remove all app references first."
        )));
    }

    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!("Delete SharedVolume '{name}' in namespace '{namespace}'?");
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    kubectl_delete(RESOURCE, name, namespace, kc.path())?;
    println!("SharedVolume '{name}' deleted from namespace '{namespace}'.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_manifest_has_size_and_namespace() {
        let m = sharedvolume_manifest("shared", "5Gi", "demo");
        assert_eq!(m["kind"], "SharedVolume");
        assert_eq!(m["spec"]["size"], "5Gi");
        assert_eq!(m["metadata"]["namespace"], "demo");
    }

    #[test]
    fn delete_guard_blocks_while_referenced() {
        assert!(delete_blocked(2));
        assert!(!delete_blocked(0));
    }

    #[test]
    fn list_row_reports_refcount_and_ready() {
        let sv = json!({
            "metadata": { "name": "shared" },
            "spec": { "size": "5Gi" },
            "status": {
                "ready": true,
                "refCount": 2,
                "capacity": { "usedBytes": 40, "capacityBytes": 50 }
            }
        });
        let row = list_row(&sv);
        assert_eq!(row.name, "shared");
        assert_eq!(row.refs, 2);
        assert!(row.ready);
        assert_eq!(row.used_free, "40/10");
    }

    #[test]
    fn list_row_missing_capacity_shows_dash() {
        let sv = json!({ "metadata": { "name": "vol" }, "spec": { "size": "1Gi" } });
        let row = list_row(&sv);
        assert_eq!(row.used_free, "\u{2014}");
    }

    #[test]
    fn sharedvolume_manifest_apiversion() {
        let m = sharedvolume_manifest("v", "10Gi", "ns");
        assert_eq!(m["apiVersion"], "apprafter.io/v1alpha1");
        assert_eq!(m["metadata"]["name"], "v");
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    use serde_json::json;

    fn sv_with(status: &str) -> Value {
        json!({ "status": { "conditions": [
            { "type": "Ready", "status": "True" },
            { "type": "CapacityWarning", "status": status, "message": "volume 91.2% full" }
        ]}})
    }

    #[test]
    fn a_full_volume_surfaces_its_message() {
        assert_eq!(
            capacity_warning_message(&sv_with("True")).as_deref(),
            Some("volume 91.2% full")
        );
    }

    #[test]
    fn a_healthy_volume_prints_nothing_extra() {
        assert!(capacity_warning_message(&sv_with("False")).is_none());
    }

    #[test]
    fn an_unsampled_volume_gets_no_reassurance() {
        // A volume the sampler could not read is not a volume with room.
        // Printing "plenty of space" from an absent condition is how a
        // capacity display stops being worth reading.
        assert!(capacity_warning_message(&json!({ "status": {} })).is_none());
        assert!(capacity_warning_message(&json!({})).is_none());
    }
}
