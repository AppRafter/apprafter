// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure validator for the v1alpha1 Application spec.
//!
//! Enforces v1alpha1 invariants the OpenAPI v3 CRD layer can't
//! express: image must be reachable through `base.image` OR through
//! every `environments[*].image` (cross-field; CUE itself accepts
//! any string for `image`, so non-empty is enforced here plus by
//! the CRD's `pattern: "^.+$"`), environment names are DNS-1123
//! labels, env keys match `^[A-Z_][A-Z0-9_]*$`, and `needs` keys
//! are known platform-service types.
//!
//! The HTTP layer (`server.rs`) extracts the `request.object.spec`
//! value before passing it here.
//!
//! Typed against `operator_core::ApplicationSpec` (ADR 0047
//! Decision #4): the spec is deserialized into the operator-core
//! struct once and the happy-path reads go through the TYPED fields
//! (`base.image`, the `environments` keys, each scope's `env` /
//! `needs` / `replicas`, the `EnvValue` literal/claim/secret variants,
//! `DiskClaim` name/size/mountPath/class); `size` is now `Option<String>`
//! fails to compile instead of silently bypassing a rule.
//!
//! A handful of PRESENCE / not-a-string / unknown-KEY diagnostics
//! necessarily stay on the raw `Value` because the typed struct cannot
//! represent the input they reject:
//!   - **unknown `needs` key** (`mysql`, …): `operator_core::Needs` is a
//!     closed struct, so an unknown key cannot exist in the typed view —
//!     only the raw map can surface it;
//!   - **env KEY regex shape** (`^[A-Z_][A-Z0-9_]*$`): the keys of
//!     `Option<BTreeMap<String, EnvValue>>` are arbitrary `String`s, so
//!     the typed struct constrains the value, not the key's character set;
//!   - **disk `mountPath` / `size` presence and disk-key-presence for the
//!     inherit merge**: `DiskClaim.mount_path` is non-`Option` (a missing one
//!     fails the typed deserialize); `size` is now `Option<String>` (required
//!     on the owned shape; the raw fallback enforces presence for the webhook
//!     rule below) and the per-key needs merge pivots on whether a scope
//!     LITERALLY declares the `disk` key.
//!
//! Those branches are unreachable in production — a validating webhook
//! runs after the apiserver's structural validation, which already
//! enforced the CUE-generated Application CRD shape — and exist for the
//! unit tests / defence-in-depth. When the spec fails to deserialize
//! (test / misconfigured apiserver) every typed read falls back to the
//! raw `Value`, matching the pre-refactor `as_object()` / `as_str()`
//! semantics exactly.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use operator_core::{
    AppResources, ApplicationBaseSpec, ApplicationEnvOverride, ApplicationSpec, DiskClaim, EnvRef,
    EnvValue, JetStreamNeed, Needs, OneOrMany, Probe, Probes, ServiceNeed,
};
use serde_json::Value;

/// The fields SHARED by a scope's typed view — `spec.base`
/// ([`ApplicationBaseSpec`]) and each `spec.environments[*]`
/// ([`ApplicationEnvOverride`], the 2.16c all-optional override). The
/// per-scope validators (`validate_expose`, `validate_disk_claims`,
/// `validate_needs_names`, `validate_env_refs`, `validate_env_keys_scope`)
/// read a scope through this trait so ONE code path serves both concrete
/// types even where a single variable must hold either (e.g. the disk
/// per-key merge picks base OR the env override). The `expose` accessors
/// project the DIFFERING expose structs (`ApplicationExpose` with a
/// required `port` vs `ExposeOverride` with `port: Option<i32>`) onto their
/// common fields — the webhook never reads `port` on the typed path, so its
/// absence on the override is transparent here. Every accessor is a field
/// read, so a renamed operator-core field fails to compile (ADR 0047 #4).
trait ScopeView {
    fn replicas(&self) -> Option<i32>;
    fn env(&self) -> Option<&BTreeMap<String, EnvValue>>;
    fn needs(&self) -> Option<&Needs>;
    fn has_expose(&self) -> bool;
    fn expose_network(&self) -> Option<&str>;
    fn expose_hostname(&self) -> Option<&OneOrMany<String>>;
    fn expose_tls(&self) -> Option<bool>;
    fn resources(&self) -> Option<&AppResources>;
    /// 2.28: the scope's declared probes.
    fn probes(&self) -> Option<&Probes>;
    /// 2.28: the port this scope DECLARES, not the merged one. The base's
    /// `expose.port` is required by the CRD so it is an `i32` there; an env
    /// override's is optional (2.16c), and `validate_probes` is what combines
    /// them — a scope that omits it inherits the base's.
    fn expose_port(&self) -> Option<i32>;
}

impl ScopeView for ApplicationBaseSpec {
    fn replicas(&self) -> Option<i32> {
        self.replicas
    }
    fn env(&self) -> Option<&BTreeMap<String, EnvValue>> {
        self.env.as_ref()
    }
    fn needs(&self) -> Option<&Needs> {
        self.needs.as_ref()
    }
    fn has_expose(&self) -> bool {
        self.expose.is_some()
    }
    fn expose_network(&self) -> Option<&str> {
        self.expose.as_ref().and_then(|e| e.network.as_deref())
    }
    fn expose_hostname(&self) -> Option<&OneOrMany<String>> {
        self.expose.as_ref().and_then(|e| e.hostname.as_ref())
    }
    fn expose_tls(&self) -> Option<bool> {
        self.expose.as_ref().and_then(|e| e.tls)
    }
    fn resources(&self) -> Option<&AppResources> {
        self.resources.as_ref()
    }
    fn probes(&self) -> Option<&Probes> {
        self.probes.as_ref()
    }
    fn expose_port(&self) -> Option<i32> {
        self.expose.as_ref().map(|e| e.port)
    }
}

impl ScopeView for ApplicationEnvOverride {
    fn replicas(&self) -> Option<i32> {
        self.replicas
    }
    fn env(&self) -> Option<&BTreeMap<String, EnvValue>> {
        self.env.as_ref()
    }
    fn needs(&self) -> Option<&Needs> {
        self.needs.as_ref()
    }
    fn has_expose(&self) -> bool {
        self.expose.is_some()
    }
    fn expose_network(&self) -> Option<&str> {
        self.expose.as_ref().and_then(|e| e.network.as_deref())
    }
    fn expose_hostname(&self) -> Option<&OneOrMany<String>> {
        self.expose.as_ref().and_then(|e| e.hostname.as_ref())
    }
    fn expose_tls(&self) -> Option<bool> {
        self.expose.as_ref().and_then(|e| e.tls)
    }
    fn resources(&self) -> Option<&AppResources> {
        self.resources.as_ref()
    }
    fn probes(&self) -> Option<&Probes> {
        self.probes.as_ref()
    }
    fn expose_port(&self) -> Option<i32> {
        // An env override's `port` is itself optional (2.16c), so `None` here
        // means "inherit the base's", not "this scope has no port".
        self.expose.as_ref().and_then(|e| e.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// 2.16b-sec (F-1): the fallback operator ServiceAccount username when the
/// `OPERATOR_SERVICEACCOUNT` env var is unset. Matches the default Helm
/// release (`apprafter-operator` in namespace `apprafter-system`). The
/// deployment (`apprafter-admission-webhook/templates/deployment.yaml`)
/// injects `OPERATOR_SERVICEACCOUNT` templated on the actual release
/// namespace so a namespace change cannot silently open/close writes; this
/// constant only keeps unit tests + a mis-deploy safe. Mirrors the const in
/// `validator_resourceclaim.rs` / `validator_retainedclaim.rs` (now all read
/// through [`operator_service_account`] — one source of truth).
pub const DEFAULT_OPERATOR_SA: &str = "system:serviceaccount:apprafter-system:apprafter-operator";

/// 2.16b-sec (F-1): the resolved operator ServiceAccount username — read
/// ONCE from the `OPERATOR_SERVICEACCOUNT` env var at first call, falling back
/// to [`DEFAULT_OPERATOR_SA`] when unset/empty. This is the single source of
/// truth for "is this write from the operator's authenticated identity",
/// replacing the per-validator hardcoded `OPERATOR_SA` const and the
/// client-supplied `fieldManager` guard (which was trivially spoofable —
/// `--field-manager=apprafter-operator` is unauthenticated). Threaded into
/// the status guards + the ResourceClaim/RetainedClaim identity gates.
///
/// The env is deliberately captured once (`OnceLock`): the value cannot
/// change over a pod's lifetime, and reading it per-request would let a
/// late `set_var` race the guards. Tests that need a specific value pass it
/// explicitly to the pure helpers below (which take `expected_sa`), so this
/// resolver is only exercised through the default fallback in-process.
pub fn operator_service_account() -> &'static str {
    static RESOLVED: OnceLock<String> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            std::env::var("OPERATOR_SERVICEACCOUNT")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_OPERATOR_SA.to_string())
        })
        .as_str()
}

/// 2.16b-sec (F-1): whether `user_info.username` is the operator's
/// authenticated ServiceAccount (`expected_sa`). Unlike the old
/// `fieldManager` check this reads the AUTHENTICATED identity the apiserver
/// stamps into `request.userInfo` — a client cannot forge it. Pure +
/// unit-testable (the caller passes the resolved SA).
pub fn is_operator(user_info: &Value, expected_sa: &str) -> bool {
    user_info.get("username").and_then(Value::as_str) == Some(expected_sa)
}

/// 2.16b-sec (F-1): whether a request's authenticated identity is the operator
/// ServiceAccount OR a cluster-admin break-glass group (`system:masters`, or
/// `kubeadm:cluster-admins` on kubeadm >= 1.29 / k8s 1.35 / kind — either is
/// already omnipotent on the cluster). Shared by the Application.status /
/// MigrationPlan.status guards and the ResourceClaim/RetainedClaim identity
/// gates so there is ONE definition of "the operator or an admin".
pub fn is_operator_or_admin(user_info: &Value, expected_sa: &str) -> bool {
    if is_operator(user_info, expected_sa) {
        return true;
    }
    user_info
        .get("groups")
        .and_then(Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|g| {
                matches!(
                    g.as_str(),
                    Some("system:masters" | "kubeadm:cluster-admins")
                )
            })
        })
}

/// 2.16b-sec (F-1): whether a write to the `Application/status` subresource
/// is allowed, given the request's AUTHENTICATED `userInfo`. `Application.status`
/// is the root of trust for the app-migration gate — `status.lastAppliedSpec`
/// is the baseline every destructive-change detection diffs against. The ONLY
/// legitimate writer is the operator's ServiceAccount (`expected_sa`) or a
/// cluster-admin break-glass; any other subject is rejected so a direct
/// `patch applications/status` cannot zero the baseline and silently disarm
/// the gate.
///
/// F-1: this gates on `request.userInfo.username` (which the apiserver
/// authenticates), NOT on the `fieldManager` (a client-supplied
/// `--field-manager=...` string that anyone can set to `apprafter-operator`).
/// Unlike `MigrationPlan.status` (which legitimately accepts an external
/// `phase→approved` approval signal), `Application.status` has no external
/// writer.
pub fn application_status_write_allowed(user_info: &Value, expected_sa: &str) -> bool {
    is_operator_or_admin(user_info, expected_sa)
}

/// Validate the `spec` block of a v1alpha1 Application. Returns
/// every error found (the validator does not short-circuit). An
/// empty `Vec` means the manifest is valid.
pub fn validate_application_spec(spec: &Value) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    let Some(obj) = spec.as_object() else {
        errors.push(ValidationError::new("spec", "spec must be a JSON object"));
        return errors;
    };

    let base = obj.get("base").and_then(|v| v.as_object());
    let envs = obj.get("environments").and_then(|v| v.as_object());

    // Deserialize the whole spec into the typed operator-core struct. In
    // production this always succeeds — a validating webhook runs after the
    // apiserver's structural validation, which already enforced the
    // CUE-generated Application CRD. When it succeeds the happy-path reads
    // go through the TYPED fields, so a renamed field fails to compile
    // (ADR 0047 #4). When it fails (test / misconfigured apiserver) every
    // typed read below falls back to the raw `Value`, matching the
    // pre-refactor `as_object()` / `as_str()` semantics exactly.
    let typed = serde_json::from_value::<ApplicationSpec>(spec.clone()).ok();
    let typed_base = typed.as_ref().and_then(|s| s.base.as_ref());
    let typed_envs = typed.as_ref().and_then(|s| s.environments.as_ref());

    // `base.image` set <=> a non-empty string. `image: Option<String>`
    // models the absent case and the empty string is `Some("")`, so the
    // typed read is exact. Falls back to the raw map when deserialize fails.
    let base_image_set = match typed_base {
        Some(b) => b.image.as_deref().is_some_and(|s| !s.is_empty()),
        None => base
            .and_then(|b| b.get("image"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
    };

    if !base_image_set {
        match envs {
            None => errors.push(ValidationError::new(
                "spec.base.image",
                "spec.base.image is unset; either set it, or declare at least one entry under spec.environments with image set",
            )),
            Some(envs_obj) if envs_obj.is_empty() => errors.push(ValidationError::new(
                "spec.base.image",
                "spec.base.image is unset and spec.environments is empty; nothing to deploy",
            )),
            Some(envs_obj) => {
                for (name, val) in envs_obj {
                    // Typed env image when the scope decoded; else the raw read.
                    let env_image_set = match typed_envs.and_then(|m| m.get(name)) {
                        Some(env_spec) => env_spec.image.as_deref().is_some_and(|s| !s.is_empty()),
                        None => val
                            .as_object()
                            .and_then(|o| o.get("image"))
                            .and_then(|v| v.as_str())
                            .is_some_and(|s| !s.is_empty()),
                    };
                    if !env_image_set {
                        errors.push(ValidationError::new(
                            format!("spec.environments.{name}.image"),
                            "spec.base.image is unset, so every spec.environments[*].image must be set",
                        ));
                    }
                }
            }
        }
    }

    // Environment NAMES are the keys of `environments`. On the happy path
    // they are the keys of the typed `BTreeMap<String, ApplicationBaseSpec>`
    // (a renamed `environments` field fails to compile); the raw keys are
    // the fallback when deserialize fails.
    let env_names: Vec<&str> = match typed_envs {
        Some(m) => m.keys().map(String::as_str).collect(),
        None => envs
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default(),
    };
    for name in env_names {
        if !is_dns_1123_label(name) {
            errors.push(ValidationError::new(
                format!("spec.environments.{name}"),
                format!(
                    "environment name {name:?} must be a DNS-1123 label (lowercase alphanumeric + '-', 1..=63 chars, start and end alphanumeric)"
                ),
            ));
        }
    }

    // env KEY shape + unknown needs KEY: the env keys come from the typed
    // `env` map on the happy path (compiler-gated `env` field); the unknown
    // needs key can only be seen on the raw map (`Needs` is a closed struct),
    // so `validate_needs_keys` stays raw.
    validate_env_keys_scope("spec.base.env", typed_base, base, &mut errors);
    if let Some(base_obj) = base {
        if let Some(needs) = base_obj.get("needs").and_then(|v| v.as_object()) {
            validate_needs_keys("spec.base.needs", needs, &mut errors);
        }
    }
    if let Some(envs_obj) = envs {
        for (name, val) in envs_obj {
            validate_env_keys_scope(
                &format!("spec.environments.{name}.env"),
                typed_envs.and_then(|m| m.get(name)),
                val.as_object(),
                &mut errors,
            );
            if let Some(needs) = val
                .as_object()
                .and_then(|o| o.get("needs"))
                .and_then(|v| v.as_object())
            {
                validate_needs_keys(
                    &format!("spec.environments.{name}.needs"),
                    needs,
                    &mut errors,
                );
            }
        }
    }

    // 2.16c: the EFFECTIVE `expose` must carry a `port`. `base.expose.port`
    // is required (CRD), so a base-only or base-inheriting effective spec is
    // never portless; guard only an env override that SETS `expose` while
    // base has no `expose` (nothing supplies the port). `ExposeOverride.port`
    // is `Option<i32>` (all-optional per-env override), so a `port`-less env
    // expose under an absent base expose is the sole reject case here. Typed
    // on the happy path; the raw map is the deserialize-failure fallback.
    let base_has_expose = typed_base.map_or_else(
        || base.and_then(|b| b.get("expose")).is_some(),
        |b| b.expose.is_some(),
    );
    if !base_has_expose {
        if let Some(env_map) = typed_envs {
            for (name, env_spec) in env_map {
                if let Some(exp) = &env_spec.expose {
                    if exp.port.is_none() {
                        errors.push(ValidationError::new(
                            format!("spec.environments.{name}.expose.port"),
                            "base.expose is absent, so an env override that sets `expose` must include `port`",
                        ));
                    }
                }
            }
        } else if let Some(envs_obj) = envs {
            for (name, val) in envs_obj {
                // raw fallback: an env that sets `expose` (a JSON object) without a `port` key
                if let Some(exp) = val
                    .as_object()
                    .and_then(|o| o.get("expose"))
                    .and_then(Value::as_object)
                {
                    if !exp.contains_key("port") {
                        errors.push(ValidationError::new(
                            format!("spec.environments.{name}.expose.port"),
                            "base.expose is absent, so an env override that sets `expose` must include `port`",
                        ));
                    }
                }
            }
        }
    }

    validate_needs_names(typed_base, typed_envs, base, envs, &mut errors);
    validate_jetstream_scopes(typed_base, typed_envs, envs, &mut errors);
    validate_env_refs(typed_base, typed_envs, base, envs, &mut errors);
    validate_disk_claims(typed_base, typed_envs, base, envs, &mut errors);
    validate_expose(typed_base, typed_envs, base, envs, &mut errors);
    validate_resources(typed_base, typed_envs, base, envs, &mut errors);
    // 2.28: the five probe rules the structural schema cannot state.
    validate_probes(typed_base, typed_envs, &mut errors);

    errors
}

/// Validate env KEY shapes for one scope. On the happy path the keys come
/// from the TYPED `env: Option<BTreeMap<String, EnvValue>>` (so a renamed
/// `env` field fails to compile); when the scope did not decode it falls
/// back to the raw map. The key character-set rule itself is on the key
/// `String` either way — the typed struct constrains the VALUE, not the
/// key's `^[A-Z_][A-Z0-9_]*$` shape.
fn validate_env_keys_scope<S: ScopeView>(
    path: &str,
    typed_scope: Option<&S>,
    raw_scope: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    match typed_scope.and_then(|s| s.env()) {
        Some(env_map) => {
            for key in env_map.keys() {
                if !is_env_var_name(key) {
                    errors.push(ValidationError::new(
                        format!("{path}.{key}"),
                        format!("env key {key:?} must match ^[A-Z_][A-Z0-9_]*$"),
                    ));
                }
            }
        }
        None => {
            if let Some(env) = raw_scope
                .and_then(|o| o.get("env"))
                .and_then(|v| v.as_object())
            {
                validate_env_keys(path, env, errors);
            }
        }
    }
}

/// 2.6b (ADR 0043): a `needs.<type>` value is either a scalar entry
/// (object) or an array of entries. Return the optional `name` of each
/// entry, in declaration order. A scalar object yields exactly one
/// entry; an array yields one per element. A non-object/non-array value
/// yields no entries (its shape is rejected by the CRD layer). An entry
/// with no `name` (or an empty `name`) is the unnamed default.
fn needs_entry_names(value: &Value) -> Vec<Option<&str>> {
    fn entry_name(v: &Value) -> Option<&str> {
        v.as_object()
            .and_then(|o| o.get("name"))
            .and_then(|n| n.as_str())
            .filter(|n| !n.is_empty())
    }
    match value {
        Value::Array(items) => items.iter().map(entry_name).collect(),
        Value::Object(_) => vec![entry_name(value)],
        _ => Vec::new(),
    }
}

/// 2.6b (ADR 0043): validate the `(type, name)` identity rules within
/// each `needs.<type>` value, in BOTH base and every environment. Each
/// explicit `name` must be env-foldable (a DNS-1123 label, so the fold
/// `-` → `_` + uppercase yields a valid `[A-Z_][A-Z0-9_]*` env-var
/// suffix); names must be unique within a single (scope, type) value;
/// and at most one unnamed default is allowed per (scope, type) value.
/// Multi-error: one error per offending `needs.<type>` field, no
/// short-circuit (matching the validator contract).
fn validate_needs_names(
    typed_base: Option<&ApplicationBaseSpec>,
    typed_envs: Option<&BTreeMap<String, ApplicationEnvOverride>>,
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    // Check one (type, OneOrMany<ServiceNeed>) slot's entry names. The
    // `name` of each entry is read from the TYPED `ServiceNeed.name`
    // (compiler-gated). `disk` is NOT a service slot here — its identity
    // rules live in `validate_disk_claims`; `jetstream` isn't one either
    // (see `service_need_slots`'s doc) — so iterating the five service
    // slots already excludes both.
    fn check_typed_slot(
        path: &str,
        service_type: &str,
        slot: &OneOrMany<ServiceNeed>,
        errors: &mut Vec<ValidationError>,
    ) {
        let entries = slot.as_slice_vec();
        let mut seen: Vec<String> = Vec::new();
        let mut unnamed = 0usize;
        for need in &entries {
            // An empty explicit name folds to the unnamed default (matches
            // the raw `filter(|n| !n.is_empty())` in `needs_entry_names`).
            match need.name.as_deref().filter(|n| !n.is_empty()) {
                None => unnamed += 1,
                Some(n) => {
                    if !is_dns_1123_label(n) {
                        errors.push(ValidationError::new(
                            format!("{path}.{service_type}"),
                            format!(
                                "needs.{service_type} entry name {n:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric) so it folds to a valid [A-Z_][A-Z0-9_]* env-var suffix"
                            ),
                        ));
                    } else if seen.iter().any(|s| s == n) {
                        errors.push(ValidationError::new(
                            format!("{path}.{service_type}"),
                            format!(
                                "needs.{service_type} has a duplicate entry name {n:?}; names must be unique within a type"
                            ),
                        ));
                    } else {
                        seen.push(n.to_string());
                    }
                }
            }
        }
        if unnamed > 1 {
            errors.push(ValidationError::new(
                format!("{path}.{service_type}"),
                format!(
                    "needs.{service_type} declares {unnamed} unnamed entries; at most one unnamed default per type is allowed (give the others a name)"
                ),
            ));
        }
    }

    // Raw fallback for a scope that did not decode. NOT merely test /
    // misconfigured-apiserver — reachable in production: `expose.hostname`
    // is preserve-unknown in the CRD but typed in Rust, so an
    // apiserver-valid `expose: {port: 80, hostname: 5}` fails the typed
    // decode and drops the WHOLE scope (not just `expose`) to raw. Every
    // check below still runs the DNS-1123-label / uniqueness rules for
    // pg/clickhouse/redis/s3/notifications on that path — `disk` is
    // skipped (its identity rules live in `validate_disk_claims`, which
    // DOES run its full rule set on both the typed and raw paths — see
    // `scope_disk_entries`). `jetstream` gets a NARROWER skip: its
    // `(type, name)` identity check (this loop's actual subject) is
    // inapplicable — jetstream is scalar-only (ADR 0061 §6) and a
    // `needs.jetstream.name` here would otherwise get "must be a
    // DNS-1123 label … so it folds to a valid env-var suffix", which is
    // impossible advice for a field with no named-claim meaning at all.
    // But `persistent`/`name` themselves are still checked, right here,
    // for PRESENCE — they exist ONLY to be rejected (ADR 0061 §6), and
    // this raw path is the only place that can reject them when the
    // scope failed to decode; `validate_jetstream_need` is typed-only
    // and never runs here. Both messages are shared, word-for-word, with
    // the typed path via `jetstream_persistent_rejected`/
    // `jetstream_name_rejected` below. `subjects`/`maxBytes` stay
    // unchecked on this path — the CRD's `minItems`/`required` already
    // block them unconditionally, before any webhook runs, typed decode
    // or not.
    fn check_scope_raw(
        scope: &str,
        path: &str,
        obj: Option<&serde_json::Map<String, Value>>,
        errors: &mut Vec<ValidationError>,
    ) {
        let Some(needs) = obj.and_then(|o| o.get("needs")).and_then(|v| v.as_object()) else {
            return;
        };
        for (service_type, value) in needs {
            if service_type == "disk" {
                continue;
            }
            if service_type == "jetstream" {
                if let Some(o) = value.as_object() {
                    if o.contains_key("persistent") {
                        errors.push(ValidationError::new(
                            format!("{path}.jetstream"),
                            jetstream_persistent_rejected(scope),
                        ));
                    }
                    if o.contains_key("name") {
                        errors.push(ValidationError::new(
                            format!("{path}.jetstream"),
                            jetstream_name_rejected(scope),
                        ));
                    }
                }
                continue;
            }
            let entries = needs_entry_names(value);
            let mut seen: Vec<&str> = Vec::new();
            let mut unnamed = 0usize;
            for name in &entries {
                match name {
                    None => unnamed += 1,
                    Some(n) => {
                        if !is_dns_1123_label(n) {
                            errors.push(ValidationError::new(
                                format!("{path}.{service_type}"),
                                format!(
                                    "needs.{service_type} entry name {n:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric) so it folds to a valid [A-Z_][A-Z0-9_]* env-var suffix"
                                ),
                            ));
                        } else if seen.contains(n) {
                            errors.push(ValidationError::new(
                                format!("{path}.{service_type}"),
                                format!(
                                    "needs.{service_type} has a duplicate entry name {n:?}; names must be unique within a type"
                                ),
                            ));
                        } else {
                            seen.push(n);
                        }
                    }
                }
            }
            if unnamed > 1 {
                errors.push(ValidationError::new(
                    format!("{path}.{service_type}"),
                    format!(
                        "needs.{service_type} declares {unnamed} unnamed entries; at most one unnamed default per type is allowed (give the others a name)"
                    ),
                ));
            }
        }
    }

    // Dispatch a scope: typed slots on the happy path, raw map as fallback.
    // The scope's typed `needs` is projected via `ScopeView` before dispatch
    // so ONE closure serves both `ApplicationBaseSpec` and the env override.
    // `scope` ("base" or the environment name) is threaded through to
    // `check_scope_raw` purely so ITS jetstream messages can match
    // `validate_jetstream_need`'s verbatim — the typed path
    // (`check_typed_slot`) does not need it, it addresses everything by
    // `path` instead.
    let check_scope = |scope: &str,
                       path: &str,
                       typed_needs: Option<&Needs>,
                       raw_scope: Option<&serde_json::Map<String, Value>>,
                       errors: &mut Vec<ValidationError>| {
        match typed_needs {
            Some(needs) => {
                for (service_type, slot) in service_need_slots(needs) {
                    if let Some(slot) = slot {
                        check_typed_slot(path, service_type, slot, errors);
                    }
                }
            }
            None => check_scope_raw(scope, path, raw_scope, errors),
        }
    };

    check_scope(
        "base",
        "spec.base.needs",
        typed_base.and_then(ScopeView::needs),
        base,
        errors,
    );
    if let Some(envs_obj) = envs {
        for (env_name, val) in envs_obj {
            let path = format!("spec.environments.{env_name}.needs");
            check_scope(
                env_name,
                &path,
                typed_envs
                    .and_then(|m| m.get(env_name))
                    .and_then(ScopeView::needs),
                val.as_object(),
                errors,
            );
        }
    }
}

/// The five `OneOrMany<ServiceNeed>`-shaped service slots of a typed
/// `Needs`, in the fixed declaration order (`disk` is intentionally
/// excluded — its identity rules live in `validate_disk_claims`;
/// `jetstream` is intentionally excluded too — it carries its own type
/// (`JetStreamNeed`, ADR 0061 §6) and is scalar-only, so the array-name
/// uniqueness this function checks cannot apply to it). A renamed slot
/// field on `Needs` fails to compile here.
fn service_need_slots(needs: &Needs) -> [(&'static str, &Option<OneOrMany<ServiceNeed>>); 5] {
    [
        ("pg", &needs.pg),
        ("clickhouse", &needs.clickhouse),
        ("redis", &needs.redis),
        ("s3", &needs.s3),
        ("notifications", &needs.notifications),
    ]
}

/// 2.6b (ADR 0043): the `needs.disk` value, scalar or array, as a list
/// of disk-entry objects in declaration order. Non-object array elements
/// and a non-object/non-array value yield no entries (their shape is
/// rejected by the CRD layer / the closed `#DiskClaim` schema).
fn disk_entries(value: &Value) -> Vec<&serde_json::Map<String, Value>> {
    match value {
        Value::Array(items) => items.iter().filter_map(|v| v.as_object()).collect(),
        Value::Object(o) => vec![o],
        _ => Vec::new(),
    }
}

/// 2.6b (ADR 0043): derive a disk claim's name — the explicit `name`,
/// else the last path segment of `mountPath` (`/var/lib/uploads` →
/// `uploads`, `/data` → `data`). Returns `None` when neither yields a
/// non-empty segment (a malformed/relative mountPath; the absolute-path
/// guard reports that separately).
fn disk_name(entry: &serde_json::Map<String, Value>) -> Option<String> {
    if let Some(n) = entry
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|n| !n.is_empty())
    {
        return Some(n.to_string());
    }
    entry
        .get("mountPath")
        .and_then(|v| v.as_str())
        .and_then(|p| p.rsplit('/').find(|seg| !seg.is_empty()))
        .map(|seg| seg.to_string())
}

/// 2.6b (ADR 0043): a Kubernetes resource quantity for the disk `size`.
/// A decimal magnitude with an optional binary (Ei…Ki) or decimal
/// (E…k, with lower-case `k`) SI suffix — sufficient for the disk size
/// surface (no exponent / signed forms, which `#DiskClaim.size` never
/// needs). KEEP IN SYNC with the design's quantity grammar.
fn is_k8s_quantity(s: &str) -> bool {
    let suffix_ok = |suffix: &str| -> bool {
        matches!(
            suffix,
            "" | "Ei" | "Pi" | "Ti" | "Gi" | "Mi" | "Ki" | "E" | "P" | "T" | "G" | "M" | "k"
        )
    };
    // Split the leading numeric magnitude (digits, optionally one '.'
    // followed by digits) from the trailing suffix.
    let mut chars = s.char_indices().peekable();
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut split = s.len();
    while let Some(&(i, ch)) = chars.peek() {
        match ch {
            '0'..='9' => {
                seen_digit = true;
                chars.next();
            }
            '.' if !seen_dot => {
                seen_dot = true;
                chars.next();
            }
            _ => {
                split = i;
                break;
            }
        }
    }
    if !seen_digit {
        return false;
    }
    let (magnitude, suffix) = s.split_at(split);
    // A trailing '.' with no fractional digits is malformed.
    if magnitude.ends_with('.') {
        return false;
    }
    suffix_ok(suffix)
}

/// 2.16d: parse a Kubernetes quantity string to a normalized f64 of base units
/// (bytes for memory-style / cores for cpu-style). Handles decimal SI
/// (k,M,G,T,P), binary SI (Ki,Mi,Gi,Ti,Pi), milli (m), and bare numbers.
/// Returns None on malformed input. Used only for the `request <= limit`
/// magnitude comparison; the FORMAT of each quantity is validated separately
/// by [`is_k8s_quantity`].
fn quantity_to_f64(q: &str) -> Option<f64> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    // split trailing unit (letters + 'i')
    let idx = q.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(q.len());
    let (num, unit) = q.split_at(idx);
    let n: f64 = num.parse().ok()?;
    let mul = match unit {
        "" => 1.0,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        _ => return None,
    };
    Some(n * mul)
}

