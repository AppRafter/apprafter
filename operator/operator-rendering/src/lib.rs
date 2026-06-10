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

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec, DeploymentStrategy};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, EnvVarSource, PersistentVolumeClaimVolumeSource, PodSpec,
    PodTemplateSpec, SecretKeySelector, Service, ServicePort, ServiceSpec, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use operator_core::{Application, ApplicationBaseSpec, EnvValue};

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
}

/// One ready `needs.disk` claim resolved into render input (2.6b /
/// ADR 0043). The Application controller builds a `Vec<DiskMount>` from
/// each `needs.disk` entry × the matching ready disk ResourceClaim's
/// `status.volumeClaimRef`, AFTER the readiness gate, and threads it into
/// [`render_application_for_env`]. The renderer appends one container
/// `volumeMount` + one pod `volume{persistentVolumeClaim}` per entry
/// (deterministic, sorted by `volume_name` → byte-stable SSA) and forces
/// `strategy: Recreate` when any is present. Keeping this a threaded
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
}

/// needs-type → the env vars to inject, each as (env-var-name,
/// secret-key). The provisioner writes exactly these keys into the
/// connection Secret; the injected EnvVar's `secretKeyRef.key` points
/// at the secret-key. `pg` ships one DSN key (2.4e); `redis` (2.6)
/// ships two — the DSN plus the pub/sub channel prefix.
///
/// KEEP IN SYNC with the admission webhook's reserved-env guard
/// (`RESERVED_ENV`) and the provisioner's connection-Secret builders.
const NEEDS_ENV_BINDINGS: &[(&str, &[(&str, &str)])] = &[
    ("pg", &[("DATABASE_URL", "DATABASE_URL")]),
    (
        "redis",
        &[
            ("REDIS_URL", "REDIS_URL"),
            ("REDIS_CHANNEL_PREFIX", "REDIS_CHANNEL_PREFIX"),
        ],
    ),
];

fn needs_env_bindings(service_type: &str) -> &'static [(&'static str, &'static str)] {
    NEEDS_ENV_BINDINGS
        .iter()
        .find(|(k, _)| *k == service_type)
        .map(|(_, v)| *v)
        .unwrap_or(&[])
}

