// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter platform …` thin wrappers. Track B.1.79.
//!
//! `status` reads `PlatformStack/default` from the cluster и
//! prints a human-readable summary (current/target/available
//! versions, conditions, recent history). `upgrade --to <v>`
//! patches `spec.pin`. Both shell out к `kubectl` rather than
//! pulling in kube-rs's Tokio runtime для the synchronous CLI
//! binary.

use cli_core::{CliError, Result};
use serde_json::Value;
use tabled::settings::{object::Rows, Modify, Width};
use tabled::{Table, Tabled};

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_merge_patch,
};

const PLATFORMSTACK_NAME: &str = "default";
const PLATFORMSTACK_NAMESPACE: &str = "apprafter-system";

#[derive(Tabled)]
struct ConditionRow {
    #[tabled(rename = "TYPE")]
    type_: String,
    #[tabled(rename = "STATUS")]
    status: String,
    #[tabled(rename = "REASON")]
    reason: String,
    #[tabled(rename = "MESSAGE")]
    message: String,
}

#[derive(Tabled)]
struct HistoryRow {
    #[tabled(rename = "APPLIED AT")]
    applied_at: String,
    #[tabled(rename = "VERSION")]
    version: String,
    #[tabled(rename = "OUTCOME")]
    outcome: String,
}

pub fn status() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let json = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} not found in cluster — \
             is `apprafter cluster-bootstrap` complete?"
        ))
    })?;

    print_status(&json);
    Ok(())
}

