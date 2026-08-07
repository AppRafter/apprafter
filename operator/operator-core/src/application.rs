// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs CRD types for v1alpha1 `Application`.
//!
//! Mirrors the OpenAPI v3 CRD shipped by the `apprafter-operator`
//! Helm chart (`templates/crd-application.yaml`) and
//! `schemas/v1alpha1/application.cue`. The `kube::CustomResource`
//! derive macro generates the wrapper struct `Application` with the
//! standard apiVersion / kind / metadata / spec / status layout —
//! possible because v0.1.25 wrapped the field tree under `spec`.

use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[kube(
    group = "apprafter.io",
    version = "v1alpha1",
    kind = "Application",
    namespaced,
    status = "ApplicationStatus"
)]
pub struct ApplicationSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<ApplicationBaseSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<BTreeMap<String, ApplicationEnvOverride>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationBaseSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose: Option<ApplicationExpose>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, EnvValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<Needs>,
    /// Image resolution policy (ADR 0040). Absent => default `digest`
    /// (the controller resolves the tag to a registry digest each
    /// reconcile). Mirrors `#ImagePolicy` in application.cue + the CRD.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "imagePolicy"
    )]
    pub image_policy: Option<ImagePolicy>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationExpose {
    pub port: i32,
    /// Visibility (ADR 0048 / 1.83b). `public` → emit an HTTPRoute on the
    /// platform Gateway; `internal` (default) → ClusterIP only; `vpn` →
    /// reserved (the webhook rejects it until AccessGrant/ExternalSurface).
    /// Enforced as an enum by the CRD; a plain `String` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// One or several public hostnames (1.83b). `OneOrMany` (the 2.6b `needs`
    /// union): a bare string OR a list of strings. Consumed only when
    /// `network == "public"`; the renderer normalizes it to a `Vec<String>`
    /// for the HTTPRoute `hostnames`. The CRD field is
    /// `x-kubernetes-preserve-unknown-fields` (CUE can't express scalar|array
    /// structurally); the webhook validates each entry is a DNS-1123 subdomain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<OneOrMany<String>>,
    /// Terminate TLS (1.83b). Minimal `bool` form; the full `#TlsOptions`
    /// lands with 4.1b. Absent → default `true` (the public HTTPS route, TLS
    /// on the 1.83a per-listener static cert). `false` + `network: public` is
    /// rejected by the webhook for now (no HTTP-only public route in this
    /// slice — the route attaches to `:443` only). Enforced as a `bool` by the
    /// CRD; an `Option<bool>` here (`None` == default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<bool>,
}

/// The per-environment PARTIAL `expose` override (2.16c). Mirrors
/// `ApplicationExpose` but every field — INCLUDING `port` — is optional,
/// so an env carries only the diff. `merge_expose` folds it onto the
/// base `ApplicationExpose`; a base-absent env override with no port
/// fails `try_into_expose` (surfaced as `InvalidEffectiveSpec`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ExposeOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<OneOrMany<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<bool>,
}

/// The per-environment PARTIAL override for `Application.spec.environments[*]`
/// (2.16c). All-optional mirror of `ApplicationBaseSpec` with `expose` as
/// the all-optional `ExposeOverride`. `needs`/`imagePolicy` reuse the base
/// types (needs stays wholesale-per-key → 2.16i; imagePolicy is one field).
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationEnvOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose: Option<ExposeOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, EnvValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<Needs>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "imagePolicy")]
    pub image_policy: Option<ImagePolicy>,
}

/// One declared platform-service dependency under
/// `Application.spec.*.needs`, keyed by service type. The 2.4d
/// controller turns each entry into a `ResourceClaim`; the 2.3
/// scheduler routes it via `selector`. Mirrors `#ServiceNeed` in
/// `schemas/v1alpha1/application.cue` and the `needs` block of the
/// OpenAPI v3 CRD.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ServiceNeed {
    /// `(type, name)` claim identity (2.6b). Omit for the unnamed
    /// default claim (`<app>-<type>`); a named entry produces
    /// `<app>-<type>-<name>` and a `<VAR>_<NAME>` env suffix. At most
    /// one unnamed entry per type; names unique within a type
    /// (enforced by the webhook). Mirrors `#ServiceNeed.name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Label selector matched against `ServiceProvider.metadata.labels`.
    /// Optional — the controller injects `{tier: integrated}` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<BTreeMap<String, String>>,
    /// Requested size class (`nano|small|medium|large|xlarge`).
    /// Optional — tier defaults fill it. Enforced as an enum by the
    /// CRD; a plain `String` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// Persist the provisioned resource across Application deletion
    /// (default false). For redis: routes the claim to a *persistent*
    /// pool instance (snapshot→PVC) instead of an ephemeral one (ADR 0042).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,
}