/// Fold a `(type, name)` claim name into a valid env-var-NAME segment
/// (2.6b / ADR 0043): uppercase every ASCII letter and map `-` → `_`.
/// A named claim's injected env NAME is `<VAR>_<fold(name)>`
/// (`my-cache` → `MY_CACHE` → `DATABASE_URL_MY_CACHE`); the Secret KEY
/// is unchanged (`DATABASE_URL`). The webhook guarantees `name` is a
/// DNS-1123 label, so the fold always yields a valid `[A-Z_][A-Z0-9_]*`
/// suffix — no other characters can appear.
pub fn fold_env_segment(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '-' => '_',
            other => other.to_ascii_uppercase(),
        })
        .collect()
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
/// entry (2.6b / ADR 0043). When threaded in (the reconcile builds it
/// from the SAME ready claims the 2.4d gate validated, AFTER the gate),
/// the renderer appends a `valueFrom.secretKeyRef` EnvVar per known
/// need: the default claim keeps the base env NAME (`DATABASE_URL`),
/// a named claim gets `<VAR>_<fold(name)>` (the Secret KEY stays
/// `DATABASE_URL`). `None` (pre-gate / claims unready) renders the
/// workload WITHOUT the DSN. Keeping the map a threaded param preserves
/// the renderer's purity — no kube client here.
///
/// `resolved_image` (2.4h-c) pins the container image to a resolved
/// `repo@sha256:...` digest when `Some(...)`; `None` renders the
/// effective spec's image verbatim (the tag/ref as authored). The
/// controller resolves the digest out-of-band and threads it in,
/// keeping this function pure (no registry calls here).
///
/// `disks` (2.6b / ADR 0043) carries the ready `needs.disk` claims as
/// render input — one [`DiskMount`] per resolved disk. When non-empty the
/// renderer appends a container `volumeMount` + a pod
/// `volume{persistentVolumeClaim}` per entry (sorted by `volume_name` →
/// byte-stable SSA) and forces `strategy: Recreate` (an RWO PVC cannot be
/// held by two pods during a RollingUpdate). `None`/empty leaves the
/// Deployment strategy unchanged from today (unset → apiserver default).
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

    RenderedApplication {
        deployment,
        service,
        network_policy,
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

    // 2.12 (ADR 0046): env values are now `EnvValue` — either a plain
    // `Literal(String)` (same rendering as before) or a `Ref(EnvRef)`
    // (claim / external-secret reference). Ref resolution is implemented
    // in the next task (2.12b); for the schema-foundation task (2.12a)
    // only Literal values are rendered. Ref variants are deferred (the
    // renderer will resolve them once the connection-secret map and the
    // ref-resolution logic land in 2.12b).
    let mut env_vars: Vec<EnvVar> = spec
        .env
        .as_ref()
        .map(|env| {
            env.iter()
                .filter_map(|(k, v)| match v {
                    EnvValue::Literal(s) => Some(EnvVar {
                        name: k.clone(),
                        value: Some(s.clone()),
                        value_from: None,
                    }),
                    // Ref resolution deferred to 2.12b (renderer expansion).
                    EnvValue::Ref(_) => None,
                })
                .collect()
        })
        .unwrap_or_default();

    // 2.4e/2.6: append a `valueFrom.secretKeyRef` EnvVar per known
    // need whose claim has a resolved connection Secret. A need may
    // inject more than one var (redis: REDIS_URL + REDIS_CHANNEL_PREFIX)
    // — the bindings slice is fixed-order. Walking `Needs::entries()`
    // (a deterministic key/index order) keeps the appended order stable
    // so the rendered Deployment is byte-stable across reconciles (SSA
    // no-op; non-deterministic order would spin the operator). Appended
    // AFTER the literal env so a (rejected-by-webhook, but defensively)
    // colliding literal would never silently win.
    //
    // 2.6b (ADR 0043): inject one Secret per `(type, name)` claim
    // identity. `needs_secrets` is keyed by `(service_type, name_opt)` —
    // `name_opt == None` is the unnamed/default claim (keeps the base env
    // NAME, e.g. `DATABASE_URL`, backward-compatible), `Some(name)` is a
    // named array entry (env NAME `<VAR>_<fold(name)>`, e.g.
    // `DATABASE_URL_ANALYTICS`; the Secret KEY stays `DATABASE_URL`).
    // Walking `Needs::entries()` (a deterministic key/index order) keeps
    // the appended order byte-stable across reconciles (SSA no-op).
    // Appended AFTER the literal env so a (webhook-rejected, but
    // defensively) colliding literal would never silently win. Disk
    // entries carry no env binding and are skipped here.
    if let (Some(needs), Some(secrets)) = (spec.needs.as_ref(), needs_secrets) {
        for (service_type, entry) in needs.entries() {
            if entry.disk.is_some() {
                continue;
            }
            let key = (service_type.clone(), entry.name.clone());
            let Some(secret_name) = secrets.get(&key) else {
                continue;
            };
            let suffix = entry
                .name
                .as_deref()
                .filter(|n| !n.is_empty())
                .map(|n| format!("_{}", fold_env_segment(n)));
            for (var_name, secret_key) in needs_env_bindings(&service_type) {
                let env_name = match &suffix {
                    Some(s) => format!("{var_name}{s}"),
                    None => var_name.to_string(),
                };
                env_vars.push(EnvVar {
                    name: env_name,
                    value: None,
                    value_from: Some(EnvVarSource {
                        secret_key_ref: Some(SecretKeySelector {
                            name: secret_name.clone(),
                            key: secret_key.to_string(),
                            optional: Some(false),
                        }),
                        ..Default::default()
                    }),
                });
            }
        }
    }

    // 2.6b (ADR 0043): mount ready disk claims into the pod. Each
    // DiskMount contributes a container `volumeMount` + a pod
    // `volume{persistentVolumeClaim}`, sorted by `volume_name` so the
    // rendered Deployment is byte-stable across reconciles (SSA no-op).
    // When ANY disk is present the strategy is forced to `Recreate` — an
    // RWO PVC cannot be held by the old + new pod simultaneously during a
    // RollingUpdate; with no disk the strategy stays unset (apiserver
    // default RollingUpdate), unchanged from today.
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
    let strategy = if sorted_disks.is_empty() {
        None
    } else {
        Some(DeploymentStrategy {
            type_: Some("Recreate".to_string()),
            rolling_update: None,
        })
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
                match_labels: Some(labels.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{ApplicationExpose, ApplicationSpec};

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
                        public: Some(false),
                        network: None,
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
                        public: None,
                        network: None,
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

    #[test]
    fn owner_reference_points_back_to_application_with_uid() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    expose: Some(ApplicationExpose {
                        port: 8080,
                        public: None,
                        network: None,
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
                    public: Some(true),
                    network: Some("public".into()),
                }),
                ..Default::default()
            },
        );
        let app = make_app_with_envs(
            ApplicationBaseSpec {
                image: Some("base".into()),
                expose: Some(ApplicationExpose {
                    port: 8080,
                    public: Some(false),
                    network: None,
                }),
                ..Default::default()
            },
            envs,
        );
        let s = effective_spec(&app, Some("prod"));
        let expose = s.expose.expect("expose decoded");
        assert_eq!(expose.port, 9000);
        assert_eq!(expose.public, Some(true));
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

    // ---- 2.4e: DATABASE_URL DSN injection ----

    use operator_core::{Needs, OneOrMany, ServiceNeed};

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
    fn pg_need_injects_database_url_secret_key_ref() {
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
        let envs = container_env(&r).expect("env present");
        let dsn = envs
            .iter()
            .find(|e| e.name == "DATABASE_URL")
            .expect("DATABASE_URL injected");
        assert_eq!(dsn.value, None);
        let source = dsn.value_from.as_ref().expect("value_from set");
        let key_ref = source.secret_key_ref.as_ref().expect("secret_key_ref set");
        assert_eq!(key_ref.name, "parser-pg-conn");
        assert_eq!(key_ref.key, "DATABASE_URL");
        assert_eq!(key_ref.optional, Some(false));
    }

    #[test]
    fn needs_present_but_no_secrets_map_skips_injection() {
        // The 2.4d "resumes WITHOUT DATABASE_URL" contract: a need is
        // declared but no resolved secret map is threaded in (claims not
        // ready / pre-gate) → no DATABASE_URL env.
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
        // No literal env + no injected DSN → env stays None.
        assert!(envs.is_none(), "no DATABASE_URL when secrets map is None");
    }

    #[test]
    fn empty_env_plus_dsn_yields_some_env_of_len_one() {
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
        let envs = container_env(&r).expect("env present (DSN injected)");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "DATABASE_URL");
    }

    #[test]
    fn literal_env_coexists_with_dsn_appended_after() {
        let mut base = base_with_need("ghcr.io/acme/web:1.0", "pg");
        base.env = Some(BTreeMap::from([(
            "LOG_LEVEL".to_string(),
            EnvValue::Literal("info".to_string()),
        )]));
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
        assert_eq!(envs.len(), 2);
        // Literal env first, DSN appended AFTER.
        assert_eq!(envs[0].name, "LOG_LEVEL");
        assert_eq!(envs[0].value.as_deref(), Some("info"));
        assert_eq!(envs[1].name, "DATABASE_URL");
        assert!(envs[1].value_from.is_some());
    }

    #[test]
    fn unknown_need_in_secrets_map_is_not_injected() {
        // The bindings table is closed: an unknown need type (no entry
        // in NEEDS_ENV_BINDINGS) must NOT produce any env var, even when
        // a secret name is threaded in for it.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base_with_need("ghcr.io/acme/web:1.0", "clickhouse")),
                environments: None,
                environment: None,
            },
            "parser",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([(
            ("clickhouse".to_string(), None),
            "parser-clickhouse-conn".to_string(),
        )]);
        let r = render_application_for_env(
            &app,
            None,
            Some(&secrets),
            None,
            None,
            operator_core::EgressProfile::Internet,
            None,
        );
        let envs = container_env(&r);
        assert!(
            envs.is_none(),
            "unknown need must not inject any env (closed bindings table)"
        );
    }

    // ---- 2.6-6: redis injects REDIS_URL + REDIS_CHANNEL_PREFIX ----

    #[test]
    fn redis_need_injects_url_and_channel_prefix() {
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base_with_need("ghcr.io/acme/web:1.0", "redis")),
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
        let url = envs
            .iter()
            .find(|e| e.name == "REDIS_URL")
            .expect("REDIS_URL injected");
        assert_eq!(
            url.value_from
                .as_ref()
                .unwrap()
                .secret_key_ref
                .as_ref()
                .unwrap()
                .key,
            "REDIS_URL"
        );
        assert_eq!(
            url.value_from
                .as_ref()
                .unwrap()
                .secret_key_ref
                .as_ref()
                .unwrap()
                .name,
            "web-redis-conn"
        );
        let pfx = envs
            .iter()
            .find(|e| e.name == "REDIS_CHANNEL_PREFIX")
            .expect("REDIS_CHANNEL_PREFIX injected");
        assert_eq!(
            pfx.value_from
                .as_ref()
                .unwrap()
                .secret_key_ref
                .as_ref()
                .unwrap()
                .name,
            "web-redis-conn"
        );
        assert_eq!(
            pfx.value_from
                .as_ref()
                .unwrap()
                .secret_key_ref
                .as_ref()
                .unwrap()
                .key,
            "REDIS_CHANNEL_PREFIX"
        );
    }

    #[test]
    fn redis_injection_is_deterministic_two_vars() {
        // Both redis env vars present, fixed binding-slice order
        // (REDIS_URL then REDIS_CHANNEL_PREFIX), no literal env.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base_with_need("ghcr.io/acme/web:1.0", "redis")),
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
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].name, "REDIS_URL");
        assert_eq!(envs[1].name, "REDIS_CHANNEL_PREFIX");
    }

    // ---- 2.6b: env disambiguation DATABASE_URL_<NAME> for named claims ----

    #[test]
    fn fold_env_segment_uppercases_and_maps_hyphen_to_underscore() {
        assert_eq!(fold_env_segment("my-cache"), "MY_CACHE");
        assert_eq!(fold_env_segment("analytics"), "ANALYTICS");
        assert_eq!(fold_env_segment("read-replica-2"), "READ_REPLICA_2");
        // already-uppercase / digits pass through.
        assert_eq!(fold_env_segment("a1b"), "A1B");
    }

    /// Helper: build a base with TWO pg needs — the unnamed default plus
    /// a named array entry. Renders to two `(pg, name)` claim identities.
    fn base_with_two_pg(image: &str, named: &str) -> ApplicationBaseSpec {
        let needs = Needs {
            pg: Some(OneOrMany::Many(vec![
                ServiceNeed::default(),
                ServiceNeed {
                    name: Some(named.to_string()),
                    ..Default::default()
                },
            ])),
            ..Default::default()
        };
        ApplicationBaseSpec {
            image: Some(image.to_string()),
            needs: Some(needs),
            ..Default::default()
        }
    }

    #[test]
    fn two_pg_claims_yield_database_url_and_suffixed_named_var() {
        // Two ready pg claims (default + named "analytics") → the
        // Deployment carries DATABASE_URL (default) AND
        // DATABASE_URL_ANALYTICS (named), each a secretKeyRef to its own
        // per-claim conn Secret with key DATABASE_URL.
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base_with_two_pg("ghcr.io/acme/web:1.0", "analytics")),
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
                ("pg".to_string(), Some("analytics".to_string())),
                "parser-pg-analytics-conn".to_string(),
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

        let default = envs
            .iter()
            .find(|e| e.name == "DATABASE_URL")
            .expect("DATABASE_URL injected for the default claim");
        let dref = default
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(dref.name, "parser-pg-conn");
        assert_eq!(dref.key, "DATABASE_URL");

        let named = envs
            .iter()
            .find(|e| e.name == "DATABASE_URL_ANALYTICS")
            .expect("DATABASE_URL_ANALYTICS injected for the named claim");
        let nref = named
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        // The Secret KEY stays DATABASE_URL — only the env NAME is suffixed.
        assert_eq!(nref.key, "DATABASE_URL");
        assert_eq!(nref.name, "parser-pg-analytics-conn");

        // Exactly the two injected DSN vars (no literal env here).
        assert_eq!(envs.len(), 2);
    }

    #[test]
    fn single_unnamed_pg_still_yields_exactly_database_url() {
        // Backward-compat: a single unnamed pg claim keeps the bare
        // DATABASE_URL env NAME — no suffix, no migration.
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
        let envs = container_env(&r).expect("env present");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "DATABASE_URL");
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
            },
            DiskMount {
                volume_name: "disk-a".to_string(),
                mount_path: "/a".to_string(),
                read_only: false,
                pvc_name: "claim-demo-app-disk-a".to_string(),
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
}