/// Pure formatter — pulled out so unit tests can drive с a
/// fixture JSON without a cluster.
fn print_status(json: &Value) {
    let spec = json.get("spec").cloned().unwrap_or(Value::Null);
    let status = json.get("status").cloned().unwrap_or(Value::Null);

    let channel = spec
        .pointer("/channel")
        .and_then(Value::as_str)
        .unwrap_or("stable");
    let pin = spec
        .pointer("/pin")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "(unpinned)".to_string());
    let auto_upgrade = spec
        .pointer("/autoUpgrade")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tier = spec
        .pointer("/values/tier")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let current = status
        .pointer("/currentVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let target = status
        .pointer("/targetVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let available = status
        .pointer("/availableVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let last_check = status
        .pointer("/lastUpstreamCheck")
        .and_then(Value::as_str)
        .unwrap_or("(never)");

    println!("PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} — tier {tier}");
    println!("  channel:     {channel}");
    println!("  pin:         {pin}");
    println!("  autoUpgrade: {auto_upgrade}");
    println!();
    println!("Versions:");
    println!("  current:   {current}");
    println!("  target:    {target}");
    println!("  available: {available}");
    println!("  lastCheck: {last_check}");
    println!();

    let conditions: Vec<ConditionRow> = status
        .pointer("/conditions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|c| ConditionRow {
                    type_: c
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    status: c
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    reason: c
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    message: c
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    if conditions.is_empty() {
        println!("Conditions: (none)");
    } else {
        println!("Conditions:");
        let mut t = Table::new(&conditions);
        // Wrap MESSAGE column so long content doesn't blow up
        // the terminal width. 60-char wrap matches what
        // `kubectl describe` does для condition messages.
        t.with(Modify::new(Rows::new(1..)).with(Width::wrap(60)));
        println!("{t}");
    }
    println!();

    let history: Vec<HistoryRow> = status
        .pointer("/versionHistory")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .rev() // newest first per UX expectation
                .take(5) // recent N
                .map(|e| HistoryRow {
                    applied_at: e
                        .get("appliedAt")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    version: e
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    outcome: e
                        .get("outcome")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    if history.is_empty() {
        println!("Recent history: (none)");
    } else {
        println!("Recent history (last {}):", history.len());
        println!("{}", Table::new(&history));
    }
}

/// `apprafter platform freeze <component> [--version <v>]`
/// patches PlatformStack.spec.overrides.<component>.pin. Без
/// `--version` reads the current effective component version
/// из `status.componentVersions.<component>` и locks that.
pub fn freeze(component: &str, version: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let json = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} not found"
        ))
    })?;

    let pin = match version {
        Some(v) => v.to_string(),
        None => json
            .pointer(&format!("/status/componentVersions/{component}"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                CliError::Other(format!(
                    "Не нашёл effective version для component '{component}' в \
                     status.componentVersions. Передай `--version <v>` явно либо \
                     запусти `apprafter platform status` чтобы посмотреть список \
                     известных components."
                ))
            })?,
    };

    let body = format!(r#"{{"spec":{{"overrides":{{"{component}":{{"pin":"{pin}"}}}}}}}}"#);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;

    println!("✓ Component '{component}' frozen at version '{pin}'.");
    println!(
        "PlatformController reconcile cycle применит override; umbrella chart's \
         curated pin для '{component}' игнорируется до тех пор пока override присутствует."
    );
    println!("Откатить: `apprafter platform unfreeze {component}`.");
    Ok(())
}

/// `apprafter platform unfreeze <component>` — RFC 7396
/// merge-patch с `null` value удаляет the override entry.
pub fn unfreeze(component: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // RFC 7396: null deletes the field. Patches the
    // `overrides.<component>` entry в whole — strips both `pin`
    // и `values` overrides. Если operator wants к keep
    // partial override (e.g. unfreeze pin but keep values
    // overrides), они должны patch вручную; `unfreeze` —
    // the "fully revert к chart's curated state" verb.
    let body = format!(r#"{{"spec":{{"overrides":{{"{component}":null}}}}}}"#);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    println!("✓ Component '{component}' unfrozen. Chart's curated pin restored.");
    Ok(())
}

/// `apprafter platform rescue` — emergency recovery wrapper
/// over `apprafter cluster-bootstrap`. Re-applies the
/// loader's Cilium + Argo CD + CRDs + operator chain against
/// the active target. Useful when Argo CD itself is unable к
/// self-adopt и а regular upgrade flow won't reach the right
/// reconcile state.
pub fn rescue(yes: bool) -> Result<()> {
    if !yes {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` чтобы пропустить confirmation prompt".into(),
            ));
        }
        println!(
            "Emergency rescue: re-run the loader's cluster-bootstrap path against the active \
             target. Это применит upstream Cilium / Argo CD / CRDs / operator manifests \
             как при initial bootstrap'е — все Apprafter-managed Applications потеряют \
             текущее состояние Sync/Healthy на несколько reconcile cycles."
        );
        let confirmed = inquire::Confirm::new("Подтвердить?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Отмена.");
            return Ok(());
        }
    }
    println!("Re-running cluster-bootstrap chain...");
    crate::commands::cluster_bootstrap::run()
}

pub fn upgrade(to: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let body = match to {
        Some(v) => format!(r#"{{"spec":{{"pin":"{v}"}}}}"#),
        None => r#"{"spec":{"pin":null,"autoUpgrade":true}}"#.to_string(),
    };
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    match to {
        Some(v) => println!("Pinned PlatformStack/{PLATFORMSTACK_NAME} к {v}"),
        None => println!(
            "Cleared pin; autoUpgrade=true. PlatformController will resolve к channel-latest."
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn print_status_handles_minimal_object() {
        // PlatformStack with only spec, no status (fresh CR).
        // Must not panic; должно gracefully print "(unset)" /
        // "(none)" placeholders.
        let obj = json!({
            "spec": { "channel": "stable", "values": { "tier": 1 } }
        });
        print_status(&obj);
    }

    #[test]
    fn print_status_renders_full_object() {
        // Smoke test for the happy path — все sections populated.
        let obj = json!({
            "spec": {
                "channel": "stable",
                "autoUpgrade": true,
                "pin": "0.1.35",
                "values": { "tier": 1 }
            },
            "status": {
                "currentVersion": "0.1.35",
                "targetVersion": "0.1.35",
                "availableVersion": "0.1.38",
                "lastUpstreamCheck": "2026-05-23T22:30:00Z",
                "conditions": [
                    { "type": "Ready", "status": "True", "reason": "Healthy", "message": "ok" }
                ],
                "versionHistory": [
                    { "appliedAt": "2026-05-23T21:00:00Z", "version": "0.1.34", "outcome": "succeeded" }
                ]
            }
        });
        print_status(&obj);
    }
}