/// A `needs.<type>` value that is either a single (scalar) entry or an
/// array of named entries (2.6b). `#[serde(untagged)]` accepts both
/// JSON shapes natively — the scalar form is the unnamed default claim
/// (zero migration), the array form carries `(type, name)` identities.
/// In the hand-rolled CRD this is an OpenAPI `oneOf: [{object},
/// {array}]`; the `JsonSchema` derive (anyOf) is not the CRD source of
/// truth — the chart is.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T: Clone> OneOrMany<T> {
    /// Normalize to a `Vec`: a scalar becomes a one-element vec, an
    /// array passes through. Consumes self.
    pub fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(t) => vec![t],
            Self::Many(v) => v,
        }
    }

    /// Borrowing variant of [`into_vec`](Self::into_vec) — clones the
    /// entries so the original is untouched.
    pub fn as_slice_vec(&self) -> Vec<T> {
        self.clone().into_vec()
    }
}

/// One persistent-disk dependency under `Application.spec.*.needs.disk`
/// (2.6b/2.6c). Two discriminated shapes share this struct (the CUE
/// disjunction + webhook enforce which fields co-exist at admission):
///
/// **Owned shape** (`reference.is_none()`): the provisioner
/// (`Backend::Disk`) creates a standalone, **unowned** RWO PVC; the
/// renderer mounts the ready claim at `mountPath` into the
/// `replicas: 1` Deployment (`strategy: Recreate`). `size` is required
/// on this path; `mountPath`/`readOnly` stay render-side — only `size`
/// (and the tier `selector`) reach the ResourceClaim.
///
/// **Referenced shape** (`reference.is_some()`): binds an existing
/// `SharedVolume` by name (`ref` wire key). `size` is absent; the
/// actual reference-claim generation is T9/T10.
///
/// Mirrors `#DiskClaim` in `application.cue` and the `disk` block of
/// the OpenAPI v3 CRD.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiskClaim {
    /// `(disk, name)` identity. Omit → derived from the last segment of
    /// `mountPath` (`/var/lib/uploads` → `uploads`); explicit wins. A
    /// DNS-1123 label (it becomes part of the PVC name); validated by
    /// the webhook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Requested capacity (`"10Gi"`) — required for the owned shape;
    /// absent on the referenced shape (discriminated by `reference`).
    /// A Kubernetes quantity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// Name of an existing `SharedVolume` to bind (wire key `ref`).
    /// Present on the referenced shape; absent on the owned shape.
    /// `ref` is a Rust keyword, so the field is named `reference` here.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ref")]
    pub reference: Option<String>,
    /// Container mount point (`"/data"`) — required on both shapes.
    /// `rename_all = "camelCase"` already yields the `mountPath` wire
    /// key.
    pub mount_path: String,
    /// Storage class abstraction — `"local"` only at launch (the matched
    /// `disk-local` provider maps it to a concrete `storageClass`).
    /// Absent → platform default `local`. Enforced as an enum by the
    /// CRD; a plain `String` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Mount the volume read-only (default false). `rename_all =
    /// "camelCase"` already yields the `readOnly` wire key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
}

impl DiskClaim {
    /// Returns `true` when this entry references an existing
    /// `SharedVolume` (`ref` shape); `false` for the owned shape.
    /// Used to gate reference-claim generation in T9/T10.
    pub fn is_reference(&self) -> bool {
        self.reference.is_some()
    }
}

