// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure validator for the v1alpha1 SourceCredential object (1.79c /
//! ADR 0039).
//!
//! Enforces cross-field invariants the OpenAPI v3 CRD layer cannot
//! express:
//!
//!   - **At least one half.** `spec.git` and/or `spec.registry` —
//!     at least one must be present (a SourceCredential with neither
//!     would derive nothing).
//!   - **Exactly one backend kind per half.** Each present
//!     `backend` sets exactly one of `sealedSecretRef` / `openBaoPath`
//!     (the structural CRD schema carries both as optional because the
//!     apiserver rejects most `oneOf` shapes — the discriminator is
//!     enforced here, mirroring MigrationPlan's scope handling).
//!   - **Non-empty coverage.** `repoPrefixes` / `hosts` must each list
//!     at least one non-empty entry.
//!
//! Takes the full object (not just `spec`) so the destructive-change gate
//! added in a later slice can read `metadata` without changing this
//! signature.
//!
//! Typed against `operator_core::SourceCredentialSpec` (ADR 0047
//! Decision #4): the spec is deserialized into the operator-core struct
//! once and the half/backend discriminators read TYPED fields, so a
//! renamed field fails to compile instead of silently bypassing a rule.
//! Several PRESENCE / non-string diagnostics stay on the raw `Value`
//! because the typed struct cannot represent the input they reject:
//!   - `backend` presence — `SourceGit.backend` / `SourceRegistry.backend`
//!     are non-`Option`, so the struct cannot model an absent `backend`;
//!   - `repoPrefixes` / `hosts` presence and per-entry non-string — they
//!     are `Vec<String>`, so the struct cannot model a missing list nor a
//!     non-string entry (a non-string entry would fail deserialization).
//!
//! Those branches are unreachable in production — a validating webhook
//! runs after the apiserver's structural validation, which already
//! enforced `required: [backend, repoPrefixes]` / `[backend, hosts]`,
//! `items.type: string`, and the `minItems: 1` floor — and exist for the
//! unit tests / defence-in-depth. When the spec fails to deserialize
//! (test / misconfigured apiserver) the typed reads fall back to the raw
//! `Value`, matching the pre-refactor `as_object()` / `as_str()`
//! semantics exactly.

use operator_core::{SourceBackend, SourceCredentialSpec, SourceGit, SourceRegistry};
use serde_json::{Map, Value};

use crate::validator::ValidationError;

/// Validate a SourceCredential AdmissionReview object. Returns every
/// error found (no short-circuit past the top-level shape guard).
pub fn validate_sourcecredential(object: &Value) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    let Some(obj) = object.as_object() else {
        errors.push(ValidationError::new(
            "object",
            "SourceCredential object must be a JSON object",
        ));
        return errors;
    };

    let Some(spec) = obj.get("spec").and_then(Value::as_object) else {
        errors.push(ValidationError::new("spec", "spec is required"));
        return errors;
    };

    // Deserialize the spec into the typed operator-core struct. In production
    // this always succeeds — a validating webhook runs after the apiserver's
    // structural validation, which already enforced the CRD shape. When it
    // succeeds the half presence + the backend discriminator read TYPED fields,
    // so a renamed field fails to compile instead of silently bypassing a rule
    // (ADR 0047 #4). When it fails (test / misconfigured apiserver) the typed
    // reads fall back to the raw `Value`, matching the pre-refactor semantics.
    let typed = serde_json::from_value::<SourceCredentialSpec>(Value::Object(spec.clone())).ok();

    let git = spec.get("git").and_then(Value::as_object);
    let registry = spec.get("registry").and_then(Value::as_object);

    if git.is_none() && registry.is_none() {
        errors.push(ValidationError::new(
            "spec",
            "at least one of git or registry is required",
        ));
        return errors;
    }

    if let Some(git) = git {
        let typed_git = typed.as_ref().and_then(|t| t.git.as_ref());
        validate_half(
            git,
            typed_git.map(GitHalf),
            "spec.git",
            "repoPrefixes",
            &mut errors,
        );
    }
    if let Some(registry) = registry {
        let typed_registry = typed.as_ref().and_then(|t| t.registry.as_ref());
        validate_half(
            registry,
            typed_registry.map(RegistryHalf),
            "spec.registry",
            "hosts",
            &mut errors,
        );
    }

    errors
}