/// 2.16d: `request <= limit` (both valid quantities). If either is
/// unparseable, return true (defer to the apiserver — the quantity-format
/// check already flagged it separately, so a comparison error here would
/// double-report a single malformed value).
fn quantity_le(req: &str, limit: &str) -> bool {
    match (quantity_to_f64(req), quantity_to_f64(limit)) {
        (Some(r), Some(l)) => r <= l,
        _ => true,
    }
}

/// 2.16d: whether a `resources.{requests|limits}[k]` value is a valid
/// Kubernetes resource quantity. Broader than [`is_k8s_quantity`] (the disk
/// `size` grammar, which never sees CPU) because a resource quantity also
/// covers the CPU `m` (milli-core) suffix: `100m`, `0.5`, `2`. A value is
/// accepted when EITHER the disk grammar accepts it (memory/binary/decimal SI
/// forms, reusing `is_k8s_quantity` so the shape rules — trailing-dot / bare
/// magnitude — stay in one place) OR it normalizes via [`quantity_to_f64`] to
/// a non-negative magnitude (which additionally admits the `m` suffix). The
/// trailing-dot / negative edge cases `quantity_to_f64` would otherwise let
/// through are rejected here so the two callers agree on validity.
fn is_resource_quantity(s: &str) -> bool {
    if is_k8s_quantity(s) {
        return true;
    }
    // CPU milli/core and other quantity_to_f64-parseable forms, minus the
    // laxer edges (`quantity_to_f64` accepts a trailing `.` and negatives).
    if s.trim().ends_with('.') || s.trim().starts_with('-') {
        return false;
    }
    quantity_to_f64(s).is_some_and(|v| v >= 0.0)
}

/// 2.6b-4 (ADR 0043): the `needs.disk` VALUE a scope literally declares,
/// if any. `Some(v)` means the scope's `needs.disk` key is present (even
/// if it is an empty array); `None` means the scope does not declare a
/// disk at all (so it INHERITS base's disk under the per-key needs merge).
/// Mirrors the renderer's `if env_needs.disk.is_some()` override pivot.
fn scope_disk_value(scope: Option<&serde_json::Map<String, Value>>) -> Option<&Value> {
    scope
        .and_then(|o| o.get("needs"))
        .and_then(|v| v.as_object())
        .and_then(|n| n.get("disk"))
}

/// 2.6b-4 (ADR 0043): disk-specific value guards, collected across
/// `spec.base.needs.disk` AND every `spec.environments.*.needs.disk`.
/// For each disk entry: the derived/explicit `name` must be a DNS-1123
/// label (it becomes part of the PVC name) and unique within disk;
/// `mountPath` must be absolute AND unique app-wide; `size` must parse as
/// a Kubernetes quantity; `class` must be `local` (replicated/shared are
/// T2-deferred). These guards run on each scope's LITERAL disk value.
///
/// Separately, the replicas invariant runs on the EFFECTIVE-merged view:
/// a scope's effective disk is its own `needs.disk` if it declares the
/// key, else base's `needs.disk` inherited under the per-key needs merge;
/// its effective replicas is the env override else the base value. When
/// the effective disk is non-empty AND effective replicas > 1 the scope
/// is rejected on `<scope>.replicas` — a standalone RWO PVC supports only
/// a single-replica Deployment at launch (per-replica multi-replica is
/// T2). This catches the bypass where an environment overrides only
/// `replicas` (no needs block) yet inherits base's disk.
///
/// Multi-error: one message per offending field, no short-circuit
/// (matching the validator contract).
///
/// One disk value-guard scope: (field-path prefix, typed view, raw map).
/// The typed view is a `&dyn ScopeView` so base ([`ApplicationBaseSpec`])
/// and env ([`ApplicationEnvOverride`]) scopes coexist in one `Vec`.
type DiskValueScope<'a> = (
    String,
    Option<&'a dyn ScopeView>,
    Option<&'a serde_json::Map<String, Value>>,
);

