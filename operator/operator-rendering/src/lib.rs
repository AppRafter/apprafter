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

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, EnvVarSource, PodSpec, PodTemplateSpec, SecretKeySelector,
    Service, ServicePort, ServiceSpec,
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
}

/// The Secret key the provisioner writes the DSN under (2.4c). The
/// connection Secret carries exactly this one `stringData` key for a
/// `pg` claim; the injected EnvVar's `secretKeyRef.key` points at it.
const CONNECTION_SECRET_KEY: &str = "DATABASE_URL";

/// needs-type → injected env-var name. 2.4e ships `pg` only;
/// jetstream/redis land in 2.5/2.6. The Secret key is always
/// `DATABASE_URL` (the provisioner writes exactly that one key).
/// KEEP IN SYNC with the admission webhook's reserved-env guard.
const NEEDS_ENV_VAR_NAME: &[(&str, &str)] = &[("pg", "DATABASE_URL")];

fn needs_env_var_name(service_type: &str) -> Option<&'static str> {
    NEEDS_ENV_VAR_NAME
        .iter()
        .find(|(k, _)| *k == service_type)
        .map(|(_, v)| *v)
}

/// Render the Application's `base` block (no environment override
/// applied). v0.1.30 entry point — keeps the simple call-site
/// shape; new code should prefer [`render_application_for_env`]
/// when the operator knows which environment it represents.
pub fn render_application(app: &Application) -> RenderedApplication {
    render_application_for_env(app, None, None, None)
}

