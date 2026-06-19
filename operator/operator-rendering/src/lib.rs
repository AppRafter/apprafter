// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure renderer: `Application` -> typed k8s resources.
//!
//! v0.1.30 (sub-phase 1.9a) emits a `Deployment` always and a
//! `Service` when `spec.base.expose` is set. Both children carry
//! `ownerReferences` pointing at the Application so
//! `kubectl delete application/<name>` cascades. Per-environment
//! expansion + HTTPRoute land in v0.1.32 (sub-phase 1.9c); the SSA
//! wiring + status subresource in v0.1.31 (sub-phase 1.9b).

use std::collections::BTreeMap;

mod egress;
pub use egress::{default_target, render_egress_policy, ConnectionTarget};

mod env;
pub use env::resolve_env;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec, DeploymentStrategy};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, PersistentVolumeClaimVolumeSource, PodSpec, PodTemplateSpec,
    Service, ServicePort, ServiceSpec, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use operator_core::{Application, ApplicationBaseSpec};

/// Output of `render_application`. Always carries a Deployment;
/// `service` is `Some(...)` only when the Application sets
/// `spec.base.expose`.
#[derive(Debug, Clone)]
pub struct RenderedApplication {
    pub deployment: Deployment,
    pub service: Option<Service>,
    /// The per-Application egress `CiliumNetworkPolicy` (2.10 / ADR
    /// 0045), built via `serde_json::json!` (the external-CR pattern —
    /// no hand-rolled CNP type). `Some(...)` when the controller threads
    /// a connection-target catalog in (always at launch on a
    /// Cilium-bootstrapped cluster); `None` when `needs_targets` is
    /// absent (e.g. the base `render_application` entry point, or a tier
    /// without Cilium) → the controller applies no CNP.
    pub network_policy: Option<serde_json::Value>,
    /// The Gateway-API `HTTPRoute` (1.83b), built via `serde_json::json!`
    /// (the external-CR pattern — no hand-rolled type). `Some(...)` only when
    /// `effective.expose.network == "public"` AND a hostname is set; the
    /// controller SSA-applies it under the platform Gateway and prunes it when
    /// the app flips back to `internal`.
    pub httproute: Option<serde_json::Value>,
}

/// One ready `needs.disk` claim resolved into render input (2.6b /
/// ADR 0043). The Application controller builds a `Vec<DiskMount>` from
/// each `needs.disk` entry × the matching ready disk ResourceClaim's
/// `status.volumeClaimRef`, AFTER the readiness gate, and threads it into
/// [`render_application_for_env`]. The renderer appends one container
/// `volumeMount` + one pod `volume{persistentVolumeClaim}` per entry
/// (deterministic, sorted by `volume_name` → byte-stable SSA) and forces
/// `strategy: Recreate` when any owned disk is present. Keeping this a threaded
/// input preserves the renderer's purity (no kube client here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskMount {
    /// The k8s volume name — `disk-<name>`, where `<name>` is the disk
    /// entry's `name` or its `mountPath`-derived default. Shared by the
    /// container `volumeMount.name` and the pod `volume.name`; sorted on
    /// for deterministic ordering.
    pub volume_name: String,
    /// Container mount point (the disk entry's `mountPath`).
    pub mount_path: String,
    /// Mount the volume read-only (the disk entry's `readOnly`).
    pub read_only: bool,
    /// The standalone RWO PVC name (the claim's `status.volumeClaimRef`).
    pub pvc_name: String,
    /// Owned disks force `strategy: Recreate` (ADR 0043); reference disks
    /// (SharedVolume bind) tolerate RollingUpdate on single-node RWO.
    pub owned: bool,
}

/// Render the Application's `base` block (no environment override
/// applied). v0.1.30 entry point — keeps the simple call-site
/// shape; new code should prefer [`render_application_for_env`]
/// when the operator knows which environment it represents.
pub fn render_application(app: &Application) -> RenderedApplication {
    render_application_for_env(
        app,
        None,
        None,
        None,
        None,
        operator_core::EgressProfile::Internet,
        None,
    )
}

/// Render the Application using the merged base + environment
/// override (when `env_name` is `Some(...)` and the override exists).
///
/// `needs_secrets` maps a `(type, name)` claim identity to the name of
/// its provisioned connection Secret (the ready claim's
/// `status.connectionSecretRef`). The `name` half is `None` for the
/// unnamed/default claim of a type and `Some(<name>)` for a named array
/// entry (2.6b / ADR 0043). This map is consumed by [`resolve_env`]
/// (2.12c / ADR 0046) to expand `EnvValue::Ref(Claim(…))` entries in
/// `spec.*.env` into concrete `valueFrom.secretKeyRef` EnvVars. The
/// 2.4e implicit DSN injection is removed in 2.12c — claim-backed env
/// vars are now expressed explicitly via `spec.*.env` claim refs.
/// `None` (pre-gate / claims unready) causes Claim-ref vars to be
/// skipped (the claim-readiness gate should precede the render call).
/// Keeping the map a threaded param preserves the renderer's purity —
/// no kube client here.
///
/// `resolved_image` (2.4h-c) pins the container image to a resolved
/// `repo@sha256:...` digest when `Some(...)`; `None` renders the
/// effective spec's image verbatim (the tag/ref as authored). The
/// controller resolves the digest out-of-band and threads it in,
/// keeping this function pure (no registry calls here).
///
/// `disks` (2.6b / ADR 0043) carries the ready `needs.disk` claims as
/// render input — one [`DiskMount`] per resolved disk. The renderer appends
/// a container `volumeMount` + a pod `volume{persistentVolumeClaim}` per
/// entry (sorted by `volume_name` → byte-stable SSA) and forces
/// `strategy: Recreate` when any [`DiskMount::owned`] disk is present (an
/// owned RWO PVC cannot be held by two pods during a RollingUpdate; reference
/// disks tolerate RollingUpdate). `None`/empty leaves the Deployment strategy
/// unchanged from today (unset → apiserver default).
///
/// `egress_profile` + `needs_targets` (2.10 / ADR 0045) drive the
/// per-Application egress [`render_egress_policy`]. The controller reads
/// the cluster-wide profile from the singleton PlatformStack and resolves
/// the connection-target catalog (namespace overrides from
/// `ServiceProvider.spec.config`), then threads both in — keeping the
/// renderer pure (no kube client). `needs_targets: Some(...)` emits the
/// CNP into `RenderedApplication.network_policy` (always at launch);
/// `None` emits no CNP (the base entry point / a tier without Cilium).
/// The CNP selects the app's pods on egress and allows DNS + same-ns +
/// world (all profile-gated) + one rule per declared network need; disk
/// needs carry no network target and add no rule. `effective.needs`'
/// deterministic [`Needs::entries`] order keeps the rule list byte-stable
/// (SSA no-op).
pub fn render_application_for_env(
    app: &Application,
    env_name: Option<&str>,
    needs_secrets: Option<&BTreeMap<(String, Option<String>), String>>,
    resolved_image: Option<&str>,
    disks: Option<&[DiskMount]>,
    egress_profile: operator_core::EgressProfile,
    needs_targets: Option<&BTreeMap<String, egress::ConnectionTarget>>,
) -> RenderedApplication {
    let name = app.metadata.name.clone().unwrap_or_default();
    let namespace = app.metadata.namespace.clone();
    let owner = owner_reference(app);
    let labels = make_labels(&name, env_name);
    let effective = effective_spec(app, env_name);

    let deployment = render_deployment(
        &name,
        namespace.as_deref(),
        &owner,
        &labels,
        &effective,
        needs_secrets,
        resolved_image,
        disks,
    );
    let service = effective
        .expose
        .as_ref()
        .map(|expose| render_service(&name, namespace.as_deref(), &owner, &labels, expose.port));

    // 2.10 (ADR 0045): one egress CNP per Application, named after the
    // env-aware Deployment (`deployment.metadata.name`) so per-env
    // children never collide. Built only when the controller threads a
    // connection-target catalog in; the rule order follows
    // `effective.needs.entries()` (deterministic → byte-stable SSA).
    let rendered_name = deployment
        .metadata
        .name
        .clone()
        .unwrap_or_else(|| name.clone());
    let network_policy = needs_targets.map(|targets| {
        let needs_entries = effective
            .needs
            .as_ref()
            .map(|n| n.entries())
            .unwrap_or_default();
        egress::render_egress_policy(
            &name,
            &rendered_name,
            &labels,
            &needs_entries,
            egress_profile,
            targets,
        )
    });

    // 1.83b: emit an HTTPRoute only when the app is public AND names at
    // least one hostname (a hostname-less public app would attach to every
    // listener — the webhook rejects it; the renderer is defensive). The
    // child + Service are both named `name`; the route backend-refs the
    // Service on port 80.
    let httproute = effective
        .expose
        .as_ref()
        .filter(|e| e.network.as_deref() == Some("public"))
        .and_then(|e| e.hostname.as_ref())
        .map(|h| h.as_slice_vec())
        .filter(|hosts| !hosts.is_empty())
        .map(|hosts| render_httproute(&name, &labels, &hosts, &name));

    RenderedApplication {
        deployment,
        service,
        network_policy,
        httproute,
    }
}

