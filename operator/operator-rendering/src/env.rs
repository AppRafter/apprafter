// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure env-value resolver for 2.12 (ADR 0046).
//!
//! Translates a `BTreeMap<String, EnvValue>` (the `spec.*.env` map after
//! `effective_spec` merges base + override) into a sorted `Vec<EnvVar>` for
//! the container spec.
//!
//! **Miss-handling policy**: both Claim and Secret lookups are best-effort —
//! if the connection-secret map has no entry for a given `(type, name)`, or
//! a Secret path lacks a `/`, the var is SKIPPED rather than causing a panic.
//! In normal operation this should never happen: the `COND_RESOURCE_CLAIM_PENDING`
//! gate ensures all claims are ready (with a resolved `connectionSecretRef`)
//! before the render runs, and the admission webhook rejects malformed Secret
//! paths. Skipping is the safe fallback so a transient race / defensive
//! path does not crash the operator — the requeue will re-render once the
//! claim is ready.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{EnvVar, EnvVarSource, SecretKeySelector};
use operator_core::{EnvRef, EnvValue};

/// Resolve an `env` map (post-`effective_spec` merge) into a deterministic,
/// sorted `Vec<EnvVar>` suitable for a container spec.
///
/// `needs_secrets` maps `(service_type, name_opt)` → connection-Secret name
/// (built from ready ResourceClaims by `resolve_needs_secrets` in the
/// Application controller). `name_opt == None` is the unnamed/default claim
/// of a type; `Some(name)` is a named array entry.
///
/// Iteration order: BTreeMap guarantees lexicographic key order, so the
/// returned `Vec` is deterministic across reconciles (SSA no-op on identical
/// input).
pub fn resolve_env(
    env: &BTreeMap<String, EnvValue>,
    needs_secrets: &BTreeMap<(String, Option<String>), String>,
) -> Vec<EnvVar> {
    env.iter()
        .filter_map(|(var_name, value)| match value {
            EnvValue::Literal(s) => Some(EnvVar {
                name: var_name.clone(),
                value: Some(s.clone()),
                value_from: None,
            }),

            EnvValue::Ref(EnvRef::Claim(path)) => {
                // Parse `"<type>.<field>"` or `"<type>.<name>.<field>"`.
                // Neither type/name/field contains a `.` by construction
                // (DNS-1123 labels + a fixed field vocab), so splitting on
                // `.` is safe.
                let parts: Vec<&str> = path.splitn(3, '.').collect();
                let (service_type, name_opt, field) = match parts.as_slice() {
                    [t, f] => (t.to_string(), None, f.to_string()),
                    [t, n, f] => (t.to_string(), Some(n.to_string()), f.to_string()),
                    _ => {
                        // Malformed path — skip. Webhook rejects these at
                        // admission time; this is a final defensive fallback.
                        return None;
                    }
                };
                let key = (service_type, name_opt);
                let Some(secret_name) = needs_secrets.get(&key) else {
                    // No resolved connection Secret for this claim identity.
                    // Skip the var — the readiness gate should have prevented
                    // this; re-rendering after the claim becomes ready will
                    // populate it.
                    return None;
                };
                Some(EnvVar {
                    name: var_name.clone(),
                    value: None,
                    value_from: Some(EnvVarSource {
                        secret_key_ref: Some(SecretKeySelector {
                            name: secret_name.clone(),
                            key: field,
                            optional: Some(false),
                        }),
                        ..Default::default()
                    }),
                })
            }

            EnvValue::Ref(EnvRef::Secret(path)) => {
                // Parse `"<name>/<key>"` — split on the FIRST `/` only.
                let Some(slash) = path.find('/') else {
                    // Malformed path (no `/`). Webhook rejects these; skip
                    // defensively.
                    return None;
                };
                let (secret_name, key) = path.split_at(slash);
                let key = &key[1..]; // skip the leading '/'
                Some(EnvVar {
                    name: var_name.clone(),
                    value: None,
                    value_from: Some(EnvVarSource {
                        secret_key_ref: Some(SecretKeySelector {
                            name: secret_name.to_string(),
                            key: key.to_string(),
                            optional: Some(false),
                        }),
                        ..Default::default()
                    }),
                })
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::EnvRef;

    /// Helper: look up an env var by name in the resolved vec.
    fn find<'a>(vars: &'a [EnvVar], name: &str) -> Option<&'a EnvVar> {
        vars.iter().find(|v| v.name == name)
    }

    // ---- 2.12c: basic resolve_env tests ----

    #[test]
    fn resolves_literal_claim_secret_to_envvars() {
        let mut secrets: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
        secrets.insert(("pg".into(), None), "web-pg-conn".into());
        let mut env: BTreeMap<String, EnvValue> = BTreeMap::new();
        env.insert("LOG".into(), EnvValue::Literal("info".into()));
        env.insert("DB".into(), EnvValue::Ref(EnvRef::Claim("pg.url".into())));
        env.insert("PW".into(), EnvValue::Ref(EnvRef::Claim("pg.pass".into())));
        env.insert(
            "K".into(),
            EnvValue::Ref(EnvRef::Secret("stripe/api-key".into())),
        );
        let vars = resolve_env(&env, &secrets);

        // BTreeMap order: DB, K, LOG, PW
        assert_eq!(vars.len(), 4);

        let lit = find(&vars, "LOG").expect("LOG present");
        assert_eq!(lit.value.as_deref(), Some("info"));
        assert!(lit.value_from.is_none());

        let db = find(&vars, "DB").expect("DB present");
        assert!(db.value.is_none());
        let kr = db
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(kr.name, "web-pg-conn");
        assert_eq!(kr.key, "url");
        assert_eq!(kr.optional, Some(false));

        let pw = find(&vars, "PW").expect("PW present");
        let pw_kr = pw
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(pw_kr.name, "web-pg-conn");
        assert_eq!(pw_kr.key, "pass");

        let k = find(&vars, "K").expect("K present");
        let k_kr = k
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(k_kr.name, "stripe");
        assert_eq!(k_kr.key, "api-key");
        assert_eq!(k_kr.optional, Some(false));
    }

    #[test]
    fn resolves_named_claim_ref() {
        // `claim.pg.main.url` with a named claim → looks up ("pg", Some("main")).
        let mut secrets: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
        secrets.insert(
            ("pg".into(), Some("main".into())),
            "web-pg-main-conn".into(),
        );
        let mut env: BTreeMap<String, EnvValue> = BTreeMap::new();
        env.insert(
            "DB_MAIN".into(),
            EnvValue::Ref(EnvRef::Claim("pg.main.url".into())),
        );
        let vars = resolve_env(&env, &secrets);
        assert_eq!(vars.len(), 1);
        let kr = vars[0]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(kr.name, "web-pg-main-conn");
        assert_eq!(kr.key, "url");
    }

    #[test]
    fn resolves_redis_claim_channel_prefix() {
        // redis need → channelPrefix field.
        let mut secrets: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
        secrets.insert(("redis".into(), None), "web-redis-conn".into());
        let mut env: BTreeMap<String, EnvValue> = BTreeMap::new();
        env.insert(
            "REDIS_PFX".into(),
            EnvValue::Ref(EnvRef::Claim("redis.channelPrefix".into())),
        );
        let vars = resolve_env(&env, &secrets);
        assert_eq!(vars.len(), 1);
        let kr = vars[0]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(kr.name, "web-redis-conn");
        assert_eq!(kr.key, "channelPrefix");
    }

    #[test]
    fn claim_ref_miss_skips_var() {
        // If the needs_secrets map has no entry for the requested claim
        // identity, the var is skipped (not panicked).
        let secrets: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
        let mut env: BTreeMap<String, EnvValue> = BTreeMap::new();
        env.insert("DB".into(), EnvValue::Ref(EnvRef::Claim("pg.url".into())));
        let vars = resolve_env(&env, &secrets);
        assert!(vars.is_empty(), "miss must skip the var");
    }

    #[test]
    fn secret_ref_splits_on_first_slash() {
        // `"ns/my-secret/with-slash/key"` — the FIRST `/` separates name
        // (`ns/my-secret/with-slash`) from key (`key`).
        // Wait — the grammar is `<name>/<key>`. The name is just the Secret
        // resource name, which is a DNS-1123 label (no `/`). So the first `/`
        // always marks the boundary. This test uses a name that does NOT
        // contain a slash (as the schema demands):
        let secrets: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
        let mut env: BTreeMap<String, EnvValue> = BTreeMap::new();
        env.insert(
            "TOKEN".into(),
            EnvValue::Ref(EnvRef::Secret("my-secret/token-value".into())),
        );
        let vars = resolve_env(&env, &secrets);
        assert_eq!(vars.len(), 1);
        let kr = vars[0]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(kr.name, "my-secret");
        assert_eq!(kr.key, "token-value");
    }

    #[test]
    fn output_is_sorted_by_key() {
        // BTreeMap is already sorted; the output should be in the same
        // lexicographic order.
        let secrets: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
        let mut env: BTreeMap<String, EnvValue> = BTreeMap::new();
        env.insert("Z_VAR".into(), EnvValue::Literal("z".into()));
        env.insert("A_VAR".into(), EnvValue::Literal("a".into()));
        env.insert("M_VAR".into(), EnvValue::Literal("m".into()));
        let vars = resolve_env(&env, &secrets);
        assert_eq!(vars.len(), 3);
        assert_eq!(vars[0].name, "A_VAR");
        assert_eq!(vars[1].name, "M_VAR");
        assert_eq!(vars[2].name, "Z_VAR");
    }
}
