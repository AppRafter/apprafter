// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter db …` — manage `SharedDatabase` CRs (2.29 / ADR 0066).
//!
//! A SharedDatabase is a PostgreSQL database or a Redis keyspace that several
//! Applications bind, each through its own credential and at its own access
//! level. It is namespaced, and it OUTLIVES every application bound to it —
//! which is the whole reason it is a separate object rather than a field on
//! one of them.
//!
//! Four verbs, the same shape as `apprafter volume`:
//!
//! * `create` — apply a new SharedDatabase manifest.
//! * `list`   — tabular view with type, readiness, refCount and backing.
//! * `status` — single-resource detail, including WHO is bound.
//! * `rm`     — delete it and its data; refused while anything is bound.
//!
//! # Why `status` lists the binders and `volume status` does not
//!
//! `rm` is refused by a count, and a count tells an operator that they are
//! blocked without telling them by whom. For a volume the next step is
//! obvious enough — grep the manifests for the volume name. For a database
//! the binding is `needs.pg.ref`, one line inside a `needs` block, and the
//! applications that have it are exactly what the refusal is about. So the
//! names are read from the claims and printed.

use std::io::IsTerminal;
use std::path::Path;

use cli_core::{CliError, Result};
use serde_json::Value;
use tabled::{settings::Style, Table, Tabled};

use crate::cli::DbCommand;
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_apply_json, kubectl_delete, kubectl_get_json,
    kubectl_get_json_cluster_wide, namespace_confirmed_missing,
};

const RESOURCE: &str = "shareddatabase.apprafter.io";
const CLAIM_RESOURCE: &str = "resourceclaim.apprafter.io";

// ---------------------------------------------------------------------------
// Pure helpers (unit-testable without a cluster)
// ---------------------------------------------------------------------------

/// Build the SharedDatabase manifest JSON ready for `kubectl apply`.
///
/// Optional fields are OMITTED rather than defaulted. `persistent: false` on
/// a `pg` database is not a harmless no-op — the webhook refuses the field on
/// `pg` at all, because it is a redis concept and accepting it there would
/// mean answering a question the backend does not have.
pub fn shareddatabase_manifest(
    name: &str,
    type_: &str,
    size: Option<&str>,
    extensions: &[String],
    persistent: bool,
    namespace: &str,
) -> Value {
    let mut spec = serde_json::json!({ "type": type_ });
    if let Some(s) = size {
        spec["size"] = serde_json::json!(s);
    }
    if !extensions.is_empty() {
        spec["extensions"] = Value::Array(
            extensions
                .iter()
                .map(|e| serde_json::json!({ "name": e }))
                .collect(),
        );
    }
    // Only when true. An explicit `persistent: false` is the redis default
    // said out loud, and on `pg` it is a refusal — so the flag not being
    // passed must produce a manifest that does not mention it.
    if persistent {
        spec["persistent"] = serde_json::json!(true);
    }
    serde_json::json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "SharedDatabase",
        "metadata": { "name": name, "namespace": namespace },
        "spec": spec
    })
}

/// Whether a delete must be refused because applications are still bound.
pub fn delete_blocked(ref_count: i64) -> bool {
    ref_count > 0
}