/// Compute the effective spec — `base` unified with the named
/// environment override (when present). Fields set in the override
/// replace base fields; the env map merges with override-wins on
/// conflict. v1alpha1 doesn't include CUE-only constructs, so this
/// pure-Rust merge is functionally equivalent to CUE unification
/// for our schema.
pub fn effective_spec(app: &Application, env_name: Option<&str>) -> ApplicationBaseSpec {
    let mut effective = app.spec.base.clone().unwrap_or_default();
    let Some(name) = env_name else {
        return effective;
    };
    let Some(env_override) = app.spec.environments.as_ref().and_then(|m| m.get(name)) else {
        return effective;
    };
    if env_override.image.is_some() {
        effective.image = env_override.image.clone();
    }
    if env_override.replicas.is_some() {
        effective.replicas = env_override.replicas;
    }
    if env_override.expose.is_some() {
        effective.expose = env_override.expose.clone();
    }
    if env_override.image_policy.is_some() {
        effective.image_policy = env_override.image_policy.clone();
    }
    if let Some(env_env) = &env_override.env {
        let mut merged = effective.env.unwrap_or_default();
        for (k, v) in env_env {
            merged.insert(k.clone(), v.clone());
        }
        effective.env = Some(merged);
    }
    // 2.4d S0: per-key whole-object replace for `needs` — the env
    // entry replaces the base entry wholesale per key (mirrors how
    // `expose` replaces wholesale); base-only need keys survive.
    // 2.6b: `needs` is now a closed struct, so the per-key merge is an
    // explicit `Option`-replace per field (override-wins) rather than a
    // map insert.
    if let Some(env_needs) = &env_override.needs {
        let mut merged = effective.needs.unwrap_or_default();
        if env_needs.pg.is_some() {
            merged.pg = env_needs.pg.clone();
        }
        if env_needs.jetstream.is_some() {
            merged.jetstream = env_needs.jetstream.clone();
        }
        if env_needs.clickhouse.is_some() {
            merged.clickhouse = env_needs.clickhouse.clone();
        }
        if env_needs.redis.is_some() {
            merged.redis = env_needs.redis.clone();
        }
        if env_needs.s3.is_some() {
            merged.s3 = env_needs.s3.clone();
        }
        if env_needs.notifications.is_some() {
            merged.notifications = env_needs.notifications.clone();
        }
        if env_needs.disk.is_some() {
            merged.disk = env_needs.disk.clone();
        }
        effective.needs = Some(merged);
    }
    effective
}