fn validate_disk_claims(
    typed_base: Option<&ApplicationBaseSpec>,
    typed_envs: Option<&BTreeMap<String, ApplicationEnvOverride>>,
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    // A scope's typed view if it decoded, else `None` (raw fallback). Carries
    // the raw map too so the cannot-model branches (mountPath/size presence)
    // and the deserialize-failure fallback read it. `typed_base` and each env
    // override are erased to `&dyn ScopeView` so the per-key merge below can
    // hold EITHER a base or an env view in one variable.
    let base_view: Option<&dyn ScopeView> = typed_base.map(|b| b as &dyn ScopeView);
    let base_replicas = scope_replicas(base_view, base);

    // ---- replicas guard on the EFFECTIVE-merged view ----
    // base's literal disk-key presence + non-emptiness, inherited by any env
    // that omits the disk key. `Needs.disk: Option<…>` models the key being
    // present (even as an empty array) on the typed path; the raw
    // `scope_disk_value` is the fallback.
    //
    // 2.6c (T10): only OWNED disks gate the single-replica invariant. An
    // owned disk is a standalone RWO PVC (one writer at a time → replicas:1);
    // a REFERENCED disk binds a shared SharedVolume (RWX), so it does not
    // constrain the replica count. The guard therefore counts only the
    // owned entries in each scope's effective disk value.
    let scope_has_owned_disk = |typed_scope: Option<&dyn ScopeView>,
                                raw_scope: Option<&serde_json::Map<String, Value>>|
     -> bool {
        scope_disk_entries(typed_scope, raw_scope)
            .iter()
            .any(|e| !e.is_reference())
    };

    let base_disk_present = scope_disk_present(base_view, base);
    let base_owned_disk = scope_has_owned_disk(base_view, base);

    let mut replicas_scopes: Vec<(String, bool, Option<i64>)> =
        vec![("spec.base".to_string(), base_owned_disk, base_replicas)];
    if let Some(envs_obj) = envs {
        for env_name in envs_obj.keys() {
            let env_view: Option<&dyn ScopeView> = typed_envs
                .and_then(|m| m.get(env_name))
                .map(|e| e as &dyn ScopeView);
            let raw_env = envs_obj.get(env_name).and_then(|v| v.as_object());
            // Per-key needs merge: the env's disk wins when its `disk` key is
            // present (even empty); else it inherits base's disk.
            let (eff_typed, eff_raw, eff_present) = if scope_disk_present(env_view, raw_env) {
                (env_view, raw_env, true)
            } else if base_disk_present {
                (base_view, base, true)
            } else {
                (None, None, false)
            };
            if !eff_present || !scope_has_owned_disk(eff_typed, eff_raw) {
                continue;
            }
            // Env-override replaces base replicas; else inherit base.
            let effective_replicas = scope_replicas(env_view, raw_env).or(base_replicas);
            replicas_scopes.push((
                format!("spec.environments.{env_name}"),
                true,
                effective_replicas,
            ));
        }
    }

    for (prefix, disk_nonempty, effective_replicas) in replicas_scopes {
        if !disk_nonempty {
            continue;
        }
        if let Some(replicas) = effective_replicas {
            if replicas > 1 {
                errors.push(ValidationError::new(
                    format!("{prefix}.replicas"),
                    "persistent disks currently support single-replica apps; use replicas: 1 (per-replica disks for multi-replica apps are T2)",
                ));
            }
        }
    }

    // ---- per-scope LITERAL disk value guards (name/mountPath/size/class) ----
    // mountPath uniqueness is app-wide (across base + every environment):
    // collect every seen mountPath, reporting on the second+ occurrence.
    let mut seen_mount_paths: Vec<String> = Vec::new();

    // Each scope's own field-path prefix + (typed, raw) scope views.
    let mut value_scopes: Vec<DiskValueScope<'_>> =
        vec![("spec.base".to_string(), base_view, base)];
    if let Some(envs_obj) = envs {
        for env_name in envs_obj.keys() {
            value_scopes.push((
                format!("spec.environments.{env_name}"),
                typed_envs
                    .and_then(|m| m.get(env_name))
                    .map(|e| e as &dyn ScopeView),
                envs_obj.get(env_name).and_then(|v| v.as_object()),
            ));
        }
    }

    for (prefix, typed_scope, raw_scope) in value_scopes {
        let entries = scope_disk_entries(typed_scope, raw_scope);
        if entries.is_empty() {
            continue;
        }
        let needs_disk_field = format!("{prefix}.needs.disk");

        // Names unique within this scope's disk value.
        let mut seen_names: Vec<String> = Vec::new();

        for entry in &entries {
            // ---- name (explicit or mountPath-derived) ----
            match entry.derived_name() {
                Some(name) => {
                    if !is_dns_1123_label(&name) {
                        errors.push(ValidationError::new(
                            &needs_disk_field,
                            format!(
                                "needs.disk entry name {name:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric) — it becomes part of the PVC name"
                            ),
                        ));
                    } else if seen_names.contains(&name) {
                        errors.push(ValidationError::new(
                            &needs_disk_field,
                            format!(
                                "needs.disk has a duplicate entry name {name:?}; names must be unique within disk (set an explicit `name` to disambiguate)"
                            ),
                        ));
                    } else {
                        seen_names.push(name);
                    }
                }
                None => {
                    // No explicit name and no usable mountPath segment.
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        "needs.disk entry has no `name` and no usable `mountPath` to derive one from",
                    ));
                }
            }

            // ---- mountPath: absolute + app-wide unique ----
            match entry.mount_path() {
                Some(mp) if mp.starts_with('/') => {
                    if seen_mount_paths.iter().any(|p| p == mp) {
                        errors.push(ValidationError::new(
                            &needs_disk_field,
                            format!(
                                "needs.disk mountPath {mp:?} is declared more than once; each disk mountPath must be unique within the app"
                            ),
                        ));
                    } else {
                        seen_mount_paths.push(mp.to_string());
                    }
                }
                Some(mp) => {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        format!(
                            "needs.disk mountPath {mp:?} must be an absolute path (start with '/')"
                        ),
                    ));
                }
                None => {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        "needs.disk entry is missing the required `mountPath`",
                    ));
                }
            }

            // ---- owned/referenced shape discrimination (2.6c T10) ----
            // The presence of `ref` is the discriminant. The owned shape
            // (`ref` absent) carries `size` (required) and an optional
            // `class`; the referenced shape (`ref` present) binds an existing
            // SharedVolume by name and carries ONLY ref + mountPath + readOnly.
            // The name/mountPath guards above apply to BOTH shapes (mountPath
            // app-wide uniqueness + derived-name uniqueness prevent pod
            // volume-name collisions). The webhook is STATELESS, so SharedVolume
            // EXISTENCE is the controller's job (AwaitingSharedVolume), not ours.
            if let Some(reference) = entry.reference() {
                // ---- referenced shape: only ref + mountPath + readOnly ----
                if reference.contains('/') {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        format!(
                            "needs.disk ref {reference:?} is namespaced; cross-namespace shared volumes require T2 (NFS) and are deferred — reference a SharedVolume in the application's own namespace"
                        ),
                    ));
                }
                if entry.size().is_some() {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        "needs.disk with `ref` must not also set `size`; a referenced disk binds an existing SharedVolume (which owns the capacity) and carries only ref + mountPath + readOnly",
                    ));
                }
                if entry.has_explicit_name() {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        "needs.disk with `ref` must not also set `name`; a referenced disk carries only ref + mountPath + readOnly",
                    ));
                }
                if entry.class().is_some() {
                    errors.push(ValidationError::new(
                        &needs_disk_field,
                        "needs.disk with `ref` must not also set `class`; the storage class is a property of the referenced SharedVolume, not the reference",
                    ));
                }
            } else {
                // ---- owned shape: size required + quantity, class local ----
                match entry.size() {
                    Some(size) if is_k8s_quantity(size) => {}
                    Some(size) => {
                        errors.push(ValidationError::new(
                            &needs_disk_field,
                            format!(
                                "needs.disk size {size:?} must be a Kubernetes quantity (e.g. \"10Gi\", \"500Mi\", \"1G\")"
                            ),
                        ));
                    }
                    None => {
                        errors.push(ValidationError::new(
                            &needs_disk_field,
                            "needs.disk entry is missing the required `size`",
                        ));
                    }
                }

                // ---- class: local only at launch ----
                if let Some(class) = entry.class() {
                    if class != "local" {
                        errors.push(ValidationError::new(
                            &needs_disk_field,
                            format!(
                                "needs.disk class {class:?} is not supported; only `local` is available at launch (replicated/shared classes are T2, deferred)"
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// 2.6c (T10): SHAPE validation for a `SharedVolume` object. The webhook
/// is STATELESS, so this checks only the static shape — `spec.size` must
/// be a Kubernetes quantity and `spec.class` (when set) must be `local`
/// (replicated/shared classes are T2-deferred, matching the owned-disk
/// class rule). EXISTENCE / capacity-fit against referencing Applications
/// is the controller's responsibility, not the webhook's.
///
/// Multi-error: one message per offending field, no short-circuit.
pub fn validate_sharedvolume(obj: &serde_json::Value) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let size = obj
        .pointer("/spec/size")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !is_k8s_quantity(size) {
        errors.push(ValidationError::new(
            "spec.size",
            format!(
                "size {size:?} must be a Kubernetes quantity (e.g. \"10Gi\", \"500Mi\", \"1G\")"
            ),
        ));
    }
    if let Some(class) = obj.pointer("/spec/class").and_then(|v| v.as_str()) {
        if class != "local" {
            errors.push(ValidationError::new(
                "spec.class",
                format!(
                    "class {class:?} is not supported; only `local` is available at launch (replicated/shared classes are T2, deferred)"
                ),
            ));
        }
    }
    errors
}

/// Reserved subject roots an application may never declare a stream over
/// (2.5 / ADR 0061 §6). `$JS.` is the JetStream API itself (ack/flow-control
/// subjects included — ADR 0061 §4.4); `$SYS.` is the NATS system account
/// surface; `_INBOX` (deliberately no trailing dot — see below) is every
/// reply subject this design mints, both the default NATS inbox tree
/// (`_INBOX.>`, used only by `mgr_<ns>`, ADR 0061 §3) and this design's
/// OWN per-claim inbox prefix (`ClaimView::inbox_prefix()` in
/// `resourceclaim-provisioner::nats_accounts` mints `_INBOX_<ns>_<app>` —
/// UNDERSCORE-, not dot-joined). Collecting any of these either breaks
/// the platform's own control plane or reads every other application's
/// in-flight replies and JetStream API responses in the same account.
///
/// Round-7 review (H1): this literal used to be `"_INBOX."` (trailing
/// dot), which a real per-claim prefix like `_INBOX_demo_feeder.>` does
/// NOT start with — so only the default dot-form tree was ever caught,
/// and a stream declared over the underscore-joined form captured a
/// namespace-mate's JetStream API replies (reproduced against
/// nats-server 2.14.3). Dropping the trailing dot is deliberately a
/// WHOLE-STRING prefix widening, not a switch to a first-token check:
/// `_INBOX` contains no `.`, so it lies entirely within the subject's
/// first token either way, and the whole-string form matches this
/// module's existing `starts_with` shape for `$JS.`/`$SYS.` exactly.
const JS_RESERVED_SUBJECT_ROOTS: &[&str] = &["$JS.", "$SYS.", "_INBOX"];

/// The rejection message for `needs.jetstream.persistent` being set —
/// shared, word-for-word, between the typed path (`validate_jetstream_need`)
/// and the raw-fallback path (`check_scope_raw` above) so the two can never
/// silently drift apart. `persistent` exists ONLY to be rejected (ADR 0061
/// §6) — a structural schema prunes unknown fields before a validating
/// webhook runs, so omitting it from the type would drop `persistent: true`
/// silently instead of rejecting it loudly.
fn jetstream_persistent_rejected(scope: &str) -> String {
    format!(
        "{scope}: needs.jetstream.persistent is not a jetstream option — persistence is per-stream (streams[].storage: file | memory), not per-claim"
    )
}

/// The rejection message for `needs.jetstream.name` being set — see
/// [`jetstream_persistent_rejected`] for why this is a shared function
/// rather than two copies of the same string.
fn jetstream_name_rejected(scope: &str) -> String {
    format!(
        "{scope}: needs.jetstream.name is not allowed — jetstream is scalar-only (ADR 0061 §6); two named claims on one application would share one subject prefix and be indistinguishable"
    )
}

/// 2.5 (ADR 0061 §6): local-only validation of one `needs.jetstream` value —
/// everything a webhook can decide from the manifest alone, with no lookup
/// of sibling objects. Cross-object checks (a `consume.from` naming no
/// application in the namespace, a stream's subjects overlapping a
/// neighbour's declared prefix) belong to the provisioner and surface as a
/// condition on resync (ADR 0061 §5: "detection runs on the provisioner
/// resync"), not a webhook rejection — this function does not, and must
/// not, reach for a Kubernetes client. Multi-error: every violation is
/// collected, no short-circuit, matching this file's convention. `scope` is
/// the scope label ("base" or an environment name) — every message is
/// prefixed with it because a flattened error list loses which scope it
/// came from otherwise.
///
/// Every field this function checks for emptiness (`max_bytes`, a
/// `consume` entry's `stream`/`durable`) is a non-`Option` `String` with no
/// serde default — a JSON payload that OMITS the key fails the typed
/// decode before this function ever runs, and is the CRD's `required` list
/// to catch, not this function's. What this function CAN see, and does, is
/// the key present with an empty string.
fn validate_jetstream_need(js: &JetStreamNeed, scope: &str) -> Vec<String> {
    let mut errs = Vec::new();

    if js.persistent.is_some() {
        errs.push(jetstream_persistent_rejected(scope));
    }
    if js.name.is_some() {
        errs.push(jetstream_name_rejected(scope));
    }

    // Stream name uniqueness within the application — load-bearing for the
    // consume/durable collision check below, which reads this same set.
    let mut seen_stream_names: Vec<&str> = Vec::new();

    for (idx, stream) in js.streams.iter().enumerate() {
        if seen_stream_names.contains(&stream.name.as_str()) {
            errs.push(format!(
                "{scope}: needs.jetstream.streams has a duplicate stream name {:?}; stream names must be unique within the application",
                stream.name
            ));
        } else {
            seen_stream_names.push(&stream.name);
        }

        // The provisioner composes this into the NATS-side stream name as
        // `<app>_<name>` (ADR 0061 §3/§6; `nats_stream_name` in
        // `resourceclaim-provisioner::nats_accounts`) — `_` is the join
        // character, so a `_` inside `name` itself would make that
        // encoding ambiguous. Two different applications could then
        // compose the IDENTICAL NATS stream name for two different
        // declared streams, and that composed name is what NATS access
        // rules are keyed on: it decides which application may read,
        // publish to, and purge the underlying stream. A DNS-1123 label
        // (this repo's `is_dns_1123_label`) excludes `_` by construction,
        // which is what keeps the join injective.
        if !is_dns_1123_label(&stream.name) {
            errs.push(format!(
                "{scope}: needs.jetstream.streams[{idx}] name {:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric) — it becomes half of the composed NATS stream name `<app>_<name>`; a '_' in it would let two different applications compose the identical NATS stream name",
                stream.name
            ));
        }

        // `streams[{idx} {name:?}]`, not just `{name:?}`: two streams
        // sharing a name (the very thing rejected above) would otherwise
        // make two DIFFERENT array elements' messages read identically,
        // with nothing to tell a human which one is missing maxBytes and
        // which is missing subjects.
        if stream.max_bytes.trim().is_empty() {
            errs.push(format!(
                "{scope}: needs.jetstream.streams[{idx} {:?}].maxBytes must not be empty — a stream without it silently claims the whole account quota",
                stream.name
            ));
        }

        if stream.subjects.is_empty() {
            errs.push(format!(
                "{scope}: needs.jetstream.streams[{idx} {:?}].subjects must declare at least one subject",
                stream.name
            ));
        }

        for subject in &stream.subjects {
            let trimmed = subject.trim();
            if trimmed == ">" {
                errs.push(format!(
                    "{scope}: needs.jetstream.streams[{idx} {:?}] declares subject {subject:?} — a bare '>' collects the ENTIRE account, not just this application's traffic",
                    stream.name
                ));
                continue;
            }
            if let Some(root) = JS_RESERVED_SUBJECT_ROOTS
                .iter()
                .find(|r| trimmed.starts_with(**r))
            {
                errs.push(format!(
                    "{scope}: needs.jetstream.streams[{idx} {:?}] declares subject {subject:?}, which starts with the reserved root {root:?} (JetStream API / system / inbox subjects — collecting them breaks the platform's own control plane)",
                    stream.name
                ));
            }
        }

        // 2.28 (ADR 0065 §2.3): the isolation-breaking fields, declared in
        // the type ONLY so this rejection is reachable. A structural schema
        // prunes an unknown field before any webhook runs, so omitting them
        // would make `mirror:` vanish silently and the manifest appear to
        // work — the failure ADR 0061 §6 established the pattern against.
        for (field, present, why) in [
            (
                "sources",
                stream.sources.is_some(),
                "a source copies another stream's messages into this one, which ADR 0061 §4.1 MEASURED as defeating the read half of the deny vector",
            ),
            (
                "mirror",
                stream.mirror.is_some(),
                "a mirror copies another stream wholesale, which ADR 0061 §4.1 MEASURED as defeating the read half of the deny vector",
            ),
            (
                "republish",
                stream.republish.is_some(),
                "republish re-emits this stream's traffic under a subject the application itself could not publish to",
            ),
            (
                "subjectTransform",
                stream.subject_transform.is_some(),
                "a subject transform rewrites messages onto subjects outside this application's prefix",
            ),
            (
                "placement",
                stream.placement.is_some(),
                "placement is clustered topology, which arrives with Tier 2",
            ),
            (
                "replicas",
                stream.replicas.is_some(),
                "replication is clustered topology, which arrives with Tier 2",
            ),
        ] {
            if present {
                errs.push(format!(
                    "{scope}: needs.jetstream.streams[{idx} {:?}].{field} is not supported — {why}. It is declared in the schema only so this refusal reaches you: an undeclared field would be pruned by the apiserver before this webhook ran, and the manifest would appear to work.",
                    stream.name
                ));
            }
        }

        // `discardPerSubject` carries two server-side preconditions (error
        // 10052, measured on 2.14.3). Stated here so the refusal names a
        // field, rather than surfacing through NACK as an opaque stream
        // creation failure with no path attached.
        if stream.discard_per_subject == Some(true) {
            if stream.discard.as_deref() != Some("new") {
                errs.push(format!(
                    "{scope}: needs.jetstream.streams[{idx} {:?}].discardPerSubject requires discard: \"new\" (the server rejects it otherwise)",
                    stream.name
                ));
            }
            if stream.max_msgs_per_subject.unwrap_or(0) <= 0 {
                errs.push(format!(
                    "{scope}: needs.jetstream.streams[{idx} {:?}].discardPerSubject requires maxMsgsPerSubject > 0 — there is no per-subject ceiling to discard against otherwise (the server rejects it)",
                    stream.name
                ));
            }
        }
    }

    // 2.28: a `deadLetter` materialises an ORDINARY declared stream owned by
    // this application (ADR 0065 §2.4), so its name shares the declared-stream
    // namespace and must be collected BEFORE the durable-collision check runs.
    // Collected in a pre-pass rather than inside the consume loop: a durable
    // in an EARLIER entry must still collide with a DLQ declared in a LATER
    // one, and an in-loop collection would only ever see the DLQs before it.
    for (idx, consume) in js.consume.iter().enumerate() {
        let Some(dlq) = consume.dead_letter.as_ref() else {
            continue;
        };
        if dlq.stream.trim().is_empty() {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].deadLetter.stream must not be empty"
            ));
            continue;
        }
        if !is_dns_1123_label(&dlq.stream) {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].deadLetter.stream {:?} must be a DNS-1123 label — it composes into the NATS-side stream name `<app>_<name>` exactly as a declared stream does, and a '_' in it would make that join ambiguous",
                dlq.stream
            ));
        } else if seen_stream_names.contains(&dlq.stream.as_str()) {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].deadLetter.stream {:?} collides with a stream this application already declares; a dead-letter queue IS a declared stream and shares that namespace",
                dlq.stream
            ));
        } else {
            seen_stream_names.push(&dlq.stream);
        }
        if dlq.max_bytes.trim().is_empty() {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].deadLetter.maxBytes must not be empty — the dead-letter queue counts against the namespace quota like any other declared stream"
            ));
        }
    }

    for (idx, consume) in js.consume.iter().enumerate() {
        if consume.stream.trim().is_empty() {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].stream must not be empty"
            ));
        }
        if consume.durable.trim().is_empty() {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].durable must not be empty"
            ));
        } else if !is_dns_1123_label(&consume.durable) {
            // Same reasoning as `streams[].name` above: composed into the
            // NATS-side durable name as `<app>_<durable>`
            // (`nats_durable_name` in
            // `resourceclaim-provisioner::nats_accounts`). A `_` in
            // `durable` would let two different applications compose the
            // identical NATS durable name and silently share one
            // consumer's delivery cursor.
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].durable {:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric) — it becomes half of the composed NATS durable name `<app>_<durable>`; a '_' in it would let two different applications compose the identical NATS durable name and silently share one consumer's delivery cursor",
                consume.durable
            ));
        } else if seen_stream_names.contains(&consume.durable.as_str()) {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}] durable {:?} collides with a stream name declared by this application — the deny vector's position-pattern grants ($JS.API.*.*.*.S) are safe only because a durable can never share a name with one of this application's own streams; use a different durable name",
                consume.durable
            ));
        }

        // 2.28 (ADR 0065 §2.3): the push surface, declared to be rejected.
        // Push delivery is performed by the SERVER, outside the
        // application's publish permissions — a write channel into a
        // neighbour's prefix, which is why none of these four can be
        // offered at all rather than merely constrained.
        for (field, present) in [
            ("deliverSubject", consume.deliver_subject.is_some()),
            ("deliverGroup", consume.deliver_group.is_some()),
            ("flowControl", consume.flow_control.is_some()),
            ("heartbeatInterval", consume.heartbeat_interval.is_some()),
        ] {
            if present {
                errs.push(format!(
                    "{scope}: needs.jetstream.consume[{idx}].{field} is not supported — it selects PUSH delivery, which the server performs outside this application's publish permissions and can therefore deliver into a neighbour's subject tree. Pull consumers carry no such channel. Declared in the schema only so this refusal reaches you rather than the field being pruned."
                ));
            }
        }
        if consume.replicas.is_some() {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].replicas is not supported — consumer replication is clustered topology, which arrives with Tier 2"
            ));
        }

        // The server rejects both filter forms together; say so here, where
        // the field names are visible.
        if consume.filter_subject.is_some() && consume.filter_subjects.is_some() {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}] sets both filterSubject and filterSubjects; they are mutually exclusive — use one"
            ));
        }

        // A start position under the wrong policy is REJECTED, not ignored:
        // an ignored start position is a consumer that silently reads from
        // the wrong place, which looks like data loss.
        let policy = consume.deliver_policy.as_deref();
        if consume.opt_start_seq.is_some() && policy != Some("byStartSequence") {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].optStartSeq requires deliverPolicy: \"byStartSequence\" (got {policy:?}); under any other policy the server ignores it and the consumer silently starts somewhere else"
            ));
        }
        if consume.opt_start_time.is_some() && policy != Some("byStartTime") {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].optStartTime requires deliverPolicy: \"byStartTime\" (got {policy:?}); under any other policy the server ignores it and the consumer silently starts somewhere else"
            ));
        }

        // Measured on 2.14.3: `max deliver is required to be > length of
        // backoff values` (10116) — STRICTLY greater. The equal case is the
        // one a loose reading of the rule admits.
        if let Some(backoff) = consume.backoff.as_ref() {
            if !backoff.is_empty() {
                let max_deliver = consume.max_deliver.unwrap_or(0);
                if max_deliver <= backoff.len() as i64 {
                    errs.push(format!(
                        "{scope}: needs.jetstream.consume[{idx}] declares {} backoff step(s) but maxDeliver {}; the server requires maxDeliver to be STRICTLY greater than the number of backoff steps",
                        backoff.len(),
                        consume.max_deliver.map_or("unset".to_string(), |v| v.to_string())
                    ));
                }
            }
        }

        // A dead-letter queue with no delivery ceiling never receives
        // anything: the advisory it collects fires only when redelivery is
        // exhausted. Rejected rather than warned, because the failure is
        // a manifest that looks like it works and stays silent forever.
        if consume.dead_letter.is_some() && consume.max_deliver.unwrap_or(0) <= 0 {
            errs.push(format!(
                "{scope}: needs.jetstream.consume[{idx}].deadLetter requires maxDeliver > 0 — the dead-letter queue collects the advisory the server publishes when redelivery is EXHAUSTED, so with no ceiling it would stay empty forever"
            ));
        }
    }

    errs
}

/// 2.5 (ADR 0061 §6): the rejection message for an environment override of
/// `needs.jetstream` that silently drops a block `base` declares.
///
/// `lost` is rendered into the message because the whole point of the rule is
/// that the loss is otherwise invisible: the manifest that causes it —
/// `environments.prod.needs.jetstream: {size: small}` — does not mention
/// streams or consume at all, so an error that only named the FIELD would
/// read as pedantry rather than as "prod is about to lose these three
/// streams".
fn jetstream_env_override_drops(scope: &str, block: &str, lost: &[String]) -> String {
    format!(
        "{scope}: needs.jetstream omits {block:?} while base declares {} of them ({}) — a \
         per-environment override REPLACES the whole needs.jetstream block (ADR 0061 §6), so \
         this environment would lose them entirely. Repeat them here, or write \"{block}: []\" \
         to state that this environment deliberately has none.",
        lost.len(),
        lost.join(", "),
    )
}

/// 2.5 (ADR 0061 §6): refuse an environment override of `needs.jetstream`
/// that omits `streams`/`consume` while `base` declares them.
///
/// Every `needs.<type>` key is replaced WHOLESALE by an environment override
/// (`effective_spec` in `operator-rendering`: `if env_needs.jetstream.is_some()
/// { merged.jetstream = env_needs.jetstream.clone() }` — a clone of the env
/// value, never a merge into base's). For most needs that costs a `size`; for
/// jetstream it costs the entire producer/consumer contract, and the
/// application then runs in that environment publishing to streams that were
/// never created. ADR 0061 §6 rejected leaving it silent, and rejected
/// special-casing jetstream into a deep merge (that would pre-empt 2.16i and
/// make one need behave unlike the other six). This rule retires when 2.16i
/// lands the deep merge generally.
///
/// KEY PRESENCE, not emptiness, is the trigger — read off the RAW override
/// (`envs`), because the typed `JetStreamNeed.streams`/`.consume` are
/// `#[serde(default)]` `Vec`s that cannot tell "omitted" from "declared
/// empty". That distinction is the escape hatch: an environment that
/// genuinely has no streams writes `streams: []` and says so, which is the
/// only way to express that intent under wholesale replacement. Rejecting the
/// explicit empty list too would leave no way to say it at all.
///
/// Base-side declarations are read from the TYPED base, matching the rest of
/// [`validate_jetstream_scopes`]; a base that failed typed decode is skipped
/// rather than guessed at.
fn validate_jetstream_env_overrides(
    typed_base: Option<&ApplicationBaseSpec>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    let Some(base_js) = typed_base
        .and_then(ScopeView::needs)
        .and_then(|n| n.jetstream.as_ref())
    else {
        return;
    };
    if base_js.streams.is_empty() && base_js.consume.is_empty() {
        return;
    }
    let Some(envs_obj) = envs else { return };

    for (env_name, env_value) in envs_obj {
        // Only an override that DECLARES needs.jetstream replaces base's.
        // An environment with no `needs` block, or a `needs` block without
        // the jetstream key, inherits base's whole need under the per-key
        // merge — nothing is lost and nothing is reported.
        let Some(js_obj) = env_value
            .pointer("/needs/jetstream")
            .and_then(Value::as_object)
        else {
            continue;
        };
        let field = format!("spec.environments.{env_name}.needs.jetstream");

        if !base_js.streams.is_empty() && !js_obj.contains_key("streams") {
            let lost: Vec<String> = base_js
                .streams
                .iter()
                .map(|s| format!("{:?}", s.name))
                .collect();
            errors.push(ValidationError::new(
                field.clone(),
                jetstream_env_override_drops(env_name, "streams", &lost),
            ));
        }
        if !base_js.consume.is_empty() && !js_obj.contains_key("consume") {
            // `<stream>/<durable>` — a consume entry has no single name, and
            // the durable alone would not say which stream it reads.
            let lost: Vec<String> = base_js
                .consume
                .iter()
                .map(|c| format!("{:?}", format!("{}/{}", c.stream, c.durable)))
                .collect();
            errors.push(ValidationError::new(
                field,
                jetstream_env_override_drops(env_name, "consume", &lost),
            ));
        }
    }
}

/// 2.5 (ADR 0061 §6): wires [`validate_jetstream_need`] into the per-scope
/// needs validation, once for `base` and once per declared environment —
/// same shape as `validate_needs_names`/`validate_disk_claims`. TYPED ONLY:
/// `validate_jetstream_need` takes a `&JetStreamNeed`, so a scope that
/// failed to decode is silently skipped here. That IS reachable in
/// production, not just in tests or a misconfigured apiserver — see the
/// comment above `check_scope_raw` for the `expose.hostname` example —
/// which is why `persistent`/`name` (the two fields that exist solely to
/// be rejected) are ALSO checked on the raw path there, via the same
/// [`jetstream_persistent_rejected`]/[`jetstream_name_rejected`] messages.
/// `subjects`/`maxBytes` stay typed-only here: the CRD's `minItems`/
/// `required` cover those unconditionally, before any webhook runs at
/// all, typed decode or not.
fn validate_jetstream_scopes(
    typed_base: Option<&ApplicationBaseSpec>,
    typed_envs: Option<&BTreeMap<String, ApplicationEnvOverride>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(js) = typed_base
        .and_then(ScopeView::needs)
        .and_then(|n| n.jetstream.as_ref())
    {
        for msg in validate_jetstream_need(js, "base") {
            errors.push(ValidationError::new("spec.base.needs.jetstream", msg));
        }
    }
    if let Some(envs_obj) = envs {
        for env_name in envs_obj.keys() {
            if let Some(js) = typed_envs
                .and_then(|m| m.get(env_name))
                .and_then(ScopeView::needs)
                .and_then(|n| n.jetstream.as_ref())
            {
                for msg in validate_jetstream_need(js, env_name) {
                    errors.push(ValidationError::new(
                        format!("spec.environments.{env_name}.needs.jetstream"),
                        msg,
                    ));
                }
            }
        }
    }
    // The cross-scope half: what an override DROPS, which no single-scope
    // validation can see.
    validate_jetstream_env_overrides(typed_base, envs, errors);
}

/// Whether `spec` declares `needs.jetstream` ANYWHERE — `base` or any
/// entry under `environments` — used only to GATE
/// [`validate_application_name_for_jetstream`], so it needs presence
/// only, not shape: a raw `Value::pointer` walk, not the typed
/// decode/raw-fallback split `validate_jetstream_scopes` uses to
/// validate the need's own FIELDS. That split exists because a scope
/// that fails typed decode still needs `persistent`/`name` rejected
/// (reachable in production, not just tests — see the comment above
/// `check_scope_raw`); this function only asks "is the key there", which
/// a raw pointer answers unconditionally, typed decode or not.
fn application_spec_has_jetstream_need(spec: &Value) -> bool {
    let has_jetstream = |scope: &Value| scope.pointer("/needs/jetstream").is_some();
    if spec.pointer("/base").is_some_and(has_jetstream) {
        return true;
    }
    spec.get("environments")
        .and_then(Value::as_object)
        .is_some_and(|envs| envs.values().any(has_jetstream))
}

/// Round-7 review (H4): `metadata.name` must be a DNS-1123 LABEL, not
/// merely the SUBDOMAIN the apiserver's own object-name validation
/// already guarantees, whenever `needs.jetstream` is present anywhere in
/// the spec. `nats_stream_name` (resourceclaim-provisioner::nats_accounts)
/// composes `<app>_<declared>` into a NATS stream name — a single token,
/// per NATS's own naming rule, so a `.` in `app` makes the composed name
/// illegal outright — and `ClaimView::subject_prefix()` composes `<app>.`
/// into the per-app subject partition, where a `.` in `app` nests one
/// app's partition inside a DIFFERENT, shorter-named app's own prefix
/// (`my.app.` inside `my.`'s `my.>`), letting that shorter-named app
/// publish into the dotted one's tree. A label is strictly narrower than
/// the subdomain the apiserver already enforces, so this only ever
/// REJECTS what would otherwise be silently accepted — never the
/// reverse.
///
/// Deliberately NOT a `name: Option<&str>` parameter threaded through
/// [`validate_application_spec`]: that function has roughly 120 call
/// sites in this file's own test suite alone, all exercising `spec`
/// alone (the same shape as the metadata-vs-spec split problem found
/// before). `server.rs`'s `validate_handler` already holds both
/// `object.metadata` and `object.spec` at the one call site that needs
/// both — for the "Application" kind, before dispatching to
/// `validate_application_spec` — so the check is wired in there instead,
/// as a second, independent call whose errors are merged into the same
/// response.
pub fn validate_application_name_for_jetstream(name: &str, spec: &Value) -> Vec<ValidationError> {
    if !application_spec_has_jetstream_need(spec) {
        return Vec::new();
    }
    if is_dns_1123_label(name) {
        return Vec::new();
    }
    vec![ValidationError::new(
        "metadata.name",
        format!(
            "metadata.name {name:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric, max 63 chars) when needs.jetstream is set — it is composed into NATS stream names and subject prefixes, neither of which can contain '.'; a Kubernetes object name alone (a DNS-1123 subdomain) is not narrow enough"
        ),
    )]
}