/// `Application.spec.*.needs` — an explicit closed struct (2.6b) so
/// `disk` can carry its own value type and every service key accepts a
/// scalar **or** an array of named entries. Replaces the former
/// `BTreeMap<String, ServiceNeed>` pattern-map. Mirrors the `needs`
/// block in `application.cue` and the OpenAPI v3 CRD. Unknown keys are
/// rejected at the CUE/CRD layer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Needs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pg: Option<OneOrMany<ServiceNeed>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jetstream: Option<OneOrMany<ServiceNeed>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clickhouse: Option<OneOrMany<ServiceNeed>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redis: Option<OneOrMany<ServiceNeed>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3: Option<OneOrMany<ServiceNeed>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<OneOrMany<ServiceNeed>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<OneOrMany<DiskClaim>>,
}

/// 2.12 (ADR 0046): an env value is a literal string OR a single-key
/// reference. The `#[serde(untagged)]` attribute makes the literal variant
/// deserialise from a plain JSON string, and the `Ref` variant from a
/// `{"claim":…}` or `{"secret":…}` object. Mirrors `#EnvValue` in
/// `schemas/v1alpha1/application.cue`.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum EnvValue {
    Literal(String),
    Ref(EnvRef),
}

/// Single-key discriminated reference inside an `EnvValue::Ref`.
/// `claim` resolves to a provisioned connection-Secret field;
/// `secret` resolves to an external Secret in the app namespace.
/// Mirrors `#EnvRef` in `schemas/v1alpha1/application.cue`.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EnvRef {
    /// `"<type>.<field>"` or `"<type>.<name>.<field>"` — claim-backed
    /// connection-Secret field (pg / redis field vocabulary, ADR 0046).
    Claim(String),
    /// `"<name>/<key>"` — external Secret in the app namespace.
    Secret(String),
}

/// A single flattened `needs` entry produced by [`Needs::entries`]. A
/// service entry carries `service` (a `ServiceNeed`); a disk entry
/// carries `disk` (a `DiskClaim`). `name` is the `(type, name)`
/// identity — `None` for the unnamed default of a type. Exactly one of
/// `service` / `disk` is `Some`.
#[derive(Clone, Debug, PartialEq)]
pub struct NeedEntry {
    /// `(type, name)` identity; `None` = the unnamed default for the type.
    pub name: Option<String>,
    /// Set for the six service types (pg/jetstream/clickhouse/redis/s3/
    /// notifications); `None` for a disk entry.
    pub service: Option<ServiceNeed>,
    /// Set for a `disk` entry; `None` for a service entry.
    pub disk: Option<DiskClaim>,
}