/// The names of the claims in `claims` bound to `db_name`, sorted.
///
/// Mirrors the operator's own `binders_of`, including the rule that a claim
/// under deletion no longer counts: it is already releasing, and naming it
/// would send an operator to edit an application that is going away.
pub fn binders_of(db_name: &str, claims: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = claims
        .iter()
        .filter(|c| {
            c.pointer("/spec/sharedRef").and_then(Value::as_str) == Some(db_name)
                && c.pointer("/metadata/deletionTimestamp").is_none()
        })
        .filter_map(|c| {
            c.pointer("/metadata/name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    out.sort();
    out
}

/// One row in `apprafter db list`.
pub struct DbRow {
    pub name: String,
    pub type_: String,
    pub ready: bool,
    pub refs: i64,
    pub backing: String,
}

/// Parse one SharedDatabase JSON object into a [`DbRow`].
pub fn list_row(db: &Value) -> DbRow {
    let database = db.pointer("/status/database").and_then(Value::as_str);
    let instance = db.pointer("/status/instance").and_then(Value::as_str);
    let dbnum = db.pointer("/status/dbnum").and_then(Value::as_i64);
    DbRow {
        name: db
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        type_: db
            .pointer("/spec/type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        ready: db
            .pointer("/status/ready")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        refs: db
            .pointer("/status/refCount")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        backing: match (database, instance, dbnum) {
            (Some(d), _, _) => d.to_string(),
            // `$0` is allocatable, so the pin is matched on PRESENCE, not on
            // truthiness — a `(Some(i), Some(0))` that printed the bare
            // instance would hide which keyspace the database actually is.
            (None, Some(i), Some(n)) => format!("{i} ${n}"),
            _ => "\u{2014}".into(), // em-dash
        },
    }
}

/// The message of a `True` condition of `type_`, if the object carries one.
pub fn condition_message(db: &Value, type_: &str) -> Option<String> {
    let conds = db.pointer("/status/conditions")?.as_array()?;
    let c = conds
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some(type_))?;
    if c.get("status").and_then(Value::as_str) != Some("True") {
        return None;
    }
    c.get("message").and_then(Value::as_str).map(str::to_string)
}

// ---------------------------------------------------------------------------
// Table display
// ---------------------------------------------------------------------------

#[derive(Tabled)]
struct DbTableRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "TYPE")]
    type_: String,
    #[tabled(rename = "READY")]
    ready: String,
    #[tabled(rename = "BOUND")]
    refs: i64,
    #[tabled(rename = "BACKING")]
    backing: String,
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

pub fn run(action: DbCommand) -> Result<()> {
    match action {
        DbCommand::Create {
            name,
            r#type,
            size,
            extensions,
            persistent,
            namespace,
        } => create(
            &name,
            &r#type,
            size.as_deref(),
            &extensions,
            persistent,
            &namespace,
        ),
        DbCommand::List { namespace } => list(namespace.as_deref()),
        DbCommand::Status { name, namespace } => status(&name, &namespace),
        DbCommand::Rm {
            name,
            namespace,
            yes,
        } => rm(&name, &namespace, yes),
    }
}

// ---------------------------------------------------------------------------
// Subcommand implementations
// ---------------------------------------------------------------------------

fn create(
    name: &str,
    type_: &str,
    size: Option<&str>,
    extensions: &[String],
    persistent: bool,
    namespace: &str,
) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let manifest = shareddatabase_manifest(name, type_, size, extensions, persistent, namespace);
    kubectl_apply_json(&manifest, kc.path())?;
    println!("shareddatabase/{name} created in namespace {namespace}.");
    println!(
        "Bind an application to it with `needs: {type_}: ref: \"{name}\"` \
         (add `access: \"ro\"` for read-only)."
    );
    Ok(())
}

fn list(namespace: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    // A LIST does not validate its namespace: it returns the same empty set
    // for a namespace that is empty and for one that does not exist. Saying
    // "none found in shopp" answers the wrong question in the second case.
    if let Some(ns) = namespace {
        if namespace_confirmed_missing(ns, kc.path()) {
            return Err(CliError::Other(format!(
                "namespace '{ns}' does not exist — so this is not an empty list, \
                 it is the wrong address. Run `apprafter db list` with no `-n` to \
                 see every SharedDatabase and the namespace each lives in"
            )));
        }
    }
    let json = kubectl_get_json_cluster_wide(RESOURCE, namespace, kc.path())?;
    let items = json
        .as_ref()
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if items.is_empty() {
        let scope = namespace.unwrap_or("<all namespaces>");
        println!("No shared databases found in {scope}.");
        return Ok(());
    }

    let rows: Vec<DbTableRow> = items
        .iter()
        .map(|db| {
            let r = list_row(db);
            DbTableRow {
                name: r.name,
                type_: r.type_,
                ready: if r.ready { "true" } else { "false" }.into(),
                refs: r.refs,
                backing: r.backing,
            }
        })
        .collect();

    println!("{}", Table::new(&rows).with(Style::blank()));
    Ok(())
}

/// The error for a SharedDatabase a GET did not return — naming whichever of
/// the two things is actually missing.
///
/// The same repair `volume` needed: "'orders' not found in shopp" reads as a
/// missing database and sends the reader looking for one, when what is
/// missing is the namespace they named.
fn db_not_found(name: &str, namespace: &str, kubeconfig_path: &Path) -> CliError {
    if namespace_confirmed_missing(namespace, kubeconfig_path) {
        CliError::Other(format!(
            "namespace '{namespace}' does not exist, so '{name}' is not missing \
             from it — check the namespace. `apprafter db list` shows every \
             shared database and where each one lives"
        ))
    } else {
        CliError::Other(format!(
            "shared database '{name}' not found in namespace '{namespace}'"
        ))
    }
}