/// 1.83b: validate the `expose` block in BOTH base and every environment.
///   - `network == "vpn"`  → reject (reserved until AccessGrant/ExternalSurface);
///   - `network == "public"` → `hostname` REQUIRED, and every entry a DNS-1123
///     subdomain (a concrete host, not a wildcard — the wildcard is the Gateway
///     listener's, not the route's);
///   - `hostname` set with `network != "public"` → reject (hostname is
///     meaningless without public exposure; catches the `network:public` typo);
///   - `tls == false` with `network == "public"` → reject (HTTP-only public
///     exposure is deferred to 4.1b's `#TlsOptions`; this slice's route
///     attaches to `:443` only).
///
/// `hostname` `OneOrMany` is normalized (scalar → `[scalar]`) before the check.
fn validate_expose(
    typed_base: Option<&ApplicationBaseSpec>,
    typed_envs: Option<&BTreeMap<String, ApplicationEnvOverride>>,
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    fn check_scope(
        prefix: &str,
        typed: Option<&dyn ScopeView>,
        raw: Option<&serde_json::Map<String, Value>>,
        errors: &mut Vec<ValidationError>,
    ) {
        // network + hostnames + tls, typed-first with a raw fallback. The
        // typed reads go through `ScopeView`, which projects the differing
        // expose structs (`ApplicationExpose` / `ExposeOverride`) onto their
        // common `network`/`hostname`/`tls` fields.
        let (network, hostnames, tls): (Option<String>, Vec<String>, Option<bool>) =
            match typed.filter(|s| s.has_expose()) {
                Some(s) => (
                    s.expose_network().map(String::from),
                    s.expose_hostname()
                        .map(|h| h.as_slice_vec())
                        .unwrap_or_default(),
                    s.expose_tls(),
                ),
                None => {
                    let expose = raw
                        .and_then(|o| o.get("expose"))
                        .and_then(|v| v.as_object());
                    let network = expose
                        .and_then(|e| e.get("network"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let hostnames = match expose.and_then(|e| e.get("hostname")) {
                        Some(Value::String(s)) => vec![s.clone()],
                        Some(Value::Array(a)) => a
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect(),
                        _ => Vec::new(),
                    };
                    let tls = expose.and_then(|e| e.get("tls")).and_then(|v| v.as_bool());
                    (network, hostnames, tls)
                }
            };
        // expose absent → nothing to check (port-only / no expose).
        let expose_present = typed.is_some_and(|s| s.has_expose())
            || raw.is_some_and(|o| o.get("expose").is_some_and(|v| v.is_object()));
        if !expose_present {
            return;
        }

        let is_public = network.as_deref() == Some("public");

        if network.as_deref() == Some("vpn") {
            errors.push(ValidationError::new(
                format!("{prefix}.expose.network"),
                "expose.network: vpn is not yet implemented — coming with AccessGrant/ExternalSurface",
            ));
        }

        if is_public {
            if hostnames.is_empty() {
                errors.push(ValidationError::new(
                    format!("{prefix}.expose.hostname"),
                    "expose.hostname is required when expose.network: public",
                ));
            } else {
                for h in &hostnames {
                    if !is_dns_1123_subdomain(h) {
                        errors.push(ValidationError::new(
                            format!("{prefix}.expose.hostname"),
                            format!(
                                "expose.hostname {h:?} must be a DNS-1123 subdomain (a concrete host like \"app.demo.dev\", not a wildcard — the wildcard lives on the Gateway listener)"
                            ),
                        ));
                    }
                }
            }
            if tls == Some(false) {
                errors.push(ValidationError::new(
                    format!("{prefix}.expose.tls"),
                    "expose.tls: false (HTTP-only public exposure) is not yet implemented — coming with #TlsOptions in 4.1b; the public route terminates TLS on the platform Gateway listener",
                ));
            }
        } else if !hostnames.is_empty() {
            errors.push(ValidationError::new(
                format!("{prefix}.expose.hostname"),
                "expose.hostname requires expose.network: public",
            ));
        }
    }

    check_scope(
        "spec.base",
        typed_base.map(|b| b as &dyn ScopeView),
        base,
        errors,
    );
    if let Some(envs_obj) = envs {
        for (env_name, val) in envs_obj {
            check_scope(
                &format!("spec.environments.{env_name}"),
                typed_envs
                    .and_then(|m| m.get(env_name))
                    .map(|e| e as &dyn ScopeView),
                val.as_object(),
                errors,
            );
        }
    }
}

/// 2.28 (ADR 0065 §1.5): validate `spec.base.probes` AND every
/// `spec.environments.*.probes`.
///
/// Five rules, each of them something the CRD's structural schema cannot
/// state:
///   1. `path` starts with `/`. Also a CRD `pattern` (`schemas/crdmeta`),
///      restated here for the same reason the jetstream format rules are —
///      a cluster whose CRD predates that patch is still covered.
///   2. `scheme` / `headers` require `path`. Silently ignoring them would
///      hide a typo'd path: the user wrote an HTTP probe and got a TCP one.
///   3. Every probe resolves a port — its own, or the scope's effective
///      `expose.port`. Without this the renderer would emit nothing (it
///      refuses to invent a port-0 probe) and the pod would simply be
///      unprobed with nothing saying why.
///   4. `successThreshold` is 1 on liveness and startup. Kubernetes requires
///      it; rejecting here names the field, whereas letting it through means
///      the apiserver rejects the *Deployment* far from its cause.
///   5. `timeoutSeconds < periodSeconds`. Kubernetes permits the overlap; we
///      do not, because overlapping probe attempts are always a mistake and
///      the refusal costs one edit.
///
/// **Typed-only, with no raw fallback**, unlike `validate_resources` and the
/// `image` rules. A probe is a nested structure whose raw re-implementation
/// would be a second parser to keep in sync, and the scopes that fail to
/// deserialize are exactly the ones the apiserver's structural validation
/// has already rejected.
///
/// The port in rule 3 is resolved PER SCOPE: an environment uses its own
/// `expose.port` when it sets one and inherits the base's otherwise (2.16c
/// deep-merge). A single whole-object check would pass every scope or fail
/// every scope.
fn validate_probes(
    typed_base: Option<&ApplicationBaseSpec>,
    typed_envs: Option<&BTreeMap<String, ApplicationEnvOverride>>,
    errors: &mut Vec<ValidationError>,
) {
    fn check_one(
        prefix: &str,
        name: &str,
        p: &Probe,
        port: Option<i32>,
        errors: &mut Vec<ValidationError>,
    ) {
        let field = |f: &str| format!("{prefix}.probes.{name}.{f}");

        match p.path.as_deref() {
            Some(path) => {
                if !path.starts_with('/') {
                    errors.push(ValidationError::new(
                        field("path"),
                        format!("probe path {path:?} must start with '/'"),
                    ));
                }
            }
            None => {
                if p.scheme.is_some() {
                    errors.push(ValidationError::new(
                        field("scheme"),
                        "`scheme` applies to an HTTP probe; this probe declares no `path`, so it is a TCP connect",
                    ));
                }
                if p.headers.is_some() {
                    errors.push(ValidationError::new(
                        field("headers"),
                        "`headers` applies to an HTTP probe; this probe declares no `path`, so it is a TCP connect",
                    ));
                }
            }
        }

        if p.port.is_none() && port.is_none() {
            errors.push(ValidationError::new(
                field("port"),
                format!(
                    "probe {name:?} declares no `port` and this scope has no `expose.port` to inherit"
                ),
            ));
        }

        if matches!(name, "liveness" | "startup") {
            if let Some(st) = p.success_threshold {
                if st != 1 {
                    errors.push(ValidationError::new(
                        field("successThreshold"),
                        format!(
                            "a {name} probe must have successThreshold 1 (Kubernetes rejects any \
                             other value on the Deployment); got {st}"
                        ),
                    ));
                }
            }
        }

        if let (Some(t), Some(period)) = (p.timeout_seconds, p.period_seconds) {
            if t >= period {
                errors.push(ValidationError::new(
                    field("timeoutSeconds"),
                    format!(
                        "timeoutSeconds {t} must be less than periodSeconds {period}; otherwise \
                         probe attempts overlap"
                    ),
                ));
            }
        }
    }

    fn check_scope(
        prefix: &str,
        probes: Option<&Probes>,
        port: Option<i32>,
        errors: &mut Vec<ValidationError>,
    ) {
        let Some(pr) = probes else { return };
        for (name, probe) in [
            ("liveness", pr.liveness.as_ref()),
            ("readiness", pr.readiness.as_ref()),
            ("startup", pr.startup.as_ref()),
        ] {
            if let Some(p) = probe {
                check_one(prefix, name, p, port, errors);
            }
        }
    }

    let base_port = typed_base.and_then(ScopeView::expose_port);
    check_scope(
        "spec.base",
        typed_base.and_then(ScopeView::probes),
        base_port,
        errors,
    );
    if let Some(envs) = typed_envs {
        for (name, env) in envs {
            check_scope(
                &format!("spec.environments.{name}"),
                env.probes(),
                env.expose_port().or(base_port),
                errors,
            );
        }
    }
}

/// 2.16d: validate `spec.base.resources` AND every
/// `spec.environments.*.resources`. This is value-INDEPENDENT — a purely
/// syntactic check, mirroring `validate_expose`:
///   1. every `requests[k]` / `limits[k]` value must be a Kubernetes quantity
///      (`is_resource_quantity`, which reuses the disk-`size` `is_k8s_quantity`
///      grammar and additionally admits the CPU `m` suffix) — an error on
///      `<scope>.resources.{requests|limits}.<key>` per malformed value;
///   2. for each key present in BOTH `requests` and `limits`, the request must
///      be `<= limit` (`quantity_le`) — an error on `<scope>.resources`.
///
/// The apiserver's structural CRD validation and the deep-merge at render
/// happen elsewhere; this only rejects locally-inconsistent quantities.
///
/// Typed-first via `ScopeView::resources` (compiler-gated `resources` field);
/// a scope that failed to deserialize (test / misconfigured apiserver) falls
/// back to reading the raw `resources` object — matching the pattern of the
/// other per-scope rules.
fn validate_resources(
    typed_base: Option<&ApplicationBaseSpec>,
    typed_envs: Option<&BTreeMap<String, ApplicationEnvOverride>>,
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    // One (map-name, map) pair for the two resource kinds, in a fixed order.
    fn check_maps(
        prefix: &str,
        requests: &BTreeMap<String, String>,
        limits: &BTreeMap<String, String>,
        errors: &mut Vec<ValidationError>,
    ) {
        for (kind, map) in [("requests", requests), ("limits", limits)] {
            for (key, val) in map {
                if !is_resource_quantity(val) {
                    errors.push(ValidationError::new(
                        format!("{prefix}.resources.{kind}.{key}"),
                        format!(
                            "resources.{kind}.{key} value {val:?} must be a Kubernetes quantity (e.g. \"256Mi\", \"100m\", \"1Gi\")"
                        ),
                    ));
                }
            }
        }
        // Cross-field: for each key in BOTH maps, request must be <= limit.
        for (key, req_val) in requests {
            if let Some(lim_val) = limits.get(key) {
                if !quantity_le(req_val, lim_val) {
                    errors.push(ValidationError::new(
                        format!("{prefix}.resources"),
                        format!(
                            "requests.{key} {req_val:?} > limits.{key} {lim_val:?}; a resource request must not exceed its limit"
                        ),
                    ));
                }
            }
        }
    }

    // Read a scope's `requests`/`limits` maps typed-first, raw as fallback.
    // Returns owned `BTreeMap`s so the two sources share one code path
    // (the raw fallback string-clones; the typed path clones the field maps).
    fn scope_maps(
        typed: Option<&dyn ScopeView>,
        raw: Option<&serde_json::Map<String, Value>>,
    ) -> Option<(BTreeMap<String, String>, BTreeMap<String, String>)> {
        // Extract a `{k: quantity-string}` map from a raw JSON object, keeping
        // only string values (non-string values are rejected by the CRD layer).
        fn raw_map(v: Option<&Value>) -> BTreeMap<String, String> {
            v.and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        }
        match typed.and_then(ScopeView::resources) {
            Some(res) => Some((
                res.requests.clone().unwrap_or_default(),
                res.limits.clone().unwrap_or_default(),
            )),
            None => raw
                .and_then(|o| o.get("resources"))
                .and_then(Value::as_object)
                // No `resources` key at all → nothing to check.
                .map(|o| (raw_map(o.get("requests")), raw_map(o.get("limits")))),
        }
    }

    let check_scope = |prefix: &str,
                       typed: Option<&dyn ScopeView>,
                       raw: Option<&serde_json::Map<String, Value>>,
                       errors: &mut Vec<ValidationError>| {
        if let Some((requests, limits)) = scope_maps(typed, raw) {
            check_maps(prefix, &requests, &limits, errors);
        }
    };

    check_scope(
        "spec.base",
        typed_base.map(|b| b as &dyn ScopeView),
        base,
        errors,
    );
    if let Some(envs_obj) = envs {
        for (env_name, val) in envs_obj {
            check_scope(
                &format!("spec.environments.{env_name}"),
                typed_envs
                    .and_then(|m| m.get(env_name))
                    .map(|e| e as &dyn ScopeView),
                val.as_object(),
                errors,
            );
        }
    }
}

/// A scope's `replicas` value as `i64`. Typed `Option<i32>` on the happy
/// path (compiler-gated `replicas` field); the raw `as_i64()` is the
/// deserialize-failure fallback.
fn scope_replicas(
    typed_scope: Option<&dyn ScopeView>,
    raw_scope: Option<&serde_json::Map<String, Value>>,
) -> Option<i64> {
    match typed_scope {
        Some(s) => s.replicas().map(i64::from),
        None => raw_scope
            .and_then(|o| o.get("replicas"))
            .and_then(|v| v.as_i64()),
    }
}

/// Whether a scope LITERALLY declares the `needs.disk` key (even as an
/// empty array). `Needs.disk: Option<OneOrMany<DiskClaim>>` models the key
/// being present on the typed path; the raw `scope_disk_value` is the
/// fallback. The per-key needs merge pivots on this presence.
fn scope_disk_present(
    typed_scope: Option<&dyn ScopeView>,
    raw_scope: Option<&serde_json::Map<String, Value>>,
) -> bool {
    match typed_scope {
        Some(s) => s.needs().is_some_and(|n| n.disk.is_some()),
        None => scope_disk_value(raw_scope).is_some(),
    }
}

/// A view over one disk entry that reads its fields from the TYPED
/// `DiskClaim` when the scope decoded, falling back to the raw map
/// otherwise. The renderer-load-bearing fields (`name` derivation,
/// `mountPath`, `size`, `class`) are compiler-gated on the typed path; the
/// raw variant preserves the pre-refactor `as_str()` semantics for a scope
/// that failed to deserialize (test / misconfigured apiserver).
enum DiskEntry<'a> {
    Typed(&'a DiskClaim),
    Raw(&'a serde_json::Map<String, Value>),
}

impl DiskEntry<'_> {
    /// Explicit `name`, else the last non-empty `mountPath` segment.
    fn derived_name(&self) -> Option<String> {
        match self {
            DiskEntry::Typed(d) => {
                if let Some(n) = d.name.as_deref().filter(|n| !n.is_empty()) {
                    return Some(n.to_string());
                }
                d.mount_path
                    .rsplit('/')
                    .find(|seg| !seg.is_empty())
                    .map(|seg| seg.to_string())
            }
            DiskEntry::Raw(o) => disk_name(o),
        }
    }

    fn mount_path(&self) -> Option<&str> {
        match self {
            // `mount_path` is non-`Option` on `DiskClaim`, so the typed path
            // always has it (a missing one fails the deserialize → raw path).
            DiskEntry::Typed(d) => Some(d.mount_path.as_str()),
            DiskEntry::Raw(o) => o.get("mountPath").and_then(|v| v.as_str()),
        }
    }

    fn size(&self) -> Option<&str> {
        match self {
            // `size` is `Option<String>` on `DiskClaim` (2.6c: owned|referenced
            // disjunction); owned entries carry the size, referenced ones don't.
            DiskEntry::Typed(d) => d.size.as_deref(),
            DiskEntry::Raw(o) => o.get("size").and_then(|v| v.as_str()),
        }
    }

    fn class(&self) -> Option<&str> {
        match self {
            DiskEntry::Typed(d) => d.class.as_deref(),
            DiskEntry::Raw(o) => o.get("class").and_then(|v| v.as_str()),
        }
    }

    /// 2.6c (T10): the `ref` value when this entry is the REFERENCED shape
    /// (binds an existing `SharedVolume`); `None` for the owned shape. The
    /// presence of `ref` is the owned/referenced discriminant.
    fn reference(&self) -> Option<&str> {
        match self {
            DiskEntry::Typed(d) => d.reference.as_deref(),
            DiskEntry::Raw(o) => o.get("ref").and_then(|v| v.as_str()),
        }
    }

    /// 2.6c (T10): whether this entry is the REFERENCED disk shape.
    fn is_reference(&self) -> bool {
        self.reference().is_some()
    }

    /// Whether this entry literally carries a `name` field (owned-shape
    /// vocabulary). Used to reject `ref` + `name` mixed shapes — distinct
    /// from `derived_name()`, which falls back to the `mountPath` segment.
    fn has_explicit_name(&self) -> bool {
        match self {
            DiskEntry::Typed(d) => d.name.is_some(),
            DiskEntry::Raw(o) => o.contains_key("name"),
        }
    }
}

/// The disk entries a scope declares, as [`DiskEntry`] views. Typed
/// `OneOrMany<DiskClaim>` on the happy path (every entry is a
/// `DiskEntry::Typed`, compiler-gating the field reads); the raw
/// `disk_entries` is the deserialize-failure fallback.
fn scope_disk_entries<'a>(
    typed_scope: Option<&'a dyn ScopeView>,
    raw_scope: Option<&'a serde_json::Map<String, Value>>,
) -> Vec<DiskEntry<'a>> {
    match typed_scope {
        Some(s) => match s.needs().and_then(|n| n.disk.as_ref()) {
            Some(OneOrMany::One(d)) => vec![DiskEntry::Typed(d)],
            Some(OneOrMany::Many(v)) => v.iter().map(DiskEntry::Typed).collect(),
            None => Vec::new(),
        },
        None => match scope_disk_value(raw_scope) {
            Some(value) => disk_entries(value)
                .into_iter()
                .map(DiskEntry::Raw)
                .collect(),
            None => Vec::new(),
        },
    }
}

/// pg connection-Secret field vocabulary (ADR 0046).
const PG_FIELDS: &[&str] = &["url", "user", "pass", "host", "port", "db"];
/// redis connection-Secret field vocabulary (ADR 0046).
const REDIS_FIELDS: &[&str] = &["url", "user", "pass", "host", "port", "db", "channelPrefix"];
/// jetstream connection-Secret field vocabulary (2.5 / ADR 0061 §6). `pub`
/// (2.5d Task 9) so `operator-controllers-resourceclaim-provisioner`'s own
/// connection-secret-builder test can assert its key SET against this
/// vocabulary directly, as a `[dev-dependencies]`-only cross-crate
/// reference, instead of hand-copying the eight names a fourth time — the
/// round-1 review objection this crate's own `claim_fields_mirror_the_cue_source_of_truth`
/// test already exists to avoid for THIS list's relationship to the CUE
/// source; a sibling test in another crate re-typing the same eight
/// strings would reintroduce exactly that hand-copy, just one crate over.
pub const JETSTREAM_FIELDS: &[&str] = &[
    "url",
    "host",
    "port",
    "user",
    "pass",
    "account",
    "subjectPrefix",
    "inboxPrefix",
];
/// Service types that have a connection Secret at launch (ADR 0046, 2.5 /
/// ADR 0061 §6). `disk` and the still-deferred types (clickhouse, s3,
/// notifications) do NOT have a connection Secret.
const CLAIM_SUPPORTED_TYPES: &[(&str, &[&str])] = &[
    ("pg", PG_FIELDS),
    ("redis", REDIS_FIELDS),
    ("jetstream", JETSTREAM_FIELDS),
];
/// Types that exist in the platform but have no connection Secret — any
/// `claim.<type>.*` ref to them is rejected at the webhook.
const CLAIM_UNSUPPORTED_TYPES: &[&str] = &["disk", "clickhouse", "s3", "notifications"];

/// 2.12 (ADR 0046): compute the effective TYPED `needs` for a given scope.
/// Base scope: just `base.needs`. Per-environment scope: base.needs merged
/// per-key with environments[name].needs (override-wins per key), matching
/// the renderer's `effective_spec` logic. The merge selects whole slots
/// (the env's slot wins when `Some`), so a renamed `Needs` field fails to
/// compile.
fn effective_needs_for_scope(base: Option<&Needs>, env_needs: Option<&Needs>) -> Needs {
    // Per-key override: env's slot wins when present, else inherit base's.
    fn pick<T: Clone>(env: &Option<T>, base: &Option<T>) -> Option<T> {
        env.clone().or_else(|| base.clone())
    }
    let empty = Needs::default();
    let b = base.unwrap_or(&empty);
    let e = env_needs.unwrap_or(&empty);
    Needs {
        pg: pick(&e.pg, &b.pg),
        jetstream: pick(&e.jetstream, &b.jetstream),
        clickhouse: pick(&e.clickhouse, &b.clickhouse),
        redis: pick(&e.redis, &b.redis),
        s3: pick(&e.s3, &b.s3),
        notifications: pick(&e.notifications, &b.notifications),
        disk: pick(&e.disk, &b.disk),
    }
}