impl Needs {
    /// True when no `needs` entry is declared (every key absent / every
    /// array empty). Equivalent to `self.entries().is_empty()` — used by
    /// the Application controller's claim-gen gate.
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    /// Flatten every declared `needs` entry into `(type, NeedEntry)`
    /// pairs in a deterministic order: keys in the fixed order
    /// `pg, jetstream, clickhouse, redis, s3, notifications, disk`, and
    /// array entries by their declared index within a type. The result
    /// is byte-stable so downstream consumers (claim-gen, renderer, GC)
    /// emit byte-stable objects under server-side apply.
    pub fn entries(&self) -> Vec<(String, NeedEntry)> {
        let mut out: Vec<(String, NeedEntry)> = Vec::new();
        let service_keys: [(&str, &Option<OneOrMany<ServiceNeed>>); 6] = [
            ("pg", &self.pg),
            ("jetstream", &self.jetstream),
            ("clickhouse", &self.clickhouse),
            ("redis", &self.redis),
            ("s3", &self.s3),
            ("notifications", &self.notifications),
        ];
        for (ty, slot) in service_keys {
            if let Some(one_or_many) = slot {
                for need in one_or_many.as_slice_vec() {
                    out.push((
                        ty.to_string(),
                        NeedEntry {
                            name: need.name.clone(),
                            service: Some(need),
                            disk: None,
                        },
                    ));
                }
            }
        }
        if let Some(disks) = &self.disk {
            for disk in disks.as_slice_vec() {
                out.push((
                    "disk".to_string(),
                    NeedEntry {
                        name: disk.name.clone(),
                        service: None,
                        disk: Some(disk),
                    },
                ));
            }
        }
        out
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ImagePolicy {
    /// `"digest"` (default) resolves the tag to `repo@sha256:…`; `"off"`
    /// renders the reference verbatim and performs no registry poll.
    /// Enforced as an enum by the CRD; a plain `String` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve: Option<String>,
}

/// The repository portion of an image reference, with any tag or digest
/// stripped: `"ghcr.io/acme/app:v1"` → `"ghcr.io/acme/app"`,
/// `"ghcr.io/acme/app@sha256:…"` → `"ghcr.io/acme/app"`,
/// `"localhost:5000/app:v1"` → `"localhost:5000/app"` (a `:` before the
/// last `/` is a registry port, not a tag), `"nginx:1.27"` → `"nginx"`.
///
/// Mirrors the 2.4h split heuristic in
/// `operator-controllers-application::pull_secret::image_repo_path`
/// (kept here as a **borrowing** `&str` variant so the migration
/// classifier — which may only depend on `operator-core` — can tell a
/// repository change (gate) from a tag change (soft rollout) without a
/// crate cycle into the application controller). The stricter
/// `oci_resolve::parse_image_ref` is a full OCI grammar validator; this
/// helper only needs the tag/port split, matching `image_repo_path`.
pub fn image_repo(image: &str) -> &str {
    let no_digest = image.split('@').next().unwrap_or(image);
    let last_slash = no_digest.rfind('/');
    let tag_colon = no_digest.rfind(':');
    let cut = match (last_slash, tag_colon) {
        // A ':' after the last '/' is a tag separator; one before it is
        // a registry port (e.g. localhost:5000/app) and is NOT a tag.
        (Some(ls), Some(tc)) if tc > ls => Some(tc),
        (None, Some(tc)) => Some(tc),
        _ => None,
    };
    match cut {
        Some(c) => &no_digest[..c],
        None => no_digest,
    }
}

/// `status.image` — the resolved-image truth (ADR 0040). `tag` is the
/// reference as written in `spec`; `resolved` is `repo@sha256:…`
/// actually rendered into the Deployment; `resolvedAt` is the RFC3339
/// time of the last successful resolution.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct StatusImage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "resolvedAt"
    )]
    pub resolved_at: Option<String>,
}

/// `ImageResolved=False` when tag→digest resolution failed this cycle
/// (registry unreachable, no covering credential for a private image,
/// malformed reference) — the Deployment falls back to the verbatim
/// tag; resolution NEVER blocks the rollout (ADR 0040). `True` after a
/// successful resolution; absent when `imagePolicy.resolve: off`.
pub const COND_IMAGE_RESOLVED: &str = "ImageResolved";

/// `PublicRouteReady` — SOFT, informational condition for a `network: public`
/// Application (1.83b). NEVER gates `Ready`. `True` when every hostname is
/// under a registered `allowedDomains` zone AND the rendered HTTPRoute reports
/// `Accepted`+`ResolvedRefs`; `False` (reason `NoMatchingZone`) when a hostname
/// is under no zone (the route is STILL emitted, so adding the zone later
/// attaches it); `False` (reason `Pending`) while the Gateway settles.
pub const COND_PUBLIC_ROUTE_READY: &str = "PublicRouteReady";

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "observedGeneration"
    )]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<ApplicationCondition>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "endpointURL"
    )]
    pub endpoint_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<StatusImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Full `ApplicationSpec` snapshot the operator stamps after a
    /// successful apply (2.16b). The app-scope migration classifier diffs
    /// the incoming spec against this baseline to decide whether a change
    /// is safe or needs a gating MigrationPlan. Operator-owned; lives in
    /// `status` so Argo (which ignores status) never sees it as drift. The
    /// CUE-derived CRD carries the whole `status` node as
    /// `x-kubernetes-preserve-unknown-fields`, so this nested spec snapshot
    /// is opaque there and needs no structural schema.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastAppliedSpec"
    )]
    pub last_applied_spec: Option<ApplicationSpec>,
}