/// Render the Application using the merged base + environment
/// override (when `env_name` is `Some(...)` and the override exists).
///
/// `needs_secrets` maps a `needs` service type (e.g. `"pg"`) to the
/// name of its provisioned connection Secret (the ready claim's
/// `status.connectionSecretRef`). When threaded in (the reconcile
/// builds it from the SAME ready claims the 2.4d gate validated,
/// AFTER the gate), the renderer appends a `valueFrom.secretKeyRef`
/// EnvVar per known need. `None` (pre-gate / claims unready) renders
/// the workload WITHOUT the DSN. Keeping the map a threaded param
/// preserves the renderer's purity — no kube client here.
///
/// `resolved_image` (2.4h-c) pins the container image to a resolved
/// `repo@sha256:...` digest when `Some(...)`; `None` renders the
/// effective spec's image verbatim (the tag/ref as authored). The
/// controller resolves the digest out-of-band and threads it in,
/// keeping this function pure (no registry calls here).
pub fn render_application_for_env(
    app: &Application,
    env_name: Option<&str>,
    needs_secrets: Option<&BTreeMap<String, String>>,
    resolved_image: Option<&str>,
) -> RenderedApplication {
    let name = app.metadata.name.clone().unwrap_or_default();
    let namespace = app.metadata.namespace.clone();
    let owner = owner_reference(app);
    let labels = make_labels(&name);
    let effective = effective_spec(app, env_name);

    let deployment = render_deployment(
        &name,
        namespace.as_deref(),
        &owner,
        &labels,
        &effective,
        needs_secrets,
        resolved_image,
    );
    let service = effective
        .expose
        .as_ref()
        .map(|expose| render_service(&name, namespace.as_deref(), &owner, &labels, expose.port));

    RenderedApplication {
        deployment,
        service,
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
    if let Some(env_needs) = &env_override.needs {
        let mut merged = effective.needs.unwrap_or_default();
        for (k, v) in env_needs {
            merged.insert(k.clone(), v.clone());
        }
        effective.needs = Some(merged);
    }
    effective
}

fn owner_reference(app: &Application) -> OwnerReference {
    OwnerReference {
        api_version: "apprafter.io/v1alpha1".to_string(),
        kind: "Application".to_string(),
        name: app.metadata.name.clone().unwrap_or_default(),
        uid: app.metadata.uid.clone().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

fn make_labels(name: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/name".to_string(), name.to_string());
    labels.insert(
        "app.kubernetes.io/managed-by".to_string(),
        "apprafter-operator".to_string(),
    );
    labels.insert("apprafter".to_string(), "true".to_string());
    labels
}

fn render_deployment(
    name: &str,
    namespace: Option<&str>,
    owner: &OwnerReference,
    labels: &BTreeMap<String, String>,
    spec: &ApplicationBaseSpec,
    needs_secrets: Option<&BTreeMap<String, String>>,
    resolved_image: Option<&str>,
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

    let mut env_vars: Vec<EnvVar> = spec
        .env
        .as_ref()
        .map(|env| {
            env.iter()
                .map(|(k, v)| EnvVar {
                    name: k.clone(),
                    value: Some(v.clone()),
                    value_from: None,
                })
                .collect()
        })
        .unwrap_or_default();

    // 2.4e: append a `valueFrom.secretKeyRef` DSN EnvVar per known
    // need whose claim has a resolved connection Secret. Iterating
    // `needs.keys()` (BTreeMap) keeps the appended order deterministic
    // so the rendered Deployment is byte-stable across reconciles
    // (SSA no-op; non-deterministic order would spin the operator).
    // Appended AFTER the literal env so a (rejected-by-webhook, but
    // defensively) colliding literal would never silently win.
    if let (Some(needs), Some(secrets)) = (spec.needs.as_ref(), needs_secrets) {
        for service_type in needs.keys() {
            let (Some(var_name), Some(secret_name)) =
                (needs_env_var_name(service_type), secrets.get(service_type))
            else {
                continue;
            };
            env_vars.push(EnvVar {
                name: var_name.to_string(),
                value: None,
                value_from: Some(EnvVarSource {
                    secret_key_ref: Some(SecretKeySelector {
                        name: secret_name.clone(),
                        key: CONNECTION_SECRET_KEY.to_string(),
                        optional: Some(false),
                    }),
                    ..Default::default()
                }),
            });
        }
    }

    let container = Container {
        name: name.to_string(),
        image: Some(image),
        env: if env_vars.is_empty() {
            None
        } else {
            Some(env_vars)
        },
        ports: container_port.map(|p| vec![p]),
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
        env.insert("LOG_LEVEL".to_string(), "info".to_string());
        env.insert("ALPHA".to_string(), "1".to_string());
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("x".to_string()),
                    env: Some(env),
                    ..Default::default()
                }),
                environments: None,
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

    fn make_app_with_envs(
        base: ApplicationBaseSpec,
        envs: BTreeMap<String, ApplicationBaseSpec>,
    ) -> Application {
        let mut app = Application::new(
            "web",
            ApplicationSpec {
                base: Some(base),
                environments: Some(envs),
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
        base_env.insert("LOG_LEVEL".to_string(), "info".to_string());
        base_env.insert("REGION".to_string(), "eu".to_string());
        let mut prod_env = BTreeMap::new();
        prod_env.insert("LOG_LEVEL".to_string(), "warn".to_string());
        prod_env.insert("PROD_FLAG".to_string(), "1".to_string());
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
        assert_eq!(env.get("LOG_LEVEL").map(String::as_str), Some("warn"));
        // base survives:
        assert_eq!(env.get("REGION").map(String::as_str), Some("eu"));
        // override-only:
        assert_eq!(env.get("PROD_FLAG").map(String::as_str), Some("1"));
    }

    #[test]
    fn effective_spec_env_override_replaces_needs_per_key_and_keeps_base_only_needs() {
        // 2.4d S0: env-scoped `needs` must merge per-key (the env
        // entry replaces the base entry WHOLESALE for that key —
        // mirrors how `expose` replaces), and base-only need keys
        // survive when the env omits them.
        use operator_core::ServiceNeed;
        let mut base_needs = BTreeMap::new();
        base_needs.insert(
            "pg".to_string(),
            ServiceNeed {
                selector: None,
                size: Some("small".into()),
            },
        );
        base_needs.insert(
            "redis".to_string(),
            ServiceNeed {
                selector: None,
                size: Some("nano".into()),
            },
        );
        let mut prod_needs = BTreeMap::new();
        prod_needs.insert(
            "pg".to_string(),
            ServiceNeed {
                selector: Some(BTreeMap::from([(
                    "tier".to_string(),
                    "managed-aws".to_string(),
                )])),
                size: None,
            },
        );
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
        let pg = needs.get("pg").expect("pg need");
        assert_eq!(
            pg.selector
                .as_ref()
                .and_then(|m| m.get("tier"))
                .map(String::as_str),
            Some("managed-aws")
        );
        assert_eq!(pg.size, None, "base size must be dropped on env replace");
        // base-only redis survives.
        let redis = needs.get("redis").expect("base-only redis survives");
        assert_eq!(redis.size.as_deref(), Some("nano"));
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
        let r = render_application_for_env(&app, Some("prod"), None, None);
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

    use operator_core::ServiceNeed;

    /// Helper: build a base with a single named need (no selector/size).
    fn base_with_need(image: &str, need_type: &str) -> ApplicationBaseSpec {
        let mut needs = BTreeMap::new();
        needs.insert(need_type.to_string(), ServiceNeed::default());
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
            },
            "parser",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([("pg".to_string(), "parser-pg-conn".to_string())]);
        let r = render_application_for_env(&app, None, Some(&secrets), None);
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
            },
            "parser",
            "demo",
            "uid-1",
        );
        let r = render_application_for_env(&app, None, None, None);
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
            },
            "parser",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([("pg".to_string(), "parser-pg-conn".to_string())]);
        let r = render_application_for_env(&app, None, Some(&secrets), None);
        let envs = container_env(&r).expect("env present (DSN injected)");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "DATABASE_URL");
    }

    #[test]
    fn literal_env_coexists_with_dsn_appended_after() {
        let mut base = base_with_need("ghcr.io/acme/web:1.0", "pg");
        base.env = Some(BTreeMap::from([(
            "LOG_LEVEL".to_string(),
            "info".to_string(),
        )]));
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base),
                environments: None,
            },
            "parser",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([("pg".to_string(), "parser-pg-conn".to_string())]);
        let r = render_application_for_env(&app, None, Some(&secrets), None);
        let envs = container_env(&r).expect("env present");
        assert_eq!(envs.len(), 2);
        // Literal env first, DSN appended AFTER.
        assert_eq!(envs[0].name, "LOG_LEVEL");
        assert_eq!(envs[0].value.as_deref(), Some("info"));
        assert_eq!(envs[1].name, "DATABASE_URL");
        assert!(envs[1].value_from.is_some());
    }

    #[test]
    fn non_pg_need_in_secrets_map_is_not_injected() {
        // pg-only table: a redis entry in the secrets map must NOT
        // produce an env var (jetstream/redis injection is 2.5/2.6).
        let app = make_app_with_uid(
            ApplicationSpec {
                base: Some(base_with_need("ghcr.io/acme/web:1.0", "redis")),
                environments: None,
            },
            "parser",
            "demo",
            "uid-1",
        );
        let secrets = BTreeMap::from([("redis".to_string(), "parser-redis-conn".to_string())]);
        let r = render_application_for_env(&app, None, Some(&secrets), None);
        let envs = container_env(&r);
        assert!(
            envs.is_none(),
            "redis need must not inject any env (pg-only)"
        );
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
            },
            "web",
            "default",
            "uid",
        )
    }

    #[test]
    fn deployment_uses_resolved_digest_when_provided() {
        let app = app_with_image("ghcr.io/acme/web:1.0");
        let rendered =
            render_application_for_env(&app, None, None, Some("ghcr.io/acme/web@sha256:abc"));
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
        let rendered = render_application_for_env(&app, None, None, None);
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
}