/// 2.12 (ADR 0046): validate env claim/secret refs across `base.env` and
/// every `environments[*].env`. For each scope the effective needs are
/// base.needs merged per-key with the scope's own needs (override-wins).
/// Multi-error, one message per bad ref, no short-circuit.
///
/// Each env VALUE is matched against the typed `EnvValue`
/// (`Literal` / `Ref(Claim)` / `Ref(Secret)`) instead of string-key
/// probing the raw object — a renamed `EnvValue`/`EnvRef` variant fails to
/// compile. When a scope did not decode, the raw map is the fallback.
fn validate_env_refs(
    typed_base: Option<&ApplicationBaseSpec>,
    typed_envs: Option<&BTreeMap<String, ApplicationEnvOverride>>,
    base: Option<&serde_json::Map<String, Value>>,
    envs: Option<&serde_json::Map<String, Value>>,
    errors: &mut Vec<ValidationError>,
) {
    let base_needs = typed_base.and_then(|b| b.needs.as_ref());

    // Check one scope's env map. On the happy path `typed_env` is the
    // scope's decoded `env`; the raw map is the fallback. `env_needs` is the
    // scope's OWN typed needs (merged with base by the caller).
    let check_scope = |prefix: &str,
                       typed_env: Option<&BTreeMap<String, EnvValue>>,
                       raw_env: Option<&serde_json::Map<String, Value>>,
                       scope_needs: Option<&Needs>,
                       errors: &mut Vec<ValidationError>| {
        let eff_needs = effective_needs_for_scope(base_needs, scope_needs);
        match typed_env {
            Some(env_map) => {
                for (var_name, val) in env_map {
                    match val {
                        // A plain string → literal; no validation needed.
                        EnvValue::Literal(_) => {}
                        EnvValue::Ref(EnvRef::Claim(claim_path)) => validate_claim_ref(
                            &format!("{prefix}.{var_name}"),
                            claim_path,
                            &eff_needs,
                            errors,
                        ),
                        EnvValue::Ref(EnvRef::Secret(secret_path)) => validate_secret_ref(
                            &format!("{prefix}.{var_name}"),
                            secret_path,
                            errors,
                        ),
                    }
                }
            }
            None => {
                // Deserialize-failure fallback: probe the raw map exactly as
                // before (other shapes are rejected by the CRD layer).
                if let Some(env_map) = raw_env
                    .and_then(|o| o.get("env"))
                    .and_then(|v| v.as_object())
                {
                    for (var_name, val) in env_map {
                        match val {
                            Value::String(_) => {}
                            Value::Object(obj) => {
                                if let Some(claim_path) = obj.get("claim").and_then(|v| v.as_str())
                                {
                                    validate_claim_ref(
                                        &format!("{prefix}.{var_name}"),
                                        claim_path,
                                        &eff_needs,
                                        errors,
                                    );
                                } else if let Some(secret_path) =
                                    obj.get("secret").and_then(|v| v.as_str())
                                {
                                    validate_secret_ref(
                                        &format!("{prefix}.{var_name}"),
                                        secret_path,
                                        errors,
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    };

    // Base scope (its own needs ARE base.needs → no extra merge).
    let base_has_env = typed_base.and_then(|b| b.env.as_ref()).is_some()
        || base
            .and_then(|o| o.get("env"))
            .and_then(|v| v.as_object())
            .is_some();
    if base_has_env {
        check_scope(
            "spec.base.env",
            typed_base.and_then(|b| b.env.as_ref()),
            base,
            None,
            errors,
        );
    }
    if let Some(envs_obj) = envs {
        for (env_name, val) in envs_obj {
            let typed_env_scope = typed_envs.and_then(|m| m.get(env_name));
            let scope_has_env = typed_env_scope.and_then(|e| e.env.as_ref()).is_some()
                || val
                    .as_object()
                    .and_then(|o| o.get("env"))
                    .and_then(|v| v.as_object())
                    .is_some();
            if scope_has_env {
                check_scope(
                    &format!("spec.environments.{env_name}.env"),
                    typed_env_scope.and_then(|e| e.env.as_ref()),
                    val.as_object(),
                    typed_env_scope.and_then(|e| e.needs.as_ref()),
                    errors,
                );
            }
        }
    }
}

/// The declared `(name)` identities for a runtime service-type under a
/// typed `Needs` — `None` when the type is absent / not a service type,
/// else one element per entry (`None` for an unnamed default, `Some` for
/// a named one). `disk` is intentionally not matched — a `claim.disk.*`
/// ref is rejected earlier by `CLAIM_UNSUPPORTED_TYPES`.
///
/// `jetstream` carries its own type (`JetStreamNeed`, ADR 0061 §6), not
/// `OneOrMany<ServiceNeed>`, so it can't share the five slots' generic
/// `&Option<OneOrMany<ServiceNeed>>` shape — hence this function returns
/// the extracted name list rather than the raw slot (the only thing
/// `validate_claim_ref` ever did with the slot). jetstream is
/// scalar-only and has no `(type, name)` identity, so a declared
/// jetstream need always yields exactly one unnamed entry (`vec![None]`)
/// — its own `name` field is NOT a claim identity; it is rejected outright
/// by `validate_jetstream_need` (ADR 0061 §6), so this function never sees
/// a value where treating it as one would matter. A renamed `Needs` slot
/// fails to compile here.
fn declared_need_names(needs: &Needs, service_type: &str) -> Option<Vec<Option<String>>> {
    fn names(slot: &Option<OneOrMany<ServiceNeed>>) -> Option<Vec<Option<String>>> {
        slot.as_ref()
            .map(|s| s.as_slice_vec().into_iter().map(|n| n.name).collect())
    }
    match service_type {
        "pg" => names(&needs.pg),
        "clickhouse" => names(&needs.clickhouse),
        "redis" => names(&needs.redis),
        "s3" => names(&needs.s3),
        "notifications" => names(&needs.notifications),
        "jetstream" => needs.jetstream.as_ref().map(|_| vec![None]),
        _ => None,
    }
}

/// Validate a `claim` ref string (`"<type>.<field>"` or
/// `"<type>.<name>.<field>"`). Reports one error for each violation. The
/// "type declared in needs" + named-entry checks read the TYPED effective
/// `Needs` (compiler-gated slot fields); only the type/field VOCABULARY
/// (`CLAIM_*_TYPES`, ADR 0046) is a webhook-side constant, not a CRD field.
fn validate_claim_ref(
    field_path: &str,
    path: &str,
    eff_needs: &Needs,
    errors: &mut Vec<ValidationError>,
) {
    let parts: Vec<&str> = path.splitn(4, '.').collect();
    let (service_type, name_opt, field) = match parts.as_slice() {
        [t, f] => (*t, None, *f),
        [t, n, f] => (*t, Some(*n), *f),
        _ => {
            errors.push(ValidationError::new(
                field_path,
                format!(
                    "claim ref {path:?} is malformed; expected \"<type>.<field>\" or \"<type>.<name>.<field>\""
                ),
            ));
            return;
        }
    };

    // Check if the type is a known-unsupported type (disk + deferred).
    if CLAIM_UNSUPPORTED_TYPES.contains(&service_type) {
        errors.push(ValidationError::new(
            field_path,
            format!(
                "claim ref {path:?}: type {service_type:?} has no connection Secret (disk is storage-only; clickhouse/s3/notifications are deferred to a future release)"
            ),
        ));
        return;
    }

    // Check if the type is declared in the effective needs for this scope
    // (the typed slot is `Some`).
    let Some(entry_names) = declared_need_names(eff_needs, service_type) else {
        errors.push(ValidationError::new(
            field_path,
            format!(
                "claim ref {path:?}: type {service_type:?} is not declared in needs for this scope; add needs.{service_type} to use a claim ref"
            ),
        ));
        return;
    };

    // Check if the field is in the type's enum.
    let type_fields = CLAIM_SUPPORTED_TYPES
        .iter()
        .find(|(t, _)| *t == service_type)
        .map(|(_, fields)| *fields);

    if let Some(fields) = type_fields {
        if !fields.contains(&field) {
            errors.push(ValidationError::new(
                field_path,
                format!(
                    "claim ref {path:?}: field {field:?} is not valid for {service_type:?}; valid fields are: {}",
                    fields.join(", ")
                ),
            ));
            return;
        }
    }

    // If a name segment is present, validate the named entry exists. The
    // entry names come from `declared_need_names` — `ServiceNeed.name` for the five
    // service types, always empty for jetstream (scalar-only, ADR 0061 §6).
    if let Some(name) = name_opt {
        let named_entries: Vec<String> = entry_names
            .into_iter()
            .flatten()
            .filter(|n| !n.is_empty())
            .collect();
        if !named_entries.iter().any(|n| n == name) {
            // The need is declared but has no entry by this name.
            // Check if it's a scalar (no named entries) vs an array lacking the name.
            let has_any_named = !named_entries.is_empty();
            if has_any_named {
                errors.push(ValidationError::new(
                    field_path,
                    format!(
                        "claim ref {path:?}: no entry named {name:?} in needs.{service_type}; declared names are: {}",
                        named_entries.join(", ")
                    ),
                ));
            } else if service_type == "jetstream" {
                // jetstream is scalar-only (ADR 0061 §6) — it can NEVER
                // gain a named entry, so "add a named entry" (the advice
                // below, correct for the other service types) is
                // impossible advice here.
                errors.push(ValidationError::new(
                    field_path,
                    format!(
                        "claim ref {path:?}: named ref (name={name:?}) used but jetstream has no named entries (it is scalar-only, ADR 0061 §6); omit the name segment"
                    ),
                ));
            } else {
                errors.push(ValidationError::new(
                    field_path,
                    format!(
                        "claim ref {path:?}: named ref (name={name:?}) used but needs.{service_type} is a scalar (unnamed default); omit the name segment or add a named entry"
                    ),
                ));
            }
        }
    }
}

/// Validate a `secret` ref string (`"<name>/<key>"`). Reports one error
/// for each violation.
fn validate_secret_ref(field_path: &str, path: &str, errors: &mut Vec<ValidationError>) {
    let Some(slash_pos) = path.find('/') else {
        errors.push(ValidationError::new(
            field_path,
            format!(
                "secret ref {path:?} is malformed; expected \"<name>/<key>\" (a DNS-1123 Secret name, a '/', then a key matching [-._a-zA-Z0-9]+)"
            ),
        ));
        return;
    };
    let (name, rest) = path.split_at(slash_pos);
    let key = &rest[1..]; // skip leading '/'

    if name.is_empty() {
        errors.push(ValidationError::new(
            field_path,
            format!("secret ref {path:?}: Secret name (before '/') must not be empty"),
        ));
    } else if !is_dns_1123_label(name) {
        errors.push(ValidationError::new(
            field_path,
            format!(
                "secret ref {path:?}: Secret name {name:?} must be a DNS-1123 label (lowercase alphanumeric + '-', start and end alphanumeric, 1..=63 chars)"
            ),
        ));
    }

    if key.is_empty() {
        errors.push(ValidationError::new(
            field_path,
            format!("secret ref {path:?}: key (after '/') must not be empty"),
        ));
    } else if !is_secret_key(key) {
        errors.push(ValidationError::new(
            field_path,
            format!("secret ref {path:?}: key {key:?} must match [-._a-zA-Z0-9]+"),
        ));
    }
}

/// Kubernetes Secret key character set: `[-._a-zA-Z0-9]+` (ADR 0046).
fn is_secret_key(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
}

/// 2.16b (S7.2): whether an UPDATE may carry the given `spec.environment`
/// transition. `spec.environment` selects which `environments.<env>`
/// override the operator unifies onto `base` (ADR 0044) — flipping it
/// (dev→prod) swaps the ENTIRE effective spec at once (image/replicas/
/// expose/env/needs, override-wins), a gate-laundering vector that would
/// never be audited as the destructive change it is. Changing environment
/// is a DIFFERENT deployment (each `<name>-<env>` is its own Argo
/// Application / Application CR), never an edit to an existing CR — so
/// `spec.environment` is IMMUTABLE once concretely set. Mirrors the
/// RetainedClaim spec-immutability precedent.
///
/// The rule (empty string is treated as UNSET, matching the codebase
/// empty-is-absent convention):
///   - `old` unset (CREATE — no oldObject — or an existing CR that never
///     set an environment): ALLOW. This lets a per-env CR be created (no
///     oldObject) and lets an existing default-env CR set its environment
///     for the first time. `environment_update_allowed(None, _) == true`.
///   - `old` concretely set:
///       - `new` equal → ALLOW (an unrelated spec edit / metadata tweak);
///       - `new` a different concrete env → REJECT (dev→prod laundering);
///       - `new` cleared (None/empty) → REJECT (dropping a set env still
///         swaps the effective spec back to base-only — a change).
///
/// Safe for per-env deploys: each `<name>-<env>` CR is CREATEd exactly once
/// (old is `None` → allowed); the rule only blocks CHANGING a concrete env
/// on a CR that already has one, which is the laundering path.
pub fn environment_update_allowed(old_env: Option<&str>, new_env: Option<&str>) -> bool {
    // Empty string ≡ unset (codebase convention).
    let old = old_env.filter(|s| !s.is_empty());
    let new = new_env.filter(|s| !s.is_empty());
    match old {
        // Old unset (CREATE, or first-set on an existing CR) → always allowed.
        None => true,
        // Old concretely set → the new value must equal it exactly. A
        // different concrete env, or clearing it, is a rejected change.
        Some(o) => new == Some(o),
    }
}

fn validate_env_keys(
    path: &str,
    env: &serde_json::Map<String, Value>,
    errors: &mut Vec<ValidationError>,
) {
    for key in env.keys() {
        if !is_env_var_name(key) {
            errors.push(ValidationError::new(
                format!("{path}.{key}"),
                format!("env key {key:?} must match ^[A-Z_][A-Z0-9_]*$"),
            ));
        }
    }
}

/// Built-in `#PlatformServiceType` values. The webhook enforces the
/// `needs` key enum because the structural OpenAPI v3 CRD accepts
/// any `additionalProperties` key. Keep in sync with
/// `schemas/v1alpha1/types.cue` (`#PlatformServiceType`) and the
/// `type` enum in BOTH the ResourceClaim and ServiceProvider CRDs
/// (`crd-resourceclaim.yaml`, `crd-serviceprovider.yaml`) — there is
/// no CUE->Rust/CRD generator yet, so adding a service type means
/// editing all four sites.
const PLATFORM_SERVICE_TYPES: [&str; 7] = [
    "pg",
    "jetstream",
    "clickhouse",
    "redis",
    "s3",
    "notifications",
    // 2.6b (ADR 0043): persistent block storage. A `needs.disk` entry
    // generates a `type: disk` ResourceClaim; the disk-specific value
    // guards (mountPath/size/class/replicas) land in 2.6b-4.
    "disk",
];

fn is_platform_service_type(s: &str) -> bool {
    PLATFORM_SERVICE_TYPES.contains(&s)
}

fn validate_needs_keys(
    path: &str,
    needs: &serde_json::Map<String, Value>,
    errors: &mut Vec<ValidationError>,
) {
    for key in needs.keys() {
        if !is_platform_service_type(key) {
            errors.push(ValidationError::new(
                format!("{path}.{key}"),
                format!(
                    "needs key {key:?} is not a known platform-service type ({})",
                    PLATFORM_SERVICE_TYPES.join(", ")
                ),
            ));
        }
    }
}

fn is_dns_1123_label(s: &str) -> bool {
    if s.is_empty() || s.len() > 63 {
        return false;
    }
    let bytes = s.as_bytes();
    let endpoint_ok = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if !endpoint_ok(bytes[0]) || !endpoint_ok(bytes[bytes.len() - 1]) {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A DNS-1123 SUBDOMAIN (an FQDN host like `app.demo.dev`): one or more
/// DNS-1123 labels joined by '.'. Rejects wildcards (`*` is not in the label
/// alphabet), empty labels (leading/trailing/double dots), and >253 chars.
fn is_dns_1123_subdomain(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    s.split('.').all(is_dns_1123_label)
}

fn is_env_var_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_uppercase() || first == b'_') {
        return false;
    }
    bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── 2.28 JetStream tuning + DLQ (ADR 0065 §2) ─────────────────────

    /// Build a spec with one jetstream stream carrying `extra` JSON fields.
    fn js_stream_spec(extra: serde_json::Value) -> Value {
        let mut stream = json!({"name": "orders", "subjects": ["app.orders.>"], "maxBytes": "1Gi"});
        for (k, v) in extra.as_object().unwrap() {
            stream[k] = v.clone();
        }
        json!({"base": {"image": "img", "expose": {"port": 8080},
            "needs": {"jetstream": {"streams": [stream]}}}})
    }

    /// Build a spec with one consume entry carrying `extra` JSON fields.
    fn js_consume_spec(extra: serde_json::Value) -> Value {
        let mut c = json!({"stream": "orders", "durable": "reader"});
        for (k, v) in extra.as_object().unwrap() {
            c[k] = v.clone();
        }
        json!({"base": {"image": "img", "expose": {"port": 8080},
            "needs": {"jetstream": {
                "streams": [{"name": "orders", "subjects": ["app.orders.>"], "maxBytes": "1Gi"}],
                "consume": [c]}}}})
    }

    fn msgs(errs: &[ValidationError]) -> String {
        errs.iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn every_isolation_breaking_stream_field_is_rejected_rather_than_pruned() {
        // A structural schema prunes an unknown field BEFORE the webhook
        // runs, so these are declared in the type precisely so that this
        // rejection is reachable. If the type ever drops one, it starts
        // vanishing silently and the manifest appears to work.
        for field in [
            "sources",
            "mirror",
            "republish",
            "subjectTransform",
            "placement",
        ] {
            let payload = if field == "sources" {
                json!({field: [{"name": "other"}]})
            } else {
                json!({field: {"name": "other"}})
            };
            let errs = validate_application_spec(&js_stream_spec(payload));
            assert!(
                msgs(&errs).contains(field),
                "{field} was accepted: {errs:?}"
            );
        }
        let errs = validate_application_spec(&js_stream_spec(json!({"replicas": 3})));
        assert!(msgs(&errs).contains("replicas"), "{errs:?}");
    }

    #[test]
    fn the_whole_push_surface_is_rejected_on_a_consumer() {
        // Push delivery is performed by the server, outside the
        // application's publish permissions — a write channel into a
        // neighbour's prefix.
        for (field, payload) in [
            ("deliverSubject", json!({"deliverSubject": "victim.inbox"})),
            ("deliverGroup", json!({"deliverGroup": "g"})),
            ("flowControl", json!({"flowControl": true})),
            ("heartbeatInterval", json!({"heartbeatInterval": "5s"})),
            ("replicas", json!({"replicas": 3})),
        ] {
            let errs = validate_application_spec(&js_consume_spec(payload));
            assert!(
                msgs(&errs).contains(field),
                "{field} was accepted: {errs:?}"
            );
        }
    }

    #[test]
    fn discard_per_subject_needs_discard_new_and_a_per_subject_ceiling() {
        // Both halves are server rules (10052), measured on 2.14.3. Without
        // them NACK surfaces a server error with no field name attached.
        let errs = validate_application_spec(&js_stream_spec(
            json!({"discardPerSubject": true, "discard": "old", "maxMsgsPerSubject": 100}),
        ));
        assert!(msgs(&errs).contains("discard"), "{errs:?}");
        let errs = validate_application_spec(&js_stream_spec(
            json!({"discardPerSubject": true, "discard": "new"}),
        ));
        assert!(msgs(&errs).contains("maxMsgsPerSubject"), "{errs:?}");
        // Both satisfied: accepted.
        let errs = validate_application_spec(&js_stream_spec(
            json!({"discardPerSubject": true, "discard": "new", "maxMsgsPerSubject": 100}),
        ));
        assert!(
            !msgs(&errs).contains("discardPerSubject"),
            "a valid combination was rejected: {errs:?}"
        );
    }

    #[test]
    fn the_two_filter_forms_are_mutually_exclusive() {
        let errs = validate_application_spec(&js_consume_spec(
            json!({"filterSubject": "a.b", "filterSubjects": ["a.c"]}),
        ));
        assert!(msgs(&errs).contains("filterSubject"), "{errs:?}");
    }

    #[test]
    fn a_start_position_requires_its_own_deliver_policy() {
        // Rejected rather than ignored: an ignored start position is a
        // consumer that silently reads from the wrong place.
        let errs = validate_application_spec(&js_consume_spec(json!({"optStartSeq": 42})));
        assert!(msgs(&errs).contains("optStartSeq"), "{errs:?}");
        let errs = validate_application_spec(&js_consume_spec(
            json!({"optStartSeq": 42, "deliverPolicy": "byStartSequence"}),
        ));
        assert!(!msgs(&errs).contains("optStartSeq"), "{errs:?}");
        let errs = validate_application_spec(&js_consume_spec(
            json!({"optStartTime": "2026-01-01T00:00:00Z", "deliverPolicy": "byStartSequence"}),
        ));
        assert!(msgs(&errs).contains("optStartTime"), "{errs:?}");
    }

    #[test]
    fn max_deliver_must_exceed_the_backoff_length_strictly() {
        // Measured: `max deliver is required to be > length of backoff
        // values` (10116). The equal case is the one a loose reading of the
        // rule would have allowed.
        let errs = validate_application_spec(&js_consume_spec(
            json!({"maxDeliver": 2, "backoff": ["1s", "2s"]}),
        ));
        assert!(
            msgs(&errs).contains("backoff"),
            "equal was accepted: {errs:?}"
        );
        let errs = validate_application_spec(&js_consume_spec(
            json!({"maxDeliver": 3, "backoff": ["1s", "2s"]}),
        ));
        assert!(!msgs(&errs).contains("backoff"), "{errs:?}");
    }

    #[test]
    fn a_dead_letter_without_a_delivery_ceiling_is_rejected() {
        // Without maxDeliver the advisory never fires and the DLQ is
        // permanently empty — a manifest that looks like it works and says
        // nothing.
        let errs = validate_application_spec(&js_consume_spec(
            json!({"deadLetter": {"stream": "dlq", "maxBytes": "64Mi"}}),
        ));
        assert!(msgs(&errs).contains("maxDeliver"), "{errs:?}");
        let errs = validate_application_spec(&js_consume_spec(
            json!({"maxDeliver": 5, "deadLetter": {"stream": "dlq", "maxBytes": "64Mi"}}),
        ));
        assert!(
            !msgs(&errs).contains("deadLetter"),
            "a valid DLQ was rejected: {errs:?}"
        );
    }

    #[test]
    fn a_dead_letter_stream_name_shares_the_declared_stream_namespace() {
        // It composes through the same nats_stream_name(app, name), so it
        // must be a DNS-1123 label and must not collide with a declared
        // stream or another DLQ.
        let errs = validate_application_spec(&js_consume_spec(
            json!({"maxDeliver": 5, "deadLetter": {"stream": "orders", "maxBytes": "64Mi"}}),
        ));
        assert!(
            msgs(&errs).contains("orders"),
            "a DLQ colliding with a declared stream was accepted: {errs:?}"
        );
        let errs = validate_application_spec(&js_consume_spec(
            json!({"maxDeliver": 5, "deadLetter": {"stream": "bad_name", "maxBytes": "64Mi"}}),
        ));
        assert!(msgs(&errs).contains("DNS-1123"), "{errs:?}");
        let errs = validate_application_spec(&js_consume_spec(
            json!({"maxDeliver": 5, "deadLetter": {"stream": "dlq", "maxBytes": ""}}),
        ));
        assert!(msgs(&errs).contains("maxBytes"), "{errs:?}");
    }

    #[test]
    fn a_durable_may_not_collide_with_a_dead_letter_stream_declared_later() {
        // The durable-vs-stream collision check is what makes the deny
        // vector's position patterns sound. A DLQ declared in a LATER
        // consume entry is still a stream of this application, so the check
        // must see it — which means collecting DLQ names before the durable
        // loop runs, not during it.
        let spec = json!({"base": {"image": "img", "expose": {"port": 8080},
        "needs": {"jetstream": {
            "streams": [{"name": "orders", "subjects": ["app.orders.>"], "maxBytes": "1Gi"}],
            "consume": [
                {"stream": "orders", "durable": "dlq"},
                {"stream": "orders", "durable": "other", "maxDeliver": 5,
                 "deadLetter": {"stream": "dlq", "maxBytes": "64Mi"}}
            ]}}}});
        let errs = validate_application_spec(&spec);
        assert!(
            msgs(&errs).contains("dlq"),
            "the durable/DLQ collision was missed: {errs:?}"
        );
    }

    // ── 2.28 probes (ADR 0065 §1.5) ───────────────────────────────────

    #[test]
    fn a_probe_path_must_start_with_a_slash() {
        let spec = json!({"base": {
            "image": "img", "expose": {"port": 8080},
            "probes": {"readiness": {"path": "healthz"}}
        }});
        let errs = validate_application_spec(&spec);
        assert!(
            errs.iter()
                .any(|e| e.field == "spec.base.probes.readiness.path"),
            "{errs:?}"
        );
    }

    #[test]
    fn scheme_and_headers_are_rejected_without_a_path() {
        // A TCP connect has no scheme and no headers. Ignoring them silently
        // would hide a typo'd `path` — the user wrote an HTTP probe and got a
        // TCP one.
        let spec = json!({"base": {
            "image": "img", "expose": {"port": 8080},
            "probes": {"readiness": {"scheme": "https", "headers": {"X": "y"}}}
        }});
        let errs = validate_application_spec(&spec);
        assert!(
            errs.iter()
                .any(|e| e.field == "spec.base.probes.readiness.scheme"),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.field == "spec.base.probes.readiness.headers"),
            "{errs:?}"
        );
    }

    #[test]
    fn a_probe_with_no_port_and_no_expose_is_rejected_naming_the_probe() {
        let spec = json!({"base": {
            "image": "img",
            "probes": {"liveness": {"path": "/livez"}}
        }});
        let errs = validate_application_spec(&spec);
        let e = errs
            .iter()
            .find(|e| e.field == "spec.base.probes.liveness.port")
            .unwrap_or_else(|| panic!("{errs:?}"));
        assert!(e.message.contains("expose.port"), "{}", e.message);
    }

    #[test]
    fn a_probe_with_its_own_port_needs_no_expose() {
        let spec = json!({"base": {
            "image": "img",
            "probes": {"liveness": {"path": "/livez", "port": 9000}}
        }});
        assert!(
            validate_application_spec(&spec)
                .iter()
                .all(|e| !e.field.contains("probes")),
            "a worker with no Service can still probe itself"
        );
    }

    #[test]
    fn liveness_and_startup_reject_a_success_threshold_other_than_one() {
        for probe in ["liveness", "startup"] {
            let spec = json!({"base": {
                "image": "img", "expose": {"port": 8080},
                "probes": {probe: {"path": "/x", "successThreshold": 2}}
            }});
            let errs = validate_application_spec(&spec);
            assert!(
                errs.iter()
                    .any(|e| e.field == format!("spec.base.probes.{probe}.successThreshold")),
                "{probe}: {errs:?}"
            );
        }
        // Readiness legitimately allows it, so the rule must be probe-specific
        // rather than a blanket one.
        let spec = json!({"base": {
            "image": "img", "expose": {"port": 8080},
            "probes": {"readiness": {"path": "/x", "successThreshold": 2}}
        }});
        assert!(validate_application_spec(&spec)
            .iter()
            .all(|e| !e.field.contains("successThreshold")));
    }

    #[test]
    fn a_timeout_at_or_above_the_period_is_rejected() {
        // Kubernetes permits the overlap; we do not. Equal counts: at
        // timeout == period the next attempt starts as the previous one
        // gives up, which is the same pathology one second later.
        let spec = json!({"base": {
            "image": "img", "expose": {"port": 8080},
            "probes": {"readiness": {"path": "/x", "periodSeconds": 5, "timeoutSeconds": 5}}
        }});
        let errs = validate_application_spec(&spec);
        assert!(
            errs.iter()
                .any(|e| e.field == "spec.base.probes.readiness.timeoutSeconds"),
            "{errs:?}"
        );
    }

    #[test]
    fn a_disabled_probe_that_still_declares_its_target_is_accepted() {
        // Keeping the declaration in git while taking the probe off the pod is
        // the reason `enabled` exists — it is not a contradiction.
        let spec = json!({"base": {
            "image": "img", "expose": {"port": 8080},
            "probes": {"readiness": {"enabled": false, "path": "/healthz"}}
        }});
        assert!(validate_application_spec(&spec)
            .iter()
            .all(|e| !e.field.contains("probes")));
    }

    #[test]
    fn an_env_scope_probe_is_checked_against_that_scopes_effective_port() {
        // `prod` supplies its own port, so its probe resolves; `dev` inherits
        // nothing because base has no expose, so its probe does not. Checking
        // against the MERGED port per scope is the whole rule — a single
        // whole-object check would pass both or fail both.
        let spec = json!({"base": {"image": "img"}, "environments": {
            "prod": {"expose": {"port": 9000}, "probes": {"readiness": {"path": "/x"}}},
            "dev":  {"probes": {"readiness": {"path": "/x"}}}
        }});
        let errs = validate_application_spec(&spec);
        assert!(
            errs.iter()
                .any(|e| e.field == "spec.environments.dev.probes.readiness.port"),
            "{errs:?}"
        );
        assert!(
            !errs
                .iter()
                .any(|e| e.field.starts_with("spec.environments.prod.probes")),
            "{errs:?}"
        );
    }

    #[test]
    fn an_env_probe_inherits_the_base_port() {
        // base declares the port, the env only tunes a number: the env's probe
        // resolves through inheritance and must not be rejected.
        let spec = json!({"base": {"image": "img", "expose": {"port": 8080}},
            "environments": {"dev": {"probes": {"readiness": {"path": "/x", "periodSeconds": 3}}}}});
        assert!(
            validate_application_spec(&spec)
                .iter()
                .all(|e| !e.field.contains("probes")),
            "{:?}",
            validate_application_spec(&spec)
        );
    }

    /// Reads `#ClaimFieldsFor` out of `schemas/v1alpha1/application.cue`
    /// and parses each `type: ["field", ...]` line into `(type, fields)`
    /// pairs. `application.cue` lives in a different cargo workspace
    /// (`schemas/` is not under `operator/`), so this cannot be a
    /// compile-time dependency — but it is the same repository, reached
    /// via `CARGO_MANIFEST_DIR` the same way
    /// `platform.rs::every_condition_type_the_operator_writes_is_classified`
    /// reaches across into `operator/`. `std::fs::read_to_string`, not
    /// `include_str!`: the webhook's container image builds with `context:
    /// operator`, so `../../schemas/…` is outside that Docker build
    /// context — a `#[cfg(test)]`-only `fs::read` never runs during the
    /// image build, whereas relying on `include_str!` being cfg-stripped
    /// away is a subtler thing to depend on.
    fn declared_claim_fields() -> Vec<(String, Vec<String>)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/v1alpha1/application.cue");
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "{}: the CUE claim field vocabulary could not be read, so this \
                 test would have judged nothing: {e}",
                path.display()
            )
        });
        let Some((_, block)) = src.split_once("#ClaimFieldsFor: {") else {
            panic!(
                "{}: no `#ClaimFieldsFor: {{` block found — the definition was \
                 renamed or reshaped, and this test would otherwise have \
                 compared against nothing",
                path.display()
            );
        };
        let mut out = Vec::new();
        for line in block.lines() {
            let line = line.trim();
            if line == "}" {
                break;
            }
            let Some((ty, rest)) = line.split_once(':') else {
                continue;
            };
            let Some(inner) = rest
                .trim()
                .strip_prefix('[')
                .and_then(|r| r.strip_suffix(']'))
            else {
                continue;
            };
            let fields: Vec<String> = inner
                .split(',')
                .map(|s| s.trim().trim_matches('"').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            out.push((ty.trim().to_string(), fields));
        }
        out
    }

    #[test]
    fn claim_fields_mirror_the_cue_source_of_truth() {
        // #ClaimFieldsFor in schemas/v1alpha1/application.cue calls itself
        // the single source of truth for the claim field vocabulary, but
        // PG_FIELDS/REDIS_FIELDS/JETSTREAM_FIELDS here are hand copies —
        // a Rust crate can't `cue export` at compile time, and evaluating
        // CUE at runtime in the webhook's validation hot path was
        // rejected as unnecessary weight for a fixed, rarely-changing
        // list. This test is what keeps the copies honest: it re-derives
        // the expected table from the CUE source and fails the moment a
        // field is added, removed, renamed, or a type is missing on
        // either side — completeness and correctness, DERIVED, not
        // hand-verified (the same shape as
        // `platform.rs::every_condition_type_the_operator_writes_is_classified`).
        let declared = declared_claim_fields();
        // Non-vacuity: a moved file, a renamed `#ClaimFieldsFor`, or a
        // reformatted block would otherwise leave `declared` empty and
        // this test comparing {} == {}, reporting success while checking
        // nothing.
        assert!(
            declared.len() >= 3,
            "only {declared:?} parsed out of schemas/v1alpha1/application.cue's \
             #ClaimFieldsFor — the block's shape changed, and an empty parse \
             would have passed vacuously"
        );
        let declared: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            declared
                .into_iter()
                .map(|(ty, fields)| (ty, fields.into_iter().collect()))
                .collect();
        let mirrored: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            CLAIM_SUPPORTED_TYPES
                .iter()
                .map(|(ty, fields)| {
                    (
                        ty.to_string(),
                        fields.iter().map(|f| f.to_string()).collect(),
                    )
                })
                .collect();
        assert_eq!(
            declared, mirrored,
            "PG_FIELDS/REDIS_FIELDS/JETSTREAM_FIELDS (validator.rs, gathered into \
             CLAIM_SUPPORTED_TYPES) drifted from #ClaimFieldsFor \
             (schemas/v1alpha1/application.cue) — keep the two in sync by hand \
             whenever either changes"
        );
    }

    // ── 2.16b-sec F-1: Application.status is operator-owned (userInfo) ────
    #[test]
    fn application_status_write_only_from_operator_user() {
        let sa = DEFAULT_OPERATOR_SA;
        let operator = json!({ "username": sa, "groups": ["system:serviceaccounts"] });
        let external = json!({ "username": "alice", "groups": ["system:authenticated"] });
        let admin = json!({ "username": "kubernetes-admin", "groups": ["system:masters"] });
        // F-1: the operator's AUTHENTICATED identity passes; a fieldManager
        // string is irrelevant to the gate now.
        assert!(application_status_write_allowed(&operator, sa));
        // A cluster-admin break-glass passes.
        assert!(application_status_write_allowed(&admin, sa));
        // A non-operator user is rejected — even if it were to set
        // `--field-manager=apprafter-operator`, that string is not consulted.
        assert!(!application_status_write_allowed(&external, sa));
        // Empty userInfo fails closed.
        assert!(!application_status_write_allowed(&json!({}), sa));
    }

    #[test]
    fn is_operator_reads_authenticated_username() {
        let sa = DEFAULT_OPERATOR_SA;
        assert!(is_operator(&json!({ "username": sa }), sa));
        assert!(!is_operator(&json!({ "username": "alice" }), sa));
        assert!(!is_operator(&json!({}), sa));
    }

    #[test]
    fn rejects_non_object_spec() {
        let errors = validate_application_spec(&json!("not-an-object"));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec");
    }

    #[test]
    fn accepts_minimal_base_only_manifest() {
        let spec = json!({
            "base": { "image": "ghcr.io/acme/web:1.0" }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_missing_base_image_when_no_environments() {
        let spec = json!({ "base": {} });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.image");
    }

    #[test]
    fn rejects_empty_environments_with_no_base_image() {
        let spec = json!({ "environments": {} });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("nothing to deploy"));
    }

    #[test]
    fn accepts_image_only_set_via_environment_overrides() {
        let spec = json!({
            "environments": {
                "dev":  { "image": "ghcr.io/acme/web:dev" },
                "prod": { "image": "ghcr.io/acme/web:prod" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_environments_missing_image_when_base_has_none() {
        let spec = json!({
            "environments": {
                "dev":  { "image": "ghcr.io/acme/web:dev" },
                "prod": {}
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.prod.image");
    }

    #[test]
    fn accepts_dns_1123_environment_names() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": {
                "dev": {},
                "prod-eu": {},
                "qa-1": {}
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_uppercase_environment_name() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "Prod": {} }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.Prod");
    }

    #[test]
    fn rejects_environment_name_with_underscore() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "prod_us": {} }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn rejects_environment_name_starting_with_hyphen() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "-dev": {} }
        });
        assert_eq!(validate_application_spec(&spec).len(), 1);
    }

    #[test]
    fn rejects_environment_name_over_63_chars() {
        let long = "a".repeat(64);
        let spec = json!({
            "base": { "image": "x" },
            "environments": { long.clone(): {} }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, format!("spec.environments.{long}"));
    }

    #[test]
    fn accepts_uppercase_underscore_env_keys() {
        let spec = json!({
            "base": {
                "image": "x",
                "env": { "LOG_LEVEL": "info", "_PRIVATE": "ok", "RETRIES_3": "5" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_lowercase_or_digit_starting_env_keys() {
        let spec = json!({
            "base": {
                "image": "x",
                "env": { "log_level": "info", "1RETRY": "5", "OK_KEY": "fine" }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 2);
        let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"spec.base.env.log_level"));
        assert!(fields.contains(&"spec.base.env.1RETRY"));
    }

    #[test]
    fn validates_env_keys_under_environment_overrides_too() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": {
                "dev": { "env": { "BAD-KEY": "v" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.dev.env.BAD-KEY");
    }

    #[test]
    fn accepts_known_needs_keys_in_base_and_environments() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": { "selector": { "tier": "integrated" } } }
            },
            "environments": {
                "prod": { "needs": { "redis": {} } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_disk_needs_key() {
        // 2.6b (ADR 0043): `disk` is a known platform-service type, so a
        // `needs.disk` entry must be accepted (the disk value shape is
        // validated by the disk-specific guards in 2.6b-4).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "size": "1Gi", "mountPath": "/data" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_unknown_needs_key_in_base() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "mysql": { "selector": { "tier": "integrated" } } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.mysql");
    }

    #[test]
    fn rejects_unknown_needs_key_under_environment_override() {
        let spec = json!({
            "base": { "image": "ghcr.io/acme/web:1.0" },
            "environments": {
                "prod": { "needs": { "elasticsearch": {} } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].field,
            "spec.environments.prod.needs.elasticsearch"
        );
    }

    #[test]
    fn reports_every_unknown_needs_key_not_just_the_first() {
        // The validator does not short-circuit — two bad keys in one
        // `needs` map must surface two errors (mirrors the env-key
        // multi-error guarantee).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "mysql": {}, "mongo": {} }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 2);
        assert!(errors
            .iter()
            .all(|e| e.field.starts_with("spec.base.needs.")));
    }

    // ---- 2.12 (ADR 0046): 2.4e collision guard REMOVED; literal DATABASE_URL is now valid ----

    #[test]
    fn accepts_database_url_literal_under_needs_pg_2_12() {
        // 2.12: the 2.4e collision guard is removed. A literal DATABASE_URL
        // under needs.pg is now valid — the user owns every env-var name.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DATABASE_URL": "postgres://override" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_database_url_literal_in_env_scope_under_needs_pg_2_12() {
        // 2.12: the cross-scope collision guard is removed. A literal
        // DATABASE_URL in an environment scope under base.needs.pg is valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} }
            },
            "environments": {
                "prod": { "env": { "DATABASE_URL": "postgres://override" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_needs_pg_without_database_url_literal() {
        // 2.12: literals are unconstrained; a LOG_LEVEL literal alongside
        // needs.pg is accepted (it was before too, but tested explicitly).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "LOG_LEVEL": "info" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_database_url_literal_when_no_needs_pg() {
        // Without needs.pg a literal DATABASE_URL is a normal env var
        // (unchanged behavior from before 2.4e was ever introduced).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "DATABASE_URL": "postgres://my-own-db" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_redis_reserved_env_literal_when_no_needs_redis() {
        // Without needs.redis a literal REDIS_URL/REDIS_CHANNEL_PREFIX is
        // a normal env var (unchanged behavior).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "REDIS_URL": "redis://my-own", "REDIS_CHANNEL_PREFIX": "p:" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_redis_url_literal_under_needs_redis_2_12() {
        // 2.12: the 2.6 redis collision guard is removed. A literal REDIS_URL
        // under needs.redis is now valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": {} },
                "env": { "REDIS_URL": "redis://x", "REDIS_CHANNEL_PREFIX": "p:" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    // ---- 2.6b-2: (type, name) uniqueness + foldability (collision guard removed) ----

    #[test]
    fn accepts_named_pg_array_with_distinct_names() {
        // An array of named pg entries with distinct, env-foldable names
        // is valid (each yields a distinct DATABASE_URL_<NAME>).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "primary" }, { "name": "analytics" }] }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_one_unnamed_default_plus_named_siblings() {
        // At most one unnamed default per type is allowed; an unnamed
        // default coexisting with named siblings is valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{}, { "name": "analytics" }] }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_duplicate_name_within_a_type() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "a" }, { "name": "a" }] }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("duplicate"));
    }

    #[test]
    fn rejects_more_than_one_unnamed_entry_in_one_type() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{}, {}] }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("unnamed"));
    }

    #[test]
    fn rejects_non_foldable_name_with_underscore() {
        // `name` must be a DNS-1123 label so the fold yields a valid
        // [A-Z_][A-Z0-9_]* env suffix; an underscore is not allowed.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "read_replica" }] }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn rejects_non_foldable_name_uppercase() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "Analytics" }] }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn accepts_named_pg_reserved_suffix_as_literal_2_12() {
        // 2.12: the named-suffix collision guard is removed. A literal
        // DATABASE_URL_ANALYTICS under needs.pg[name=analytics] is valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "analytics" }] },
                "env": { "DATABASE_URL_ANALYTICS": "postgres://override" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_named_redis_reserved_suffix_as_literal_2_12() {
        // 2.12: the named-suffix collision guard is removed. Literal
        // REDIS_URL_CACHE / REDIS_CHANNEL_PREFIX_CACHE are valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": [{ "name": "cache" }] },
                "env": {
                    "REDIS_URL_CACHE": "redis://x",
                    "REDIS_CHANNEL_PREFIX_CACHE": "p:"
                }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn collects_names_across_base_and_environments_for_duplicate_check() {
        // A name declared once in base and once in an environment for the
        // SAME type is the SAME claim identity in different scopes and is
        // not a duplicate within either scope's array — but two entries
        // with the same name in a single array is a duplicate. This test
        // pins the per-(scope,type) array duplicate semantics.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "a" }, { "name": "b" }] }
            },
            "environments": {
                "prod": { "needs": { "pg": [{ "name": "a" }, { "name": "a" }] } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.prod.needs.pg");
        assert!(errors[0].message.contains("duplicate"));
    }

    #[test]
    fn scalar_named_entry_validates_its_name() {
        // The scalar form may also carry a name; a non-foldable scalar
        // name is rejected just like an array entry.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": { "name": "BAD_NAME" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.pg");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    // ---- 2.6b-4: disk value guards (name/mountPath/size/class/replicas) ----

    #[test]
    fn accepts_valid_single_replica_local_disk() {
        // The happy path: a single-replica app with a well-formed
        // `needs.disk` (valid quantity, absolute mountPath, class local,
        // mountPath-derived name) is accepted.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1,
                "needs": { "disk": { "size": "1Gi", "mountPath": "/var/lib/uploads" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_disk_with_explicit_name_and_array_form() {
        // An array of two disks with distinct explicit names and distinct
        // absolute mountPaths is valid.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": {
                    "disk": [
                        { "name": "data", "size": "1Gi", "mountPath": "/data" },
                        { "name": "cache", "size": "500Mi", "mountPath": "/cache" }
                    ]
                }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_duplicate_disk_mount_path() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": {
                    "disk": [
                        { "name": "a", "size": "1Gi", "mountPath": "/data" },
                        { "name": "b", "size": "1Gi", "mountPath": "/data" }
                    ]
                }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("mountPath"));
        assert!(errors[0].message.contains("/data"));
    }

    #[test]
    fn rejects_duplicate_derived_disk_name() {
        // Two disks whose derived names collide (same last mountPath
        // segment) are rejected — the name becomes part of the PVC name.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": {
                    "disk": [
                        { "size": "1Gi", "mountPath": "/var/lib/data" },
                        { "size": "1Gi", "mountPath": "/srv/data" }
                    ]
                }
            }
        });
        let errors = validate_application_spec(&spec);
        // mountPaths are distinct, but both derive name "data".
        let name_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("duplicate") && e.message.contains("name"))
            .collect();
        assert_eq!(name_errs.len(), 1);
        assert_eq!(name_errs[0].field, "spec.base.needs.disk");
        assert!(name_errs[0].message.contains("data"));
    }

    #[test]
    fn rejects_disk_name_not_dns_1123() {
        // An explicit disk name that is not a DNS-1123 label is rejected
        // (it becomes part of the PVC name).
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "Bad_Name", "size": "1Gi", "mountPath": "/data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn rejects_disk_derived_name_not_dns_1123() {
        // The mountPath-derived name must also be a valid DNS-1123 label;
        // a last segment that is not (uppercase) is rejected.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "size": "1Gi", "mountPath": "/var/Uploads" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn rejects_relative_disk_mount_path() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("absolute"));
    }

    #[test]
    fn rejects_disk_size_not_a_quantity() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "notaquantity", "mountPath": "/data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("quantity"));
    }

    #[test]
    fn accepts_disk_decimal_and_plain_quantities() {
        // `1.5Gi`, a plain `1000000`, and lower-k `512k` are valid
        // Kubernetes quantities.
        for size in ["1.5Gi", "1000000", "512k", "10G", "256Mi"] {
            let spec = json!({
                "base": {
                    "image": "ghcr.io/acme/web:1.0",
                    "needs": { "disk": { "name": "data", "size": size, "mountPath": "/data" } }
                }
            });
            assert!(
                validate_application_spec(&spec).is_empty(),
                "size {size:?} should be a valid quantity"
            );
        }
    }

    #[test]
    fn rejects_disk_class_replicated_with_t2_hint() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data", "class": "replicated" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.needs.disk");
        assert!(errors[0].message.contains("local"));
        assert!(errors[0].message.contains("T2"));
    }

    #[test]
    fn accepts_disk_class_local_explicit() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data", "class": "local" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    // ---- 2.16d: resources quantity + request <= limit ----
    #[test]
    fn rejects_malformed_resource_quantity() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "resources": { "limits": { "memory": "12x" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.resources.limits.memory");
        assert!(errors[0].message.contains("quantity"));
    }

    #[test]
    fn rejects_request_greater_than_limit() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "resources": {
                    "requests": { "memory": "1Gi" },
                    "limits":   { "memory": "256Mi" }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.resources");
        assert!(errors[0].message.contains("requests.memory"));
        assert!(errors[0].message.contains("limits.memory"));
    }

    #[test]
    fn accepts_valid_resources_and_request_leq_limit() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "resources": {
                    "requests": { "cpu": "100m", "memory": "256Mi" },
                    "limits":   { "cpu": "500m", "memory": "512Mi" }
                }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn quantity_le_unit_tests() {
        assert!(quantity_le("256Mi", "1Gi"));
        assert!(quantity_le("100m", "1"));
        assert!(quantity_le("512Mi", "512Mi"));
        assert!(!quantity_le("2Gi", "1Gi"));
        assert!(!quantity_le("600m", "500m"));
    }

    #[test]
    fn rejects_disk_with_base_replicas_greater_than_one() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 2,
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.base.replicas");
        assert!(replica_errs[0].message.contains("replicas: 1"));
        assert!(replica_errs[0].message.contains("T2"));
    }

    #[test]
    fn rejects_disk_with_env_override_replicas_greater_than_one() {
        // A per-environment replicas override > 1 with disk present in
        // that environment is rejected against the effective replicas.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1
            },
            "environments": {
                "prod": {
                    "replicas": 3,
                    "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.environments.prod.replicas");
    }

    #[test]
    fn rejects_env_disk_against_inherited_base_replicas() {
        // A disk declared in an environment with no env-scoped replicas
        // override inherits the base replicas; base > 1 is rejected.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 4
            },
            "environments": {
                "prod": {
                    "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.environments.prod.replicas");
    }

    #[test]
    fn rejects_inherited_base_disk_against_env_replicas_override() {
        // 2.6b-4 BYPASS GUARD: base declares needs.disk + replicas:1; an
        // environment overrides ONLY replicas (no needs block), so the
        // effective prod spec INHERITS base's disk and mounts it on 3
        // replicas. The replicas guard must reject on prod.replicas even
        // though prod has no literal needs.disk.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1,
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "prod": { "replicas": 3 }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.environments.prod.replicas");
    }

    #[test]
    fn rejects_inherited_base_disk_when_env_redeclares_other_needs_only() {
        // Per-key needs merge: an env that re-declares `needs` with a
        // DIFFERENT type (pg) but NO `disk` key still INHERITS base's
        // disk (the merge is per-key, not whole-block replace). So with
        // env replicas:2 the effective prod still mounts the inherited
        // disk on 2 replicas → rejected on prod.replicas.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1,
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "prod": {
                    "replicas": 2,
                    "needs": { "pg": {} }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.environments.prod.replicas");
    }

    #[test]
    fn accepts_inherited_base_disk_with_single_replica_env_override() {
        // The same base, but prod overrides replicas back to 1 → the
        // effective prod mounts the inherited disk on a single replica,
        // which is allowed. (Base replicas:1 already, but the env
        // explicitly re-pins 1 — both effective views must accept.)
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 1,
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "prod": { "replicas": 1 }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_base_disk_inherited_into_default_env_replicas() {
        // base.needs.disk with NO base.replicas (default 1) + an env with
        // replicas:5 and no needs → the inherited disk is rejected against
        // the env override even though base never set replicas.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "staging": { "replicas": 5 }
            }
        });
        let errors = validate_application_spec(&spec);
        let replica_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("single-replica"))
            .collect();
        assert_eq!(replica_errs.len(), 1);
        assert_eq!(replica_errs[0].field, "spec.environments.staging.replicas");
    }

    #[test]
    fn accepts_multi_replica_app_with_no_disk() {
        // The replicas guard fires ONLY when a disk is present; a
        // disk-less app may have replicas > 1.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "replicas": 5
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    // ── 2.6c (T10): reference-disk discrimination ─────────────────────────

    #[test]
    fn reference_disk_does_not_require_replicas_one() {
        // A referenced disk (binds an existing SharedVolume) is an RWX
        // shared volume, so it does NOT contribute to the single-replica
        // guard — a multi-replica app may mount it.
        let spec = json!({
            "base": {
                "image": "x",
                "replicas": 3,
                "needs": { "disk": { "ref": "shared", "mountPath": "/data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert!(
            errors.is_empty(),
            "reference disk must allow multi-replica: {errors:?}"
        );
    }

    #[test]
    fn rejects_namespaced_ref() {
        // A `ref` carrying a namespace (`ns/name`) implies a cross-namespace
        // shared volume — deferred to T2 (NFS).
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "disk": { "ref": "other-ns/shared", "mountPath": "/data" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert!(
            errors.iter().any(|e| e.message.contains("cross-namespace")),
            "expected a cross-namespace rejection, got {errors:?}"
        );
    }

    #[test]
    fn reference_disk_rejects_size_field() {
        // `ref` + `size` is an invalid mixed shape: a referenced disk
        // carries only ref + mountPath + readOnly.
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "disk": { "ref": "shared", "size": "1Gi", "mountPath": "/d" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert!(
            !errors.is_empty(),
            "ref + size is an invalid mixed shape: {errors:?}"
        );
    }

    #[test]
    fn reference_disk_rejects_name_and_class_fields() {
        // A referenced disk carries only ref + mountPath + readOnly; an
        // explicit `name` or `class` is the owned-shape vocabulary.
        let with_name = json!({
            "base": {
                "image": "x",
                "needs": { "disk": { "ref": "shared", "name": "data", "mountPath": "/d" } }
            }
        });
        assert!(
            !validate_application_spec(&with_name).is_empty(),
            "ref + name is an invalid mixed shape"
        );
        let with_class = json!({
            "base": {
                "image": "x",
                "needs": { "disk": { "ref": "shared", "class": "local", "mountPath": "/d" } }
            }
        });
        assert!(
            !validate_application_spec(&with_class).is_empty(),
            "ref + class is an invalid mixed shape"
        );
    }

    #[test]
    fn reference_disk_still_requires_absolute_unique_mount_path() {
        // Reference disks STILL go through mountPath (absolute + app-wide
        // unique) — this prevents pod volume-name collisions.
        let relative = json!({
            "base": {
                "image": "x",
                "needs": { "disk": { "ref": "shared", "mountPath": "data" } }
            }
        });
        assert!(
            validate_application_spec(&relative)
                .iter()
                .any(|e| e.message.contains("absolute")),
            "reference disk mountPath must still be absolute"
        );
        // Two reference disks colliding on mountPath.
        let collide = json!({
            "base": {
                "image": "x",
                "needs": { "disk": [
                    { "ref": "a", "mountPath": "/data" },
                    { "ref": "b", "mountPath": "/data" }
                ] }
            }
        });
        assert!(
            validate_application_spec(&collide)
                .iter()
                .any(|e| e.message.contains("more than once")),
            "reference disk mountPath must still be app-wide unique"
        );
    }

    #[test]
    fn accepts_valid_reference_disk() {
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "disk": { "ref": "shared", "mountPath": "/data", "readOnly": true } }
            }
        });
        assert!(
            validate_application_spec(&spec).is_empty(),
            "a bare ref + mountPath + readOnly disk must be accepted"
        );
    }

    #[test]
    fn owned_disk_still_requires_size_and_replicas_one() {
        // Regression: an owned disk with replicas 3 is still rejected.
        let spec = json!({
            "base": {
                "image": "x",
                "replicas": 3,
                "needs": { "disk": { "size": "1Gi", "mountPath": "/data" } }
            }
        });
        assert!(!validate_application_spec(&spec).is_empty());
        // And an owned disk missing `size` is still rejected.
        let no_size = json!({
            "base": {
                "image": "x",
                "needs": { "disk": { "mountPath": "/data" } }
            }
        });
        assert!(
            validate_application_spec(&no_size)
                .iter()
                .any(|e| e.message.contains("required `size`")),
            "owned disk still requires size"
        );
    }

    #[test]
    fn sharedvolume_requires_quantity_size_and_local_class() {
        assert!(
            validate_sharedvolume(&json!({ "spec": { "size": "oops" } }))
                .iter()
                .any(|e| e.message.contains("quantity"))
        );
        assert!(validate_sharedvolume(
            &json!({ "spec": { "size": "5Gi", "class": "replicated" } })
        )
        .iter()
        .any(|e| e.message.contains("local")));
        assert!(validate_sharedvolume(&json!({ "spec": { "size": "5Gi" } })).is_empty());
    }

    #[test]
    fn disk_mount_path_uniqueness_is_app_wide_across_scopes() {
        // mountPath must be unique within the app: the same mountPath in
        // base and in an environment collides.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "name": "data", "size": "1Gi", "mountPath": "/data" } }
            },
            "environments": {
                "prod": {
                    "needs": { "disk": { "name": "other", "size": "1Gi", "mountPath": "/data" } }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let mp_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.message.contains("mountPath") && e.message.contains("/data"))
            .collect();
        assert_eq!(mp_errs.len(), 1);
        assert!(mp_errs[0].message.contains("/data"));
    }

    #[test]
    fn reports_every_disk_violation_no_short_circuit() {
        // Multi-error: a bad size, a bad class, and a relative mountPath
        // in one disk array each surface (one message per offending
        // field). Distinct mountPaths/names so only these three fire.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": {
                    "disk": [
                        { "name": "a", "size": "nope", "mountPath": "/a" },
                        { "name": "b", "size": "1Gi", "mountPath": "/b", "class": "shared" },
                        { "name": "c", "size": "1Gi", "mountPath": "rel" }
                    ]
                }
            }
        });
        let errors = validate_application_spec(&spec);
        assert!(errors.iter().any(|e| e.message.contains("quantity")));
        assert!(errors
            .iter()
            .any(|e| e.message.contains("local") && e.message.contains("T2")));
        assert!(errors.iter().any(|e| e.message.contains("absolute")));
        assert_eq!(errors.len(), 3);
    }

    // ---- 2.4h-b: imagePolicy is a CRD-enforced pass-through ----

    #[test]
    fn application_with_image_policy_is_accepted() {
        // `imagePolicy.resolve` is an enum the OpenAPI v3 CRD enforces;
        // there is no cross-field invariant, so the webhook has no rule
        // for it and must accept an Application that declares it.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:latest",
                "imagePolicy": { "resolve": "off" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn application_with_per_env_image_policy_is_accepted() {
        // The per-environment mirror of `imagePolicy` is likewise a pure
        // pass-through for the webhook.
        let spec = json!({
            "base": { "image": "ghcr.io/acme/web:latest" },
            "environments": {
                "prod": { "imagePolicy": { "resolve": "digest" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    // ---- 2.16c (R4-H3): the undeclared-`spec.environment` rejection is
    // REMOVED. It closed nothing and rejected the legitimate base-only +
    // `--env prod` shape (empty/absent `environments`) on every Argo
    // UPDATE → a permanent sync-fail loop. `spec.environment` selecting an
    // env NOT under `spec.environments` is now ACCEPTED (it falls back to
    // base). The immutability guard (`environment_update_allowed`) still
    // governs CHANGING a concrete env on UPDATE. ----

    #[test]
    fn accepts_spec_environment_not_in_declared_environments() {
        // R4-H3: previously rejected; now a no-op (selector falls back to base).
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "dev": {}, "prod": {} },
            "environment": "staging"
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_spec_environment_matching_declared_key() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": { "dev": {}, "prod": {} },
            "environment": "prod"
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_spec_without_environment_field() {
        let spec = json!({ "base": { "image": "x" }, "environments": { "dev": {} } });
        assert!(validate_application_spec(&spec).is_empty());
    }

    // ---- 2.12d (ADR 0046): env claim/secret ref validation ----

    #[test]
    fn rejects_claim_ref_type_not_in_needs() {
        // (a) `claim.foo.url` where `foo` is NOT in needs → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "FOO_URL": { "claim": "foo.url" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.FOO_URL");
        assert!(errors[0].message.contains("foo"));
        assert!(errors[0].message.contains("not declared in needs"));
    }

    #[test]
    fn rejects_claim_ref_bogus_field_for_pg() {
        // (b) `claim.pg.bogus` — field not in the pg enum → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DB_BOGUS": { "claim": "pg.bogus" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DB_BOGUS");
        assert!(errors[0].message.contains("bogus"));
        assert!(errors[0].message.contains("not valid for"));
    }

    #[test]
    fn rejects_claim_ref_disk_has_no_connection_secret() {
        // (c) `claim.disk.url` — disk has no connection Secret → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "disk": { "size": "1Gi", "mountPath": "/data" } },
                "env": { "DISK_URL": { "claim": "disk.url" } }
            }
        });
        let errors = validate_application_spec(&spec);
        // There may be additional errors from the disk validation shape,
        // but the claim-ref error must be present.
        let claim_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.DISK_URL")
            .collect();
        assert_eq!(claim_errs.len(), 1);
        assert!(claim_errs[0].message.contains("disk"));
        assert!(claim_errs[0].message.contains("no connection Secret"));
    }

    #[test]
    fn rejects_claim_ref_named_on_scalar_need() {
        // (d) `claim.pg.main.url` where needs.pg is scalar → REJECT (named
        // ref on scalar). Named ref on array WITH "main" → ACCEPT.
        let spec_reject = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DB_MAIN": { "claim": "pg.main.url" } }
            }
        });
        let errors = validate_application_spec(&spec_reject);
        assert_eq!(errors.len(), 1, "scalar need: named ref must be rejected");
        assert_eq!(errors[0].field, "spec.base.env.DB_MAIN");
        assert!(errors[0].message.contains("named ref") || errors[0].message.contains("scalar"));

        // With a named array entry `main` → ACCEPT.
        let spec_accept = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "main" }] },
                "env": { "DB_MAIN": { "claim": "pg.main.url" } }
            }
        });
        assert!(
            validate_application_spec(&spec_accept).is_empty(),
            "named array entry: named ref must be accepted"
        );
    }

    #[test]
    fn rejects_secret_ref_no_slash() {
        // (e) `secret: ""` and `secret: "nokey"` (no `/`) → REJECT.
        let spec_empty = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "KEY": { "secret": "" } }
            }
        });
        let errors = validate_application_spec(&spec_empty);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.KEY");
        assert!(errors[0].message.contains("malformed"));

        let spec_nokey = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "KEY": { "secret": "nokey" } }
            }
        });
        let errors = validate_application_spec(&spec_nokey);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.KEY");
        assert!(errors[0].message.contains("malformed"));
    }

    #[test]
    fn accepts_literal_database_url_under_needs_pg_2_12_guard_removed() {
        // (f) a literal `env.DATABASE_URL` under needs.pg → ACCEPT.
        // The 2.4e collision/reserved guard is removed.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DATABASE_URL": "postgres://override" }
            }
        });
        assert!(
            validate_application_spec(&spec).is_empty(),
            "literal DATABASE_URL under needs.pg must be accepted after 2.4e guard removal"
        );
    }

    #[test]
    fn accepts_fully_valid_app_with_literal_claim_and_secret_refs() {
        // (g) a fully-valid app: literal + claim.pg.url + claim.pg.pass
        //     + secret stripe/api-key → ACCEPT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": {
                    "LOG_LEVEL": "info",
                    "DATABASE_URL": { "claim": "pg.url" },
                    "DB_PASS": { "claim": "pg.pass" },
                    "STRIPE_KEY": { "secret": "stripe/api-key" }
                }
            }
        });
        assert!(
            validate_application_spec(&spec).is_empty(),
            "fully valid app with literal + claim + secret refs must be accepted"
        );
    }

    #[test]
    fn accepts_claim_ref_to_jetstream_fields() {
        // 2.5 / ADR 0061 §6: jetstream now has a connection-Secret field
        // vocabulary (`#ClaimFieldsFor.jetstream` in application.cue), so
        // every one of its fields must resolve when declared.
        for field in [
            "url",
            "host",
            "port",
            "user",
            "pass",
            "account",
            "subjectPrefix",
            "inboxPrefix",
        ] {
            let spec = json!({
                "base": {
                    "image": "ghcr.io/acme/web:1.0",
                    "needs": { "jetstream": {} },
                    "env": { "U": { "claim": format!("jetstream.{field}") } }
                }
            });
            let errors = validate_application_spec(&spec);
            let claim_errs: Vec<&ValidationError> = errors
                .iter()
                .filter(|e| e.field == "spec.base.env.U")
                .collect();
            assert!(
                claim_errs.is_empty(),
                "claim.jetstream.{field} must resolve: {claim_errs:?}"
            );
        }
    }

    #[test]
    fn rejects_unknown_jetstream_claim_field() {
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "jetstream": {} },
                "env": { "U": { "claim": "jetstream.nosuchfield" } }
            }
        });
        let errors = validate_application_spec(&spec);
        let claim_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.U")
            .collect();
        assert_eq!(claim_errs.len(), 1);
        assert!(
            claim_errs[0].message.contains("not valid for"),
            "{claim_errs:?}"
        );
    }

    #[test]
    fn rejects_jetstream_claim_ref_without_needs_declared() {
        // The trap the reviewer found: adding jetstream to
        // `CLAIM_SUPPORTED_TYPES` without restoring its
        // `declared_need_names()` arm would make every jetstream claim
        // ref — even a correctly declared one — read as "not declared".
        // Assert the genuinely-undeclared case still reports that
        // message, so a regression here (the `declared_need_names()` arm
        // going missing again) goes red instead of silently changing
        // which case produces the message.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "U": { "claim": "jetstream.url" } }
            }
        });
        let errors = validate_application_spec(&spec);
        let claim_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.U")
            .collect();
        assert_eq!(claim_errs.len(), 1);
        assert!(
            claim_errs[0].message.contains("not declared in needs"),
            "{claim_errs:?}"
        );
    }

    #[test]
    fn rejects_named_jetstream_claim_ref_with_advice_that_is_actually_possible() {
        // `claim.jetstream.foo.url` — a name segment on a type that is
        // declared but is ALWAYS scalar (ADR 0061 §6, no array form
        // exists). The generic fall-through message for this shape
        // ("omit the name segment or add a named entry") is wrong here:
        // jetstream can never gain a named entry, so half the advice is
        // impossible. Pin the corrected, jetstream-specific message —
        // there was no test on this path at all before.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "jetstream": {} },
                "env": { "U": { "claim": "jetstream.foo.url" } }
            }
        });
        let errors = validate_application_spec(&spec);
        let claim_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.U")
            .collect();
        assert_eq!(claim_errs.len(), 1);
        assert!(
            claim_errs[0]
                .message
                .contains("jetstream has no named entries"),
            "{claim_errs:?}"
        );
        assert!(
            !claim_errs[0].message.contains("add a named entry"),
            "the advice must not tell the user to do something impossible: {claim_errs:?}"
        );
    }

    #[test]
    fn rejects_claim_ref_malformed_too_many_segments() {
        // More than 3 segments is malformed.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": { "DB": { "claim": "pg.a.b.c" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DB");
        assert!(errors[0].message.contains("malformed"));
    }

    #[test]
    fn claim_ref_env_scope_uses_effective_needs_not_just_env_needs() {
        // `environments.prod.env` has a claim.redis.url ref. base.needs.redis
        // is declared (not prod.needs). The effective needs for prod is
        // base.needs merged with prod.needs (empty) → redis IS in effective
        // needs → ACCEPT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": {} }
            },
            "environments": {
                "prod": { "env": { "REDIS_URL": { "claim": "redis.url" } } }
            }
        });
        assert!(
            validate_application_spec(&spec).is_empty(),
            "env scope should inherit base needs when checking claim refs"
        );
    }

    #[test]
    fn claim_ref_env_scope_overridden_need_replaces_base() {
        // prod.needs.pg overrides base.needs.redis (different type, so
        // base.needs.redis is still in the merged effective).
        // prod.env has claim.redis.url → redis IS in effective needs → ACCEPT.
        // prod.env has claim.pg.url → pg IS in effective needs (from prod.needs) → ACCEPT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "redis": {} }
            },
            "environments": {
                "prod": {
                    "needs": { "pg": {} },
                    "env": {
                        "REDIS_CONN": { "claim": "redis.url" },
                        "DB_URL": { "claim": "pg.url" }
                    }
                }
            }
        });
        assert!(
            validate_application_spec(&spec).is_empty(),
            "per-key needs merge: both inherited redis and overriding pg should be accessible"
        );
    }

    #[test]
    fn rejects_claim_ref_named_entry_not_found_in_array() {
        // claim.pg.missing.url where needs.pg has [name=main] but not
        // name=missing → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": [{ "name": "main" }] },
                "env": { "DB": { "claim": "pg.missing.url" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.DB");
        assert!(
            errors[0].message.contains("missing") || errors[0].message.contains("no entry named")
        );
    }

    #[test]
    fn accepts_secret_ref_valid_dns_name_and_key() {
        // A well-formed `secret: "stripe/api-key"` → ACCEPT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "STRIPE_KEY": { "secret": "stripe/api-key" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_secret_ref_bad_dns_name() {
        // Secret name with uppercase is not DNS-1123 → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "KEY": { "secret": "BadName/key" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.KEY");
        assert!(errors[0].message.contains("DNS-1123"));
    }

    #[test]
    fn rejects_secret_ref_empty_key() {
        // `secret: "myname/"` — key is empty → REJECT.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "env": { "KEY": { "secret": "myname/" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.env.KEY");
        assert!(errors[0].message.contains("key"));
    }

    #[test]
    fn env_ref_validation_multi_error_no_short_circuit() {
        // Two bad refs → two errors, no short-circuit.
        let spec = json!({
            "base": {
                "image": "ghcr.io/acme/web:1.0",
                "needs": { "pg": {} },
                "env": {
                    "DB_BOGUS": { "claim": "pg.bogus" },
                    "BAD_SEC": { "secret": "nokey" }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let claim_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.DB_BOGUS")
            .collect();
        let secret_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.env.BAD_SEC")
            .collect();
        assert_eq!(claim_errs.len(), 1);
        assert_eq!(secret_errs.len(), 1);
    }

    #[test]
    fn accepts_public_with_valid_subdomain_hostname() {
        let spec = json!({
            "base": {
                "image": "x",
                "expose": { "port": 8080, "network": "public", "hostname": "app.demo.dev" }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_public_with_array_hostnames() {
        let spec = json!({
            "base": {
                "image": "x",
                "expose": { "port": 8080, "network": "public",
                            "hostname": ["a.demo.dev", "b.demo.dev"] }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn rejects_public_without_hostname() {
        let spec = json!({
            "base": { "image": "x", "expose": { "port": 8080, "network": "public" } }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.expose.hostname");
        assert!(errors[0].message.contains("required"));
    }

    #[test]
    fn rejects_hostname_without_public() {
        let spec = json!({
            "base": { "image": "x",
                      "expose": { "port": 8080, "hostname": "app.demo.dev" } }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.expose.hostname");
        assert!(errors[0].message.contains("network: public"));
    }

    #[test]
    fn rejects_network_vpn() {
        let spec = json!({
            "base": { "image": "x", "expose": { "port": 8080, "network": "vpn" } }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.expose.network");
        assert!(errors[0].message.contains("not yet implemented"));
    }

    #[test]
    fn rejects_public_wildcard_hostname() {
        let spec = json!({
            "base": { "image": "x",
                      "expose": { "port": 8080, "network": "public", "hostname": "*.demo.dev" } }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.expose.hostname");
    }

    #[test]
    fn validates_expose_under_environment_overrides_too() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": {
                "prod": { "expose": { "port": 8080, "network": "public" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.prod.expose.hostname");
    }

    #[test]
    fn rejects_tls_false_with_public() {
        let spec = json!({
            "base": { "image": "x",
                      "expose": { "port": 8080, "network": "public",
                                  "hostname": "app.demo.dev", "tls": false } }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.base.expose.tls");
        assert!(errors[0].message.contains("4.1b"));
    }

    #[test]
    fn accepts_tls_true_explicit_with_public() {
        let spec = json!({
            "base": { "image": "x",
                      "expose": { "port": 8080, "network": "public",
                                  "hostname": "app.demo.dev", "tls": true } }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    // ── 2.16c: an env override that sets `expose` while base has no
    // `expose` must carry a `port` (the effective expose is otherwise
    // portless — base cannot supply it). `base.expose.port` is required by
    // the CRD, so only the base-absent case needs guarding.
    #[test]
    fn rejects_env_only_expose_without_port_when_base_has_no_expose() {
        let spec = json!({
            "base": { "image": "x" },
            "environments": {
                "prod": { "expose": { "network": "internal" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "spec.environments.prod.expose.port");
        assert!(errors[0].message.contains("port"));
    }

    #[test]
    fn accepts_env_expose_without_port_when_base_expose_has_port() {
        let spec = json!({
            "base": {
                "image": "x",
                "expose": { "port": 8080, "network": "public", "hostname": "app.demo.dev" }
            },
            "environments": {
                "dev": { "expose": { "network": "internal" } }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn accepts_base_only_with_deploy_env_scalar() {
        // R4-H3 regression: base-only (with image) + a `spec.environment`
        // selector naming an env that is NOT declared under `environments`
        // is ACCEPTED — no `spec.environment ∈ environments` rejection here,
        // so base-only + `--env` keeps working.
        let spec = json!({
            "base": { "image": "x" },
            "environment": "prod"
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    // ── 2.16b S7.2: spec.environment immutable on UPDATE ──────────────────
    #[test]
    fn environment_change_on_update_is_rejected() {
        assert!(environment_update_allowed(None, Some("dev"))); // CREATE (no old) -> ok
        assert!(environment_update_allowed(Some("dev"), Some("dev"))); // unchanged -> ok
        assert!(environment_update_allowed(Some(""), Some("dev"))); // old absent-as-empty -> first set ok
        assert!(!environment_update_allowed(Some("dev"), Some("prod"))); // change -> rejected
        assert!(!environment_update_allowed(Some("dev"), None)); // clearing a set env -> rejected
    }

    // ── 2.5 (ADR 0061 §6): validate_jetstream_need — local rejection ──────
    // Cross-object checks (fan-in, `consume.from` naming no application)
    // are explicitly OUT of scope for this webhook (ADR 0061 §5: detection
    // runs on the provisioner resync) — every test here stays within one
    // manifest, no sibling object involved.

    #[test]
    fn rejects_jetstream_persistent_and_name() {
        // both are declared in the schema ONLY so they can be rejected here —
        // a structural schema prunes unknown fields before a validating webhook
        // runs, so omitting them would drop `persistent: true` silently
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "name": "x", "persistent": true } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 2, "{js_errs:?}");
        assert!(
            js_errs
                .iter()
                .any(|e| e.message.contains("needs.jetstream.persistent")),
            "{js_errs:?}"
        );
        assert!(
            js_errs
                .iter()
                .any(|e| e.message.contains("needs.jetstream.name")),
            "{js_errs:?}"
        );
        // The `app` parameter is used, not dropped — every message is
        // prefixed with the scope label.
        assert!(
            js_errs.iter().all(|e| e.message.starts_with("base:")),
            "{js_errs:?}"
        );
    }

    #[test]
    fn rejects_reserved_and_wildcard_subjects() {
        // "$JS.foo.>", "$SYS.x", "_INBOX.y", "_INBOX_demo_feeder.>", ">"
        // — each must be rejected. `_INBOX_demo_feeder.>` is the REAL
        // shape ClaimView::inbox_prefix() mints (resourceclaim-provisioner
        // ::nats_accounts) — underscore-, not dot-joined — round-7 review
        // (H1): the guard's literal was `"_INBOX."` (trailing dot), which
        // `"_INBOX_demo_feeder.>".starts_with(...)` is FALSE against, so
        // only the default dot-form inbox (used solely by `mgr_<ns>`) was
        // ever caught; a stream declared over this exact subject captured
        // a namespace-mate's JetStream API replies, reproduced against
        // nats-server 2.14.3. `_INBOX.y` (the dot form) already passed
        // before this fix — kept here so the fix is proven not to have
        // narrowed the existing coverage while it widened it.
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "streams": [
                    { "name": "s1", "subjects": ["$JS.foo.>"], "maxBytes": "1Gi" },
                    { "name": "s2", "subjects": ["$SYS.x"], "maxBytes": "1Gi" },
                    { "name": "s3", "subjects": ["_INBOX.y"], "maxBytes": "1Gi" },
                    { "name": "s4", "subjects": [">"], "maxBytes": "1Gi" },
                    { "name": "s5", "subjects": ["_INBOX_demo_feeder.>"], "maxBytes": "1Gi" }
                ] } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 5, "{js_errs:?}");
        for subject in [
            "$JS.foo.>",
            "$SYS.x",
            "_INBOX.y",
            ">",
            "_INBOX_demo_feeder.>",
        ] {
            assert!(
                js_errs
                    .iter()
                    .any(|e| e.message.contains(&format!("{subject:?}"))),
                "expected a rejection naming {subject:?}: {js_errs:?}"
            );
        }
    }

    // ── round-7 review (H4): metadata.name must be a DNS-1123 LABEL when
    // needs.jetstream is present ─────────────────────────────────────────
    // `nats_stream_name` composes `<app>_<declared>` into a NATS stream
    // name (a single token — NATS's own rule) and
    // `ClaimView::subject_prefix()` composes `<app>.` into the per-app
    // subject partition. `metadata.name` is normally a Kubernetes object
    // name — a DNS-1123 SUBDOMAIN, which permits '.' — so
    // `nats_stream_name("my.app", "orders")` composes `my.app_orders`
    // (illegal: NATS rejects '.' in a stream name outright) and
    // `subject_prefix()` composes `my.app.`, nesting app `my.app`'s
    // partition inside a namesake app `my`'s own `my.` prefix, letting
    // `my` publish into `my.app`'s tree. Masked for any app with
    // `expose` (the Service name is the app name and Service names are
    // themselves labels), but a jetstream app WITHOUT `expose` hits it
    // cleanly. Gated on `needs.jetstream` because the constraint this
    // adds (label, not subdomain) is strictly narrower than what the
    // apiserver's own object-name validation already enforces
    // unconditionally, and only jetstream composes the name this way.

    #[test]
    fn jetstream_gate_rejects_a_dotted_metadata_name() {
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "streams": [
                    { "name": "orders", "subjects": ["orders.>"], "maxBytes": "1Gi" }
                ] } }
            }
        });
        let errs = validate_application_name_for_jetstream("my.app", &spec);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].field, "metadata.name");
        assert!(errs[0].message.contains("DNS-1123 label"), "{:?}", errs[0]);
    }

    #[test]
    fn jetstream_gate_allows_a_label_metadata_name() {
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "streams": [
                    { "name": "orders", "subjects": ["orders.>"], "maxBytes": "1Gi" }
                ] } }
            }
        });
        assert!(validate_application_name_for_jetstream("my-app", &spec).is_empty());
    }

    #[test]
    fn jetstream_gate_ignores_a_dotted_name_without_jetstream() {
        // The gate is conditional: a dotted metadata.name is otherwise the
        // apiserver's own business (a DNS-1123 subdomain, which permits
        // '.'), not this webhook's, unless jetstream is what will compose
        // it into a NATS identifier.
        let spec = json!({ "base": { "image": "x" } });
        assert!(validate_application_name_for_jetstream("my.app", &spec).is_empty());
    }

    #[test]
    fn jetstream_gate_checks_environments_too() {
        // needs.jetstream can live under an environment override instead
        // of base — the presence check must union both, not just base.
        let spec = json!({
            "base": { "image": "x" },
            "environments": {
                "prod": {
                    "image": "x",
                    "needs": { "jetstream": { "streams": [
                        { "name": "orders", "subjects": ["orders.>"], "maxBytes": "1Gi" }
                    ] } }
                }
            }
        });
        let errs = validate_application_name_for_jetstream("my.app", &spec);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].field, "metadata.name");
    }

    #[test]
    fn requires_max_bytes_on_every_declared_stream() {
        // a stream without maxBytes silently claims the whole account quota.
        // Two streams — one valid, one not — proves the check runs
        // per-stream (the valid one produces no error) and names the
        // offending one.
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "streams": [
                    { "name": "orders", "subjects": ["shop.orders.>"], "maxBytes": "1Gi" },
                    { "name": "events", "subjects": ["shop.events.>"], "maxBytes": "" }
                ] } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 1, "{js_errs:?}");
        assert!(js_errs[0].message.contains("maxBytes"), "{js_errs:?}");
        assert!(js_errs[0].message.contains("events"), "{js_errs:?}");
    }

    #[test]
    fn requires_at_least_one_subject_per_stream() {
        // the CRD minItems you added covers the apiserver path; this covers the
        // webhook path, and they must agree
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "streams": [
                    { "name": "orders", "subjects": ["shop.orders.>"], "maxBytes": "1Gi" },
                    { "name": "events", "subjects": [], "maxBytes": "1Gi" }
                ] } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 1, "{js_errs:?}");
        assert!(js_errs[0].message.contains("subjects"), "{js_errs:?}");
        assert!(js_errs[0].message.contains("events"), "{js_errs:?}");
    }

    #[test]
    fn rejects_duplicate_stream_names() {
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "streams": [
                    { "name": "orders", "subjects": ["shop.orders.>"], "maxBytes": "1Gi" },
                    { "name": "orders", "subjects": ["shop.orders2.>"], "maxBytes": "1Gi" }
                ] } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 1, "{js_errs:?}");
        assert!(js_errs[0].message.contains("duplicate"), "{js_errs:?}");
    }

    #[test]
    fn rejects_non_dns_1123_stream_name() {
        // Round-7 review: `streams[].name` becomes half of the composed
        // NATS stream name `<app>_<name>` (nats_stream_name, ADR 0061 §3/
        // §6). A `_` inside the declared name would make that encoding
        // ambiguous — this is the guard that keeps the join injective.
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "streams": [
                    { "name": "blocks_head", "subjects": ["shop.orders.>"], "maxBytes": "1Gi" }
                ] } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 1, "{js_errs:?}");
        assert!(js_errs[0].message.contains("DNS-1123"), "{js_errs:?}");
    }

    #[test]
    fn rejects_non_dns_1123_durable_name() {
        // The other half of the same guard: `consume[].durable` becomes
        // half of the composed NATS durable name `<app>_<durable>`
        // (nats_durable_name).
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "consume": [
                    { "stream": "blocks-head", "durable": "idx_app" }
                ] } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 1, "{js_errs:?}");
        assert!(js_errs[0].message.contains("DNS-1123"), "{js_errs:?}");
    }

    #[test]
    fn rejects_durable_name_colliding_with_a_declared_stream() {
        // load-bearing: the deny vector's position patterns ($JS.API.*.*.*.S)
        // are safe ONLY because a durable can never share a name with a stream
        // of the same application. streams[{name: "orders"}] +
        // consume[{durable: "orders"}] must be rejected.
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": {
                    "streams": [
                        { "name": "orders", "subjects": ["shop.orders.>"], "maxBytes": "1Gi" }
                    ],
                    "consume": [
                        { "stream": "orders", "durable": "orders" }
                    ]
                } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 1, "{js_errs:?}");
        assert!(js_errs[0].message.contains("durable"), "{js_errs:?}");
    }

    #[test]
    fn rejects_consume_entry_missing_stream_or_durable() {
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": {
                    "consume": [
                        { "stream": "", "durable": "" }
                    ]
                } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 2, "{js_errs:?}");
        assert!(
            js_errs
                .iter()
                .any(|e| e.message.contains(".stream must not be empty")),
            "{js_errs:?}"
        );
        assert!(
            js_errs
                .iter()
                .any(|e| e.message.contains(".durable must not be empty")),
            "{js_errs:?}"
        );
    }

    // ---- 2.5 (ADR 0061 §6): per-environment override drops ----

    /// A base with both blocks, for the override tests below.
    fn base_with_streams_and_consume() -> Value {
        json!({
            "image": "x",
            "needs": { "jetstream": {
                "streams": [
                    { "name": "orders", "subjects": ["shop.orders.>"], "maxBytes": "1Gi" },
                    { "name": "events", "subjects": ["shop.events.>"], "maxBytes": "1Gi" }
                ],
                "consume": [
                    { "from": "billing", "stream": "invoices", "durable": "reader" }
                ]
            } }
        })
    }

    fn jetstream_env_errors<'a>(
        errors: &'a [ValidationError],
        env: &str,
    ) -> Vec<&'a ValidationError> {
        let field = format!("spec.environments.{env}.needs.jetstream");
        errors.iter().filter(|e| e.field == field).collect()
    }

    #[test]
    fn rejects_an_env_override_that_drops_the_declared_streams() {
        // THE case ADR 0061 §6 names: one field's sake, and the whole
        // producer contract is gone for prod.
        let spec = json!({
            "base": base_with_streams_and_consume(),
            "environments": { "prod": { "needs": { "jetstream": { "size": "small" } } } }
        });
        let errors = validate_application_spec(&spec);
        let env_errs = jetstream_env_errors(&errors, "prod");
        assert_eq!(
            env_errs.len(),
            2,
            "streams AND consume are both lost: {env_errs:?}"
        );

        let streams_err = env_errs
            .iter()
            .find(|e| e.message.contains("\"streams\""))
            .expect("a streams error");
        // Names what is lost, not merely the field — the manifest that
        // caused it never mentions these.
        assert!(
            streams_err.message.contains("\"orders\""),
            "{streams_err:?}"
        );
        assert!(
            streams_err.message.contains("\"events\""),
            "{streams_err:?}"
        );
        assert!(streams_err.message.contains("prod:"), "{streams_err:?}");

        let consume_err = env_errs
            .iter()
            .find(|e| e.message.contains("\"consume\""))
            .expect("a consume error");
        assert!(
            consume_err.message.contains("\"invoices/reader\""),
            "the consume entry is named <stream>/<durable>: {consume_err:?}"
        );
    }

    #[test]
    fn accepts_an_env_override_that_repeats_the_declared_blocks() {
        // The fix the message asks for. Must be accepted, or the rule has
        // no way out.
        let spec = json!({
            "base": base_with_streams_and_consume(),
            "environments": { "prod": { "needs": { "jetstream": {
                "size": "small",
                "streams": [
                    { "name": "orders", "subjects": ["shop.orders.>"], "maxBytes": "1Gi" },
                    { "name": "events", "subjects": ["shop.events.>"], "maxBytes": "1Gi" }
                ],
                "consume": [
                    { "from": "billing", "stream": "invoices", "durable": "reader" }
                ]
            } } } }
        });
        assert!(
            jetstream_env_errors(&validate_application_spec(&spec), "prod").is_empty(),
            "{:?}",
            validate_application_spec(&spec)
        );
    }

    #[test]
    fn accepts_an_env_override_that_empties_the_blocks_explicitly() {
        // The escape hatch: an environment that genuinely produces and
        // consumes nothing. Under wholesale replacement this is the ONLY
        // way to express that, so rejecting it would leave the intent
        // inexpressible rather than merely awkward.
        let spec = json!({
            "base": base_with_streams_and_consume(),
            "environments": { "staging": { "needs": { "jetstream": {
                "streams": [],
                "consume": []
            } } } }
        });
        assert!(
            jetstream_env_errors(&validate_application_spec(&spec), "staging").is_empty(),
            "{:?}",
            validate_application_spec(&spec)
        );
    }

    #[test]
    fn an_env_that_does_not_override_jetstream_at_all_inherits_it() {
        // No `needs.jetstream` key in the override → the per-key needs merge
        // carries base's whole need through. Nothing is lost, so nothing is
        // reported — including when the override touches a DIFFERENT need.
        let spec = json!({
            "base": base_with_streams_and_consume(),
            "environments": {
                "prod": { "replicas": 3 },
                "staging": { "needs": { "pg": {} } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert!(
            jetstream_env_errors(&errors, "prod").is_empty(),
            "{errors:?}"
        );
        assert!(
            jetstream_env_errors(&errors, "staging").is_empty(),
            "{errors:?}"
        );
    }

    #[test]
    fn an_env_override_drops_only_the_block_base_actually_declares() {
        // base declares streams but NO consume. An override omitting both
        // must be told about streams only — reporting a lost `consume` that
        // never existed would train readers to ignore the message.
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": { "streams": [
                    { "name": "orders", "subjects": ["shop.orders.>"], "maxBytes": "1Gi" }
                ] } }
            },
            "environments": { "prod": { "needs": { "jetstream": { "size": "small" } } } }
        });
        let env_errs_owned = validate_application_spec(&spec);
        let env_errs = jetstream_env_errors(&env_errs_owned, "prod");
        assert_eq!(env_errs.len(), 1, "{env_errs:?}");
        assert!(env_errs[0].message.contains("\"streams\""), "{env_errs:?}");
    }

    #[test]
    fn a_base_without_declarations_never_trips_the_override_rule() {
        // A dynamicStreams-only app: base has no streams/consume, so an
        // override that omits them loses nothing.
        let spec = json!({
            "base": { "image": "x", "needs": { "jetstream": { "dynamicStreams": true } } },
            "environments": { "prod": { "needs": { "jetstream": { "size": "large" } } } }
        });
        assert!(
            jetstream_env_errors(&validate_application_spec(&spec), "prod").is_empty(),
            "{:?}",
            validate_application_spec(&spec)
        );
    }

    #[test]
    fn accepts_a_bare_jetstream_need() {
        // `needs: {jetstream: {}}` is the "all my streams are dynamic" case and
        // must stay valid
        let spec = json!({
            "base": {
                "image": "x",
                "needs": { "jetstream": {} }
            }
        });
        assert!(validate_application_spec(&spec).is_empty());
    }

    #[test]
    fn validates_jetstream_need_in_environment_scope_too() {
        // Step 6: wired once for base, once per environment. Two
        // INDEPENDENT violations (persistent + an empty-subjects stream),
        // and a non-exact `!is_empty()` assertion rather than a count — on
        // purpose, so this test proves WIRING (env scope reached, field
        // path + `{scope}:` prefix correct) without being sensitive to
        // either individual rule, which already has its own dedicated
        // mutation-tested case above. A count-exact assertion here would
        // make this test go red on EITHER of those two rules' removal too,
        // muddying which test caught what.
        let spec = json!({
            "base": { "image": "x" },
            "environments": {
                "prod": {
                    "needs": { "jetstream": {
                        "persistent": true,
                        "streams": [
                            { "name": "orders", "subjects": [], "maxBytes": "1Gi" }
                        ]
                    } }
                }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.environments.prod.needs.jetstream")
            .collect();
        assert!(!js_errs.is_empty(), "{js_errs:?}");
        assert!(
            js_errs.iter().all(|e| e.message.starts_with("prod:")),
            "{js_errs:?}"
        );
    }

    #[test]
    fn raw_fallback_does_not_give_impossible_dns_1123_advice_for_jetstream() {
        // Item A (inherited from the earlier review): `expose.hostname` is
        // preserve-unknown in the CRD but typed in Rust, so an
        // apiserver-valid `expose: {port: 80, hostname: 5}` fails the typed
        // decode and drops the WHOLE scope to `check_scope_raw`. Before this
        // fix, a jetstream `name` on that raw path got "must be a DNS-1123
        // label … so it folds to a valid env-var suffix" — impossible
        // advice, since jetstream has no named claims at all. The name
        // MUST itself fail the DNS-1123 check ("x" alone would not — it
        // IS a valid label — and would defeat this regression test) to
        // actually exercise the buggy branch.
        let spec = json!({
            "base": {
                "image": "x",
                "expose": { "port": 80, "hostname": 5 },
                "needs": { "jetstream": { "name": "Not_Valid" } }
            }
        });
        let errors = validate_application_spec(&spec);
        assert!(
            errors.iter().all(|e| !e.message.contains("DNS-1123")),
            "the raw fallback must not name-validate jetstream: {errors:?}"
        );
    }

    #[test]
    fn raw_fallback_still_rejects_jetstream_persistent_and_name() {
        // I1 (combined review, round 5): the exact probe that found the
        // gap, widened to also carry a `name` (the probe as given only
        // set `persistent`; the fix checks both keys, so the regression
        // test does too — "asserting both messages appear"). Before this
        // fix, `expose.hostname: 5` (apiserver-legal, serde-fatal) drops
        // the base scope to `check_scope_raw`, and `persistent: true` +
        // `name: "x"` both reached the cluster completely unrejected: 0
        // errors total, where `validate_jetstream_need` would have
        // produced 2 had the scope decoded. `persistent`/`name` exist
        // ONLY to be rejected (ADR 0061 §6) — a scope that fails the
        // typed decode is exactly the case that design has to survive,
        // not one it is allowed to skip. The `streams`/`subjects`/
        // `maxBytes` violations in the same manifest are deliberately
        // NOT asserted here — those stay typed-only (the CRD covers them
        // unconditionally instead), so this probe proves only what
        // changed.
        let spec = json!({
            "base": {
                "image": "x",
                "expose": { "port": 80, "hostname": 5 },
                "needs": { "jetstream": {
                    "persistent": true,
                    "name": "x",
                    "streams": [
                        { "name": "s1", "subjects": [], "maxBytes": "" }
                    ]
                } }
            }
        });
        let errors = validate_application_spec(&spec);
        let js_errs: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| e.field == "spec.base.needs.jetstream")
            .collect();
        assert_eq!(js_errs.len(), 2, "{js_errs:?}");
        assert!(
            js_errs
                .iter()
                .any(|e| e.message.contains("needs.jetstream.persistent")),
            "{js_errs:?}"
        );
        assert!(
            js_errs
                .iter()
                .any(|e| e.message.contains("needs.jetstream.name")),
            "{js_errs:?}"
        );
        assert!(
            js_errs.iter().all(|e| e.message.starts_with("base:")),
            "{js_errs:?}"
        );
    }
}