/// The claims in `namespace`, for the binder lookup. A failure to list is not
/// fatal to `status` — the rest of the detail is still worth printing, and an
/// empty binder list is visibly different from a refCount that says otherwise.
fn namespace_claims(namespace: &str, kubeconfig_path: &Path) -> Vec<Value> {
    kubectl_get_json_cluster_wide(CLAIM_RESOURCE, Some(namespace), kubeconfig_path)
        .ok()
        .flatten()
        .and_then(|v| v.get("items").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

fn status(name: &str, namespace: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let db = match kubectl_get_json(RESOURCE, Some(name), Some(namespace), kc.path())? {
        Some(db) => db,
        None => return Err(db_not_found(name, namespace, kc.path())),
    };

    let row = list_row(&db);
    let type_ = db
        .pointer("/spec/type")
        .and_then(Value::as_str)
        .unwrap_or("?");
    println!("SharedDatabase: {namespace}/{name}");
    println!("  Type:         {type_}");
    println!("  Ready:        {}", row.ready);
    println!("  Backing:      {}", row.backing);

    if let Some(exts) = db.pointer("/spec/extensions").and_then(Value::as_array) {
        let names: Vec<&str> = exts
            .iter()
            .filter_map(|e| e.get("name").and_then(Value::as_str))
            .collect();
        if !names.is_empty() {
            println!("  Extensions:   {}", names.join(", "));
        }
    }

    // The refCount comes from the operator and the names from the claims, so
    // they are two reads of the same fact a moment apart. Print both rather
    // than deriving one from the other: a disagreement is worth seeing, and
    // silently preferring one would hide the only symptom a stale count has.
    let binders = binders_of(name, &namespace_claims(namespace, kc.path()));
    println!("  Bound apps:   {}", row.refs);
    if binders.is_empty() {
        println!("                (none)");
    } else {
        for b in &binders {
            println!("                {b}");
        }
    }

    if let Some(msg) = condition_message(&db, "ExtensionUnavailable") {
        println!();
        println!("  ExtensionUnavailable: {msg}");
    }
    if !row.ready {
        if let Some(conds) = db.pointer("/status/conditions").and_then(Value::as_array) {
            if let Some(c) = conds
                .iter()
                .find(|c| c.get("type").and_then(Value::as_str) == Some("Ready"))
            {
                let reason = c.get("reason").and_then(Value::as_str).unwrap_or("?");
                let msg = c.get("message").and_then(Value::as_str).unwrap_or("");
                println!();
                println!("  Not ready ({reason}): {msg}");
            }
        }
    }
    Ok(())
}

fn rm(name: &str, namespace: &str, yes: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    let db = match kubectl_get_json(RESOURCE, Some(name), Some(namespace), kc.path())? {
        Some(db) => db,
        None => return Err(db_not_found(name, namespace, kc.path())),
    };

    let ref_count = db
        .pointer("/status/refCount")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    if delete_blocked(ref_count) {
        let binders = binders_of(name, &namespace_claims(namespace, kc.path()));
        let who = if binders.is_empty() {
            // refCount says bound and the claim list does not name anyone.
            // Say so rather than printing an empty list as if it settled the
            // matter — the delete is still refused, and the operator needs to
            // know the two sources disagree.
            "the operator reports bindings this list could not name".to_string()
        } else {
            binders.join(", ")
        };
        return Err(CliError::Other(format!(
            "shared database '{name}' still has {ref_count} bound application(s): {who}. \
             Remove `ref` from each application's `needs` block first — deleting the \
             database would take their data with it"
        )));
    }

    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!(
            "Delete shared database '{name}' in namespace '{namespace}' AND ITS DATA? \
             Nothing is retained."
        );
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
    println!("Shared database '{name}' deleted from namespace '{namespace}'.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claim(name: &str, shared_ref: Option<&str>) -> Value {
        let mut c = json!({ "metadata": { "name": name }, "spec": {} });
        if let Some(r) = shared_ref {
            c["spec"]["sharedRef"] = json!(r);
        }
        c
    }

    #[test]
    fn a_minimal_manifest_mentions_only_the_type() {
        let m = shareddatabase_manifest("orders", "pg", None, &[], false, "shop");
        assert_eq!(m["kind"], "SharedDatabase");
        assert_eq!(m["spec"]["type"], "pg");
        assert_eq!(m["metadata"]["namespace"], "shop");
        // Each absence is load-bearing: `persistent: false` on pg is REFUSED
        // by the webhook, and an empty extension list is the statement "this
        // database should have none", which is not what a create with no
        // --extension means.
        assert!(m["spec"].get("persistent").is_none());
        assert!(m["spec"].get("extensions").is_none());
        assert!(m["spec"].get("size").is_none());
    }

    #[test]
    fn extensions_become_named_entries() {
        let m = shareddatabase_manifest(
            "orders",
            "pg",
            Some("small"),
            &["vector".into(), "pg_trgm".into()],
            false,
            "shop",
        );
        assert_eq!(m["spec"]["size"], "small");
        assert_eq!(m["spec"]["extensions"][0]["name"], "vector");
        assert_eq!(m["spec"]["extensions"][1]["name"], "pg_trgm");
    }

    #[test]
    fn persistent_is_emitted_only_when_asked_for() {
        let m = shareddatabase_manifest("cache", "redis", None, &[], true, "shop");
        assert_eq!(m["spec"]["persistent"], json!(true));
    }

    #[test]
    fn delete_guard_blocks_while_bound() {
        assert!(delete_blocked(1));
        assert!(!delete_blocked(0));
    }

    #[test]
    fn binders_are_named_and_sorted() {
        let claims = vec![
            claim("web-pg", Some("orders")),
            claim("api-pg", Some("orders")),
            claim("other-pg", Some("reporting")),
            claim("own-pg", None),
        ];
        assert_eq!(binders_of("orders", &claims), vec!["api-pg", "web-pg"]);
    }

    #[test]
    fn a_claim_under_deletion_is_not_a_binder() {
        let mut dying = claim("web-pg", Some("orders"));
        dying["metadata"]["deletionTimestamp"] = json!("2026-09-15T10:00:00Z");
        assert!(binders_of("orders", &[dying]).is_empty());
    }

    #[test]
    fn a_pg_row_reports_the_database_as_its_backing() {
        let db = json!({
            "metadata": {"name": "orders"},
            "spec": {"type": "pg"},
            "status": {"ready": true, "refCount": 2, "database": "shd_shop_orders"}
        });
        let r = list_row(&db);
        assert_eq!(r.backing, "shd_shop_orders");
        assert_eq!(r.refs, 2);
        assert!(r.ready);
    }

    #[test]
    fn a_redis_row_reports_the_instance_and_its_pin() {
        let db = json!({
            "metadata": {"name": "cache"},
            "spec": {"type": "redis"},
            "status": {"ready": true, "refCount": 1,
                       "instance": "platform-redis-ephemeral-000", "dbnum": 4}
        });
        assert_eq!(list_row(&db).backing, "platform-redis-ephemeral-000 $4");
    }

    #[test]
    fn dbnum_zero_is_shown_rather_than_treated_as_absent() {
        // `$0` is allocatable and is what the FIRST shared cache on a cluster
        // gets, so a truthiness check here would blank the backing column for
        // exactly the most common case.
        let db = json!({
            "metadata": {"name": "cache"},
            "spec": {"type": "redis"},
            "status": {"instance": "platform-redis-ephemeral-000", "dbnum": 0}
        });
        assert_eq!(list_row(&db).backing, "platform-redis-ephemeral-000 $0");
    }

    #[test]
    fn an_unprovisioned_row_has_no_backing_to_report() {
        let db = json!({"metadata": {"name": "orders"}, "spec": {"type": "pg"}});
        assert_eq!(list_row(&db).backing, "\u{2014}");
        assert!(!list_row(&db).ready);
    }

    #[test]
    fn only_a_true_condition_carries_a_message() {
        let db = json!({"status": {"conditions": [
            {"type": "ExtensionUnavailable", "status": "False", "message": "cleared"}
        ]}});
        assert_eq!(condition_message(&db, "ExtensionUnavailable"), None);
        let db = json!({"status": {"conditions": [
            {"type": "ExtensionUnavailable", "status": "True", "message": "no vector"}
        ]}});
        assert_eq!(
            condition_message(&db, "ExtensionUnavailable").as_deref(),
            Some("no vector")
        );
    }
}