/// Build the `OwnerReference` back to the owning Application
/// (`controller: true`, `blockOwnerDeletion: true`) used on every child
/// the renderer emits (Deployment, Service). Public so the controller can
/// reuse the exact same shape when SSA-applying the egress CNP (2.10 / ADR
/// 0045) — the CNP must carry the identical Application ownerRef so it
/// cascades on Application delete, just like the Deployment/Service.
pub fn owner_reference(app: &Application) -> OwnerReference {
    OwnerReference {
        api_version: "apprafter.io/v1alpha1".to_string(),
        kind: "Application".to_string(),
        name: app.metadata.name.clone().unwrap_or_default(),
        uid: app.metadata.uid.clone().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

fn make_labels(name: &str, env_name: Option<&str>) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/name".to_string(), name.to_string());
    labels.insert(
        "app.kubernetes.io/managed-by".to_string(),
        "apprafter-operator".to_string(),
    );
    labels.insert("apprafter".to_string(), "true".to_string());
    // 2.9 (ADR 0044): group an app's env-deployments + surface the env.
    // Reuses the established apprafter.io/application +
    // apprafter.io/environment keys.
    labels.insert("apprafter.io/application".to_string(), name.to_string());
    if let Some(env) = env_name.filter(|s| !s.is_empty()) {
        labels.insert("apprafter.io/environment".to_string(), env.to_string());
    }
    labels
}

// The render inputs (env/needs/image/disk) are each an independent,
// orthogonal threaded param kept separate for the renderer's purity; a
// bundling struct would only obscure the call site. Mirrors the
// provisioner's `reconcile.rs` allow.
#[allow(clippy::too_many_arguments)]
fn render_deployment(
    name: &str,
    namespace: Option<&str>,
    owner: &OwnerReference,
    labels: &BTreeMap<String, String>,
    spec: &ApplicationBaseSpec,
    needs_secrets: Option<&BTreeMap<(String, Option<String>), String>>,
    resolved_image: Option<&str>,
    disks: Option<&[DiskMount]>,
) -> Deployment {
    let replicas = spec.replicas.unwrap_or(1);
    let image = resolved_image
        .map(String::from)
        .unwrap_or_else(|| spec.image.clone().unwrap_or_default());
    let container_port = spec.expose.as_ref().map(|e| ContainerPort {
        container_port: e.port,
        protocol: Some("TCP".to_string()),
        ..Default::default()
    });

    // 2.12c (ADR 0046): resolve spec.env via the pure `resolve_env`
    // function. `Literal` values become plain `EnvVar { value }` entries;
    // `Ref(Claim(…))` entries are resolved through `needs_secrets` into
    // `valueFrom.secretKeyRef` EnvVars; `Ref(Secret(…))` entries are
    // resolved directly into `valueFrom.secretKeyRef` EnvVars. The
    // 2.4e/2.6 implicit DSN injection (DATABASE_URL / REDIS_URL etc.)
    // is removed — claim-backed env vars must now be declared explicitly
    // in `spec.*.env`. Deterministic BTreeMap order keeps the output
    // byte-stable (SSA no-op across reconciles).
    let empty_secrets = BTreeMap::new();
    let resolved_secrets = needs_secrets.unwrap_or(&empty_secrets);
    let env_vars: Vec<EnvVar> = spec
        .env
        .as_ref()
        .map(|env| resolve_env(env, resolved_secrets))
        .unwrap_or_default();

    // 2.6b (ADR 0043): mount ready disk claims into the pod. Each
    // DiskMount contributes a container `volumeMount` + a pod
    // `volume{persistentVolumeClaim}`, sorted by `volume_name` so the
    // rendered Deployment is byte-stable across reconciles (SSA no-op).
    // When ANY owned disk is present the strategy is forced to `Recreate` —
    // an owned RWO PVC cannot be held by the old + new pod simultaneously
    // during a RollingUpdate; reference disks (SharedVolume bind) tolerate
    // RollingUpdate on single-node RWO. With no owned disk the strategy stays
    // unset (apiserver default RollingUpdate), unchanged from today.
    let mut sorted_disks: Vec<&DiskMount> = disks.unwrap_or(&[]).iter().collect();
    sorted_disks.sort_by(|a, b| a.volume_name.cmp(&b.volume_name));
    let volume_mounts: Vec<VolumeMount> = sorted_disks
        .iter()
        .map(|d| VolumeMount {
            name: d.volume_name.clone(),
            mount_path: d.mount_path.clone(),
            read_only: Some(d.read_only),
            ..Default::default()
        })
        .collect();
    let volumes: Vec<Volume> = sorted_disks
        .iter()
        .map(|d| Volume {
            name: d.volume_name.clone(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: d.pvc_name.clone(),
                read_only: Some(d.read_only),
            }),
            ..Default::default()
        })
        .collect();
    let force_recreate = sorted_disks.iter().any(|d| d.owned);
    let strategy = if force_recreate {
        Some(DeploymentStrategy {
            type_: Some("Recreate".to_string()),
            rolling_update: None,
        })
    } else {
        None
    };

    let container = Container {
        name: name.to_string(),
        image: Some(image),
        env: if env_vars.is_empty() {
            None
        } else {
            Some(env_vars)
        },
        ports: container_port.map(|p| vec![p]),
        volume_mounts: if volume_mounts.is_empty() {
            None
        } else {
            Some(volume_mounts)
        },
        ..Default::default()
    };

    Deployment {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: namespace.map(String::from),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner.clone()]),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(replicas),
            strategy,
            selector: LabelSelector {
                // STABLE MINIMAL selector. `spec.selector` is IMMUTABLE, so
                // it must be a fixed subset that never grows with the
                // pod/metadata label-set — walk-found: 2.9 widened
                // `make_labels` (added apprafter.io/application +
                // .../environment) and, because the selector reused the FULL
                // set, every Deployment created under the prior set 422'd on
                // every reconcile ("spec.selector: field is immutable") and
                // could never be updated again. `apprafter.io/application`
                // equals the Deployment name, so it is unique within the
                // namespace (two Deployments can't share a name) — a single
                // label is sufficient AND stable. The pod template + metadata
                // keep the full label-set below.
                match_labels: Some(BTreeMap::from([(
                    "apprafter.io/application".to_string(),
                    name.to_string(),
                )])),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels.clone()),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![container],
                    volumes: if volumes.is_empty() {
                        None
                    } else {
                        Some(volumes)
                    },
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        status: None,
    }
}