// `PHASE_AWAITING_MIGRATION_APPROVAL` + `COND_MIGRATION_PENDING` moved to
// `operator_core::migration_state` in 2.16b-sc so the Application AND
// SourceCredential controllers share one source of truth (the state machine
// that yields the pause lives there). Both are re-exported from the crate
// root, so `operator_core::{PHASE_AWAITING_MIGRATION_APPROVAL,
// COND_MIGRATION_PENDING}` imports are unchanged.

/// Reserved phase: the Application reconciler is paused awaiting a
/// generated `ResourceClaim` (from `spec.*.needs`) to be provisioned
/// (`status.ready` + `connectionSecretRef`). Phase 2.4d.
pub const PHASE_AWAITING_RESOURCE_CLAIM: &str = "AwaitingResourceClaim";

/// Condition emitted alongside `AwaitingResourceClaim`; `message`
/// carries the unready claim name(s). Phase 2.4d.
pub const COND_RESOURCE_CLAIM_PENDING: &str = "ResourceClaimPending";

/// Reserved phase: the Application reconciler is blocked because one or more
/// `env` secret refs point at a Secret / key that does not exist in the app
/// namespace. 2.12 / ADR 0046 Decision #4.
pub const PHASE_ENV_SECRET_MISSING: &str = "EnvSecretMissing";

