// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! CNPG CR builders + claim-derivation pure helpers (Phase 2.4c).
//!
//! Every function here is pure (`-> serde_json::Value` / `String` /
//! `Vec`), so the whole module is unit-testable without a cluster. The
//! reconcile loop (`reconcile.rs`) wires these into SSA-applies +
//! read-modify-write of the shared CNPG `Cluster`.

use serde_json::{json, Value};

/// Max length of a Postgres identifier (`NAMEDATALEN - 1` = 63 bytes).
const PG_IDENT_MAX: usize = 63;

/// Derive a deterministic, valid Postgres identifier for a claim's role
/// and database from its `(namespace, name)`.
///
/// The result is `claim_<ns>_<name>` with every character that is not
/// lowercase-ASCII-alphanumeric folded to `_`, lowercased, and truncated
/// to 63 bytes. The `claim_` prefix guarantees a leading letter (Postgres
/// identifiers must start with a letter or underscore), so a claim whose
/// namespace/name begins with a digit is still valid.
///
/// The same claim always derives the same identifier — provisioning is
/// idempotent: a re-reconcile of an already-provisioned claim targets the
/// same role + database.
pub fn pg_identifier(namespace: &str, name: &str) -> String {
    let mut out = String::with_capacity(PG_IDENT_MAX);
    out.push_str("claim_");
    for ch in format!("{namespace}_{name}").chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '_'
        };
        out.push(mapped);
    }
    out.truncate(PG_IDENT_MAX);
    out
}

/// Build the application DSN for a provisioned role/database against the
/// CNPG cluster's read-write endpoint (`<cluster>-rw`).
pub fn dsn(role: &str, password: &str, db: &str, cluster: &str, ns: &str) -> String {
    format!("postgresql://{role}:{password}@{cluster}-rw.{ns}.svc:5432/{db}")
}

/// Build the CNPG `Cluster` SSA apply body. Sole-owned by the
/// provisioner's field manager — `spec.managed.roles` is appended to via
/// a read-modify-write loop, NOT through this body (the unkeyed list
/// would clobber co-owned entries under SSA).
pub fn cluster_object(name: &str, ns: &str, instances: i64, storage: &str) -> Value {
    json!({
        "apiVersion": "postgresql.cnpg.io/v1",
        "kind": "Cluster",
        "metadata": {
            "name": name,
            "namespace": ns,
        },
        "spec": {
            "instances": instances,
            "storage": {
                "size": storage,
            },
        },
    })
}

/// Build the CNPG `Database` SSA apply body. `owner` must reference an
/// existing managed role — create the role first; CNPG retries the
/// Database until the owner exists.
pub fn database_object(name: &str, ns: &str, cluster: &str, db: &str, owner: &str) -> Value {
    json!({
        "apiVersion": "postgresql.cnpg.io/v1",
        "kind": "Database",
        "metadata": {
            "name": name,
            "namespace": ns,
        },
        "spec": {
            "cluster": { "name": cluster },
            "name": db,
            "owner": owner,
            "ensure": "present",
        },
    })
}

/// Build the `kubernetes.io/basic-auth` password Secret CNPG reads for a
/// managed role. The `cnpg.io/reload: "true"` label makes CNPG pick up a
/// rotated password without an operator restart; the secret lives in the
/// CNPG cluster's namespace.
pub fn basic_auth_secret(name: &str, ns: &str, role: &str, password: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": {
                "cnpg.io/reload": "true",
                "apprafter.io/managed-by": "apprafter",
            },
        },
        "type": "kubernetes.io/basic-auth",
        "stringData": {
            "username": role,
            "password": password,
        },
    })
}

/// Build one `spec.managed.roles[]` entry: a login, non-superuser role
/// whose password lives in the named basic-auth Secret.
pub fn managed_role_entry(role: &str, secret_name: &str) -> Value {
    json!({
        "name": role,
        "ensure": "present",
        "login": true,
        "superuser": false,
        "passwordSecret": { "name": secret_name },
    })
}