fn render_service(
    name: &str,
    namespace: Option<&str>,
    owner: &OwnerReference,
    labels: &BTreeMap<String, String>,
    port: i32,
) -> Service {
    Service {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: namespace.map(String::from),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner.clone()]),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            type_: Some("ClusterIP".to_string()),
            selector: Some(labels.clone()),
            ports: Some(vec![ServicePort {
                name: Some("http".to_string()),
                port: 80,
                target_port: Some(IntOrString::Int(port)),
                protocol: Some("TCP".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        status: None,
    }
}

/// Build the per-Application Gateway-API `HTTPRoute` (1.83b) as a
/// `serde_json::Value` — the external-CR pattern (`render_egress_policy`
/// precedent; the gateway-api crate is not a dep). `name` is the app's
/// child name (== the CR metadata name, already env-distinct); `service_name`
/// is the backend Service's name (same as `name`). The route:
///   - `parentRefs[0]` → the `platform` Gateway in `apprafter-system`,
///     `port: 443` (attaches to the HTTPS listeners ONLY — never `:80`, so
///     1.83a's catch-all http→https redirect is preserved — and NO
///     `sectionName`, so Gateway API hostname-matches apex/wildcard
///     zone-agnostically);
///   - `hostnames` → the normalized `Vec<String>`;
///   - one rule, `backendRefs` → the Service on port `80` (the Service maps
///     `80` → `targetPort: expose.port`), no `matches` (apiserver defaults
///     match-all `/`).
///
/// The controller injects `metadata.namespace` + `ownerReferences` at apply.
fn render_httproute(
    name: &str,
    labels: &BTreeMap<String, String>,
    hostnames: &[String],
    service_name: &str,
) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "gateway.networking.k8s.io/v1",
        "kind": "HTTPRoute",
        "metadata": {
            "name": name,
            "labels": labels,
        },
        "spec": {
            "parentRefs": [{
                "name": "platform",
                "namespace": "apprafter-system",
                "port": 443
            }],
            "hostnames": hostnames,
            "rules": [{
                "backendRefs": [{ "name": service_name, "port": 80 }]
            }]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{ApplicationExpose, ApplicationSpec, EnvValue};

    fn make_app_with_uid(
        spec: ApplicationSpec,
        name: &str,
        namespace: &str,
        uid: &str,
    ) -> Application {
        let mut app = Application::new(name, spec);
        app.metadata.namespace = Some(namespace.to_string());
        app.metadata.uid = Some(uid.to_string());
        app
    }

    #[test]
    fn deployment_replicas_default_to_one_when_unset() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("ghcr.io/acme/x:1.0".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "abc-123",
        );
        let r = render_application(&app);
        assert_eq!(r.deployment.spec.as_ref().unwrap().replicas, Some(1));
    }

    #[test]
    fn deployment_replicas_use_base_value_when_set() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    replicas: Some(5),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app);
        assert_eq!(r.deployment.spec.as_ref().unwrap().replicas, Some(5));
    }

    #[test]
    fn deployment_container_image_matches_base_image() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("ghcr.io/acme/web:1.0".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app);
        let containers = &r
            .deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers;
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].image.as_deref(), Some("ghcr.io/acme/web:1.0"));
    }

    #[test]
    fn deployment_env_vars_match_base_env_in_btreemap_order() {
        let mut env = BTreeMap::new();
        env.insert(
            "LOG_LEVEL".to_string(),
            EnvValue::Literal("info".to_string()),
        );
        env.insert("ALPHA".to_string(), EnvValue::Literal("1".to_string()));
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    env: Some(env),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app);
        let envs = r
            .deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .env
            .as_ref()
            .expect("env present");
        // BTreeMap iteration → ALPHA before LOG_LEVEL.
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].name, "ALPHA");
        assert_eq!(envs[0].value.as_deref(), Some("1"));
        assert_eq!(envs[1].name, "LOG_LEVEL");
        assert_eq!(envs[1].value.as_deref(), Some("info"));
    }

    #[test]
    fn deployment_container_port_matches_expose_port() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    expose: Some(ApplicationExpose {
                        port: 8080,
                        network: None,
                        hostname: None,
                        tls: None,
                    }),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app);
        let ports = r
            .deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .ports
            .as_ref()
            .expect("ports present");
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].container_port, 8080);
    }

    #[test]
    fn no_service_when_expose_unset() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app);
        assert!(r.service.is_none());
    }

    #[test]
    fn service_has_clusterip_type_and_target_port_matches_expose() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    expose: Some(ApplicationExpose {
                        port: 9000,
                        network: None,
                        hostname: None,
                        tls: None,
                    }),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app);
        let svc = r.service.expect("service rendered");
        let spec = svc.spec.expect("svc spec");
        assert_eq!(spec.type_.as_deref(), Some("ClusterIP"));
        let ports = spec.ports.expect("svc ports");
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 80);
        assert_eq!(ports[0].target_port, Some(IntOrString::Int(9000)));
    }

    // ---- 1.83b: HTTPRoute emission ----

    #[test]
    fn no_httproute_when_network_not_public() {
        // internal (default) + a hostname is irrelevant → no route.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".into()),
                    expose: Some(ApplicationExpose {
                        port: 8080,
                        network: Some("internal".into()),
                        hostname: None,
                        tls: None,
                    }),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        assert!(render_application(&app).httproute.is_none());
    }

    #[test]
    fn httproute_emitted_for_public_with_scalar_hostname() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".into()),
                    expose: Some(ApplicationExpose {
                        port: 8080,
                        network: Some("public".into()),
                        hostname: Some(operator_core::OneOrMany::One("app.demo.dev".into())),
                        tls: None,
                    }),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app).httproute.expect("route rendered");
        assert_eq!(r["apiVersion"], "gateway.networking.k8s.io/v1");
        assert_eq!(r["kind"], "HTTPRoute");
        assert_eq!(r["metadata"]["name"], "web");
        // parentRef attaches to the HTTPS listeners ONLY (port 443), so http
        // still hits 1.83a's catch-all 301 redirect; no sectionName (zone-agnostic).
        let parent = &r["spec"]["parentRefs"][0];
        assert_eq!(parent["name"], "platform");
        assert_eq!(parent["namespace"], "apprafter-system");
        assert_eq!(parent["port"], 443);
        assert!(parent.get("sectionName").is_none());
        // hostnames normalized to a list.
        assert_eq!(r["spec"]["hostnames"], serde_json::json!(["app.demo.dev"]));
        // backendRef → the Service (named after the app) on port 80 (the Service
        // port; render_service maps 80 → targetPort expose.port). No `matches`.
        let backend = &r["spec"]["rules"][0]["backendRefs"][0];
        assert_eq!(backend["name"], "web");
        assert_eq!(backend["port"], 80);
        assert!(r["spec"]["rules"][0].get("matches").is_none());
    }

    #[test]
    fn httproute_hostnames_normalize_array_form() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".into()),
                    expose: Some(ApplicationExpose {
                        port: 8080,
                        network: Some("public".into()),
                        hostname: Some(operator_core::OneOrMany::Many(vec![
                            "a.demo.dev".into(),
                            "b.demo.dev".into(),
                        ])),
                        tls: None,
                    }),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app).httproute.expect("route rendered");
        assert_eq!(
            r["spec"]["hostnames"],
            serde_json::json!(["a.demo.dev", "b.demo.dev"])
        );
    }

    #[test]
    fn no_httproute_when_public_but_hostname_absent() {
        // Defensive: the webhook rejects this, but the renderer must not emit a
        // hostname-less route (it would attach to every listener).
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".into()),
                    expose: Some(ApplicationExpose {
                        port: 8080,
                        network: Some("public".into()),
                        hostname: None,
                        tls: None,
                    }),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        assert!(render_application(&app).httproute.is_none());
    }

    #[test]
    fn owner_reference_points_back_to_application_with_uid() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    expose: Some(ApplicationExpose {
                        port: 8080,
                        network: None,
                        hostname: None,
                        tls: None,
                    }),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "abc-123-uid",
        );
        let r = render_application(&app);
        let dep_owners = r.deployment.metadata.owner_references.unwrap();
        assert_eq!(dep_owners.len(), 1);
        assert_eq!(dep_owners[0].api_version, "apprafter.io/v1alpha1");
        assert_eq!(dep_owners[0].kind, "Application");
        assert_eq!(dep_owners[0].name, "web");
        assert_eq!(dep_owners[0].uid, "abc-123-uid");
        assert_eq!(dep_owners[0].controller, Some(true));
        assert_eq!(dep_owners[0].block_owner_deletion, Some(true));
        // Service mirrors the same owner ref.
        let svc_owners = r.service.unwrap().metadata.owner_references.unwrap();
        assert_eq!(svc_owners[0].uid, "abc-123-uid");
    }

    #[test]
    fn labels_include_app_kubernetes_io_name_managed_by_and_apprafter() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app);
        let labels = r.deployment.metadata.labels.unwrap();
        assert_eq!(
            labels.get("app.kubernetes.io/name").map(String::as_str),
            Some("web")
        );
        assert_eq!(
            labels
                .get("app.kubernetes.io/managed-by")
                .map(String::as_str),
            Some("apprafter-operator")
        );
        assert_eq!(labels.get("apprafter").map(String::as_str), Some("true"));
    }

    #[test]
    fn deployment_selector_is_minimal_and_env_stable() {
        // Regression (walk-found): `spec.selector` must be the STABLE
        // minimal `{apprafter.io/application: <name>}` — NOT the full
        // label-set — or a later label-set widening (as 2.9 did) wedges
        // every existing Deployment on the immutable-selector apiserver
        // rule. The env key in particular must NOT be a selector key.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "u",
        );
        let r = render_application(&app);
        let sel = r
            .deployment
            .spec
            .as_ref()
            .unwrap()
            .selector
            .match_labels
            .clone()
            .unwrap();
        assert_eq!(
            sel,
            BTreeMap::from([("apprafter.io/application".to_string(), "web".to_string())])
        );

        // Env-scoped render: the selector is IDENTICAL (env is not a
        // selector key — the stability invariant), even though the pod
        // labels below DO carry apprafter.io/environment.
        let mut envapp = Application::new("web", ApplicationSpec::default());
        envapp.metadata.namespace = Some("web-dev".into());
        envapp.spec.base = Some(ApplicationBaseSpec {
            image: Some("x".into()),
            ..Default::default()
        });
        let renv = render_application_for_env(
            &envapp,
            Some("dev"),
            None,
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let sel_env = renv
            .deployment
            .spec
            .as_ref()
            .unwrap()
            .selector
            .match_labels
            .clone()
            .unwrap();
        assert_eq!(
            sel_env,
            BTreeMap::from([("apprafter.io/application".to_string(), "web".to_string())])
        );
    }

    #[test]
    fn rendered_children_carry_app_and_environment_labels() {
        // 2.9 (ADR 0044): rendered children carry
        // `apprafter.io/application` (always) + `apprafter.io/environment`
        // (only when the render is env-scoped) so an app's per-env
        // deployments are groupable and the active env is surfaced.
        let mut app = Application::new("web", ApplicationSpec::default());
        app.metadata.namespace = Some("web-dev".into());
        app.spec.base = Some(ApplicationBaseSpec {
            image: Some("x".into()),
            ..Default::default()
        });
        let rendered = render_application_for_env(
            &app,
            Some("dev"),
            None,
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let labels = rendered
            .deployment
            .metadata
            .labels
            .clone()
            .unwrap_or_default();
        assert_eq!(
            labels.get("apprafter.io/application").map(String::as_str),
            Some("web")
        );
        assert_eq!(
            labels.get("apprafter.io/environment").map(String::as_str),
            Some("dev")
        );
        // Base-only render (env=None) carries the application label but
        // no environment label.
        let base = render_application_for_env(
            &app,
            None,
            None,
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let base_labels = base.deployment.metadata.labels.clone().unwrap_or_default();
        assert_eq!(
            base_labels
                .get("apprafter.io/application")
                .map(String::as_str),
            Some("web")
        );
        assert!(!base_labels.contains_key("apprafter.io/environment"));
    }

    fn make_app_with_envs(
        base: ApplicationBaseSpec,
        envs: BTreeMap<String, ApplicationBaseSpec>,
    ) -> Application {
        let mut app = Application::new(
            "web",
            ApplicationSpec {
                base: Some(base),
                environments: Some(envs),
                environment: None,
            },
        );
        app.metadata.namespace = Some("default".to_string());
        app.metadata.uid = Some("uid".to_string());
        app
    }

    #[test]
    fn effective_spec_returns_base_when_env_name_absent() {
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("base".into()),
                replicas: Some(2),
                ..Default::default()
            },
            BTreeMap::new(),
        );
        let s = effective_spec(&app, None);
        assert_eq!(s.image.as_deref(), Some("base"));
        assert_eq!(s.replicas, Some(2));
    }

    #[test]
    fn effective_spec_returns_base_when_env_not_in_environments_map() {
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("base".into()),
                ..Default::default()
            },
            BTreeMap::new(),
        );
        let s = effective_spec(&app, Some("prod"));
        assert_eq!(s.image.as_deref(), Some("base"));
    }

    #[test]
    fn effective_spec_env_override_replaces_image_and_replicas() {
        let mut envs = BTreeMap::new();
        envs.insert(
            "prod".to_string(),
            ApplicationBaseSpec {
                image: Some("prod-image".into()),
                replicas: Some(5),
                ..Default::default()
            },
        );
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("base-image".into()),
                replicas: Some(1),
                ..Default::default()
            },
            envs,
        );
        let s = effective_spec(&app, Some("prod"));
        assert_eq!(s.image.as_deref(), Some("prod-image"));
        assert_eq!(s.replicas, Some(5));
    }

    #[test]
    fn effective_spec_env_override_replaces_expose_block() {
        let mut envs = BTreeMap::new();
        envs.insert(
            "prod".to_string(),
            ApplicationBaseSpec {
                expose: Some(ApplicationExpose {
                    port: 9000,
                    network: Some("public".into()),
                    hostname: Some(operator_core::OneOrMany::One("app.demo.dev".into())),
                    tls: None,
                }),
                ..Default::default()
            },
        );
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("base".into()),
                expose: Some(ApplicationExpose {
                    port: 8080,
                    network: None,
                    hostname: None,
                    tls: None,
                }),
                ..Default::default()
            },
            envs,
        );
        let s = effective_spec(&app, Some("prod"));
        let expose = s.expose.expect("expose decoded");
        assert_eq!(expose.port, 9000);
        assert_eq!(
            expose.hostname.unwrap().into_vec(),
            vec!["app.demo.dev".to_string()]
        );
        assert_eq!(expose.network.as_deref(), Some("public"));
    }

    #[test]
    fn effective_spec_env_override_replaces_image_policy() {
        use operator_core::ImagePolicy;
        // 2.4h Fix A: a per-environment `imagePolicy.resolve` override
        // must replace the base policy (REPLACEMENT semantics, like
        // image/replicas/expose) — otherwise an env-scoped opt-out is
        // silently ignored.
        let mut envs = BTreeMap::new();
        envs.insert(
            "prod".to_string(),
            ApplicationBaseSpec {
                image_policy: Some(ImagePolicy {
                    resolve: Some("off".into()),
                }),
                ..Default::default()
            },
        );
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("base".into()),
                image_policy: Some(ImagePolicy {
                    resolve: Some("digest".into()),
                }),
                ..Default::default()
            },
            envs,
        );
        let s = effective_spec(&app, Some("prod"));
        assert_eq!(
            s.image_policy.and_then(|p| p.resolve).as_deref(),
            Some("off")
        );
    }

    #[test]
    fn effective_spec_env_override_env_merges_with_override_wins_on_conflict() {
        let mut base_env = BTreeMap::new();
        base_env.insert(
            "LOG_LEVEL".to_string(),
            EnvValue::Literal("info".to_string()),
        );
        base_env.insert("REGION".to_string(), EnvValue::Literal("eu".to_string()));
        let mut prod_env = BTreeMap::new();
        prod_env.insert(
            "LOG_LEVEL".to_string(),
            EnvValue::Literal("warn".to_string()),
        );
        prod_env.insert("PROD_FLAG".to_string(), EnvValue::Literal("1".to_string()));
        let mut envs = BTreeMap::new();
        envs.insert(
            "prod".to_string(),
            ApplicationBaseSpec {
                env: Some(prod_env),
                ..Default::default()
            },
        );
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("x".into()),
                env: Some(base_env),
                ..Default::default()
            },
            envs,
        );
        let s = effective_spec(&app, Some("prod"));
        let env = s.env.expect("env decoded");
        // override wins:
        assert_eq!(env["LOG_LEVEL"], EnvValue::Literal("warn".into()));
        // base survives:
        assert_eq!(env["REGION"], EnvValue::Literal("eu".into()));
        // override-only:
        assert_eq!(env["PROD_FLAG"], EnvValue::Literal("1".into()));
    }

    #[test]
    fn effective_spec_env_override_replaces_needs_per_key_and_keeps_base_only_needs() {
        // 2.4d S0: env-scoped `needs` must merge per-key (the env
        // entry replaces the base entry WHOLESALE for that key —
        // mirrors how `expose` replaces), and base-only need keys
        // survive when the env omits them. 2.6b: `needs` is a closed
        // struct, so per-key entries are typed fields.
        use operator_core::{Needs, OneOrMany, ServiceNeed};
        let base_needs = Needs {
            pg: Some(OneOrMany::One(ServiceNeed {
                name: None,
                selector: None,
                size: Some("small".into()),
                persistent: None,
            })),
            redis: Some(OneOrMany::One(ServiceNeed {
                name: None,
                selector: None,
                size: Some("nano".into()),
                persistent: None,
            })),
            ..Default::default()
        };
        let prod_needs = Needs {
            pg: Some(OneOrMany::One(ServiceNeed {
                name: None,
                selector: Some(BTreeMap::from([(
                    "tier".to_string(),
                    "managed-aws".to_string(),
                )])),
                size: None,
                persistent: None,
            })),
            ..Default::default()
        };
        let mut envs = BTreeMap::new();
        envs.insert(
            "prod".to_string(),
            ApplicationBaseSpec {
                needs: Some(prod_needs),
                ..Default::default()
            },
        );
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("x".into()),
                needs: Some(base_needs),
                ..Default::default()
            },
            envs,
        );
        let s = effective_spec(&app, Some("prod"));
        let needs = s.needs.expect("needs merged");
        // pg replaced wholesale: env selector wins, base size DROPPED.
        let pg = needs.pg.expect("pg need").into_vec();
        assert_eq!(pg.len(), 1);
        assert_eq!(
            pg[0]
                .selector
                .as_ref()
                .and_then(|m| m.get("tier"))
                .map(String::as_str),
            Some("managed-aws")
        );
        assert_eq!(pg[0].size, None, "base size must be dropped on env replace");
        // base-only redis survives.
        let redis = needs.redis.expect("base-only redis survives").into_vec();
        assert_eq!(redis[0].size.as_deref(), Some("nano"));
    }

    #[test]
    fn render_application_for_env_uses_env_image() {
        let mut envs = BTreeMap::new();
        envs.insert(
            "prod".to_string(),
            ApplicationBaseSpec {
                image: Some("ghcr.io/acme/web:prod".into()),
                ..Default::default()
            },
        );
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("ghcr.io/acme/web:dev".into()),
                ..Default::default()
            },
            envs,
        );
        let r = render_application_for_env(
            &app,
            Some("prod"),
            None,
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        assert_eq!(
            r.deployment.spec.unwrap().template.spec.unwrap().containers[0]
                .image
                .as_deref(),
            Some("ghcr.io/acme/web:prod")
        );
    }

    #[test]
    fn render_application_no_env_falls_back_to_base() {
        let mut envs = BTreeMap::new();
        envs.insert(
            "prod".to_string(),
            ApplicationBaseSpec {
                image: Some("ghcr.io/acme/web:prod".into()),
                ..Default::default()
            },
        );
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("ghcr.io/acme/web:dev".into()),
                ..Default::default()
            },
            envs,
        );
        let r = render_application(&app); // no env → base.image wins
        assert_eq!(
            r.deployment.spec.unwrap().template.spec.unwrap().containers[0]
                .image
                .as_deref(),
            Some("ghcr.io/acme/web:dev")
        );
    }

    // ---- 2.12c: env claim/secret refs rendered via resolve_env ----
    // (2.4e auto-inject removed — claim-backed env vars must be declared
    // explicitly in spec.*.env as EnvValue::Ref(Claim(…)) entries.)

    use operator_core::{EnvRef, Needs, OneOrMany, ServiceNeed};

    /// Helper: build a base with a single (unnamed, scalar) need of the
    /// given type (no selector/size). 2.6b: `needs` is a closed struct.
    fn base_with_need(image: &str, need_type: &str) -> ApplicationBaseSpec {
        let one = Some(OneOrMany::One(ServiceNeed::default()));
        let mut needs = Needs::default();
        match need_type {
            "pg" => needs.pg = one,
            "jetstream" => needs.jetstream = one,
            "clickhouse" => needs.clickhouse = one,
            "redis" => needs.redis = one,
            "s3" => needs.s3 = one,
            "notifications" => needs.notifications = one,
            other => panic!("base_with_need: unknown service type {other}"),
        }
        ApplicationBaseSpec {
            image: Some(image.to_string()),
            needs: Some(needs),
            ..Default::default()
        }
    }

    /// Helper: extract the rendered container's env vars (or None).
    fn container_env(r: &RenderedApplication) -> Option<Vec<EnvVar>> {
        r.deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .env
            .clone()
    }

    #[test]
    fn pg_need_without_env_ref_produces_no_env_vars() {
        // 2.12c: the 2.4e implicit DSN injection is removed. An app that
        // declares needs.pg but has NO spec.env claim-ref entries gets NO
        // DATABASE_URL injected — it must be wired explicitly.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base_with_need("ghcr.io/acme/web:1.0", "pg")),
                environments: None,
                environment: None,
            },
            "parser",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([(("pg".to_string(), None), "parser-pg-conn".to_string())]);
        let r = render_application_for_env(
            &app,
            None,
            Some(&secrets),
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        // No spec.env → no env vars regardless of the secrets map.
        assert!(
            container_env(&r).is_none(),
            "no auto-inject: needs.pg without an env ref must not produce DATABASE_URL"
        );
    }

    #[test]
    fn needs_present_but_no_env_refs_and_no_secrets_map_yields_no_env() {
        // The 2.4d "resumes without DSN" contract still holds in 2.12c:
        // a need is declared but no resolved secret map is threaded in
        // AND no env refs → no env vars.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base_with_need("ghcr.io/acme/web:1.0", "pg")),
                environments: None,
                environment: None,
            },
            "parser",
            "demo",
            "uid-1",
        );
        let r = render_application_for_env(
            &app,
            None,
            None,
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let envs = container_env(&r);
        assert!(envs.is_none(), "no env when no spec.env and no secrets map");
    }

    #[test]
    fn claim_ref_in_env_resolves_to_secret_key_ref() {
        // 2.12c: an explicit `spec.env.DB = {claim: "pg.url"}` entry is
        // resolved to a valueFrom.secretKeyRef against the connection
        // Secret threaded in via needs_secrets.
        let mut env = BTreeMap::new();
        env.insert(
            "DB".to_string(),
            EnvValue::Ref(EnvRef::Claim("pg.url".to_string())),
        );
        let mut base = base_with_need("ghcr.io/acme/web:1.0", "pg");
        base.env = Some(env);
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base),
                environments: None,
                environment: None,
            },
            "parser",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([(("pg".to_string(), None), "parser-pg-conn".to_string())]);
        let r = render_application_for_env(
            &app,
            None,
            Some(&secrets),
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let envs = container_env(&r).expect("env present");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "DB");
        assert!(envs[0].value.is_none());
        let kr = envs[0]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(kr.name, "parser-pg-conn");
        assert_eq!(kr.key, "url");
        assert_eq!(kr.optional, Some(false));
    }

    #[test]
    fn literal_env_coexists_with_claim_ref_in_btreemap_order() {
        // 2.12c: literal env + a Claim ref coexist; the output is in
        // BTreeMap (lexicographic) order. No implicit DATABASE_URL.
        let mut env = BTreeMap::new();
        env.insert(
            "LOG_LEVEL".to_string(),
            EnvValue::Literal("info".to_string()),
        );
        env.insert(
            "DB".to_string(),
            EnvValue::Ref(EnvRef::Claim("pg.url".to_string())),
        );
        let mut base = base_with_need("ghcr.io/acme/web:1.0", "pg");
        base.env = Some(env);
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base),
                environments: None,
                environment: None,
            },
            "parser",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([(("pg".to_string(), None), "parser-pg-conn".to_string())]);
        let r = render_application_for_env(
            &app,
            None,
            Some(&secrets),
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let envs = container_env(&r).expect("env present");
        // BTreeMap order: DB < LOG_LEVEL
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].name, "DB");
        assert!(envs[0].value_from.is_some());
        assert_eq!(envs[1].name, "LOG_LEVEL");
        assert_eq!(envs[1].value.as_deref(), Some("info"));
    }

    #[test]
    fn needs_pg_with_no_env_ref_gets_no_database_url_auto_inject_removed() {
        // 2.12c regression-guard: an app with needs.pg + a secrets map but
        // no spec.env must NOT get a DATABASE_URL injected implicitly.
        // (The 2.4e auto-inject is fully removed.)
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base_with_need("ghcr.io/acme/web:1.0", "pg")),
                environments: None,
                environment: None,
            },
            "web",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([(("pg".to_string(), None), "web-pg-conn".to_string())]);
        let r = render_application_for_env(
            &app,
            None,
            Some(&secrets),
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        assert!(
            container_env(&r).is_none(),
            "DATABASE_URL must NOT be auto-injected (2.4e removed)"
        );
    }

    #[test]
    fn redis_claim_ref_in_env_resolves_to_channel_prefix() {
        // 2.12c: explicit redis.channelPrefix ref → resolves to the
        // connection Secret's channelPrefix key. No auto REDIS_URL
        // injection.
        let mut env = BTreeMap::new();
        env.insert(
            "REDIS_PFX".to_string(),
            EnvValue::Ref(EnvRef::Claim("redis.channelPrefix".to_string())),
        );
        let mut base = base_with_need("ghcr.io/acme/web:1.0", "redis");
        base.env = Some(env);
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base),
                environments: None,
                environment: None,
            },
            "web",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([(("redis".to_string(), None), "web-redis-conn".to_string())]);
        let r = render_application_for_env(
            &app,
            None,
            Some(&secrets),
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let envs = container_env(&r).expect("env present");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "REDIS_PFX");
        let kr = envs[0]
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
    fn named_pg_claim_ref_resolves_to_named_connection_secret() {
        // 2.12c: `spec.env.DB_MAIN = {claim: "pg.main.url"}` — named
        // claim `(pg, Some("main"))` → resolved from the
        // `("pg", Some("main"))` entry in needs_secrets.
        let mut env = BTreeMap::new();
        env.insert(
            "DB_MAIN".to_string(),
            EnvValue::Ref(EnvRef::Claim("pg.main.url".to_string())),
        );
        let needs = Needs {
            pg: Some(OneOrMany::Many(vec![
                ServiceNeed::default(),
                ServiceNeed {
                    name: Some("main".to_string()),
                    ..Default::default()
                },
            ])),
            ..Default::default()
        };
        let base = ApplicationBaseSpec {
            image: Some("ghcr.io/acme/web:1.0".to_string()),
            needs: Some(needs),
            env: Some(env),
            ..Default::default()
        };
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base),
                environments: None,
                environment: None,
            },
            "parser",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([
            (("pg".to_string(), None), "parser-pg-conn".to_string()),
            (
                ("pg".to_string(), Some("main".to_string())),
                "parser-pg-main-conn".to_string(),
            ),
        ]);
        let r = render_application_for_env(
            &app,
            None,
            Some(&secrets),
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let envs = container_env(&r).expect("env present");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "DB_MAIN");
        let kr = envs[0]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(kr.name, "parser-pg-main-conn");
        assert_eq!(kr.key, "url");
    }

    #[test]
    fn secret_ref_in_env_resolves_to_secret_key_ref() {
        // 2.12c: `spec.env.TOKEN = {secret: "stripe/api-key"}` →
        // valueFrom.secretKeyRef{name:"stripe", key:"api-key"}.
        let mut env = BTreeMap::new();
        env.insert(
            "TOKEN".to_string(),
            EnvValue::Ref(EnvRef::Secret("stripe/api-key".to_string())),
        );
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("ghcr.io/acme/web:1.0".to_string()),
                    env: Some(env),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "demo",
            "uid-1",
        );
        let secrets: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
        let r = render_application_for_env(
            &app,
            None,
            Some(&secrets),
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let envs = container_env(&r).expect("env present");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "TOKEN");
        let kr = envs[0]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(kr.name, "stripe");
        assert_eq!(kr.key, "api-key");
        assert_eq!(kr.optional, Some(false));
    }

    // ---- 2.4h-c: resolved-digest threading ----

    /// Helper: build an Application whose `base.image` is `image`.
    fn app_with_image(image: &str) -> Application {
        make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some(image.to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "web",
            "default",
            "uid",
        )
    }

    #[test]
    fn deployment_uses_resolved_digest_when_provided() {
        let app = app_with_image("ghcr.io/acme/web:1.0");
        let rendered = render_application_for_env(
            &app,
            None,
            None,
            Some("ghcr.io/acme/web@sha256:abc"),
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let c = &rendered
            .deployment
            .spec
            .unwrap()
            .template
            .spec
            .unwrap()
            .containers[0];
        assert_eq!(c.image.as_deref(), Some("ghcr.io/acme/web@sha256:abc"));
    }

    #[test]
    fn deployment_uses_verbatim_tag_when_resolved_is_none() {
        let app = app_with_image("ghcr.io/acme/web:1.0");
        let rendered = render_application_for_env(
            &app,
            None,
            None,
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let c = &rendered
            .deployment
            .spec
            .unwrap()
            .template
            .spec
            .unwrap()
            .containers[0];
        assert_eq!(c.image.as_deref(), Some("ghcr.io/acme/web:1.0"));
    }

    // ---- 2.6b-4: disk mounts + Recreate strategy ----

    /// Helper: extract the rendered container's volumeMounts (or None).
    fn container_volume_mounts(r: &RenderedApplication) -> Option<Vec<VolumeMount>> {
        r.deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .volume_mounts
            .clone()
    }

    /// Helper: extract the rendered pod spec's volumes (or None).
    fn pod_volumes(r: &RenderedApplication) -> Option<Vec<Volume>> {
        r.deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .volumes
            .clone()
    }

    #[test]
    fn disk_claim_renders_volume_mount_volume_and_recreate_strategy() {
        // An app with needs.disk.data {mountPath:/data,size:1Gi} + a ready
        // claim (status.volumeClaimRef=claim-demo-app-disk-data) → the
        // Deployment carries a container volumeMount disk-data@/data, a pod
        // volume disk-data → that PVC, and spec.strategy.type == Recreate
        // (an RWO PVC cannot be held by two pods during a RollingUpdate).
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("ghcr.io/acme/web:1.0".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "demo-app",
            "demo",
            "uid-1",
        );
        let disks = vec![DiskMount {
            volume_name: "disk-data".to_string(),
            mount_path: "/data".to_string(),
            read_only: false,
            pvc_name: "claim-demo-app-disk-data".to_string(),
            owned: true,
        }];
        let r = render_application_for_env(
            &app,
            None,
            None,
            None,
            Some(&disks),
            operator_core::EgressProfile::Internet,
            None,
        );

        let mounts = container_volume_mounts(&r).expect("volumeMounts present");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].name, "disk-data");
        assert_eq!(mounts[0].mount_path, "/data");
        assert_eq!(mounts[0].read_only, Some(false));

        let volumes = pod_volumes(&r).expect("volumes present");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].name, "disk-data");
        let pvc = volumes[0]
            .persistent_volume_claim
            .as_ref()
            .expect("persistentVolumeClaim source");
        assert_eq!(pvc.claim_name, "claim-demo-app-disk-data");

        let strategy = r
            .deployment
            .spec
            .as_ref()
            .unwrap()
            .strategy
            .as_ref()
            .expect("strategy set when disk present");
        assert_eq!(strategy.type_.as_deref(), Some("Recreate"));
    }

    #[test]
    fn no_disk_leaves_strategy_unchanged() {
        // An app with no disk keeps today's strategy (unset → apiserver
        // default RollingUpdate) and no volumes / volumeMounts.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("ghcr.io/acme/web:1.0".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "demo-app",
            "demo",
            "uid-1",
        );
        let r = render_application_for_env(
            &app,
            None,
            None,
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        assert!(
            r.deployment.spec.as_ref().unwrap().strategy.is_none(),
            "strategy unchanged (unset) when no disk"
        );
        assert!(container_volume_mounts(&r).is_none());
        assert!(pod_volumes(&r).is_none());
    }

    #[test]
    fn empty_disks_slice_leaves_strategy_unchanged() {
        // A threaded-but-empty disks slice (resolved claims yielded none)
        // is treated like no disk — no strategy, no volumes.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("ghcr.io/acme/web:1.0".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "demo-app",
            "demo",
            "uid-1",
        );
        let r = render_application_for_env(
            &app,
            None,
            None,
            None,
            Some(&[]),
            operator_core::EgressProfile::Internet,
            None,
        );
        assert!(r.deployment.spec.as_ref().unwrap().strategy.is_none());
        assert!(container_volume_mounts(&r).is_none());
        assert!(pod_volumes(&r).is_none());
    }

    #[test]
    fn multiple_disks_render_in_deterministic_volume_name_order() {
        // Two disks given out of sorted order → rendered sorted by
        // volume_name (byte-stable SSA): disk-a before disk-z, mounts and
        // volumes aligned.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("ghcr.io/acme/web:1.0".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "demo-app",
            "demo",
            "uid-1",
        );
        let disks = vec![
            DiskMount {
                volume_name: "disk-z".to_string(),
                mount_path: "/z".to_string(),
                read_only: true,
                pvc_name: "claim-demo-app-disk-z".to_string(),
                owned: true,
            },
            DiskMount {
                volume_name: "disk-a".to_string(),
                mount_path: "/a".to_string(),
                read_only: false,
                pvc_name: "claim-demo-app-disk-a".to_string(),
                owned: true,
            },
        ];
        let r = render_application_for_env(
            &app,
            None,
            None,
            None,
            Some(&disks),
            operator_core::EgressProfile::Internet,
            None,
        );
        let mounts = container_volume_mounts(&r).expect("volumeMounts present");
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].name, "disk-a");
        assert_eq!(mounts[0].read_only, Some(false));
        assert_eq!(mounts[1].name, "disk-z");
        assert_eq!(mounts[1].read_only, Some(true));
        let volumes = pod_volumes(&r).expect("volumes present");
        assert_eq!(volumes[0].name, "disk-a");
        assert_eq!(volumes[1].name, "disk-z");
    }

    #[test]
    fn reference_only_disks_do_not_force_recreate() {
        // A render with a single reference (owned:false) DiskMount must NOT
        // force Recreate — SharedVolume binds tolerate RollingUpdate on a
        // single-node RWO PVC. The volume + volumeMount must still be present.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("ghcr.io/acme/web:1.0".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "demo-app",
            "demo",
            "uid-ref",
        );
        let disks = vec![DiskMount {
            volume_name: "disk-shared".to_string(),
            mount_path: "/shared".to_string(),
            read_only: false,
            pvc_name: "pvc-shared-vol".to_string(),
            owned: false,
        }];
        let r = render_application_for_env(
            &app,
            None,
            None,
            None,
            Some(&disks),
            operator_core::EgressProfile::Internet,
            None,
        );
        // Strategy must remain unset (apiserver default = RollingUpdate).
        assert!(
            r.deployment.spec.as_ref().unwrap().strategy.is_none(),
            "reference-only disks must NOT force Recreate"
        );
        // The volume + volumeMount must still be present.
        let mounts = container_volume_mounts(&r).expect("volumeMounts present for reference disk");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].name, "disk-shared");
        assert_eq!(mounts[0].mount_path, "/shared");
        let volumes = pod_volumes(&r).expect("volumes present for reference disk");
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].name, "disk-shared");
        let pvc = volumes[0]
            .persistent_volume_claim
            .as_ref()
            .expect("persistentVolumeClaim source");
        assert_eq!(pvc.claim_name, "pvc-shared-vol");
    }

    #[test]
    fn any_owned_disk_forces_recreate() {
        // Two DiskMounts: one reference (owned:false) + one owned (owned:true).
        // The presence of the owned disk must force strategy: Recreate.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("ghcr.io/acme/web:1.0".to_string()),
                    ..Default::default()
                }),
                environments: None,
                environment: None,
            },
            "demo-app",
            "demo",
            "uid-mixed",
        );
        let disks = vec![
            DiskMount {
                volume_name: "disk-ref".to_string(),
                mount_path: "/ref".to_string(),
                read_only: false,
                pvc_name: "pvc-shared-vol".to_string(),
                owned: false,
            },
            DiskMount {
                volume_name: "disk-own".to_string(),
                mount_path: "/own".to_string(),
                read_only: false,
                pvc_name: "claim-demo-app-disk-own".to_string(),
                owned: true,
            },
        ];
        let r = render_application_for_env(
            &app,
            None,
            None,
            None,
            Some(&disks),
            operator_core::EgressProfile::Internet,
            None,
        );
        let strategy = r
            .deployment
            .spec
            .as_ref()
            .unwrap()
            .strategy
            .as_ref()
            .expect("strategy set when owned disk present");
        assert_eq!(strategy.type_.as_deref(), Some("Recreate"));
    }
}