/// k8s-style condition (mirrors `meta/v1.Condition`). Operator
/// emits `Ready` of `True` after a successful reconcile.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApplicationCondition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    #[serde(rename = "lastTransitionTime")]
    pub last_transition_time: String,
    pub reason: String,
    pub message: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "observedGeneration"
    )]
    pub observed_generation: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::Resource;
    use serde_json::json;

    #[test]
    fn status_roundtrips_last_applied_spec() {
        // 2.16b Task 7: `status.lastAppliedSpec` is a full ApplicationSpec
        // snapshot the operator stamps after a successful apply; the classifier
        // diffs against it. `ApplicationSpec` has no direct `image` — it lives
        // on `base` (ApplicationBaseSpec) — so the snapshot carries a `base`.
        let st = ApplicationStatus {
            last_applied_spec: Some(ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x:1".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let j = serde_json::to_value(&st).unwrap();
        assert_eq!(j["lastAppliedSpec"]["base"]["image"], "x:1");
        let back: ApplicationStatus = serde_json::from_value(j).unwrap();
        assert_eq!(
            back.last_applied_spec
                .unwrap()
                .base
                .unwrap()
                .image
                .as_deref(),
            Some("x:1")
        );
    }

    #[test]
    fn application_kind_and_apiversion_match_crd() {
        // The kube derive macro wires <Application as Resource>::kind()
        // and api_version() to "Application" / "apprafter.io/v1alpha1".
        assert_eq!(Application::kind(&()), "Application");
        assert_eq!(Application::api_version(&()), "apprafter.io/v1alpha1");
        assert_eq!(Application::group(&()), "apprafter.io");
        assert_eq!(Application::version(&()), "v1alpha1");
    }

    #[test]
    fn application_round_trips_through_serde_json() {
        let json_obj = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "web", "namespace": "default" },
            "spec": {
                "base": {
                    "image": "ghcr.io/acme/web:1.0",
                    "replicas": 3,
                    "expose": { "port": 8080, "network": "internal" },
                    "env": { "LOG_LEVEL": "info" }
                },
                "environments": {
                    "prod": { "replicas": 5 }
                }
            }
        });
        let app: Application = serde_json::from_value(json_obj).unwrap();

        let base = app.spec.base.as_ref().expect("base decoded");
        assert_eq!(base.image.as_deref(), Some("ghcr.io/acme/web:1.0"));
        assert_eq!(base.replicas, Some(3));
        let expose = base.expose.as_ref().expect("expose decoded");
        assert_eq!(expose.port, 8080);
        assert_eq!(expose.network.as_deref(), Some("internal"));
        let env = base.env.as_ref().expect("env decoded");
        assert_eq!(env["LOG_LEVEL"], EnvValue::Literal("info".into()));

        let envs = app
            .spec
            .environments
            .as_ref()
            .expect("environments decoded");
        let prod = envs.get("prod").expect("prod decoded");
        assert_eq!(prod.replicas, Some(5));

        // Round-trip serialize → deserialize.
        let serialized = serde_json::to_value(&app).unwrap();
        let deserialized: Application = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.spec, app.spec);
    }

    #[test]
    fn needs_round_trips_through_serde_json() {
        let json_obj = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "web", "namespace": "default" },
            "spec": {
                "base": {
                    "image": "ghcr.io/acme/web:1.0",
                    "needs": {
                        "pg": { "selector": { "tier": "integrated" } }
                    }
                },
                "environments": {
                    "prod": {
                        "needs": { "pg": { "selector": { "tier": "managed-aws" }, "size": "small" } }
                    }
                }
            }
        });
        let app: Application = serde_json::from_value(json_obj).unwrap();

        let base_needs = app
            .spec
            .base
            .as_ref()
            .unwrap()
            .needs
            .as_ref()
            .expect("base needs");
        let pg = base_needs.pg.as_ref().expect("pg need").as_slice_vec();
        assert_eq!(pg.len(), 1);
        assert_eq!(
            pg[0]
                .selector
                .as_ref()
                .and_then(|s| s.get("tier"))
                .map(String::as_str),
            Some("integrated")
        );
        assert_eq!(pg[0].size, None);

        let prod = app.spec.environments.as_ref().unwrap().get("prod").unwrap();
        let prod_pg = prod
            .needs
            .as_ref()
            .unwrap()
            .pg
            .as_ref()
            .unwrap()
            .as_slice_vec();
        assert_eq!(prod_pg[0].size.as_deref(), Some("small"));

        // Round-trip serialize → deserialize.
        let serialized = serde_json::to_value(&app).unwrap();
        let deserialized: Application = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.spec, app.spec);
    }

    #[test]
    fn needs_accepts_scalar_and_array_and_disk() {
        // Scalar form (today's single unnamed claim) still deserializes.
        let scalar: ApplicationBaseSpec = serde_json::from_value(json!({
            "image": "x",
            "needs": { "pg": { "selector": { "tier": "integrated" } } }
        }))
        .unwrap();
        let scalar_pg = scalar.needs.unwrap().pg.unwrap();
        assert!(matches!(scalar_pg, OneOrMany::One(_)));
        assert_eq!(scalar_pg.into_vec().len(), 1);

        // Array form + disk array (the 2.6b named multi-claim shape).
        let base: ApplicationBaseSpec = serde_json::from_value(json!({
            "image": "x",
            "needs": {
                "pg": [{ "name": "a" }, { "name": "b" }],
                "disk": [{ "name": "data", "size": "1Gi", "mountPath": "/data" }]
            }
        }))
        .unwrap();
        let needs = base.needs.unwrap();
        let pg = needs.pg.unwrap().into_vec();
        assert_eq!(pg.len(), 2);
        assert_eq!(pg[0].name.as_deref(), Some("a"));
        assert_eq!(pg[1].name.as_deref(), Some("b"));
        let disk = needs.disk.unwrap().into_vec();
        assert_eq!(disk.len(), 1);
        assert_eq!(disk[0].name.as_deref(), Some("data"));
        assert_eq!(disk[0].size.as_deref(), Some("1Gi"));
        assert_eq!(disk[0].mount_path, "/data");
        // Launch defaults are absent on the wire (filled by the platform).
        assert_eq!(disk[0].class, None);
        assert_eq!(disk[0].read_only, None);
    }

    #[test]
    fn disk_claim_round_trips_camel_case() {
        let dc: DiskClaim = serde_json::from_value(json!({
            "name": "uploads",
            "size": "10Gi",
            "mountPath": "/var/lib/uploads",
            "class": "local",
            "readOnly": true
        }))
        .unwrap();
        assert_eq!(dc.name.as_deref(), Some("uploads"));
        assert_eq!(dc.mount_path, "/var/lib/uploads");
        assert_eq!(dc.class.as_deref(), Some("local"));
        assert_eq!(dc.read_only, Some(true));
        // Round-trips back to camelCase wire keys.
        let v = serde_json::to_value(&dc).unwrap();
        assert!(v.get("mountPath").is_some());
        assert!(v.get("readOnly").is_some());
        assert!(v.get("mount_path").is_none());
    }

    #[test]
    fn needs_entries_flatten_is_deterministic() {
        let needs: Needs = serde_json::from_value(json!({
            "disk": [{ "name": "data", "size": "1Gi", "mountPath": "/data" }],
            "redis": { "selector": { "tier": "integrated" } },
            "pg": [{ "name": "a" }, { "name": "b" }]
        }))
        .unwrap();
        let entries = needs.entries();
        // Deterministic key order: pg, jetstream, clickhouse, redis, s3,
        // notifications, disk — array entries by index within each type.
        let shape: Vec<(String, Option<String>, bool)> = entries
            .iter()
            .map(|(ty, e)| (ty.clone(), e.name.clone(), e.disk.is_some()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("pg".to_string(), Some("a".to_string()), false),
                ("pg".to_string(), Some("b".to_string()), false),
                ("redis".to_string(), None, false),
                ("disk".to_string(), Some("data".to_string()), true),
            ]
        );
        // Service entries carry the ServiceNeed; disk entries carry DiskClaim.
        let redis = entries.iter().find(|(ty, _)| ty == "redis").unwrap();
        assert!(redis.1.service.is_some());
        assert!(redis.1.disk.is_none());
        let disk = entries.iter().find(|(ty, _)| ty == "disk").unwrap();
        assert!(disk.1.disk.is_some());
        assert!(disk.1.service.is_none());
    }

    #[test]
    fn service_need_persistent_round_trips() {
        let sn: ServiceNeed = serde_json::from_value(serde_json::json!({
            "persistent": true
        }))
        .unwrap();
        assert_eq!(sn.persistent, Some(true));
        // Absent → None (default false at the platform layer).
        let empty: ServiceNeed = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(empty.persistent, None);
    }

    #[test]
    fn image_policy_and_status_image_round_trip() {
        let json_obj = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "web", "namespace": "default" },
            "spec": { "base": {
                "image": "ghcr.io/acme/web:latest",
                "imagePolicy": { "resolve": "off" }
            }},
            "status": {
                "image": {
                    "tag": "ghcr.io/acme/web:latest",
                    "resolved": "ghcr.io/acme/web@sha256:abc",
                    "resolvedAt": "2026-06-05T00:00:00Z"
                }
            }
        });
        let app: Application = serde_json::from_value(json_obj).unwrap();
        assert_eq!(
            app.spec
                .base
                .unwrap()
                .image_policy
                .unwrap()
                .resolve
                .as_deref(),
            Some("off")
        );
        let img = app.status.unwrap().image.unwrap();
        assert_eq!(img.resolved.as_deref(), Some("ghcr.io/acme/web@sha256:abc"));
        assert_eq!(img.tag.as_deref(), Some("ghcr.io/acme/web:latest"));
    }

    #[test]
    fn status_subresource_is_optional() {
        let json_obj = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": "x" },
            "spec": {}
        });
        let app: Application = serde_json::from_value(json_obj).unwrap();
        assert!(app.status.is_none());
    }

    #[test]
    fn spec_and_status_environment_round_trip() {
        let spec: ApplicationSpec = serde_json::from_value(serde_json::json!({
            "base": { "image": "ghcr.io/acme/web:1.0" },
            "environments": { "dev": { "replicas": 1 } },
            "environment": "dev"
        }))
        .unwrap();
        assert_eq!(spec.environment.as_deref(), Some("dev"));
        let bare: ApplicationSpec =
            serde_json::from_value(serde_json::json!({ "base": { "image": "x" } })).unwrap();
        assert!(bare.environment.is_none());
        assert!(serde_json::to_value(&bare)
            .unwrap()
            .get("environment")
            .is_none());
        let status: ApplicationStatus =
            serde_json::from_value(serde_json::json!({ "phase": "Ready", "environment": "dev" }))
                .unwrap();
        assert_eq!(status.environment.as_deref(), Some("dev"));
    }

    #[test]
    fn env_value_deserialises_literal_claim_secret() {
        let j = serde_json::json!({"LOG":"info","DB":{"claim":"pg.url"},"K":{"secret":"stripe/api-key"}});
        let m: std::collections::BTreeMap<String, EnvValue> = serde_json::from_value(j).unwrap();
        assert_eq!(m["LOG"], EnvValue::Literal("info".into()));
        assert_eq!(m["DB"], EnvValue::Ref(EnvRef::Claim("pg.url".into())));
        assert_eq!(
            m["K"],
            EnvValue::Ref(EnvRef::Secret("stripe/api-key".into()))
        );
    }

    #[test]
    fn expose_hostname_round_trips_scalar_and_array_and_drops_public() {
        // network: "public" + a scalar hostname + explicit tls.
        let scalar: ApplicationExpose = serde_json::from_value(serde_json::json!({
            "port": 8080, "network": "public", "hostname": "app.demo.dev", "tls": true
        }))
        .unwrap();
        assert_eq!(scalar.network.as_deref(), Some("public"));
        assert_eq!(scalar.tls, Some(true));
        assert_eq!(
            scalar.hostname.clone().unwrap().into_vec(),
            vec!["app.demo.dev".to_string()]
        );

        // Array hostname form.
        let many: ApplicationExpose = serde_json::from_value(serde_json::json!({
            "port": 8080, "network": "public", "hostname": ["a.demo.dev", "b.demo.dev"]
        }))
        .unwrap();
        assert_eq!(
            many.hostname.unwrap().into_vec(),
            vec!["a.demo.dev".to_string(), "b.demo.dev".to_string()]
        );

        // `public` is gone: an unknown field is ignored (serde default), and the
        // struct no longer carries it. Absent hostname/tls are None, not serialized
        // (tls defaults to true at the webhook/render layer, not on the wire).
        let internal: ApplicationExpose =
            serde_json::from_value(serde_json::json!({ "port": 8080 })).unwrap();
        assert!(internal.hostname.is_none());
        assert!(internal.tls.is_none());
        let v = serde_json::to_value(&internal).unwrap();
        assert!(v.get("hostname").is_none());
        assert!(v.get("tls").is_none());
        assert!(v.get("public").is_none());
    }

    #[test]
    fn disk_claim_referenced_shape_parses() {
        let d: DiskClaim = serde_json::from_value(serde_json::json!({
            "ref": "shared", "mountPath": "/data"
        }))
        .unwrap();
        assert_eq!(d.reference.as_deref(), Some("shared"));
        assert!(d.size.is_none());
        assert!(d.is_reference());
    }

    #[test]
    fn disk_claim_owned_shape_parses() {
        let d: DiskClaim = serde_json::from_value(serde_json::json!({
            "size": "1Gi", "mountPath": "/data"
        }))
        .unwrap();
        assert_eq!(d.size.as_deref(), Some("1Gi"));
        assert!(d.reference.is_none());
        assert!(!d.is_reference());
    }

    #[test]
    fn image_repo_strips_tag_but_keeps_repo() {
        // A tag change leaves the repo untouched (soft rollout, 2.16b).
        assert_eq!(image_repo("ghcr.io/acme/api:v1"), "ghcr.io/acme/api");
        assert_eq!(image_repo("ghcr.io/acme/api:v2"), "ghcr.io/acme/api");
        assert_eq!(
            image_repo("ghcr.io/acme/api:v1"),
            image_repo("ghcr.io/acme/api:v2")
        );
        // A repo change moves the repo (gated).
        assert_ne!(
            image_repo("ghcr.io/acme/api:v1"),
            image_repo("ghcr.io/acme/other:v1")
        );
    }

    #[test]
    fn image_repo_strips_digest() {
        assert_eq!(
            image_repo("ghcr.io/acme/api@sha256:abc"),
            "ghcr.io/acme/api"
        );
    }

    #[test]
    fn image_repo_keeps_registry_port() {
        // A ':' before the last '/' is a registry port, NOT a tag.
        assert_eq!(image_repo("localhost:5000/app"), "localhost:5000/app");
        assert_eq!(image_repo("localhost:5000/app:v1"), "localhost:5000/app");
    }

    #[test]
    fn image_repo_bare_name_and_tagged_bare_name() {
        assert_eq!(image_repo("nginx"), "nginx");
        assert_eq!(image_repo("nginx:1.27"), "nginx");
    }
}
