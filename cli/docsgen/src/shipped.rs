// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Which declared `needs` keys actually reach a running backend.
//!
//! `#Needs` in `schemas/v1alpha1/application.cue` is a *schema*
//! membership list — the admission webhook's `BUILTIN_TYPES` accepts
//! all seven of its keys on a `ServiceProvider`, and `cue vet` will
//! happily pass a manifest that asks for any of them. None of that
//! means a claim of that type is ever provisioned: the
//! `resourceclaim-provisioner`'s `Backend` enum
//! (`operator/operator-controllers/resourceclaim-provisioner/src/reconcile.rs`)
//! only has arms for `cloudnative-pg`, `dragonfly`, `disk` and
//! `shared-disk`, and `platform-stack/cue/service_providers.cue` only
//! seeds `ServiceProvider` CRs for `pg`, `redis` and `disk`. A guide
//! that shows a reader `needs: jetstream: {}` and calls it usable would
//! be schema-valid and product-false.
//!
//! This table is the identifier check's second gate, after schema
//! membership: a need must be both declared *and* shipped before a
//! doc page may present it as something a reader can use today.

use std::error::Error;
use std::fs;
use std::path::Path;

/// Whether a `needs` key is provisionable today, or only declared in
/// the schema pending a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// A `ServiceProvider` CR is seeded and a provisioner backend
    /// exists — a claim of this type reaches a running service.
    Shipped,
    /// `#Needs` declares the key and the webhook's `BUILTIN_TYPES`
    /// accepts it on a `ServiceProvider`, but no provisioner backend
    /// and no seeded `ServiceProvider` CR exist, so a claim of this
    /// type can never leave `AwaitingResourceClaim`.
    Declared,
}

/// Every key `#Needs` declares, classified by whether a provider
/// ships for it. Kept as a flat table rather than derived from the
/// provisioner's `Backend` enum because that enum lives in a sibling
/// workspace (`operator/`) this crate does not depend on — the
/// `every_declared_need_is_classified` test is what keeps this table
/// honest against the schema instead.
///
/// Each entry names the evidence: the `ServiceProvider` seed and
/// provisioner `Backend` arm for a shipped need, or the absence of
/// both (plus the stub `providers/*/README.md`, which documents
/// intent, not code) for a declared-only one.
pub const SHIPPED: &[(&str, Status)] = &[
    // Seeded as "pg-integrated" in platform-stack/cue/service_providers.cue
    // (backend: "cloudnative-pg"); Backend::Cloudnativepg in
    // operator/operator-controllers/resourceclaim-provisioner/src/reconcile.rs
    // provisions a database + role in the shared CNPG cluster.
    ("pg", Status::Shipped),
    // #Needs declares it and BUILTIN_TYPES (validator_serviceprovider.rs)
    // accepts it on a ServiceProvider, but service_providers.cue seeds no
    // "jetstream-*" entry and reconcile.rs's Backend enum has no Jetstream
    // arm — from_spec_backend() returns None for it, so the provisioner
    // requeues forever instead of provisioning. providers/jetstream-integrated/
    // is a README-only stub (no source).
    ("jetstream", Status::Declared),
    // Same shape as jetstream: declared + webhook-accepted, no
    // service_providers.cue seed, no Backend::Clickhouse arm.
    // providers/clickhouse-integrated/ is a README-only stub.
    ("clickhouse", Status::Declared),
    // Seeded as "redis-integrated" in service_providers.cue
    // (backend: "dragonfly"); Backend::Dragonfly in reconcile.rs
    // provisions a per-claim logical DB in the shared Dragonfly pool.
    ("redis", Status::Shipped),
    // Same shape as jetstream: declared + webhook-accepted, no
    // service_providers.cue seed, no Backend::S3 arm.
    // providers/s3-integrated/ is a README-only stub.
    ("s3", Status::Declared),
    // Same shape as jetstream: declared + webhook-accepted, no
    // service_providers.cue seed, no Backend::Notifications arm, and
    // unlike the other five, no `providers/notifications-*/` directory
    // exists at all — not even a README stub.
    ("notifications", Status::Declared),
    // Seeded as "disk-local" in service_providers.cue (backend: "disk");
    // Backend::Disk in reconcile.rs stamps a standalone RWO PVC per
    // claim (marked ready on PVC-exists, per the local-path
    // WaitForFirstConsumer note in that same seed).
    ("disk", Status::Shipped),
];

/// Look up a need's status. `None` if the table has no entry —
/// distinct from `Declared`, because an unclassified need is a gap in
/// this table, not a claim about the product.
pub fn status(need: &str) -> Option<Status> {
    SHIPPED
        .iter()
        .find(|(name, _)| *name == need)
        .map(|(_, status)| *status)
}