/// A typed view of one half that exposes its `backend` uniformly so
/// `validate_half` reads it the same way for git and registry. Carrying
/// the typed half (not just its backend) keeps the field reference
/// (`SourceGit::backend` / `SourceRegistry::backend`) compiler-gated.
enum TypedHalf<'a> {
    Git(&'a SourceGit),
    Registry(&'a SourceRegistry),
}
use TypedHalf::{Git as GitHalf, Registry as RegistryHalf};

impl<'a> TypedHalf<'a> {
    fn backend(&self) -> &'a SourceBackend {
        match self {
            TypedHalf::Git(g) => &g.backend,
            TypedHalf::Registry(r) => &r.backend,
        }
    }
}

/// Validate one half (`git` or `registry`): a valid `backend` plus a
/// non-empty coverage list (`repoPrefixes` or `hosts`).
///
/// `typed_half` is the deserialized half on the happy path (always, in
/// production); the backend discriminator reads its TYPED `Option` fields.
/// The `backend` / `list_field` PRESENCE diagnostics and the per-entry
/// non-string check stay on the raw `Map`: `SourceGit.backend` /
/// `SourceRegistry.backend` are non-`Option` and `repoPrefixes` / `hosts`
/// are `Vec<String>`, so the typed struct cannot model an absent backend, a
/// missing list, or a non-string entry (those are test-only branches).
fn validate_half(
    half: &Map<String, Value>,
    typed_half: Option<TypedHalf<'_>>,
    path: &str,
    list_field: &str,
    errors: &mut Vec<ValidationError>,
) {
    match half.get("backend").and_then(Value::as_object) {
        None => errors.push(ValidationError::new(
            format!("{path}.backend"),
            "backend is required",
        )),
        Some(backend) => validate_backend(
            backend,
            typed_half.as_ref().map(TypedHalf::backend),
            &format!("{path}.backend"),
            errors,
        ),
    }

    match half.get(list_field).and_then(Value::as_array) {
        None => errors.push(ValidationError::new(
            format!("{path}.{list_field}"),
            format!("{list_field} is required"),
        )),
        Some(arr) if arr.is_empty() => errors.push(ValidationError::new(
            format!("{path}.{list_field}"),
            format!("{list_field} must list at least one entry"),
        )),
        Some(arr) => {
            for (i, item) in arr.iter().enumerate() {
                if item.as_str().map(|s| s.trim().is_empty()).unwrap_or(true) {
                    errors.push(ValidationError::new(
                        format!("{path}.{list_field}[{i}]"),
                        "entry must be a non-empty string",
                    ));
                }
            }
        }
    }
}