/// Idempotent read-modify-write helper for the unkeyed
/// `spec.managed.roles` list: replace the entry whose `name` matches
/// `entry`'s, else append it. Foreign entries (other claims' roles, or
/// roles CNPG seeded) are preserved untouched.
pub fn merge_role(existing: Vec<Value>, entry: Value) -> Vec<Value> {
    let entry_name = entry.get("name").and_then(Value::as_str).map(str::to_owned);
    let mut out: Vec<Value> = existing
        .into_iter()
        .filter(|r| r.get("name").and_then(Value::as_str) != entry_name.as_deref())
        .collect();
    out.push(entry);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    // --- pg_identifier() ---

    #[test]
    fn pg_identifier_lowercases_and_replaces_non_conforming() {
        assert_eq!(pg_identifier("my-app", "web-db"), "claim_my_app_web_db");
    }

    #[test]
    fn pg_identifier_truncates_to_63_bytes() {
        let long_ns = "a".repeat(80);
        let id = pg_identifier(&long_ns, "x");
        assert!(id.len() <= 63, "len was {}", id.len());
        assert!(id.starts_with("claim_a"));
    }

    #[test]
    fn pg_identifier_starts_with_a_letter() {
        // The `claim_` prefix guarantees a leading letter even when the
        // namespace/name start with a digit.
        let id = pg_identifier("9ns", "0name");
        let first = id.chars().next().unwrap();
        assert!(first.is_ascii_alphabetic(), "id was {id}");
        assert_eq!(id, "claim_9ns_0name");
    }

    #[test]
    fn pg_identifier_uses_only_lowercase_alnum_and_underscore() {
        let id = pg_identifier("My.App", "Web/DB");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "id was {id}"
        );
        assert_eq!(id, "claim_my_app_web_db");
    }

    // --- dsn() ---

    #[test]
    fn dsn_renders_the_cnpg_rw_endpoint() {
        let got = dsn("role1", "secretpw", "db1", "platform-postgres", "cnpg-system");
        assert_eq!(
            got,
            "postgresql://role1:secretpw@platform-postgres-rw.cnpg-system.svc:5432/db1"
        );
    }

    // --- cluster_object() ---

    #[test]
    fn cluster_object_has_the_cnpg_apply_shape() {
        let c = cluster_object("platform-postgres", "cnpg-system", 1, "10Gi");
        assert_eq!(c["apiVersion"], "postgresql.cnpg.io/v1");
        assert_eq!(c["kind"], "Cluster");
        assert_eq!(c["metadata"]["name"], "platform-postgres");
        assert_eq!(c["metadata"]["namespace"], "cnpg-system");
        assert_eq!(c["spec"]["instances"], 1);
        assert_eq!(c["spec"]["storage"]["size"], "10Gi");
    }

    // --- database_object() ---

    #[test]
    fn database_object_has_the_cnpg_apply_shape() {
        let d = database_object("claim-db", "cnpg-system", "platform-postgres", "appdb", "approle");
        assert_eq!(d["apiVersion"], "postgresql.cnpg.io/v1");
        assert_eq!(d["kind"], "Database");
        assert_eq!(d["metadata"]["name"], "claim-db");
        assert_eq!(d["metadata"]["namespace"], "cnpg-system");
        assert_eq!(d["spec"]["cluster"]["name"], "platform-postgres");
        assert_eq!(d["spec"]["name"], "appdb");
        assert_eq!(d["spec"]["owner"], "approle");
        assert_eq!(d["spec"]["ensure"], "present");
    }

    // --- basic_auth_secret() ---

    #[test]
    fn basic_auth_secret_has_the_reload_label_and_credentials() {
        let s = basic_auth_secret("approle-pw", "cnpg-system", "approle", "secretpw");
        assert_eq!(s["apiVersion"], "v1");
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "approle-pw");
        assert_eq!(s["metadata"]["namespace"], "cnpg-system");
        assert_eq!(s["metadata"]["labels"]["cnpg.io/reload"], "true");
        assert_eq!(s["type"], "kubernetes.io/basic-auth");
        assert_eq!(s["stringData"]["username"], "approle");
        assert_eq!(s["stringData"]["password"], "secretpw");
    }

    // --- managed_role_entry() ---

    #[test]
    fn managed_role_entry_is_a_login_non_superuser_role() {
        let e = managed_role_entry("approle", "approle-pw");
        assert_eq!(e["name"], "approle");
        assert_eq!(e["ensure"], "present");
        assert_eq!(e["login"], true);
        assert_eq!(e["superuser"], false);
        assert_eq!(e["passwordSecret"]["name"], "approle-pw");
    }

    // --- merge_role() ---

    #[test]
    fn merge_role_appends_when_name_absent() {
        let out = merge_role(vec![], managed_role_entry("a", "a-pw"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["name"], "a");
    }

    #[test]
    fn merge_role_replaces_when_name_present() {
        let existing = vec![managed_role_entry("a", "old-pw")];
        let out = merge_role(existing, managed_role_entry("a", "new-pw"));
        assert_eq!(out.len(), 1, "same name must not duplicate");
        assert_eq!(out[0]["passwordSecret"]["name"], "new-pw");
    }

    #[test]
    fn merge_role_keeps_distinct_names() {
        let existing = vec![managed_role_entry("a", "a-pw")];
        let out = merge_role(existing, managed_role_entry("b", "b-pw"));
        assert_eq!(out.len(), 2);
        let names: Vec<&str> = out.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"a") && names.contains(&"b"));
    }

    #[test]
    fn merge_role_preserves_foreign_entries() {
        let foreign = json!({ "name": "keep-me", "login": false });
        let out = merge_role(vec![foreign], managed_role_entry("a", "a-pw"));
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|r: &Value| r["name"] == "keep-me"));
    }
}