/// Read the field names declared directly under `#Needs` in
/// `schemas/v1alpha1/application.cue`, by text scan.
///
/// No `cue` dependency on purpose: this runs on every `docsgen check`,
/// which a pre-commit hook may invoke, and shelling out to evaluate
/// CUE per run would put a subprocess (and the "is `cue` on PATH"
/// question) in the hot path for no gain — the shape being read is a
/// flat `key?: <type>` struct, which a scan can read reliably without
/// evaluating the language.
///
/// Deliberately loud rather than quiet on drift: if the `#Needs:`
/// anchor cannot be found (renamed, moved to another file) or the
/// braces after it are unbalanced, this returns `Err` rather than
/// `Ok(vec![])`. An empty list would make
/// `every_declared_need_is_classified` pass vacuously — the exact
/// silent-gate failure this table exists to prevent elsewhere.
pub fn declared_needs(repo_root: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let path = repo_root.join("schemas/v1alpha1/application.cue");
    let src = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;

    let body = needs_body(&src).ok_or_else(|| {
        format!(
            "no `#Needs: {{ ... }}` definition found in {}",
            path.display()
        )
    })?;
    let names = top_level_field_names(body);
    if names.is_empty() {
        return Err(format!(
            "found `#Needs {{ }}` in {} but it declares no fields — the anchor \
             likely moved and this read the wrong block",
            path.display()
        )
        .into());
    }
    Ok(names)
}

/// Find the literal `#Needs:` definition (as opposed to a *reference*
/// to it, like `needs?: #Needs`, which is never followed by `{`) and
/// return the source between its opening and matching closing brace.
///
/// Scans past `//` line comments and `"..."` string literals so a
/// brace or colon inside either cannot desynchronise the count — CUE
/// has none of either inside `#Needs` today, but the point of scanning
/// rather than assuming is to survive the reformatting that eventually
/// happens to every hand-maintained schema file.
fn needs_body(src: &str) -> Option<&str> {
    let mut search_from = 0usize;
    loop {
        let rel = src[search_from..].find("#Needs:")?;
        let after_colon = search_from + rel + "#Needs:".len();
        let rest = &src[after_colon..];
        let trimmed = rest.trim_start();
        if trimmed.starts_with('{') {
            let body_start = after_colon + (rest.len() - trimmed.len()) + 1;
            return find_matching_brace(src, body_start).map(|end| &src[body_start..end]);
        }
        // Not a definition (no `{` follows) — keep looking, in case
        // `#Needs:` recurs, e.g. in a doc comment.
        search_from = after_colon;
    }
}

/// From just after an opening `{` (depth already 1), find the byte
/// offset of its matching `}`, skipping `//` comments and `"..."`
/// strings so their contents cannot be mistaken for structural braces.
fn find_matching_brace(src: &str, from: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth = 1i32;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    // A backslash escapes the next byte (including a
                    // quote), so it cannot end the string early.
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Extract `name` from every `name?:` or `name:` field at the top
/// level of a struct body (depth 0 relative to the body's own
/// braces) — skipping `//` comments, so a field mentioned only in
/// prose between two real fields is not mistaken for one, and
/// skipping nested struct bodies, so a value that is itself a struct
/// cannot contribute its own inner fields.
///
/// Order-preserving, first-seen order; callers that only need
/// membership do not care, but a stable order keeps failure messages
/// reproducible.
fn top_level_field_names(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut names = Vec::new();
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
            }
            c if depth == 0 && (c.is_ascii_alphabetic() || c == b'_') => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let ident = &body[start..i];
                let mut j = i;
                while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'?') {
                    j += 1;
                    while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                        j += 1;
                    }
                }
                if bytes.get(j) == Some(&b':') {
                    names.push(ident.to_string());
                }
            }
            _ => i += 1,
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_need_is_classified() {
        // Reading the schema rather than hardcoding the list means adding
        // a need to the CUE forces a decision here instead of silently
        // defaulting to "documented as usable".
        let declared =
            declared_needs(&crate::repo_root().unwrap()).expect("read #Needs from the schema");
        for need in &declared {
            assert!(
                SHIPPED.iter().any(|(n, _)| n == need),
                "need `{need}` is declared in the schema but not classified in shipped.rs"
            );
        }
    }

    #[test]
    fn the_three_with_providers_are_shipped() {
        for need in ["pg", "redis", "disk"] {
            assert_eq!(status(need), Some(Status::Shipped), "{need}");
        }
    }

    #[test]
    fn declared_without_a_provider_is_not_shipped() {
        for need in ["jetstream", "clickhouse"] {
            assert_eq!(status(need), Some(Status::Declared), "{need}");
        }
    }
}