/// Enforce exactly-one-of `sealedSecretRef` / `openBaoPath`, plus the
/// non-empty inner field of whichever is set.
///
/// `typed_backend` is the deserialized backend on the happy path (always,
/// in production): the `sealed_secret_ref` / `open_bao_path` presence
/// discriminator and the inner `name` / path reads come from the TYPED
/// `Option` fields, so a rename fails to compile. When the spec failed to
/// deserialize (test / misconfigured apiserver) the reads fall back to the
/// raw `Value` with the pre-refactor `is_null()` / `as_str()` semantics.
fn validate_backend(
    backend: &Map<String, Value>,
    typed_backend: Option<&SourceBackend>,
    path: &str,
    errors: &mut Vec<ValidationError>,
) {
    // `sealedSecretRef: null` / `openBaoPath: null` count as absent on both
    // paths: serde deserializes an explicit null to `None`, matching the raw
    // `is_some_and(|v| !v.is_null())` guard.
    let has_sealed = match typed_backend {
        Some(b) => b.sealed_secret_ref.is_some(),
        None => backend.get("sealedSecretRef").is_some_and(|v| !v.is_null()),
    };
    let has_openbao = match typed_backend {
        Some(b) => b.open_bao_path.is_some(),
        None => backend.get("openBaoPath").is_some_and(|v| !v.is_null()),
    };

    match (has_sealed, has_openbao) {
        (false, false) => errors.push(ValidationError::new(
            path,
            "backend must set exactly one of sealedSecretRef or openBaoPath",
        )),
        (true, true) => errors.push(ValidationError::new(
            path,
            "backend must set exactly one of sealedSecretRef or openBaoPath, not both",
        )),
        (true, false) => {
            let name = typed_backend
                .and_then(|b| b.sealed_secret_ref.as_ref())
                .map(|r| r.name.as_str())
                .or_else(|| {
                    backend
                        .get("sealedSecretRef")
                        .and_then(Value::as_object)
                        .and_then(|s| s.get("name"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("");
            if name.trim().is_empty() {
                errors.push(ValidationError::new(
                    format!("{path}.sealedSecretRef.name"),
                    "name is required",
                ));
            }
        }
        (false, true) => {
            let p = typed_backend
                .and_then(|b| b.open_bao_path.as_deref())
                .or_else(|| backend.get("openBaoPath").and_then(Value::as_str))
                .unwrap_or("");
            if p.trim().is_empty() {
                errors.push(ValidationError::new(
                    format!("{path}.openBaoPath"),
                    "openBaoPath must be non-empty",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields(errors: &[ValidationError]) -> Vec<&str> {
        errors.iter().map(|e| e.field.as_str()).collect()
    }

    #[test]
    fn accepts_single_pat_both_halves() {
        let obj = json!({
            "spec": {
                "git": {
                    "backend": {"sealedSecretRef": {"name": "srccred-acme-material"}},
                    "repoPrefixes": ["github.com/acme/"]
                },
                "registry": {
                    "backend": {"sealedSecretRef": {"name": "srccred-acme-material"}},
                    "hosts": ["ghcr.io/acme/"]
                }
            }
        });
        assert!(validate_sourcecredential(&obj).is_empty());
    }

    #[test]
    fn rejects_when_neither_git_nor_registry() {
        let obj = json!({"spec": {}});
        let errs = validate_sourcecredential(&obj);
        assert_eq!(fields(&errs), vec!["spec"]);
        assert!(errs[0].message.contains("at least one of git or registry"));
    }

    #[test]
    fn rejects_empty_repo_prefixes() {
        let obj = json!({
            "spec": {"git": {"backend": {"sealedSecretRef": {"name": "m"}}, "repoPrefixes": []}}
        });
        let errs = validate_sourcecredential(&obj);
        assert_eq!(fields(&errs), vec!["spec.git.repoPrefixes"]);
    }

    #[test]
    fn rejects_backend_with_both_refs() {
        let obj = json!({
            "spec": {"registry": {
                "backend": {"sealedSecretRef": {"name": "m"}, "openBaoPath": "secret/x"},
                "hosts": ["ghcr.io/acme/"]
            }}
        });
        let errs = validate_sourcecredential(&obj);
        assert_eq!(fields(&errs), vec!["spec.registry.backend"]);
        assert!(errs[0].message.contains("not both"));
    }

    #[test]
    fn rejects_backend_with_neither_ref() {
        let obj = json!({
            "spec": {"git": {"backend": {}, "repoPrefixes": ["github.com/acme/"]}}
        });
        let errs = validate_sourcecredential(&obj);
        assert_eq!(fields(&errs), vec!["spec.git.backend"]);
        assert!(errs[0].message.contains("exactly one"));
    }

    #[test]
    fn rejects_empty_sealed_secret_ref_name() {
        let obj = json!({
            "spec": {"git": {"backend": {"sealedSecretRef": {"name": ""}}, "repoPrefixes": ["github.com/acme/"]}}
        });
        let errs = validate_sourcecredential(&obj);
        assert_eq!(fields(&errs), vec!["spec.git.backend.sealedSecretRef.name"]);
    }

    #[test]
    fn collects_multiple_errors_without_short_circuit() {
        let obj = json!({
            "spec": {
                "git": {"backend": {}, "repoPrefixes": []},
                "registry": {"backend": {"openBaoPath": ""}, "hosts": ["ghcr.io/acme/"]}
            }
        });
        let errs = validate_sourcecredential(&obj);
        let f = fields(&errs);
        assert!(f.contains(&"spec.git.backend"));
        assert!(f.contains(&"spec.git.repoPrefixes"));
        assert!(f.contains(&"spec.registry.backend.openBaoPath"));
    }

    #[test]
    fn rejects_non_object() {
        let errs = validate_sourcecredential(&json!("nope"));
        assert_eq!(fields(&errs), vec!["object"]);
    }
}
